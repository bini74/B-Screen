//! Les moments de navigation privée, chiffrés à part (phase 6, choix de Julien du 20/11).
//!
//! Leurs images sortent des vidéos : codées en suites HEVC de 30 images au plus (comme les vidéos,
//! ≈ 25 Ko par image), chaque suite chiffrée d'un bloc (ECIES, P-256 + AES-GCM) dans
//! `prive/AAAA-MM-JJ/<id>.bsv`. Avant le 22/11, chaque image était un HEIC à part (`.bsp`,
//! ≈ 250 Ko) : ces fichiers sont recopiés en suites au premier déverrouillage par Touch ID.
//! B-Screen ne garde en temps normal que la **clé publique** (un fichier) : elle sert à chiffrer,
//! jamais à relire. La clé privée est au Trousseau, réservée à B-Screen, et n'est lue qu'après
//! Touch ID, pour 5 minutes (et une fois par lancement pour vérifier un aller-retour).
//!
//! Même B-Screen ouvert, sans Touch ID, ces images ne se relisent pas. Elles ne se récupèrent
//! pas avec la clé de secours de l'image chiffrée : la clé privée suit le Trousseau du Mac
//! (sauvegarde Time Machine ou migration comprises).

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bscreen_core::chemins;
use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2_core_graphics::CGImage;
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey};
use security_framework::passwords;

use crate::suite::{Encodeur, Suite};

const SERVICE: &str = "B-Screen moments privés";
const COMPTE: &str = "bscreen";
const ALGORITHME: Algorithm = Algorithm::ECIESEncryptionCofactorVariableIVX963SHA256AESGCM;
/// En-tête des fichiers privés (version 1).
const MAGIE: &[u8; 4] = b"BSP1";
/// Combien de temps la clé privée reste en mémoire après Touch ID.
pub const DUREE_DEVERROUILLAGE: Duration = Duration::from_secs(5 * 60);
/// Extension des fichiers privés d'une image (avant les suites).
pub const EXTENSION: &str = "bsp";
/// En-tête et extension des suites privées (HEVC, version 1).
const MAGIE_SUITE: &[u8; 4] = b"BSV1";
pub const EXTENSION_SUITE: &str = "bsv";
/// Au plus 30 images par suite (une image clé) : effacer une image pendant que les moments privés
/// sont verrouillés emporte au plus ses 29 voisines.
const IMAGES_PAR_SUITE: usize = 30;
/// Suites déchiffrées gardées en mémoire après Touch ID (les plus récentes), oubliées au reverrouillage.
static SUITES: Mutex<Vec<(PathBuf, Arc<Suite>)>> = Mutex::new(Vec::new());
const SUITES_GARDEES: usize = 3;
static CONVERSION_EN_COURS: AtomicBool = AtomicBool::new(false);

static PUBLIQUE: Mutex<Option<SecKey>> = Mutex::new(None);
static PRIVEE: Mutex<Option<(SecKey, Instant)>> = Mutex::new(None);

fn verrou<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn dossier() -> PathBuf {
    chemins::donnees().join("prive")
}

fn chemin_cle_publique() -> PathBuf {
    dossier().join("cle-publique")
}

/// Le fichier d'une suite privée, nommé d'après sa première image.
pub fn chemin_suite(jour: &str, premier_id: i64) -> String {
    format!("prive/{jour}/{premier_id}.{EXTENSION_SUITE}")
}

pub fn est_prive(fichier: &str) -> bool {
    fichier.ends_with(&format!(".{EXTENSION}")) || est_suite(fichier)
}

pub fn est_suite(fichier: &str) -> bool {
    fichier.ends_with(&format!(".{EXTENSION_SUITE}"))
}

