//! Encodage HEVC des images d'un écran, par la puce (phase 2).
//!
//! Un morceau vidéo de 5 minutes au plus par écran. Le fichier est écrit par
//! fragments : il reste lisible même si l'app s'arrête brutalement.
//! Chaque image garde son heure réelle comme position dans la vidéo
//! (les images sont espacées de 2 s ou plus, pas à cadence fixe).

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVFileTypeMPEG4, AVMediaTypeVideo,
    AVVideoAllowFrameReorderingKey, AVVideoCodecKey, AVVideoCodecTypeHEVC, AVVideoCompressionPropertiesKey,
    AVVideoExpectedSourceFrameRateKey, AVVideoHeightKey, AVVideoMaxKeyFrameIntervalKey, AVVideoQualityKey,
    AVVideoWidthKey,
};
use objc2_core_media::CMTime;
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSNumber, NSString, NSURL};

use crate::capture::ImageCapturee;

/// Durée maximale d'un morceau.
pub const DUREE_MORCEAU: Duration = Duration::from_secs(5 * 60);

/// Qualité constante de l'encodeur HEVC de la puce (0 à 1). Mesuré le 19/11 sur une vraie capture :
/// 0,60 → 43 Ko/image (SSIM 0,998) ; 0,50 → 24 Ko/image (SSIM 0,993) ; 0,42 → 19 Ko (SSIM 0,989).
pub const QUALITE: f64 = 0.5;

/// Une image clé toutes les 30 images (≈ 1 min). Mesuré le 19/11 : presque aussi compact que 60 ou 120,
/// et l'accès à une image précise reste rapide (≈ 11 ms par image décodée).
pub const INTERVALLE_IMAGES_CLES: i64 = 30;

/// Fragment écrit toutes les 5 s : au pire, un arrêt brutal (plantage) fait perdre les 5 dernières
/// secondes. Un arrêt normal (Quitter, extinction, réinstallation) termine proprement le fichier.
const INTERVALLE_FRAGMENTS_S: i64 = 5;

