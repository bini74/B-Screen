//! Audio (phase 5) : le micro et le son du Mac, reçus de ScreenCaptureKit, découpés en
//! passages de voix (on n'enregistre que quand ça parle), écrits en AAC (flux ADTS) avec l'heure exacte.
//!
//! Détection de voix (première version) : énergie de chaque trame de 30 ms comparée à un
//! plancher de bruit qui s'adapte ; 0,3 s gardées avant le début ; fin après 0,75 s de silence ;
//! passages de 30 s au plus (pour la transcription).
//!
//! L'heure de chaque passage vient de l'horodatage des tampons (horloge de la machine),
//! pas de l'heure de réception.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow};
use bscreen_core::chemins;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_avf_audio::{
    AVAudioFile, AVAudioFormat, AVAudioPCMBuffer, AVEncoderBitRateKey, AVFormatIDKey, AVNumberOfChannelsKey,
    AVSampleRateKey,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSNumber, NSString, NSURL};

use crate::encodage::ms_depuis_1970;
use crate::moteur::avec_base;

/// Taux d'échantillonnage gardé (suffisant pour la voix et la transcription).
pub const TAUX: u32 = 16_000;
const TRAME: usize = 480; // 30 ms
const AVANT: usize = 4_800; // 0,3 s gardées avant le début de la parole
const TRAMES_POUR_COMMENCER: u32 = 3;
const TRAMES_DE_SILENCE_POUR_FINIR: u32 = 25; // 0,75 s
const DUREE_MIN: usize = 9_600; // 0,6 s
const DUREE_MAX: usize = 480_000; // 30 s
/// La parole doit dépasser le bruit de fond d'au moins 12 dB, et ne jamais être sous −50 dBFS.
const MARGE_DB: f32 = 12.0;
const PLANCHER_ABSOLU_DB: f32 = -50.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAudio {
    /// Ce que Julien dit (micro).
    Micro = 0,
    /// Ce qui sort du Mac (appels, vidéos…).
    Systeme = 1,
}

impl SourceAudio {
    fn nom(self) -> &'static str {
        match self {
            SourceAudio::Micro => "micro",
            SourceAudio::Systeme => "systeme",
        }
    }
}

/// Compteurs pour le journal.
pub static TAMPONS: AtomicU64 = AtomicU64::new(0);
pub static TAMPONS_ILLISIBLES: AtomicU64 = AtomicU64::new(0);
pub static SEGMENTS: AtomicU64 = AtomicU64::new(0);
/// Passages écartés par le classifieur (musique, chant, bruits) : jamais écrits.
pub static ECARTES: AtomicU64 = AtomicU64::new(0);
pub static MS_DE_VOIX: AtomicU64 = AtomicU64::new(0);

/// Ramène n'importe quel taux à 16 kHz en moyennant les échantillons de chaque intervalle
/// (filtre anti-repliement simple), en gardant son état d'un tampon à l'autre.
#[derive(Debug, Default)]
struct Reechantillonneur {
    taux: f64,
    somme: f32,
    compte: u32,
    reste: f64,
}

impl Reechantillonneur {
    fn pousser(&mut self, taux: f64, entree: &[f32], sortie: &mut Vec<f32>) {
        // Un taux absurde (0, infini…) ferait tourner la boucle sans fin : on l'ignore.
        if !taux.is_finite() || !(1_000.0..=1_000_000.0).contains(&taux) {
            return;
        }
        let pas = taux / f64::from(TAUX);
        if (taux - self.taux).abs() > 0.5 {
            *self = Self {
                taux,
                reste: if pas > 1.0 { pas } else { 0.0 },
                ..Self::default()
            };
        }
        if (pas - 1.0).abs() < 1e-9 {
            sortie.extend_from_slice(entree);
            return;
        }
        for &x in entree {
            if pas > 1.0 {
                self.somme += x;
                self.compte += 1;
                self.reste -= 1.0;
                if self.reste <= 1e-9 {
                    sortie.push(self.somme / self.compte as f32);
                    self.somme = 0.0;
                    self.compte = 0;
                    self.reste += pas;
                }
            } else {
                // Moins de 16 kHz (rare) : on répète les échantillons.
                self.reste += 1.0 / pas;
                while self.reste >= 1.0 {
                    sortie.push(x);
                    self.reste -= 1.0;
                }
            }
        }
    }
}

struct EtatSource {
    reechantillonneur: Reechantillonneur,
    /// Échantillons à 16 kHz pas encore analysés (moins d'une trame).
    reste: Vec<f32>,
    avant: VecDeque<f32>,
    segment: Vec<f32>,
    debut: Option<SystemTime>,
    trames_de_parole: u32,
    trames_de_silence: u32,
    plancher_db: f32,
}

impl EtatSource {
    fn nouveau() -> Self {
        Self {
            reechantillonneur: Reechantillonneur::default(),
            reste: Vec::new(),
            avant: VecDeque::with_capacity(AVANT + TRAME),
            segment: Vec::new(),
            debut: None,
            trames_de_parole: 0,
            trames_de_silence: 0,
            plancher_db: -60.0,
        }
    }
}

