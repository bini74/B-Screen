//! Les résumés de la nuit (phase 7.4) : sur secteur, Mac inactif depuis 15 minutes, les épisodes
//! terminés des 7 derniers jours qui n'ont pas encore de résumé sont résumés par le modèle local
//! (Ollama), du plus récent au plus ancien, un par un ; puis les échanges de voix sont relus pour
//! leurs sous-entendus (7.6, `sous_entendus`). On s'arrête dès que Julien revient.
//! Rien de privé n'y entre (l'activité et les voix écartent les moments privés), rien ne sort du Mac.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use bscreen_api::episodes::{self, Plage};
use bscreen_api::heures::lisible;
use bscreen_core::chemins;
use bscreen_store::Base;

use crate::ollama;

const INACTIF_DEPUIS_S: f64 = 15.0 * 60.0;
const JOURS: i64 = 7;
const INTERVALLE: Duration = Duration::from_secs(10 * 60);
static EN_COURS: AtomicBool = AtomicBool::new(false);

fn maintenant_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Sur secteur, Mac inactif, frise et Ask fermés.
fn moment_propice() -> bool {
    crate::vivants::travail_nuit()
        && crate::sens::sur_secteur()
        && crate::moteur::secondes_d_inactivite() >= INACTIF_DEPUIS_S
        && !crate::moteur::interface_est_ouverte()
}

/// Le fil des résumés : un coup d'œil toutes les 10 minutes.
pub fn demarrer() {
    if !ollama::modele_present() {
        tracing::info!(modele = ollama::MODELE, "résumés de la nuit : modèle local absent, rien à faire");
        return;
    }
    let _ = std::thread::Builder::new().name("bscreen-resumes".into()).spawn(|| {
        // Des modèles restés en mémoire après un arrêt brutal : rendus tout de suite.
        ollama::nettoyer_orphelins();
        // Priorité « en arrière-plan » : ne gêne jamais ce que fait Julien.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
        }
        loop {
            std::thread::sleep(INTERVALLE);
            if !moment_propice() {
                continue;
            }
            match resumer(usize::MAX, false) {
                Ok(0) => {}
                Ok(n) => tracing::info!(episodes = n, "résumés de la nuit faits"),
                Err(e) => tracing::warn!(%e, "résumés de la nuit interrompus"),
            }
        }
    });
}

/// Les épisodes terminés des 7 derniers jours sans résumé, du plus récent au plus ancien.
pub fn a_resumer(base: &Base) -> Result<Vec<Plage>> {
    let maintenant = maintenant_ms();
    // Terminé : plus de 20 minutes sans image après lui (une suite commencerait un autre épisode).
    let jusqu_a = maintenant - episodes::TROU_MS - 5 * 60_000;
    let heures = base.heures_non_privees(maintenant - JOURS * 86_400_000, jusqu_a)?;
    let mut plages: Vec<Plage> = episodes::decouper(&heures)
        .into_iter()
        .filter(|p| base.episodes_entre(p.debut_ms, p.fin_ms).is_ok_and(|e| e.is_empty()))
        .collect();
    plages.reverse();
    Ok(plages)
}

/// Résume au plus `limite` épisodes ; `toujours` : même si le Mac sert (commande `bscreen resumer`).
/// Rend le nombre d'épisodes résumés.
pub fn resumer(limite: usize, toujours: bool) -> Result<usize> {
    if EN_COURS.swap(true, Ordering::SeqCst) {
        return Ok(0);
    }
    let resultat = (|| {
        // La base d'écriture d'abord : elle met le schéma à jour (table des épisodes) s'il le faut.
        crate::moteur::avec_base(|_| Ok(())).ok_or_else(|| anyhow!("base inaccessible"))?;
        let base = Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
        let plages = a_resumer(&base)?;
        // Les sous-entendus (7.6) aussi : les échanges de voix pas encore relus.
        let a_relire = crate::sous_entendus::a_faire(&base, maintenant_ms());
        if plages.is_empty() && !a_relire {
            return Ok(0);
        }
        let _serveur = ollama::Serveur::demarrer()?;
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "titre": { "type": "string" }, "resume": { "type": "string" } },
            "required": ["titre", "resume"]
        });
        let mut faits = 0;
        for plage in plages.into_iter().take(limite) {
            if !toujours && !moment_propice() {
                break;
            }
            // Un effacement pendant le résumé : celui-ci parlerait de ce qui a été effacé.
            let effacements = crate::effacement::EFFACEMENTS.load(Ordering::SeqCst);
            let debut = Instant::now();
            let matiere = episodes::matiere(&base, plage);
            let reponse = ollama::discuter(episodes::CONSIGNES, &matiere, &schema)?;
            let Some((titre, resume)) = episodes::lire_reponse(&reponse) else {
                tracing::warn!(debut = %lisible(plage.debut_ms), "résumé illisible : épisode laissé pour la prochaine fois");
                continue;
            };
            if crate::effacement::EFFACEMENTS.load(Ordering::SeqCst) != effacements {
                continue;
            }
            crate::moteur::avec_base(|b| {
                b.ajouter_episode(plage.debut_ms, plage.fin_ms, &titre, &resume, ollama::MODELE, maintenant_ms())
            })
            .ok_or_else(|| anyhow!("base inaccessible"))?;
            faits += 1;
            tracing::info!(
                secondes = debut.elapsed().as_secs(),
                debut = %lisible(plage.debut_ms),
                caracteres = matiere.chars().count(),
                "épisode résumé"
            );
        }
        // Puis les phrases floues des échanges de voix, tant que le moment reste propice.
        if a_relire {
            let base_ecriture = Base::ouvrir(&chemins::donnees().join("index.sqlite"))?;
            let propice = || toujours || moment_propice();
            match crate::sous_entendus::relire(&base_ecriture, maintenant_ms(), limite, &propice) {
                Ok(0) => {}
                Ok(n) => tracing::info!(deduites = n, "sous-entendus de la nuit"),
                Err(e) => tracing::warn!(%e, "sous-entendus interrompus"),
            }
        }
        Ok(faits)
    })();
    EN_COURS.store(false, Ordering::SeqCst);
    resultat
}
