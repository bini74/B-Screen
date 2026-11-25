//! Une suite d'images HEVC tenue en mémoire : codée et décodée par la puce (VideoToolbox), sans
//! jamais passer par un fichier en clair. Sert aux moments privés : chiffrée d'un bloc, une suite
//! pèse comme la vidéo (≈ 25 Ko par image) au lieu de ≈ 250 Ko pour une image HEIC seule.
//!
//! Mêmes réglages que les vidéos de B-Screen : qualité 0,5, une image clé toutes les 30 images,
//! pas de réordonnancement (l'ordre de décodage est celui des images).
//!
//! Format (petit-boutiste) : largeur, hauteur, longueur des en-têtes NAL, nombre de jeux de
//! paramètres puis chacun (taille, octets), nombre d'images puis chacune (clé 0/1, taille, octets).

use std::ffi::c_void;
use std::sync::Mutex;

use anyhow::{Result, anyhow, bail};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_video::CVPixelBuffer;
use objc2_core_graphics::CGImage;

/// Une image clé toutes les 30 images, comme les vidéos (accès à une image : ≤ 29 décodages).
pub const INTERVALLE_IMAGES_CLES: i32 = 30;
/// Qualité de l'encodeur (celle des vidéos : 24 Ko/image, SSIM 0,993 mesuré le 19/11).
pub const QUALITE: f64 = 0.5;

type OSStatus = i32;
type CFTypeRef = *const c_void;

const HEVC: u32 = u32::from_be_bytes(*b"hvc1");
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
/// kVTDecodeFrame_DoNotOutputFrame : l'image sert de référence, on ne la veut pas en sortie.
const SANS_SORTIE: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const fn temps(valeur: i64) -> CMTime {
    CMTime { value: valeur, timescale: 1, flags: 1, epoch: 0 }
}

const TEMPS_INVALIDE: CMTime = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };

#[repr(C)]
#[derive(Clone, Copy)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation: CMTime,
    decode: CMTime,
}

type SortieEncodeur = extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void);
type SortieDecodeur = extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void, CMTime, CMTime);

