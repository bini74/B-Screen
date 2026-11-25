//! Les corrections de Julien (phase 7.8) : quand il corrige Ask (« non, c'était mardi »), Ask
//! commence sa réponse par « Retenu : … », le fait corrigé en une phrase. B-Screen garde ce fait
//! et le redonne en tête des questions suivantes, où il prime sur le reste.

use bscreen_store::Base;

use crate::heures::{lisible, plier};

/// Le début d'une ligne de fait retenu.
pub const RETENU: &str = "Retenu";

/// Les faits retenus dans une réponse : ses lignes « Retenu : … » (gras ou tiret autour admis).
pub fn extraire(reponse: &str) -> Vec<String> {
    reponse
        .lines()
        .filter_map(|ligne| {
            let ligne = ligne.trim().trim_start_matches(['-', '•', '*', ' ']).trim_start_matches("**");
            let reste = ligne.strip_prefix(RETENU)?;
            let reste = reste.trim_start_matches("**").trim_start();
            let fait = reste.strip_prefix(':')?.trim().trim_start_matches("**").trim();
            (fait.chars().count() >= 8).then(|| fait.to_string())
        })
        .collect()
}

/// La question ressemble-t-elle à une correction de la réponse précédente (« non, c'était mardi ») ?
/// Seule une question de suite peut l'être ; Ask ne retient « Retenu : … » que dans ce cas (le
/// modèle local en écrit parfois sans raison : 3 fois sur 7 au banc d'essai du 22/11).
pub fn est_une_correction(question: &str) -> bool {
    const MARQUES: [&str; 14] = [
        " non ", " pas ", " c'etait ", " n'etait ", " etait ", " plutot ", " en fait ", " faux ", " fausse ", " erreur ",
        " trompe", " corrig", " au lieu ", " mais ",
    ];
    let q = format!(" {} ", plier(question).replace(['?', '!', '.', ',', ';', ':', '«', '»', '"'], " "));
    MARQUES.iter().any(|m| q.contains(m))
}

/// Écarte d'une réponse qui arrive morceau par morceau ses lignes « Retenu : … » (quand la question
/// n'était pas une correction). Le reste passe au fil ; seul le début d'une ligne attend le temps de
/// savoir ce qu'elle est.
#[derive(Debug, Default)]
pub struct SansRetenu {
    debut: String,
    ligne: Ligne,
    /// Des lignes ont été écartées.
    pub ecartees: usize,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum Ligne {
    #[default]
    Debut,
    Gardee,
    Ecartee,
}

impl SansRetenu {
    /// Ce qui peut être montré de ce morceau.
    pub fn passer(&mut self, morceau: &str) -> String {
        let mut sortie = String::new();
        for c in morceau.chars() {
            match self.ligne {
                Ligne::Debut if c == '\n' => {
                    sortie.push_str(&std::mem::take(&mut self.debut));
                    sortie.push('\n');
                }
                Ligne::Debut => {
                    self.debut.push(c);
                    match juger(&self.debut) {
                        Some(true) => {
                            self.debut.clear();
                            self.ligne = Ligne::Ecartee;
                            self.ecartees += 1;
                        }
                        Some(false) => {
                            sortie.push_str(&std::mem::take(&mut self.debut));
                            self.ligne = Ligne::Gardee;
                        }
                        None => {}
                    }
                }
                Ligne::Gardee => {
                    sortie.push(c);
                    if c == '\n' {
                        self.ligne = Ligne::Debut;
                    }
                }
                Ligne::Ecartee => {
                    if c == '\n' {
                        self.ligne = Ligne::Debut;
                    }
                }
            }
        }
        sortie
    }