static ETATS: OnceLock<[Mutex<EtatSource>; 2]> = OnceLock::new();

// ---------- écho : le micro qui entend les haut-parleurs du Mac ----------

/// Le niveau (dB) de chaque trame de 30 ms du son du Mac, avec l'heure de sa fin (ms), sur 40 s.
static ENVELOPPE_MAC: Mutex<VecDeque<(i64, f32)>> = Mutex::new(VecDeque::new());
const ENVELOPPE_DUREE_MS: i64 = 40_000;
/// Retard du micro sur le son du Mac pris en compte : jusqu'à 300 ms (10 trames).
const RETARD_MAX_TRAMES: usize = 10;
/// Corrélation des enveloppes au-delà de laquelle le passage du micro n'est qu'un écho.
const SEUIL_ECHO: f32 = 0.6;

pub static ECHOS: AtomicU64 = AtomicU64::new(0);

fn noter_enveloppe_mac(fin_trame: SystemTime, db: f32) {
    let ms = ms_depuis_1970(fin_trame);
    let mut enveloppe = ENVELOPPE_MAC.lock().unwrap_or_else(|e| e.into_inner());
    enveloppe.push_back((ms, db));
    while enveloppe.front().is_some_and(|&(t, _)| t < ms - ENVELOPPE_DUREE_MS) {
        enveloppe.pop_front();
    }
}

/// Corrélation (Pearson) de deux suites de même longueur ; 0 si l'une est plate.
fn correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n < 8 {
        return 0.0;
    }
    let (ma, mb) = (a[..n].iter().sum::<f32>() / n as f32, b[..n].iter().sum::<f32>() / n as f32);
    let (mut ab, mut aa, mut bb) = (0f32, 0f32, 0f32);
    for (x, y) in a[..n].iter().zip(&b[..n]) {
        let (dx, dy) = (x - ma, y - mb);
        ab += dx * dy;
        aa += dx * dx;
        bb += dy * dy;
    }
    if aa < 1e-3 || bb < 1e-3 { 0.0 } else { ab / (aa * bb).sqrt() }
}

/// Le passage du micro suit-il le son du Mac (même enveloppe, un peu en retard) ?
/// `mac` : le niveau du Mac à la fin de chaque trame, `micro` : celui du passage, trame par trame.
fn ressemble_au_mac(micro: &[f32], mac: &[Option<f32>]) -> bool {
    (0..=RETARD_MAX_TRAMES).any(|retard| {
        // La trame k du micro est comparée à la trame k - retard du Mac.
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for (k, &m) in micro.iter().enumerate().skip(retard) {
            if let Some(x) = mac.get(k - retard).copied().flatten() {
                a.push(m);
                b.push(x);
            }
        }
        // Le Mac doit avoir vraiment fait du son (pas seulement du silence).
        let actif = b.iter().filter(|&&x| x > PLANCHER_ABSOLU_DB).count();
        actif * 3 >= b.len().max(1) && correlation(&a, &b) >= SEUIL_ECHO
    })
}

/// Un passage du micro n'est-il qu'un écho du son du Mac ?
fn est_un_echo(segment: &Segment) -> bool {
    let debut = ms_depuis_1970(segment.debut);
    let micro: Vec<f32> = segment.echantillons.chunks_exact(TRAME).map(niveau_db).collect();
    // Le niveau du Mac à la fin de chaque trame du passage (à 15 ms près).
    let enveloppe = ENVELOPPE_MAC.lock().unwrap_or_else(|e| e.into_inner());
    if enveloppe.is_empty() {
        return false;
    }
    let mac: Vec<Option<f32>> = (0..micro.len())
        .map(|k| {
            let t = debut + (k as i64 + 1) * 30;
            let i = enveloppe.partition_point(|&(fin, _)| fin < t - 15);
            enveloppe.get(i).filter(|&&(fin, _)| fin <= t + 15).map(|&(_, db)| db)
        })
        .collect();
    drop(enveloppe);
    ressemble_au_mac(&micro, &mac)
}

fn etat(source: SourceAudio) -> &'static Mutex<EtatSource> {
    &ETATS.get_or_init(|| [Mutex::new(EtatSource::nouveau()), Mutex::new(EtatSource::nouveau())])[source as usize]
}

// ---------- échantillons PCM d'un CMSampleBuffer ----------

#[repr(C)]
struct AudioBuffer {
    canaux: u32,
    octets: u32,
    donnees: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    nombre: u32,
    tampons: [AudioBuffer; 8],
}

#[repr(C)]
struct DescriptionFlux {
    taux: f64,
    format: u32,
    drapeaux: u32,
    octets_par_paquet: u32,
    trames_par_paquet: u32,
    octets_par_trame: u32,
    canaux: u32,
    bits_par_canal: u32,
    reserve: u32,
}

/// Même disposition que CMTime.
#[repr(C)]
#[derive(Clone, Copy)]
struct Temps {
    valeur: i64,
    echelle: i32,
    drapeaux: u32,
    epoque: i64,
}

