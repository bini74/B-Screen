//! Pilote la capture : démarrage, pause, autorisations, changements d'app,
//! puis l'enregistrement de chaque image (vidéo + base) et la purge.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ptr::NonNull;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use bscreen_core::chemins;
use bscreen_core::reglages::Reglages;
use bscreen_store::{Base, NouveauMorceau, NouvelleImage};
use objc2_app_kit::{
    NSWorkspace, NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidWakeNotification,
    NSWorkspaceScreensDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
};
use objc2_foundation::NSNotification;

use crate::autorisations::Autorisation;
use crate::capture::{self, Capteur, ImageCapturee};
use crate::encodage::{MorceauEnCours, ms_depuis_1970};

/// Après un échec, on attend avant de réessayer (pas de boucle d'erreurs).
const ATTENTE_APRES_ECHEC: Duration = Duration::from_secs(30);

static CAPTEUR: Mutex<Option<Capteur>> = Mutex::new(None);
static DEMARRAGE_EN_COURS: AtomicBool = AtomicBool::new(false);
static ECHEC: Mutex<Option<(Instant, String)>> = Mutex::new(None);
/// Faux pendant l'arrêt : les dernières images en vol ne rouvrent pas de morceau.
static ENREGISTREMENT_ACTIF: AtomicBool = AtomicBool::new(false);
/// Vrai pendant que la frise (ou une autre fenêtre de B-Screen) est ouverte : rien n'est enregistré.
static INTERFACE_OUVERTE: AtomicBool = AtomicBool::new(false);

/// La frise est-elle ouverte ?
pub fn interface_est_ouverte() -> bool {
    INTERFACE_OUVERTE.load(Ordering::SeqCst)
}

/// La base, ouverte au premier besoin.
static BASE: LazyLock<Mutex<Option<Base>>> = LazyLock::new(|| {
    let fichier = chemins::donnees().join("index.sqlite");
    Mutex::new(match Base::ouvrir(&fichier) {
        Ok(base) => Some(base),
        Err(e) => {
            tracing::error!(%e, fichier = %fichier.display(), "base inaccessible");
            None
        }
    })
});

/// Le morceau vidéo en cours, par écran.
static MORCEAUX: LazyLock<Mutex<HashMap<u32, MorceauEnCours>>> = LazyLock::new(Default::default);

/// L'app au premier plan (identifiant, nom), tenue à jour par les notifications de macOS.
static APP_COURANTE: Mutex<(Option<String>, Option<String>, Option<i32>)> = Mutex::new((None, None, None));

/// Les apps exclues (gardées en mémoire, relues quand le réglage change) : leur texte n'est jamais lu.
static EXCLUE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Relit la liste des apps exclues (au démarrage et quand Julien la change).
pub fn noter_apps_exclues() {
    let mut liste: Vec<String> = Reglages::charger().apps_exclues;
    liste.extend(capture::APPS_EXCLUES_FIXES.iter().map(|s| s.to_string()));
    *verrou(&EXCLUE) = liste;
}

/// L'app au premier plan : (identifiant, nom).
pub fn app_au_premier_plan() -> (Option<String>, Option<String>) {
    let (bundle, nom, _) = verrou(&APP_COURANTE).clone();
    (bundle, nom)
}

fn verrou<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[track_caller]
pub(crate) fn avec_base<T>(f: impl FnOnce(&Base) -> bscreen_store::Result<T>) -> Option<T> {
    let lieu = std::panic::Location::caller();
    let base = verrou(&BASE);
    match f(base.as_ref()?) {
        Ok(valeur) => Some(valeur),
        Err(e) => {
            tracing::warn!(%e, %lieu, "base");
            None
        }
    }
}

pub fn en_marche() -> bool {
    verrou(&CAPTEUR).is_some()
}

/// Le dernier échec de démarrage, s'il est récent.
pub fn dernier_echec() -> Option<String> {
    verrou(&ECHEC).as_ref().map(|(_, message)| message.clone())
}

