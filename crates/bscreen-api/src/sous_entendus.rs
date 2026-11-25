//! Les sous-entendus (phase 7.6) : la nuit, le modèle local relit chaque échange de voix et écrit,
//! pour une phrase floue (« il est derrière le truc »), une version explicite tirée du contexte
//! (« le chargeur du Mac est derrière le support d'écran ») : ce qui a été dit avant, ce qui était
//! à l'écran. L'original ne change jamais ; la version explicite est marquée « déduit », avec une
//! confiance, et entre dans la recherche. Si le contexte ne dit rien, on ne devine pas.

use std::collections::HashMap;

use bscreen_store::{Base, Voix};

use crate::contexte::{compacter_heures, couper, raccourcir_adresses};
use crate::heures::{lisible, plier};
use crate::mcp::{activite, nom_de_voix, origine_de_voix};

/// Deux passages à moins de 5 min l'un de l'autre : le même échange.
const TROU_ECHANGE_MS: i64 = 5 * 60_000;
/// Ce qui a été dit dans la demi-heure d'avant éclaire un échange.
const AVANT_MS: i64 = 30 * 60_000;
/// Au plus ce nombre de passages d'avant.
const PASSAGES_AVANT: usize = 20;
/// Un échange fini depuis moins longtemps peut encore continuer : il sera relu plus tard.
pub const FINI_DEPUIS_MS: i64 = 20 * 60_000;
/// Au plus ce nombre de passages par lecture (un long appel est relu en plusieurs fois).
pub const PASSAGES_PAR_LECTURE: usize = 40;
/// En dessous, la version explicite n'est pas gardée.
pub const CONFIANCE_MIN: f32 = 0.6;

pub const CONSIGNES: &str = "Tu relis une conversation enregistrée par B-Screen (la mémoire du Mac de Julien) pour \
retrouver de quoi parlent ses phrases floues. Une phrase floue désigne une chose, un lieu ou une personne par un mot \
qui ne se comprend pas seul : il, elle, ça, le truc, le machin, là-bas, celui-là, lui… Pour chaque phrase floue de \
L'ÉCHANGE (lignes numérotées), écris une version explicite qui se comprend seule, sans l'échange : elle commence par \
ce dont on parle (jamais par il, elle, ça ou c'est) et ne contient plus aucun de ces mots. Exemple : « il est derrière \
le truc », réponse à « tu sais où est le chargeur ? » juste après « j'ai mis le support d'écran contre le mur », \
devient « Le chargeur du Mac est derrière le support d'écran. » Ne touche pas aux phrases claires ni aux expressions \
toutes faites (« c'était le bazar », « ça marche ») ; n'écris rien quand le contexte ne dit pas de quoi il s'agit : \
ne devine jamais. Garde le sens et le temps de l'original. « confiance » : de 0 (au hasard) à 1 (certain). Réponds \
seulement en JSON : {\"phrases\": [{\"numero\": 2, \"explicite\": \"…\", \"confiance\": 0.8}]} ; \
{\"phrases\": []} s'il n'y a rien.";

/// La forme de la réponse attendue du modèle.
pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "phrases": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "numero": { "type": "integer" },
                        "explicite": { "type": "string" },
                        "confiance": { "type": "number" }
                    },
                    "required": ["numero", "explicite", "confiance"]
                }
            }
        },
        "required": ["phrases"]
    })
}

/// Une vidéo (le son du Mac hors appel) : pas une conversation de Julien.
fn video(p: &Voix) -> bool {
    p.source != 0 && !p.appel && p.locuteur_id.is_none()
}

