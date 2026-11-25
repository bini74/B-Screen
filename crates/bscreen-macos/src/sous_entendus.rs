//! Les sous-entendus (phase 7.6), relus la nuit par le modèle local avec les résumés (voir
//! `resumes`) : chaque échange de voix des 7 derniers jours, une fois, le plus récent d'abord.
//! Rien de privé n'y entre, rien ne sort du Mac.

use std::sync::atomic::Ordering::SeqCst;

use anyhow::Result;
use bscreen_api::sous_entendus::{self, CONSIGNES, PASSAGES_PAR_LECTURE};
use bscreen_store::Base;

use crate::ollama;

const JOURS: i64 = 7;

/// Y a-t-il des échanges à relire ?
pub fn a_faire(base: &Base, maintenant_ms: i64) -> bool {
    !sous_entendus::a_relire(base, maintenant_ms - JOURS * 86_400_000, maintenant_ms).0.is_empty()
}

/// Relit au plus `limite` échanges tant que `propice()` (le serveur du modèle local doit tourner).
/// Rend le nombre de versions explicites gardées.
pub fn relire(base: &Base, maintenant_ms: i64, limite: usize, propice: &dyn Fn() -> bool) -> Result<usize> {
    let (mut echanges, ecartes) = sous_entendus::a_relire(base, maintenant_ms - JOURS * 86_400_000, maintenant_ms);
    base.noter_relus(&ecartes)?;
    echanges.reverse();
    let schema = sous_entendus::schema();
    let mut gardees = 0;
    for echange in echanges.into_iter().take(limite) {
        if !propice() {
            break;
        }
        let effacements = crate::effacement::EFFACEMENTS.load(SeqCst);
        let debut = std::time::Instant::now();
        let mut trouvees = Vec::new();
        for morceau in echange.chunks(PASSAGES_PAR_LECTURE) {
            let matiere = sous_entendus::matiere(base, morceau);
            let reponse = ollama::discuter(CONSIGNES, &matiere, &schema)?;
            for (i, texte, confiance) in sous_entendus::lire_reponse(&reponse, morceau) {
                trouvees.push((morceau[i].clone(), texte, confiance));
            }
        }
        // Un effacement pendant la lecture : ce qui a été lu n'existe peut-être plus ; relu plus tard.
        if crate::effacement::EFFACEMENTS.load(SeqCst) != effacements {
            continue;
        }
        for (passage, texte, confiance) in &trouvees {
            base.ajouter_deduit(passage, texte, *confiance, ollama::MODELE, maintenant_ms)?;
        }
        gardees += trouvees.len();
        base.noter_relus(&echange.iter().map(|p| p.id).collect::<Vec<_>>())?;
        tracing::info!(
            passages = echange.len(),
            deduites = trouvees.len(),
            secondes = debut.elapsed().as_secs(),
            "échange relu (sous-entendus)"
        );
    }
    Ok(gardees)
}