/// Démarre la capture si elle est autorisée et pas en pause. Ne bloque pas.
pub fn demarrer_si_possible(pause: bool) {
    if pause || en_marche() || DISQUE_PLEIN.load(Ordering::SeqCst) || !Autorisation::Ecran.etat().accordee() {
        return;
    }
    if let Some((quand, _)) = verrou(&ECHEC).as_ref()
        && quand.elapsed() < ATTENTE_APRES_ECHEC
    {
        return;
    }
    if DEMARRAGE_EN_COURS.swap(true, Ordering::SeqCst) {
        return;
    }
    let lance = std::thread::Builder::new().name("bscreen-demarrage".into()).spawn(|| {
        ENREGISTREMENT_ACTIF.store(true, Ordering::SeqCst);
        let quand_arrete = |raison: String| relancer(&format!("macOS a arrêté le flux : {raison}"));
        let reglages = Reglages::charger();
        let audio = capture::ChoixAudio {
            micro: reglages.micro && Autorisation::Micro.etat().accordee(),
            son_du_mac: reglages.son_du_mac,
        };
        match Capteur::demarrer(traiter, quand_arrete, audio) {
            Ok(capteur) => {
                *verrou(&CAPTEUR) = Some(capteur);
                *verrou(&ECHEC) = None;
            }
            Err(e) => {
                // La même raison qu'au dernier essai (écran éteint…) : une seule ligne dans le journal.
                static DERNIERE_RAISON: Mutex<String> = Mutex::new(String::new());
                let raison = e.to_string();
                let mut derniere = verrou(&DERNIERE_RAISON);
                if *derniere == raison {
                    tracing::debug!(%e, "capture impossible");
                } else {
                    tracing::warn!(%e, "capture impossible (essai toutes les 30 s)");
                    *derniere = raison.clone();
                }
                drop(derniere);
                *verrou(&ECHEC) = Some((Instant::now(), raison));
            }
        }
        DEMARRAGE_EN_COURS.store(false, Ordering::SeqCst);
    });
    if lance.is_err() {
        DEMARRAGE_EN_COURS.store(false, Ordering::SeqCst);
    }
}

static RELANCE_EN_COURS: AtomicBool = AtomicBool::new(false);

/// Relance la capture (flux arrêté par macOS, réveil, flux muet). Ne bloque pas le fil appelant.
pub fn relancer(raison: &str) {
    if RELANCE_EN_COURS.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::warn!(raison, "capture relancée");
    let lance = std::thread::Builder::new().name("bscreen-relance".into()).spawn(|| {
        arreter();
        std::thread::sleep(Duration::from_millis(500));
        *verrou(&ECHEC) = None;
        demarrer_si_possible(Reglages::charger().pause);
        RELANCE_EN_COURS.store(false, Ordering::SeqCst);
    });
    if lance.is_err() {
        RELANCE_EN_COURS.store(false, Ordering::SeqCst);
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(source: i32, genre: u32) -> f64;
}

/// Secondes depuis la dernière action de Julien (clavier, souris, trackpad).
pub fn secondes_d_inactivite() -> f64 {
    // kCGEventSourceStateCombinedSessionState = 0, kCGAnyInputEventType = !0
    unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) }
}

/// Arrête la capture et termine les fichiers vidéo (attend au plus 3 s).
pub fn arreter() {
    ENREGISTREMENT_ACTIF.store(false, Ordering::SeqCst);
    let capteur = verrou(&CAPTEUR).take();
    if let Some(capteur) = capteur {
        capteur.arreter();
    }
    crate::audio::interrompre();
    let ouverts: Vec<MorceauEnCours> = verrou(&MORCEAUX).drain().map(|(_, m)| m).collect();
    let attentes: Vec<mpsc::Receiver<()>> = ouverts.into_iter().map(fermer).collect();
    for attente in attentes {
        let _ = attente.recv_timeout(Duration::from_secs(3));
    }
}

