//! Recherche dans les images (phase 7.3) : SigLIP 2 (Google, licence Apache 2.0 ; « base », 256 px)
//! converti pour Core ML par `outils/coreml/convertir_siglip.py`. Pour ce qui n'est écrit nulle
//! part : « j'ai vu quand la voiture rouge ? ». Le modèle est multilingue : une question en
//! français marche telle quelle (essai du 22/11 : aussi bien qu'en anglais). MobileCLIP 2, prévu
//! d'abord, n'est permis que pour la recherche scientifique.
//!
//! - Une image par scène : une nouvelle app, fenêtre ou page ; la moitié de l'écran changée depuis
//!   la dernière regardée (au plus une toutes les 10 s : une vidéo change tout à chaque image) ;
//!   ou 5 minutes passées. Jamais les moments privés. Une scène qui n'a pas bougé (vecteur presque
//!   identique au précédent) n'est pas gardée.
//! - L'écran entier, écrasé à 256 × 256 comme à l'entraînement, donne un vecteur de 768 nombres
//!   (ramenés à des octets signés), calculé par la puce neuronale (≈ 10 ms) ; en fond, par
//!   rafales comme le sens (au plus toutes les 20 min, sur secteur ou Mac inactif, seulement s'il y
//!   a de nouvelles images), le passé compris.
//! - Une question : le texte (en minuscules, 64 jetons) donne un vecteur sur le processeur
//!   (≈ 8 ms, modèle chargé en 1 s ; sa table des mots est lue à part, voir [`crate::mots`]),
//!   comparé à ceux des images.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bscreen_core::chemins;
use bscreen_core::index_compact::IndexCompact;
use bscreen_core::image::ImageBgra;
use bscreen_store::{Base, Image};
use objc2::rc::Retained;
use objc2_core_ml::{MLComputeUnits, MLModel, MLModelConfiguration};
use objc2_foundation::{NSString, NSURL};

use crate::sens::{predire, quantifier, tableau_flottants};

/// Taille d'un vecteur.
pub const DIMENSION: usize = 768;
/// Côté de l'image donnée au modèle (pixels).
pub const COTE: u32 = 256;
const LONGUEUR_TEXTE: usize = 64;
const EOS: u32 = 1;
/// Appris par SigLIP 2 : la probabilité qu'une image corresponde à un texte vaut
/// σ(ECHELLE × cosinus + BIAIS).
pub const ECHELLE: f32 = 112.9012;
pub const BIAIS: f32 = -16.7718;
/// Un modèle est rendu après ce temps sans usage.
const GARDE_MODELE: Duration = Duration::from_secs(60);
/// L'intervalle des rafales et l'indexation sur batterie se règlent (voir `crate::vivants`).
const CLE_AVANCEMENT: &str = "images_derniere";
/// Une image plus récente que ça peut ne pas être encore lisible dans sa vidéo (fragments de 5 s).
const RECUL_MS: i64 = 30_000;
/// Une scène : la moitié de l'écran changée, au plus une toutes les 10 s ; un changement d'app,
/// de fenêtre ou de page (2 s au moins après la précédente) ; ou 5 min passées avec un peu de
/// changement.
const AIRE_SCENE: f64 = 0.5;
const ECART_SCENE_MS: i64 = 10_000;
const ECART_CLE_MS: i64 = 2_000;
const ECART_LONG_MS: i64 = 300_000;
const AIRE_LONGUE: f64 = 0.02;
/// Au-dessus de cette similarité avec la précédente du même écran, l'image n'apporte rien.
const MEME_SCENE: f32 = 0.97;

/// Les fichiers des modèles (dans l'app, ou assets/modeles en développement).
struct Fichiers {
    image: PathBuf,
    texte: PathBuf,
    /// La table des mots du modèle des textes (voir [`crate::mots`]).
    mots: PathBuf,
    jetons: PathBuf,
}

fn fichiers_modele() -> Option<Fichiers> {
    let dossier = crate::empreintes::dossier_modeles()?;
    let f = Fichiers {
        image: dossier.join("SigLIP2-Image.mlmodelc"),
        texte: dossier.join("SigLIP2-Texte.mlmodelc"),
        mots: dossier.join("SigLIP2-Texte.mots"),
        jetons: dossier.join("SigLIP2.tokenizer.json"),
    };
    (f.image.is_dir() && f.texte.is_dir() && f.mots.is_file() && f.jetons.is_file()).then_some(f)
}

