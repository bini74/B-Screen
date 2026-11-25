//! Transcription des passages de voix (phase 5) : reconnaissance vocale d'Apple, sur le Mac
//! (jamais sur un serveur : `requiresOnDeviceRecognition`), en français, avec l'heure de chaque mot.
//!
//! Un fil à part prend les passages pas encore transcrits, les plus anciens d'abord, et range
//! le texte dans la base : il entre alors dans la recherche (« écran » 0, source 2 = voix).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use block2::RcBlock;
use bscreen_core::chemins;
use bscreen_core::reglages::Reglages;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSError, NSLocale, NSOperationQueue, NSString, NSURL};
use objc2_speech::{
    SFSpeechRecognitionResult, SFSpeechRecognitionTaskHint, SFSpeechRecognizer, SFSpeechURLRecognitionRequest,
};

use crate::autorisations::{Autorisation, Etat};
use crate::moteur::avec_base;

/// Nom du moteur, rangé avec chaque transcription (pour comparer plus tard avec Whisper, Parakeet…).
pub const MODELE: &str = "apple-local-fr";

/// Essais avant de renoncer à transcrire un passage.
const ECHECS_MAX: u32 = 3;

/// Compteurs pour le journal.
pub static TRANSCRITS: AtomicU64 = AtomicU64::new(0);
pub static DUREE_MS: AtomicU64 = AtomicU64::new(0);
pub static AUDIO_MS: AtomicU64 = AtomicU64::new(0);

/// Un mot dit, avec sa place dans le passage (en secondes depuis son début).
#[derive(Debug, Clone, PartialEq)]
pub struct MotDit {
    pub debut_s: f64,
    pub duree_s: f64,
    pub mot: String,
    pub confiance: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transcription {
    pub texte: String,
    pub mots: Vec<MotDit>,
}

pub struct Transcripteur {
    reconnaisseur: Retained<SFSpeechRecognizer>,
    _file: Retained<NSOperationQueue>,
}

impl Transcripteur {
    pub fn nouveau() -> Result<Self> {
        let locale = NSLocale::initWithLocaleIdentifier(NSLocale::alloc(), &NSString::from_str("fr-FR"));
        let reconnaisseur = unsafe { SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale) }
            .ok_or_else(|| anyhow!("reconnaissance vocale en français indisponible"))?;
        if !unsafe { reconnaisseur.supportsOnDeviceRecognition() } {
            bail!("la reconnaissance en français sur le Mac n'est pas installée (Réglages › Clavier › Dictée)");
        }
        // Les réponses arrivent sur une file à part, pas sur le fil principal (l'interface).
        let file = NSOperationQueue::new();
        unsafe { reconnaisseur.setQueue(&file) };
        Ok(Self {
            reconnaisseur,
            _file: file,
        })
    }

    /// Transcrit un fichier audio. Un passage sans parole compréhensible donne un texte vide.
    /// Avec son bassin de libération : le fil de la transcription dure autant que l'app.
    pub fn transcrire(&self, fichier: &Path, vocabulaire: &[String]) -> Result<Transcription> {
        objc2::rc::autoreleasepool(|_| self.transcrire_seul(fichier, vocabulaire))
    }

    fn transcrire_seul(&self, fichier: &Path, vocabulaire: &[String]) -> Result<Transcription> {
        if !fichier.exists() {
            bail!("fichier absent : {}", fichier.display());
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(&fichier.to_string_lossy()));
        let requete =
            unsafe { SFSpeechURLRecognitionRequest::initWithURL(SFSpeechURLRecognitionRequest::alloc(), &url) };
        unsafe {
            requete.setRequiresOnDeviceRecognition(true);
            requete.setShouldReportPartialResults(false);
            requete.setAddsPunctuation(true);
            requete.setTaskHint(SFSpeechRecognitionTaskHint::Dictation);
            if !vocabulaire.is_empty() {
                let mots: Vec<Retained<NSString>> = vocabulaire.iter().map(|m| NSString::from_str(m)).collect();
                requete.setContextualStrings(&NSArray::from_retained_slice(&mots));
            }
        }
        let (envoi, reception) = mpsc::channel::<Result<Transcription>>();
        let reponse = RcBlock::new(move |resultat: *mut SFSpeechRecognitionResult, erreur: *mut NSError| {
            if let Some(erreur) = unsafe { erreur.as_ref() } {
                // 1110 = aucune parole détectée : ce n'est pas une panne.
                let _ = envoi.send(if erreur.code() == 1110 {
                    Ok(Transcription::default())
                } else {
                    Err(anyhow!(
                        "reconnaissance vocale ({} {}) : {}",
                        erreur.domain(),
                        erreur.code(),
                        erreur.localizedDescription()
                    ))
                });
                return;
            }
            let Some(resultat) = (unsafe { resultat.as_ref() }) else { return };
            if !unsafe { resultat.isFinal() } {
                return;
            }
            let meilleure = unsafe { resultat.bestTranscription() };
            let mots = unsafe { meilleure.segments() }
                .iter()
                .map(|s| unsafe {
                    MotDit {
                        // Heures lues dans le fichier décodé : on retire l'amorce de l'encodeur.
                        debut_s: (s.timestamp() - crate::audio::AMORCE_S).max(0.0),
                        duree_s: s.duration(),
                        mot: s.substring().to_string(),
                        confiance: s.confidence(),
                    }
                })
                .collect();
            let _ = envoi.send(Ok(Transcription {
                texte: unsafe { meilleure.formattedString() }.to_string(),
                mots,
            }));
        });
        let tache = unsafe {
            self.reconnaisseur
                .recognitionTaskWithRequest_resultHandler(&requete, &reponse)
        };
        match reception.recv_timeout(Duration::from_secs(120)) {
            Ok(resultat) => resultat,
            Err(_) => {
                unsafe { tache.cancel() };
                bail!("la transcription ne répond pas")
            }
        }
    }
}