/// Termine proprement (vidéos fermées) et relance l'app, quand elle tourne depuis son paquet.
pub fn relancer_l_app() {
    arreter();
    crate::ollama::arreter();
    let paquet = std::env::current_exe()
        .ok()
        .and_then(|e| e.ancestors().nth(3).map(Path::to_path_buf))
        .filter(|p| p.extension().is_some_and(|x| x == "app"));
    if let Some(paquet) = paquet {
        // Relancée par un shell détaché, une fois celle-ci partie.
        let _ = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 3; exec /usr/bin/open -n \"$0\"")
            .arg(&paquet)
            .spawn();
    }
    std::process::exit(0);
}

/// La frise s'ouvre ou se ferme. Ouverte : plus rien n'est enregistré, et les morceaux
/// en cours sont terminés pour que la frise puisse les lire jusqu'à la dernière image.
pub fn interface_ouverte(ouverte: bool) {
    let debut = Instant::now();
    INTERFACE_OUVERTE.store(ouverte, Ordering::SeqCst);
    if ouverte {
        // Les heures de fin du texte en attente sont écrites (la frise voit tout jusqu'à maintenant).
        crate::indexation::reporter_maintenant();
        let ouverts: Vec<MorceauEnCours> = verrou(&MORCEAUX).drain().map(|(_, m)| m).collect();
        let attentes: Vec<mpsc::Receiver<()>> = ouverts.into_iter().map(fermer).collect();
        for attente in attentes {
            let _ = attente.recv_timeout(Duration::from_millis(1500));
        }
    }
    tracing::info!(ouverte, ms = debut.elapsed().as_millis(), "interface");
}

static EFFACEMENT_EN_COURS: AtomicBool = AtomicBool::new(false);

/// Un effacement commence (`true`) ou finit : pendant ce temps rien n'est enregistré, et les
/// vidéos en cours sont terminées pour pouvoir en retirer les images.
pub fn suspendre_pour_effacement(suspendu: bool) {
    EFFACEMENT_EN_COURS.store(suspendu, Ordering::SeqCst);
    if suspendu {
        let ouverts: Vec<MorceauEnCours> = verrou(&MORCEAUX).drain().map(|(_, m)| m).collect();
        let attentes: Vec<mpsc::Receiver<()>> = ouverts.into_iter().map(fermer).collect();
        for attente in attentes {
            let _ = attente.recv_timeout(Duration::from_secs(3));
        }
    }
}

/// Chaque image gardée : écrite dans la vidéo de son écran, puis inscrite dans la base.
fn traiter(image: ImageCapturee) {
    if !ENREGISTREMENT_ACTIF.load(Ordering::SeqCst)
        || INTERFACE_OUVERTE.load(Ordering::SeqCst)
        || EFFACEMENT_EN_COURS.load(Ordering::SeqCst)
    {
        return;
    }
    let mut morceaux = verrou(&MORCEAUX);
    if morceaux.get(&image.ecran).is_some_and(|m| m.a_remplacer(&image))
        && let Some(ancien) = morceaux.remove(&image.ecran)
    {
        fermer(ancien);
    }
    if let Entry::Vacant(place) = morceaux.entry(image.ecran) {
        let Some(morceau) = ouvrir_morceau(&image) else { return };
        place.insert(morceau);
    }
    let Some(morceau) = morceaux.get_mut(&image.ecran) else {
        return;
    };
    match morceau.ajouter(&image) {
        Ok(position_ms) => {
            let (bundle, nom, pid) = verrou(&APP_COURANTE).clone();
            let id_morceau = morceau.id_base;
            let id_image = avec_base(|b| {
                b.ajouter_image(&NouvelleImage {
                    morceau: id_morceau,
                    quand_ms: ms_depuis_1970(image.quand),
                    position_ms,
                    ecran: image.ecran,
                    app_bundle: bundle.as_deref(),
                    app_nom: nom.as_deref(),
                    aire: image.aire_modifiee,
                    declenchee: image.declenchee,
                })
            });
            // Le texte de l'écran à ce moment, lu à part (la capture n'attend pas) — jamais celui
            // d'une app exclue.
            let app_exclue = bundle.as_deref().is_some_and(|b| verrou(&EXCLUE).iter().any(|e| e == b));
            if let Some(id_image) = id_image.filter(|_| !app_exclue) {
                crate::indexation::indexer(crate::indexation::Travail {
                    image: id_image,
                    ecran: image.ecran,
                    quand_ms: ms_depuis_1970(image.quand),
                    pid,
                    navigateur: bundle
                        .as_deref()
                        .is_some_and(|b| crate::texte::NAVIGATEURS.contains(&b)),
                    declenchee: image.declenchee,
                    tampon: Some(image.tampon),
                });
            }
        }
        Err(e) => tracing::warn!(%e, ecran = image.ecran, "image non enregistrée"),
    }
}