fn importer(octets: &[u8], privee: bool) -> Result<SecKey> {
    use security_framework_sys::item::{
        kSecAttrKeyClass, kSecAttrKeyClassPrivate, kSecAttrKeyClassPublic, kSecAttrKeyType,
        kSecAttrKeyTypeECSECPrimeRandom,
    };
    // SAFETY : constantes du cadre Security, valables toute la vie du programme.
    let (cle_type, type_ec, cle_classe, classe) = unsafe {
        (
            CFString::wrap_under_get_rule(kSecAttrKeyType),
            CFString::wrap_under_get_rule(kSecAttrKeyTypeECSECPrimeRandom),
            CFString::wrap_under_get_rule(kSecAttrKeyClass),
            CFString::wrap_under_get_rule(if privee { kSecAttrKeyClassPrivate } else { kSecAttrKeyClassPublic }),
        )
    };
    let attributs = CFDictionary::from_CFType_pairs(&[(cle_type, type_ec), (cle_classe, classe)]);
    let donnees = CFData::from_buffer(octets);
    let mut erreur: core_foundation::error::CFErrorRef = std::ptr::null_mut();
    // SAFETY : arguments CF valides ; la clé rendue (règle « create ») est prise en charge par SecKey.
    let cle = unsafe {
        security_framework_sys::key::SecKeyCreateWithData(
            donnees.as_concrete_TypeRef(),
            attributs.as_concrete_TypeRef(),
            &mut erreur,
        )
    };
    if cle.is_null() {
        if !erreur.is_null() {
            // SAFETY : l'erreur rendue suit la règle « create ».
            let e = unsafe { core_foundation::error::CFError::wrap_under_create_rule(erreur) };
            bail!("clé illisible : {}", e.description());
        }
        bail!("clé illisible");
    }
    // SAFETY : `cle` est une SecKeyRef rendue par une fonction « Create ».
    Ok(unsafe { SecKey::wrap_under_create_rule(cle) })
}

/// La clé privée est-elle au Trousseau ? (sans la lire : seulement ses attributs)
fn cle_privee_presente() -> bool {
    use security_framework::item::{ItemClass, ItemSearchOptions};
    ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(SERVICE)
        .account(COMPTE)
        .load_attributes(true)
        .search()
        .is_ok_and(|r| !r.is_empty())
}

/// Crée la paire de clés : la privée au Trousseau d'abord, la publique ensuite (sinon on
/// chiffrerait pour une clé perdue). Vérifie l'aller-retour avant de s'en servir.
fn creer_paire() -> Result<SecKey> {
    let mut options = GenerateKeyOptions::default();
    options.set_key_type(KeyType::ec_sec_prime_random()).set_size_in_bits(256);
    let privee = SecKey::new(&options).map_err(|e| anyhow!("création de la clé : {}", e.description()))?;
    let publique = privee.public_key().ok_or_else(|| anyhow!("clé publique absente"))?;
    let octets_prives = privee
        .external_representation()
        .ok_or_else(|| anyhow!("clé privée illisible"))?
        .to_vec();
    let octets_publics = publique
        .external_representation()
        .ok_or_else(|| anyhow!("clé publique illisible"))?
        .to_vec();
    let essai = publique
        .encrypt_data(ALGORITHME, b"essai")
        .map_err(|e| anyhow!("chiffrement d'essai : {}", e.description()))?;
    let relu = privee
        .decrypt_data(ALGORITHME, &essai)
        .map_err(|e| anyhow!("relecture d'essai : {}", e.description()))?;
    if relu != b"essai" {
        bail!("aller-retour de la clé raté");
    }
    passwords::set_generic_password(SERVICE, COMPTE, &octets_prives)
        .context("impossible de ranger la clé des moments privés au Trousseau")?;
    fs::create_dir_all(dossier())?;
    ecrire_prive(&chemin_cle_publique(), &octets_publics)?;
    tracing::info!("clé des moments privés créée (privée au Trousseau, publique dans prive/)");
    Ok(publique)
}

fn cle_publique() -> Result<SecKey> {
    let mut publique = verrou(&PUBLIQUE);
    if let Some(cle) = publique.as_ref() {
        return Ok(cle.clone());
    }
    let cle = match fs::read(chemin_cle_publique()) {
        Ok(octets) => {
            if !cle_privee_presente() {
                bail!("la clé privée des moments privés n'est plus au Trousseau : rien n'est chiffré avec l'ancienne");
            }
            importer(&octets, false)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => creer_paire()?,
        Err(e) => return Err(e).context("clé publique illisible"),
    };
    *publique = Some(cle.clone());
    Ok(cle)
}

/// Écrit un fichier lisible par Julien seul (0600), d'un coup (temporaire puis renommage).
fn ecrire_prive(chemin: &Path, octets: &[u8]) -> Result<()> {
    let temporaire = chemin.with_extension("tmp");
    let mut fichier = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporaire)
        .with_context(|| format!("écriture de {}", temporaire.display()))?;
    fichier.write_all(octets)?;
    fichier.sync_all()?;
    fs::rename(&temporaire, chemin)?;
    Ok(())
}