impl Temps {
    fn secondes(self) -> Option<f64> {
        // kCMTimeFlags_Valid = 1
        (self.drapeaux & 1 == 1 && self.echelle > 0).then(|| self.valeur as f64 / f64::from(self.echelle))
    }
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        echantillon: *const c_void,
        taille_requise: *mut usize,
        liste: *mut AudioBufferList,
        taille_liste: usize,
        allocateur_structure: *const c_void,
        allocateur_bloc: *const c_void,
        drapeaux: u32,
        bloc: *mut *const c_void,
    ) -> i32;
    fn CMSampleBufferGetFormatDescription(echantillon: *const c_void) -> *const c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(description: *const c_void) -> *const DescriptionFlux;
    fn CMSampleBufferGetPresentationTimeStamp(echantillon: *const c_void) -> Temps;
    fn CMClockGetHostTimeClock() -> *const c_void;
    fn CMClockGetTime(horloge: *const c_void) -> Temps;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(objet: *const c_void);
}

const FLOTTANT: u32 = 1;
const ENTIER_SIGNE: u32 = 1 << 2;
const NON_ENTRELACE: u32 = 1 << 5;

/// Lit des échantillons (flottant 32 bits, entier 16 ou 32 bits) en flottants de −1 à 1.
fn lire_valeurs(octets: &[u8], drapeaux: u32, bits: u32) -> Option<Vec<f32>> {
    match (drapeaux & FLOTTANT != 0, bits) {
        (true, 32) => Some(
            octets
                .chunks_exact(4)
                .map(|o| f32::from_ne_bytes([o[0], o[1], o[2], o[3]]))
                .collect(),
        ),
        (false, 16) if drapeaux & ENTIER_SIGNE != 0 => Some(
            octets
                .chunks_exact(2)
                .map(|o| f32::from(i16::from_ne_bytes([o[0], o[1]])) / 32_768.0)
                .collect(),
        ),
        (false, 32) if drapeaux & ENTIER_SIGNE != 0 => Some(
            octets
                .chunks_exact(4)
                .map(|o| i32::from_ne_bytes([o[0], o[1], o[2], o[3]]) as f32 / 2_147_483_648.0)
                .collect(),
        ),
        _ => None,
    }
}

/// Les échantillons d'un tampon audio, en mono, avec leur taux ; sinon pourquoi il est illisible.
fn pcm_mono(echantillon: &CMSampleBuffer) -> Result<(f64, Vec<f32>), String> {
    let brut = (echantillon as *const CMSampleBuffer).cast::<c_void>();
    // SAFETY : `brut` est un CMSampleBuffer valide pendant tout l'appel ; la liste a la place de
    // 8 tampons (la fonction échoue s'il en faut plus) ; le bloc retenu est relâché à la fin.
    unsafe {
        let description = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(brut));
        let description = description.as_ref().ok_or("pas de description de format")?;
        let format = || {
            format!(
                "format {:?}, drapeaux {:#x}, {} bits, {} canaux, {} Hz, {} octets/trame",
                String::from_utf8_lossy(&description.format.to_be_bytes()),
                description.drapeaux,
                description.bits_par_canal,
                description.canaux,
                description.taux,
                description.octets_par_trame
            )
        };
        // D'abord la taille exacte de la liste de tampons, puis la liste elle-même
        // (une taille plus grande que nécessaire est refusée : -12737).
        let mut taille: usize = 0;
        let statut = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            brut,
            &mut taille,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            1,
            std::ptr::null_mut(),
        );
        if statut != 0 || taille == 0 || taille > std::mem::size_of::<AudioBufferList>() {
            return Err(format!("CoreMedia {statut}, taille {taille} ({})", format()));
        }
        let mut liste: AudioBufferList = std::mem::zeroed();
        let mut bloc: *const c_void = std::ptr::null();
        let statut = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            brut,
            std::ptr::null_mut(),
            &mut liste,
            taille,
            std::ptr::null(),
            std::ptr::null(),
            1,
            &mut bloc,
        );
        if statut != 0 {
            return Err(format!("CoreMedia {statut}, taille {taille} ({})", format()));
        }
        let nombre = (liste.nombre as usize).min(liste.tampons.len());
        let mut mono: Option<Vec<f32>> = None;
        let mut tampons_lus = 0usize;
        let mut refus = None;
        for tampon in &liste.tampons[..nombre] {
            if tampon.donnees.is_null() {
                continue;
            }
            let octets = std::slice::from_raw_parts(tampon.donnees.cast::<u8>(), tampon.octets as usize);
            let Some(valeurs) = lire_valeurs(octets, description.drapeaux, description.bits_par_canal) else {
                refus = Some(format!("échantillons illisibles ({})", format()));
                break;
            };
            // Canaux entrelacés dans ce tampon : moyenne de chaque trame.
            let canaux = if description.drapeaux & NON_ENTRELACE != 0 {
                1
            } else {
                tampon.canaux.max(1) as usize
            };
            let valeurs: Vec<f32> = if canaux > 1 {
                valeurs
                    .chunks_exact(canaux)
                    .map(|t| t.iter().sum::<f32>() / canaux as f32)
                    .collect()
            } else {
                valeurs
            };
            tampons_lus += 1;
            match mono.as_mut() {
                None => mono = Some(valeurs),
                Some(m) => m.iter_mut().zip(&valeurs).for_each(|(a, b)| *a += b),
            }
        }
        if !bloc.is_null() {
            CFRelease(bloc);
        }
        let mut mono = match (mono, refus) {
            (Some(m), _) => m,
            (None, Some(r)) => return Err(r),
            (None, None) => return Err(format!("aucune donnée ({nombre} tampon(s), {})", format())),
        };
        if tampons_lus > 1 {
            mono.iter_mut().for_each(|m| *m /= tampons_lus as f32);
        }
        Ok((description.taux, mono))
    }
}