fn ouvrir_morceau(image: &ImageCapturee) -> Option<MorceauEnCours> {
    let (jour, heure) = date_locale(image.quand);
    // Deux morceaux dans la même seconde (changement de résolution) : suffixe.
    for essai in 1..=5 {
        let suffixe = if essai == 1 { String::new() } else { format!("-{essai}") };
        let relatif = format!("video/{jour}/ecran{}-{heure}{suffixe}.mp4", image.ecran);
        if chemins::donnees().join(&relatif).exists() {
            continue;
        }
        let Some(id) = avec_base(|b| {
            b.nouveau_morceau(&NouveauMorceau {
                ecran: image.ecran,
                fichier: &relatif,
                debut_ms: ms_depuis_1970(image.quand),
                largeur: image.largeur as u32,
                hauteur: image.hauteur as u32,
            })
        }) else {
            continue;
        };
        return match MorceauEnCours::ouvrir(
            chemins::donnees().join(&relatif),
            id,
            image.quand,
            image.largeur,
            image.hauteur,
            true,
        ) {
            Ok(morceau) => {
                tracing::info!(fichier = relatif, "nouveau morceau");
                Some(morceau)
            }
            Err(e) => {
                tracing::warn!(%e, "morceau impossible");
                avec_base(|b| b.supprimer_morceau(id));
                None
            }
        };
    }
    None
}

fn fermer(morceau: MorceauEnCours) -> mpsc::Receiver<()> {
    let (id, fin_ms) = (morceau.id_base, morceau.fin_ms());
    morceau.fermer(move |octets| {
        avec_base(|b| b.fermer_morceau(id, fin_ms, octets));
        tracing::info!(id, ko = octets / 1024, "morceau terminé");
    })
}

/// (« 2026-11-19 », « 215700 ») à l'heure locale.
fn date_locale(quand: SystemTime) -> (String, String) {
    let secondes = quand
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY : localtime_r écrit seulement dans `tm`, une structure C ordinaire.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secondes, &mut tm) };
    (
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        format!("{:02}{:02}{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
    )
}

/// Au démarrage : les morceaux restés ouverts après un arrêt brutal sont rendus cohérents
/// (on retire de la base les images jamais arrivées sur le disque). À appeler avant la capture.
pub fn reparer() {
    let ouverts: std::collections::HashSet<i64> = verrou(&MORCEAUX).values().map(|m| m.id_base).collect();
    let Some(a_reparer) = avec_base(|b| b.morceaux_non_fermes()) else {
        return;
    };
    for morceau in a_reparer.into_iter().filter(|m| !ouverts.contains(&m.id)) {
        let chemin = chemins::donnees().join(&morceau.fichier);
        let octets = std::fs::metadata(&chemin).map(|m| m.len()).unwrap_or(0);
        match crate::lecture::duree_lisible_ms(&chemin) {
            Some(duree) if octets > 0 && duree > 0 => {
                let retirees = avec_base(|b| b.supprimer_images_apres(morceau.id, duree)).unwrap_or(0);
                avec_base(|b| b.fermer_morceau(morceau.id, morceau.debut_ms + duree, octets));
                tracing::info!(fichier = morceau.fichier, retirees, "morceau réparé");
            }
            _ => {
                let _ = std::fs::remove_file(&chemin);
                avec_base(|b| b.supprimer_morceau(morceau.id));
                tracing::warn!(fichier = morceau.fichier, "morceau illisible supprimé");
            }
        }
    }
}