/// Les modèles sont-ils dans cette installation ?
pub fn disponible() -> bool {
    fichiers_modele().is_some()
}

fn charger_modele(chemin: &Path, unites: MLComputeUnits) -> Result<Retained<MLModel>> {
    objc2::rc::autoreleasepool(|_| {
        let configuration = unsafe { MLModelConfiguration::new() };
        unsafe { configuration.setComputeUnits(unites) };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&chemin.to_string_lossy()));
        unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url, &configuration) }
            .map_err(|e| anyhow!("modèle {} : {}", chemin.display(), e.localizedDescription()))
    })
}

/// La probabilité, d'après SigLIP 2, qu'une image corresponde au texte (de leur cosinus).
pub fn probabilite(cosinus: f32) -> f32 {
    1.0 / (1.0 + (-(ECHELLE * cosinus + BIAIS)).exp())
}

/// Le modèle des images (puce neuronale : ≈ 10 ms par image).
pub struct EncodeurImage {
    modele: Retained<MLModel>,
}

impl EncodeurImage {
    pub fn charger() -> Result<Self> {
        let fichiers = fichiers_modele().ok_or_else(|| anyhow!("modèle d'images absent"))?;
        let debut = Instant::now();
        let modele = charger_modele(&fichiers.image, MLComputeUnits::All)?;
        tracing::info!(ms = debut.elapsed().as_millis(), "modèle d'images chargé");
        Ok(Self { modele })
    }

    /// Le vecteur (normé) d'une image de 256 × 256.
    pub fn vecteur(&self, image: &ImageBgra) -> Result<Vec<f32>> {
        let pixels = pixels_normes(image)?;
        objc2::rc::autoreleasepool(|_| {
            let tableau = tableau_flottants(&[1, 3, COTE as usize, COTE as usize], &pixels)?;
            let sortie = predire(&self.modele, &[("pixels", &tableau)], "vecteur")?;
            crate::empreintes::lire_tableau(&sortie)
        })
    }
}

/// BGRA 256 × 256 → RGB en trois plans, de -1 à 1 (ce que SigLIP a appris).
fn pixels_normes(image: &ImageBgra) -> Result<Vec<f32>> {
    if image.largeur != COTE || image.hauteur != COTE {
        bail!("image de {}×{} au lieu de {COTE}×{COTE}", image.largeur, image.hauteur);
    }
    let n = (COTE * COTE) as usize;
    let mut plans = vec![0f32; 3 * n];
    for (k, px) in image.pixels.chunks_exact(4).enumerate().take(n) {
        plans[k] = f32::from(px[2]) / 127.5 - 1.0;
        plans[n + k] = f32::from(px[1]) / 127.5 - 1.0;
        plans[2 * n + k] = f32::from(px[0]) / 127.5 - 1.0;
    }
    Ok(plans)
}

/// Le modèle des textes (processeur : ≈ 8 ms par question), sans sa table des mots, lue à part :
/// 164 Mo chargés au lieu de 565.
pub struct EncodeurTexte {
    modele: Retained<MLModel>,
    mots: crate::mots::TableDeMots,
    jetons: tokenizers::Tokenizer,
}

impl EncodeurTexte {
    pub fn charger() -> Result<Self> {
        let fichiers = fichiers_modele().ok_or_else(|| anyhow!("modèle d'images absent"))?;
        let debut = Instant::now();
        // Le processeur seul : chargé en 1 s (la puce neuronale demande 18 s de préparation) et
        // aussi rapide pour une phrase.
        let modele = charger_modele(&fichiers.texte, MLComputeUnits::CPUOnly)?;
        let mots = crate::mots::TableDeMots::ouvrir(&fichiers.mots)?;
        if mots.dimension() != DIMENSION {
            bail!("table des mots des images : {} nombres par mot au lieu de {DIMENSION}", mots.dimension());
        }
        let mut jetons = tokenizers::Tokenizer::from_file(&fichiers.jetons).map_err(|e| anyhow!("tokeniseur : {e}"))?;
        // Le fichier remplit à 64 jetons de lui-même : on borne et on remplit ici (EOS, puis zéros).
        jetons.with_padding(None);
        jetons
            .with_truncation(None)
            .map_err(|e| anyhow!("tokeniseur : {e}"))?;
        tracing::info!(ms = debut.elapsed().as_millis(), "modèle des textes d'images chargé");
        Ok(Self { modele, mots, jetons })
    }