#[repr(C)]
struct RappelDecodeur {
    rappel: SortieDecodeur,
    contexte: *mut c_void,
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTCompressionSessionCreate(
        allocateur: CFTypeRef,
        largeur: i32,
        hauteur: i32,
        codec: u32,
        specification: CFTypeRef,
        attributs_source: CFTypeRef,
        allocateur_sortie: CFTypeRef,
        rappel: SortieEncodeur,
        contexte: *mut c_void,
        session: *mut *mut c_void,
    ) -> OSStatus;
    fn VTSessionSetProperty(session: *mut c_void, cle: CFTypeRef, valeur: CFTypeRef) -> OSStatus;
    fn VTCompressionSessionEncodeFrame(
        session: *mut c_void,
        image: *mut c_void,
        presentation: CMTime,
        duree: CMTime,
        proprietes: CFTypeRef,
        contexte_image: *mut c_void,
        infos: *mut u32,
    ) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(session: *mut c_void, jusqua: CMTime) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: *mut c_void);
    fn VTDecompressionSessionCreate(
        allocateur: CFTypeRef,
        format: *mut c_void,
        specification: CFTypeRef,
        attributs_sortie: CFTypeRef,
        rappel: *const RappelDecodeur,
        session: *mut *mut c_void,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: *mut c_void,
        echantillon: *mut c_void,
        drapeaux: u32,
        contexte_image: *mut c_void,
        infos: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: *mut c_void) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: *mut c_void);
    fn VTCreateCGImageFromCVPixelBuffer(tampon: *mut c_void, options: CFTypeRef, image: *mut *mut c_void) -> OSStatus;
    static kVTCompressionPropertyKey_RealTime: CFTypeRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFTypeRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFTypeRef;
    static kVTCompressionPropertyKey_Quality: CFTypeRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFTypeRef;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetFormatDescription(echantillon: *const c_void) -> *const c_void;
    fn CMSampleBufferGetDataBuffer(echantillon: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetSampleAttachmentsArray(echantillon: *mut c_void, creer: bool) -> *const c_void;
    fn CMBlockBufferGetDataLength(bloc: *mut c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(bloc: *mut c_void, decalage: usize, longueur: usize, destination: *mut c_void) -> OSStatus;
    fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        format: *mut c_void,
        index: usize,
        jeu: *mut *const u8,
        taille: *mut usize,
        nombre: *mut usize,
        entete_nal: *mut i32,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocateur: CFTypeRef,
        nombre: usize,
        jeux: *const *const u8,
        tailles: *const usize,
        entete_nal: i32,
        extensions: CFTypeRef,
        format: *mut *mut c_void,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocateur: CFTypeRef,
        memoire: *mut c_void,
        longueur: usize,
        allocateur_bloc: CFTypeRef,
        source: *const c_void,
        decalage: usize,
        longueur_donnees: usize,
        drapeaux: u32,
        bloc: *mut *mut c_void,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocateur: CFTypeRef,
        donnees: *mut c_void,
        format: *mut c_void,
        echantillons: isize,
        nb_temps: isize,
        temps: *const CMSampleTimingInfo,
        nb_tailles: isize,
        tailles: *const usize,
        echantillon: *mut *mut c_void,
    ) -> OSStatus;
    static kCMSampleAttachmentKey_NotSync: CFTypeRef;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
    fn CVPixelBufferCreate(
        allocateur: CFTypeRef,
        largeur: usize,
        hauteur: usize,
        format: u32,
        attributs: CFTypeRef,
        tampon: *mut *mut c_void,
    ) -> i32;
    fn CVPixelBufferLockBaseAddress(tampon: *mut c_void, drapeaux: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(tampon: *mut c_void, drapeaux: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(tampon: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(tampon: *mut c_void) -> usize;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(espace: *mut c_void);
    fn CGBitmapContextCreate(
        donnees: *mut c_void,
        largeur: usize,
        hauteur: usize,
        bits: usize,
        octets_par_ligne: usize,
        espace: *mut c_void,
        info: u32,
    ) -> *mut c_void;
    fn CGContextDrawImage(contexte: *mut c_void, rectangle: CGRect, image: *const c_void);
    fn CGContextRelease(contexte: *mut c_void);
}

/// Une CGImage recopiée dans un tampon BGRA (pour l'encodeur).
pub fn tampon_depuis_cgimage(image: &CGImage) -> Result<CFRetained<CVPixelBuffer>> {
    let ptr: *const c_void = (image as *const CGImage).cast();
    // SAFETY : image valide ; le tampon et le contexte sont rendus ici ou par l'appelant.
    unsafe {
        let (largeur, hauteur) = (CGImageGetWidth(ptr), CGImageGetHeight(ptr));
        let mut tampon: *mut c_void = std::ptr::null_mut();
        verifier(
            CVPixelBufferCreate(std::ptr::null(), largeur, hauteur, BGRA, std::ptr::null(), &mut tampon),
            "tampon d'image",
        )?;
        CVPixelBufferLockBaseAddress(tampon, 0);
        let espace = CGColorSpaceCreateDeviceRGB();
        // kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little : du BGRA en mémoire.
        let contexte = CGBitmapContextCreate(
            CVPixelBufferGetBaseAddress(tampon),
            largeur,
            hauteur,
            8,
            CVPixelBufferGetBytesPerRow(tampon),
            espace,
            2 | (2 << 12),
        );
        CGColorSpaceRelease(espace);
        if contexte.is_null() {
            CVPixelBufferUnlockBaseAddress(tampon, 0);
            CFRelease(tampon);
            bail!("dessin de l'image impossible");
        }
        let tout = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(largeur as f64, hauteur as f64));
        CGContextDrawImage(contexte, tout, ptr);
        CGContextRelease(contexte);
        CVPixelBufferUnlockBaseAddress(tampon, 0);
        // Rendu par CVPixelBufferCreate (règle « create ») : libéré quand le CFRetained part.
        let tampon = std::ptr::NonNull::new(tampon.cast::<CVPixelBuffer>()).ok_or_else(|| anyhow!("tampon vide"))?;
        Ok(CFRetained::from_raw(tampon))
    }
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(objet: CFTypeRef);
    fn CFRetain(objet: CFTypeRef) -> CFTypeRef;
    fn CFArrayGetCount(tableau: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(tableau: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dictionnaire: *const c_void, cle: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(booleen: *const c_void) -> bool;
    static kCFAllocatorNull: CFTypeRef;
}

fn verifier(statut: OSStatus, quoi: &str) -> Result<()> {
    if statut == 0 { Ok(()) } else { Err(anyhow!("{quoi} : erreur {statut}")) }
}

/// Une suite d'images codées (HEVC), prête à chiffrer ou relue après déchiffrement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suite {
    pub largeur: u32,
    pub hauteur: u32,
    entete_nal: i32,
    parametres: Vec<Vec<u8>>,
    /// (image clé ?, octets HEVC de l'image, unités NAL précédées de leur longueur)
    images: Vec<(bool, Vec<u8>)>,
}

impl Suite {
    pub fn nombre(&self) -> usize {
        self.images.len()
    }

    pub fn en_octets(&self) -> Vec<u8> {
        fn mot(v: u32, sortie: &mut Vec<u8>) {
            sortie.extend_from_slice(&v.to_le_bytes());
        }
        let taille: usize = self.parametres.iter().map(|p| p.len() + 4).sum::<usize>()
            + self.images.iter().map(|(_, i)| i.len() + 5).sum::<usize>();
        let mut sortie = Vec::with_capacity(20 + taille);
        mot(self.largeur, &mut sortie);
        mot(self.hauteur, &mut sortie);
        mot(self.entete_nal as u32, &mut sortie);
        mot(self.parametres.len() as u32, &mut sortie);
        for p in &self.parametres {
            mot(p.len() as u32, &mut sortie);
            sortie.extend_from_slice(p);
        }
        mot(self.images.len() as u32, &mut sortie);
        for (cle, image) in &self.images {
            sortie.push(u8::from(*cle));
            mot(image.len() as u32, &mut sortie);
            sortie.extend_from_slice(image);
        }
        sortie
    }

    pub fn depuis_octets(octets: &[u8]) -> Result<Self> {
        let mut l = Lecture { octets, curseur: 0 };
        let largeur = l.mot()?;
        let hauteur = l.mot()?;
        let entete_nal = l.mot()? as i32;
        let nb_parametres = l.mot()? as usize;
        if nb_parametres == 0 || nb_parametres > 16 {
            bail!("suite illisible (jeux de paramètres)");
        }
        let mut parametres = Vec::with_capacity(nb_parametres);
        for _ in 0..nb_parametres {
            let n = l.mot()? as usize;
            parametres.push(l.prendre(n)?.to_vec());
        }
        let nb_images = l.mot()? as usize;
        let mut images = Vec::with_capacity(nb_images.min(10_000));
        for _ in 0..nb_images {
            let cle = l.prendre(1)?[0] == 1;
            let n = l.mot()? as usize;
            images.push((cle, l.prendre(n)?.to_vec()));
        }
        if images.first().is_some_and(|(cle, _)| !cle) {
            bail!("suite illisible (pas d'image clé au début)");
        }
        Ok(Self { largeur, hauteur, entete_nal, parametres, images })
    }

    /// L'image `index`, décodée depuis l'image clé qui la précède.
    pub fn image(&self, index: usize) -> Result<Retained<CGImage>> {
        if index >= self.images.len() {
            bail!("image {index} absente d'une suite de {}", self.images.len());
        }
        let depart = (0..=index).rev().find(|&i| self.images[i].0).unwrap_or(0);
        let mut derniere = None;
        self.decoder(depart..=index, false, |i, tampon| {
            if i == index {
                derniere = Some(en_cgimage(tampon));
            }
        })?;
        derniere.ok_or_else(|| anyhow!("image {index} pas décodée"))?
    }

    /// Décode toutes les images dans l'ordre et passe chacune (tampon BGRA) à `visiteur`.
    pub fn pour_chaque_image(&self, visiteur: impl FnMut(usize, &CVPixelBuffer)) -> Result<()> {
        self.decoder(0..=self.images.len().saturating_sub(1), true, visiteur)
    }

    /// Décode les images de `plage` (qui part d'une image clé). `tout` : chacune sort vers
    /// `visiteur` ; sinon seule la dernière (les autres ne servent que de référence).
    fn decoder(
        &self,
        plage: std::ops::RangeInclusive<usize>,
        tout: bool,
        mut visiteur: impl FnMut(usize, &CVPixelBuffer),
    ) -> Result<()> {
        if self.images.is_empty() {
            return Ok(());
        }
        let pointeurs: Vec<*const u8> = self.parametres.iter().map(|p| p.as_ptr()).collect();
        let tailles: Vec<usize> = self.parametres.iter().map(Vec::len).collect();
        let mut format: *mut c_void = std::ptr::null_mut();
        // SAFETY : les jeux de paramètres vivent pendant l'appel ; le format suit la règle « create ».
        verifier(
            unsafe {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    std::ptr::null(),
                    pointeurs.len(),
                    pointeurs.as_ptr(),
                    tailles.as_ptr(),
                    self.entete_nal,
                    std::ptr::null(),
                    &mut format,
                )
            },
            "format de la suite",
        )?;
        let sortie: Box<Mutex<SortieDecodee>> = Box::new(Mutex::new(SortieDecodee::default()));
        let rappel = RappelDecodeur {
            rappel: sortie_decodeur,
            contexte: (&*sortie as *const Mutex<SortieDecodee>).cast_mut().cast(),
        };
        // SAFETY : constante de CoreVideo ; le dictionnaire vit pendant la création.
        let cle_format = unsafe { CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey.cast()) };
        let attributs = CFDictionary::from_CFType_pairs(&[(cle_format, CFNumber::from(BGRA as i32))]);
        let mut session: *mut c_void = std::ptr::null_mut();
        // SAFETY : format valide, rappel et sortie vivent jusqu'à l'invalidation de la session.
        let cree = unsafe {
            VTDecompressionSessionCreate(
                std::ptr::null(),
                format,
                std::ptr::null(),
                attributs.as_concrete_TypeRef().cast(),
                &rappel,
                &mut session,
            )
        };
        if cree != 0 {
            unsafe { CFRelease(format) };
            bail!("décodeur de la suite : erreur {cree}");
        }
        let mut resultat = Ok(());
        for i in plage.clone() {
            let (_, octets) = &self.images[i];
            let drapeaux = if tout || i == *plage.end() { 0 } else { SANS_SORTIE };
            if let Err(e) = decoder_une(session, format, i, octets, drapeaux) {
                resultat = Err(e);
                break;
            }
            // SAFETY : session valide ; attend la sortie de l'image (décodage synchrone).
            unsafe { VTDecompressionSessionWaitForAsynchronousFrames(session) };
            let (statut, tampon) = {
                let mut s = sortie.lock().unwrap_or_else(|e| e.into_inner());
                (std::mem::take(&mut s.statut), s.tampon.take())
            };
            if statut != 0 {
                resultat = Err(anyhow!("décodage de l'image {i} : erreur {statut}"));
                break;
            }
            if let Some(tampon) = tampon {
                // SAFETY : tampon décodé (un CVPixelBuffer), retenu jusqu'au CFRelease ci-dessous.
                visiteur(i, unsafe { &*tampon.cast::<CVPixelBuffer>() });
                // SAFETY : tampon retenu dans le rappel.
                unsafe { CFRelease(tampon) };
            }
        }
        // SAFETY : la session et le format nous appartiennent.
        unsafe {
            VTDecompressionSessionInvalidate(session);
            CFRelease(session);
            CFRelease(format);
        }
        // Un tampon arrivé après la boucle (erreur) est rendu.
        if let Some(t) = sortie.lock().unwrap_or_else(|e| e.into_inner()).tampon.take() {
            unsafe { CFRelease(t) };
        }
        resultat
    }

    /// La même suite sans certaines images (effacées) : décodée puis recodée.
    pub fn sans(&self, retirees: &std::collections::HashSet<usize>) -> Result<Option<Suite>> {
        if (0..self.images.len()).all(|i| retirees.contains(&i)) {
            return Ok(None);
        }
        let mut encodeur = Encodeur::nouveau(self.largeur as usize, self.hauteur as usize)?;
        let mut erreur = None;
        self.pour_chaque_image(|i, tampon| {
            if erreur.is_none()
                && !retirees.contains(&i)
                && let Err(e) = encodeur.ajouter(tampon)
            {
                erreur = Some(e);
            }
        })?;
        if let Some(e) = erreur {
            return Err(e);
        }
        encodeur.terminer().map(Some)
    }
}

struct Lecture<'a> {
    octets: &'a [u8],
    curseur: usize,
}

