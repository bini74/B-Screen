//! Les épisodes de la journée (phase 7.4) : l'activité découpée en plages, et ce qu'on donne au
//! modèle qui les résume la nuit, sur le Mac. Rien de privé : l'activité et les voix viennent des
//! mêmes lectures que le serveur MCP, qui écartent les moments privés.

use bscreen_store::Base;

use crate::contexte::{compacter_heures, couper, raccourcir_adresses};
use crate::heures::lisible;

/// Au-delà de ce trou sans image, un nouvel épisode commence.
pub const TROU_MS: i64 = 15 * 60_000;
/// Un épisode dure au plus une heure (au-delà, il est coupé).
pub const DUREE_MAX_MS: i64 = 60 * 60_000;
/// Les miettes (moins de 2 minutes, ou moins de 3 images) ne sont pas résumées.
const DUREE_MIN_MS: i64 = 2 * 60_000;
const IMAGES_MIN: usize = 3;
/// Ce qu'on donne au modèle pour un épisode : ≈ 2 500 jetons, ≈ 12 s de lecture sur un M1 Pro.
const MAX_ACTIVITE: usize = 3_500;
const MAX_VOIX: usize = 1_500;
/// Les résumés d'une période dans le contexte d'Ask.
const MAX_RESUMES: usize = 4_000;

/// Une plage d'activité (première et dernière image).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plage {
    pub debut_ms: i64,
    pub fin_ms: i64,
}

/// Découpe des heures d'images (triées) en épisodes : un trou de plus de 15 minutes en commence un
/// nouveau, un épisode dure au plus une heure, les miettes sont laissées.
pub fn decouper(heures: &[i64]) -> Vec<Plage> {
    let mut plages = Vec::new();
    // (début, dernière image, nombre d'images)
    let mut courante: Option<(i64, i64, usize)> = None;
    for &h in heures {
        courante = match courante {
            Some((debut, derniere, n)) if h - derniere <= TROU_MS && h - debut < DUREE_MAX_MS => Some((debut, h, n + 1)),
            Some((debut, derniere, n)) => {
                garder(&mut plages, debut, derniere, n);
                Some((h, h, 1))
            }
            None => Some((h, h, 1)),
        };
    }
    if let Some((debut, derniere, n)) = courante {
        garder(&mut plages, debut, derniere, n);
    }
    plages
}

fn garder(plages: &mut Vec<Plage>, debut_ms: i64, fin_ms: i64, images: usize) {
    if fin_ms - debut_ms >= DUREE_MIN_MS && images >= IMAGES_MIN {
        plages.push(Plage { debut_ms, fin_ms });
    }
}

/// Les consignes du modèle qui résume (réponse en JSON : titre et résumé).
pub const CONSIGNES: &str = "Tu résumes un épisode de la journée de Julien, à partir de ce que B-Screen a vu sur son \
écran et entendu sur son Mac. Réponds en JSON : {\"titre\": \"…\", \"resume\": \"…\"}. Le titre : 3 à 8 mots, ce qu'il \
faisait. Le résumé : 2 à 4 phrases en français, concrètes : les apps, sites, documents et sujets, les noms propres \
importants. La voix de Julien s'appelle « Toi » (micro). « vidéo (son du Mac) » : le son d'une vidéo, d'un film ou \
de musique ; ce qui y est dit est son contenu, pas une conversation de Julien. « (appel) » : la personne au bout du \
fil d'un appel sur le Mac. Une voix nommée au micro (« Camille ») parlait près de Julien, ou au téléphone en \
haut-parleur. « voix pas reconnue » : quelqu'un dont la voix n'a pas été reconnue avec certitude. N'invente rien : \
seulement ce que montre le texte.";

/// Ce qu'on donne au modèle pour une plage : l'activité à l'écran (et qui parlait), puis ce qui a été dit.
pub fn matiere(base: &Base, plage: Plage) -> String {
    let activite = compacter_heures(&raccourcir_adresses(&crate::mcp::activite(base, plage.debut_ms, plage.fin_ms)));
    let voix = crate::mcp::voix_de(base, plage.debut_ms, plage.fin_ms, None);
    format!(
        "Épisode du {} au {}.\n\n## Activité à l'écran (et qui parlait)\n{}\n\n## Ce qui a été dit\n{}",
        lisible(plage.debut_ms),
        lisible(plage.fin_ms),
        couper(activite.trim(), MAX_ACTIVITE),
        couper(voix.trim(), MAX_VOIX)
    )
}