    /// La fin de la réponse : ce qui attendait encore.
    pub fn finir(&mut self) -> String {
        let reste = std::mem::take(&mut self.debut);
        let ligne = std::mem::take(&mut self.ligne);
        if ligne == Ligne::Debut && juger(&reste) != Some(true) { reste } else { String::new() }
    }
}

/// Ce début de ligne est-il une ligne « Retenu : » (vrai), sûrement pas (faux), ou trop court pour
/// le dire (rien) ? Tirets, puces et gras autour admis, comme dans [`extraire`].
fn juger(debut: &str) -> Option<bool> {
    const MOT: &str = "retenu";
    let t = debut.trim_start_matches([' ', '\t', '-', '•', '*']).to_lowercase();
    if t.is_empty() {
        return (debut.chars().count() > 8).then_some(false);
    }
    if t.len() < MOT.len() {
        return if MOT.starts_with(&t) { None } else { Some(false) };
    }
    if !t.starts_with(MOT) {
        return Some(false);
    }
    let reste = t[MOT.len()..].trim_start_matches(['*', ' ']);
    if reste.is_empty() { None } else { Some(reste.starts_with(':')) }
}

/// La section du contexte : les dernières corrections, les plus récentes d'abord, en `max`
/// caractères au plus. Rien s'il n'y en a pas.
pub fn section(base: &Base, max: usize) -> Option<String> {
    let corrections = base.corrections(30).ok()?;
    if corrections.is_empty() {
        return None;
    }
    let mut texte = String::from("## Corrections de Julien (elles priment sur tout le reste)\n");
    for c in corrections {
        let ligne = format!("• {} (corrigé le {})\n", c.texte, lisible(c.cree_ms).get(..10).unwrap_or_default());
        if texte.chars().count() + ligne.chars().count() > max {
            break;
        }
        texte.push_str(&ligne);
    }
    Some(texte.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lignes_retenues() {
        let reponse = "Retenu : le dîner avec Camille était le mardi 17/11, pas le lundi.\n\
                       D'accord, c'était donc mardi [écran 17/11 20:14].\n\
                       - **Retenu :** Camille est son associé.\n\
                       Retenu sans deux-points\nRetenu : ok";
        assert_eq!(
            extraire(reponse),
            vec![
                "le dîner avec Camille était le mardi 17/11, pas le lundi.".to_string(),
                "Camille est son associé.".to_string()
            ]
        );
        assert!(extraire("Rien à retenir ici.").is_empty());
    }

    #[test]
    fn corrections_et_lignes_retenu_ecartees() {
        assert!(est_une_correction("Non, c'était mardi"));
        assert!(est_une_correction("c'était plutôt Camille"));
        assert!(!est_une_correction("et avant ?"));
        // Au fil des morceaux, coupés n'importe où : les lignes « Retenu » partent, le reste passe.
        let reponse = "Retenu : rien du tout.\nLe 19/11 à 17:40.\n- **Retenu :** autre\nRetenue par Camille.\nFin";
        for taille in [1, 3, 7, 50] {
            let mut filtre = SansRetenu::default();
            let mut vu = String::new();
            let caracteres: Vec<char> = reponse.chars().collect();
            for morceau in caracteres.chunks(taille) {
                vu.push_str(&filtre.passer(&morceau.iter().collect::<String>()));
            }
            vu.push_str(&filtre.finir());
            assert_eq!(vu, "Le 19/11 à 17:40.\nRetenue par Camille.\nFin", "morceaux de {taille}");
            assert_eq!(filtre.ecartees, 2);
        }
        // Une réponse qui finit sur un début ambigu le garde.
        let mut filtre = SansRetenu::default();
        assert_eq!(format!("{}{}", filtre.passer("Ok\nRet"), filtre.finir()), "Ok\nRet");
    }

    #[test]
    fn section_des_corrections() {
        let base = Base::en_memoire().unwrap();
        assert_eq!(section(&base, 1_000), None);
        base.ajouter_correction(1_000, "Le dîner était mardi.", None).unwrap();
        base.ajouter_correction(2_000, "Camille est son associé.", None).unwrap();
        let texte = section(&base, 1_000).unwrap();
        assert!(texte.starts_with("## Corrections de Julien"));
        assert!(texte.find("Camille").unwrap() < texte.find("dîner").unwrap(), "{texte}");
        // Faute de place : la plus récente seulement.
        let court = section(&base, 115).unwrap();
        assert!(court.contains("Camille") && !court.contains("dîner"), "{court}");
    }
}