impl<'a> Lecture<'a> {
    fn prendre(&mut self, n: usize) -> Result<&'a [u8]> {
        let fin = self
            .curseur
            .checked_add(n)
            .filter(|f| *f <= self.octets.len())
            .ok_or_else(|| anyhow!("suite tronquée"))?;
        let tranche = &self.octets[self.curseur..fin];
        self.curseur = fin;
        Ok(tranche)
    }

    fn mot(&mut self) -> Result<u32> {
        let quatre: [u8; 4] = self.prendre(4)?.try_into().map_err(|_| anyhow!("suite tronquée"))?;
        Ok(u32::from_le_bytes(quatre))
    }
}

fn decoder_une(session: *mut c_void, format: *mut c_void, i: usize, octets: &[u8], drapeaux: u32) -> Result<()> {
    let mut bloc: *mut c_void = std::ptr::null_mut();
    // SAFETY : le bloc pointe sur `octets` sans les copier ni les libérer (kCFAllocatorNull) ;
    // ils vivent plus longtemps que le décodage (synchrone).
    verifier(
        unsafe {
            CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null(),
                octets.as_ptr().cast_mut().cast(),
                octets.len(),
                kCFAllocatorNull,
                std::ptr::null(),
                0,
                octets.len(),
                0,
                &mut bloc,
            )
        },
        "bloc de l'image",
    )?;
    let minutage = CMSampleTimingInfo { duration: temps(1), presentation: temps(i as i64), decode: TEMPS_INVALIDE };
    let taille = octets.len();
    let mut echantillon: *mut c_void = std::ptr::null_mut();
    // SAFETY : bloc et format valides ; l'échantillon suit la règle « create ».
    let statut = unsafe {
        CMSampleBufferCreateReady(std::ptr::null(), bloc, format, 1, 1, &minutage, 1, &taille, &mut echantillon)
    };
    if statut != 0 {
        unsafe { CFRelease(bloc) };
        bail!("échantillon de l'image {i} : erreur {statut}");
    }
    let mut infos = 0u32;
    // SAFETY : session et échantillon valides.
    let decode = unsafe { VTDecompressionSessionDecodeFrame(session, echantillon, drapeaux, std::ptr::null_mut(), &mut infos) };
    unsafe {
        CFRelease(echantillon);
        CFRelease(bloc);
    }
    verifier(decode, "décodage")
}

