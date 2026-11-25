//! Capture des écrans avec ScreenCaptureKit (phase 1).
//!
//! Un flux par écran. macOS ne livre une image que lorsque l'écran change,
//! avec la liste des zones modifiées. B-Screen en garde au plus une toutes
//! les 2 secondes (`INTERVALLE`), et seulement si assez de pixels ont changé
//! depuis la dernière (`AIRE_MINIMALE`). Un changement d'app (`declencher`)
//! fait garder l'image suivante tout de suite.
//!
//! Si une image arrive trop tôt, elle est mise de côté et gardée à la fin
//! de l'intervalle : le dernier état d'un écran n'est jamais perdu.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained, DispatchTime};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFDictionary, CFRetained, CGRect};
use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_graphics::{CGMainDisplayID, CGRectMakeWithDictionaryRepresentation};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCRunningApplication, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamFrameInfoDirtyRects, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};

/// Au plus une image gardée toutes les 2 secondes par écran (comme Rewind).
pub const INTERVALLE: Duration = Duration::from_secs(2);

/// Part de l'écran qui doit avoir changé (cumulée) pour garder une nouvelle image : 0,1 %.
/// Un curseur qui clignote ne suffit pas ; une ligne tapée ou un nouveau message, si.
const AIRE_MINIMALE: f64 = 0.001;

/// Images que ScreenCaptureKit peut préparer d'avance (moins = moins de mémoire).
const PROFONDEUR_FILE: isize = 4;

/// Apps jamais capturées : B-Screen lui-même et les gestionnaires de mots de passe.
pub const APPS_EXCLUES_FIXES: &[&str] = APPS_EXCLUES;
const APPS_EXCLUES: &[&str] = &[
    bscreen_core::IDENTIFIANT,
    "com.apple.Passwords",
    "com.apple.keychainaccess",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "org.keepassxc.keepassxc",
];

/// Une app jamais enregistrée : B-Screen, les gestionnaires de mots de passe, et celles que Julien
/// a exclues (réglage `apps_exclues`).
pub fn est_exclue(identifiant: &str) -> bool {
    APPS_EXCLUES.contains(&identifiant)
        || bscreen_core::reglages::Reglages::charger()
            .apps_exclues
            .iter()
            .any(|a| a == identifiant)
}

/// Une image d'écran gardée.
pub struct ImageCapturee {
    /// Identifiant macOS de l'écran (CGDirectDisplayID).
    pub ecran: u32,
    pub quand: SystemTime,
    pub largeur: usize,
    pub hauteur: usize,
    /// Part de l'écran qui a changé depuis l'image gardée précédente.
    pub aire_modifiee: f64,
    /// Gardée tout de suite à cause d'un changement d'app.
    pub declenchee: bool,
    /// L'image elle-même, en NV12 (format accepté tel quel par l'encodeur vidéo de la puce).
    pub tampon: TamponImage,
}

/// Une image CoreVideo, qu'on peut passer d'un fil à l'autre une fois retenue.
pub struct TamponImage(CFRetained<CVPixelBuffer>);

// SAFETY : un CVPixelBuffer retenu peut être lu depuis n'importe quel fil.
unsafe impl Send for TamponImage {}
unsafe impl Sync for TamponImage {}

impl TamponImage {
    pub fn pixels(&self) -> &CVPixelBuffer {
        &self.0
    }
}

/// Compteurs lus par le menu.
pub struct Statistiques {
    pub ecrans: AtomicU64,
    pub recues: AtomicU64,
    pub gardees: AtomicU64,
    /// Heure de la dernière image gardée, en millisecondes depuis 1970.
    pub derniere_ms: AtomicU64,
}

pub static STATS: Statistiques = Statistiques {
    ecrans: AtomicU64::new(0),
    recues: AtomicU64::new(0),
    gardees: AtomicU64::new(0),
    derniere_ms: AtomicU64::new(0),
};

static DECLENCHEUR: AtomicU64 = AtomicU64::new(0);

/// À appeler quand l'app au premier plan change : chaque écran garde sa prochaine image tout de suite.
pub fn declencher() {
    DECLENCHEUR.fetch_add(1, Ordering::Relaxed);
}

type Sortie = Arc<dyn Fn(ImageCapturee) + Send + Sync>;
/// Appelé si macOS arrête un flux (veille, changement d'écran, erreur), avec la raison.
type QuandArrete = Arc<dyn Fn(String) + Send + Sync>;

