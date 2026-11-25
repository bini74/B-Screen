//! Démarrage de B-Screen côté macOS : mode agent, services de fond, minuteries.
//! La boucle principale, elle, est celle de l'interface (gpui).

use std::ptr::NonNull;

use anyhow::{Result, anyhow};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_foundation::{NSString, NSTimer};

use crate::{barre, moteur};

/// Intervalle de mise à jour du menu (autorisations…). Peu coûteux : trois appels système.
const RAFRAICHISSEMENT_S: f64 = 2.0;

/// Mode « agent » : pas d'icône dans le Dock, seulement celle de la barre des menus.
pub fn mode_agent() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Démarre tout ce qui tourne en fond : icône et menu, capture, minuteries, purge.
/// À appeler une fois, sur le fil principal, quand la boucle de l'app tourne déjà.
/// `ouvrir_frise` et `ouvrir_reglages` sont appelés quand Julien les choisit dans le menu.
pub fn demarrer_services(
    ouvrir_frise: impl Fn() + 'static,
    ouvrir_reglages: impl Fn() + 'static,
    ouvrir_ask: impl Fn() + 'static,
) -> Result<()> {
    MainThreadMarker::new().ok_or_else(|| anyhow!("services à démarrer sur le fil principal"))?;
    let quitter = || {
        if let Some(mtm) = MainThreadMarker::new() {
            tracing::info!("arrêt demandé depuis le menu");
            NSApplication::sharedApplication(mtm).terminate(None);
        }
    };
    if let Err(e) = barre::installer(quitter, ouvrir_frise, ouvrir_reglages, ouvrir_ask) {
        tracing::error!(%e, "icône de la barre des menus impossible");
    }
    moteur::noter_apps_exclues();
    moteur::surveiller_changements_d_app();
    crate::declencheurs::surveiller();
    moteur::reparer();
    moteur::demarrer_si_possible(bscreen_core::reglages::Reglages::charger().pause);
    let _ = std::thread::Builder::new().name("bscreen-purge".into()).spawn(|| {
        moteur::masquer_retroactivement();
        moteur::purger();
        moteur::mesurer_place();
    });
    crate::transcription::demarrer();
    // Les résumés de la nuit (modèle local), quand le Mac est sur secteur et inactif.
    crate::resumes::demarrer();
    // Appels sur le Mac : le son du Mac n'a de voix que pendant un appel.
    crate::appels::demarrer();
    // L'archive du homelab (phase 10) : chaque nuit, ce qui a passé l'âge part, chiffré.
    crate::archive::demarrer();
    moteur::mesurer_ressources();
    let _ = std::thread::Builder::new()
        .name("bscreen-prechauffage".into())
        .spawn(prechauffer_ocr_si_besoin);
    let _ = std::thread::Builder::new().name("bscreen-cache".into()).spawn(nettoyer_cache_coreml);
    let reglages = bscreen_core::reglages::Reglages::charger();
    crate::vivants::appliquer(&reglages);
    if reglages.api_locale {
        crate::api_locale::demarrer(reglages.port_api);
    }

    // Une ligne par minute dans le journal : images reçues et gardées.
    let bilan = RcBlock::new(|_minuterie: NonNull<NSTimer>| bilan_minute());
    let minuterie_bilan = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(60.0, true, &bilan) };
    minuterie_bilan.setTolerance(10.0);

    let rafraichir = RcBlock::new(|_minuterie: NonNull<NSTimer>| barre::rafraichir());
    let minuterie =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(RAFRAICHISSEMENT_S, true, &rafraichir) };
    // Laisse macOS regrouper les réveils : moins de consommation.
    minuterie.setTolerance(RAFRAICHISSEMENT_S / 2.0);

    tracing::info!(version = bscreen_core::VERSION, "B-Screen démarre");
    Ok(())
}

/// Vrai si une autre instance de B-Screen tourne déjà.
pub fn deja_lancee() -> bool {
    let identifiant = NSString::from_str(bscreen_core::IDENTIFIANT);
    let instances = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifiant);
    let moi = std::process::id() as i32;
    instances.iter().any(|app| app.processIdentifier() != moi)
}

