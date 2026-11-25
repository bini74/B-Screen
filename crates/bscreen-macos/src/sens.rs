//! Recherche par le sens (phase 7) : EmbeddingGemma converti pour Core ML (précision mixte,
//! identique à l'original ; voir `outils/coreml/convertir.py`), sans sa table des mots, lue à part
//! (`EmbeddingGemma.mots`, voir [`crate::mots`]) : 204 Mo chargés au lieu de 600.
//!
//! - Un fil « bscreen-sens » garde le modèle (≈ 600 Mo) : il est chargé à la demande (frise
//!   ouverte, indexation) puis rendu après 1 minute sans usage (≈ 1,6 Go ; rechargé en 4-5 s).
//! - Indexation en fond par rafales (au plus toutes les 20 min sur secteur, Mac inactif sur
//!   batterie, et seulement s'il y a du nouveau ; tout de suite à l'ouverture de la frise) : chaque
//!   texte vu ou dit d'au moins 3 mots est découpé en morceaux de 128 jetons au plus, chaque
//!   morceau reçoit un vecteur (768 nombres ramenés à des octets signés).
//! - Recherche : le code de la question (le signe de ses nombres) est comparé à ceux de tous les
//!   vecteurs, gardés en mémoire (96 octets chacun) ; les 1 000 meilleurs sont départagés par leurs
//!   vrais vecteurs, relus dans la base.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bscreen_core::chemins;
use bscreen_core::index_compact::IndexCompact;
use bscreen_store::Base;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel, MLModelConfiguration, MLMultiArray,
    MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

/// Taille d'un vecteur.
pub const DIMENSION: usize = 768;
/// La longueur fixe du modèle (jetons, BOS et EOS compris), complétée au besoin : avec plusieurs
/// longueurs au choix, la carte graphique tombait à 116 ms par texte au lieu de 17 (24/11/2026).
/// Avant le 24/11, les morceaux faisaient 256 jetons (les vecteurs rangés alors restent bons).
const LONGUEUR: usize = 128;
/// Au plus 16 morceaux par texte (≈ 2 000 jetons : un très long texte n'est indexé que sur son début).
const MORCEAUX_MAX: usize = 16;
const PREFIXE_TEXTE: &str = "title: none | text: ";
const PREFIXE_QUESTION: &str = "task: search result | query: ";
const BOS: u32 = 2;
const EOS: u32 = 1;
/// Le modèle est rendu après ce temps sans usage.
const GARDE_MODELE: Duration = Duration::from_secs(60);
/// L'intervalle des rafales et l'indexation sur batterie se règlent (voir `crate::vivants`).
pub use bscreen_core::index_compact::{MOTS_MIN, a_comprendre};
/// L'indexation de fond attend que le Mac soit inactif depuis ce temps.
const INACTIF_DEPUIS_S: f64 = 120.0;
const CLE_AVANCEMENT: &str = "sens_dernier_texte";

/// Où sont le modèle, sa table des mots et son tokeniseur (dans l'app, ou assets/modeles en
/// développement).
fn fichiers_modele() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let dossier = crate::empreintes::dossier_modeles()?;
    let modele = dossier.join("EmbeddingGemma.mlmodelc");
    let mots = dossier.join("EmbeddingGemma.mots");
    let jetons = dossier.join("EmbeddingGemma.tokenizer.json");
    (modele.is_dir() && mots.is_file() && jetons.is_file()).then_some((modele, mots, jetons))
}

/// Le modèle disponible dans cette installation ?
pub fn disponible() -> bool {
    fichiers_modele().is_some()
}

/// Le modèle chargé : Core ML (sans sa table des mots) + la table des mots + le tokeniseur.
pub struct Encodeur {
    modele: Retained<MLModel>,
    mots: crate::mots::TableDeMots,
    jetons: tokenizers::Tokenizer,
}

impl Encodeur {
    pub fn charger() -> Result<Self> {
        let (modele, mots, jetons) = fichiers_modele().ok_or_else(|| anyhow!("modèle de sens absent"))?;
        objc2::rc::autoreleasepool(|_| Self::charger_depuis(&modele, &mots, &jetons))
    }