/// La transcription sans secrets : le texte passe par le masquage ; si quelque chose y a été
/// masqué, les mots qui portent des chiffres le sont aussi (un numéro dicté arrive mot à mot).
pub fn masquer_transcription(t: &Transcription) -> (String, Vec<MotDit>) {
    use bscreen_core::masquage::{MASQUE, masquer};
    let texte = masquer(&t.texte);
    let change = matches!(texte, std::borrow::Cow::Owned(_));
    let mots = t
        .mots
        .iter()
        .map(|m| {
            let chiffres = m.mot.chars().filter(char::is_ascii_digit).count();
            let mot = if change && chiffres >= 2 {
                MASQUE.to_string()
            } else {
                masquer(&m.mot).into_owned()
            };
            MotDit { mot, ..m.clone() }
        })
        .collect();
    (texte.into_owned(), mots)
}

/// Les mots en JSON pour la base : `[[debut_ms, fin_ms, "mot"], ...]`, heures absolues.
pub fn mots_en_json(debut_passage_ms: i64, mots: &[MotDit]) -> String {
    let lignes: Vec<(i64, i64, &str)> = mots
        .iter()
        .map(|m| {
            let debut = debut_passage_ms + (m.debut_s * 1000.0).round() as i64;
            (debut, debut + (m.duree_s * 1000.0).round() as i64, m.mot.as_str())
        })
        .collect();
    serde_json::to_string(&lignes).unwrap_or_else(|_| "[]".into())
}

static REVEIL: OnceLock<mpsc::SyncSender<()>> = OnceLock::new();

/// Un nouveau passage attend : réveille le fil de transcription (sans attendre).
pub fn signaler() {
    if let Some(envoi) = REVEIL.get() {
        let _ = envoi.try_send(());
    }
}

/// Lance le fil de transcription (une fois).
pub fn demarrer() {
    if REVEIL.get().is_some() {
        return;
    }
    let (envoi, reception) = mpsc::sync_channel::<()>(1);
    if REVEIL.set(envoi).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("bscreen-transcription".into())
        .spawn(move || boucle(&reception));
}