define_class!(
    // SAFETY : NSObject n'impose rien aux sous-classes ; DelegueFlux n'implémente pas Drop.
    #[unsafe(super(NSObject))]
    #[name = "BScreenDelegueFlux"]
    #[ivars = QuandArrete]
    struct DelegueFlux;

    unsafe impl NSObjectProtocol for DelegueFlux {}

    unsafe impl SCStreamDelegate for DelegueFlux {
        // Le nom vient d'Apple (stream:didStopWithError:).
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_didStopWithError(&self, _flux: &SCStream, erreur: &NSError) {
            (self.ivars())(erreur.localizedDescription().to_string());
        }
    }
);

impl DelegueFlux {
    fn new(quand_arrete: QuandArrete) -> Retained<Self> {
        let this = Self::alloc().set_ivars(quand_arrete);
        unsafe { msg_send![super(this), init] }
    }
}

#[derive(Default)]
struct EtatEcran {
    derniere: Option<Instant>,
    aire_cumulee: f64,
    declencheur_vu: u64,
    en_attente: Option<ImageCapturee>,
    minuterie_armee: bool,
}

struct Partage {
    ecran: u32,
    etat: Mutex<EtatEcran>,
    file: DispatchRetained<DispatchQueue>,
    sortie: Sortie,
}

define_class!(
    // SAFETY : NSObject n'impose rien aux sous-classes ; SortieCapture n'implémente pas Drop.
    #[unsafe(super(NSObject))]
    #[name = "BScreenSortieCapture"]
    #[ivars = Arc<Partage>]
    struct SortieCapture;

    unsafe impl NSObjectProtocol for SortieCapture {}

    unsafe impl SCStreamOutput for SortieCapture {
        // Le nom vient d'Apple (stream:didOutputSampleBuffer:ofType:).
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(&self, _flux: &SCStream, image: &CMSampleBuffer, genre: SCStreamOutputType) {
            if genre == SCStreamOutputType::Screen {
                // Appelé par macOS : une panique arrêterait toute l'app (l'image est perdue, pas la capture).
                let partage = self.ivars();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recevoir(partage, image))).is_err() {
                    tracing::error!("image d'écran : panique rattrapée");
                }
            }
        }
    }
);