/// Les échanges à relire de la période (finis depuis au moins 20 min), et les passages qu'on ne
/// relira jamais (vidéos, moments privés), à noter comme relus.
pub fn a_relire(base: &Base, depuis_ms: i64, maintenant_ms: i64) -> (Vec<Vec<Voix>>, Vec<i64>) {
    let jusqu = maintenant_ms - FINI_DEPUIS_MS;
    // Jusqu'à maintenant : un échange qui se poursuit après la borne n'est pas fini.
    let Ok(passages) = base.passages_a_relire(depuis_ms, maintenant_ms) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(verrou) = base.verrou_prive(depuis_ms, maintenant_ms) else {
        return (Vec::new(), Vec::new());
    };
    let mut ecartes = Vec::new();
    let mut echanges: Vec<Vec<Voix>> = Vec::new();
    for p in passages {
        if video(&p) || verrou.pendant(p.debut_ms, p.fin_ms) || p.texte.as_deref().is_none_or(|t| t.trim().is_empty()) {
            ecartes.push(p.id);
            continue;
        }
        match echanges.last_mut() {
            Some(e) if e.last().is_some_and(|d| p.debut_ms - d.fin_ms <= TROU_ECHANGE_MS) => e.push(p),
            _ => echanges.push(vec![p]),
        }
    }
    // Seulement les échanges finis depuis 20 min : les autres seront relus une autre fois.
    echanges.retain(|e| e.last().is_some_and(|d| d.fin_ms <= jusqu));
    (echanges, ecartes)
}

/// « 22:40:18 · Voix 3 : « … » »
fn ligne(p: &Voix) -> String {
    let ou = if p.source != 0 { origine_de_voix(p) } else { "" };
    format!(
        "{} · {}{ou} : « {} »",
        lisible(p.debut_ms).get(11..).unwrap_or_default(),
        nom_de_voix(p),
        p.texte.as_deref().unwrap_or_default().trim()
    )
}

/// Ce que lit le modèle pour un échange (ou un morceau d'un long échange) : ce qui a été dit dans
/// la demi-heure d'avant, ce qui était à l'écran, puis l'échange, ses lignes numérotées.
pub fn matiere(base: &Base, echange: &[Voix]) -> String {
    let (Some(premier), Some(dernier)) = (echange.first(), echange.last()) else {
        return String::new();
    };
    let mut parties = Vec::new();
    let verrou = base.verrou_prive(premier.debut_ms - AVANT_MS, premier.debut_ms).ok();
    let avant: Vec<String> = base
        .voix_entre(premier.debut_ms - AVANT_MS, premier.debut_ms)
        .unwrap_or_default()
        .iter()
        .filter(|p| p.debut_ms < premier.debut_ms && !video(p) && p.texte.as_deref().is_some_and(|t| !t.trim().is_empty()))
        .filter(|p| verrou.as_ref().is_some_and(|v| !v.pendant(p.debut_ms, p.fin_ms)))
        .map(|p| format!("  {}", ligne(p)))
        .collect();
    if !avant.is_empty() {
        let debut = avant.len().saturating_sub(PASSAGES_AVANT);
        parties.push(format!("Ce qui a été dit avant (la demi-heure précédente) :\n{}", avant[debut..].join("\n")));
    }
    let ecran = activite(base, premier.debut_ms - 5 * 60_000, dernier.fin_ms);
    let ecran: String = compacter_heures(&raccourcir_adresses(&ecran)).lines().skip(1).collect::<Vec<_>>().join("\n");
    if !ecran.trim().is_empty() {
        parties.push(format!("À l'écran pendant ce temps :\n{}", couper(&ecran, 1_500)));
    }
    let lignes: Vec<String> = echange.iter().enumerate().map(|(i, p)| format!("  [{}] {}", i + 1, ligne(p))).collect();
    parties.push(format!(
        "L'ÉCHANGE (le {}) :\n{}",
        lisible(premier.debut_ms).get(..15).unwrap_or_default(),
        lignes.join("\n")
    ));
    parties.join("\n\n")
}