    pub fn charger_depuis(modele: &Path, mots: &Path, jetons: &Path) -> Result<Self> {
        let debut = Instant::now();
        let configuration = unsafe { MLModelConfiguration::new() };
        // GPU : mesuré 12 à 24 ms par texte ; le moteur neuronal (« All ») gère mal les longueurs
        // multiples et retombe à 90-320 ms.
        // (Processeur seul essayé le 25/11/2026 : vecteurs nuls avec ce modèle, et pas moins de mémoire.)
        unsafe { configuration.setComputeUnits(MLComputeUnits::CPUAndGPU) };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&modele.to_string_lossy()));
        let modele = unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url, &configuration) }
            .map_err(|e| anyhow!("modèle de sens : {}", e.localizedDescription()))?;
        let mots = crate::mots::TableDeMots::ouvrir(mots)?;
        if mots.dimension() != DIMENSION {
            return Err(anyhow!("table des mots du sens : {} nombres par mot au lieu de {DIMENSION}", mots.dimension()));
        }
        let jetons = tokenizers::Tokenizer::from_file(jetons).map_err(|e| anyhow!("tokeniseur : {e}"))?;
        tracing::info!(ms = debut.elapsed().as_millis(), "modèle de sens chargé");
        Ok(Self { modele, mots, jetons })
    }

    fn jetons(&self, texte: &str) -> Result<Vec<u32>> {
        let encodage = self.jetons.encode(texte, false).map_err(|e| anyhow!("tokeniseur : {e}"))?;
        Ok(encodage.get_ids().to_vec())
    }

    /// Le vecteur d'une suite de jetons déjà bornée (≤ 128 avec BOS et EOS).
    fn calculer(&self, ids: &[u32]) -> Result<Vec<f32>> {
        // Bassin de libération : sans lui, les objets temporaires de Core ML s'accumulent dans
        // ce fil (qui n'en a pas) à chaque calcul.
        objc2::rc::autoreleasepool(|_| {
            let longueur = LONGUEUR;
            // Les lignes des jetons, puis celles du remplissage (jeton 0, masqué) jusqu'à la longueur.
            let lignes = self.mots.lignes_de(&ids[..ids.len().min(longueur)], longueur, 0)?;
            let entrees = tableau_flottants(&[1, longueur, DIMENSION], &lignes)?;
            let masque = tableau_entiers(longueur, ids.iter().map(|_| 1))?;
            let sortie = predire(&self.modele, &[("inputs_embeds", &entrees), ("attention_mask", &masque)], "vecteur")?;
            lire(&sortie)
        })
    }

    /// Le vecteur d'une question (normalisé).
    pub fn question(&self, texte: &str) -> Result<Vec<f32>> {
        let mut ids = vec![BOS];
        ids.extend(self.jetons(&format!("{PREFIXE_QUESTION}{texte}"))?);
        ids.truncate(LONGUEUR - 1);
        ids.push(EOS);
        self.calculer(&ids)
    }

    /// Les vecteurs d'un texte : un par morceau de 128 jetons au plus.
    pub fn texte(&self, texte: &str) -> Result<Vec<Vec<f32>>> {
        let prefixe = self.jetons(PREFIXE_TEXTE)?;
        let corps = self.jetons(texte)?;
        let place = LONGUEUR - 2 - prefixe.len();
        let mut vecteurs = Vec::new();
        for morceau in corps.chunks(place).take(MORCEAUX_MAX) {
            let mut ids = Vec::with_capacity(morceau.len() + prefixe.len() + 2);
            ids.push(BOS);
            ids.extend_from_slice(&prefixe);
            ids.extend_from_slice(morceau);
            ids.push(EOS);
            vecteurs.push(self.calculer(&ids)?);
        }
        Ok(vecteurs)
    }
}

/// Un vecteur en octets signés + son échelle (valeur ≈ octet × échelle).
pub fn quantifier(v: &[f32]) -> (f32, Vec<i8>) {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let echelle = if max > 0.0 { max / 127.0 } else { 1.0 };
    (echelle, v.iter().map(|x| (x / echelle).round().clamp(-127.0, 127.0) as i8).collect())
}