#[derive(Default)]
struct SortieDecodee {
    statut: OSStatus,
    tampon: Option<*mut c_void>,
}

extern "C" fn sortie_decodeur(
    contexte: *mut c_void,
    _image: *mut c_void,
    statut: OSStatus,
    _infos: u32,
    tampon: *mut c_void,
    _presentation: CMTime,
    _duree: CMTime,
) {
    // SAFETY : `contexte` est le Mutex<SortieDecodee> du décodeur, vivant jusqu'à son invalidation.
    let sortie = unsafe { &*(contexte as *const Mutex<SortieDecodee>) };
    let mut s = sortie.lock().unwrap_or_else(|e| e.into_inner());
    if statut != 0 {
        s.statut = statut;
        return;
    }
    if !tampon.is_null() {
        // SAFETY : on garde le tampon au-delà du rappel : retenu ici, rendu par le décodeur.
        let retenu = unsafe { CFRetain(tampon) }.cast_mut();
        if let Some(ancien) = s.tampon.replace(retenu) {
            unsafe { CFRelease(ancien) };
        }
    }
}

/// Un tampon décodé (BGRA) en CGImage.
fn en_cgimage(tampon: &CVPixelBuffer) -> Result<Retained<CGImage>> {
    let mut image: *mut c_void = std::ptr::null_mut();
    let tampon: *mut c_void = (tampon as *const CVPixelBuffer).cast_mut().cast();
    // SAFETY : tampon valide ; l'image suit la règle « create ».
    verifier(unsafe { VTCreateCGImageFromCVPixelBuffer(tampon, std::ptr::null(), &mut image) }, "image décodée")?;
    // SAFETY : CGImageRef rendue par une fonction « Create ».
    unsafe { Retained::from_raw(image.cast::<CGImage>()) }.ok_or_else(|| anyhow!("image décodée vide"))
}