/// L'heure (murale) du premier échantillon d'un tampon, d'après son horodatage.
fn heure_du_tampon(echantillon: &CMSampleBuffer) -> SystemTime {
    let maintenant = SystemTime::now();
    let brut = (echantillon as *const CMSampleBuffer).cast::<c_void>();
    // SAFETY : lectures sur un tampon valide et sur l'horloge de la machine.
    let (pts, horloge) = unsafe {
        (
            CMSampleBufferGetPresentationTimeStamp(brut).secondes(),
            CMClockGetTime(CMClockGetHostTimeClock()).secondes(),
        )
    };
    match (pts, horloge) {
        (Some(pts), Some(horloge)) if (0.0..10.0).contains(&(horloge - pts)) => {
            maintenant - Duration::from_secs_f64(horloge - pts)
        }
        _ => maintenant,
    }
}

fn niveau_db(trame: &[f32]) -> f32 {
    let energie = trame.iter().map(|v| v * v).sum::<f32>() / trame.len().max(1) as f32;
    10.0 * (energie + 1e-12).log10()
}

/// Un passage de voix fini, à écrire.
struct Segment {
    source: SourceAudio,
    debut: SystemTime,
    echantillons: Vec<f32>,
}

static ECRITURE: OnceLock<mpsc::SyncSender<Segment>> = OnceLock::new();

fn ecrire_plus_tard(segment: Segment) {
    let envoi = ECRITURE.get_or_init(|| {
        // Au plus 8 passages en attente (≤ 15 Mo) : si l'écriture bloque, on perd la voix, pas la mémoire.
        let (envoi, reception) = mpsc::sync_channel::<Segment>(8);
        let _ = std::thread::Builder::new()
            .name("bscreen-audio".into())
            .spawn(move || {
                // Empreintes de voix : les modèles se chargent une fois, sur ce fil (~1 s).
                let empreinteur = match crate::empreintes::dossier_modeles() {
                    Some(dossier) => match crate::empreintes::Empreinteur::charger(&dossier) {
                        Ok(e) => Some(e),
                        Err(e) => {
                            tracing::warn!(%e, "empreintes de voix indisponibles");
                            None
                        }
                    },
                    None => {
                        tracing::warn!("modèles d'empreintes de voix introuvables");
                        None
                    }
                };
                while let Ok(segment) = reception.recv() {
                    // Une panique sur un passage ne doit pas arrêter l'écriture des suivants.
                    let traite = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        traiter_passage(&segment, empreinteur.as_ref());
                    }));
                    if traite.is_err() {
                        tracing::error!("passage de voix : panique rattrapée");
                    }
                }
            });
        envoi
    });
    let _ = envoi.try_send(segment);
}

/// Un passage fini : écho ? parole ? empreinte de voix, puis écriture.
/// Avec son bassin de libération (SoundAnalysis, Core ML, AVAudioFile) : le fil de l'audio dure
/// autant que l'app.
fn traiter_passage(segment: &Segment, empreinteur: Option<&crate::empreintes::Empreinteur>) {
    objc2::rc::autoreleasepool(|_| traiter_passage_seul(segment, empreinteur));
}

fn traiter_passage_seul(segment: &Segment, empreinteur: Option<&crate::empreintes::Empreinteur>) {
    // Le micro n'a fait qu'entendre les haut-parleurs : le son du Mac a déjà la voix, en propre.
    if segment.source == SourceAudio::Micro && est_un_echo(segment) {
        ECHOS.fetch_add(1, Ordering::Relaxed);
        tracing::debug!("passage du micro écarté : écho du Mac");
        return;
    }
    // Musique, chant, bruit : pas une voix, rien n'est écrit.
    match crate::classement::ecouter(&segment.echantillons) {
        Ok(ecoute) if !ecoute.est_de_la_parole() => {
            ECARTES.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(?ecoute, source = segment.source.nom(), "passage écarté");
            return;
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(%e, "classement du son impossible : passage gardé"),
    }
    // Qui parle ? Empreinte de la voix, rattachée à une voix connue (ou nouvelle).
    let empreinte = empreinteur.and_then(|e| match e.empreinte(&segment.echantillons) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!(%e, "pas d'empreinte pour ce passage");
            None
        }
    });
    // Le son du Mac n'a de voix que pendant un appel (une autre app se sert du micro) ; le reste
    // du temps, c'est une vidéo.
    use bscreen_core::voix_familieres::Origine;
    let debut_ms = ms_depuis_1970(segment.debut);
    let fin_ms = debut_ms + segment.echantillons.len() as i64 * 1000 / i64::from(TAUX);
    let origine = match segment.source {
        SourceAudio::Micro => Origine::Micro,
        _ if crate::appels::pendant(debut_ms, fin_ms) => Origine::Appel,
        _ => Origine::Video,
    };
    // Les appels ne sont pas enregistrés (réglages) : ni ta voix, ni celle d'en face.
    if !crate::vivants::voix_appels() && crate::appels::pendant(debut_ms, fin_ms) {
        tracing::debug!("passage pendant un appel : pas enregistré (réglages)");
        return;
    }
    let locuteur = empreinte
        .as_deref()
        .and_then(|v| crate::empreintes::rattacher(v, origine));
    if let Err(e) = enregistrer_segment(segment, empreinte.as_deref(), locuteur, origine == Origine::Appel) {
        tracing::warn!(%e, "passage de voix non enregistré");
    }
}

