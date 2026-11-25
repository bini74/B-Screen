//! Indexation du texte (phase 3) : pour chaque image gardée, on lit le texte des
//! fenêtres de l'app au premier plan et on le range dans la base.
//!
//! Tout se passe sur un fil à part : la capture n'attend jamais. Si plusieurs
//! images attendent, seule la plus récente est lue (l'accessibilité donne l'état
//! d'« maintenant », pas celui d'il y a quelques secondes).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};

use bscreen_core::masquage::{masquer, masquer_url};
use bscreen_store::{NouvelleApparition, empreinte};
use objc2_core_foundation::CGRect;

use crate::moteur::avec_base;
use crate::texte;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayBounds(ecran: u32) -> CGRect;
}

/// Une image à indexer.
pub struct Travail {
    pub image: i64,
    pub ecran: u32,
    pub quand_ms: i64,
    /// Processus de l'app au premier plan au moment de l'image.
    pub pid: Option<i32>,
    /// L'app au premier plan est un navigateur (sa navigation privée est reconnue).
    pub navigateur: bool,
    /// L'image capturée, pour l'OCR de secours (relâchée aussitôt si l'accessibilité suffit).
    pub tampon: Option<crate::capture::TamponImage>,
    /// Prise tout de suite après un clic, une pause de frappe ou un changement d'app.
    pub declenchee: bool,
}

/// Dans la même app, le texte n'est relu qu'au plus une fois par cet intervalle (sauf après un
/// clic, une pause de frappe ou un changement d'app) : la lecture par l'accessibilité (≈ 900
/// éléments, 190 ms) était l'essentiel du processeur pris pendant qu'on se sert du Mac.
const ECART_LECTURES_MS: i64 = 4_000;

/// En dessous de ce nombre de caractères lus par l'accessibilité, on lit aussi l'image (OCR).
const SEUIL_OCR_CARACTERES: usize = 200;
/// Au plus un OCR de secours toutes les 5 s par écran.
const INTERVALLE_OCR_MS: i64 = 5_000;

pub static OCR_LECTURES: AtomicU64 = AtomicU64::new(0);
pub static OCR_DUREE_MS: AtomicU64 = AtomicU64::new(0);

/// Compteurs pour le journal.
pub static LECTURES: AtomicU64 = AtomicU64::new(0);
pub static DUREE_TOTALE_MS: AtomicU64 = AtomicU64::new(0);
pub static ELEMENTS: AtomicU64 = AtomicU64::new(0);
pub static NOUVELLES_APPARITIONS: AtomicU64 = AtomicU64::new(0);
/// Textes dont un secret a été masqué.
pub static MASQUES: AtomicU64 = AtomicU64::new(0);
/// Images prises en navigation privée (verrouillées, rien de lu n'est gardé).
pub static PRIVEES: AtomicU64 = AtomicU64::new(0);

/// Augmente à chaque fois que des textes quittent la base (effacement, purge, masquage) : les
/// numéros de textes et d'apparitions gardés par l'indexation ne valent plus (un texte effacé
/// qui revient à l'écran doit être recréé, sinon son apparition est refusée par la base).
pub static TEXTES_OUBLIES: AtomicU64 = AtomicU64::new(0);

/// Demande d'écrire tout de suite les heures de fin en attente (la frise s'ouvre : elle doit voir le
/// texte jusqu'à la dernière image).
static REPORTER_MAINTENANT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn reporter_maintenant() {
    REPORTER_MAINTENANT.store(true, Ordering::SeqCst);
}

/// L'heure (ms) où la lecture en cours a commencé ; 0 entre deux lectures.
static LECTURE_DEPUIS_MS: AtomicI64 = AtomicI64::new(0);
/// Une lecture qui dure plus que ça est bloquée. Le 24/11/2026, la lecture d'image d'Apple
/// (Vision) n'est jamais revenue d'une mise en veille : 31 heures sans texte, sans titres de
/// fenêtre, et surtout sans reconnaître la navigation privée (ni sélection de texte dans la frise,
/// Vision étant pris).
const LECTURE_BLOQUEE_MS: i64 = 90_000;

fn maintenant_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Le temps d'une lecture (remis à zéro à la fin, même par un `continue`).
struct EnLecture;

impl EnLecture {
    fn commencer() -> Self {
        LECTURE_DEPUIS_MS.store(maintenant_ms(), Ordering::SeqCst);
        EnLecture
    }
}

impl Drop for EnLecture {
    fn drop(&mut self) {
        LECTURE_DEPUIS_MS.store(0, Ordering::SeqCst);
    }
}