#[derive(Default)]
struct EtatEncodeur {
    erreur: Option<OSStatus>,
    entete_nal: i32,
    parametres: Vec<Vec<u8>>,
    images: Vec<(bool, Vec<u8>)>,
}

/// Code des images (CVPixelBuffer, 4:2:0 ou BGRA) en une suite HEVC, en mémoire.
pub struct Encodeur {
    session: *mut c_void,
    etat: Box<Mutex<EtatEncodeur>>,
    suivante: i64,
    largeur: u32,
    hauteur: u32,
}

// SAFETY : la session VideoToolbox peut être utilisée depuis n'importe quel fil (un à la fois).
unsafe impl Send for Encodeur {}

impl Encodeur {
    pub fn nouveau(largeur: usize, hauteur: usize) -> Result<Self> {
        let etat: Box<Mutex<EtatEncodeur>> = Box::default();
        let mut session: *mut c_void = std::ptr::null_mut();
        // SAFETY : `etat` vit aussi longtemps que la session (rendue dans Drop avant lui).
        verifier(
            unsafe {
                VTCompressionSessionCreate(
                    std::ptr::null(),
                    largeur as i32,
                    hauteur as i32,
                    HEVC,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    sortie_encodeur,
                    (&*etat as *const Mutex<EtatEncodeur>).cast_mut().cast(),
                    &mut session,
                )
            },
            "encodeur HEVC",
        )?;
        let encodeur = Self { session, etat, suivante: 0, largeur: largeur as u32, hauteur: hauteur as u32 };
        // SAFETY : constantes de VideoToolbox ; valeurs CF vivantes pendant chaque appel.
        unsafe {
            let regler = |cle: CFTypeRef, valeur: CFTypeRef| VTSessionSetProperty(session, cle, valeur);
            verifier(regler(kVTCompressionPropertyKey_RealTime, CFBoolean::false_value().as_CFTypeRef()), "temps réel")?;
            verifier(
                regler(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::false_value().as_CFTypeRef()),
                "réordonnancement",
            )?;
            verifier(
                regler(kVTCompressionPropertyKey_MaxKeyFrameInterval, CFNumber::from(INTERVALLE_IMAGES_CLES).as_CFTypeRef()),
                "images clés",
            )?;
            verifier(regler(kVTCompressionPropertyKey_Quality, CFNumber::from(QUALITE).as_CFTypeRef()), "qualité")?;
            let _ = regler(kVTCompressionPropertyKey_ExpectedFrameRate, CFNumber::from(1i32).as_CFTypeRef());
        }
        Ok(encodeur)
    }