/// Le titre et le résumé rendus par le modèle (JSON), nettoyés ; rien s'ils sont vides ou absurdes.
pub fn lire_reponse(texte: &str) -> Option<(String, String)> {
    let debut = texte.find('{')?;
    let fin = texte.rfind('}')?;
    let valeur: serde_json::Value = serde_json::from_str(texte.get(debut..=fin)?).ok()?;
    let propre = |cle: &str, max: usize| -> Option<String> {
        let t = valeur[cle].as_str()?.split_whitespace().collect::<Vec<_>>().join(" ");
        let t = t.trim().trim_end_matches('.').trim().to_string();
        (!t.is_empty() && t.chars().count() <= max).then_some(t)
    };
    let titre = propre("titre", 120)?;
    let resume = valeur["resume"].as_str()?.split_whitespace().collect::<Vec<_>>().join(" ");
    let resume = resume.trim().to_string();
    if resume.chars().count() < 20 || resume.chars().count() > 1_500 {
        return None;
    }
    Some((titre, resume))
}

/// Les résumés des épisodes d'une période, une ligne chacun (pour le contexte d'Ask) ; vide s'il n'y
/// en a pas.
pub fn resumes(base: &Base, debut_ms: i64, fin_ms: i64) -> String {
    let lignes: Vec<String> = base
        .episodes_entre(debut_ms, fin_ms)
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            format!(
                "• {} → {} · {} — {}",
                compacter_heures(&lisible(e.debut_ms)),
                compacter_heures(&lisible(e.fin_ms)),
                e.titre,
                e.resume
            )
        })
        .collect();
    couper(&lignes.join("\n"), MAX_RESUMES)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn decoupage_en_episodes() {
        // 10:00 → 10:20 toutes les minutes ; trou de 40 min ; 11:00 → 13:00 toutes les 2 min ;
        // une miette à 15:00 (deux images).
        let mut heures: Vec<i64> = (0..=20).map(|m| 600 * MIN + m * MIN).collect();
        heures.extend((0..=60).map(|m| 660 * MIN + 2 * m * MIN));
        heures.extend([900 * MIN, 900 * MIN + 30_000]);
        let plages = decouper(&heures);
        assert_eq!(plages[0], Plage { debut_ms: 600 * MIN, fin_ms: 620 * MIN });
        // 11:00 → 13:00 : coupée par heures (l'image de 13:00, seule, et la miette de 15:00 sont laissées).
        assert_eq!(plages[1], Plage { debut_ms: 660 * MIN, fin_ms: 718 * MIN });
        assert_eq!(plages[2], Plage { debut_ms: 720 * MIN, fin_ms: 778 * MIN });
        assert_eq!(plages.len(), 3);
        assert!(decouper(&[]).is_empty());
    }

    #[test]
    fn reponse_du_modele() {
        let r = lire_reponse("Voici :\n{\"titre\": \"Recherche de montres.\", \"resume\": \"Julien compare des montres Tissot et Citizen sur leboncoin.\"}");
        assert_eq!(
            r,
            Some(("Recherche de montres".to_string(), "Julien compare des montres Tissot et Citizen sur leboncoin.".to_string()))
        );
        assert_eq!(lire_reponse("{\"titre\": \"\", \"resume\": \"Assez long pour passer le filtre.\"}"), None);
        assert_eq!(lire_reponse("{\"titre\": \"Court\", \"resume\": \"Trop court\"}"), None);
        assert_eq!(lire_reponse("pas de JSON"), None);
    }

    #[test]
    fn resumes_d_une_periode() {
        let base = Base::en_memoire().unwrap();
        base.ajouter_episode(1_000, 61_000, "Montres", "Julien compare des montres sur leboncoin.", "qwen3.5:9b", 0)
            .unwrap();
        let texte = resumes(&base, 0, 100_000);
        assert!(texte.contains("Montres — Julien compare des montres"), "{texte}");
        assert!(resumes(&base, 200_000, 300_000).is_empty());
        assert_eq!(base.oublier_episodes_a(&[30_000]).unwrap(), 1);
        assert!(resumes(&base, 0, 100_000).is_empty());
    }
}
