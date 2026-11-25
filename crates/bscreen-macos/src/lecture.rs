//! Lecture des images enregistrées (décodées par la puce) et des icônes d'apps,
//! rendues en BGRA pour l'interface.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;

use anyhow::{Result, anyhow};
use bscreen_core::image::ImageBgra;
use objc2::rc::Retained;
use objc2_av_foundation::{AVAssetImageGenerator, AVURLAsset};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_media::CMTime;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *const c_void);
    fn CGContextSetInterpolationQuality(context: *mut c_void, quality: i32);
    fn CGContextRelease(context: *mut c_void);
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceCreateDeviceGray() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
    fn CGBitmapContextCreateImage(context: *mut c_void) -> *mut c_void;
    fn CGImageGetColorSpace(image: *const c_void) -> *mut c_void;
    fn CGImageGetBitmapInfo(image: *const c_void) -> u32;
    fn CGImageGetBitsPerPixel(image: *const c_void) -> usize;
    fn CGImageGetBytesPerRow(image: *const c_void) -> usize;
    fn CGImageGetDataProvider(image: *const c_void) -> *mut c_void;
    fn CGDataProviderCopyData(provider: *mut c_void) -> *mut c_void;
}

/// kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little : du BGRA en mémoire.
const BGRA: u32 = 2 | (2 << 12);
/// kCGInterpolationHigh
const INTERPOLATION_HAUTE: i32 = 3;

/// Dessine une CGImage dans un tampon BGRA, à la taille demandée (ou à sa taille).
pub(crate) fn en_bgra(image: *const c_void, taille: Option<(u32, u32)>) -> Option<ImageBgra> {
    let (l0, h0) = unsafe { (CGImageGetWidth(image), CGImageGetHeight(image)) };
    if l0 == 0 || h0 == 0 {
        return None;
    }
    // À sa taille, une image déjà en BGRA (celles des vidéos) : ses octets passés par la table de
    // couleurs, au lieu de la redessiner (8 ms au lieu de 66 pour un écran Retina, 24/11/2026).
    if taille.is_none_or(|(l, h)| l as usize == l0 && h as usize == h0)
        && let Some(directe) = bgra_direct(image, l0, h0)
    {
        return Some(directe);
    }
    let (l, h) = taille.map(|(l, h)| (l as usize, h as usize)).unwrap_or((l0, h0));
    let mut pixels = vec![0u8; l * h * 4];
    unsafe {
        let espace = CGColorSpaceCreateDeviceRGB();
        let contexte = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), l, h, 8, l * 4, espace, BGRA);
        CGColorSpaceRelease(espace);
        if contexte.is_null() {
            return None;
        }
        CGContextSetInterpolationQuality(contexte, INTERPOLATION_HAUTE);
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: l as f64,
                height: h as f64,
            },
        };
        CGContextDrawImage(contexte, rect, image);
        CGContextRelease(contexte);
    }
    Some(ImageBgra {
        largeur: l as u32,
        hauteur: h as u32,
        pixels,
    })
}

/// kCGBitmapByteOrder32Little, et les trois façons d'avoir l'alpha en tête (rien, prémultiplié, ignoré).
const ORDRE_32_PETIT: u32 = 2 << 12;
const ALPHA_EN_TETE: [u32; 3] = [2, 4, 6];