/// Après Touch ID : la clé privée est lue au Trousseau et gardée 5 minutes.
pub fn deverrouiller() -> bool {
    let octets = match passwords::get_generic_password(SERVICE, COMPTE) {
        Ok(o) => o,
        Err(e) => {
            tracing::info!(%e, "pas de clé des moments privés au Trousseau (rien n'a encore été chiffré ?)");
            return false;
        }
    };
    match importer(&octets, true) {
        Ok(cle) => {
            *verrou(&PRIVEE) = Some((cle, Instant::now()));
            tracing::info!("moments privés déverrouillés (5 min)");
            convertir_les_anciennes();
            true
        }
        Err(e) => {
            tracing::warn!(%e, "clé des moments privés illisible");
            false
        }
    }
}

/// Oublie la clé privée (frise fermée, ou 5 minutes passées).
pub fn verrouiller() {
    verrou(&SUITES).clear();
    if verrou(&PRIVEE).take().is_some() {
        tracing::info!("moments privés reverrouillés");
    }
}

fn cle_privee() -> Option<SecKey> {
    let mut privee = verrou(&PRIVEE);
    match privee.as_ref() {
        Some((cle, depuis)) if depuis.elapsed() < crate::vivants::deverrouillage() => Some(cle.clone()),
        Some(_) => {
            *privee = None;
            verrou(&SUITES).clear();
            None
        }
        None => None,
    }
}

/// L'image d'un fichier privé (déverrouillé seulement).
pub fn cgimage(chemin: &Path) -> Result<Retained<CGImage>> {
    let cle = cle_privee().ok_or_else(|| anyhow!("moments privés verrouillés"))?;
    cgimage_avec(chemin, &cle)
}

fn cgimage_avec(chemin: &Path, cle: &SecKey) -> Result<Retained<CGImage>> {
    let octets = fs::read(chemin).with_context(|| format!("lecture de {}", chemin.display()))?;
    let chiffre = octets
        .strip_prefix(MAGIE.as_slice())
        .ok_or_else(|| anyhow!("fichier privé inconnu : {}", chemin.display()))?;
    let heic = cle
        .decrypt_data(ALGORITHME, chiffre)
        .map_err(|e| anyhow!("déchiffrement : {}", e.description()))?;
    depuis_heic(&heic)
}

// ---------- suites (HEVC) ----------

/// Chiffre une suite et l'écrit à `relatif` (sous le dossier des données).
fn ranger_suite(suite: &Suite, relatif: &str) -> Result<u64> {
    let chiffre = cle_publique()?
        .encrypt_data(ALGORITHME, &suite.en_octets())
        .map_err(|e| anyhow!("chiffrement : {}", e.description()))?;
    let chemin = chemins::donnees().join(relatif);
    if let Some(parent) = chemin.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut octets = Vec::with_capacity(MAGIE_SUITE.len() + chiffre.len());
    octets.extend_from_slice(MAGIE_SUITE);
    octets.extend_from_slice(&chiffre);
    ecrire_prive(&chemin, &octets)?;
    Ok(octets.len() as u64)
}

fn lire_suite(chemin: &Path, cle: &SecKey) -> Result<Suite> {
    let octets = fs::read(chemin).with_context(|| format!("lecture de {}", chemin.display()))?;
    let chiffre = octets
        .strip_prefix(MAGIE_SUITE.as_slice())
        .ok_or_else(|| anyhow!("suite privée inconnue : {}", chemin.display()))?;
    let clair = cle
        .decrypt_data(ALGORITHME, chiffre)
        .map_err(|e| anyhow!("déchiffrement : {}", e.description()))?;
    Suite::depuis_octets(&clair)
}