impl SortieCapture {
    fn new(partage: Arc<Partage>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(partage);
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY : NSObject n'impose rien aux sous-classes ; SortieAudio n'implémente pas Drop.
    #[unsafe(super(NSObject))]
    #[name = "BScreenSortieAudio"]
    struct SortieAudio;

    unsafe impl NSObjectProtocol for SortieAudio {}

    unsafe impl SCStreamOutput for SortieAudio {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(&self, _flux: &SCStream, tampon: &CMSampleBuffer, genre: SCStreamOutputType) {
            if genre == SCStreamOutputType::Audio {
                crate::audio::recevoir(crate::audio::SourceAudio::Systeme, tampon);
            } else if genre == SCStreamOutputType::Microphone {
                crate::audio::recevoir(crate::audio::SourceAudio::Micro, tampon);
            }
        }
    }
);

impl SortieAudio {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Le son n'a pas pu être capturé au dernier démarrage (l'écran, lui, l'est).
pub static SON_EN_ECHEC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ce que la capture enregistre en plus des écrans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChoixAudio {
    pub micro: bool,
    pub son_du_mac: bool,
}

/// Le micro intégré du Mac (son identifiant), s'il y en a un : capter celui des AirPods
/// les ferait passer en qualité « appel » tant que B-Screen tourne.
fn micro_integre() -> Option<Retained<NSString>> {
    const INTEGRE: i32 = i32::from_be_bytes(*b"bltn");
    let type_audio = unsafe { AVMediaTypeAudio }?;
    #[allow(deprecated)]
    let appareils = unsafe { AVCaptureDevice::devicesWithMediaType(type_audio) };
    appareils
        .iter()
        .find(|a| unsafe { a.transportType() } == INTEGRE)
        .map(|a| unsafe { a.uniqueID() })
}

/// Ce que ScreenCaptureKit dit d'une image : son état et la surface modifiée (en pixels²).
struct InfosImage {
    statut: SCFrameStatus,
    aire_px: f64,
}

fn infos_image(image: &CMSampleBuffer) -> Option<InfosImage> {
    let tableau = unsafe { image.sample_attachments_array(false) }?;
    // Pont gratuit CoreFoundation ↔ Foundation : un CFArray de CFDictionary est un NSArray de NSDictionary.
    let tableau: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe {
        &*CFRetained::as_ptr(&tableau)
            .as_ptr()
            .cast::<NSArray<NSDictionary<NSString, AnyObject>>>()
    };
    let infos = tableau.firstObject()?;
    let statut = infos
        .objectForKey(unsafe { SCStreamFrameInfoStatus })?
        .downcast::<NSNumber>()
        .ok()?
        .integerValue();
    let mut aire_px = 0.0;
    if let Some(zones) = infos.objectForKey(unsafe { SCStreamFrameInfoDirtyRects })
        && let Ok(zones) = zones.downcast::<NSArray>()
    {
        for zone in zones.iter() {
            let dictionnaire: &CFDictionary = unsafe { &*Retained::as_ptr(&zone).cast::<CFDictionary>() };
            let mut rect = CGRect::default();
            if unsafe { CGRectMakeWithDictionaryRepresentation(Some(dictionnaire), &mut rect) } {
                aire_px += rect.size.width * rect.size.height;
            }
        }
    }
    Some(InfosImage {
        statut: SCFrameStatus(statut),
        aire_px,
    })
}

fn ms_depuis_1970(quand: SystemTime) -> u64 {
    quand
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Marque une image comme gardée (sous verrou) et la rend, prête à être envoyée.
fn garder(etat: &mut EtatEcran, image: ImageCapturee) -> ImageCapturee {
    etat.derniere = Some(Instant::now());
    etat.aire_cumulee = 0.0;
    etat.en_attente = None;
    STATS.gardees.fetch_add(1, Ordering::Relaxed);
    STATS.derniere_ms.store(ms_depuis_1970(image.quand), Ordering::Relaxed);
    image
}

fn recevoir(partage: &Arc<Partage>, echantillon: &CMSampleBuffer) {
    STATS.recues.fetch_add(1, Ordering::Relaxed);
    let Some(infos) = infos_image(echantillon) else { return };
    if infos.statut != SCFrameStatus::Complete {
        return;
    }
    let Some(tampon) = (unsafe { echantillon.image_buffer() }) else {
        return;
    };
    let (largeur, hauteur) = (CVPixelBufferGetWidth(&tampon), CVPixelBufferGetHeight(&tampon));
    let surface = (largeur * hauteur).max(1) as f64;

    let a_envoyer = {
        let mut etat = partage.etat.lock().unwrap_or_else(|e| e.into_inner());
        etat.aire_cumulee += infos.aire_px / surface;
        let declencheur = DECLENCHEUR.load(Ordering::Relaxed);
        let declenchee = declencheur != etat.declencheur_vu;
        etat.declencheur_vu = declencheur;
        let image = ImageCapturee {
            ecran: partage.ecran,
            quand: SystemTime::now(),
            largeur,
            hauteur,
            aire_modifiee: etat.aire_cumulee,
            declenchee,
            tampon: TamponImage(tampon),
        };

        match etat.derniere {
            // Première image, ou changement d'app : on garde tout de suite.
            None => Some(garder(&mut etat, image)),
            Some(_) if declenchee => Some(garder(&mut etat, image)),
            // Trop peu de changement : on oublie l'image, l'aire reste cumulée.
            Some(_) if etat.aire_cumulee < AIRE_MINIMALE => None,
            Some(derniere) => {
                let ecoule = derniere.elapsed();
                let intervalle = crate::vivants::intervalle_capture();
                if ecoule >= intervalle {
                    Some(garder(&mut etat, image))
                } else {
                    // Trop tôt : on la met de côté, elle sera gardée à la fin de l'intervalle
                    // si aucune image plus récente ne la remplace.
                    etat.en_attente = Some(image);
                    if !etat.minuterie_armee {
                        etat.minuterie_armee = true;
                        programmer_attente(partage, intervalle - ecoule);
                    }
                    None
                }
            }
        }
    };
    if let Some(image) = a_envoyer {
        (partage.sortie)(image);
    }
}

fn programmer_attente(partage: &Arc<Partage>, delai: Duration) {
    let partage_minuterie = Arc::clone(partage);
    let quand = DispatchTime::try_from(delai).unwrap_or(DispatchTime::NOW);
    let _ = partage.file.after(quand, move || {
        let a_envoyer = {
            let mut etat = partage_minuterie.etat.lock().unwrap_or_else(|e| e.into_inner());
            etat.minuterie_armee = false;
            etat.en_attente.take().map(|image| garder(&mut etat, image))
        };
        if let Some(image) = a_envoyer {
            (partage_minuterie.sortie)(image);
        }
    });
}

/// Un objet Objective-C qu'on fait passer d'un fil à l'autre.
struct Envoyable<T>(T);
// SAFETY : utilisé seulement pour rendre au fil appelant un objet que ScreenCaptureKit vient de créer.
unsafe impl<T> Send for Envoyable<T> {}

fn contenu_partageable() -> Result<Retained<SCShareableContent>> {
    let (envoi, reception) = mpsc::channel::<Result<Envoyable<Retained<SCShareableContent>>, String>>();
    let reponse = RcBlock::new(move |contenu: *mut SCShareableContent, erreur: *mut NSError| {
        let resultat = match unsafe { Retained::retain(contenu) } {
            Some(contenu) => Ok(Envoyable(contenu)),
            None => Err(unsafe { erreur.as_ref() }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "erreur inconnue".into())),
        };
        let _ = envoi.send(resultat);
    });
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &reponse,
        )
    };
    match reception.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(contenu)) => Ok(contenu.0),
        Ok(Err(e)) => Err(anyhow!("liste des écrans impossible : {e}")),
        Err(_) => bail!("ScreenCaptureKit ne répond pas"),
    }
}