/// Chaque minute (l'app) : une lecture bloquée verrouille par précaution ce que les navigateurs ont
/// montré depuis (la navigation privée n'a pas pu être reconnue), puis B-Screen se relance.
pub fn surveiller() {
    let depuis = LECTURE_DEPUIS_MS.load(Ordering::SeqCst);
    let maintenant = maintenant_ms();
    if depuis == 0 || maintenant - depuis < LECTURE_BLOQUEE_MS {
        return;
    }
    tracing::error!(secondes = (maintenant - depuis) / 1000, "lecture du texte bloquée (Vision ne répond plus) : B-Screen se relance");
    let n = avec_base(|b| b.noter_privee_apps(depuis - 5_000, maintenant, texte::NAVIGATEURS)).unwrap_or(0);
    if n > 0 {
        tracing::warn!(images = n, "images de navigateur prises pendant le blocage : verrouillées par précaution");
    }
    crate::moteur::relancer_l_app();
}

/// Combien de temps un état privé (d'un navigateur, d'une adresse) est retenu.
const MEMOIRE_PRIVEE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// À appeler après tout ce qui retire des textes ou des apparitions de la base.
pub fn textes_oublies() {
    TEXTES_OUBLIES.fetch_add(1, Ordering::SeqCst);
}

static FILE: OnceLock<mpsc::SyncSender<Travail>> = OnceLock::new();

/// Confie une image à l'indexation (sans attendre). Si l'indexation est déjà occupée avec une image
/// en attente, celle-ci est ignorée : on ne garde jamais plusieurs images capturées en mémoire
/// (macOS n'en prête que quelques-unes ; les retenir arrêterait la capture).
pub fn indexer(travail: Travail) {
    let envoi = FILE.get_or_init(|| {
        let (envoi, reception) = mpsc::sync_channel(1);
        let _ = std::thread::Builder::new()
            .name("bscreen-texte".into())
            .spawn(move || boucle(reception));
        envoi
    });
    let _ = envoi.try_send(travail);
}

/// Ce qui identifie une apparition encore à l'écran (même texte, même place, même état).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Cle {
    texte: i64,
    vu: bool,
    focus: bool,
    cadre: [i32; 4],
}

fn arrondi(cadre: [f64; 4]) -> [i32; 4] {
    // Au pixel près à 4 points : un élément qui bouge d'un pixel reste la même apparition.
    cadre.map(|v| (v / 4.0).round() as i32 * 4)
}

