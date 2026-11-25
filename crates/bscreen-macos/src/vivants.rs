//! Les réglages qui changent pendant que B-Screen tourne (25/11/2026 : « tout ce qui est dans les
//! réglages, je dois pouvoir le contrôler ») : lus sans attendre par la capture, l'indexation et les
//! moments privés ; posés au lancement et à chaque changement.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use bscreen_core::reglages::Reglages;

static INTERVALLE_CAPTURE_MS: AtomicU64 = AtomicU64::new(2_000);
static DEVERROUILLAGE_S: AtomicU64 = AtomicU64::new(300);
static SENS: AtomicBool = AtomicBool::new(true);
static IMAGES: AtomicBool = AtomicBool::new(true);
static RAFALES_S: AtomicU64 = AtomicU64::new(20 * 60);
static BATTERIE: AtomicBool = AtomicBool::new(true);
static VOIX_APPELS: AtomicBool = AtomicBool::new(true);
static TRAVAIL_NUIT: AtomicBool = AtomicBool::new(true);
static TEXTE_CACHE: AtomicBool = AtomicBool::new(false);
static SITES_EXCLUS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Pose les réglages vivants.
pub fn appliquer(r: &Reglages) {
    INTERVALLE_CAPTURE_MS.store(u64::from(r.intervalle_capture_s.clamp(1, 60)) * 1_000, Relaxed);
    DEVERROUILLAGE_S.store(u64::from(r.deverrouillage_min.clamp(1, 120)) * 60, Relaxed);
    SENS.store(r.recherche_sens, Relaxed);
    IMAGES.store(r.recherche_images, Relaxed);
    RAFALES_S.store(u64::from(r.indexation_min.clamp(1, 24 * 60)) * 60, Relaxed);
    BATTERIE.store(r.indexation_batterie, Relaxed);
    bscreen_core::masquage::ACTIF.store(r.masquer_secrets, Relaxed);
    VOIX_APPELS.store(r.voix_appels, Relaxed);
    TRAVAIL_NUIT.store(r.travail_nuit, Relaxed);
    TEXTE_CACHE.store(r.texte_cache, Relaxed);
    *SITES_EXCLUS.lock().unwrap_or_else(|e| e.into_inner()) = r.sites_exclus.clone();
}

/// Cette adresse est-elle sur un site jamais enregistré ?
pub fn site_exclu(url: &str) -> bool {
    bscreen_core::reglages::site_exclu(url, &SITES_EXCLUS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Enregistrer les voix pendant les appels ?
pub fn voix_appels() -> bool {
    VOIX_APPELS.load(Relaxed)
}

/// Résumés et phrases floues la nuit ?
pub fn travail_nuit() -> bool {
    TRAVAIL_NUIT.load(Relaxed)
}

/// Au plus une image par cet intervalle.
pub fn intervalle_capture() -> Duration {
    Duration::from_millis(INTERVALLE_CAPTURE_MS.load(Relaxed))
}

/// La navigation privée reste visible ce temps après Touch ID.
pub fn deverrouillage() -> Duration {
    Duration::from_secs(DEVERROUILLAGE_S.load(Relaxed))
}

pub fn recherche_sens() -> bool {
    SENS.load(Relaxed)
}

pub fn recherche_images() -> bool {
    IMAGES.load(Relaxed)
}

/// L'indexation de fond passe au plus une fois par cet intervalle.
pub fn intervalle_rafales() -> Duration {
    Duration::from_secs(RAFALES_S.load(Relaxed))
}

/// L'indexation de fond peut-elle tourner sur batterie (Mac inactif) ?
pub fn indexation_sur_batterie() -> bool {
    BATTERIE.load(Relaxed)
}

/// Garder aussi le texte jamais visible ?
pub fn texte_cache() -> bool {
    TEXTE_CACHE.load(Relaxed)
}
