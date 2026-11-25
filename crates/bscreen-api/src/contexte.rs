//! Le contexte qu'Ask prépare sur le Mac avant d'interroger le modèle : la période dont parle la
//! question, ce qui était à l'écran et qui parlait, ce qui a été dit, les textes proches par le
//! sens et les noms cités. Avec lui, le modèle répond d'un seul tour, sans allers-retours vers la
//! mémoire (c'est ce qui rendait Ask lent) — et un modèle local peut répondre aussi.

use bscreen_store::Base;

use crate::heures::{lisible, periode_dans_question, plier};
use crate::mcp::{activite, chercher, chercher_images_entre, chercher_sens_entre, recherches, recherches_parmi, voix_de};

/// Taille maximale de chaque partie (caractères).
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub resumes: usize,
    pub activite: usize,
    pub voix: usize,
    pub sens: usize,
    pub exacts: usize,
    pub recherches: usize,
    /// Les échanges avec une personne citée (voix et messages).
    pub personnes: usize,
    /// Les corrections de Julien, en tête.
    pub corrections: usize,
    /// Les moments dont l'image ressemble à la question (ce qui se voyait sans être écrit).
    pub images: usize,
}

impl Budget {
    /// Pour Claude : de quoi répondre d'un seul tour (≈ 7 000 jetons au plus ; ≈ 8 500 avec les
    /// échanges d'une personne citée, qui remplacent alors « ce qui a été dit »).
    pub const COMPLET: Budget =
        Budget { resumes: 4_000, activite: 12_000, voix: 6_000, sens: 4_000, exacts: 2_500, recherches: 1_500, personnes: 12_000, corrections: 2_000, images: 1_500 };
    /// Pour le modèle local (≈ 210 jetons lus par seconde sur un M1 Pro) : l'essentiel, les
    /// résumés de la nuit d'abord (≈ 2 500 jetons, ≈ 12 s de lecture).
    pub const LOCAL: Budget =
        Budget { resumes: 2_000, activite: 1_800, voix: 900, sens: 600, exacts: 400, recherches: 300, personnes: 1_500, corrections: 600, images: 500 };
}

/// Ce que l'app a trouvé par ses modèles pour une question : les textes proches par le sens et
/// les images qui ressemblent, (identifiant, similarité), du plus proche au moins proche.
#[derive(Debug, Clone, Copy, Default)]
pub struct Proches<'a> {
    pub textes: Option<&'a [(i64, f32)]>,
    pub images: Option<&'a [(i64, f32)]>,
}

/// Le contexte d'une question, pour Claude ; `proches` : calculés par l'app (les modèles y sont
/// déjà chargés).
pub fn preparer(base: &Base, question: &str, maintenant_ms: i64, proches: Proches) -> String {
    preparer_avec(base, question, maintenant_ms, proches, Budget::COMPLET)
}