/// L'image `rang` d'une suite privée (déverrouillée seulement). La suite déchiffrée reste en
/// mémoire jusqu'au reverrouillage : passer d'une image à la voisine ne la redéchiffre pas.
pub fn cgimage_suite(chemin: &Path, rang: i64) -> Result<Retained<CGImage>> {
    let cle = cle_privee().ok_or_else(|| anyhow!("moments privés verrouillés"))?;
    let rang = usize::try_from(rang).map_err(|_| anyhow!("rang négatif dans la suite"))?;
    let deja = verrou(&SUITES).iter().find(|(c, _)| c == chemin).map(|(_, s)| Arc::clone(s));
    let suite = match deja {
        Some(suite) => suite,
        None => {
            let suite = Arc::new(lire_suite(chemin, &cle)?);
            let mut suites = verrou(&SUITES);
            suites.retain(|(c, _)| c != chemin);
            suites.push((chemin.to_path_buf(), Arc::clone(&suite)));
            if suites.len() > SUITES_GARDEES {
                suites.remove(0);
            }
            suite
        }
    };
    suite.image(rang)
}

fn oublier_suite(chemin: &Path) {
    verrou(&SUITES).retain(|(c, _)| c != chemin);
}

fn maintenant_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Après un effacement d'images : leurs fichiers privés disparaissent. Une suite où restent
/// d'autres images est recodée sans les effacées (clé ouverte par Touch ID) ; verrouillée, elle
/// part entière avec ses voisines (au plus 29). Une image effacée ne reste jamais nulle part.
pub fn apres_effacement(fichiers: &[String]) {
    let mut vus: HashSet<&String> = HashSet::new();
    for f in fichiers {
        if !vus.insert(f) || !est_prive(f) {
            continue;
        }
        let chemin = chemins::donnees().join(f);
        if !est_suite(f) {
            let _ = fs::remove_file(&chemin);
            continue;
        }
        let Some(restantes) = crate::moteur::avec_base(|b| b.images_de_la_suite(f)) else {
            tracing::error!(fichier = f, "base illisible : suite privée laissée en place");
            continue;
        };
        if restantes.is_empty() {
            let _ = fs::remove_file(&chemin);
            oublier_suite(&chemin);
            continue;
        }
        if let Err(e) = recoder_sans_les_effacees(f, &restantes) {
            let ids: Vec<i64> = restantes.iter().map(|(id, _)| *id).collect();
            crate::moteur::avec_base(|b| b.supprimer_images(&ids));
            let _ = fs::remove_file(&chemin);
            oublier_suite(&chemin);
            tracing::warn!(%e, voisines = ids.len(), "suite privée effacée entière, avec ses voisines");
        }
    }
}

/// Recode la suite `f` avec seulement les images qui restent (clé ouverte par Touch ID) : nouveau
/// fichier, puis la base, puis l'ancien effacé (jamais une image montrée à la place d'une autre).
fn recoder_sans_les_effacees(f: &str, restantes: &[(i64, i64)]) -> Result<()> {
    let cle = cle_privee().ok_or_else(|| anyhow!("moments privés verrouillés"))?;
    let chemin = chemins::donnees().join(f);
    let suite = lire_suite(&chemin, &cle)?;
    let mut gardees: Vec<usize> = restantes.iter().map(|(_, rang)| *rang as usize).collect();
    gardees.sort_unstable();
    gardees.dedup();
    let retirees: HashSet<usize> = (0..suite.nombre()).filter(|i| gardees.binary_search(i).is_err()).collect();
    let nouvelle = suite.sans(&retirees)?.ok_or_else(|| anyhow!("suite vide"))?;
    let base = f.strip_suffix(&format!(".{EXTENSION_SUITE}")).unwrap_or(f);
    let nouveau = format!("{base}-{}.{EXTENSION_SUITE}", maintenant_ms());
    ranger_suite(&nouvelle, &nouveau)?;
    for (id, ancien) in restantes {
        let rang = gardees.binary_search(&(*ancien as usize)).map_err(|_| anyhow!("rang perdu"))?;
        if crate::moteur::avec_base(|b| b.noter_dans_suite(*id, &nouveau, rang as i64)).is_none() {
            bail!("base illisible pendant le recodage de la suite");
        }
    }
    let _ = fs::remove_file(&chemin);
    oublier_suite(&chemin);
    tracing::info!(gardees = gardees.len(), retirees = retirees.len(), "suite privée recodée sans les images effacées");
    Ok(())
}