/// La table de conversion de couleurs (par canal : B, V, R) d'un espace de couleurs vers celui du
/// dessin habituel (DeviceRGB), tirée de CoreGraphics lui-même : une rampe de 256 gris dessinée.
/// Valable parce que les deux espaces ont les mêmes couleurs primaires (vidéos « HDTV ») : seule
/// la courbe de chaque canal change (vérifié au pixel près sur de vraies images, `essai-lecture`).
fn table_de_couleurs(espace: *mut c_void) -> Option<[[u8; 256]; 3]> {
    use core_foundation::base::TCFType;
    thread_local! {
        static TABLES: RefCell<HashMap<String, Option<[[u8; 256]; 3]>>> = RefCell::new(HashMap::new());
    }
    if espace.is_null() {
        return None;
    }
    // SAFETY : description d'un objet CoreFoundation valide, rendue par la règle « Create ».
    let cle = unsafe {
        core_foundation::string::CFString::wrap_under_create_rule(core_foundation::base::CFCopyDescription(espace as _))
    }
    .to_string();
    let cle = cle.split_once(')').map_or(cle.clone(), |(debut, _)| debut.split_once('(').map_or(debut, |(_, d)| d).to_string());
    TABLES.with(|tables| {
        *tables.borrow_mut().entry(cle).or_insert_with(|| {
            let mut rampe: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, 255]).collect();
            let mut sortie = vec![0u8; 256 * 4];
            // SAFETY : deux contextes sur nos propres tampons de 256 × 1 pixels BGRA ; tout est libéré.
            unsafe {
                let source = CGBitmapContextCreate(rampe.as_mut_ptr().cast(), 256, 1, 8, 256 * 4, espace, BGRA);
                if source.is_null() {
                    return None;
                }
                let image = CGBitmapContextCreateImage(source);
                CGContextRelease(source);
                if image.is_null() {
                    return None;
                }
                let rgb = CGColorSpaceCreateDeviceRGB();
                let cible = CGBitmapContextCreate(sortie.as_mut_ptr().cast(), 256, 1, 8, 256 * 4, rgb, BGRA);
                CGColorSpaceRelease(rgb);
                if !cible.is_null() {
                    let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: 256.0, height: 1.0 } };
                    CGContextDrawImage(cible, rect, image);
                    CGContextRelease(cible);
                }
                CFRelease(image);
            }
            let mut table = [[0u8; 256]; 3];
            for v in 0..256 {
                for c in 0..3 {
                    table[c][v] = sortie[v * 4 + c];
                }
            }
            Some(table)
        })
    })
}

/// Les pixels d'une image déjà en BGRA 8 bits (alpha en tête, petit-boutiste), sans la redessiner :
/// octets copiés à travers la table de couleurs, alpha à 255, sur plusieurs cœurs.
fn bgra_direct(image: *const c_void, l: usize, h: usize) -> Option<ImageBgra> {
    let (info, bits, ligne) = unsafe { (CGImageGetBitmapInfo(image), CGImageGetBitsPerPixel(image), CGImageGetBytesPerRow(image)) };
    if bits != 32 || info & 0x7000 != ORDRE_32_PETIT || !ALPHA_EN_TETE.contains(&(info & 0x1f)) || ligne < l * 4 {
        return None;
    }
    let table = table_de_couleurs(unsafe { CGImageGetColorSpace(image) })?;
    // SAFETY : copie (souvent sans recopie réelle) des octets de l'image, libérée en fin de bloc.
    let donnees = unsafe { CGDataProviderCopyData(CGImageGetDataProvider(image)) };
    if donnees.is_null() {
        return None;
    }
    let n = unsafe { CFDataGetLength(donnees) } as usize;
    let mut pixels = vec![0u8; l * h * 4];
    if n >= ligne * (h - 1) + l * 4 {
        // SAFETY : `n` octets lisibles tant que `donnees` vit.
        let brut = unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(donnees), n) };
        let fils = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        let lignes_par_fil = h.div_ceil(fils);
        std::thread::scope(|portee| {
            for (k, bande) in pixels.chunks_mut(lignes_par_fil * l * 4).enumerate() {
                let table = &table;
                portee.spawn(move || {
                    for (y, sortie) in bande.chunks_exact_mut(l * 4).enumerate() {
                        let debut = (k * lignes_par_fil + y) * ligne;
                        for (o, i) in sortie.chunks_exact_mut(4).zip(brut[debut..debut + l * 4].chunks_exact(4)) {
                            o[0] = table[0][i[0] as usize];
                            o[1] = table[1][i[1] as usize];
                            o[2] = table[2][i[2] as usize];
                            o[3] = 255;
                        }
                    }
                });
            }
        });
    }
    let ok = n >= ligne * (h - 1) + l * 4;
    unsafe { CFRelease(donnees) };
    ok.then_some(ImageBgra { largeur: l as u32, hauteur: h as u32, pixels })
}

/// Une image en niveaux de gris (un octet par pixel, ligne par ligne du haut vers le bas).
pub struct ImageGris {
    pub largeur: usize,
    pub hauteur: usize,
    pub pixels: Vec<u8>,
}