/// Les versions explicites lues dans la réponse : (rang du passage dans l'échange, texte, confiance),
/// seulement les sûres, différentes de l'original, une par passage (la plus sûre).
pub fn lire_reponse(reponse: &str, echange: &[Voix]) -> Vec<(usize, String, f32)> {
    let Ok(valeur) = serde_json::from_str::<serde_json::Value>(reponse) else {
        return Vec::new();
    };
    let mut gardees: HashMap<usize, (String, f32)> = HashMap::new();
    for phrase in valeur.get("phrases").and_then(|p| p.as_array()).into_iter().flatten() {
        let (Some(numero), Some(explicite), Some(confiance)) = (
            phrase.get("numero").and_then(|n| n.as_u64()),
            phrase.get("explicite").and_then(|t| t.as_str()).map(str::trim),
            phrase.get("confiance").and_then(|c| c.as_f64()),
        ) else {
            continue;
        };
        let Some(passage) = (numero as usize).checked_sub(1).and_then(|i| echange.get(i)) else { continue };
        let confiance = confiance.clamp(0.0, 1.0) as f32;
        let original = plier(passage.texte.as_deref().unwrap_or_default().trim());
        if confiance < CONFIANCE_MIN
            || explicite.chars().count() < 8
            || explicite.chars().count() > 300
            || plier(explicite).trim_end_matches(['.', '!', '?']) == original.trim_end_matches(['.', '!', '?'])
        {
            continue;
        }
        let i = numero as usize - 1;
        if gardees.get(&i).is_none_or(|(_, c)| *c < confiance) {
            gardees.insert(i, (explicite.to_string(), confiance));
        }
    }
    let mut liste: Vec<(usize, String, f32)> = gardees.into_iter().map(|(i, (t, c))| (i, t, c)).collect();
    liste.sort_by_key(|(i, _, _)| *i);
    liste
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le support d'écran rangé à 22:30, le chargeur cherché à 22:40, et une vidéo à côté.
    fn base_d_essai() -> Base {
        let base = Base::en_memoire().unwrap();
        let moi = base.nouveau_locuteur(&[1u8; 8], true).unwrap();
        let autre = base.nouveau_locuteur(&[2u8; 8], false).unwrap();
        for (source, debut, voix, texte) in [
            (0u8, 1_000_000, Some(autre), "J'ai mis le support d'écran contre le mur."),
            (0, 1_600_000, Some(moi), "Tu sais où est le chargeur ?"),
            (0, 1_608_000, Some(autre), "Il est derrière le truc."),
            (1, 1_610_000, None, "dans cette vidéo on teste un chargeur"),
            (0, 1_616_000, Some(moi), "Ah oui, merci."),
        ] {
            let v = base.ajouter_voix(source, debut, debut + 4_000, "a.aac", None, voix).unwrap();
            let p = base.voix_entre(debut, debut + 1).unwrap().into_iter().find(|p| p.id == v).unwrap();
            base.noter_transcription(&p, texte, "[]", "essai").unwrap();
        }
        base
    }

    #[test]
    fn echanges_et_matiere() {
        let base = base_d_essai();
        // Trop tôt : l'échange de 26 min peut encore continuer (fini depuis moins de 20 min).
        let (echanges, _) = a_relire(&base, 0, 1_620_000 + 10 * 60_000);
        assert!(echanges.len() == 1, "seul le premier échange est fini");
        let (echanges, ecartes) = a_relire(&base, 0, 1_620_000 + FINI_DEPUIS_MS + TROU_ECHANGE_MS + 1);
        assert_eq!(echanges.len(), 2);
        assert_eq!(ecartes.len(), 1, "la vidéo est écartée");
        let charge = &echanges[1];
        assert_eq!(charge.len(), 3);
        let m = matiere(&base, charge);
        assert!(m.contains("Ce qui a été dit avant") && m.contains("support d'écran"), "{m}");
        assert!(m.contains("[2]") && m.contains("Il est derrière le truc."), "{m}");
        assert!(!m.contains("vidéo"), "{m}");
    }

    #[test]
    fn reponses_lues() {
        let base = base_d_essai();
        let (echanges, _) = a_relire(&base, 0, 10_000_000);
        let charge = &echanges[1];
        let reponse = r#"{"phrases": [
            {"numero": 2, "explicite": "Le chargeur est derrière le support d'écran.", "confiance": 0.85},
            {"numero": 2, "explicite": "Le chargeur est derrière l'écran.", "confiance": 0.7},
            {"numero": 1, "explicite": "Tu sais où est le chargeur ?", "confiance": 0.9},
            {"numero": 3, "explicite": "Ah oui, merci pour le chargeur.", "confiance": 0.3},
            {"numero": 9, "explicite": "hors de l'échange", "confiance": 1.0}
        ]}"#;
        let lues = lire_reponse(reponse, charge);
        assert_eq!(lues, vec![(1, "Le chargeur est derrière le support d'écran.".to_string(), 0.85)]);
        assert!(lire_reponse("pas du json", charge).is_empty());
    }
}