/// Supprime les morceaux plus vieux que la durée de conservation.
/// Une seule fois : masque les secrets du texte stocké avant que le masquage existe (phase 6.3).
pub fn masquer_retroactivement() {
    const CLE: &str = "masquage-v1";
    if avec_base(|b| b.meta(CLE)).flatten().is_some() {
        return;
    }
    let debut = Instant::now();
    let masquer = |t: &str| {
        let masque = if t.starts_with("http://") || t.starts_with("https://") {
            bscreen_core::masquage::masquer_url(t)
        } else {
            bscreen_core::masquage::masquer(t)
        };
        match masque {
            std::borrow::Cow::Owned(m) => Some(m),
            std::borrow::Cow::Borrowed(_) => None,
        }
    };
    if let Some(lignes) = avec_base(|b| b.remasquer(masquer)) {
        crate::indexation::textes_oublies();
        avec_base(|b| b.noter_meta(CLE, "1"));
        avec_base(|b| b.vider_journal());
        tracing::info!(lignes, ms = debut.elapsed().as_millis() as u64, "secrets déjà stockés : masqués");
    }
}

pub fn purger() {
    let jours = Reglages::charger().conservation_jours;
    // L'archive du homelab en service (phase 10) : rien n'est effacé, l'index reste sur le Mac.
    if jours == 0 || crate::archive::active() {
        return;
    }
    let limite = ms_depuis_1970(SystemTime::now()) - i64::from(jours) * 86_400_000;
    avec_base(|b| b.oublier_textes_avant(limite));
    crate::indexation::textes_oublies();
    if let Some(fichiers) = avec_base(|b| b.oublier_voix_avant(limite)) {
        for fichier in &fichiers {
            let _ = std::fs::remove_file(chemins::donnees().join(fichier));
        }
        if !fichiers.is_empty() {
            tracing::info!(passages = fichiers.len(), jours, "purge des voix");
        }
    }
    let Some(vieux) = avec_base(|b| b.morceaux_avant(limite)) else {
        return;
    };
    let mut octets = 0u64;
    for morceau in &vieux {
        let _ = std::fs::remove_file(chemins::donnees().join(&morceau.fichier));
        if let Some(prives) = avec_base(|b| b.fichiers_prives_du_morceau(morceau.id)) {
            crate::prive::effacer_fichiers(&prives);
        }
        avec_base(|b| b.supprimer_morceau(morceau.id));
        octets += morceau.octets.unwrap_or(0);
    }
    if !vieux.is_empty() {
        tracing::info!(morceaux = vieux.len(), mo = octets / 1_048_576, jours, "purge");
    }
}

/// Le disque est presque plein : l'enregistrement est en pause jusqu'à ce qu'il y ait de la place.
pub static DISQUE_PLEIN: AtomicBool = AtomicBool::new(false);
/// En dessous : pause automatique ; au-dessus de la reprise : l'enregistrement repart.
const PLACE_MINIMALE: u64 = 5 * 1024 * 1024 * 1024;
const PLACE_REPRISE: u64 = 8 * 1024 * 1024 * 1024;