/// Dessine une CGImage en niveaux de gris, à sa taille (pour lire l'encre du texte) : quatre fois
/// moins de données que le BGRA, et sans interpolation.
pub(crate) fn en_gris(image: *const c_void) -> Option<ImageGris> {
    let (l, h) = unsafe { (CGImageGetWidth(image), CGImageGetHeight(image)) };
    if l == 0 || h == 0 {
        return None;
    }
    let mut pixels = vec![0u8; l * h];
    unsafe {
        let espace = CGColorSpaceCreateDeviceGray();
        // kCGImageAlphaNone = 0
        let contexte = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), l, h, 8, l, espace, 0);
        CGColorSpaceRelease(espace);
        if contexte.is_null() {
            return None;
        }
        CGContextSetInterpolationQuality(contexte, 1);
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: l as f64,
                height: h as f64,
            },
        };
        CGContextDrawImage(contexte, rect, image);
        CGContextRelease(contexte);
    }
    Some(ImageGris {
        largeur: l,
        hauteur: h,
        pixels,
    })
}

thread_local! {
    /// Un générateur par fichier vidéo, gardé par le fil qui décode.
    static GENERATEURS: RefCell<HashMap<String, Retained<AVAssetImageGenerator>>> = RefCell::new(HashMap::new());
}

fn generateur(fichier: &Path) -> Retained<AVAssetImageGenerator> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&fichier.to_string_lossy()));
    let video = unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) };
    let generateur = unsafe { AVAssetImageGenerator::assetImageGeneratorWithAsset(&video) };
    unsafe {
        // L'image exacte, pas la plus proche image clé.
        generateur.setRequestedTimeToleranceBefore(CMTime::new(0, 1));
        generateur.setRequestedTimeToleranceAfter(CMTime::new(0, 1));
        generateur.setAppliesPreferredTrackTransform(true);
    }
    generateur
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithData(donnees: *const c_void, options: *const c_void) -> *mut c_void;
    fn CGImageSourceCreateImageAtIndex(source: *mut c_void, index: usize, options: *const c_void) -> *mut c_void;
    fn CGImageDestinationCreateWithData(donnees: *mut c_void, genre: *const c_void, nombre: usize, options: *const c_void)
    -> *mut c_void;
    fn CGImageDestinationAddImage(destination: *mut c_void, image: *const c_void, proprietes: *const c_void);
    fn CGImageDestinationFinalize(destination: *mut c_void) -> bool;
    static kCGImageDestinationLossyCompressionQuality: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocateur: *const c_void, octets: *const u8, longueur: isize) -> *mut c_void;
    fn CFDataCreateMutable(allocateur: *const c_void, capacite: isize) -> *mut c_void;
    fn CFDataGetBytePtr(donnees: *const c_void) -> *const u8;
    fn CFDataGetLength(donnees: *const c_void) -> isize;
    fn CFRelease(objet: *const c_void);
}

/// Une vignette de l'image enregistrée à `position_ms` dans `fichier` : `largeur` pixels de large
/// (proportions gardées), en HEIC de qualité moyenne (≈ 10 Ko pour un écran). Pour la frise, une
/// fois la vidéo partie sur l'archive.
pub fn vignette_heic(fichier: &Path, position_ms: i64, largeur: u32) -> Result<Vec<u8>> {
    objc2::rc::autoreleasepool(|_| {
        let image = cgimage_video(fichier, position_ms)?;
        reduire_et_encoder(Retained::as_ptr(&image).cast::<c_void>(), largeur, "public.heic", 0.5)
    })
}

/// L'image enregistrée à `position_ms` dans `fichier`, en JPEG de `largeur` pixels au plus (la
/// page web). Jamais une image privée : l'appelant le vérifie, et les suites chiffrées ne sont
/// pas lues ici.
pub fn image_jpeg(fichier: &Path, position_ms: i64, largeur: u32, qualite: f64) -> Result<Vec<u8>> {
    if crate::prive::est_prive(&fichier.to_string_lossy()) {
        return Err(anyhow!("image privée"));
    }
    objc2::rc::autoreleasepool(|_| {
        // Partie sur l'archive sans être ramenable : la vignette gardée sur le Mac.
        if !fichier.exists()
            && let Some(octets) = crate::archive::vignette(fichier, position_ms)
            && let Some(petite) = image_depuis_octets(&octets, None)
        {
            return jpeg_de(&petite, largeur, qualite);
        }
        let image = cgimage_video(fichier, position_ms)?;
        reduire_et_encoder(Retained::as_ptr(&image).cast::<c_void>(), largeur, "public.jpeg", qualite)
    })
}

