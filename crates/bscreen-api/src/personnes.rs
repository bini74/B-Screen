//! Les personnes (phase 7.5) : une voix nommée (« Camille ») et les conversations qui portent son
//! nom (une fenêtre de Messages « Camille Nicolas Guttin »). Pour « on a parlé de quoi avec
//! Camille ? » : les échanges de voix avec les deux côtés (pas seulement ce qu'il a dit), et les
//! messages échangés, avec qui a écrit quoi.

use std::collections::HashSet;

use bscreen_store::{Base, TexteVu, Voix};

use crate::heures::{lisible, plier};
use crate::mcp::{dit_avec, nom_de_voix, origine_de_voix};

/// Les apps de conversation, où le titre de la fenêtre est le nom de la personne (débuts
/// d'identifiant).
pub const APPS_DE_CONVERSATION: [&str; 7] = [
    "com.apple.MobileSMS",
    "com.apple.FaceTime",
    "net.whatsapp.WhatsApp",
    "desktop.WhatsApp",
    "org.whispersystems.signal-desktop",
    "ru.keepcoder.Telegram",
    "com.facebook.archon",
];
/// Des mots de titre qui ne sont pas des noms (« Messages », « Nouveau message », « Coup d'œil »).
const PAS_DES_NOMS: [&str; 12] = [
    "messages", "message", "nouveau", "nouvelle", "whatsapp", "facetime", "signal", "telegram", "messenger",
    "coup", "d'oeil", "appel",
];
/// Deux passages d'une même voix à moins de 5 min : le même échange.
const TROU_ECHANGE_MS: i64 = 5 * 60_000;
/// Ce qui a été dit juste avant et juste après en fait partie.
const BORD_ECHANGE_MS: i64 = 60_000;
/// Deux moments d'une même conversation écrite à moins de 10 min : la même séance.
const TROU_SEANCE_MS: i64 = 10 * 60_000;
/// Plus large que ça (en points) : la barre de titre, pas une bulle.
const LARGEUR_TITRE: f64 = 800.0;
/// Une bulle à moins de ça d'un bord de la conversation y est collée.
const PRES_DU_BORD: f64 = 24.0;
/// En dessous de cette largeur de conversation, on ne sait pas qui a écrit (une seule bulle vue).
const LARGEUR_CONVERSATION: f64 = 600.0;
/// La part minimale d'un échange quand la place manque (son début et sa fin), en caractères.
const PART_MIN: usize = 700;

/// Une personne : son nom (celui de sa voix, sinon le titre de la conversation), sa voix si
/// elle est nommée, et les titres de ses conversations écrites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Personne {
    pub nom: String,
    pub voix: Option<i64>,
    pub conversations: Vec<String>,
}

/// Les mots d'un nom ou d'une phrase, pliés (minuscules, sans accents), d'au moins 3 lettres.
fn mots(texte: &str) -> Vec<String> {
    plier(texte)
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|m| m.chars().count() >= 3)
        .map(str::to_string)
        .collect()
}

/// Les personnes citées dans la question : une voix nommée dont le nom y est (pas celle de
/// Julien), ou une conversation de la période dont un mot du titre y est (« Maman »). Une voix
/// et une conversation qui partagent un mot (« Camille », « Camille Nicolas Guttin ») sont la
/// même personne.
pub fn citees(base: &Base, question: &str, debut_ms: i64, fin_ms: i64) -> Vec<Personne> {
    let mots_question = mots(question);
    let titres: Vec<String> = base
        .titres_de_fenetres(&APPS_DE_CONVERSATION, debut_ms, fin_ms)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, titre)| titre)
        .collect();
    let du_nom = |titre: &str| -> Vec<String> {
        mots(titre).into_iter().filter(|m| !PAS_DES_NOMS.contains(&m.as_str())).collect()
    };
    let mut personnes: Vec<Personne> = Vec::new();
    for l in base.locuteurs().unwrap_or_default() {
        let Some(nom) = l.nom.filter(|_| !l.moi) else { continue };
        let mots_voix = mots(&nom);
        if !mots_voix.iter().any(|m| mots_question.contains(m)) {
            continue;
        }
        let conversations = titres
            .iter()
            .filter(|t| du_nom(t).iter().any(|m| mots_voix.contains(m)))
            .cloned()
            .collect();
        personnes.push(Personne { nom, voix: Some(l.id), conversations });
    }
    for titre in &titres {
        if personnes.iter().any(|p| p.conversations.contains(titre)) {
            continue;
        }
        if du_nom(titre).iter().any(|m| mots_question.contains(m)) {
            personnes.push(Personne {
                nom: titre.clone(),
                voix: None,
                conversations: vec![titre.clone()],
            });
        }
    }
    personnes
}