fn boucle(reception: mpsc::Receiver<Travail>) {
    // Pour chaque écran : les apparitions encore à l'écran à la dernière lecture.
    let mut actives: HashMap<u32, HashMap<Cle, i64>> = HashMap::new();
    let mut ids_textes: HashMap<i64, i64> = HashMap::new();
    let mut dernier_ocr: HashMap<u32, i64> = HashMap::new();
    // Pour chaque écran : l'heure de la dernière image lue (les images sautées entre deux
    // lectures suivent le sort de la suivante si elle est privée).
    let mut derniere_lue: HashMap<u32, i64> = HashMap::new();
    let mut oublis_vus = TEXTES_OUBLIES.load(Ordering::SeqCst);
    // Navigation privée : le dernier état connu de chaque navigateur (processus), et les adresses
    // vues dans une fenêtre privée.
    let mut etat_prive: HashMap<i32, (bool, std::time::Instant)> = HashMap::new();
    let mut adresses_privees: HashMap<String, std::time::Instant> = HashMap::new();
    // Les textes toujours à l'écran : leur heure de fin n'est écrite qu'au plus toutes les 30 s (et
    // à la fin d'une période calme), pas à chaque image : 900 lignes et leur index réécrits toutes
    // les 2 s faisaient l'essentiel des écritures sur le disque (25/11/2026).
    let mut a_prolonger: HashMap<i64, i64> = HashMap::new();
    // Par écran : la dernière lecture (heure, processus, titre, adresse).
    #[allow(clippy::type_complexity)]
    let mut derniere_lecture: HashMap<u32, (i64, Option<i32>, Option<String>, Option<String>)> = HashMap::new();
    let mut dernier_report = std::time::Instant::now();
    let reporter = |a_prolonger: &mut HashMap<i64, i64>| {
        if a_prolonger.is_empty() {
            return;
        }
        let lot: Vec<(i64, i64)> = a_prolonger.drain().collect();
        avec_base(|b| b.prolonger_apparitions(&lot));
    };
    loop {
        let mut travail = match reception.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(t) => t,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Pas de nouvelle image : on écrit ce qui attend si la frise l'a demandé, ou au bout de 30 s.
                if REPORTER_MAINTENANT.swap(false, Ordering::SeqCst) || dernier_report.elapsed() >= std::time::Duration::from_secs(30) {
                    reporter(&mut a_prolonger);
                    dernier_report = std::time::Instant::now();
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                reporter(&mut a_prolonger);
                return;
            }
        };
        while let Ok(plus_recent) = reception.try_recv() {
            travail = plus_recent;
        }
        if REPORTER_MAINTENANT.swap(false, Ordering::SeqCst) || dernier_report.elapsed() >= std::time::Duration::from_secs(30) {
            reporter(&mut a_prolonger);
            dernier_report = std::time::Instant::now();
        }
        let _en_lecture = EnLecture::commencer();
        // Des textes ont quitté la base depuis la dernière image : ce qu'on en savait est périmé.
        let oublis = TEXTES_OUBLIES.load(Ordering::SeqCst);
        if oublis != oublis_vus {
            oublis_vus = oublis;
            ids_textes.clear();
            actives.clear();
        }
        // Même app, pas de clic ni de frappe, lecture récente : l'image garde le titre et l'adresse
        // de la dernière lecture ; son texte sera prolongé par la suivante.
        if let Some((quand, pid, fenetre, url)) = derniere_lecture.get(&travail.ecran)
            && *pid == travail.pid
            && !travail.declenchee
            && travail.quand_ms - quand < ECART_LECTURES_MS
        {
            avec_base(|b| b.noter_fenetre(travail.image, fenetre.as_deref(), url.as_deref()));
            continue;
        }
        let bornes = unsafe { CGDisplayBounds(travail.ecran) };
        let ecran = [bornes.origin.x, bornes.origin.y, bornes.size.width, bornes.size.height];
        let mut lecture = match travail.pid {
            Some(pid) => texte::lire_app(pid, ecran),
            None => texte::Lecture::default(),
        };
        // Sans lecture précédente (juste après le lancement), on ne remonte que de 5 s : jamais
        // « depuis toujours » (tout l'historique de l'écran serait verrouillé, titres effacés).
        let precedente = derniere_lue
            .insert(travail.ecran, travail.quand_ms)
            .unwrap_or(travail.quand_ms - 5_000)
            .max(travail.quand_ms - 60_000);
        // Navigation privée : l'image (et celles sautées depuis la dernière lecture) est verrouillée ;
        // ni texte, ni titre, ni adresse, ni recherche ne sont gardés.
        // Plein écran (une vidéo) : ni titre ni barre d'outils, donc ni l'insigne ni le titre
        // « Navigation privée » (25/11/2026 : une vidéo YouTube privée en plein écran se voyait).
        // Une fenêtre sans titre garde l'état de la lecture d'avant de la même app ; une adresse
        // vue dans une fenêtre privée reste privée.
        if travail.navigateur {
            let maintenant = std::time::Instant::now();
            let sans_titre = lecture.fenetre.as_deref().is_none_or(|t| t.trim().is_empty());
            if !lecture.privee {
                let par_adresse = lecture
                    .url
                    .as_ref()
                    .and_then(|u| adresses_privees.get(u))
                    .is_some_and(|t: &std::time::Instant| t.elapsed() < MEMOIRE_PRIVEE);
                let par_heritage = sans_titre
                    && travail
                        .pid
                        .and_then(|p| etat_prive.get(&p))
                        .is_some_and(|(prive, t): &(bool, std::time::Instant)| *prive && t.elapsed() < MEMOIRE_PRIVEE);
                if par_adresse || par_heritage {
                    lecture.privee = true;
                }
            }
            if let Some(pid) = travail.pid
                && (lecture.privee || !sans_titre)
            {
                etat_prive.insert(pid, (lecture.privee, maintenant));
            }
            if lecture.privee
                && let Some(u) = &lecture.url
            {
                adresses_privees.insert(u.clone(), maintenant);
            }
            adresses_privees.retain(|_, t| t.elapsed() < MEMOIRE_PRIVEE);
        }
        // Un site jamais enregistré (réglages) : comme la navigation privée.
        if travail.navigateur && !lecture.privee && lecture.url.as_deref().is_some_and(crate::vivants::site_exclu) {
            lecture.privee = true;
        }
        if travail.navigateur && lecture.privee {
            drop(travail.tampon.take());
            avec_base(|b| b.noter_privee(travail.ecran, precedente, travail.quand_ms));
            actives.remove(&travail.ecran);
            PRIVEES.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        // OCR de secours : l'app ne donne presque rien par l'accessibilité (jeu, bureau à distance, vidéo…).
        let caracteres: usize = lecture.elements.iter().map(|e| e.texte.len()).sum();
        let tampon = travail.tampon.take();
        if caracteres < SEUIL_OCR_CARACTERES
            && let Some(tampon) = tampon
            && travail.quand_ms - dernier_ocr.get(&travail.ecran).copied().unwrap_or(0) >= INTERVALLE_OCR_MS
        {
            dernier_ocr.insert(travail.ecran, travail.quand_ms);
            let debut = std::time::Instant::now();
            if let Ok(lignes) = crate::ocr::lire_tampon(tampon.pixels()) {
                for (texte_ligne, c) in lignes {
                    lecture.elements.push(texte::ElementTexte {
                        texte: texte_ligne,
                        role: "OCR".into(),
                        cadre: [
                            ecran[0] + c[0] * ecran[2],
                            ecran[1] + c[1] * ecran[3],
                            c[2] * ecran[2],
                            c[3] * ecran[3],
                        ],
                        vu: true,
                        focus: false,
                    });
                }
            }
            OCR_LECTURES.fetch_add(1, Ordering::Relaxed);
            OCR_DUREE_MS.fetch_add(debut.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
        LECTURES.fetch_add(1, Ordering::Relaxed);
        DUREE_TOTALE_MS.fetch_add(lecture.duree.as_millis() as u64, Ordering::Relaxed);
        ELEMENTS.fetch_add(lecture.elements.len() as u64, Ordering::Relaxed);

        // Les secrets (cartes, clés, mots de passe affichés…) sont masqués avant tout stockage.
        let fenetre = lecture.fenetre.as_deref().map(masquer);
        let url = lecture.url.as_deref().map(masquer_url);
        avec_base(|b| b.noter_fenetre(travail.image, fenetre.as_deref(), url.as_deref()));
        derniere_lecture.insert(
            travail.ecran,
            (travail.quand_ms, travail.pid, fenetre.as_deref().map(str::to_string), url.as_deref().map(str::to_string)),
        );
        if let Some(url) = url.as_deref()
            && let Some((moteur, requete)) = texte::recherche_dans_url(url)
        {
            avec_base(|b| b.noter_recherche(travail.quand_ms, &moteur, &masquer(&requete), url));
        }
        for element in &mut lecture.elements {
            if let std::borrow::Cow::Owned(masque) = masquer(&element.texte) {
                element.texte = masque;
                MASQUES.fetch_add(1, Ordering::Relaxed);
            }
        }

        if ids_textes.len() > 200_000 {
            ids_textes.clear();
        }
        let anciennes = actives.remove(&travail.ecran).unwrap_or_default();
        let mut encore: HashMap<Cle, i64> = HashMap::new();
        let mut a_creer: Vec<(Cle, NouvelleApparition)> = Vec::new();
        let mut prolongees: Vec<i64> = Vec::new();
        let garder_cache = crate::vivants::texte_cache();
        for element in &lecture.elements {
            // Le texte jamais visible (hors de l'écran, fenêtres derrière) n'est gardé que sur demande.
            if !element.vu && !garder_cache {
                continue;
            }
            let cle_texte = empreinte(&element.texte);
            let texte_id = match ids_textes.get(&cle_texte) {
                Some(id) => *id,
                None => match avec_base(|b| b.texte_id(&element.texte)) {
                    Some(id) => {
                        ids_textes.insert(cle_texte, id);
                        id
                    }
                    None => continue,
                },
            };
            let cle = Cle {
                texte: texte_id,
                vu: element.vu,
                focus: element.focus,
                cadre: arrondi(element.cadre),
            };
            if encore.contains_key(&cle) || a_creer.iter().any(|(c, _)| *c == cle) {
                continue;
            }
            match anciennes.get(&cle) {
                Some(&id) => {
                    prolongees.push(id);
                    encore.insert(cle, id);
                }
                None => a_creer.push((
                    cle,
                    NouvelleApparition {
                        texte: texte_id,
                        ecran: travail.ecran,
                        quand_ms: travail.quand_ms,
                        source: u8::from(element.role == "OCR"),
                        vu: element.vu,
                        focus: element.focus,
                        cadre: element.cadre,
                    },
                )),
            }
        }
        let nouvelles: Vec<NouvelleApparition> = a_creer.iter().map(|(_, n)| n.clone()).collect();
        for id in prolongees.drain(..) {
            a_prolonger.insert(id, travail.quand_ms);
        }
        let Some(ids) = avec_base(|b| b.noter_apparitions(&nouvelles, &prolongees, travail.quand_ms)) else {
            // Refusé (un texte effacé entre-temps) : on repart de zéro à l'image suivante.
            ids_textes.clear();
            actives.remove(&travail.ecran);
            continue;
        };
        NOUVELLES_APPARITIONS.fetch_add(ids.len() as u64, Ordering::Relaxed);
        for ((cle, _), id) in a_creer.into_iter().zip(ids) {
            encore.insert(cle, id);
        }
        actives.insert(travail.ecran, encore);
    }
}