/// Les images privées encore rangées une à une (avant les suites) sont recopiées en suites
/// pendant que la clé est ouverte (Touch ID). Les anciens fichiers, devenus inutiles, sont effacés
/// au rangement suivant, frise fermée.
fn convertir_les_anciennes() {
    if CONVERSION_EN_COURS.swap(true, Ordering::SeqCst) {
        return;
    }
    let lance = std::thread::Builder::new().name("bscreen-prive-conversion".into()).spawn(|| {
        match convertir() {
            Ok(0) => {}
            Ok(n) => tracing::info!(images = n, "anciennes images privées recopiées en suites"),
            Err(e) => tracing::warn!(%e, "anciennes images privées pas toutes recopiées (reprise au prochain Touch ID)"),
        }
        CONVERSION_EN_COURS.store(false, Ordering::SeqCst);
    });
    if lance.is_err() {
        CONVERSION_EN_COURS.store(false, Ordering::SeqCst);
    }
}

fn convertir() -> Result<usize> {
    let anciennes = crate::moteur::avec_base(|b| b.privees_une_a_une(100_000)).ok_or_else(|| anyhow!("base"))?;
    // Par morceau, 30 images au plus par suite.
    let mut groupes: Vec<Vec<(i64, String, i64)>> = Vec::new();
    let mut morceau_courant = None;
    for (id, morceau, fichier, quand) in anciennes {
        if morceau_courant != Some(morceau) || groupes.last().is_none_or(|g| g.len() >= IMAGES_PAR_SUITE) {
            groupes.push(Vec::new());
            morceau_courant = Some(morceau);
        }
        if let Some(g) = groupes.last_mut() {
            g.push((id, fichier, quand));
        }
    }
    let mut faites = 0;
    for groupe in groupes {
        // Reverrouillé (frise fermée, 5 minutes passées) : on reprendra au prochain Touch ID.
        let Some(cle) = cle_privee() else { break };
        let _seul = crate::effacement::REECRITURE.lock().unwrap_or_else(|e| e.into_inner());
        faites += objc2::rc::autoreleasepool(|_| convertir_groupe(&groupe, &cle))?;
    }
    Ok(faites)
}