/// Une image BGRA en JPEG, réduite à `largeur` au plus.
fn jpeg_de(image: &ImageBgra, largeur: u32, qualite: f64) -> Result<Vec<u8>> {
    let mut pixels = image.pixels.clone();
    // SAFETY : contexte sur nos propres pixels BGRA, image tirée puis libérée.
    unsafe {
        let espace = CGColorSpaceCreateDeviceRGB();
        let (l, h) = (image.largeur as usize, image.hauteur as usize);
        let contexte = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), l, h, 8, l * 4, espace, BGRA);
        CGColorSpaceRelease(espace);
        if contexte.is_null() {
            return Err(anyhow!("contexte de dessin impossible"));
        }
        let cg = CGBitmapContextCreateImage(contexte);
        CGContextRelease(contexte);
        if cg.is_null() {
            return Err(anyhow!("image impossible"));
        }
        let resultat = reduire_et_encoder(cg, largeur, "public.jpeg", qualite);
        CFRelease(cg);
        resultat
    }
}

/// Réduit une CGImage à `largeur` pixels au plus (proportions gardées) et l'encode (`genre` :
/// « public.heic », « public.jpeg »).
fn reduire_et_encoder(image: *const c_void, largeur: u32, genre: &'static str, qualite: f64) -> Result<Vec<u8>> {
    use core_foundation::base::TCFType;
    let (l0, h0) = unsafe { (CGImageGetWidth(image), CGImageGetHeight(image)) };
    if l0 == 0 || h0 == 0 {
        return Err(anyhow!("image vide"));
    }
    let l = (largeur as usize).min(l0);
    let h = (h0 * l / l0).max(1);
    // SAFETY : fonctions CoreGraphics / ImageIO / CoreFoundation documentées ; chaque objet créé
    // est libéré une fois, le contexte dessine dans sa propre mémoire.
    unsafe {
        let espace = CGColorSpaceCreateDeviceRGB();
        let contexte = CGBitmapContextCreate(std::ptr::null_mut(), l, h, 8, 0, espace, BGRA);
        CGColorSpaceRelease(espace);
        if contexte.is_null() {
            return Err(anyhow!("contexte de dessin impossible"));
        }
        CGContextSetInterpolationQuality(contexte, INTERPOLATION_HAUTE);
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: l as f64, height: h as f64 } };
        CGContextDrawImage(contexte, rect, image);
        let petite = CGBitmapContextCreateImage(contexte);
        CGContextRelease(contexte);
        if petite.is_null() {
            return Err(anyhow!("vignette impossible"));
        }
        let donnees = CFDataCreateMutable(std::ptr::null(), 0);
        let genre_cf = core_foundation::string::CFString::from_static_string(genre);
        let destination = CGImageDestinationCreateWithData(donnees, genre_cf.as_concrete_TypeRef().cast(), 1, std::ptr::null());
        if destination.is_null() {
            CFRelease(petite);
            CFRelease(donnees);
            return Err(anyhow!("encodeur {genre} absent"));
        }
        let cle = core_foundation::string::CFString::wrap_under_get_rule(kCGImageDestinationLossyCompressionQuality.cast());
        let options = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[(
            cle.as_CFType(),
            core_foundation::number::CFNumber::from(qualite).as_CFType(),
        )]);
        CGImageDestinationAddImage(destination, petite, options.as_concrete_TypeRef().cast());
        let ok = CGImageDestinationFinalize(destination);
        CFRelease(destination);
        CFRelease(petite);
        let octets = std::slice::from_raw_parts(CFDataGetBytePtr(donnees), CFDataGetLength(donnees).max(0) as usize).to_vec();
        CFRelease(donnees);
        if !ok || octets.is_empty() {
            return Err(anyhow!("encodage {genre} impossible"));
        }
        Ok(octets)
    }
}

