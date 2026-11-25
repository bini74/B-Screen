//! Parole ou pas ? Le classifieur de sons d'Apple (SoundAnalysis, sur le Mac) écoute chaque
//! passage détecté par l'énergie : la musique, le chant, le clavier ou une toux ne sont pas gardés.

use std::sync::Mutex;

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_avf_audio::{AVAudioFormat, AVAudioPCMBuffer};
use objc2_core_media::CMTime;
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, NSValue};
use objc2_sound_analysis::{
    SNAudioStreamAnalyzer, SNClassificationResult, SNClassifierIdentifierVersion1, SNClassifySoundRequest, SNRequest,
    SNResult, SNResultsObserving, SNTimeDurationConstraintType,
};

/// Durée de fenêtre voulue : 1 s (celle par défaut, 3 s, ne dit rien des passages courts).
const FENETRE_VOULUE_S: f64 = 1.0;

fn secondes(t: CMTime) -> f64 {
    if t.timescale > 0 { t.value as f64 / f64::from(t.timescale) } else { 0.0 }
}

/// Les durées de fenêtre que le classifieur accepte, en secondes : (liste, ou bornes min et max).
fn durees_permises(requete: &SNClassifySoundRequest) -> (Vec<f64>, Option<(f64, f64)>) {
    let contrainte = unsafe { requete.windowDurationConstraint() };
    if unsafe { contrainte.r#type() } == SNTimeDurationConstraintType::Range {
        let plage = unsafe { contrainte.durationRange() };
        let debut = secondes(plage.start);
        (Vec::new(), Some((debut, debut + secondes(plage.duration))))
    } else {
        let durees = unsafe { contrainte.enumeratedDurations() }
            .iter()
            .map(|v: Retained<NSValue>| {
                let t: CMTime = unsafe { msg_send![&*v, CMTimeValue] };
                secondes(t)
            })
            .collect();
        (durees, None)
    }
}

/// La fenêtre la plus proche d'une seconde parmi celles permises.
fn choisir_fenetre(requete: &SNClassifySoundRequest) -> Option<f64> {
    match durees_permises(requete) {
        (_, Some((min, max))) => Some(FENETRE_VOULUE_S.clamp(min, max)),
        (durees, None) => durees
            .into_iter()
            .min_by(|a, b| (a - FENETRE_VOULUE_S).abs().total_cmp(&(b - FENETRE_VOULUE_S).abs())),
    }
}

use crate::audio::TAUX;

/// Ce que le classifieur a entendu dans un passage (moyennes sur ses fenêtres d'environ 1 s).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ecoute {
    pub parole: f64,
    pub musique: f64,
    pub chant: f64,
    pub fenetres: usize,
    /// Fenêtres où c'est nettement de la parole (voir [`fenetre_parlee`]).
    pub fenetres_parlees: usize,
}

impl Ecoute {
    /// Gardé si au moins la moitié des secondes sont de la parole.
    pub fn est_de_la_parole(&self) -> bool {
        self.fenetres == 0 || self.fenetres_parlees * 2 >= self.fenetres
    }
}

/// Une seconde est « parlée » si la parole y est nette et domine le chant ; sur un fond musical
/// marqué (une chanson, du rap), il faut une voix très claire.
fn fenetre_parlee(&(parole, musique, chant): &Fenetre) -> bool {
    parole >= 0.5 && parole > chant && (musique < 0.6 || parole >= 0.8)
}

/// Une fenêtre analysée : confiance « parole », « musique », « chant ».
type Fenetre = (f64, f64, f64);

define_class!(
    // SAFETY : NSObject n'impose rien aux sous-classes ; Observateur n'implémente pas Drop.
    #[unsafe(super(NSObject))]
    #[name = "BScreenObservateurSons"]
    #[ivars = Mutex<Vec<Fenetre>>]
    struct Observateur;

    unsafe impl NSObjectProtocol for Observateur {}

    unsafe impl SNResultsObserving for Observateur {
        // Le nom vient d'Apple (request:didProduceResult:).
        #[allow(non_snake_case)]
        #[unsafe(method(request:didProduceResult:))]
        fn request_didProduceResult(&self, _requete: &ProtocolObject<dyn SNRequest>, resultat: &ProtocolObject<dyn SNResult>) {
            let objet: &AnyObject = resultat.as_ref();
            let Some(classement) = objet.downcast_ref::<SNClassificationResult>() else { return };
            let confiance = |etiquette: &str| {
                unsafe { classement.classificationForIdentifier(&NSString::from_str(etiquette)) }
                    .map(|c| unsafe { c.confidence() })
                    .unwrap_or(0.0)
            };
            let fenetre = (confiance("speech"), confiance("music"), confiance("singing"));
            self.ivars().lock().unwrap_or_else(|e| e.into_inner()).push(fenetre);
        }
    }
);

impl Observateur {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(Mutex::new(Vec::new()));
        unsafe { msg_send![super(this), init] }
    }
}