fn convertir_groupe(groupe: &[(i64, String, i64)], cle: &SecKey) -> Result<usize> {
    let mut encodeur: Option<Encodeur> = None;
    let mut membres: Vec<(i64, usize)> = Vec::new();
    for (id, fichier, _) in groupe {
        let image = match cgimage_avec(&chemins::donnees().join(fichier), cle) {
            Ok(image) => image,
            Err(e) => {
                tracing::warn!(%e, image = id, "ancienne image privée illisible : laissée telle quelle");
                continue;
            }
        };
        let tampon = crate::suite::tampon_depuis_cgimage(&image)?;
        let ajout = (|| -> Result<usize> {
            if encodeur.is_none() {
                // SAFETY : image valide.
                let (l, h) = unsafe {
                    let p = Retained::as_ptr(&image).cast();
                    (CGImageGetWidth(p), CGImageGetHeight(p))
                };
                encodeur = Some(Encodeur::nouveau(l, h)?);
            }
            let e = encodeur.as_mut().ok_or_else(|| anyhow!("encodeur absent"))?;
            let rang = e.nombre();
            e.ajouter(&tampon)?;
            Ok(rang)
        })();
        membres.push((*id, ajout?));
    }
    let (Some(encodeur), Some((premier, _, quand))) = (encodeur, groupe.first()) else {
        return Ok(0);
    };
    let suite = encodeur.terminer()?;
    let relatif = chemin_suite(&jour_local(*quand), *premier);
    ranger_suite(&suite, &relatif)?;
    // Relue depuis le disque (déchiffrée, décodée) avant que la base ne la désigne.
    let relue = lire_suite(&chemins::donnees().join(&relatif), cle)
        .and_then(|s| s.image(s.nombre().saturating_sub(1)).map(|_| ()));
    if let Err(e) = relue {
        let _ = fs::remove_file(chemins::donnees().join(&relatif));
        return Err(e).context("suite recopiée illisible : anciennes images gardées");
    }
    for (id, rang) in &membres {
        if crate::moteur::avec_base(|b| b.noter_dans_suite(*id, &relatif, *rang as i64)).is_none() {
            bail!("base illisible pendant la recopie");
        }
    }
    // Les anciens fichiers restent encore 48 h, comptées d'ici (date remise à maintenant).
    for (id, fichier, _) in groupe {
        if membres.iter().any(|(m, _)| m == id)
            && let Ok(f) = fs::OpenOptions::new().write(true).open(chemins::donnees().join(fichier))
        {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
    }
    Ok(membres.len())
}

/// Les fichiers privés que plus rien n'utilise (anciens fichiers recopiés en suites, restes d'un
/// arrêt brutal) : effacés, frise fermée. Les anciens fichiers d'une image (`.bsp`) restent 48 h
/// après leur recopie (on pourrait encore revenir en arrière) ; les autres, 10 minutes.
fn effacer_orphelins() {
    let Some(utilises) = crate::moteur::avec_base(|b| b.fichiers_prives_utilises()) else {
        return;
    };
    let Ok(jours) = fs::read_dir(dossier()) else { return };
    let mut effaces = 0usize;
    for jour in jours.flatten() {
        let Ok(fichiers) = fs::read_dir(jour.path()) else { continue };
        for fichier in fichiers.flatten() {
            let nom = fichier.file_name().to_string_lossy().into_owned();
            let relatif = format!("prive/{}/{nom}", jour.file_name().to_string_lossy());
            if !est_prive(&relatif) || utilises.contains(&relatif) {
                continue;
            }
            // Un ancien fichier d'une image recopié en suite porte la date de sa recopie : il reste
            // 48 h (on pourrait encore revenir en arrière). Les autres restes, 10 minutes.
            let delai = if nom.ends_with(&format!(".{EXTENSION}")) {
                Duration::from_secs(48 * 3600)
            } else {
                Duration::from_secs(600)
            };
            let vieux = fichier
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|m| m.elapsed().is_ok_and(|d| d > delai));
            if vieux && fs::remove_file(fichier.path()).is_ok() {
                effaces += 1;
            }
        }
    }
    if effaces > 0 {
        tracing::info!(fichiers = effaces, "fichiers privés inutilisés effacés");
    }
}

// ---------- HEIC (ImageIO) ----------

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithData(data: *const c_void, options: *const c_void) -> *mut c_void;
    fn CGImageSourceCreateImageAtIndex(source: *mut c_void, index: usize, options: *const c_void) -> *mut c_void;
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(objet: *const c_void);
}

fn depuis_heic(octets: &[u8]) -> Result<Retained<CGImage>> {
    let donnees = CFData::from_buffer(octets);
    // SAFETY : la source est libérée ci-dessous ; l'image rendue suit la règle « create ».
    unsafe {
        let source = CGImageSourceCreateWithData(donnees.as_concrete_TypeRef().cast(), std::ptr::null());
        if source.is_null() {
            bail!("image privée illisible");
        }
        let image = CGImageSourceCreateImageAtIndex(source, 0, std::ptr::null());
        CFRelease(source);
        if image.is_null() {
            bail!("image privée vide");
        }
        Retained::from_raw(image.cast::<CGImage>()).ok_or_else(|| anyhow!("image privée vide"))
    }
}

// ---------- ranger les moments privés ----------