/// Place libre (octets) sur le disque des données.
pub fn place_libre() -> Option<u64> {
    let chemin = std::ffi::CString::new(chemins::donnees().to_string_lossy().as_bytes()).ok()?;
    let mut infos: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY : statfs n'écrit que dans `infos`.
    (unsafe { libc::statfs(chemin.as_ptr(), &mut infos) } == 0).then(|| infos.f_bavail * u64::from(infos.f_bsize))
}

/// Chaque minute : sous 5 Go libres, l'enregistrement se met en pause (le Mac a besoin de place) ;
/// il reprend au-dessus de 8 Go.
pub fn surveiller_place() {
    let Some(libre) = place_libre() else { return };
    let plein = DISQUE_PLEIN.load(Ordering::SeqCst);
    if !plein && libre < PLACE_MINIMALE {
        DISQUE_PLEIN.store(true, Ordering::SeqCst);
        tracing::warn!(libre_mo = libre / 1_048_576, "disque presque plein : enregistrement en pause");
        arreter();
    } else if plein && libre > PLACE_REPRISE {
        DISQUE_PLEIN.store(false, Ordering::SeqCst);
        tracing::info!(libre_mo = libre / 1_048_576, "place retrouvée : l'enregistrement reprend");
        demarrer_si_possible(Reglages::charger().pause);
    }
}

/// Place prise sur le disque (octets) : vidéos, voix, base — mesurée toutes les 10 minutes.
pub static PLACE_VIDEO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PLACE_VOIX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PLACE_BASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn taille_dossier(dossier: &std::path::Path) -> u64 {
    let Ok(entrees) = std::fs::read_dir(dossier) else { return 0 };
    entrees
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => taille_dossier(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Processeur utilisé par B-Screen sur la dernière minute, en dixièmes de pourcent d'un cœur.
pub static CPU_POUR_MILLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
/// Mémoire occupée (octets), comptée comme le Moniteur d'activité (empreinte physique).
pub static MEMOIRE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Temps de processeur consommé depuis le lancement (µs) et mémoire occupée (octets).
fn ressources() -> (u64, u64) {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY : getrusage n'écrit que dans `usage`.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let us = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    // L'empreinte physique, pas la mémoire résidente : celle-ci compte aussi les bibliothèques
    // partagées et ce que macOS peut reprendre à tout moment (elle double presque le chiffre).
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY : proc_pid_rusage remplit `info` (version 2) pour notre propre processus.
    let ok = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V2,
            (&mut info as *mut libc::rusage_info_v2).cast(),
        )
    } == 0;
    (us(usage.ru_utime) + us(usage.ru_stime), if ok { info.ri_phys_footprint } else { 0 })
}

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// Rend au système la mémoire déjà libérée que malloc garde pour plus tard (après la frise,
/// après le modèle de sens : des centaines de Mo d'images décodées).
pub fn rendre_la_memoire() {
    // SAFETY : zone nulle = toutes les zones ; but 0 = tout ce qui peut être rendu.
    let rendu = unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    let (_, apres) = ressources();
    MEMOIRE.store(apres, Ordering::Relaxed);
    tracing::info!(rendu_mo = rendu / 1_000_000, memoire_mo = apres / 1_000_000, "mémoire libre rendue au système");
}

/// À appeler chaque minute : met à jour le processeur (sur la minute écoulée) et la mémoire.
pub fn mesurer_ressources() {
    static PRECEDENT: Mutex<Option<(Instant, u64)>> = Mutex::new(None);
    let (cpu_us, memoire) = ressources();
    MEMOIRE.store(memoire, Ordering::Relaxed);
    let mut precedent = verrou(&PRECEDENT);
    if let Some((quand, avant)) = *precedent {
        let ecoule = quand.elapsed().as_micros() as u64;
        if let Some(pour_mille) = (cpu_us.saturating_sub(avant) * 1000).checked_div(ecoule) {
            CPU_POUR_MILLE.store(pour_mille, Ordering::Relaxed);
        }
    }
    *precedent = Some((Instant::now(), cpu_us));
}