/// Un tampon reçu de ScreenCaptureKit (micro ou son du Mac). Appelé par macOS : une panique ici
/// arrêterait toute l'app, elle est donc rattrapée (le tampon est perdu, pas l'enregistrement).
pub fn recevoir(source: SourceAudio, echantillon: &CMSampleBuffer) {
    let resultat = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recevoir_tampon(source, echantillon)));
    if resultat.is_err() {
        tracing::error!(source = source.nom(), "tampon audio : panique rattrapée");
    }
}

fn recevoir_tampon(source: SourceAudio, echantillon: &CMSampleBuffer) {
    TAMPONS.fetch_add(1, Ordering::Relaxed);
    let heure = heure_du_tampon(echantillon);
    let (taux, mono) = match pcm_mono(echantillon) {
        Ok(pcm) => pcm,
        Err(raison) => {
            TAMPONS_ILLISIBLES.fetch_add(1, Ordering::Relaxed);
            // Chaque raison une seule fois dans le journal.
            static VUES: Mutex<Vec<String>> = Mutex::new(Vec::new());
            let mut vues = VUES.lock().unwrap_or_else(|e| e.into_inner());
            if vues.len() < 16 && !vues.contains(&raison) {
                tracing::warn!(source = source.nom(), raison, "tampon audio illisible");
                vues.push(raison);
            }
            return;
        }
    };
    let mut finis = Vec::new();
    {
        let mut etat = etat(source).lock().unwrap_or_else(|e| e.into_inner());
        let etat = &mut *etat;
        // Les échantillons restés du tampon précédent précèdent immédiatement celui-ci.
        let debut_reste = heure - Duration::from_secs_f64(etat.reste.len() as f64 / f64::from(TAUX));
        let mut reste = std::mem::take(&mut etat.reste);
        etat.reechantillonneur.pousser(taux, &mono, &mut reste);
        let complets = reste.len() / TRAME * TRAME;
        for (i, trame) in reste[..complets].chunks_exact(TRAME).enumerate() {
            let fin_trame = debut_reste + Duration::from_secs_f64(((i + 1) * TRAME) as f64 / f64::from(TAUX));
            if let Some(segment) = analyser(etat, source, trame, fin_trame) {
                finis.push(segment);
            }
        }
        reste.drain(..complets);
        etat.reste = reste;
    }
    for segment in finis {
        ecrire_plus_tard(segment);
    }
}

/// La capture s'arrête (pause, veille, relance) : les passages en cours sont écrits tels quels
/// et l'analyse repart de zéro, pour ne jamais coller deux moments séparés par un arrêt.
pub fn interrompre() {
    for source in [SourceAudio::Micro, SourceAudio::Systeme] {
        let fini = {
            let mut etat = etat(source).lock().unwrap_or_else(|e| e.into_inner());
            let fini = etat.debut.take().and_then(|debut| {
                let echantillons = std::mem::take(&mut etat.segment);
                (echantillons.len() >= DUREE_MIN).then_some(Segment {
                    source,
                    debut,
                    echantillons,
                })
            });
            *etat = EtatSource::nouveau();
            fini
        };
        if let Some(segment) = fini {
            ecrire_plus_tard(segment);
        }
    }
}

/// Un effacement est demandé : les passages en cours sont jetés sans être écrits.
pub fn oublier_en_cours() {
    for source in [SourceAudio::Micro, SourceAudio::Systeme] {
        *etat(source).lock().unwrap_or_else(|e| e.into_inner()) = EtatSource::nouveau();
    }
}

