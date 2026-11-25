//! La semaine de mesures (phase 11.3). Toutes les 10 minutes, une ligne dans
//! `~/Library/Logs/B-Screen/mesures.jsonl` — seulement des chiffres, rien de ce qui est vu ou dit :
//! processeur et énergie de B-Screen sur l'intervalle, mémoire, place sur le disque, écritures,
//! minutes d'activité et d'appel, secteur ou batterie, images gardées ; et les temps des gestes
//! qui comptent (relire une image dans la frise, chercher, préparer Ask), notés au fil par
//! [`noter`]. `bscreen mesures` en fait le bilan face aux objectifs du plan.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Un geste chronométré.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Geste {
    /// Une grande image décodée pour la frise.
    LectureImage,
    /// Une recherche de la frise (mots, sens, images).
    Recherche,
    /// Le contexte préparé par Ask avant d'interroger le modèle.
    ContexteAsk,
}

/// Une ligne du relevé.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Releve {
    pub quand_ms: i64,
    /// Durée couverte (s).
    pub duree_s: f64,
    /// Temps de processeur pris par B-Screen pendant ce temps (s, tous cœurs confondus).
    pub cpu_s: f64,
    /// Énergie comptée à B-Screen par macOS (joules).
    pub energie_j: f64,
    pub memoire_mo: f64,
    pub memoire_max_mo: f64,
    /// Minutes relevées, minutes où Julien s'est servi du Mac, minutes en appel.
    pub minutes: u64,
    pub actives: u64,
    pub en_appel: u64,
    pub secteur: bool,
    pub en_marche: bool,
    pub images_gardees: u64,
    pub video_mo: f64,
    pub voix_mo: f64,
    pub base_mo: f64,
    /// Écrit sur le disque par B-Screen pendant ce temps (Mo).
    pub ecrit_mo: f64,
    /// Les temps des gestes (ms).
    #[serde(default)]
    pub temps: BTreeMap<Geste, Vec<u32>>,
}

static TEMPS: Mutex<BTreeMap<Geste, Vec<u32>>> = Mutex::new(BTreeMap::new());
static MINUTES: AtomicU64 = AtomicU64::new(0);
static ACTIVES: AtomicU64 = AtomicU64::new(0);
static EN_APPEL: AtomicU64 = AtomicU64::new(0);
static MEMOIRE_MAX: AtomicU64 = AtomicU64::new(0);

/// Note le temps d'un geste.
pub fn noter(geste: Geste, ms: u128) {
    if let Ok(mut temps) = TEMPS.lock() {
        let liste = temps.entry(geste).or_default();
        // Au plus 2 000 par intervalle (une rafale de recherches ne fait pas gonfler le fichier).
        if liste.len() < 2_000 {
            liste.push(ms.min(u128::from(u32::MAX)) as u32);
        }
    }
}

/// À chaque minute : Julien s'est-il servi du Mac, un appel est-il en cours, la mémoire.
pub fn minute() {
    MINUTES.fetch_add(1, Relaxed);
    if crate::moteur::secondes_d_inactivite() < 60.0 {
        ACTIVES.fetch_add(1, Relaxed);
    }
    if crate::appels::app_en_appel().is_some() {
        EN_APPEL.fetch_add(1, Relaxed);
    }
    MEMOIRE_MAX.fetch_max(crate::moteur::MEMOIRE.load(Relaxed), Relaxed);
}

/// Ce que macOS compte pour B-Screen depuis son lancement : processeur (µs), énergie (nJ),
/// écritures sur le disque (octets).
fn compteurs() -> Compteurs {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY : getrusage n'écrit que dans `usage`.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let us = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY : proc_pid_rusage remplit `info` (version 4) pour notre propre processus.
    let ok = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            (&mut info as *mut libc::rusage_info_v4).cast(),
        )
    } == 0;
    let (energie, ecrit) = if ok { (info.ri_billed_energy, info.ri_diskio_byteswritten) } else { (0, 0) };
    (us(usage.ru_utime) + us(usage.ru_stime), energie, ecrit)
}

