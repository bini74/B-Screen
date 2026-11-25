//! Les appels sur le Mac : une autre app se sert du micro (FaceTime, WhatsApp, Meet dans Safari…).
//! Pendant un appel, le son du Mac est la personne au bout du fil ; le reste du temps, c'est une
//! vidéo, dont les voix ne sont pas des personnes à reconnaître (946 voix venues de vidéos le 22/11).
//!
//! macOS dit quels processus se servent du micro (les objets « processus » de Core Audio,
//! macOS 14 et plus). Toujours là sans être des appels : Siri qui écoute (« Dis Siri »), la dictée,
//! et replayd, qui enregistre le micro pour B-Screen lui-même.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Un relevé toutes les 2 s ; on garde les 15 dernières minutes (un passage est traité bien avant).
const INTERVALLE: Duration = Duration::from_secs(2);
const GARDE_MS: i64 = 15 * 60_000;
/// Un passage est « pendant un appel » si un relevé d'appel tombe à moins de 3 s de lui.
const TOLERANCE_MS: i64 = 3_000;

/// Les processus qui écoutent le micro sans être des appels (préfixes d'identifiant).
const PAS_DES_APPELS: [&str; 6] = [
    "com.apple.CoreSpeech",
    "com.apple.corespeech",
    "com.apple.siri",
    "com.apple.SpeechRecognitionCore",
    "com.apple.replayd",
    "com.julienbonzi.bscreen",
];

/// Les derniers relevés : (heure en ms, une app était en appel).
static RELEVES: Mutex<VecDeque<(i64, bool)>> = Mutex::new(VecDeque::new());

#[repr(C)]
struct Adresse {
    selecteur: u32,
    portee: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        objet: u32,
        adresse: *const Adresse,
        taille_qualificatif: u32,
        qualificatif: *const c_void,
        taille: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        objet: u32,
        adresse: *const Adresse,
        taille_qualificatif: u32,
        qualificatif: *const c_void,
        taille: *mut u32,
        donnees: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(s: *const c_void, buf: *mut u8, taille: isize, codage: u32) -> bool;
    fn CFRelease(p: *const c_void);
}

const fn code(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}
/// kAudioObjectSystemObject.
const SYSTEME: u32 = 1;
const GLOBAL: u32 = code(b"glob");
const LISTE_DES_PROCESSUS: u32 = code(b"prs#");
const IDENTIFIANT: u32 = code(b"pbid");
const PID: u32 = code(b"ppid");
const ENTREE_EN_MARCHE: u32 = code(b"piri");
const UTF8: u32 = 0x0800_0100;

fn adresse(selecteur: u32) -> Adresse {
    Adresse {
        selecteur,
        portee: GLOBAL,
        element: 0,
    }
}

/// Une propriété de taille fixe (un entier) d'un objet Core Audio.
fn entier(objet: u32, selecteur: u32) -> Option<u32> {
    let mut valeur = 0u32;
    let mut taille = size_of::<u32>() as u32;
    // SAFETY : propriété documentée, de 4 octets, écrite dans `valeur`.
    let statut = unsafe {
        AudioObjectGetPropertyData(
            objet,
            &adresse(selecteur),
            0,
            std::ptr::null(),
            &mut taille,
            (&raw mut valeur).cast(),
        )
    };
    (statut == 0).then_some(valeur)
}

/// L'identifiant (bundle) d'un processus audio.
fn identifiant(objet: u32) -> Option<String> {
    let mut chaine: *const c_void = std::ptr::null();
    let mut taille = size_of::<*const c_void>() as u32;
    // SAFETY : la propriété rend une CFString à nous (libérée ci-dessous).
    unsafe {
        let statut = AudioObjectGetPropertyData(
            objet,
            &adresse(IDENTIFIANT),
            0,
            std::ptr::null(),
            &mut taille,
            (&raw mut chaine).cast(),
        );
        if statut != 0 || chaine.is_null() {
            return None;
        }
        let mut tampon = [0u8; 256];
        let ok = CFStringGetCString(chaine, tampon.as_mut_ptr(), tampon.len() as isize, UTF8);
        CFRelease(chaine);
        if !ok {
            return None;
        }
        let fin = tampon.iter().position(|&o| o == 0).unwrap_or(tampon.len());
        Some(String::from_utf8_lossy(&tampon[..fin]).into_owned())
    }
}

/// Les processus qui se servent du micro en ce moment (identifiant, ou « pid N » sans identifiant).
pub fn apps_au_micro() -> Vec<String> {
    let mut taille = 0u32;
    // SAFETY : la liste des processus audio (macOS 14+) ; rien n'est écrit avant d'en avoir la taille.
    let statut =
        unsafe { AudioObjectGetPropertyDataSize(SYSTEME, &adresse(LISTE_DES_PROCESSUS), 0, std::ptr::null(), &mut taille) };
    if statut != 0 || taille == 0 {
        return Vec::new();
    }
    let mut objets = vec![0u32; taille as usize / size_of::<u32>()];
    // SAFETY : le tableau a la taille demandée par Core Audio.
    let statut = unsafe {
        AudioObjectGetPropertyData(
            SYSTEME,
            &adresse(LISTE_DES_PROCESSUS),
            0,
            std::ptr::null(),
            &mut taille,
            objets.as_mut_ptr().cast(),
        )
    };
    if statut != 0 {
        return Vec::new();
    }
    objets.truncate(taille as usize / size_of::<u32>());
    objets
        .into_iter()
        .filter(|&o| entier(o, ENTREE_EN_MARCHE) == Some(1))
        .filter(|&o| entier(o, PID) != Some(std::process::id()))
        .map(|o| identifiant(o).unwrap_or_else(|| format!("pid {}", entier(o, PID).unwrap_or(0))))
        .collect()
}

/// L'app en appel : une app qui se sert du micro, hors Siri, dictée et B-Screen.
pub fn app_en_appel() -> Option<String> {
    apps_au_micro()
        .into_iter()
        .find(|app| !PAS_DES_APPELS.iter().any(|p| app.starts_with(p)))
}

fn maintenant_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Relève toutes les 2 s si une app est en appel (fil à part, priorité « utilitaire »).
pub fn demarrer() {
    let _ = std::thread::Builder::new().name("bscreen-appels".into()).spawn(|| {
        // SAFETY : change seulement la priorité du fil courant.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
        }
        let mut en_cours: Option<String> = None;
        loop {
            let appel = app_en_appel();
            if appel != en_cours {
                match &appel {
                    Some(app) => tracing::info!(%app, "appel en cours (une app se sert du micro)"),
                    None => tracing::info!("fin de l'appel"),
                }
                en_cours = appel.clone();
            }
            let quand = maintenant_ms();
            if let Ok(mut releves) = RELEVES.lock() {
                releves.push_back((quand, appel.is_some()));
                while releves.front().is_some_and(|(t, _)| *t < quand - GARDE_MS) {
                    releves.pop_front();
                }
            }
            std::thread::sleep(INTERVALLE);
        }
    });
}

/// Une app était-elle en appel pendant cette plage (à 3 s près) ?
pub fn pendant(debut_ms: i64, fin_ms: i64) -> bool {
    RELEVES.lock().is_ok_and(|releves| {
        releves
            .iter()
            .any(|(t, appel)| *appel && *t >= debut_ms - TOLERANCE_MS && *t <= fin_ms + TOLERANCE_MS)
    })
}