/// Mesure la place prise sur le disque (hors du fil principal).
pub fn mesurer_place() {
    let donnees = chemins::donnees();
    let base: u64 = ["index.sqlite", "index.sqlite-wal", "index.sqlite-shm"]
        .iter()
        .filter_map(|f| std::fs::metadata(donnees.join(f)).ok())
        .map(|m| m.len())
        .sum();
    PLACE_VIDEO.store(taille_dossier(&donnees.join("video")), Ordering::Relaxed);
    PLACE_VOIX.store(taille_dossier(&donnees.join("audio")), Ordering::Relaxed);
    PLACE_BASE.store(base, Ordering::Relaxed);
}

/// Ce qu'il y a dans la base, pour le journal et le menu.
pub fn bilan() -> Option<bscreen_store::Bilan> {
    avec_base(|b| b.bilan())
}

fn noter_app_courante() {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication();
    let courante = app
        .map(|a| {
            (
                a.bundleIdentifier().map(|s| s.to_string()),
                a.localizedName().map(|s| s.to_string()),
                Some(a.processIdentifier()),
            )
        })
        .unwrap_or((None, None, None));
    *verrou(&APP_COURANTE) = courante;
}

/// Les apps ouvertes, celles du Dock : (identifiant, nom), sans B-Screen, triées par nom.
pub fn apps_ouvertes() -> Vec<(String, String)> {
    objc2::rc::autoreleasepool(|_| {
        let mut apps: Vec<(String, String)> = NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter(|a| a.activationPolicy() == objc2_app_kit::NSApplicationActivationPolicy::Regular)
            .filter_map(|a| Some((a.bundleIdentifier()?.to_string(), a.localizedName()?.to_string())))
            .filter(|(identifiant, _)| identifiant != bscreen_core::IDENTIFIANT)
            .collect();
        apps.sort_by_key(|(_, nom)| nom.to_lowercase());
        apps.dedup_by(|a, b| a.0 == b.0);
        apps
    })
}

/// Suit l'app au premier plan et prévient la capture à chaque changement.
/// À appeler une fois, sur le fil principal.
pub fn surveiller_changements_d_app() {
    noter_app_courante();
    let centre = NSWorkspace::sharedWorkspace().notificationCenter();
    let quand_change = RcBlock::new(|_notification: NonNull<NSNotification>| {
        noter_app_courante();
        capture::declencher();
    });
    let observateur = unsafe {
        centre.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidActivateApplicationNotification),
            None,
            None,
            &quand_change,
        )
    };
    // L'observateur vit aussi longtemps que l'app.
    std::mem::forget(observateur);

    // Au réveil (du Mac, de l'écran, ou retour sur la session), le flux de macOS peut être mort.
    let au_reveil = RcBlock::new(|_notification: NonNull<NSNotification>| {
        let _ = std::thread::Builder::new().name("bscreen-reveil".into()).spawn(|| {
            std::thread::sleep(Duration::from_secs(3));
            if !verrou(&CAPTEUR).is_none() || !Reglages::charger().pause {
                relancer("réveil");
            }
        });
    });
    for nom in unsafe {
        [
            NSWorkspaceDidWakeNotification,
            NSWorkspaceScreensDidWakeNotification,
            NSWorkspaceSessionDidBecomeActiveNotification,
        ]
    } {
        let observateur =
            unsafe { centre.addObserverForName_object_queue_usingBlock(Some(nom), None, None, &au_reveil) };
        std::mem::forget(observateur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_libre_lue() {
        let libre = place_libre().expect("statfs");
        assert!(libre > 0);
    }

    #[test]
    fn ressources_mesurees() {
        let (cpu1, memoire) = ressources();
        let mut x = 0u64;
        for i in 0..5_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        let (cpu2, _) = ressources();
        assert!(cpu2 > cpu1, "le temps de processeur avance");
        assert!(memoire > 1_000_000, "mémoire résidente lue : {memoire}");
    }
}