struct FluxEcran {
    ecran: u32,
    flux: Retained<SCStream>,
    _sortie: Retained<SortieCapture>,
    _sortie_audio: Option<Retained<SortieAudio>>,
    _delegue: Retained<DelegueFlux>,
}

/// La capture en cours, un flux par écran. L'arrêter avec [`Capteur::arreter`].
pub struct Capteur {
    flux: Vec<FluxEcran>,
}

// SAFETY : SCStream peut être arrêté depuis n'importe quel fil.
unsafe impl Send for Capteur {}

impl Capteur {
    /// Démarre la capture de tous les écrans. Bloque jusqu'au démarrage :
    /// à appeler hors du fil principal.
    pub fn demarrer(
        sortie: impl Fn(ImageCapturee) + Send + Sync + 'static,
        quand_arrete: impl Fn(String) + Send + Sync + 'static,
        audio: ChoixAudio,
    ) -> Result<Self> {
        let sortie: Sortie = Arc::new(sortie);
        let quand_arrete: QuandArrete = Arc::new(quand_arrete);
        SON_EN_ECHEC.store(false, Ordering::Relaxed);
        let contenu = contenu_partageable()?;
        let ecrans = unsafe { contenu.displays() };
        if ecrans.is_empty() {
            bail!("aucun écran trouvé");
        }
        let exclues: Vec<Retained<SCRunningApplication>> = unsafe { contenu.applications() }
            .iter()
            .filter(|app| est_exclue(&unsafe { app.bundleIdentifier() }.to_string()))
            .collect();
        let exclues = NSArray::from_retained_slice(&exclues);

        // Le son (une seule fois) passe par le flux de l'écran principal.
        let principal = CGMainDisplayID();
        let avec_son = |ecran: &SCDisplay, rang: usize| {
            let id = unsafe { ecran.displayID() };
            id == principal || (rang == 0 && !ecrans.iter().any(|e| unsafe { e.displayID() } == principal))
        };
        let mut flux = Vec::new();
        for (rang, ecran) in ecrans.iter().enumerate() {
            let son = if avec_son(&ecran, rang) { audio } else { ChoixAudio::default() };
            let essai = demarrer_ecran(&ecran, &exclues, Arc::clone(&sortie), Arc::clone(&quand_arrete), son);
            let flux_ecran = match essai {
                Ok(f) => f,
                // L'écran ne dépend jamais du son : s'il échoue avec, on repart sans.
                Err(e) if son != ChoixAudio::default() => {
                    tracing::warn!(%e, "capture avec le son impossible : écran seul");
                    SON_EN_ECHEC.store(true, Ordering::Relaxed);
                    demarrer_ecran(
                        &ecran,
                        &exclues,
                        Arc::clone(&sortie),
                        Arc::clone(&quand_arrete),
                        ChoixAudio::default(),
                    )?
                }
                Err(e) => return Err(e),
            };
            flux.push(flux_ecran);
        }
        STATS.ecrans.store(flux.len() as u64, Ordering::Relaxed);
        Ok(Self { flux })
    }