fn boucle(reception: &mpsc::Receiver<()>) {
    let mut transcripteur: Option<Transcripteur> = None;
    let mut dernier_avertissement: Option<(Instant, String)> = None;
    let mut avertir = |message: String| {
        // Le même souci au plus une fois par heure dans le journal.
        let deja = dernier_avertissement
            .as_ref()
            .is_some_and(|(quand, m)| *m == message && quand.elapsed() < Duration::from_secs(3600));
        if !deja {
            tracing::warn!(message, "transcription");
            dernier_avertissement = Some((Instant::now(), message));
        }
    };
    let mut demande_faite = false;
    // Échecs par passage : après 3, le passage est marqué « échec » pour ne pas bloquer la file.
    let mut echecs: std::collections::HashMap<i64, u32> = std::collections::HashMap::new();
    // Vocabulaire automatique (noms propres vus souvent à l'écran), recalculé toutes les heures.
    let mut vocabulaire_auto: Vec<String> = Vec::new();
    let mut maj_vocabulaire: Option<Instant> = None;
    loop {
        let _ = reception.recv_timeout(Duration::from_secs(60));
        match Autorisation::Voix.etat() {
            Etat::Accordee => {}
            Etat::ADemander if !demande_faite => {
                demande_faite = true;
                Autorisation::Voix.demander();
                continue;
            }
            _ => {
                avertir("reconnaissance vocale pas autorisée".into());
                continue;
            }
        }
        if transcripteur.is_none() {
            match Transcripteur::nouveau() {
                Ok(t) => transcripteur = Some(t),
                Err(e) => {
                    avertir(e.to_string());
                    continue;
                }
            }
        }
        let Some(t) = transcripteur.as_ref() else { continue };
        if maj_vocabulaire.is_none_or(|q| q.elapsed() > Duration::from_secs(3600)) {
            let depuis = crate::encodage::ms_depuis_1970(std::time::SystemTime::now()) - 7 * 86_400_000;
            let textes = avec_base(|b| b.textes_vus_depuis(depuis, 20_000)).unwrap_or_default();
            vocabulaire_auto = bscreen_core::vocabulaire::noms_propres(&textes, 5, 50);
            maj_vocabulaire = Some(Instant::now());
            tracing::info!(mots = vocabulaire_auto.len(), "vocabulaire automatique de la transcription mis à jour");
        }
        // Le vocabulaire de Julien d'abord, puis les noms propres vus à l'écran (sans doublon).
        let mut vocabulaire = Reglages::charger().vocabulaire;
        for mot in &vocabulaire_auto {
            if !vocabulaire.iter().any(|v| v.eq_ignore_ascii_case(mot)) {
                vocabulaire.push(mot.clone());
            }
        }
        vocabulaire.truncate(100);
        'lots: while let Some(lot) = avec_base(|b| b.voix_a_transcrire(8)) {
            if lot.is_empty() {
                break;
            }
            for voix in lot {
                let debut = Instant::now();
                let chemin = chemins::donnees().join(&voix.fichier);
                let essai = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.transcrire(&chemin, &vocabulaire)))
                    .unwrap_or_else(|_| Err(anyhow!("panique pendant la transcription")));
                let transcription = match essai {
                    Ok(tr) => tr,
                    // Fichier effacé (purge, suppression) : rien à transcrire.
                    Err(_) if !chemin.exists() => Transcription::default(),
                    Err(e) => {
                        let n = echecs.entry(voix.id).or_insert(0);
                        *n += 1;
                        if *n >= ECHECS_MAX {
                            // Toujours en échec (fichier abîmé…) : on le marque et la file avance.
                            tracing::warn!(id = voix.id, %e, "passage jamais transcrit : marqué en échec");
                            avec_base(|b| b.noter_transcription(&voix, "", "[]", &format!("echec: {e}")));
                            echecs.remove(&voix.id);
                            continue;
                        }
                        avertir(e.to_string());
                        break 'lots;
                    }
                };
                // Un numéro de carte ou un mot de passe dit à voix haute est masqué, mot à mot aussi.
                let (texte, mots) = masquer_transcription(&transcription);
                let json = mots_en_json(voix.debut_ms, &mots);
                avec_base(|b| b.noter_transcription(&voix, texte.trim(), &json, MODELE));
                TRANSCRITS.fetch_add(1, Ordering::Relaxed);
                DUREE_MS.fetch_add(debut.elapsed().as_millis() as u64, Ordering::Relaxed);
                AUDIO_MS.fetch_add((voix.fin_ms - voix.debut_ms).max(0) as u64, Ordering::Relaxed);
                tracing::debug!(id = voix.id, mots = transcription.mots.len(), "passage transcrit");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numero_dicte_masque() {
        let mot = |m: &str, t: f64| MotDit {
            debut_s: t,
            duree_s: 0.3,
            mot: m.into(),
            confiance: 0.9,
        };
        let t = Transcription {
            texte: "ma carte c'est 4111 1111 1111 1111".into(),
            mots: vec![mot("ma", 0.0), mot("carte", 0.3), mot("4111", 1.0), mot("1111", 1.3)],
        };
        let (texte, mots) = masquer_transcription(&t);
        assert!(!texte.contains("4111"), "{texte}");
        assert_eq!(mots[2].mot, bscreen_core::masquage::MASQUE);
        assert_eq!(mots[1].mot, "carte");
    }

    #[test]
    fn mots_avec_heures_absolues() {
        let mots = vec![
            MotDit {
                debut_s: 0.25,
                duree_s: 0.4,
                mot: "les".into(),
                confiance: 0.9,
            },
            MotDit {
                debut_s: 0.7,
                duree_s: 0.55,
                mot: "dauphins".into(),
                confiance: 0.8,
            },
        ];
        assert_eq!(
            mots_en_json(10_000, &mots),
            r#"[[10250,10650,"les"],[10700,11250,"dauphins"]]"#
        );
    }
}