/// Le contexte d'une question, dans un budget donné.
pub fn preparer_avec(base: &Base, question: &str, maintenant_ms: i64, proches: Proches, budget: Budget) -> String {
    let images = proches.images;
    let proches = proches.textes;
    let mut parties: Vec<String> = Vec::new();
    // Les corrections de Julien d'abord : elles priment sur tout le reste.
    if let Some(corrections) = crate::corrections::section(base, budget.corrections) {
        parties.push(corrections);
    }
    let periode = periode_dans_question(question, maintenant_ms);
    let q = plier(question);
    match &periode {
        Some((debut, fin, libelle)) => {
            parties.push(format!("Période comprise : {libelle} ({} → {}).", lisible(*debut), lisible(*fin)));
            // Les épisodes déjà résumés (la nuit, sur le Mac) : la vue d'ensemble, avant le détail.
            let resumes = crate::episodes::resumes(base, *debut, *fin);
            if !resumes.is_empty() {
                parties.push(section("Résumés des épisodes (écrits la nuit sur le Mac)", &resumes, budget.resumes));
            }
            let tout = compacter_heures(&raccourcir_adresses(&activite(base, *debut, *fin)));
            let (lignes, filtre) = filtrer_sur_la_question(&tout, &q);
            let titre = match filtre {
                Some(mots) => format!("Activité à l'écran (et qui parlait), seulement « {mots} »"),
                None => "Activité à l'écran (et qui parlait)".to_string(),
            };
            parties.push(section(&titre, &lignes, budget.activite));
            // Une personne citée (« avec Camille ») : ses échanges de voix, les deux côtés, et ses
            // messages, avec qui a écrit quoi.
            let personnes = crate::personnes::citees(base, question, *debut, *fin);
            for p in &personnes {
                let echanges = crate::personnes::echanges(base, p, *debut, *fin, budget.personnes);
                parties.push(section(&format!("Échanges avec {}", p.nom), &echanges, budget.personnes + 200));
            }
            // Ce qui a été dit : si la question parle de voix, d'appel, de conversation, ou de Julien.
            let personne = personne_citee(base, &q);
            if personnes.is_empty() && (personne.is_some() || parle_de_voix(&q)) {
                let dit = voix_de(base, *debut, *fin, personne.as_deref());
                parties.push(section("Ce qui a été dit", &dit, budget.voix));
            }
            if parle_de_recherches(&q) {
                let liste = compacter_heures(&recherches(base, *debut, *fin));
                parties.push(section("Recherches tapées", &liste, budget.recherches));
            }
        }
        None => {
            parties.push("Pas de période précise dans la question : tout l'historique est concerné.".to_string());
            // Une personne citée sans période : ses échanges de la dernière semaine.
            let depuis = maintenant_ms - 7 * 86_400_000;
            for p in crate::personnes::citees(base, question, depuis, maintenant_ms) {
                let echanges = crate::personnes::echanges(base, &p, depuis, maintenant_ms, budget.personnes);
                parties.push(section(&format!("Échanges avec {} (7 derniers jours)", p.nom), &echanges, budget.personnes + 200));
            }
            // « Le poisson que je cherchais » : les recherches tapées du dernier mois qui en
            // parlent (sinon les plus récentes).
            if parle_de_recherches(&q) {
                let depuis = maintenant_ms - 30 * 86_400_000;
                let (titre, liste) = match recherches_parmi(base, depuis, maintenant_ms, &mots_utiles(&q)) {
                    Some((mots, liste)) => (format!("Recherches tapées qui parlent de « {mots} » (30 derniers jours)"), liste),
                    None => ("Recherches tapées (30 derniers jours)".to_string(), recherches(base, depuis, maintenant_ms)),
                };
                parties.push(section(&titre, &compacter_heures(&liste), budget.recherches));
            }
        }
    }
    // Ce qui se voyait sans être écrit (« la voiture rouge ») : seulement si la question parle de
    // voir. Pas sur la seule ressemblance : SigLIP lit aussi le texte affiché, et « on a parlé de quoi
    // avec Camille ? » ressemble fort à un écran où « Camille » est écrit (0,155 le 22/11), ce que la
    // recherche dans les textes donne déjà.
    if let Some(images) = images.filter(|i| !i.is_empty() && parle_d_images(&q)) {
        let bornes = periode.as_ref().map(|(d, f, _)| (*d, *f));
        let vues = compacter_heures(&raccourcir_adresses(&chercher_images_entre(base, images, question, 6, bornes)));
        parties.push(section_en_tete(
            "Images qui ressemblent à la question (ce qui se voyait à l'écran ; à vérifier, une image peut se tromper)",
            &vues,
            budget.images,
        ));
    }
    if let Some(proches) = proches.filter(|p| !p.is_empty()) {
        let bornes = periode.as_ref().map(|(d, f, _)| (*d, *f));
        let sens = compacter_heures(&raccourcir_adresses(&chercher_sens_entre(base, proches, question, 12, bornes)));
        // Sans période, pas d'activité ni de résumés : leur place va aux textes proches.
        let place = if periode.is_none() { budget.sens + budget.activite } else { budget.sens };
        parties.push(section_en_tete("Textes proches par le sens", &sens, place));
    }
    let termes = termes_exacts(question);
    if !termes.is_empty() {
        let mut exacts = String::new();
        for terme in termes.iter().take(3) {
            exacts.push_str(&chercher(base, terme, 8));
            exacts.push('\n');
        }
        let exacts = compacter_heures(&raccourcir_adresses(&exacts));
        parties.push(section_en_tete("Mots exacts", &exacts, budget.exacts));
    }
    parties.join("\n\n")
}