/// Sort des vidéos les images privées des morceaux terminés : codées en suites chiffrées dans
/// `prive/`, puis retirées de la vidéo (réécrite). Rend le nombre d'images rangées. Hors du fil
/// principal. `a_blanc` : ne touche à rien, compte seulement.
pub fn ranger_les_moments_prives(a_blanc: bool) -> Result<usize> {
    // Jamais pendant un effacement (les deux réécrivent les vidéos), ni frise ouverte (elle lit
    // les vidéos telles qu'elles étaient à son ouverture).
    if crate::moteur::interface_est_ouverte() {
        return Ok(0);
    }
    let Ok(_seul) = crate::effacement::REECRITURE.try_lock() else {
        return Ok(0);
    };
    if !a_blanc {
        effacer_orphelins();
    }
    let a_ranger = crate::moteur::avec_base(|b| b.privees_a_chiffrer(10_000)).ok_or_else(|| anyhow!("base"))?;
    if a_ranger.is_empty() {
        return Ok(0);
    }
    let mut par_morceau: BTreeMap<i64, Vec<&bscreen_store::PriveeARanger>> = BTreeMap::new();
    for p in &a_ranger {
        par_morceau.entry(p.morceau).or_default().push(p);
    }
    if a_blanc {
        tracing::info!(images = a_ranger.len(), morceaux = par_morceau.len(), "moments privés à ranger (essai à blanc)");
        return Ok(a_ranger.len());
    }
    // Pas de clé (Trousseau refusé…) : on ne touche à rien.
    cle_publique()?;
    let (mut rangees, mut octets) = (0usize, 0u64);
    for (morceau, images) in &par_morceau {
        match ranger_morceau(*morceau, images) {
            Ok((n, o)) => {
                rangees += n;
                octets += o;
            }
            Err(e) => tracing::warn!(%e, morceau, "moments privés pas rangés : gardés dans la vidéo, nouvel essai plus tard"),
        }
    }
    tracing::info!(
        rangees,
        ko_par_image = (octets / 1024).checked_div(rangees as u64).unwrap_or(0),
        "moments privés chiffrés à part"
    );
    Ok(rangees)
}

/// Les images privées d'un morceau : une seule relecture de la vidéo en écrit une copie sans elles
/// et les code en suites. Les suites sont chiffrées et vérifiées, la base pointe sur elles, puis la
/// copie remplace la vidéo. Au moindre échec, tout revient comme avant.
fn ranger_morceau(morceau: i64, images: &[&bscreen_store::PriveeARanger]) -> Result<(usize, u64)> {
    use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};
    let fichier = &images.first().ok_or_else(|| anyhow!("rien à ranger"))?.fichier;
    let chemin_video = chemins::donnees().join(fichier);
    let positions: HashSet<i64> = images.iter().map(|p| p.position_ms).collect();
    // 1. La relecture : copie sans les images privées, et ces images codées en suites.
    type Membres<'a> = Vec<(&'a bscreen_store::PriveeARanger, usize)>;
    let mut suites: Vec<(Encodeur, Membres)> = Vec::new();
    let copie = crate::effacement::reecrire_en_separant(&chemin_video, &positions, &mut |tampon, position| {
        let siennes: Vec<&bscreen_store::PriveeARanger> =
            images.iter().copied().filter(|p| (p.position_ms - position).abs() <= 2).collect();
        if siennes.is_empty() {
            return Ok(());
        }
        if suites.last().is_none_or(|(e, _)| e.nombre() >= IMAGES_PAR_SUITE) {
            let (l, h) = (CVPixelBufferGetWidth(tampon), CVPixelBufferGetHeight(tampon));
            suites.push((Encodeur::nouveau(l, h)?, Vec::new()));
        }
        let (encodeur, membres) = suites.last_mut().ok_or_else(|| anyhow!("suite absente"))?;
        let rang = encodeur.nombre();
        encodeur.ajouter(tampon)?;
        membres.extend(siennes.into_iter().map(|p| (p, rang)));
        Ok(())
    })?;
    let annuler = |ecrites: &[(String, Membres)]| {
        for (relatif, _) in ecrites {
            let _ = fs::remove_file(chemins::donnees().join(relatif));
        }
        if let Some(c) = &copie {
            let _ = fs::remove_file(c);
        }
    };
    // 2. Chaque suite chiffrée dans son fichier.
    let mut ecrites: Vec<(String, Membres)> = Vec::new();
    let mut octets = 0u64;
    for (encodeur, membres) in suites {
        let ecrit = encodeur.terminer().and_then(|suite| {
            // La suite se relit-elle ? (sa dernière image : toute la suite est décodée)
            suite.image(suite.nombre().saturating_sub(1))?;
            let (premier, _) = membres.first().ok_or_else(|| anyhow!("suite vide"))?;
            let relatif = chemin_suite(&jour_local(premier.quand_ms), premier.id);
            Ok((ranger_suite(&suite, &relatif)?, relatif))
        });
        match ecrit {
            Ok((n, relatif)) => {
                octets += n;
                ecrites.push((relatif, membres));
            }
            Err(e) => {
                annuler(&ecrites);
                return Err(e);
            }
        }
    }
    let Some((premiere, _)) = ecrites.first() else {
        annuler(&ecrites);
        bail!("aucune image privée trouvée dans {fichier}");
    };
    // 3. Une fois par lancement, l'aller-retour complet (relire, déchiffrer, décoder) avant de
    //    retirer quoi que ce soit de la vidéo : sinon ces images seraient perdues.
    if !VERIFIE.load(Ordering::SeqCst) {
        if let Err(e) = verifier(premiere) {
            annuler(&ecrites);
            bail!("images privées illisibles après chiffrement, rien n'est retiré de la vidéo : {e}");
        }
        VERIFIE.store(true, Ordering::SeqCst);
    }
    // 4. La base pointe sur les suites (la position devient le rang dans la suite)…
    let remettre = |ecrites: &[(String, Membres)]| {
        for (_, membres) in ecrites {
            for (p, _) in membres {
                crate::moteur::avec_base(|b| b.remettre_dans_la_video(p.id, p.position_ms));
            }
        }
    };
    for (relatif, membres) in &ecrites {
        for (p, rang) in membres {
            if crate::moteur::avec_base(|b| b.noter_dans_suite(p.id, relatif, *rang as i64)).is_none() {
                remettre(&ecrites);
                annuler(&ecrites);
                bail!("base illisible : images privées laissées dans la vidéo");
            }
        }
    }
    // 5. … puis la copie remplace la vidéo (qui disparaît si elle n'avait que des images privées).
    let remplacee = match &copie {
        Some(c) => fs::rename(c, &chemin_video).map(|()| fs::metadata(&chemin_video).map(|m| m.len()).unwrap_or(0)),
        None => match fs::remove_file(&chemin_video) {
            Ok(()) => Ok(0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        },
    };
    match remplacee {
        Ok(taille) => {
            crate::moteur::avec_base(|b| b.changer_taille_morceau(morceau, taille));
        }
        Err(e) => {
            remettre(&ecrites);
            annuler(&ecrites);
            return Err(e).context("vidéo pas remplacée : images privées remises dans la vidéo");
        }
    }
    Ok((ecrites.iter().map(|(_, m)| m.len()).sum(), octets))
}

