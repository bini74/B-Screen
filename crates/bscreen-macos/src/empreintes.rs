//! Empreintes de voix (phase 5, voix familières) : deux modèles Core ML tirés de pyannote
//! Community-1 (bandes de fréquences « FBank », puis WeSpeaker), convertis par FluidAudio
//! (CC-BY-4.0, voir CREDITS.md). 256 nombres par passage ; la même personne donne des
//! empreintes proches. Calcul sur le Mac (moteur neuronal ou GPU), ~25 ms par tranche de 10 s.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use bscreen_core::voix_familieres::{DIMENSION, normaliser};
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel, MLModelConfiguration,
    MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

use crate::audio::TAUX;

/// Une tranche = 10 s de son (ce qu'attend le modèle).
const TRANCHE: usize = 160_000;
/// Le modèle pèse chaque trame de sortie de la segmentation de pyannote : 589 trames par tranche.
const TRAMES: usize = 589;

/// Où sont les modèles : dans l'app (Contents/Resources/modeles), ou dans assets/modeles
/// en développement, ou là où `BSCREEN_MODELES` le dit.
pub fn dossier_modeles() -> Option<PathBuf> {
    let present = |d: &Path| d.join("Embedding.mlmodelc").is_dir() && d.join("FBank.mlmodelc").is_dir();
    if let Ok(d) = std::env::var("BSCREEN_MODELES") {
        let d = PathBuf::from(d);
        if present(&d) {
            return Some(d);
        }
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().find_map(|a| {
        [a.join("Resources/modeles"), a.join("assets/modeles")]
            .into_iter()
            .find(|d| present(d))
    })
}

pub struct Empreinteur {
    fbank: Retained<MLModel>,
    plongement: Retained<MLModel>,
}

fn charger(chemin: &Path) -> Result<Retained<MLModel>> {
    let configuration = unsafe { MLModelConfiguration::new() };
    unsafe { configuration.setComputeUnits(MLComputeUnits::All) };
    let url = NSURL::fileURLWithPath(&NSString::from_str(&chemin.to_string_lossy()));
    unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url, &configuration) }
        .map_err(|e| anyhow!("modèle {} : {}", chemin.display(), e.localizedDescription()))
}

fn tableau(forme: &[usize]) -> Result<Retained<MLMultiArray>> {
    let forme: Vec<Retained<NSNumber>> = forme.iter().map(|&n| NSNumber::new_usize(n)).collect();
    unsafe {
        MLMultiArray::initWithShape_dataType_error(
            MLMultiArray::alloc(),
            &NSArray::from_retained_slice(&forme),
            MLMultiArrayDataType::Float32,
        )
    }
    .map_err(|e| anyhow!("tableau Core ML : {}", e.localizedDescription()))
}

/// Remplit un tableau Float32 tout juste créé (donc contigu) avec `valeurs`, le reste à 0.
fn remplir(tableau: &MLMultiArray, valeurs: impl Iterator<Item = f32>) {
    let n = unsafe { tableau.count() }.max(0) as usize;
    #[allow(deprecated)]
    let donnees = unsafe { tableau.dataPointer() }.cast::<f32>().as_ptr();
    // SAFETY : tableau Float32 de `n` valeurs, que personne d'autre ne lit pendant ce temps.
    let tranche = unsafe { std::slice::from_raw_parts_mut(donnees, n) };
    tranche.fill(0.0);
    for (case, v) in tranche.iter_mut().zip(valeurs) {
        *case = v;
    }
}

fn predire(
    modele: &MLModel,
    entrees: &[(&str, &MLMultiArray)],
    sortie: &str,
) -> Result<Retained<MLMultiArray>> {
    let cles: Vec<Retained<NSString>> = entrees.iter().map(|(n, _)| NSString::from_str(n)).collect();
    let valeurs: Vec<Retained<MLFeatureValue>> = entrees
        .iter()
        .map(|(_, t)| unsafe { MLFeatureValue::featureValueWithMultiArray(t) })
        .collect();
    let cles_ref: Vec<&NSString> = cles.iter().map(|c| &**c).collect();
    let valeurs_ref: Vec<&AnyObject> = valeurs.iter().map(|v| v.as_ref()).collect();
    let dictionnaire = NSDictionary::from_slices(&cles_ref, &valeurs_ref);
    let fournisseur =
        unsafe { MLDictionaryFeatureProvider::initWithDictionary_error(MLDictionaryFeatureProvider::alloc(), &dictionnaire) }
            .map_err(|e| anyhow!("entrées Core ML : {}", e.localizedDescription()))?;
    let resultat = unsafe { modele.predictionFromFeatures_error(ProtocolObject::from_ref(&*fournisseur)) }
        .map_err(|e| anyhow!("calcul Core ML : {}", e.localizedDescription()))?;
    unsafe { resultat.featureValueForName(&NSString::from_str(sortie)) }
        .and_then(|v| unsafe { v.multiArrayValue() })
        .ok_or_else(|| anyhow!("sortie « {sortie} » absente"))
}

/// Pour les autres modèles Core ML de B-Screen (le sens).
pub(crate) fn lire_tableau(tableau: &MLMultiArray) -> Result<Vec<f32>> {
    lire(tableau)
}