    pub fn arreter(self) {
        for f in &self.flux {
            unsafe { f.flux.stopCaptureWithCompletionHandler(None) };
            tracing::info!(ecran = f.ecran, "capture arrêtée");
        }
        STATS.ecrans.store(0, Ordering::Relaxed);
    }
}

fn demarrer_ecran(
    ecran: &SCDisplay,
    exclues: &NSArray<SCRunningApplication>,
    sortie: Sortie,
    quand_arrete: QuandArrete,
    son: ChoixAudio,
) -> Result<FluxEcran> {
    let id = unsafe { ecran.displayID() };
    let filtre = unsafe {
        SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            ecran,
            exclues,
            &NSArray::new(),
        )
    };
    // Taille réelle en pixels (écran Retina : points × échelle).
    let echelle = f64::from(unsafe { filtre.pointPixelScale() });
    let zone = unsafe { filtre.contentRect() };
    let largeur = (zone.size.width * echelle).round() as usize;
    let hauteur = (zone.size.height * echelle).round() as usize;

    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(largeur);
        config.setHeight(hauteur);
        // macOS livre au plus 2 images par seconde ; B-Screen trie ensuite.
        config.setMinimumFrameInterval(CMTime::new(1, 2));
        config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        config.setShowsCursor(false);
        config.setQueueDepth(PROFONDEUR_FILE);
        config.setCapturesAudio(son.son_du_mac);
        config.setCaptureMicrophone(son.micro);
        if son.son_du_mac || son.micro {
            config.setSampleRate(48_000);
            config.setChannelCount(1);
            config.setExcludesCurrentProcessAudio(true);
        }
        if son.micro {
            let micro = micro_integre();
            tracing::info!(micro = ?micro.as_ref().map(|m| m.to_string()), "micro choisi (aucun = micro par défaut)");
            config.setMicrophoneCaptureDeviceID(micro.as_deref());
        }
    }

    let file = DispatchQueue::new(&format!("bscreen.capture.ecran{id}"), None);
    let partage = Arc::new(Partage {
        ecran: id,
        etat: Mutex::new(EtatEcran::default()),
        file: file.clone(),
        sortie,
    });
    let sortie_objc = SortieCapture::new(partage);
    let delegue = DelegueFlux::new(quand_arrete);
    let flux = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filtre,
            &config,
            Some(ProtocolObject::from_ref(&*delegue)),
        )
    };
    unsafe {
        flux.addStreamOutput_type_sampleHandlerQueue_error(
            ProtocolObject::from_ref(&*sortie_objc),
            SCStreamOutputType::Screen,
            Some(&file),
        )
    }
    .map_err(|e| anyhow!("flux de l'écran {id} : {}", e.localizedDescription()))?;

    let sortie_audio = if son.son_du_mac || son.micro {
        let sortie_audio = SortieAudio::new();
        let file_audio = DispatchQueue::new("bscreen.audio", None);
        let genres = [
            (son.son_du_mac, SCStreamOutputType::Audio),
            (son.micro, SCStreamOutputType::Microphone),
        ];
        for (_, genre) in genres.into_iter().filter(|(oui, _)| *oui) {
            unsafe {
                flux.addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*sortie_audio),
                    genre,
                    Some(&file_audio),
                )
            }
            .map_err(|e| anyhow!("son de l'écran {id} : {}", e.localizedDescription()))?;
        }
        Some(sortie_audio)
    } else {
        None
    };

    let (envoi, reception) = mpsc::channel::<Option<String>>();
    let reponse = RcBlock::new(move |erreur: *mut NSError| {
        let _ = envoi.send(unsafe { erreur.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    unsafe { flux.startCaptureWithCompletionHandler(Some(&reponse)) };
    match reception.recv_timeout(Duration::from_secs(10)) {
        Ok(None) => {}
        Ok(Some(e)) => bail!("capture de l'écran {id} refusée : {e}"),
        Err(_) => bail!("la capture de l'écran {id} ne démarre pas"),
    }
    tracing::info!(ecran = id, largeur, hauteur, micro = son.micro, son_du_mac = son.son_du_mac, "capture démarrée");
    Ok(FluxEcran {
        ecran: id,
        flux,
        _sortie: sortie_objc,
        _sortie_audio: sortie_audio,
        _delegue: delegue,
    })
}