pub fn ms_depuis_1970(quand: SystemTime) -> i64 {
    quand
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn constante(c: Option<&'static NSString>) -> Result<&'static NSString> {
    c.ok_or_else(|| anyhow!("constante AVFoundation absente"))
}

fn reglages_video(largeur: usize, hauteur: usize) -> Result<Retained<NSDictionary<NSString, AnyObject>>> {
    let compression = NSMutableDictionary::<NSString, AnyObject>::new();
    unsafe {
        compression.insert(constante(AVVideoQualityKey)?, &*NSNumber::numberWithDouble(QUALITE));
        compression.insert(
            constante(AVVideoMaxKeyFrameIntervalKey)?,
            &*NSNumber::numberWithLongLong(INTERVALLE_IMAGES_CLES),
        );
        compression.insert(
            constante(AVVideoAllowFrameReorderingKey)?,
            &*NSNumber::numberWithBool(false),
        );
        compression.insert(
            constante(AVVideoExpectedSourceFrameRateKey)?,
            &*NSNumber::numberWithInt(1),
        );
    }

    let reglages = NSMutableDictionary::<NSString, AnyObject>::new();
    unsafe {
        reglages.insert(constante(AVVideoCodecKey)?, constante(AVVideoCodecTypeHEVC)?);
        reglages.insert(
            constante(AVVideoWidthKey)?,
            &*NSNumber::numberWithUnsignedLong(largeur as _),
        );
        reglages.insert(
            constante(AVVideoHeightKey)?,
            &*NSNumber::numberWithUnsignedLong(hauteur as _),
        );
        reglages.insert(constante(AVVideoCompressionPropertiesKey)?, &*compression);
    }
    Ok(Retained::into_super(reglages))
}

/// Un morceau vidéo en cours d'écriture, pour un écran.
pub struct MorceauEnCours {
    /// Identifiant du morceau dans la base.
    pub id_base: i64,
    pub largeur: usize,
    pub hauteur: usize,
    pub debut: SystemTime,
    chemin: PathBuf,
    ecrivain: Retained<AVAssetWriter>,
    entree: Retained<AVAssetWriterInput>,
    adaptateur: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    derniere_position_ms: i64,
}

// SAFETY : AVAssetWriter n'est pas lié à un fil ; les accès sont tous protégés par un verrou.
unsafe impl Send for MorceauEnCours {}

impl MorceauEnCours {
    /// `temps_reel` : vrai pour la capture (l'encodeur ne fait jamais attendre), faux pour un réencodage.
    pub fn ouvrir(
        chemin: PathBuf,
        id_base: i64,
        debut: SystemTime,
        largeur: usize,
        hauteur: usize,
        temps_reel: bool,
    ) -> Result<Self> {
        if let Some(dossier) = chemin.parent() {
            fs::create_dir_all(dossier)?;
        }
        let texte = chemin.to_str().ok_or_else(|| anyhow!("chemin non UTF-8"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(texte));
        let type_fichier = unsafe { AVFileTypeMPEG4 }.ok_or_else(|| anyhow!("type MPEG-4 absent"))?;
        let ecrivain = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, type_fichier) }
            .map_err(|e| anyhow!("création de {} : {}", chemin.display(), e.localizedDescription()))?;
        unsafe {
            ecrivain.setMovieFragmentInterval(CMTime::new(INTERVALLE_FRAGMENTS_S, 1));
            ecrivain.setShouldOptimizeForNetworkUse(false);
        }

        let reglages = reglages_video(largeur, hauteur)?;
        let video = unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("type vidéo absent"))?;
        let entree =
            unsafe { AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(video, Some(&reglages)) };
        unsafe {
            entree.setExpectsMediaDataInRealTime(temps_reel);
            // Positions exactes à la milliseconde (sinon arrondies au 1/600 s).
            entree.setMediaTimeScale(1000);
        }
        if !unsafe { ecrivain.canAddInput(&entree) } {
            bail!("l'encodeur refuse ces réglages ({largeur} × {hauteur})");
        }
        unsafe { ecrivain.addInput(&entree) };
        let adaptateur = unsafe {
            AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
                &entree, None,
            )
        };
        if !unsafe { ecrivain.startWriting() } {
            bail!("écriture impossible : {}", Self::erreur_de(&ecrivain));
        }
        unsafe { ecrivain.startSessionAtSourceTime(CMTime::new(0, 1000)) };

        Ok(Self {
            id_base,
            largeur,
            hauteur,
            debut,
            chemin,
            ecrivain,
            entree,
            adaptateur,
            derniere_position_ms: -1,
        })
    }

    fn erreur_de(ecrivain: &AVAssetWriter) -> String {
        unsafe { ecrivain.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "erreur inconnue".into())
    }

    /// Faut-il passer à un nouveau morceau pour cette image ?
    pub fn a_remplacer(&self, image: &ImageCapturee) -> bool {
        let trop_long = image
            .quand
            .duration_since(self.debut)
            .map(|d| d >= DUREE_MORCEAU)
            .unwrap_or(true);
        trop_long || image.largeur != self.largeur || image.hauteur != self.hauteur
    }

    /// Heure de la dernière image écrite.
    pub fn fin_ms(&self) -> i64 {
        ms_depuis_1970(self.debut) + self.derniere_position_ms.max(0)
    }

    /// Ajoute une image et rend sa position dans la vidéo (en ms).
    pub fn ajouter(&mut self, image: &ImageCapturee) -> Result<i64> {
        let position = (ms_depuis_1970(image.quand) - ms_depuis_1970(self.debut)).max(0);
        self.ajouter_tampon(image.tampon.pixels(), position, false)
    }

    /// Ajoute une image à une position donnée (en ms) et rend la position retenue.
    /// `attendre` : pour un réencodage, on attend que l'encodeur soit prêt au lieu de sauter l'image.
    pub fn ajouter_tampon(&mut self, pixels: &CVPixelBuffer, position_ms: i64, attendre: bool) -> Result<i64> {
        // Les positions doivent strictement augmenter.
        let position = position_ms.max(self.derniere_position_ms + 1);
        let mut essais = 0;
        while !unsafe { self.entree.isReadyForMoreMediaData() } {
            if !attendre || essais > 5_000 {
                bail!("encodeur occupé, image sautée");
            }
            essais += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        let acceptee = unsafe {
            self.adaptateur
                .appendPixelBuffer_withPresentationTime(pixels, CMTime::new(position, 1000))
        };
        if !acceptee {
            bail!("image refusée par l'encodeur : {}", Self::erreur_de(&self.ecrivain));
        }
        self.derniere_position_ms = position;
        Ok(position)
    }

    /// Termine le fichier en arrière-plan. `fini` reçoit la taille du fichier.
    /// Le récepteur rendu permet d'attendre la fin (par exemple avant de quitter).
    pub fn fermer(self, fini: impl Fn(u64) + Send + 'static) -> mpsc::Receiver<()> {
        let (signal, attente) = mpsc::channel();
        unsafe {
            self.entree.markAsFinished();
            // La dernière image dure 1 s : avec une durée nulle, elle serait marquée « à ignorer ».
            self.ecrivain
                .endSessionAtSourceTime(CMTime::new(self.derniere_position_ms + 1000, 1000));
        }
        let chemin = self.chemin.clone();
        let quand_fini = RcBlock::new(move || {
            let octets = fs::metadata(&chemin).map(|m| m.len()).unwrap_or(0);
            fini(octets);
            let _ = signal.send(());
        });
        unsafe { self.ecrivain.finishWritingWithCompletionHandler(&quand_fini) };
        attente
    }
}
