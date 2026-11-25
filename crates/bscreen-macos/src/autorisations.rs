//! Les autorisations dont B-Screen a besoin : écran, micro, accessibilité, reconnaissance vocale.
//!
//! Attention : macOS attribue une autorisation au processus « responsable ».
//! Lancé depuis un terminal, c'est le terminal qui est vérifié, pas B-Screen.
//! Il faut donc lancer l'app avec `open B-Screen.app`.

use std::ffi::c_void;
use std::process::Command;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_speech::{SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

/// L'état d'une autorisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Etat {
    Accordee,
    Refusee,
    /// Jamais demandée, ou macOS ne dit pas si elle a été refusée.
    ADemander,
}

impl Etat {
    pub fn accordee(self) -> bool {
        self == Etat::Accordee
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Autorisation {
    Ecran,
    Micro,
    Accessibilite,
    /// Reconnaissance vocale (transcription des voix, sur le Mac).
    Voix,
}

impl Autorisation {
    pub const TOUTES: [Autorisation; 4] = [
        Autorisation::Ecran,
        Autorisation::Micro,
        Autorisation::Accessibilite,
        Autorisation::Voix,
    ];

    pub fn nom(self) -> &'static str {
        match self {
            Autorisation::Ecran => "Enregistrement de l'écran",
            Autorisation::Micro => "Micro",
            Autorisation::Accessibilite => "Accessibilité",
            Autorisation::Voix => "Reconnaissance vocale",
        }
    }

    /// Pourquoi B-Screen en a besoin (affiché dans le menu).
    pub fn raison(self) -> &'static str {
        match self {
            Autorisation::Ecran => "pour capturer ce qui s'affiche",
            Autorisation::Micro => "pour enregistrer les voix",
            Autorisation::Accessibilite => "pour lire le texte exact des fenêtres",
            Autorisation::Voix => "pour transcrire les voix, sur le Mac",
        }
    }

    pub fn etat(self) -> Etat {
        match self {
            Autorisation::Ecran => {
                if unsafe { CGPreflightScreenCaptureAccess() } {
                    Etat::Accordee
                } else {
                    Etat::ADemander
                }
            }
            Autorisation::Micro => {
                let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
                    return Etat::ADemander;
                };
                let statut = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
                if statut == AVAuthorizationStatus::Authorized {
                    Etat::Accordee
                } else if statut == AVAuthorizationStatus::Denied || statut == AVAuthorizationStatus::Restricted {
                    Etat::Refusee
                } else {
                    Etat::ADemander
                }
            }
            Autorisation::Accessibilite => {
                if unsafe { AXIsProcessTrusted() } != 0 {
                    Etat::Accordee
                } else {
                    Etat::ADemander
                }
            }
            Autorisation::Voix => {
                let statut = unsafe { SFSpeechRecognizer::authorizationStatus() };
                if statut == SFSpeechRecognizerAuthorizationStatus::Authorized {
                    Etat::Accordee
                } else if statut == SFSpeechRecognizerAuthorizationStatus::NotDetermined {
                    Etat::ADemander
                } else {
                    Etat::Refusee
                }
            }
        }
    }

    /// Demande l'autorisation : fenêtre du système la première fois,
    /// sinon ouverture du bon panneau des Réglages.
    pub fn demander(self) {
        match self {
            Autorisation::Ecran => {
                // Ne montre la fenêtre du système qu'une seule fois dans la vie de l'app.
                let accordee = unsafe { CGRequestScreenCaptureAccess() };
                if !accordee {
                    self.ouvrir_reglages();
                }
            }
            Autorisation::Micro => match self.etat() {
                Etat::ADemander => {
                    if let Some(audio) = unsafe { AVMediaTypeAudio } {
                        let reponse = RcBlock::new(|accorde: Bool| {
                            tracing::info!(accorde = accorde.as_bool(), "réponse pour le micro");
                        });
                        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &reponse) };
                    }
                }
                Etat::Refusee => self.ouvrir_reglages(),
                Etat::Accordee => {}
            },
            Autorisation::Accessibilite => {
                // La clé du dictionnaire est une CFString, interchangeable avec NSString.
                let cle: &NSString = unsafe { &*(kAXTrustedCheckOptionPrompt as *const NSString) };
                let oui = NSNumber::numberWithBool(true);
                let options = NSDictionary::from_slices(&[cle], &[&*oui]);
                let deja = unsafe { AXIsProcessTrustedWithOptions(objc2::rc::Retained::as_ptr(&options).cast()) } != 0;
                if !deja {
                    tracing::info!("fenêtre d'accessibilité demandée au système");
                }
            }
            Autorisation::Voix => match self.etat() {
                Etat::ADemander => {
                    let reponse = RcBlock::new(|statut: SFSpeechRecognizerAuthorizationStatus| {
                        tracing::info!(statut = statut.0, "réponse pour la reconnaissance vocale");
                    });
                    unsafe { SFSpeechRecognizer::requestAuthorization(&reponse) };
                }
                Etat::Refusee => self.ouvrir_reglages(),
                Etat::Accordee => {}
            },
        }
    }

    /// Ouvre le panneau « Confidentialité et sécurité » correspondant.
    pub fn ouvrir_reglages(self) {
        let panneau = match self {
            Autorisation::Ecran => "Privacy_ScreenCapture",
            Autorisation::Micro => "Privacy_Microphone",
            Autorisation::Accessibilite => "Privacy_Accessibility",
            Autorisation::Voix => "Privacy_SpeechRecognition",
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{panneau}");
        if let Err(e) = Command::new("/usr/bin/open").arg(&url).spawn() {
            tracing::warn!(%e, "impossible d'ouvrir les Réglages");
        }
    }
}