/// Une trame de 30 ms (finie à `fin_trame`) : met à jour le plancher de bruit et l'état parole / silence.
fn analyser(etat: &mut EtatSource, source: SourceAudio, trame: &[f32], fin_trame: SystemTime) -> Option<Segment> {
    let db = niveau_db(trame);
    if source == SourceAudio::Systeme {
        noter_enveloppe_mac(fin_trame, db);
    }
    // Le plancher suit vite le bruit qui baisse, lentement celui qui monte (~0,7 dB/s).
    if db < etat.plancher_db {
        etat.plancher_db = db;
    } else {
        etat.plancher_db += 0.02;
    }
    let parle = db > (etat.plancher_db + MARGE_DB).max(PLANCHER_ABSOLU_DB);
    match etat.debut {
        None => {
            etat.trames_de_parole = if parle { etat.trames_de_parole + 1 } else { 0 };
            etat.avant.extend(trame.iter().copied());
            while etat.avant.len() > AVANT {
                etat.avant.pop_front();
            }
            if etat.trames_de_parole >= TRAMES_POUR_COMMENCER {
                // La parole commence : on garde aussi les 0,3 s d'avant.
                let avance = Duration::from_secs_f64(etat.avant.len() as f64 / f64::from(TAUX));
                etat.debut = Some(fin_trame - avance);
                etat.segment = etat.avant.drain(..).collect();
                etat.trames_de_silence = 0;
            }
            None
        }
        Some(debut) => {
            etat.segment.extend_from_slice(trame);
            etat.trames_de_silence = if parle { 0 } else { etat.trames_de_silence + 1 };
            let fini = etat.trames_de_silence >= TRAMES_DE_SILENCE_POUR_FINIR || etat.segment.len() >= DUREE_MAX;
            if !fini {
                return None;
            }
            let mut echantillons = std::mem::take(&mut etat.segment);
            etat.debut = None;
            etat.trames_de_parole = 0;
            // Le silence de la fin ne sert à rien : on n'en garde que 0,3 s.
            let silence = (etat.trames_de_silence as usize * TRAME).saturating_sub(AVANT);
            echantillons.truncate(echantillons.len() - silence.min(echantillons.len()));
            etat.trames_de_silence = 0;
            (echantillons.len() >= DUREE_MIN).then_some(Segment {
                source,
                debut,
                echantillons,
            })
        }
    }
}

/// (« 2026-11-19 », « 215700-123 ») à l'heure locale.
fn date_locale(quand: SystemTime) -> (String, String) {
    let ms = ms_depuis_1970(quand);
    let secondes = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY : localtime_r n'écrit que dans `tm`.
    unsafe { libc::localtime_r(&secondes, &mut tm) };
    (
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        format!("{:02}{:02}{:02}-{:03}", tm.tm_hour, tm.tm_min, tm.tm_sec, ms % 1000),
    )
}

fn constante(c: Option<&'static NSString>) -> Result<&'static NSString> {
    c.ok_or_else(|| anyhow!("constante AVFAudio absente"))
}

/// Décalage de l'encodeur AAC (« priming ») au début de chaque fichier : 2112 échantillons.
/// Le flux ADTS ne le signale pas ; à soustraire des heures lues dans le fichier décodé.
pub const AMORCE_S: f64 = 2112.0 / TAUX as f64;

/// Écrit des échantillons (16 kHz mono) en AAC à 24 kb/s, dans un flux ADTS (`.aac`) :
/// aucun en-tête (un `.m4a` réserve 24 Ko par fichier), et rien n'est perdu si l'app s'arrête net.
pub fn ecrire_aac(chemin: &Path, echantillons: &[f32]) -> Result<()> {
    if let Some(dossier) = chemin.parent() {
        std::fs::create_dir_all(dossier)?;
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&chemin.to_string_lossy()));
    let reglages = NSMutableDictionary::<NSString, AnyObject>::new();
    // SAFETY : constantes globales d'AVFAudio, lues seulement.
    unsafe {
        // kAudioFormatMPEG4AAC = 'aac '
        reglages.insert(constante(AVFormatIDKey)?, &*NSNumber::numberWithUnsignedInt(0x6161_6320));
        reglages.insert(constante(AVSampleRateKey)?, &*NSNumber::numberWithDouble(f64::from(TAUX)));
        reglages.insert(constante(AVNumberOfChannelsKey)?, &*NSNumber::numberWithInt(1));
        reglages.insert(constante(AVEncoderBitRateKey)?, &*NSNumber::numberWithInt(24_000));
    }
    let reglages: Retained<NSDictionary<NSString, AnyObject>> = Retained::into_super(reglages);
    // SAFETY : réglages d'écriture AAC usuels ; tampon au format standard (flottant, non entrelacé, mono).
    let fichier = unsafe { AVAudioFile::initForWriting_settings_error(AVAudioFile::alloc(), &url, &reglages) }
        .map_err(|e| anyhow!("fichier audio : {}", e.localizedDescription()))?;
    let format =
        unsafe { AVAudioFormat::initStandardFormatWithSampleRate_channels(AVAudioFormat::alloc(), f64::from(TAUX), 1) }
            .ok_or_else(|| anyhow!("format audio"))?;
    let n = echantillons.len() as u32;
    let tampon = unsafe { AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(AVAudioPCMBuffer::alloc(), &format, n) }
        .ok_or_else(|| anyhow!("tampon audio"))?;
    unsafe {
        let canaux = tampon.floatChannelData();
        if canaux.is_null() {
            return Err(anyhow!("tampon audio sans canal"));
        }
        std::ptr::copy_nonoverlapping(echantillons.as_ptr(), (*canaux).as_ptr(), n as usize);
        tampon.setFrameLength(n);
        fichier
            .writeFromBuffer_error(&tampon)
            .map_err(|e| anyhow!("écriture audio : {}", e.localizedDescription()))?;
    }
    // Le fichier est fermé (et finalisé) quand `fichier` est relâché, à la sortie.
    Ok(())
}