    pub fn nombre(&self) -> usize {
        self.suivante as usize
    }

    /// Ajoute une image (VideoToolbox la retient le temps de la coder).
    pub fn ajouter(&mut self, tampon: &CVPixelBuffer) -> Result<()> {
        let tampon: *mut c_void = (tampon as *const CVPixelBuffer).cast_mut().cast();
        let mut infos = 0u32;
        // SAFETY : session et tampon valides ; VideoToolbox retient le tampon s'il en a besoin.
        verifier(
            unsafe {
                VTCompressionSessionEncodeFrame(
                    self.session,
                    tampon,
                    temps(self.suivante),
                    temps(1),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    &mut infos,
                )
            },
            "codage d'une image",
        )?;
        self.suivante += 1;
        Ok(())
    }

    /// Termine la suite : toutes les images codées, dans l'ordre.
    pub fn terminer(self) -> Result<Suite> {
        // SAFETY : session valide.
        verifier(unsafe { VTCompressionSessionCompleteFrames(self.session, TEMPS_INVALIDE) }, "fin du codage")?;
        let etat = std::mem::take(&mut *self.etat.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(statut) = etat.erreur {
            bail!("codage de la suite : erreur {statut}");
        }
        if etat.images.len() != self.suivante as usize {
            bail!("codage de la suite : {} images sur {}", etat.images.len(), self.suivante);
        }
        if etat.parametres.is_empty() || etat.images.first().is_some_and(|(cle, _)| !cle) {
            bail!("codage de la suite : début illisible");
        }
        Ok(Suite {
            largeur: self.largeur,
            hauteur: self.hauteur,
            entete_nal: etat.entete_nal,
            parametres: etat.parametres,
            images: etat.images,
        })
    }
}

impl Drop for Encodeur {
    fn drop(&mut self) {
        // SAFETY : la session nous appartient ; plus aucun rappel après l'invalidation.
        unsafe {
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session);
        }
    }
}

