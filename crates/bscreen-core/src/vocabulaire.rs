//! Vocabulaire automatique pour la transcription : les noms propres vus souvent à l'écran
//! (contacts, projets, lieux) aident la reconnaissance vocale à les comprendre quand on les dit.

use std::collections::HashMap;

/// Mots à majuscule trop courants (menus, débuts de phrase, jours…) : jamais retenus.
const MOTS_COURANTS: &[&str] = &[
    "fichier", "édition", "edition", "affichage", "fenêtre", "fenetre", "aide", "format", "outils", "présentation",
    "insertion", "historique", "signets", "profils", "onglet", "nouveau", "nouvelle", "ouvrir", "fermer",
    "enregistrer", "annuler", "rechercher", "recherche", "réglages", "reglages", "préférences", "accueil",
    "file", "edit", "view", "window", "help", "tools", "history", "bookmarks", "search", "settings", "home",
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche", "janvier", "février", "mars",
    "avril", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre", "monday", "tuesday",
    "wednesday", "thursday", "friday", "saturday", "sunday", "les", "des", "une", "pour", "dans", "avec",
    "the", "this", "that", "with", "from", "pas", "est", "sur", "par", "plus", "tout", "mais", "comme", "elle",
    "vous", "nous", "cette", "votre", "notre", "quand", "aussi", "très", "sans", "sous", "entre", "après",
    "avant", "bien", "faire", "voir", "copier", "coller", "supprimer", "terminal", "finder", "safari", "chrome",
    "espace", "échap", "echap", "entrée", "entree", "retour", "touch", "call", "control", "remote", "bindings",
    "windows", "contents", "support", "library", "users", "developer", "applications",
];

/// Un nom d'API plutôt qu'un nom qu'on prononce : préfixe de deux majuscules ou plus suivi d'une
/// minuscule (« NSString », « CGImage », « AVAssetWriter »), ou plusieurs traits d'union
/// (« Co-Authored-By »).
fn nom_d_api(mot: &str) -> bool {
    let majuscules_en_tete = mot.chars().take_while(|c| c.is_uppercase()).count();
    let suivie_de_minuscule = mot.chars().nth(majuscules_en_tete).is_some_and(char::is_lowercase);
    (majuscules_en_tete >= 2 && suivie_de_minuscule) || mot.matches('-').count() >= 2
}

/// Une ligne de code (on ne dit pas « NSString » à voix haute) : symboles de programmation,
/// mots-clés en début de ligne, ou forte densité de ponctuation.
fn ressemble_a_du_code(texte: &str) -> bool {
    let t = texte.trim_start();
    const DEBUTS: &[&str] = &[
        "fn ", "let ", "use ", "pub ", "impl ", "struct ", "enum ", "mod ", "const ", "static ", "#[", "//", "match ",
        "if let", "for ", "while ", "return ", "import ", "def ", "class ", "function ", "var ", "#include", "$ ",
    ];
    if DEBUTS.iter().any(|d| t.starts_with(d)) || ["::", "->", "=>", "();", "&mut", "</"].iter().any(|m| t.contains(m)) {
        return true;
    }
    let n = t.chars().count().max(1);
    let symboles = t.chars().filter(|c| "{}()[];:<>=&|*#$_`\\".contains(*c)).count();
    symboles * 100 / n >= 8
}

/// Les noms propres vus souvent : un mot (4 à 24 lettres) écrit avec une majuscule dans au moins
/// 80 % de ses apparitions, et vu au moins `minimum` fois, hors code et chemins de fichiers.
/// Les plus fréquents d'abord.
pub fn noms_propres(textes: &[String], minimum: usize, combien: usize) -> Vec<String> {
    // Pour chaque mot (en minuscules) : apparitions totales, avec majuscule, et la forme la plus vue.
    let mut comptes: HashMap<String, (usize, usize, HashMap<String, usize>)> = HashMap::new();
    for texte in textes {
        if ressemble_a_du_code(texte) {
            continue;
        }
        // Les chemins de fichiers (« /Users/…/Library ») ne sont pas des mots dits : on les retire.
        let sans_chemins: String = texte
            .split_whitespace()
            .filter(|m| !m.contains('/') && !m.contains('\\'))
            .collect::<Vec<_>>()
            .join(" ");
        // Le premier mot d'une ligne a une majuscule de début de phrase : il ne dit rien.
        let mots = sans_chemins
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .map(|m| m.trim_matches('-'))
            .filter(|m| !m.is_empty())
            .skip(1);
        for mot in mots {
            let n = mot.chars().count();
            if !(4..=24).contains(&n) || mot.chars().all(|c| c.is_ascii_digit()) || nom_d_api(mot) {
                continue;
            }
            let minuscule = mot.to_lowercase();
            let entree = comptes.entry(minuscule).or_default();
            entree.0 += 1;
            if mot.chars().next().is_some_and(char::is_uppercase) {
                entree.1 += 1;
                *entree.2.entry(mot.to_string()).or_default() += 1;
            }
        }
    }
    let mut retenus: Vec<(usize, String)> = comptes
        .into_iter()
        .filter(|(minuscule, (total, majuscules, _))| {
            *majuscules >= minimum && *majuscules * 5 >= *total * 4 && !MOTS_COURANTS.contains(&minuscule.as_str())
        })
        .filter_map(|(_, (_, majuscules, formes))| {
            // Un mot tout en majuscules (« ERREUR », « TODO ») n'est pas un nom propre.
            let (forme, _) = formes.into_iter().max_by_key(|(_, n)| *n)?;
            (!forme.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase)).then_some((majuscules, forme))
        })
        .collect();
    retenus.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    retenus.into_iter().take(combien).map(|(_, f)| f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_propres_vus_souvent() {
        let mut textes: Vec<String> = Vec::new();
        for _ in 0..6 {
            textes.push("Réunion avec Camille sur Proxmox et WhiteRose".into());
            textes.push("Fichier Édition Affichage Fenêtre Aide".into());
            textes.push("ERREUR : le Serveur ne répond pas".into());
        }
        // « serveur » écrit aussi en minuscules : pas un nom propre.
        for _ in 0..6 {
            textes.push("le serveur redémarre".into());
        }
        textes.push("Bastille vue une seule fois".into());
        for _ in 0..8 {
            textes.push("let image: Retained<NSString> = NSString::from_str(texte);".into());
            textes.push("ouvre /Users/moi/Library/Application Support et Contents".into());
        }
        let noms = noms_propres(&textes, 5, 10);
        assert!(noms.contains(&"Camille".to_string()));
        assert!(noms.contains(&"Proxmox".to_string()));
        assert!(noms.contains(&"WhiteRose".to_string()));
        assert!(!noms.iter().any(|n| n == "Fichier" || n == "Réunion" || n == "Serveur" || n == "ERREUR"), "{noms:?}");
        assert!(!noms.contains(&"Bastille".to_string()), "vu une seule fois");
        assert!(!noms.iter().any(|n| n == "NSString" || n == "Retained" || n == "Library"), "{noms:?}");
        assert!(nom_d_api("CVPixelBuffer") && nom_d_api("Co-Authored-By") && !nom_d_api("WhiteRose") && !nom_d_api("YouTube"));
    }
}