// ---------------------------------------------------------------------------------------------
// Le fil du modèle.

enum Demande {
    Question(String, Sender<Option<Vec<f32>>>),
    Prechauffer,
}

static FIL: OnceLock<Mutex<Sender<Demande>>> = OnceLock::new();

/// Lance le fil du modèle (une fois) : indexation de fond et réponses aux questions.
pub fn demarrer() {
    demarrer_avec(true);
}

/// `indexation` faux : seulement répondre aux questions (le serveur MCP ; c'est l'app qui indexe).
pub fn demarrer_avec(indexation: bool) {
    if !disponible() {
        tracing::info!("recherche par le sens : modèle absent de cette installation");
        return;
    }
    FIL.get_or_init(|| {
        let (envoi, reception) = mpsc::channel();
        let _ = std::thread::Builder::new().name("bscreen-sens".into()).spawn(move || boucle(reception, indexation));
        Mutex::new(envoi)
    });
}

fn envoyer(demande: Demande) -> bool {
    FIL.get().is_some_and(|f| f.lock().map(|e| e.send(demande).is_ok()).unwrap_or(false))
}

/// Charge le modèle d'avance (frise ouverte) pour que la première recherche soit rapide.
pub fn prechauffer() {
    envoyer(Demande::Prechauffer);
}

/// Le vecteur d'une question ; `None` si le modèle n'a pas répondu à temps (il se charge).
pub fn vecteur_question(texte: &str, attente: Duration) -> Option<Vec<f32>> {
    if !crate::vivants::recherche_sens() {
        return None;
    }
    let (envoi, reception) = mpsc::channel();
    if !envoyer(Demande::Question(texte.to_string(), envoi)) {
        return None;
    }
    reception.recv_timeout(attente).ok().flatten()
}

pub(crate) fn sur_secteur() -> bool {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> *const std::ffi::c_void;
        fn IOPSGetProvidingPowerSourceType(blob: *const std::ffi::c_void) -> *const std::ffi::c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(s: *const std::ffi::c_void, buf: *mut u8, taille: isize, codage: u32) -> bool;
        fn CFRelease(p: *const std::ffi::c_void);
    }
    // SAFETY : fonctions IOKit documentées ; le blob est libéré, la chaîne ne nous appartient pas.
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return true; // pas d'info : un Mac de bureau
        }
        let genre = IOPSGetProvidingPowerSourceType(blob);
        let mut tampon = [0u8; 64];
        let lu = !genre.is_null() && CFStringGetCString(genre, tampon.as_mut_ptr(), 64, 0x0800_0100);
        CFRelease(blob);
        if !lu {
            return true;
        }
        let fin = tampon.iter().position(|&b| b == 0).unwrap_or(0);
        &tampon[..fin] == b"AC Power"
    }
}

/// Le moment est-il bon pour indexer ? Sur secteur : toujours (par petits lots, en basse
/// priorité, pour que ce qui vient d'être vu soit cherchable tout de suite) ; sur batterie :
/// seulement quand le Mac est inactif.
pub(crate) fn moment_propice() -> bool {
    sur_secteur() || (crate::vivants::indexation_sur_batterie() && crate::moteur::secondes_d_inactivite() >= INACTIF_DEPUIS_S)
}