/// Les échanges avec une personne pendant une période, dans l'ordre du temps : de voix (au
/// téléphone en haut-parleur, à côté de Julien, en appel sur le Mac) et écrits. En `max`
/// caractères environ : chaque échange a sa part (son début et sa fin) ; s'il en reste trop, les
/// plus anciens sont laissés (les plus récents comptent le plus).
pub fn echanges(base: &Base, personne: &Personne, debut_ms: i64, fin_ms: i64, max: usize) -> String {
    let mut parties: Vec<(i64, String)> = Vec::new();
    if let Some(voix) = personne.voix {
        parties.extend(echanges_de_voix(base, voix, debut_ms, fin_ms));
    }
    for titre in &personne.conversations {
        parties.extend(messages(base, titre, &personne.nom, debut_ms, fin_ms));
    }
    if parties.is_empty() {
        return format!(
            "Aucun échange avec {} entre {} et {}.",
            personne.nom,
            lisible(debut_ms),
            lisible(fin_ms)
        );
    }
    parties.sort_by_key(|(debut, _)| *debut);
    let longueurs: Vec<usize> = parties.iter().map(|(_, p)| p.chars().count()).collect();
    let mut gardees: Vec<String> = parties
        .into_iter()
        .zip(repartir(&longueurs, max))
        .map(|((_, p), part)| crate::contexte::couper(&p, part))
        .collect();
    let mut laisses = 0;
    while gardees.len() > 1 && gardees.iter().map(|p| p.chars().count() + 2).sum::<usize>() > max {
        gardees.remove(0);
        laisses += 1;
    }
    let mut texte = gardees.join("\n\n");
    if laisses > 0 {
        texte = format!("({laisses} échange(s) plus ancien(s) non montré(s), faute de place.)\n\n{texte}");
    }
    texte
}

/// La place de chaque échange : les courts (quelques messages) gardent tout, les longs (un appel
/// d'une demi-heure) se partagent le reste ; jamais moins de [`PART_MIN`].
fn repartir(longueurs: &[usize], max: usize) -> Vec<usize> {
    let mut parts = vec![0; longueurs.len()];
    let mut ordre: Vec<usize> = (0..longueurs.len()).collect();
    ordre.sort_by_key(|&i| longueurs[i]);
    let mut reste = max;
    for (k, &i) in ordre.iter().enumerate() {
        let equitable = reste / (longueurs.len() - k);
        parts[i] = longueurs[i].min(equitable.max(PART_MIN));
        reste = reste.saturating_sub(parts[i]);
    }
    parts
}

/// « 21:34 ».
fn heure(ms: i64) -> String {
    lisible(ms).get(11..16).unwrap_or_default().to_string()
}

/// Chaque fois qu'on entend cette voix (passages à moins de 5 min l'un de l'autre) : tout ce qui
/// a été dit autour, par elle comme par Julien. Jamais en navigation privée, ni une vidéo.
fn echanges_de_voix(base: &Base, voix: i64, debut_ms: i64, fin_ms: i64) -> Vec<(i64, String)> {
    let (de, a) = (debut_ms - BORD_ECHANGE_MS, fin_ms + BORD_ECHANGE_MS);
    let Ok(passages) = base.voix_entre(de, a) else {
        return Vec::new();
    };
    let verrou = base.verrou_prive(de, a).ok();
    let ids: Vec<i64> = passages.iter().map(|p| p.id).collect();
    let deduits = base.deduits_de(&ids).unwrap_or_default();
    let garde = |p: &&Voix| {
        verrou.as_ref().is_some_and(|v| !v.pendant(p.debut_ms, p.fin_ms))
            && !(p.source != 0 && !p.appel && p.locuteur_id.is_none())
    };
    let mut plages: Vec<(i64, i64)> = Vec::new();
    for p in passages
        .iter()
        .filter(|p| p.locuteur_id == Some(voix) && p.debut_ms >= debut_ms && p.debut_ms <= fin_ms)
        .filter(garde)
    {
        match plages.last_mut() {
            Some((_, fin)) if p.debut_ms - *fin <= TROU_ECHANGE_MS => *fin = (*fin).max(p.fin_ms),
            _ => plages.push((p.debut_ms, p.fin_ms)),
        }
    }
    plages
        .into_iter()
        .map(|(debut, fin)| {
            let lignes: Vec<String> = passages
                .iter()
                .filter(|p| p.debut_ms >= debut - BORD_ECHANGE_MS && p.debut_ms <= fin + BORD_ECHANGE_MS)
                .filter(garde)
                .map(|p| {
                    let ou = if p.source != 0 { origine_de_voix(p) } else { "" };
                    format!("  {} · {}{ou} : {}", heure(p.debut_ms), nom_de_voix(p), dit_avec(p, &deduits))
                })
                .collect();
            let minutes = ((fin - debut) as f64 / 60_000.0).round().max(1.0);
            let texte = format!(
                "Échange de voix du {} à {} ({minutes} min) :\n{}",
                lisible(debut),
                heure(fin),
                lignes.join("\n")
            );
            (debut, texte)
        })
        .collect()
}