/// Une image (JPEG, PNG, HEIC…) donnée en octets, en BGRA, dessinée à `taille` si demandé.
pub fn image_depuis_octets(octets: &[u8], taille: Option<(u32, u32)>) -> Option<ImageBgra> {
    // SAFETY : fonctions CoreFoundation / ImageIO documentées ; chaque objet créé est libéré une fois.
    unsafe {
        let donnees = CFDataCreate(std::ptr::null(), octets.as_ptr(), octets.len() as isize);
        if donnees.is_null() {
            return None;
        }
        let source = CGImageSourceCreateWithData(donnees, std::ptr::null());
        CFRelease(donnees);
        if source.is_null() {
            return None;
        }
        let image = CGImageSourceCreateImageAtIndex(source, 0, std::ptr::null());
        CFRelease(source);
        if image.is_null() {
            return None;
        }
        let bgra = en_bgra(image, taille);
        CFRelease(image);
        bgra
    }
}

/// L'image enregistrée à `position_ms` dans `fichier`, réduite à `taille` si demandé (vignette).
///
/// Chaque lecture a son bassin de libération : les objets temporaires d'AVFoundation sont rendus
/// tout de suite, sinon ils s'accumulent sur le fil qui décode jusqu'à ce qu'il s'arrête.
pub fn image_enregistree(fichier: &Path, position_ms: i64, taille: Option<(u32, u32)>) -> Result<ImageBgra> {
    // Une vidéo partie sur l'archive (phase 10) : une vignette suffit en petit (pas de réseau) ; en
    // grand, la vidéo est ramenée, et la vignette sert si l'archive ne répond pas.
    if !fichier.exists() && !crate::prive::est_prive(&fichier.to_string_lossy()) {
        let depuis_vignette = || {
            crate::archive::vignette(fichier, position_ms)
                .and_then(|octets| image_depuis_octets(&octets, taille))
                .ok_or_else(|| anyhow!("ni vidéo ni vignette : {}", fichier.display()))
        };
        if taille.is_some()
            && let Ok(image) = depuis_vignette()
        {
            return Ok(image);
        }
        return match crate::archive::lisible(fichier) {
            Ok(ramene) => image_enregistree(&ramene, position_ms, taille),
            Err(e) => {
                tracing::warn!(%e, "vidéo archivée injoignable : la vignette la remplace");
                depuis_vignette()
            }
        };
    }
    objc2::rc::autoreleasepool(|_| {
        let image = cgimage_enregistree(fichier, position_ms)?;
        en_bgra(Retained::as_ptr(&image).cast(), taille).ok_or_else(|| anyhow!("image vide"))
    })
}

/// L'image enregistrée, telle que décodée (CGImage), pour l'OCR : dans la vidéo, ou dans son
/// fichier chiffré si c'est un moment privé rangé à part (seulement après Touch ID).
fn cgimage_enregistree(fichier: &Path, position_ms: i64) -> Result<Retained<objc2_core_graphics::CGImage>> {
    let nom = fichier.to_string_lossy();
    // Dans une suite privée, la « position » est le rang de l'image dans la suite.
    if crate::prive::est_suite(&nom) {
        return crate::prive::cgimage_suite(fichier, position_ms);
    }
    if crate::prive::est_prive(&nom) {
        return crate::prive::cgimage(fichier);
    }
    cgimage_video(fichier, position_ms)
}

/// L'image à `position_ms` dans une vidéo.
pub(crate) fn cgimage_video(fichier: &Path, position_ms: i64) -> Result<Retained<objc2_core_graphics::CGImage>> {
    // Partie sur l'archive : ramenée dans le cache d'abord (OCR, sélection de texte).
    let fichier = &crate::archive::lisible(fichier)?;
    let cle = fichier.to_string_lossy().into_owned();
    GENERATEURS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 12 {
            cache.clear();
        }
        let generateur = cache.entry(cle).or_insert_with(|| generateur(fichier));
        #[allow(deprecated)]
        unsafe { generateur.copyCGImageAtTime_actualTime_error(CMTime::new(position_ms, 1000), std::ptr::null_mut()) }
            .map_err(|e| {
                anyhow!(
                    "décodage de {} à {position_ms} ms : {}",
                    fichier.display(),
                    e.localizedDescription()
                )
            })
    })
}