fn boucle(reception: Receiver<Demande>, indexation: bool) {
    let moment_propice = || indexation && moment_propice();
    // Priorité « utilitaire » : ne gêne jamais ce que fait Julien.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    if indexation {
        match Base::ouvrir(&chemins::donnees().join("index.sqlite")).and_then(|b| b.completer_codes()) {
            Ok(0) => {}
            Ok(n) => tracing::info!(n, "codes de l'index compact rattrapés"),
            Err(e) => tracing::warn!(%e, "codes de l'index compact pas rattrapés"),
        }
    }
    let mut encodeur: Option<Encodeur> = None;
    let mut dernier_usage = Instant::now();
    // Rafales : au plus une toutes les 20 min hors frise ; la frise ouverte en déclenche une tout de suite.
    let mut derniere_rafale: Option<Instant> = None;
    let mut rattraper = false;
    let mut en_rafale = false;
    // La base, ouverte le temps d'une rafale.
    let mut base: Option<Base> = None;
    let charger = |encodeur: &mut Option<Encodeur>| {
        if encodeur.is_none() {
            match Encodeur::charger() {
                Ok(e) => *encodeur = Some(e),
                Err(e) => tracing::warn!(%e, "modèle de sens pas chargé"),
            }
        }
    };
    loop {
        // En pleine rafale, on ne fait que jeter un œil aux demandes entre deux lots.
        let attente = if en_rafale { Duration::from_millis(1) } else { Duration::from_secs(30) };
        match reception.recv_timeout(attente) {
            Ok(Demande::Question(texte, reponse)) => {
                charger(&mut encodeur);
                dernier_usage = Instant::now();
                let v = encodeur.as_ref().and_then(|e| e.question(&texte).ok());
                let _ = reponse.send(v);
                continue;
            }
            Ok(Demande::Prechauffer) => {
                let froid = encodeur.is_none();
                charger(&mut encodeur);
                if froid && let Some(e) = &encodeur {
                    // Un premier calcul à blanc : la carte graphique prépare ses programmes une
                    // fois, et la première vraie question ne le paie pas.
                    let _ = e.question("bonjour");
                }
                precharger_index();
                dernier_usage = Instant::now();
                rattraper = indexation;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let rafale_due = crate::vivants::recherche_sens()
            && moment_propice()
            && derniere_rafale.is_none_or(|t| t.elapsed() >= crate::vivants::intervalle_rafales());
        if !en_rafale && (rattraper || rafale_due) {
            derniere_rafale = Some(Instant::now());
            // Le modèle n'est chargé que s'il y a vraiment un texte à comprendre (les textes trop
            // courts sont passés sans lui).
            base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).ok();
            en_rafale = match base.as_ref().map(travail_en_attente) {
                Some(Ok(oui)) => oui,
                Some(Err(err)) => {
                    tracing::warn!(%err, "indexation par le sens : base illisible");
                    false
                }
                None => false,
            };
            if en_rafale {
                dernier_usage = Instant::now();
            } else {
                rattraper = false;
                base = None;
            }
        }
        if en_rafale {
            charger(&mut encodeur);
            // Frise ouverte : un texte à la fois, pour qu'une question n'attende jamais un lot entier.
            let lot = if rattraper { 1 } else { 16 };
            let fini = match (encodeur.as_ref(), base.as_ref()) {
                (Some(e), Some(b)) => match indexer_un_lot_dans(e, b, lot) {
                    Ok(0) => true,
                    Ok(_) => false,
                    Err(err) => {
                        tracing::warn!(%err, "indexation par le sens interrompue");
                        true
                    }
                },
                _ => true,
            };
            if fini {
                en_rafale = false;
                rattraper = false;
                base = None;
            } else {
                continue;
            }
        }
        // Le modèle (≈ 1,6 Go en mémoire) est rendu vite : il se recharge en 4 à 5 s.
        if encodeur.is_some() && !en_rafale && dernier_usage.elapsed() > GARDE_MODELE {
            objc2::rc::autoreleasepool(|_| encodeur = None);
            tracing::info!("modèle de sens rendu (inutilisé)");
            crate::moteur::rendre_la_memoire();
        }
    }
}

/// Où en est l'index du sens : (textes compris, textes en attente).
pub fn etat_index() -> Option<(u64, u64)> {
    let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).ok()?;
    let depuis: i64 = base.meta(CLE_AVANCEMENT).ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
    base.compter_sens(depuis).ok()
}