/// Une conversation écrite pendant une période : chaque séance (moments à moins de 10 min
/// l'un de l'autre) et ses bulles, dans l'ordre où elles apparaissent, avec qui les a écrites :
/// collées à droite, Julien ; à gauche, l'autre. Le titre, les libellés du milieu (l'heure,
/// « Distribué ») et ce qui est en train d'être tapé (le champ qui a le focus) sont laissés.
fn messages(base: &Base, titre: &str, nom: &str, debut_ms: i64, fin_ms: i64) -> Vec<(i64, String)> {
    let Ok(images) = base.images_de_fenetre(&APPS_DE_CONVERSATION, titre, debut_ms, fin_ms) else {
        return Vec::new();
    };
    let mut seances: Vec<Vec<(i64, u32)>> = Vec::new();
    for image in images {
        match seances.last_mut() {
            Some(seance) if seance.last().is_some_and(|(q, _)| image.0 - q <= TROU_SEANCE_MS) => seance.push(image),
            _ => seances.push(vec![image]),
        }
    }
    seances
        .into_iter()
        .filter_map(|seance| {
            let vus: Vec<TexteVu> = seance
                .iter()
                .flat_map(|(quand, ecran)| base.textes_vus_a(*ecran, *quand, 1_500).unwrap_or_default())
                .filter(|t| t.source == 0 && !t.focus && t.cadre[2] < LARGEUR_TITRE)
                .filter(|t| !t.texte.trim().is_empty() && t.texte.trim() != titre)
                .collect();
            let gauche = vus.iter().map(|t| t.cadre[0]).fold(f64::INFINITY, f64::min);
            let droite = vus.iter().map(|t| t.cadre[0] + t.cadre[2]).fold(f64::NEG_INFINITY, f64::max);
            if droite - gauche < LARGEUR_CONVERSATION {
                return None;
            }
            let mut deja: HashSet<&str> = HashSet::new();
            let lignes: Vec<String> = vus
                .iter()
                .filter_map(|t| {
                    let qui = if t.cadre[0] + t.cadre[2] >= droite - PRES_DU_BORD {
                        "Toi"
                    } else if t.cadre[0] <= gauche + PRES_DU_BORD {
                        nom
                    } else {
                        return None;
                    };
                    deja.insert(t.texte.trim()).then(|| format!("  {qui} : « {} »", t.texte.trim()))
                })
                .collect();
            let (premier, dernier) = (seance.first()?.0, seance.last()?.0);
            (!lignes.is_empty()).then(|| {
                let texte = format!(
                    "Messages avec « {titre} », du {} à {} :\n{}",
                    lisible(premier),
                    heure(dernier),
                    lignes.join("\n")
                );
                (premier, texte)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bscreen_store::{NouveauMorceau, NouvelleApparition, NouvelleImage};

    /// Une conversation Messages avec Camille (une bulle reçue, une envoyée, le titre, un libellé
    /// au milieu, un brouillon), un appel avec lui en haut-parleur, et une vidéo pendant ce temps.
    fn base_d_essai() -> Base {
        let base = Base::en_memoire().unwrap();
        let morceau = base
            .nouveau_morceau(&NouveauMorceau {
                ecran: 1,
                fichier: "a.mp4",
                debut_ms: 0,
                largeur: 1,
                hauteur: 1,
            })
            .unwrap();
        for quand in [100_000, 160_000] {
            let image = base
                .ajouter_image(&NouvelleImage {
                    morceau,
                    quand_ms: quand,
                    position_ms: 0,
                    ecran: 1,
                    app_bundle: Some("com.apple.MobileSMS"),
                    app_nom: Some("Messages"),
                    aire: 1.0,
                    declenchee: true,
                })
                .unwrap();
            base.noter_fenetre(image, Some("Camille Nicolas Guttin"), None).unwrap();
        }
        let textes = [
            ("Camille Nicolas Guttin", [426.0, 302.0, 1272.0, 20.0], false),
            ("On se voit au bar ce soir ?", [773.0, 400.0, 300.0, 20.0], false),
            ("Oui, vers 20 h", [1450.0, 440.0, 228.0, 20.0], false),
            ("Distribué", [1185.0, 470.0, 75.0, 14.0], false),
            ("je tape encore", [804.0, 1052.0, 836.0, 20.0], true),
        ];
        let nouvelles: Vec<NouvelleApparition> = textes
            .iter()
            .map(|(texte, cadre, focus)| NouvelleApparition {
                texte: base.texte_id(texte).unwrap(),
                ecran: 1,
                quand_ms: 100_000,
                source: 0,
                vu: true,
                focus: *focus,
                cadre: *cadre,
            })
            .collect();
        base.noter_apparitions(&nouvelles, &[], 100_000).unwrap();
        // L'appel : Camille au micro (le téléphone en haut-parleur), Julien, et une vidéo.
        let moi = base.nouveau_locuteur(&[1u8; 8], true).unwrap();
        let camille = base.nouveau_locuteur(&[2u8; 8], false).unwrap();
        base.nommer_locuteur(camille, "Camille").unwrap();
        for (source, debut, voix, texte) in [
            (0u8, 300_000, Some(camille), "tu as réservé ?"),
            (0, 312_000, Some(moi), "pas encore"),
            (1, 315_000, None, "dans cet épisode"),
            (0, 330_000, Some(camille), "je m'en occupe"),
        ] {
            let v = base.ajouter_voix(source, debut, debut + 5_000, "a.aac", None, voix).unwrap();
            let passage = base.voix_entre(debut, debut + 1).unwrap().into_iter().find(|p| p.id == v).unwrap();
            base.noter_transcription(&passage, texte, "[]", "essai").unwrap();
        }
        base
    }

    #[test]
    fn la_place_va_aux_longs_echanges() {
        // Trois séances de messages courtes et un long appel, pour 3 000 caractères.
        assert_eq!(repartir(&[200, 300, 250, 9_000], 3_000), vec![200, 300, 250, 2_250]);
        // Assez de place : chacun garde tout.
        assert_eq!(repartir(&[200, 800], 3_000), vec![200, 800]);
    }

    #[test]
    fn camille_cite_par_sa_voix_et_sa_conversation() {
        let base = base_d_essai();
        let personnes = citees(&base, "On a parlé de quoi avec Camille ?", 0, 1_000_000);
        assert_eq!(personnes.len(), 1);
        assert_eq!(personnes[0].nom, "Camille");
        assert!(personnes[0].voix.is_some());
        assert_eq!(personnes[0].conversations, vec!["Camille Nicolas Guttin".to_string()]);
        // Personne de cité : rien.
        assert!(citees(&base, "Qu'ai-je fait dans Messages ?", 0, 1_000_000).is_empty());
        // Par un autre mot du titre, sans voix nommée de ce nom.
        let par_le_nom = citees(&base, "et avec Guttin ?", 0, 1_000_000);
        assert_eq!(par_le_nom.len(), 1);
        assert_eq!((par_le_nom[0].voix, par_le_nom[0].nom.as_str()), (None, "Camille Nicolas Guttin"));
    }

    #[test]
    fn echanges_avec_qui_a_dit_et_ecrit_quoi() {
        let base = base_d_essai();
        let camille = citees(&base, "avec camille", 0, 1_000_000).remove(0);
        let texte = echanges(&base, &camille, 0, 1_000_000, 10_000);
        // Les deux côtés de l'appel, pas la vidéo.
        assert!(texte.contains("Camille : « tu as réservé ? »"), "{texte}");
        assert!(texte.contains("Toi : « pas encore »"), "{texte}");
        assert!(!texte.contains("épisode"), "{texte}");
        // Les messages : qui a écrit quoi ; ni le titre, ni le libellé, ni le brouillon.
        assert!(texte.contains("Camille : « On se voit au bar ce soir ? »"), "{texte}");
        assert!(texte.contains("Toi : « Oui, vers 20 h »"), "{texte}");
        assert!(!texte.contains("Distribué") && !texte.contains("je tape encore"), "{texte}");
        // Dans l'ordre du temps : les messages (100 s) avant l'appel (300 s).
        assert!(texte.find("Messages avec").unwrap() < texte.find("Échange de voix").unwrap(), "{texte}");
        // Faute de place, le plus ancien part, pas le plus récent.
        let serre = echanges(&base, &camille, 0, 1_000_000, 300);
        assert!(serre.contains("Échange de voix") && !serre.contains("Messages avec"), "{serre}");
        assert!(serre.starts_with("(1 échange(s) plus ancien(s)"), "{serre}");
    }
}