/// Les mots de la question qui disent de quoi elle parle (4 lettres et plus, ni mots vides, ni
/// verbes de recherche) : « c'était quoi le poisson que je cherchais ? » → « poisson ».
fn mots_utiles(q: &str) -> Vec<String> {
    const VIDES: [&str; 16] = [
        "quoi", "quel", "quelle", "quelles", "quels", "avec", "dans", "pour", "cette", "est-ce", "etait", "c'etait",
        "faisais", "j'ai", "sais", "plus",
    ];
    const VERBES: [&str; 5] = ["cherch", "recherch", "trouv", "tape", "regard"];
    q.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '\''))
        .filter(|m| m.chars().count() >= 4 && !VIDES.contains(m) && !VERBES.iter().any(|v| m.starts_with(v)))
        .map(str::to_string)
        .collect()
}

/// Les mots de la question qui nomment une app ou un site présents dans l'activité (« youtube »,
/// « messages ») : seules ces lignes sont gardées (sinon une soirée entière est coupée au milieu).
fn filtrer_sur_la_question(activite: &str, q: &str) -> (String, Option<String>) {
    const VIDES: [&str; 12] = [
        "quoi", "quel", "quelle", "quelles", "quels", "avec", "dans", "pour", "cette", "est-ce", "etait", "faisais",
    ];
    let lignes: Vec<&str> = activite.lines().collect();
    let lignes_pliees: Vec<String> = lignes.iter().map(|l| plier(l)).collect();
    let mots: Vec<&str> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|m| m.chars().count() >= 4 && !VIDES.contains(m))
        // Un mot présent dans peu de lignes nomme quelque chose (une app, un site, un titre).
        .filter(|m| {
            let n = lignes_pliees.iter().filter(|l| l.contains(m)).count();
            n > 0 && n < lignes.len().max(2)
        })
        .collect();
    if mots.is_empty() || lignes.len() < 20 {
        return (activite.to_string(), None);
    }
    let gardees: Vec<&str> = lignes
        .iter()
        .zip(&lignes_pliees)
        .enumerate()
        // La première ligne (l'en-tête « Activité du … ») reste.
        .filter(|(i, (_, pliee))| *i == 0 || mots.iter().any(|m| pliee.contains(m)))
        .map(|(_, (l, _))| *l)
        .collect();
    (gardees.join("\n"), Some(mots.join(" », « ")))
}

/// « ven. 20/11 21:49:12 → 21:50:03 » → « 20/11 21:49 → 21:50 » : la soirée tient.
pub(crate) fn compacter_heures(texte: &str) -> String {
    const JOURS: [&str; 7] = ["lun. ", "mar. ", "mer. ", "jeu. ", "ven. ", "sam. ", "dim. "];
    let mut sortie = String::with_capacity(texte.len());
    for (i, ligne) in texte.lines().enumerate() {
        if i > 0 {
            sortie.push('\n');
        }
        let mut l = ligne.to_string();
        for j in JOURS {
            l = l.replace(j, "");
        }
        // Les secondes : « HH:MM:SS » → « HH:MM ».
        let octets = l.as_bytes();
        let mut propre = String::with_capacity(l.len());
        let mut k = 0;
        while k < l.len() {
            let suite = &octets[k..];
            let est_heure = suite.len() >= 8
                && suite[..8].iter().enumerate().all(|(n, c)| if n == 2 || n == 5 { *c == b':' } else { c.is_ascii_digit() });
            if est_heure {
                propre.push_str(&l[k..k + 5]);
                k += 8;
            } else {
                let c = l[k..].chars().next().unwrap_or(' ');
                propre.push(c);
                k += c.len_utf8();
            }
        }
        sortie.push_str(&propre);
    }
    sortie
}