/// Où les mots cherchés sont visibles sur l'image enregistrée, d'après l'OCR d'Apple (Vision).
/// Rend des cadres normalisés (0 à 1, origine en haut à gauche), au mot près quand c'est possible.
pub fn reperer_mots(fichier: &Path, position_ms: i64, mots: &[String]) -> Result<Vec<[f64; 4]>> {
    objc2::rc::autoreleasepool(|_| {
        let image = cgimage_enregistree(fichier, position_ms)?;
        crate::ocr::reperer(&image, mots)
    })
}

/// L'icône d'une app (par son identifiant), en carré de `cote` pixels.
pub fn icone_app(identifiant: &str, cote: u32) -> Option<ImageBgra> {
    objc2::rc::autoreleasepool(|_| icone_app_seule(identifiant, cote))
}

fn icone_app_seule(identifiant: &str, cote: u32) -> Option<ImageBgra> {
    let espace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    let url = espace.URLForApplicationWithBundleIdentifier(&NSString::from_str(identifiant))?;
    let chemin = url.path()?;
    let icone = espace.iconForFile(&chemin);
    let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f64::from(cote), f64::from(cote)));
    let image = unsafe { icone.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    en_bgra(Retained::as_ptr(&image).cast(), Some((cote, cote)))
}

/// Le nom d'une app d'après son identifiant (« Safari »), si elle est installée.
pub fn nom_app(identifiant: &str) -> Option<String> {
    objc2::rc::autoreleasepool(|_| {
        let espace = objc2_app_kit::NSWorkspace::sharedWorkspace();
        let url = espace.URLForApplicationWithBundleIdentifier(&NSString::from_str(identifiant))?;
        let chemin = url.path()?;
        // Le nom affiché dans la langue du Mac (« Trousseau d'accès », pas « Keychain Access »).
        let nom = objc2_foundation::NSFileManager::defaultManager().displayNameAtPath(&chemin).to_string();
        Some(nom.strip_suffix(".app").unwrap_or(&nom).to_string())
    })
}

/// Durée réellement lisible d'une vidéo, en ms (après un arrêt brutal, la fin peut manquer).
pub fn duree_lisible_ms(fichier: &Path) -> Option<i64> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&fichier.to_string_lossy()));
    let video = unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) };
    let duree = unsafe { video.duration() };
    if duree.timescale <= 0 {
        return None;
    }
    Some(duree.value * 1000 / i64::from(duree.timescale))
}

/// Tout le texte de l'image enregistrée, mot à mot (OCR précis), pour le sélectionner dans la frise.
pub fn lire_image_enregistree(fichier: &Path, position_ms: i64) -> Result<Vec<bscreen_core::texte_lu::LigneLue>> {
    objc2::rc::autoreleasepool(|_| {
        let image = cgimage_enregistree(fichier, position_ms)?;
        crate::ocr::lire_mots(&image)
    })
}

