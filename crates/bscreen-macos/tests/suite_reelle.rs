//! Essai sur une vraie vidéo, ignoré par défaut (il lui faut un morceau et ses positions) :
//!
//! ```sh
//! BSCREEN_ESSAI_MORCEAU=<vidéo.mp4> BSCREEN_ESSAI_POSITIONS=1000,3000,… \
//!   cargo test -p bscreen-macos --test suite_reelle -- --ignored --nocapture
//! ```
//!
//! Travaille sur une copie, dans un dossier temporaire, avec une clé jetable : la vidéo d'origine,
//! la base et le Trousseau ne sont pas touchés.

use std::collections::HashSet;

use bscreen_macos::suite::{Encodeur, Suite};
use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey};

#[test]
#[ignore]
fn suites_sur_une_vraie_video() {
    let (Ok(source), Ok(positions)) = (std::env::var("BSCREEN_ESSAI_MORCEAU"), std::env::var("BSCREEN_ESSAI_POSITIONS")) else {
        eprintln!("BSCREEN_ESSAI_MORCEAU et BSCREEN_ESSAI_POSITIONS absents : rien à essayer");
        return;
    };
    let positions: Vec<i64> = positions.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    assert!(positions.len() >= 45, "il faut au moins 45 images");
    let dossier = std::env::temp_dir().join(format!("bscreen-essai-suite-{}", std::process::id()));
    std::fs::create_dir_all(&dossier).unwrap();
    let copie = dossier.join("morceau.mp4");
    std::fs::copy(&source, &copie).unwrap();
    let taille_avant = std::fs::metadata(&copie).unwrap().len();

    // Une « navigation privée » de 40 images d'affilée, au milieu du morceau.
    let debut = positions.len() / 2 - 20;
    let privees: HashSet<i64> = positions[debut..debut + 40].iter().copied().collect();
    let mut suites: Vec<Encodeur> = Vec::new();
    let mut dimensions = (0, 0);
    let ecrite = bscreen_macos::effacement::reecrire_en_separant(&copie, &privees, &mut |tampon, _| {
        if suites.last().is_none_or(|e| e.nombre() >= 30) {
            dimensions = (CVPixelBufferGetWidth(tampon), CVPixelBufferGetHeight(tampon));
            suites.push(Encodeur::nouveau(dimensions.0, dimensions.1)?);
        }
        suites.last_mut().unwrap().ajouter(tampon)
    })
    .unwrap()
    .expect("la copie garde les autres images");
    let taille_apres = std::fs::metadata(&ecrite).unwrap().len();

    let mut options = GenerateKeyOptions::default();
    options.set_key_type(KeyType::ec_sec_prime_random()).set_size_in_bits(256);
    let cle = SecKey::new(&options).unwrap();
    let publique = cle.public_key().unwrap();
    let algorithme = Algorithm::ECIESEncryptionCofactorVariableIVX963SHA256AESGCM;
    let (mut images, mut octets) = (0usize, 0usize);
    for encodeur in suites {
        let suite = encodeur.terminer().unwrap();
        let chiffre = publique.encrypt_data(algorithme, &suite.en_octets()).unwrap();
        octets += chiffre.len();
        images += suite.nombre();
        let relue = Suite::depuis_octets(&cle.decrypt_data(algorithme, &chiffre).unwrap()).unwrap();
        assert_eq!(relue, suite);
        let debut_decodage = std::time::Instant::now();
        let derniere = relue.image(relue.nombre() - 1).unwrap();
        let ms = debut_decodage.elapsed().as_millis();
        let p: *const std::ffi::c_void = objc2::rc::Retained::as_ptr(&derniere).cast();
        unsafe extern "C" {
            fn CGImageGetWidth(image: *const std::ffi::c_void) -> usize;
            fn CGImageGetHeight(image: *const std::ffi::c_void) -> usize;
        }
        assert_eq!(unsafe { (CGImageGetWidth(p), CGImageGetHeight(p)) }, dimensions);
        eprintln!("suite de {} images : {} Ko chiffrée, dernière image décodée en {ms} ms", suite.nombre(), chiffre.len() / 1024);
    }
    assert_eq!(images, 40);
    eprintln!(
        "{images} images privées : {} Ko/image en suites ; vidéo {} Ko → {} Ko sans elles ({}x{})",
        octets / 1024 / images,
        taille_avant / 1024,
        taille_apres / 1024,
        dimensions.0,
        dimensions.1
    );
    std::fs::remove_dir_all(&dossier).unwrap();
}