/// Écoute un passage (16 kHz mono). Rapide : quelques millisecondes par seconde de son.
pub fn ecouter(echantillons: &[f32]) -> Result<Ecoute> {
    // Au moins une fenêtre entière (~1 s) : on complète par du silence.
    let mut son = echantillons.to_vec();
    if son.len() < TAUX as usize {
        son.resize(TAUX as usize, 0.0);
    }
    let format =
        unsafe { AVAudioFormat::initStandardFormatWithSampleRate_channels(AVAudioFormat::alloc(), f64::from(TAUX), 1) }
            .ok_or_else(|| anyhow!("format audio"))?;
    let n = son.len() as u32;
    let tampon = unsafe { AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(AVAudioPCMBuffer::alloc(), &format, n) }
        .ok_or_else(|| anyhow!("tampon audio"))?;
    // SAFETY : tampon au format standard (flottant, non entrelacé), assez grand pour `n` échantillons.
    unsafe {
        let canaux = tampon.floatChannelData();
        if canaux.is_null() {
            return Err(anyhow!("tampon audio sans canal"));
        }
        std::ptr::copy_nonoverlapping(son.as_ptr(), (*canaux).as_ptr(), son.len());
        tampon.setFrameLength(n);
    }
    let identifiant = unsafe { SNClassifierIdentifierVersion1 }.ok_or_else(|| anyhow!("classifieur absent"))?;
    let requete =
        unsafe { SNClassifySoundRequest::initWithClassifierIdentifier_error(SNClassifySoundRequest::alloc(), identifiant) }
            .map_err(|e| anyhow!("classifieur : {}", e.localizedDescription()))?;
    if let Some(fenetre) = choisir_fenetre(&requete) {
        unsafe {
            requete.setWindowDuration(CMTime::new((fenetre * 1000.0).round() as i64, 1000));
            requete.setOverlapFactor(0.5);
        }
    }
    let analyseur = unsafe { SNAudioStreamAnalyzer::initWithFormat(SNAudioStreamAnalyzer::alloc(), &format) };
    let observateur = Observateur::new();
    unsafe {
        analyseur.addRequest_withObserver_error(
            ProtocolObject::from_ref(&*requete),
            ProtocolObject::from_ref(&*observateur),
        )
    }
    .map_err(|e| anyhow!("analyse du son : {}", e.localizedDescription()))?;
    // `completeAnalysis` rend la main une fois tous les résultats livrés.
    unsafe {
        analyseur.analyzeAudioBuffer_atAudioFramePosition(&tampon, 0);
        analyseur.completeAnalysis();
    }
    let fenetres = std::mem::take(&mut *observateur.ivars().lock().unwrap_or_else(|e| e.into_inner()));
    let n = fenetres.len();
    if n == 0 {
        return Ok(Ecoute::default());
    }
    let moyenne = |f: fn(&Fenetre) -> f64| fenetres.iter().map(f).sum::<f64>() / n as f64;
    Ok(Ecoute {
        parole: moyenne(|f| f.0),
        musique: moyenne(|f| f.1),
        chant: moyenne(|f| f.2),
        fenetres: n,
        fenetres_parlees: fenetres.iter().filter(|f| fenetre_parlee(f)).count(),
    })
}

/// Les étiquettes que le classifieur connaît et les fenêtres qu'il accepte (pour l'essai).
pub fn description() -> (Vec<String>, String) {
    let Some(identifiant) = (unsafe { SNClassifierIdentifierVersion1 }) else {
        return (Vec::new(), String::new());
    };
    let Ok(requete) = (unsafe {
        SNClassifySoundRequest::initWithClassifierIdentifier_error(SNClassifySoundRequest::alloc(), identifiant)
    }) else {
        return (Vec::new(), String::new());
    };
    let etiquettes = unsafe { requete.knownClassifications() }.iter().map(|s| s.to_string()).collect();
    let fenetres = match durees_permises(&requete) {
        (_, Some((min, max))) => format!("de {min:.2} s à {max:.2} s"),
        (durees, None) => format!("{durees:.2?} s"),
    };
    (
        etiquettes,
        format!(
            "fenêtres permises {fenetres}, par défaut {:.2} s, choisie {:?}",
            secondes(unsafe { requete.windowDuration() }),
            choisir_fenetre(&requete)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regle_de_la_parole() {
        assert!(fenetre_parlee(&(0.9, 0.25, 0.02)), "voix seule");
        assert!(fenetre_parlee(&(0.85, 0.7, 0.3)), "voix claire sur de la musique");
        assert!(!fenetre_parlee(&(0.6, 0.7, 0.65)), "rap / chanson");
        assert!(!fenetre_parlee(&(0.02, 0.9, 0.03)), "musique");
        let e = Ecoute {
            fenetres: 4,
            fenetres_parlees: 2,
            ..Ecoute::default()
        };
        assert!(e.est_de_la_parole());
        assert!(!Ecoute { fenetres_parlees: 1, ..e }.est_de_la_parole());
    }
}