extern "C" fn sortie_encodeur(contexte: *mut c_void, _image: *mut c_void, statut: OSStatus, _infos: u32, echantillon: *mut c_void) {
    // SAFETY : `contexte` est le Mutex<EtatEncodeur> de l'encodeur, vivant jusqu'à son invalidation.
    let etat = unsafe { &*(contexte as *const Mutex<EtatEncodeur>) };
    let mut etat = etat.lock().unwrap_or_else(|e| e.into_inner());
    if statut != 0 || echantillon.is_null() {
        etat.erreur.get_or_insert(if statut != 0 { statut } else { -1 });
        return;
    }
    // SAFETY : échantillon valide pendant le rappel ; on copie ce qu'on garde.
    unsafe {
        if etat.parametres.is_empty() {
            let format = CMSampleBufferGetFormatDescription(echantillon).cast_mut();
            let (mut nombre, mut entete) = (0usize, 0i32);
            let lu = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                format,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut nombre,
                &mut entete,
            );
            if lu != 0 || nombre == 0 {
                etat.erreur.get_or_insert(if lu != 0 { lu } else { -2 });
                return;
            }
            for i in 0..nombre {
                let (mut jeu, mut taille): (*const u8, usize) = (std::ptr::null(), 0);
                let lu = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    i,
                    &mut jeu,
                    &mut taille,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if lu != 0 || jeu.is_null() {
                    etat.erreur.get_or_insert(if lu != 0 { lu } else { -3 });
                    return;
                }
                etat.parametres.push(std::slice::from_raw_parts(jeu, taille).to_vec());
            }
            etat.entete_nal = entete;
        }
        let bloc = CMSampleBufferGetDataBuffer(echantillon);
        if bloc.is_null() {
            etat.erreur.get_or_insert(-4);
            return;
        }
        let taille = CMBlockBufferGetDataLength(bloc);
        let mut octets = vec![0u8; taille];
        let copie = CMBlockBufferCopyDataBytes(bloc, 0, taille, octets.as_mut_ptr().cast());
        if copie != 0 {
            etat.erreur.get_or_insert(copie);
            return;
        }
        let cle = !pas_synchronisable(echantillon);
        etat.images.push((cle, octets));
    }
}