/// « https://www.youtube.com/watch?v=… » → « youtube.com » : le site suffit, et la soirée tient.
pub(crate) fn raccourcir_adresses(texte: &str) -> String {
    let mut sortie = String::with_capacity(texte.len());
    for (i, ligne) in texte.lines().enumerate() {
        if i > 0 {
            sortie.push('\n');
        }
        let morceaux: Vec<String> = ligne
            .split(' ')
            .map(|mot| {
                // Toute adresse « schéma://… » (https, vscode-file…) ; « file:/// » n'a pas d'hôte.
                let schema = mot
                    .find("://")
                    .filter(|&i| i > 0 && mot[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)));
                let Some(i) = schema else { return mot.to_string() };
                let reste = &mot[i + 3..];
                let hote = reste.split(['/', '?', '#', ')']).next().unwrap_or(reste);
                let hote = if hote.is_empty() { &mot[..i] } else { hote.strip_prefix("www.").unwrap_or(hote) };
                // La parenthèse qui ferme après l'adresse (« (Safari · … · https://…) ») reste.
                let ferme = mot.ends_with(')') && mot.matches('(').count() < mot.matches(')').count();
                format!("{hote}{}", if ferme { ")" } else { "" })
            })
            .collect();
        sortie.push_str(&morceaux.join(" "));
    }
    sortie
}

fn section(titre: &str, contenu: &str, max: usize) -> String {
    format!("## {titre}\n{}", couper(contenu.trim(), max))
}

/// Une liste classée (la plus pertinente d'abord) : on garde le début, par lignes « • » entières
/// avec leur suite (l'échange autour d'un passage), pas la fin comme pour une liste dans le temps.
fn section_en_tete(titre: &str, contenu: &str, max: usize) -> String {
    let mut blocs: Vec<String> = Vec::new();
    for ligne in contenu.trim().lines() {
        match blocs.last_mut() {
            Some(bloc) if !ligne.starts_with("• ") && bloc.starts_with("• ") => {
                bloc.push('\n');
                bloc.push_str(ligne);
            }
            _ => blocs.push(ligne.to_string()),
        }
    }
    let mut garde = String::new();
    let mut laisses = 0;
    for bloc in &blocs {
        if laisses == 0 && garde.chars().count() + bloc.chars().count() < max {
            garde.push_str(bloc);
            garde.push('\n');
        } else if bloc.starts_with("• ") {
            laisses += 1;
        }
    }
    if laisses > 0 {
        garde.push_str(&format!("… ({laisses} de plus, moins proches)"));
    }
    format!("## {titre}\n{}", garde.trim_end())
}

/// Garde le début et la fin d'un texte trop long (les lignes entières).
pub(crate) fn couper(texte: &str, max: usize) -> String {
    if texte.chars().count() <= max {
        return texte.to_string();
    }
    let lignes: Vec<&str> = texte.lines().collect();
    let mut debut: Vec<&str> = Vec::new();
    let mut fin: Vec<&str> = Vec::new();
    let mut taille = 0;
    let (mut i, mut j) = (0usize, lignes.len());
    // Alternativement une ligne du début, une de la fin, tant que ça tient.
    while i < j {
        let l = lignes[i];
        if taille + l.chars().count() > max {
            break;
        }
        taille += l.chars().count() + 1;
        debut.push(l);
        i += 1;
        if i >= j {
            break;
        }
        let l = lignes[j - 1];
        if taille + l.chars().count() > max {
            break;
        }
        taille += l.chars().count() + 1;
        fin.push(l);
        j -= 1;
    }
    fin.reverse();
    format!("{}\n… ({} lignes passées) …\n{}", debut.join("\n"), j - i, fin.join("\n"))
}