/// Banc d'essai de la relecture (`bscreen essai-lecture`) : pour chaque position, le temps du
/// décodage et de trois façons d'en tirer les pixels BGRA. Rend des lignes à afficher.
pub fn chronometrer_lecture(fichier: &Path, positions: &[i64]) -> Result<Vec<String>> {
    let mut lignes = Vec::new();
    for &position in positions {
        objc2::rc::autoreleasepool(|_| -> Result<()> {
            let t = std::time::Instant::now();
            let image = cgimage_video(fichier, position)?;
            let decodage = t.elapsed().as_secs_f64() * 1000.0;
            let ptr = Retained::as_ptr(&image).cast::<c_void>();
            let t = std::time::Instant::now();
            let directe = bgra_direct(ptr, unsafe { CGImageGetWidth(ptr) }, unsafe { CGImageGetHeight(ptr) });
            let direct_ms = t.elapsed().as_secs_f64() * 1000.0;
            let t = std::time::Instant::now();
            let dessinee = dessiner_bgra(ptr);
            let dessin = t.elapsed().as_secs_f64() * 1000.0;
            let ecart_direct = match (&directe, &dessinee) {
                (Some(a), Some(b)) => a.pixels.iter().zip(&b.pixels).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0).to_string(),
                _ => "—".into(),
            };
            lignes.push(format!("   directe {direct_ms:5.1} ms, écart maxi avec le dessin {ecart_direct}"));
            // Dessin dans l'espace de couleurs de l'image elle-même (pas de conversion de couleurs).
            let t = std::time::Instant::now();
            let (l, h) = unsafe { (CGImageGetWidth(ptr), CGImageGetHeight(ptr)) };
            let mut pixels = vec![0u8; l * h * 4];
            unsafe {
                let espace = CGImageGetColorSpace(ptr);
                let contexte = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), l, h, 8, l * 4, espace, BGRA);
                if !contexte.is_null() {
                    CGContextDrawImage(contexte, CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: l as f64, height: h as f64 } }, ptr);
                    CGContextRelease(contexte);
                }
            }
            let meme_espace = t.elapsed().as_secs_f64() * 1000.0;
            // Les octets tels quels.
            let t = std::time::Instant::now();
            let (info, bpp, ligne) = unsafe { (CGImageGetBitmapInfo(ptr), CGImageGetBitsPerPixel(ptr), CGImageGetBytesPerRow(ptr)) };
            let donnees = unsafe { CGDataProviderCopyData(CGImageGetDataProvider(ptr)) };
            let n = if donnees.is_null() { 0 } else { unsafe { CFDataGetLength(donnees) } };
            let mut ecart = (0u64, 0u8, 0u8);
            // Pour chaque canal et chaque valeur brute : les valeurs dessinées (mini, maxi).
            let mut table = [[(255u8, 0u8); 256]; 3];
            if !donnees.is_null() {
                let brut = unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(donnees), n as usize) };
                if let Some(dessinee) = dessiner_bgra(ptr) {
                    for (a, b) in brut.chunks_exact(4).zip(dessinee.pixels.chunks_exact(4)) {
                        for c in 0..3 {
                            let t = &mut table[c][a[c] as usize];
                            t.0 = t.0.min(b[c]);
                            t.1 = t.1.max(b[c]);
                            ecart.0 += u64::from(a[c].abs_diff(b[c]));
                            ecart.1 = ecart.1.max(a[c].abs_diff(b[c]));
                        }
                        ecart.2 = ecart.2.max(255 - a[3]);
                    }
                }
                unsafe { CFRelease(donnees) };
            }
            let brut = t.elapsed().as_secs_f64() * 1000.0;
            let espace = unsafe { core_foundation::base::CFCopyDescription(CGImageGetColorSpace(ptr) as _) };
            let espace = unsafe { <core_foundation::string::CFString as core_foundation::base::TCFType>::wrap_under_create_rule(espace) }.to_string();
            let flou = table.iter().flatten().filter(|t| t.0 <= t.1).map(|t| t.1 - t.0).max().unwrap_or(0);
            let meme = (0..256).all(|v| table[0][v].0 > table[0][v].1 || (table[0][v] == table[1][v] || table[1][v].0 > table[1][v].1));
            lignes.push(format!("   table par canal : écart maxi entre deux pixels de même valeur brute {flou} ; mêmes tables R/V {meme}"));
            lignes.push(format!("   écart brut / dessinée : moyen {:.3}, pire {}, alpha mini {} ; espace {}", ecart.0 as f64 / (l * h * 3) as f64, ecart.1, 255 - ecart.2, espace.chars().take(90).collect::<String>()));
            lignes.push(format!(
                "{position:>7} ms : décodage {decodage:5.1} ms, dessin {dessin:5.1} ms, même espace {meme_espace:5.1} ms, octets bruts {brut:5.1} ms ({l}×{h}, info {info:#x}, {bpp} bits, {ligne} octets/ligne, {n} octets)"
            ));
            Ok(())
        })?;
    }
    Ok(lignes)
}

/// Le dessin habituel à sa taille (pour comparer, `essai-lecture`).
fn dessiner_bgra(image: *const c_void) -> Option<ImageBgra> {
    let (l, h) = unsafe { (CGImageGetWidth(image), CGImageGetHeight(image)) };
    let mut pixels = vec![0u8; l * h * 4];
    unsafe {
        let espace = CGColorSpaceCreateDeviceRGB();
        let contexte = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), l, h, 8, l * 4, espace, BGRA);
        CGColorSpaceRelease(espace);
        if contexte.is_null() {
            return None;
        }
        CGContextDrawImage(contexte, CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: l as f64, height: h as f64 } }, image);
        CGContextRelease(contexte);
    }
    Some(ImageBgra { largeur: l as u32, hauteur: h as u32, pixels })
}