/// Les valeurs d'un tableau de sortie, en f32 (Float32, Float16 ou Double ; contigu).
fn lire(tableau: &MLMultiArray) -> Result<Vec<f32>> {
    let n = unsafe { tableau.count() }.max(0) as usize;
    #[allow(deprecated)]
    let donnees = unsafe { tableau.dataPointer() };
    let genre = unsafe { tableau.dataType() };
    // SAFETY : `n` valeurs du type annoncé par Core ML, le tableau vit pendant la lecture.
    unsafe {
        Ok(if genre == MLMultiArrayDataType::Float32 {
            std::slice::from_raw_parts(donnees.cast::<f32>().as_ptr(), n).to_vec()
        } else if genre == MLMultiArrayDataType::Double {
            std::slice::from_raw_parts(donnees.cast::<f64>().as_ptr(), n).iter().map(|&x| x as f32).collect()
        } else if genre == MLMultiArrayDataType::Float16 {
            std::slice::from_raw_parts(donnees.cast::<u16>().as_ptr(), n).iter().map(|&h| f16_en_f32(h)).collect()
        } else {
            bail!("type de sortie Core ML inattendu ({:?})", genre)
        })
    }
}

fn f16_en_f32(h: u16) -> f32 {
    let signe = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exposant = i32::from((h >> 10) & 0x1f);
    let mantisse = f32::from(h & 0x3ff);
    signe
        * match exposant {
            0 => mantisse * 2f32.powi(-24),
            31 => f32::INFINITY,
            e => (1.0 + mantisse / 1024.0) * 2f32.powi(e - 15),
        }
}

impl Empreinteur {
    pub fn charger(dossier: &Path) -> Result<Self> {
        Ok(Self {
            fbank: charger(&dossier.join("FBank.mlmodelc"))?,
            plongement: charger(&dossier.join("Embedding.mlmodelc"))?,
        })
    }

    /// L'empreinte (normalisée) d'un passage, 16 kHz mono : moyenne de ses tranches de 10 s,
    /// pesées par leur durée.
    pub fn empreinte(&self, echantillons: &[f32]) -> Result<Vec<f32>> {
        if echantillons.len() < TAUX as usize / 2 {
            bail!("passage trop court pour une empreinte");
        }
        let audio = tableau(&[1, 1, TRANCHE])?;
        let poids = tableau(&[1, TRAMES])?;
        let mut somme = vec![0f32; DIMENSION];
        for (rang, tranche) in echantillons.chunks(TRANCHE).enumerate() {
            // Un bout de fin de moins de 2 s n'apporte rien de sûr.
            if rang > 0 && tranche.len() < 2 * TAUX as usize {
                break;
            }
            remplir(&audio, tranche.iter().copied());
            let fbank = predire(&self.fbank, &[("audio", &audio)], "fbank_features")?;
            // Seules les trames qui couvrent du vrai son comptent (le reste est du remplissage).
            let actives = (tranche.len() * TRAMES).div_ceil(TRANCHE).min(TRAMES);
            remplir(&poids, (0..TRAMES).map(|i| if i < actives { 1.0 } else { 0.0 }));
            let sortie = predire(
                &self.plongement,
                &[("fbank_features", &fbank), ("weights", &poids)],
                "embedding",
            )?;
            let mut v = lire(&sortie)?;
            if v.len() != DIMENSION {
                bail!("empreinte de {} valeurs au lieu de {DIMENSION}", v.len());
            }
            normaliser(&mut v);
            let part = tranche.len() as f32 / TRANCHE as f32;
            somme.iter_mut().zip(&v).for_each(|(s, x)| *s += x * part);
        }
        normaliser(&mut somme);
        Ok(somme)
    }
}

/// Rattache un passage à une voix connue (ou en crée une) ; le centre de la voix n'apprend que
/// d'un passage sûr. Rend l'identifiant de la voix, ou rien : une vidéo, ou « ? ».
pub fn rattacher(empreinte: &[f32], origine: bscreen_core::voix_familieres::Origine) -> Option<i64> {
    use bscreen_core::voix_familieres::{
        Attribution, VoixConnue, attribuer, depuis_octets, en_octets, nouveau_centre,
    };
    let connues: Vec<VoixConnue> = crate::moteur::avec_base(|b| b.locuteurs())?
        .into_iter()
        .map(|l| VoixConnue {
            id: l.id,
            centre: depuis_octets(&l.centre),
            passages: l.passages,
            moi: l.moi,
        })
        .collect();
    match attribuer(&connues, empreinte, origine) {
        Attribution::Connue { id, similarite, apprendre } => {
            let voix = connues.iter().find(|v| v.id == id)?;
            let centre = if apprendre {
                nouveau_centre(&voix.centre, voix.passages, empreinte)
            } else {
                voix.centre.clone()
            };
            crate::moteur::avec_base(|b| b.maj_locuteur(id, &en_octets(&centre), voix.passages + 1));
            tracing::debug!(id, similarite, apprendre, "passage rattaché à une voix connue");
            Some(id)
        }
        Attribution::NouvelleMoi => crate::moteur::avec_base(|b| b.nouveau_locuteur(&en_octets(empreinte), true)),
        Attribution::Nouvelle => crate::moteur::avec_base(|b| b.nouveau_locuteur(&en_octets(empreinte), false)),
        Attribution::Incertaine | Attribution::Video => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demi_flottants() {
        assert_eq!(f16_en_f32(0x3c00), 1.0);
        assert_eq!(f16_en_f32(0xc000), -2.0);
        assert!((f16_en_f32(0x3555) - 0.333_25).abs() < 1e-4);
        assert_eq!(f16_en_f32(0), 0.0);
    }
}