/// Processeur (µs), énergie (nJ), écritures (octets) : ce que macOS compte pour B-Screen.
type Compteurs = (u64, u64, u64);

/// Le relevé des 10 dernières minutes, ajouté au fichier (à appeler hors du fil principal).
pub fn relever() {
    static PRECEDENT: Mutex<Option<(Instant, Compteurs)>> = Mutex::new(None);
    static GARDEES: AtomicU64 = AtomicU64::new(0);
    let maintenant = compteurs();
    let gardees = crate::capture::STATS.gardees.load(Relaxed);
    let images_gardees = gardees.saturating_sub(GARDEES.swap(gardees, Relaxed));
    let Ok(mut precedent) = PRECEDENT.lock() else { return };
    let Some((quand, avant)) = precedent.replace((Instant::now(), maintenant)) else {
        // Le premier appel ne fait que poser le point de départ.
        TEMPS.lock().map(|mut t| t.clear()).ok();
        return;
    };
    let mo = |octets: u64| octets as f64 / 1_048_576.0;
    let releve = Releve {
        quand_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        duree_s: quand.elapsed().as_secs_f64(),
        cpu_s: maintenant.0.saturating_sub(avant.0) as f64 / 1e6,
        energie_j: maintenant.1.saturating_sub(avant.1) as f64 / 1e9,
        memoire_mo: mo(crate::moteur::MEMOIRE.load(Relaxed)),
        memoire_max_mo: mo(MEMOIRE_MAX.swap(0, Relaxed)),
        minutes: MINUTES.swap(0, Relaxed),
        actives: ACTIVES.swap(0, Relaxed),
        en_appel: EN_APPEL.swap(0, Relaxed),
        secteur: crate::sens::sur_secteur(),
        en_marche: crate::moteur::en_marche(),
        images_gardees,
        video_mo: mo(crate::moteur::PLACE_VIDEO.load(Relaxed)),
        voix_mo: mo(crate::moteur::PLACE_VOIX.load(Relaxed)),
        base_mo: mo(crate::moteur::PLACE_BASE.load(Relaxed)),
        ecrit_mo: mo(maintenant.2.saturating_sub(avant.2)),
        temps: TEMPS.lock().map(|mut t| std::mem::take(&mut *t)).unwrap_or_default(),
    };
    let fichier = bscreen_core::chemins::journaux().join("mesures.jsonl");
    let ligne = match serde_json::to_string(&releve) {
        Ok(l) => l,
        Err(e) => return tracing::warn!(%e, "relevé de mesures illisible"),
    };
    let ecrit = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&fichier)
        .and_then(|mut f| writeln!(f, "{ligne}"));
    if let Err(e) = ecrit {
        tracing::warn!(%e, "relevé de mesures pas écrit");
    }
}

/// Les relevés enregistrés, du plus ancien au plus récent.
pub fn releves() -> Vec<Releve> {
    std::fs::read_to_string(bscreen_core::chemins::journaux().join("mesures.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_releve_se_relit() {
        let mut r = Releve { duree_s: 600.0, cpu_s: 3.0, ..Default::default() };
        r.temps.insert(Geste::Recherche, vec![40, 120]);
        let ligne = serde_json::to_string(&r).unwrap();
        assert!(ligne.contains("\"recherche\":[40,120]"), "{ligne}");
        let relu: Releve = serde_json::from_str(&ligne).unwrap();
        assert_eq!(relu.temps[&Geste::Recherche], vec![40, 120]);
        // Les compteurs avancent.
        let (cpu1, _, _) = compteurs();
        let mut x = 0u64;
        for i in 0..3_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        assert!(x > 0);
        assert!(compteurs().0 > cpu1);
    }
}