/// Écrit un passage en AAC et l'inscrit dans la base (avec son empreinte de voix et sa voix connue ;
/// `appel` : le son du Mac pendant un appel).
fn enregistrer_segment(segment: &Segment, empreinte: Option<&[f32]>, locuteur: Option<i64>, appel: bool) -> Result<()> {
    let (jour, heure) = date_locale(segment.debut);
    let relatif = format!("audio/{jour}/{}-{heure}.aac", segment.source.nom());
    ecrire_aac(&chemins::donnees().join(&relatif), &segment.echantillons)?;
    let debut_ms = ms_depuis_1970(segment.debut);
    let fin_ms = debut_ms + segment.echantillons.len() as i64 * 1000 / i64::from(TAUX);
    let octets = empreinte.map(bscreen_core::voix_familieres::en_octets);
    avec_base(|b| b.ajouter_voix_avec(segment.source as u8, debut_ms, fin_ms, &relatif, octets.as_deref(), locuteur, appel));
    crate::transcription::signaler();
    SEGMENTS.fetch_add(1, Ordering::Relaxed);
    MS_DE_VOIX.fetch_add((fin_ms - debut_ms) as u64, Ordering::Relaxed);
    Ok(())
}

thread_local! {
    /// Le lecteur en cours (fil principal) : le garder en vie, c'est continuer à jouer.
    static LECTEUR: std::cell::RefCell<Option<Retained<objc2_avf_audio::AVAudioPlayer>>> =
        const { std::cell::RefCell::new(None) };
}

/// Joue un passage enregistré à partir de `depuis_s` secondes ; arrête celui qui jouait.
/// Rend la durée restante, en secondes.
pub fn ecouter(chemin: &Path, depuis_s: f64) -> Result<f64> {
    arreter_ecoute();
    // Partie sur l'archive (phase 10) : ramenée dans le cache d'abord.
    let chemin = &crate::archive::lisible(chemin)?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&chemin.to_string_lossy()));
    let lecteur = unsafe {
        objc2_avf_audio::AVAudioPlayer::initWithContentsOfURL_error(objc2_avf_audio::AVAudioPlayer::alloc(), &url)
    }
    .map_err(|e| anyhow!("lecture : {}", e.localizedDescription()))?;
    // Le fichier commence par l'amorce de l'encodeur : on la saute.
    let debut = (AMORCE_S + depuis_s.max(0.0)).min(unsafe { lecteur.duration() });
    let reste = unsafe {
        lecteur.prepareToPlay();
        lecteur.setCurrentTime(debut);
        if !lecteur.play() {
            return Err(anyhow!("lecture refusée"));
        }
        lecteur.duration() - debut
    };
    LECTEUR.with(|l| *l.borrow_mut() = Some(lecteur));
    Ok(reste)
}

/// Arrête l'écoute en cours, s'il y en a une.
pub fn arreter_ecoute() {
    LECTEUR.with(|l| {
        if let Some(lecteur) = l.borrow_mut().take() {
            unsafe { lecteur.stop() };
        }
    });
}

/// Lit un fichier audio entier, en mono, avec son taux.
pub fn lire_fichier(chemin: &Path) -> Result<(f64, Vec<f32>)> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&chemin.to_string_lossy()));
    let fichier = unsafe { AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url) }
        .map_err(|e| anyhow!("lecture audio : {}", e.localizedDescription()))?;
    let format = unsafe { fichier.processingFormat() };
    let longueur = u32::try_from(unsafe { fichier.length() }).map_err(|_| anyhow!("fichier trop long"))?;
    let tampon =
        unsafe { AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(AVAudioPCMBuffer::alloc(), &format, longueur) }
            .ok_or_else(|| anyhow!("tampon audio"))?;
    unsafe { fichier.readIntoBuffer_error(&tampon) }.map_err(|e| anyhow!("lecture audio : {}", e.localizedDescription()))?;
    let (canaux, n) = unsafe { (format.channelCount().max(1) as usize, tampon.frameLength() as usize) };
    let donnees = unsafe { tampon.floatChannelData() };
    if donnees.is_null() {
        return Err(anyhow!("format audio non flottant"));
    }
    let mut mono = vec![0f32; n];
    for c in 0..canaux {
        // SAFETY : format de traitement = flottant non entrelacé, un tableau de `n` valeurs par canal.
        let canal = unsafe { std::slice::from_raw_parts((*donnees.add(c)).as_ptr(), n) };
        mono.iter_mut().zip(canal).for_each(|(m, v)| *m += v / canaux as f32);
    }
    Ok((unsafe { format.sampleRate() }, mono))
}

/// Ramène un enregistrement entier à 16 kHz.
pub fn en_16k(taux: f64, echantillons: &[f32]) -> Vec<f32> {
    let mut sortie = Vec::with_capacity((echantillons.len() as f64 * f64::from(TAUX) / taux) as usize + 1);
    Reechantillonneur::default().pousser(taux, echantillons, &mut sortie);
    sortie
}