/// Les images privées se relisent-elles ? (clé privée du Trousseau, sans rien montrer)
fn verifier(relatif: &str) -> Result<()> {
    let octets = passwords::get_generic_password(SERVICE, COMPTE).context("clé privée absente du Trousseau")?;
    let cle = importer(&octets, true)?;
    let chemin = chemins::donnees().join(relatif);
    let image = if est_suite(relatif) {
        let suite = lire_suite(&chemin, &cle)?;
        // La dernière : toute la suite est décodée pour l'obtenir.
        suite.image(suite.nombre().saturating_sub(1))?
    } else {
        cgimage_avec(&chemin, &cle)?
    };
    // SAFETY : image valide.
    let largeur = unsafe { CGImageGetWidth(Retained::as_ptr(&image).cast()) };
    if largeur == 0 {
        bail!("image vide");
    }
    tracing::info!(largeur, "moments privés : aller-retour vérifié");
    Ok(())
}

static VERIFIE: AtomicBool = AtomicBool::new(false);

/// « 2026-11-20 » (jour local).
fn jour_local(ms: i64) -> String {
    let secondes = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY : localtime_r n'écrit que dans `tm`.
    unsafe { libc::localtime_r(&secondes, &mut tm) };
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

/// Efface les fichiers privés donnés (relatifs au dossier des données).
pub fn effacer_fichiers(fichiers: &[String]) {
    for f in fichiers {
        if est_prive(f) {
            let _ = fs::remove_file(chemins::donnees().join(f));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chemins_prives() {
        assert_eq!(chemin_suite("2026-11-22", 42), "prive/2026-11-22/42.bsv");
        assert!(est_prive("prive/2026-11-20/42.bsp") && !est_suite("prive/2026-11-20/42.bsp"));
        assert!(est_prive("prive/2026-11-22/42.bsv") && est_suite("prive/2026-11-22/42-1763769600000.bsv"));
        assert!(!est_prive("video/2026-11-20/ecran1-101500.mp4"));
    }
}