/// L'heure de démarrage du Mac (secondes), pour ne préparer l'OCR qu'une fois par démarrage.
fn demarrage_du_mac() -> Option<i64> {
    let mut quand: libc::timeval = unsafe { std::mem::zeroed() };
    let mut taille = std::mem::size_of::<libc::timeval>();
    let mut nom = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    // SAFETY : sysctl n'écrit que dans `quand` (de la taille donnée).
    let ok = unsafe {
        libc::sysctl(
            nom.as_mut_ptr(),
            2,
            (&mut quand as *mut libc::timeval).cast(),
            &mut taille,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    ok.then_some(quand.tv_sec)
}

/// Une fois par démarrage du Mac, 90 s après le lancement : prépare le modèle d'OCR précis (une
/// minute la première fois) dans un processus à part, qui rend sa mémoire en finissant. La première
/// sélection de texte dans la frise n'attend plus.
fn prechauffer_ocr_si_besoin() {
    std::thread::sleep(std::time::Duration::from_secs(90));
    let Some(demarrage) = demarrage_du_mac() else { return };
    let repere = bscreen_core::chemins::donnees().join("ocr-prechauffe");
    if std::fs::read_to_string(&repere).ok().and_then(|t| t.trim().parse::<i64>().ok()) == Some(demarrage) {
        return;
    }
    let Ok(moi) = std::env::current_exe() else { return };
    let debut = std::time::Instant::now();
    match std::process::Command::new(moi).arg("prechauffer-ocr").status() {
        Ok(etat) if etat.success() => {
            let _ = std::fs::write(&repere, demarrage.to_string());
            tracing::info!(secondes = debut.elapsed().as_secs(), "OCR précis préparé pour ce démarrage");
        }
        Ok(etat) => tracing::warn!(?etat, "préparation de l'OCR précis : échec"),
        Err(e) => tracing::warn!(%e, "préparation de l'OCR précis impossible"),
    }
}

fn bilan_minute() {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    moteur::mesurer_ressources();
    crate::mesures::minute();
    moteur::surveiller_place();
    static PRECEDENT: (AtomicU64, AtomicU64) = (AtomicU64::new(0), AtomicU64::new(0));
    let (recues, gardees) = (
        crate::capture::STATS.recues.load(Relaxed),
        crate::capture::STATS.gardees.load(Relaxed),
    );
    let (r0, g0) = (PRECEDENT.0.swap(recues, Relaxed), PRECEDENT.1.swap(gardees, Relaxed));
    if moteur::en_marche() {
        let (clics, pauses) = (
            crate::declencheurs::CLICS.swap(0, Relaxed),
            crate::declencheurs::PAUSES_DE_FRAPPE.swap(0, Relaxed),
        );
        tracing::info!(
            recues = recues - r0,
            gardees = gardees - g0,
            clics,
            pauses_de_frappe = pauses,
            "capture, dernière minute"
        );
        // Chien de garde : macOS n'envoie plus rien depuis 3 min alors que le Mac est utilisé.
        static MINUTES_MUETTES: AtomicU64 = AtomicU64::new(0);
        let muettes = if recues == r0 {
            MINUTES_MUETTES.fetch_add(1, Relaxed) + 1
        } else {
            MINUTES_MUETTES.store(0, Relaxed);
            0
        };
        if muettes >= 3 && moteur::secondes_d_inactivite() < 60.0 {
            MINUTES_MUETTES.store(0, Relaxed);
            moteur::relancer("aucune image depuis 3 min alors que le Mac est utilisé");
        }
    }
    // Audio : tampons reçus (et illisibles), passages de voix écrits.
    {
        use crate::audio::{ECARTES, ECHOS, MS_DE_VOIX, SEGMENTS, TAMPONS, TAMPONS_ILLISIBLES};
        let (tampons, illisibles, passages, ecartes, echos, ms) = (
            TAMPONS.swap(0, Relaxed),
            TAMPONS_ILLISIBLES.swap(0, Relaxed),
            SEGMENTS.swap(0, Relaxed),
            ECARTES.swap(0, Relaxed),
            ECHOS.swap(0, Relaxed),
            MS_DE_VOIX.swap(0, Relaxed),
        );
        if tampons > 0 {
            tracing::info!(
                tampons,
                illisibles,
                passages,
                ecartes,
                echos,
                secondes_de_voix = ms / 1000,
                "audio, dernière minute"
            );
        }
        use crate::transcription::{AUDIO_MS, DUREE_MS, TRANSCRITS};
        let (transcrits, duree, audio) = (TRANSCRITS.swap(0, Relaxed), DUREE_MS.swap(0, Relaxed), AUDIO_MS.swap(0, Relaxed));
        if transcrits > 0 {
            // Vitesse : secondes de voix transcrites par seconde de calcul.
            let vitesse = audio as f64 / duree.max(1) as f64;
            tracing::info!(transcrits, secondes_de_voix = audio / 1000, vitesse = format!("{vitesse:.1}x"), "transcription, dernière minute");
        }
    }
    // Indexation du texte : lectures, durée moyenne, éléments lus, nouvelles apparitions.
    {
        use crate::indexation::{DUREE_TOTALE_MS, ELEMENTS, LECTURES, NOUVELLES_APPARITIONS};
        let (lectures, duree, elements, nouvelles) = (
            LECTURES.swap(0, Relaxed),
            DUREE_TOTALE_MS.swap(0, Relaxed),
            ELEMENTS.swap(0, Relaxed),
            NOUVELLES_APPARITIONS.swap(0, Relaxed),
        );
        let masques = crate::indexation::MASQUES.swap(0, Relaxed);
        let privees = crate::indexation::PRIVEES.swap(0, Relaxed);
        if let (Some(ms_moyen), Some(elements_moyen)) = (duree.checked_div(lectures), elements.checked_div(lectures)) {
            tracing::info!(lectures, ms_moyen, elements_moyen, nouvelles, masques, privees, "texte, dernière minute");
        }
        let (ocr, ocr_ms) = (
            crate::indexation::OCR_LECTURES.swap(0, Relaxed),
            crate::indexation::OCR_DUREE_MS.swap(0, Relaxed),
        );
        if let Some(ms_moyen) = ocr_ms.checked_div(ocr) {
            tracing::info!(lectures = ocr, ms_moyen, "OCR de secours, dernière minute");
        }
        crate::indexation::surveiller();
    }

    // Toutes les 10 minutes : ce qu'il y a dans la base ; toutes les heures : purge.
    static MINUTES: AtomicU64 = AtomicU64::new(0);
    let minutes = MINUTES.fetch_add(1, Relaxed) + 1;
    if minutes.is_multiple_of(10) {
        let _ = std::thread::Builder::new().name("bscreen-bilan".into()).spawn(|| {
            moteur::mesurer_place();
            if let Some(b) = moteur::bilan() {
                tracing::info!(
                    images = b.images,
                    morceaux = b.morceaux,
                    mo = b.octets / 1_048_576,
                    "base"
                );
            }
            // La semaine de mesures (phase 11.3) : une ligne de chiffres toutes les 10 minutes.
            crate::mesures::relever();
            // Et dans le journal : processeur et mémoire de B-Screen.
            let cpu = moteur::CPU_POUR_MILLE.load(Relaxed);
            if cpu != u64::MAX {
                tracing::info!(
                    cpu_pourcent = format!("{},{}", cpu / 10, cpu % 10),
                    memoire_mo = moteur::MEMOIRE.load(Relaxed) / 1_048_576,
                    voix_mo = moteur::PLACE_VOIX.load(Relaxed) / 1_048_576,
                    "ressources"
                );
            }
        });
    }
    if minutes.is_multiple_of(60) {
        let _ = std::thread::Builder::new()
            .name("bscreen-purge".into())
            .spawn(moteur::purger);
    }
    // Toutes les 5 minutes (et 2 minutes après le lancement) : les images privées des vidéos
    // terminées sont chiffrées à part et retirées des vidéos.
    if minutes == 2 || minutes.is_multiple_of(5) {
        let _ = std::thread::Builder::new().name("bscreen-prive".into()).spawn(|| {
            if let Err(e) = crate::prive::ranger_les_moments_prives(false) {
                tracing::warn!(%e, "moments privés pas rangés à part");
            }
        });
    }
}

/// Le cache où macOS garde les modèles Core ML compilés pour B-Screen
/// (`~/Library/Caches/<identifiant>/com.apple.e5rt.e5bundlecache`) : chaque installation de B-Screen
/// recompile ses modèles (≈ 370 Mo) et garde les anciennes copies, qui ne resservent jamais
/// (4,5 Go le 25/11/2026). Au lancement, ce qui est plus vieux que l'installation part.
fn nettoyer_cache_coreml() {
    let Some(installation) = std::env::current_exe().ok().and_then(|e| e.metadata().ok()).and_then(|m| m.modified().ok()) else {
        return;
    };
    let Some(cache) = dirs_cache().map(|c| c.join(bscreen_core::IDENTIFIANT).join("com.apple.e5rt.e5bundlecache")) else {
        return;
    };
    let mut liberes: u64 = 0;
    let mut entrees = 0;
    for version in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        for entree in std::fs::read_dir(version.path()).into_iter().flatten().flatten() {
            let vieille = entree
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|quand| quand + std::time::Duration::from_secs(60) < installation);
            if vieille {
                liberes += taille_dossier(&entree.path());
                if std::fs::remove_dir_all(entree.path()).is_ok() {
                    entrees += 1;
                }
            }
        }
    }
    if entrees > 0 {
        tracing::info!(entrees, mo = liberes / 1_048_576, "cache Core ML : copies d'anciennes installations retirées");
    }
}

fn dirs_cache() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Caches"))
}

fn taille_dossier(chemin: &std::path::Path) -> u64 {
    match std::fs::symlink_metadata(chemin) {
        Ok(m) if m.is_dir() => std::fs::read_dir(chemin)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| taille_dossier(&e.path()))
            .sum(),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}
