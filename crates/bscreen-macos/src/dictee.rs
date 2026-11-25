//! Le mode vocal d'Ask sur le Mac (25/11/2026) : la question dite au micro, transcrite sur le Mac
//! (reconnaissance d'Apple, comme les voix), et la réponse lue à voix haute.
//!
//! Le panneau Ask lit les touches lui-même (la dictée de macOS n'y entre pas) : un clic sur le
//! micro enregistre, un second arrête ; le fichier (dans les données chiffrées) est effacé dès la
//! transcription faite.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use bscreen_core::chemins;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_avf_audio::{AVAudioRecorder, AVFormatIDKey, AVNumberOfChannelsKey, AVSampleRateKey};
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSNumber, NSString, NSURL};

struct Enregistreur(Retained<AVAudioRecorder>);
// SAFETY : l'enregistreur n'est touché que sous le verrou (démarrer, arrêter), jamais en même temps.
unsafe impl Send for Enregistreur {}

static EN_COURS: Mutex<Option<Enregistreur>> = Mutex::new(None);
static LECTURE: Mutex<Option<Child>> = Mutex::new(None);

fn fichier() -> PathBuf {
    chemins::donnees().join("dictee.m4a")
}

/// Commence à enregistrer la question.
pub fn commencer() -> Result<()> {
    let mut en_cours = EN_COURS.lock().unwrap_or_else(|e| e.into_inner());
    if en_cours.is_some() {
        return Ok(());
    }
    arreter_lecture();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&fichier().to_string_lossy()));
    let reglages = NSMutableDictionary::<NSString, AnyObject>::new();
    let cle = |c: Option<&'static NSString>| c.ok_or_else(|| anyhow!("constante AVFoundation absente"));
    // SAFETY : constantes globales d'AVFAudio, lues seulement ; AAC mono 16 kHz.
    unsafe {
        reglages.insert(cle(AVFormatIDKey)?, &*NSNumber::numberWithUnsignedInt(0x6161_6320));
        reglages.insert(cle(AVSampleRateKey)?, &*NSNumber::numberWithDouble(16_000.0));
        reglages.insert(cle(AVNumberOfChannelsKey)?, &*NSNumber::numberWithInt(1));
    }
    let reglages: Retained<NSDictionary<NSString, AnyObject>> = Retained::into_super(reglages);
    // SAFETY : enregistreur standard vers un fichier à nous.
    let enregistreur = unsafe { AVAudioRecorder::initWithURL_settings_error(AVAudioRecorder::alloc(), &url, &reglages) }
        .map_err(|e| anyhow!("micro : {}", e.localizedDescription()))?;
    if !unsafe { enregistreur.prepareToRecord() && enregistreur.record() } {
        return Err(anyhow!("le micro n'enregistre pas"));
    }
    *en_cours = Some(Enregistreur(enregistreur));
    tracing::info!("Ask : question au micro");
    Ok(())
}

/// Arrête l'enregistrement et rend la question transcrite (sur le Mac). Bloque le temps de la
/// transcription : hors du fil principal.
pub fn finir() -> Result<String> {
    let enregistreur = EN_COURS.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(Enregistreur(enregistreur)) = enregistreur else { return Err(anyhow!("rien d'enregistré")) };
    // SAFETY : l'enregistreur qu'on vient de retirer, arrêté une fois.
    unsafe { enregistreur.stop() };
    drop(enregistreur);
    let chemin = fichier();
    let resultat = crate::transcription::Transcripteur::nouveau()
        .and_then(|t| t.transcrire(&chemin, &bscreen_core::reglages::Reglages::charger().vocabulaire));
    let _ = std::fs::remove_file(&chemin);
    let texte = resultat?.texte.trim().to_string();
    tracing::info!(caracteres = texte.chars().count(), "Ask : question transcrite");
    Ok(texte)
}

/// Lit un texte à voix haute (la voix du Mac) ; une nouvelle lecture remplace la précédente.
pub fn lire(texte: &str) {
    arreter_lecture();
    let propre = nettoyer(texte);
    if propre.is_empty() {
        return;
    }
    match Command::new("/usr/bin/say").arg("--").arg(propre).spawn() {
        Ok(enfant) => *LECTURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(enfant),
        Err(e) => tracing::warn!(%e, "lecture à voix haute impossible"),
    }
}

pub fn arreter_lecture() {
    if let Some(mut enfant) = LECTURE.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = enfant.kill();
        let _ = enfant.wait();
    }
}

/// Sans les renvois ([écran 20/11 21:56]), le gras ni les puces.
fn nettoyer(texte: &str) -> String {
    let mut sortie = String::new();
    let mut dans_renvoi = false;
    for c in texte.chars() {
        match c {
            '[' => dans_renvoi = true,
            ']' if dans_renvoi => dans_renvoi = false,
            '*' => {}
            _ if dans_renvoi => {}
            _ => sortie.push(c),
        }
    }
    sortie
        .lines()
        .map(|l| l.trim_start().trim_start_matches(['-', '•']).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn texte_a_lire() {
        assert_eq!(
            super::nettoyer("**Oui** : tu as vu [écran 20/11 21:56] la page.\n- un point\n"),
            "Oui : tu as vu  la page.\nun point"
        );
    }
}