fn parle_de_voix(q: &str) -> bool {
    [
        "parl", "dit ", "dis ", "disait", "dire", "discut", "appel", "conversation", "voix", "entendu", "raconte",
        "reunion", "visio", "facetime", "telephone", "oral",
    ]
    .iter()
    .any(|m| q.contains(m))
}

/// La question parle-t-elle de ce qui se voyait ? (« j'ai vu », une photo, une couleur…) Mot par
/// mot : « pouvoir » ne parle pas de voir, ni « ouvert » de vert.
fn parle_d_images(q: &str) -> bool {
    const MOTS: [&str; 22] = [
        "vu", "vue", "vus", "vues", "voir", "vois", "voit", "voyais", "voyait", "image", "images", "photo", "photos",
        "video", "videos", "film", "films", "scene", "couleur", "couleurs", "dessin", "logo",
    ];
    const COULEURS: [&str; 13] = [
        "rouge", "bleu", "vert", "jaune", "noir", "blanc", "blanch", "rose", "violet", "orange", "gris", "marron", "dore",
    ];
    const DEBUTS: [&str; 5] = ["regard", "montr", "affich", "ressembl", "photograph"];
    q.split(|c: char| !c.is_alphanumeric()).filter(|m| !m.is_empty()).any(|m| {
        MOTS.contains(&m)
            || DEBUTS.iter().any(|d| m.starts_with(d))
            // « rouge », « bleue », « vertes », « blanche », « dorés » : la couleur et ses accords.
            || COULEURS.iter().any(|c| m.strip_prefix(c).is_some_and(|reste| ["", "e", "s", "es", "he", "hes", "te", "tes"].contains(&reste)))
    })
}

fn parle_de_recherches(q: &str) -> bool {
    ["cherch", "recherch", "google", "tape", "youtube", "regard"].iter().any(|m| q.contains(m))
}

/// Une voix connue nommée dans la question (« avec Camille ») ; « je », « moi » : la voix de Julien.
fn personne_citee(base: &Base, q: &str) -> Option<String> {
    let mots: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).filter(|m| !m.is_empty()).collect();
    for l in base.locuteurs().ok()? {
        if let Some(nom) = l.nom.as_deref()
            && !l.moi
            && mots.contains(&plier(nom).as_str())
        {
            return Some(nom.to_string());
        }
    }
    let parle_de_moi = ["j'ai dit", "ai-je dit", "j'ai parle", "ai-je parle", "ce que je disais"]
        .iter()
        .any(|m| q.contains(m));
    parle_de_moi.then(|| "toi".to_string())
}