/// Découpe un enregistrement en passages de voix, comme le fait la capture : (début en s, échantillons à 16 kHz).
pub fn decouper(taux: f64, echantillons: &[f32]) -> Vec<(f64, Vec<f32>)> {
    let mut etat = EtatSource::nouveau();
    let mut a_16k = Vec::new();
    etat.reechantillonneur.pousser(taux, echantillons, &mut a_16k);
    // 1 s de silence à la fin, pour clore un passage en cours.
    a_16k.extend(std::iter::repeat_n(0.0, TAUX as usize));
    let t0 = SystemTime::UNIX_EPOCH;
    let mut passages = Vec::new();
    for (i, trame) in a_16k.chunks_exact(TRAME).enumerate() {
        let fin = t0 + Duration::from_secs_f64(((i + 1) * TRAME) as f64 / f64::from(TAUX));
        if let Some(segment) = analyser(&mut etat, SourceAudio::Systeme, trame, fin) {
            let debut = segment.debut.duration_since(t0).unwrap_or_default().as_secs_f64();
            passages.push((debut, segment.echantillons));
        }
    }
    passages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reechantillonnage_vers_16k_sans_derive() {
        let mut r = Reechantillonneur::default();
        let mut sortie = Vec::new();
        // Des tampons de 1024 échantillons (pas un multiple de 3) : aucune dérive au total.
        for _ in 0..30 {
            let entree: Vec<f32> = (0..1024).map(|i| (i % 3) as f32).collect();
            r.pousser(48_000.0, &entree, &mut sortie);
        }
        assert_eq!(sortie.len(), 30 * 1024 / 3);
        let mut r = Reechantillonneur::default();
        let mut sortie = Vec::new();
        r.pousser(44_100.0, &vec![0.5; 44_100], &mut sortie);
        assert_eq!(sortie.len(), 16_000);
        assert!(sortie.iter().all(|v| (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn echo_du_mac_reconnu() {
        // Une enveloppe de parole (syllabes), et la même vue par le micro 2 trames plus tard, atténuée.
        let mac: Vec<f32> = (0..120).map(|k| if (k / 4) % 3 == 0 { -50.0 } else { -20.0 - (k % 7) as f32 }).collect();
        let echo: Vec<f32> = (0..120).map(|k| if k < 2 { -60.0 } else { mac[k - 2] - 12.0 }).collect();
        let mac_vu: Vec<Option<f32>> = mac.iter().map(|&x| Some(x)).collect();
        assert!(ressemble_au_mac(&echo, &mac_vu), "écho retardé de 60 ms");
        // Une autre voix (autre rythme) pendant que le Mac parle : pas un écho.
        let autre: Vec<f32> = (0..120).map(|k| if (k / 5) % 2 == 0 { -22.0 } else { -48.0 }).collect();
        assert!(!ressemble_au_mac(&autre, &mac_vu));
        // Le Mac silencieux : jamais un écho.
        let silence = vec![Some(-80.0f32); 120];
        assert!(!ressemble_au_mac(&echo, &silence));
    }

    #[test]
    fn lecture_des_formats() {
        let flottants: Vec<u8> = [0.5f32, -0.25].iter().flat_map(|v| v.to_ne_bytes()).collect();
        assert_eq!(lire_valeurs(&flottants, FLOTTANT, 32), Some(vec![0.5, -0.25]));
        let entiers: Vec<u8> = [16_384i16, -32_768].iter().flat_map(|v| v.to_ne_bytes()).collect();
        assert_eq!(lire_valeurs(&entiers, ENTIER_SIGNE, 16), Some(vec![0.5, -1.0]));
        assert_eq!(lire_valeurs(&entiers, 0, 24), None);
    }

    #[test]
    fn la_parole_fait_un_passage_le_silence_non() {
        let mut etat = EtatSource::nouveau();
        let silence = vec![0.0005f32; TRAME];
        let voix: Vec<f32> = (0..TRAME).map(|i| ((i as f32) * 0.3).sin() * 0.2).collect();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut t = t0;
        let mut passages = Vec::new();
        let mut trame = |etat: &mut EtatSource, son: &[f32], passages: &mut Vec<Segment>| {
            t += Duration::from_millis(30);
            passages.extend(analyser(etat, SourceAudio::Micro, son, t));
        };
        // 3 s de silence, 1,8 s de voix, puis 1,2 s de silence.
        for _ in 0..100 {
            trame(&mut etat, &silence, &mut passages);
        }
        assert!(passages.is_empty(), "le silence ne doit rien enregistrer");
        for _ in 0..60 {
            trame(&mut etat, &voix, &mut passages);
        }
        for _ in 0..40 {
            trame(&mut etat, &silence, &mut passages);
        }
        assert_eq!(passages.len(), 1, "une phrase = un passage");
        let p = &passages[0];
        let secondes = p.echantillons.len() as f32 / TAUX as f32;
        assert!((2.0..2.6).contains(&secondes), "durée {secondes} s");
        // La voix commence à 3,0 s ; le passage 0,3 s avant.
        let debut = p.debut.duration_since(t0).unwrap().as_secs_f32();
        assert!((2.6..2.85).contains(&debut), "début {debut} s");
    }
}