/// L'échantillon porte-t-il « NotSync » (ce n'est pas une image clé) ?
unsafe fn pas_synchronisable(echantillon: *mut c_void) -> bool {
    unsafe {
        let pieces = CMSampleBufferGetSampleAttachmentsArray(echantillon, false);
        if pieces.is_null() || CFArrayGetCount(pieces) == 0 {
            return false;
        }
        let dictionnaire = CFArrayGetValueAtIndex(pieces, 0);
        let valeur = CFDictionaryGetValue(dictionnaire, kCMSampleAttachmentKey_NotSync);
        !valeur.is_null() && CFBooleanGetValue(valeur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une image BGRA : fond gris, un rectangle blanc qui avance avec `n`.
    fn image_essai(largeur: usize, hauteur: usize, n: usize) -> *mut c_void {
        let mut tampon: *mut c_void = std::ptr::null_mut();
        unsafe {
            assert_eq!(CVPixelBufferCreate(std::ptr::null(), largeur, hauteur, BGRA, std::ptr::null(), &mut tampon), 0);
            CVPixelBufferLockBaseAddress(tampon, 0);
            let base = CVPixelBufferGetBaseAddress(tampon).cast::<u8>();
            let ligne = CVPixelBufferGetBytesPerRow(tampon);
            for y in 0..hauteur {
                let rangee = std::slice::from_raw_parts_mut(base.add(y * ligne), largeur * 4);
                for x in 0..largeur {
                    let blanc = x / 16 == n % (largeur / 16) && y < hauteur / 2;
                    let v = if blanc { 250 } else { 60 + (y % 50) as u8 };
                    rangee[x * 4..x * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            CVPixelBufferUnlockBaseAddress(tampon, 0);
        }
        tampon
    }

    #[test]
    fn coder_relire_et_retirer() {
        let (l, h) = (640, 384);
        let mut encodeur = Encodeur::nouveau(l, h).unwrap();
        for n in 0..40 {
            let tampon = image_essai(l, h, n);
            encodeur.ajouter(unsafe { &*tampon.cast::<CVPixelBuffer>() }).unwrap();
            unsafe { CFRelease(tampon) };
        }
        let suite = encodeur.terminer().unwrap();
        assert_eq!(suite.nombre(), 40);
        assert!(suite.images[0].0 && suite.images[30].0, "une image clé toutes les 30");
        // Aller-retour du format, puis décodage d'images au milieu et à la fin.
        let relue = Suite::depuis_octets(&suite.en_octets()).unwrap();
        assert_eq!(relue, suite);
        for i in [0, 17, 39] {
            let image = relue.image(i).unwrap();
            let p = Retained::as_ptr(&image).cast();
            assert_eq!(unsafe { (CGImageGetWidth(p), CGImageGetHeight(p)) }, (l, h));
        }
        assert!(relue.image(40).is_err());
        assert!(Suite::depuis_octets(&suite.en_octets()[..50]).is_err());
        // Retirer des images : la suite est recodée sans elles.
        let retirees: std::collections::HashSet<usize> = [0, 5, 39].into_iter().collect();
        let reste = relue.sans(&retirees).unwrap().unwrap();
        assert_eq!(reste.nombre(), 37);
        assert!(reste.image(36).is_ok());
        let toutes: std::collections::HashSet<usize> = (0..40).collect();
        assert!(relue.sans(&toutes).unwrap().is_none());
        // Une CGImage recopiée en tampon BGRA se code aussi (anciennes images privées).
        let image = relue.image(3).unwrap();
        let tampon = tampon_depuis_cgimage(&image).unwrap();
        let mut encodeur = Encodeur::nouveau(l, h).unwrap();
        encodeur.ajouter(&tampon).unwrap();
        assert_eq!(encodeur.terminer().unwrap().nombre(), 1);
    }
}