/// Les noms propres et passages entre guillemets de la question, cherchés tels quels.
fn termes_exacts(question: &str) -> Vec<String> {
    let mut termes: Vec<String> = Vec::new();
    // Entre guillemets : « … », "…".
    for (ouvre, ferme) in [('«', '»'), ('"', '"'), ('“', '”')] {
        let mut reste = question;
        while let Some(i) = reste.find(ouvre) {
            let apres = &reste[i + ouvre.len_utf8()..];
            let Some(j) = apres.find(ferme) else { break };
            let terme = apres[..j].trim();
            if terme.chars().count() >= 2 {
                termes.push(terme.to_string());
            }
            reste = &apres[j + ferme.len_utf8()..];
        }
    }
    // Les mots en majuscule (sauf le premier de la question), de 3 lettres et plus.
    for (i, mot) in question
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|m| !m.is_empty())
        .enumerate()
    {
        if i > 0
            && mot.chars().count() >= 3
            && mot.chars().next().is_some_and(char::is_uppercase)
            && !termes.iter().any(|t| t.contains(mot))
        {
            termes.push(mot.to_string());
        }
    }
    termes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_et_guillemets() {
        assert_eq!(
            termes_exacts("Qu'est-ce qu'Camille disait sur « Proxmox Backup » mardi ?"),
            vec!["Proxmox Backup".to_string(), "Camille".to_string()]
        );
        assert_eq!(termes_exacts("Quand ai-je parlé avec Camille de WhiteRose ?"), vec!["Camille", "WhiteRose"]);
        assert!(termes_exacts("quelle page sur les dauphins ?").is_empty());
    }

    #[test]
    fn activite_filtree_et_adresses_courtes() {
        let mut activite = String::from("Activité du … :\n");
        for i in 0..30 {
            activite.push_str(&format!("• 21:{i:02} · Safari · « Vidéo {i} » · https://www.youtube.com/watch?v={i}\n"));
            activite.push_str(&format!("• 21:{i:02} · VSCodium · « main.rs »\n"));
        }
        let courte = raccourcir_adresses(&activite);
        assert_eq!(
            compacter_heures("• ven. 20/11 21:49:12 → 21:50:03 · Safari"),
            "• 20/11 21:49 → 21:50 · Safari"
        );
        assert!(courte.contains("« Vidéo 3 » · youtube.com") && !courte.contains("watch?v"), "{courte}");
        assert_eq!(
            raccourcir_adresses("(Safari · « Escargot » · https://fr.wikipedia.org/wiki/Escargot) — « texte »"),
            "(Safari · « Escargot » · fr.wikipedia.org) — « texte »"
        );
        assert_eq!(
            raccourcir_adresses("(VSCodium · « x » · vscode-file://vscode-app/Applications/VSCodium.app/a.html) file:///Users/a"),
            "(VSCodium · « x » · vscode-app) file"
        );
        let (gardees, mots) = filtrer_sur_la_question(&courte, &plier("Qu'est-ce que je regardais sur YouTube ce soir ?"));
        assert_eq!(mots.as_deref(), Some("youtube"));
        assert!(gardees.lines().count() == 31 && !gardees.contains("VSCodium"), "{gardees}");
        // Sans nom d'app ou de site dans la question : tout reste.
        let (tout, rien) = filtrer_sur_la_question(&courte, &plier("Qu'ai-je fait ce soir ?"));
        assert!(rien.is_none() && tout.contains("VSCodium"));
    }

    #[test]
    fn couper_garde_debut_et_fin() {
        let texte: String = (0..100).map(|i| format!("ligne {i}\n")).collect();
        let court = couper(&texte, 100);
        assert!(court.starts_with("ligne 0\n") && court.trim_end().ends_with("ligne 99"), "{court}");
        assert!(court.contains("lignes passées"));
        assert_eq!(couper("court", 100), "court");
    }

    #[test]
    fn questions_qui_parlent_d_images() {
        assert!(parle_d_images(&plier("J'ai vu quand la voiture rouge ?")));
        assert!(parle_d_images(&plier("la photo du chien")));
        assert!(!parle_d_images(&plier("On a parlé de quoi hier avec Camille ?")));
        assert!(!parle_d_images(&plier("mollusque")));
        assert!(parle_d_images(&plier("la robe bleue")) && parle_d_images(&plier("une chemise blanche")));
        assert!(!parle_d_images(&plier("est-ce que je vais pouvoir ? c'était ouvert ? j'adore")));
    }

    #[test]
    fn sujets_de_la_question() {
        assert!(parle_de_voix(&plier("De quoi a-t-on parlé hier ?")));
        assert!(parle_de_voix(&plier("l'appel avec Léa")));
        assert!(!parle_de_voix(&plier("quelle série ce soir ?")));
        assert!(parle_de_recherches(&plier("qu'est-ce que j'ai cherché sur Google ?")));
    }
}