    /// Les jetons d'un texte : en minuscules (comme à l'entraînement), EOS à la fin, 64 au plus.
    pub fn jetons(&self, texte: &str) -> Result<Vec<u32>> {
        let encodage = self.jetons.encode(texte.to_lowercase(), false).map_err(|e| anyhow!("tokeniseur : {e}"))?;
        let mut ids = encodage.get_ids().to_vec();
        ids.truncate(LONGUEUR_TEXTE - 1);
        ids.push(EOS);
        Ok(ids)
    }

    /// Le vecteur (normé) d'un texte.
    pub fn vecteur(&self, texte: &str) -> Result<Vec<f32>> {
        let ids = self.jetons(texte)?;
        // Les lignes des jetons, puis celles du jeton 0 jusqu'à 64 (SigLIP lit aussi le remplissage).
        let lignes = self.mots.lignes_de(&ids, LONGUEUR_TEXTE, 0)?;
        objc2::rc::autoreleasepool(|_| {
            let tableau = tableau_flottants(&[1, LONGUEUR_TEXTE, DIMENSION], &lignes)?;
            let sortie = predire(&self.modele, &[("inputs_embeds", &tableau)], "vecteur")?;
            crate::empreintes::lire_tableau(&sortie)
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Les images à regarder : une par scène.

/// La dernière scène regardée d'un écran.
struct Scene {
    quand_ms: i64,
    cle: String,
    aire: f64,
}

/// Le choix des images à regarder, au fil des images enregistrées (dans l'ordre).
#[derive(Default)]
pub struct Selection {
    par_ecran: HashMap<u32, Scene>,
}

impl Selection {
    /// Cette image ouvre-t-elle une nouvelle scène ?
    pub fn retenir(&mut self, image: &Image) -> bool {
        if image.privee {
            // Après un moment privé, ce qui revient est une nouvelle scène.
            self.par_ecran.remove(&image.ecran);
            return false;
        }
        let cle = format!(
            "{}|{}|{}",
            image.app_bundle.as_deref().unwrap_or_default(),
            image.fenetre.as_deref().unwrap_or_default(),
            image.url.as_deref().unwrap_or_default()
        );
        let retenue = match self.par_ecran.get_mut(&image.ecran) {
            None => true,
            Some(scene) => {
                scene.aire += image.aire;
                let ecart = image.quand_ms - scene.quand_ms;
                (scene.cle != cle && ecart >= ECART_CLE_MS)
                    || (scene.aire >= AIRE_SCENE && ecart >= ECART_SCENE_MS)
                    || (scene.aire >= AIRE_LONGUE && ecart >= ECART_LONG_MS)
            }
        };
        if retenue {
            self.par_ecran.insert(image.ecran, Scene { quand_ms: image.quand_ms, cle, aire: 0.0 });
        }
        retenue
    }
}

/// Où en est l'indexation : le choix des scènes, et le dernier vecteur gardé de chaque écran.
#[derive(Default)]
pub struct Indexation {
    selection: Selection,
    derniers: HashMap<u32, Vec<f32>>,
}

impl Indexation {
    /// Garde-t-on ce vecteur ? (pas s'il redit le précédent du même écran)
    fn nouveau(&mut self, ecran: u32, vecteur: &[f32]) -> bool {
        let pareil = self
            .derniers
            .get(&ecran)
            .is_some_and(|v| v.iter().zip(vecteur).map(|(a, b)| a * b).sum::<f32>() >= MEME_SCENE);
        if !pareil {
            self.derniers.insert(ecran, vecteur.to_vec());
        }
        !pareil
    }
}

/// Y a-t-il des images enregistrées pas encore parcourues (celles d'au moins 30 s) ?
fn a_regarder() -> Result<bool> {
    let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).context("base")?;
    let depuis: i64 = base.meta(CLE_AVANCEMENT)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    Ok(base.images_apres(depuis, 1)?.first().is_some_and(|i| i.quand_ms <= chrono_ms() - RECUL_MS))
}

/// Regarde jusqu'à `lot` images enregistrées (celles d'au moins 30 s) ; rend le nombre parcouru
/// (0 : tout est à jour).
pub fn indexer_un_lot(encodeur: &EncodeurImage, etat: &mut Indexation, lot: usize) -> Result<usize> {
    let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).context("base")?;
    let depuis: i64 = base.meta(CLE_AVANCEMENT)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let limite = chrono_ms() - RECUL_MS;
    let images: Vec<Image> = base.images_apres(depuis, lot)?.into_iter().take_while(|i| i.quand_ms <= limite).collect();
    let mut gardees = 0;
    for image in &images {
        if !etat.selection.retenir(image) {
            continue;
        }
        let fichier = chemins::donnees().join(&image.fichier);
        let vecteur = match crate::lecture::image_enregistree(&fichier, image.position_ms, Some((COTE, COTE)))
            .and_then(|i| encodeur.vecteur(&i))
        {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(%e, image = image.id, "image pas regardée");
                continue;
            }
        };
        if etat.nouveau(image.ecran, &vecteur) {
            let (echelle, octets) = quantifier(&vecteur);
            base.ranger_vue(image.id, echelle, &octets)?;
            gardees += 1;
        }
    }
    if let Some(derniere) = images.last() {
        base.noter_meta(CLE_AVANCEMENT, &derniere.id.to_string())?;
    }
    if !images.is_empty() {
        tracing::debug!(parcourues = images.len(), gardees, "images regardées");
    }
    Ok(images.len())
}

fn chrono_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Le fil des modèles.

enum Demande {
    Question(String, Sender<Option<Vec<f32>>>),
    Prechauffer,
}

static FIL: OnceLock<Mutex<Sender<Demande>>> = OnceLock::new();

/// Lance le fil (une fois) ; `indexation` faux : seulement répondre aux questions (le serveur
/// MCP ; c'est l'app qui regarde les images).
pub fn demarrer_avec(indexation: bool) {
    if !disponible() {
        tracing::info!("recherche dans les images : modèle absent de cette installation");
        return;
    }
    FIL.get_or_init(|| {
        let (envoi, reception) = mpsc::channel();
        let _ = std::thread::Builder::new().name("bscreen-images".into()).spawn(move || boucle(reception, indexation));
        Mutex::new(envoi)
    });
}

fn envoyer(demande: Demande) -> bool {
    FIL.get().is_some_and(|f| f.lock().map(|e| e.send(demande).is_ok()).unwrap_or(false))
}

/// Charge le modèle des textes d'avance (frise ou Ask ouverts) : la première recherche sera rapide.
pub fn prechauffer() {
    envoyer(Demande::Prechauffer);
}

/// Le vecteur d'une question ; `None` si le modèle n'a pas répondu à temps (il se charge).
pub fn vecteur_question(texte: &str, attente: Duration) -> Option<Vec<f32>> {
    if !crate::vivants::recherche_images() {
        return None;
    }
    let (envoi, reception) = mpsc::channel();
    if !envoyer(Demande::Question(texte.to_string(), envoi)) {
        return None;
    }
    reception.recv_timeout(attente).ok().flatten()
}

fn boucle(reception: Receiver<Demande>, indexation: bool) {
    // Priorité « utilitaire » : ne gêne jamais ce que fait Julien.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    let mut texte: Option<EncodeurTexte> = None;
    let mut image: Option<EncodeurImage> = None;
    let (mut usage_texte, mut usage_image) = (Instant::now(), Instant::now());
    let mut etat = Indexation::default();
    let mut derniere_rafale: Option<Instant> = None;
    let mut en_rafale = false;
    let charger_texte = |texte: &mut Option<EncodeurTexte>| {
        if texte.is_none() {
            match EncodeurTexte::charger() {
                Ok(t) => *texte = Some(t),
                Err(e) => tracing::warn!(%e, "modèle des textes d'images pas chargé"),
            }
        }
    };
    loop {
        let attente = if en_rafale { Duration::from_millis(1) } else { Duration::from_secs(30) };
        match reception.recv_timeout(attente) {
            Ok(Demande::Question(question, reponse)) => {
                charger_texte(&mut texte);
                usage_texte = Instant::now();
                let _ = reponse.send(texte.as_ref().and_then(|t| t.vecteur(&question).ok()));
                continue;
            }
            Ok(Demande::Prechauffer) => {
                // Seulement le modèle des textes : regarder des images (les décoder) pendant que la
                // frise décode ses vignettes la ralentirait ; les rafales de fond suffisent.
                let froid = texte.is_none();
                charger_texte(&mut texte);
                if froid && let Some(t) = &texte {
                    let _ = t.vecteur("bonjour"); // premier calcul à blanc
                }
                precharger_index();
                usage_texte = Instant::now();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let rafale_due = indexation
            && crate::vivants::recherche_images()
            && crate::sens::moment_propice()
            && derniere_rafale.is_none_or(|t| t.elapsed() >= crate::vivants::intervalle_rafales());
        if !en_rafale && rafale_due {
            derniere_rafale = Some(Instant::now());
            // Le modèle n'est chargé que s'il y a de nouvelles images.
            en_rafale = a_regarder().unwrap_or_else(|err| {
                tracing::warn!(%err, "recherche dans les images : base illisible");
                false
            });
        }
        if en_rafale {
            if image.is_none() {
                match EncodeurImage::charger() {
                    Ok(e) => image = Some(e),
                    Err(e) => tracing::warn!(%e, "modèle d'images pas chargé"),
                }
            }
            usage_image = Instant::now();
            let fini = match image.as_ref().map(|e| indexer_un_lot(e, &mut etat, 32)) {
                Some(Ok(0)) | None => true,
                Some(Ok(_)) => false,
                Some(Err(err)) => {
                    tracing::warn!(%err, "recherche dans les images : indexation interrompue");
                    true
                }
            };
            if fini {
                en_rafale = false;
                derniere_rafale = Some(Instant::now());
            } else {
                continue;
            }
        }
        if texte.is_some() && usage_texte.elapsed() > GARDE_MODELE {
            objc2::rc::autoreleasepool(|_| texte = None);
        }
        if image.is_some() && usage_image.elapsed() > GARDE_MODELE {
            objc2::rc::autoreleasepool(|_| image = None);
            tracing::info!("modèle d'images rendu (inutilisé)");
            crate::moteur::rendre_la_memoire();
        }
    }
}

/// Combien d'images ont un vecteur.
pub fn etat_index() -> Option<u64> {
    Base::ouvrir(&chemins::donnees().join("index.sqlite")).ok()?.compter_vues().ok()
}

// ---------------------------------------------------------------------------------------------
// L'index en mémoire : seulement les codes (voir `bscreen_core::index_compact`), les meilleurs
// candidats départagés par leurs vrais vecteurs, relus dans la base.

/// Candidats départagés à chaque recherche. La question (un texte) ne ressemble pas aux images :
/// elle est gardée en vrais nombres face aux codes des images ([`IndexCompact::candidats_fins`]) ;
/// 99,9 % des 20 premiers retrouvés avec 2 000 candidats (99,3 % avec 1 000) sur les recherches
/// de Julien, 8 652 images, 24/11/2026.
const CANDIDATS: usize = 2000;

static INDEX: Mutex<Option<IndexCompact>> = Mutex::new(None);

/// Les images les plus proches d'un vecteur de question : (image, cosinus), du plus proche au
/// moins proche, au-dessus de `seuil`, `n` au plus. Les images effacées depuis peuvent y être :
/// l'appelant ne garde que celles qui existent encore (et pas les privées).
pub fn plus_proches(base: &Base, question: &[f32], n: usize, seuil: f32) -> Result<Vec<(i64, f32)>> {
    let candidats = {
        let mut garde = INDEX.lock().map_err(|_| anyhow!("index des images bloqué"))?;
        let index = garde.get_or_insert_with(IndexCompact::default);
        mettre_a_jour(index, base)?;
        index.candidats_fins(question, CANDIDATS)
    };
    let mut liste: Vec<(i64, f32)> = base
        .vues_par_ids(&candidats)?
        .into_iter()
        .filter(|v| v.vecteur.len() == DIMENSION)
        .filter_map(|v| {
            let cosinus = v.vecteur.iter().zip(question).map(|(&a, &b)| f32::from(a) * b).sum::<f32>() * v.echelle;
            (cosinus >= seuil).then_some((v.image, cosinus))
        })
        .collect();
    liste.sort_by(|a, b| b.1.total_cmp(&a.1));
    liste.dedup_by_key(|p| p.0);
    liste.truncate(n);
    Ok(liste)
}

/// Ajoute à l'index les codes rangés depuis sa dernière lecture.
fn mettre_a_jour(index: &mut IndexCompact, base: &Base) -> Result<()> {
    loop {
        let nouveaux = base.codes_depuis(false, index.dernier().unwrap_or(0), 50_000)?;
        if nouveaux.is_empty() {
            return Ok(());
        }
        for (id, code) in nouveaux {
            index.ajouter(id, &code);
        }
    }
}

/// Lit l'index d'avance (frise ou Ask ouverts), pour que la première recherche n'attende pas.
fn precharger_index() {
    let Ok(base) = Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite")) else { return };
    if let Ok(mut garde) = INDEX.lock() {
        let _ = mettre_a_jour(garde.get_or_insert_with(IndexCompact::default), &base);
    }
}

/// Mémoire prise par l'index (octets), s'il est chargé.
pub fn memoire_index() -> usize {
    INDEX.lock().ok().and_then(|g| g.as_ref().map(IndexCompact::octets)).unwrap_or(0)
}

/// Oublie l'index en mémoire (après un effacement : il sera relu).
pub fn oublier_index() {
    if let Ok(mut garde) = INDEX.lock() {
        *garde = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: i64, quand_ms: i64, app: &str, aire: f64, privee: bool) -> Image {
        Image {
            id,
            morceau: 1,
            fichier: String::new(),
            quand_ms,
            position_ms: 0,
            ecran: 1,
            app_bundle: Some(app.to_string()),
            app_nom: None,
            aire,
            declenchee: false,
            largeur: 10,
            hauteur: 10,
            fenetre: None,
            url: None,
            privee,
        }
    }

    #[test]
    fn une_image_par_scene() {
        let mut s = Selection::default();
        // La première, puis rien tant que l'écran change peu ; un changement d'app : une scène.
        assert!(s.retenir(&image(1, 0, "safari", 0.0, false)));
        assert!(!s.retenir(&image(2, 4_000, "safari", 0.1, false)));
        assert!(s.retenir(&image(3, 6_000, "messages", 0.1, false)));
        // Une vidéo : tout change à chaque image, mais une toutes les 10 s au plus.
        let retenues = (0..30).filter(|k| s.retenir(&image(10 + k, 8_000 + k * 2_000, "messages", 1.0, false))).count();
        assert_eq!(retenues, 6, "60 s de vidéo : une image toutes les 10 s");
        // Rien de privé ; après, une nouvelle scène.
        assert!(!s.retenir(&image(50, 100_000, "safari", 1.0, true)));
        assert!(s.retenir(&image(51, 101_000, "safari", 0.0, false)));
        // Longtemps sur la même page avec un peu de changement : une image toutes les 5 min.
        assert!(!s.retenir(&image(52, 200_000, "safari", 0.03, false)));
        assert!(s.retenir(&image(53, 401_000, "safari", 0.0, false)));
    }

    #[test]
    fn probabilites() {
        assert!(probabilite(0.0) < 1e-6);
        assert!((probabilite(-BIAIS / ECHELLE) - 0.5).abs() < 1e-4);
    }

    /// Avec les vrais modèles (s'ils sont là) : les jetons attendus par SigLIP 2, et une question
    /// en français plus proche de la même en anglais que d'une autre.
    #[test]
    fn jetons_et_questions_du_vrai_modele() {
        let Ok(texte) = EncodeurTexte::charger() else { return };
        assert_eq!(texte.jetons("La voiture rouge").unwrap(), vec![522, 28646, 20826, 1]);
        let fr = texte.vecteur("la voiture rouge").unwrap();
        let en = texte.vecteur("a red car").unwrap();
        let autre = texte.vecteur("un chat qui dort").unwrap();
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(cos(&fr, &en) > cos(&fr, &autre) + 0.1, "{} / {}", cos(&fr, &en), cos(&fr, &autre));
    }
}