fn avancement(base: &Base) -> Result<i64> {
    Ok(base.meta(CLE_AVANCEMENT)?.and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// Passe (sans le modèle) les textes trop courts en attente ; rend vrai s'il reste un texte à
/// comprendre.
fn travail_en_attente(base: &Base) -> Result<bool> {
    loop {
        let textes = base.textes_sans_sens(avancement(base)?, 512)?;
        let Some((dernier, _)) = textes.last() else { return Ok(false) };
        match textes.iter().position(|(_, contenu)| a_comprendre(contenu)) {
            Some(0) => return Ok(true),
            Some(i) => {
                base.noter_meta(CLE_AVANCEMENT, &textes[i - 1].0.to_string())?;
                return Ok(true);
            }
            None => base.noter_meta(CLE_AVANCEMENT, &dernier.to_string())?,
        }
    }
}

/// Indexe jusqu'à `lot` textes ; rend le nombre parcouru (0 = tout est à jour).
pub fn indexer_un_lot(encodeur: &Encodeur, lot: usize) -> Result<usize> {
    let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).context("base")?;
    indexer_un_lot_dans(encodeur, &base, lot)
}

fn indexer_un_lot_dans(encodeur: &Encodeur, base: &Base, lot: usize) -> Result<usize> {
    let textes = base.textes_sans_sens(avancement(base)?, lot)?;
    let mut compris = 0;
    for (id, contenu) in &textes {
        if a_comprendre(contenu) {
            let vecteurs: Vec<(f32, Vec<i8>)> = encodeur.texte(contenu)?.iter().map(|v| quantifier(v)).collect();
            base.ranger_sens(*id, &vecteurs)?;
            compris += 1;
        }
        base.noter_meta(CLE_AVANCEMENT, &id.to_string())?;
    }
    if compris > 0 {
        tracing::debug!(textes = compris, "textes indexés par le sens");
    }
    Ok(textes.len())
}

// ---------------------------------------------------------------------------------------------
// L'index en mémoire : seulement les codes (le signe de chaque nombre, 96 octets par vecteur, voir
// `bscreen_core::index_compact`) ; les meilleurs candidats sont départagés par leurs vrais vecteurs,
// relus dans la base.

/// Candidats départagés à chaque recherche : les 20 premiers sont tous retrouvés (100 % sur les
/// 76 recherches tapées par Julien, 76 528 vecteurs, 24/11/2026 ; 99,9 % avec 200).
const CANDIDATS: usize = 1000;

static INDEX: Mutex<Option<IndexCompact>> = Mutex::new(None);

/// Les textes les plus proches d'un vecteur de question : (texte, similarité), du plus proche au
/// moins proche, au-dessus de `seuil`, `n` au plus.
pub fn plus_proches(base: &Base, question: &[f32], n: usize, seuil: f32) -> Result<Vec<(i64, f32)>> {
    let candidats = {
        let mut garde = INDEX.lock().map_err(|_| anyhow!("index de sens bloqué"))?;
        let index = garde.get_or_insert_with(IndexCompact::default);
        mettre_a_jour(index, base)?;
        index.candidats(&bscreen_core::index_compact::code(question), CANDIDATS)
    };
    let mut meilleurs: std::collections::HashMap<i64, f32> = std::collections::HashMap::new();
    for s in base.sens_par_ids(&candidats)? {
        if s.vecteur.len() != DIMENSION {
            continue;
        }
        let produit: f32 = s.vecteur.iter().zip(question).map(|(&a, &b)| f32::from(a) * b).sum::<f32>() * s.echelle;
        if produit >= seuil {
            let m = meilleurs.entry(s.texte).or_insert(f32::MIN);
            *m = m.max(produit);
        }
    }
    let mut liste: Vec<(i64, f32)> = meilleurs.into_iter().collect();
    liste.sort_by(|a, b| b.1.total_cmp(&a.1));
    liste.truncate(n);
    Ok(liste)
}

/// Ajoute à l'index les codes rangés depuis sa dernière lecture.
fn mettre_a_jour(index: &mut IndexCompact, base: &Base) -> Result<()> {
    loop {
        let nouveaux = base.codes_depuis(true, index.dernier().unwrap_or(0), 50_000)?;
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

// ---------------------------------------------------------------------------------------------
// Core ML.

pub(crate) fn tableau_entiers(longueur: usize, valeurs: impl Iterator<Item = i32>) -> Result<Retained<MLMultiArray>> {
    let forme = [NSNumber::new_usize(1), NSNumber::new_usize(longueur)];
    let tableau = unsafe {
        MLMultiArray::initWithShape_dataType_error(
            MLMultiArray::alloc(),
            &NSArray::from_retained_slice(&forme),
            MLMultiArrayDataType::Int32,
        )
    }
    .map_err(|e| anyhow!("tableau Core ML : {}", e.localizedDescription()))?;
    #[allow(deprecated)]
    let donnees = unsafe { tableau.dataPointer() }.cast::<i32>().as_ptr();
    // SAFETY : tableau Int32 tout juste créé (contigu) de `longueur` valeurs, à nous seuls.
    let tranche = unsafe { std::slice::from_raw_parts_mut(donnees, longueur) };
    tranche.fill(0);
    for (case, v) in tranche.iter_mut().zip(valeurs) {
        *case = v;
    }
    Ok(tableau)
}

/// Un tableau Core ML de flottants (Float32) de la forme donnée, rempli des `valeurs` (dans l'ordre).
pub(crate) fn tableau_flottants(forme: &[usize], valeurs: &[f32]) -> Result<Retained<MLMultiArray>> {
    let total: usize = forme.iter().product();
    if valeurs.len() != total {
        return Err(anyhow!("tableau Core ML : {} valeurs pour {total} cases", valeurs.len()));
    }
    let dimensions: Vec<Retained<NSNumber>> = forme.iter().map(|&d| NSNumber::new_usize(d)).collect();
    let tableau = unsafe {
        MLMultiArray::initWithShape_dataType_error(
            MLMultiArray::alloc(),
            &NSArray::from_retained_slice(&dimensions),
            MLMultiArrayDataType::Float32,
        )
    }
    .map_err(|e| anyhow!("tableau Core ML : {}", e.localizedDescription()))?;
    #[allow(deprecated)]
    let donnees = unsafe { tableau.dataPointer() }.cast::<f32>().as_ptr();
    // SAFETY : tableau Float32 tout juste créé (contigu) de `total` valeurs, à nous seuls.
    unsafe { std::slice::from_raw_parts_mut(donnees, total) }.copy_from_slice(valeurs);
    Ok(tableau)
}

pub(crate) fn predire(modele: &MLModel, entrees: &[(&str, &MLMultiArray)], sortie: &str) -> Result<Retained<MLMultiArray>> {
    let cles: Vec<Retained<NSString>> = entrees.iter().map(|(n, _)| NSString::from_str(n)).collect();
    let valeurs: Vec<Retained<MLFeatureValue>> =
        entrees.iter().map(|(_, t)| unsafe { MLFeatureValue::featureValueWithMultiArray(t) }).collect();
    let cles_ref: Vec<&NSString> = cles.iter().map(|c| &**c).collect();
    let valeurs_ref: Vec<&AnyObject> = valeurs.iter().map(|v| v.as_ref()).collect();
    let dictionnaire = NSDictionary::from_slices(&cles_ref, &valeurs_ref);
    let fournisseur = unsafe {
        MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dictionnaire)
    }
    .map_err(|e| anyhow!("entrées Core ML : {}", e.localizedDescription()))?;
    let resultat = unsafe { modele.predictionFromFeatures_error(ProtocolObject::from_ref(&*fournisseur)) }
        .map_err(|e| anyhow!("calcul Core ML : {}", e.localizedDescription()))?;
    unsafe { resultat.featureValueForName(&NSString::from_str(sortie)) }
        .and_then(|v| unsafe { v.multiArrayValue() })
        .ok_or_else(|| anyhow!("sortie « {sortie} » absente"))
}

fn lire(tableau: &MLMultiArray) -> Result<Vec<f32>> {
    crate::empreintes::lire_tableau(tableau)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantification_fidele() {
        let v: Vec<f32> = (0..DIMENSION).map(|i| ((i as f32) * 0.37).sin() * 0.1).collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let v: Vec<f32> = v.iter().map(|x| x / n).collect();
        let (e, q) = quantifier(&v);
        let produit: f32 = q.iter().zip(&v).map(|(&a, &b)| f32::from(a) * b).sum::<f32>() * e;
        assert!((produit - 1.0).abs() < 0.01, "produit = {produit}");
    }
}
