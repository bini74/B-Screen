//! Serveur MCP en lecture seule (phase 9.2), sur l'entrée / la sortie standard :
//! `bscreen mcp`, lancé par Claude Code (ou un autre agent).
//!
//! La base est ouverte en lecture seule ; les moments en navigation privée ne sortent jamais ;
//! le texte est celui déjà masqué à l'enregistrement (aucun secret).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bscreen_store::{Base, Image};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::heures::{lisible, periode, plier};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Chercher {
    /// Les mots cherchés (accents et majuscules ignorés, débuts de mots acceptés : « dauphin » trouve « dauphins »).
    pub requete: String,
    /// Nombre de résultats au plus (20 par défaut, 100 au plus).
    pub limite: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Periode {
    /// Début : « aujourd'hui », « hier », « 2026-11-19 », « 2026-11-19 21:30 », « 21:30 »… (par défaut : il y a une heure).
    pub debut: Option<String>,
    /// Fin, mêmes formes (par défaut : maintenant). Une journée seule (« hier ») couvre toute la journée.
    pub fin: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Paroles {
    /// Début : « aujourd'hui », « hier », « 2026-11-19 », « 2026-11-19 21:30 », « 21:30 »… (par défaut : il y a une heure).
    pub debut: Option<String>,
    /// Fin, mêmes formes (par défaut : maintenant). Une journée seule (« hier ») couvre toute la journée.
    pub fin: Option<String>,
    /// Seulement ce qu'a dit cette personne (nom de la voix : « Camille », « Toi »…), sans majuscules ni accents.
    pub personne: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AvecQui {
    /// La personne : le nom de sa voix (« Camille ») ou de sa conversation (« Maman »).
    pub personne: String,
    /// Début, mêmes formes que pour les autres outils (par défaut : il y a 7 jours).
    pub debut: Option<String>,
    /// Fin (par défaut : maintenant).
    pub fin: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Images {
    /// Ce qui se voyait, en français ou en anglais : « une voiture rouge », « un chien sur la plage », « un gâteau d'anniversaire ».
    pub requete: String,
    /// Début (facultatif), mêmes formes que pour les autres outils.
    pub debut: Option<String>,
    /// Fin (facultative ; par défaut : maintenant).
    pub fin: Option<String>,
    /// Nombre de moments au plus (10 par défaut, 50 au plus).
    pub limite: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Moment {
    /// L'instant : « 2026-11-19 21:30 », « 21:30:05 » (aujourd'hui), ou RFC 3339.
    pub quand: String,
}

const INSTRUCTIONS: &str = "Mémoire de l'écran et des voix de Julien, enregistrée par B-Screen sur son Mac \
(lecture seule). Outils : `chercher` (un mot ou une phrase vus à l'écran ou dits), `activite` (apps, fenêtres et \
pages d'une période), `voix` (ce qui a été dit, avec qui parlait, éventuellement par une personne), \
`recherches` (ce que Julien a tapé dans Google, YouTube… : utile pour « le … que je cherchais »), `ecran` (le texte \
affiché à un instant), `chercher_dans_les_images` (ce qui se voyait sans être écrit : une photo, un objet, une couleur). \
Heures locales. Les moments en navigation privée ne sont jamais donnés ; les secrets sont masqués (••••••).";

/// Le serveur : la base (lecture seule) et ses outils.
#[derive(Clone)]
pub struct Memoire {
    base: Arc<Mutex<Base>>,
    sens: Option<ChercheurSens>,
    images: Option<ChercheurImages>,
    tool_router: ToolRouter<Self>,
}

fn maintenant_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

fn bornes(p: &Periode) -> std::result::Result<(i64, i64), String> {
    periode_de(p.debut.as_deref(), p.fin.as_deref())
}

/// Une période (début et fin tapés comme pour les outils) en ms ; par défaut la dernière heure.
pub fn periode_de(debut: Option<&str>, fin: Option<&str>) -> std::result::Result<(i64, i64), String> {
    let p = Periode {
        debut: debut.map(str::to_string),
        fin: fin.map(str::to_string),
    };
    let maintenant = maintenant_ms();
    let debut = match p.debut.as_deref() {
        Some(t) => periode(t, maintenant).ok_or_else(|| format!("début incompris : « {t} »"))?.0,
        None => maintenant - 3_600_000,
    };
    let fin = match (p.fin.as_deref(), p.debut.as_deref()) {
        (Some(t), _) => periode(t, maintenant).ok_or_else(|| format!("fin incomprise : « {t} »"))?.1,
        // « hier » seul : toute la journée.
        (None, Some(t)) => match periode(t, maintenant) {
            Some((d, f)) if f > d => f,
            _ => maintenant,
        },
        (None, None) => maintenant,
    };
    Ok((debut, fin.max(debut)))
}

/// Un extrait d'au plus `n` caractères autour du premier mot cherché.
fn extrait(texte: &str, requete: &str, n: usize) -> String {
    let texte = texte.split_whitespace().collect::<Vec<_>>().join(" ");
    let caracteres: Vec<char> = texte.chars().collect();
    if caracteres.len() <= n {
        return texte;
    }
    let minuscule = texte.to_lowercase();
    let position = requete
        .split_whitespace()
        .filter_map(|m| minuscule.find(&m.to_lowercase()))
        .min()
        .map(|octet| minuscule[..octet].chars().count())
        .unwrap_or(0);
    let debut = position.saturating_sub(n / 3).min(caracteres.len() - n);
    let morceau: String = caracteres[debut..debut + n].iter().collect();
    format!(
        "{}{morceau}{}",
        if debut > 0 { "…" } else { "" },
        if debut + n < caracteres.len() { "…" } else { "" }
    )
}

fn description_image(image: &Image) -> String {
    let mut parties = Vec::new();
    if let Some(app) = image.app_nom.as_deref() {
        parties.push(app.to_string());
    }
    if let Some(fenetre) = image.fenetre.as_deref().filter(|f| !f.is_empty()) {
        parties.push(format!("« {} »", extrait(fenetre, "", 90)));
    }
    if let Some(url) = image.url.as_deref() {
        parties.push(url.to_string());
    }
    parties.join(" · ")
}

/// La recherche par le sens, fournie par la couche système (modèle Core ML) : les textes proches
/// d'une question, (texte, similarité), du plus proche au moins proche.
pub type ChercheurSens = Arc<dyn Fn(&Base, &str, usize) -> Vec<(i64, f32)> + Send + Sync>;

/// La recherche dans les images, fournie par la couche système (SigLIP 2 en Core ML) : les images
/// proches d'une question, (image, cosinus), de la plus proche à la moins proche.
pub type ChercheurImages = ChercheurSens;

/// Ne sont gardées que les images à moins de ça de la plus ressemblante. La ressemblance n'a pas
/// d'échelle fixe : SigLIP lit aussi le texte affiché, et un écran de code monte à 0,17 pour « le
/// code Rust » quand la bonne photo d'une voiture rouge reste à 0,12 (vraies captures, 22/11).
pub const ECART_IMAGES: f32 = 0.02;

/// Les moments dont l'image ressemble à la question, sans les moments privés : un par endroit
/// (même app, fenêtre et page dans le quart d'heure : un film en donnerait des dizaines), le plus
/// ressemblant d'abord ; seulement pendant `periode` si donnée.
pub fn chercher_images_entre(
    base: &Base,
    proches: &[(i64, f32)],
    requete: &str,
    limite: usize,
    periode: Option<(i64, i64)>,
) -> String {
    struct Endroit {
        cle: String,
        quand_ms: i64,
        cosinus: f32,
        contexte: String,
        autres: usize,
    }
    let limite = limite.clamp(1, 50);
    let ids: Vec<i64> = proches.iter().map(|p| p.0).collect();
    let images: std::collections::HashMap<i64, Image> =
        base.images_par_id(&ids).unwrap_or_default().into_iter().map(|i| (i.id, i)).collect();
    let mut endroits: Vec<Endroit> = Vec::new();
    for &(id, cosinus) in proches {
        let Some(image) = images.get(&id) else { continue };
        if image.privee || periode.is_some_and(|(debut, fin)| image.quand_ms < debut || image.quand_ms > fin) {
            continue;
        }
        let contexte = description_image(image);
        let cle = format!("{contexte}|{}", image.quand_ms / QUART_D_HEURE_MS);
        if let Some(e) = endroits.iter_mut().find(|e| e.cle == cle) {
            e.autres += 1;
        } else if endroits.len() < limite {
            endroits.push(Endroit { cle, quand_ms: image.quand_ms, cosinus, contexte, autres: 0 });
        }
    }
    if let Some(meilleure) = endroits.first().map(|e| e.cosinus) {
        endroits.retain(|e| e.cosinus >= meilleure - ECART_IMAGES);
    }
    if endroits.is_empty() {
        return format!("Aucune image ne ressemble à « {requete} ».");
    }
    let lignes: Vec<String> = endroits
        .iter()
        .map(|e| {
            format!(
                "• {} — à l'écran, ressemblance {:.2}{}{}",
                lisible(e.quand_ms),
                e.cosinus,
                if e.contexte.is_empty() { String::new() } else { format!(" ({})", e.contexte) },
                if e.autres > 0 { format!(", et {} autre(s) image(s) de ce moment", e.autres) } else { String::new() }
            )
        })
        .collect();
    format!(
        "{} moment(s) dont l'image ressemble à « {requete} », du plus ressemblant au moins ressemblant :\n{}",
        lignes.len(),
        lignes.join("\n")
    )
}

/// Recherche par le sens, sans les moments privés : même présentation que `chercher`.
pub fn chercher_sens(base: &Base, proches: &[(i64, f32)], requete: &str, limite: usize) -> String {
    chercher_sens_entre(base, proches, requete, limite, None)
}

/// Comme [`chercher_sens`], mais seulement ce qui était là pendant `periode` (si donnée).
pub fn chercher_sens_entre(
    base: &Base,
    proches: &[(i64, f32)],
    requete: &str,
    limite: usize,
    periode: Option<(i64, i64)>,
) -> String {
    let limite = limite.clamp(1, 100);
    let par_texte = if periode.is_some() { 6 } else { 2 };
    let trouvees = match base.trouvailles_de_textes(proches, par_texte, limite * 8) {
        Ok(t) => t,
        Err(e) => return format!("recherche impossible : {e}"),
    };
    let lignes = en_lignes(base, trouvees, "", limite, periode, true);
    if lignes.is_empty() {
        format!("Rien de proche de « {requete} ».")
    } else {
        format!("{} endroit(s) proches de « {requete} », du plus proche au moins proche :\n{}", lignes.len(), lignes.join("\n"))
    }
}

/// Recherche plein texte, sans les moments privés.
pub fn chercher(base: &Base, requete: &str, limite: usize) -> String {
    let limite = limite.clamp(1, 100);
    let trouvees = match base.chercher(requete, limite * 8) {
        Ok(t) => t,
        Err(e) => return format!("recherche impossible : {e}"),
    };
    let lignes = en_lignes(base, trouvees, requete, limite, None, false);
    if lignes.is_empty() {
        format!("Rien trouvé pour « {requete} ».")
    } else {
        format!("{} endroit(s) pour « {requete} », les plus pertinents d'abord :\n{}", lignes.len(), lignes.join("\n"))
    }
}

/// Au plus ce nombre de textes d'une même page sur sa ligne.
const TEXTES_PAR_PAGE: usize = 4;
/// Deux textes d'une même page (même app, fenêtre et adresse) dans le même quart d'heure : le même endroit.
const QUART_D_HEURE_MS: i64 = 15 * 60_000;

/// Les trouvailles d'une recherche en lignes, regroupées par endroit, le plus pertinent d'abord :
/// une page en une ligne avec ses textes trouvés ; un passage entendu avec l'échange autour (qui a
/// dit quoi juste avant et juste après : la réponse à une question est souvent dans le passage
/// suivant). Jamais les moments privés. Au plus `limite` textes ; `sens` : la proximité est
/// donnée, et un même texte n'est montré qu'une fois.
fn en_lignes(
    base: &Base,
    trouvees: Vec<bscreen_store::Trouvaille>,
    requete: &str,
    limite: usize,
    periode: Option<(i64, i64)>,
    sens: bool,
) -> Vec<String> {
    struct Endroit {
        cle: String,
        entete: String,
        textes: Vec<String>,
        echange: Vec<String>,
    }
    let mut endroits: Vec<Endroit> = Vec::new();
    // Les passages déjà montrés dans un échange (leur début).
    let mut passages_montres: Vec<i64> = Vec::new();
    let mut vus: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut textes = 0;
    for t in trouvees {
        if textes >= limite {
            break;
        }
        if periode.is_some_and(|(debut, fin)| t.fin_ms < debut || t.debut_ms > fin) {
            continue;
        }
        let image = base.image_avant(t.debut_ms.max(t.fin_ms.min(t.debut_ms + 1))).ok().flatten();
        if image.as_ref().is_some_and(|i| i.privee) || base.privee_pendant(t.debut_ms, t.fin_ms).unwrap_or(true) {
            continue;
        }
        let proximite = if sens { format!(", proximité {:.2}", t.score) } else { String::new() };
        if t.ecran == 0 {
            if passages_montres.contains(&t.debut_ms) {
                continue;
            }
            let Some((montres, lignes)) = echange_autour(base, t.debut_ms) else { continue };
            passages_montres.extend(montres);
            textes += 1;
            endroits.push(Endroit {
                cle: String::new(),
                entete: format!("• {} — entendu{proximite} ; l'échange autour :", lisible(t.debut_ms)),
                textes: Vec::new(),
                echange: lignes,
            });
            continue;
        }
        let ou = if t.vu { "à l'écran" } else { "texte caché" };
        let contexte = image.as_ref().map(description_image).unwrap_or_default();
        let cle = format!("{ou}|{contexte}|{}", t.debut_ms / QUART_D_HEURE_MS);
        let deja = if sens { t.texte.clone() } else { format!("{cle}|{}", t.texte) };
        if !vus.insert(deja) {
            continue;
        }
        let texte = format!("« {} »", extrait(&t.texte, requete, 240));
        match endroits.iter_mut().find(|e| e.cle == cle) {
            Some(e) if e.textes.len() >= TEXTES_PAR_PAGE => continue,
            Some(e) => e.textes.push(texte),
            None => endroits.push(Endroit {
                entete: format!(
                    "• {} — {ou}{proximite}{}",
                    lisible(t.debut_ms),
                    if contexte.is_empty() { String::new() } else { format!(" ({contexte})") }
                ),
                cle,
                textes: vec![texte],
                echange: Vec::new(),
            }),
        }
        textes += 1;
    }
    endroits
        .into_iter()
        .map(|e| {
            if e.echange.is_empty() {
                format!("{} — {}", e.entete, e.textes.join(" · "))
            } else {
                format!("{}\n{}", e.entete, e.echange.join("\n"))
            }
        })
        .collect()
}

/// Ce qui a été dit juste avant et juste après un passage (à moins d'une minute, 3 passages de
/// chaque côté au plus), et par qui ; pas les vidéos (sauf le passage lui-même, s'il en est une)
/// ni les moments privés. Rend les passages montrés (leur début) et les lignes.
fn echange_autour(base: &Base, debut_ms: i64) -> Option<(Vec<i64>, Vec<String>)> {
    const AUTOUR_MS: i64 = 60_000;
    const DE_CHAQUE_COTE: usize = 3;
    let (de, a) = (debut_ms - AUTOUR_MS, debut_ms + AUTOUR_MS);
    let passages = base.voix_entre(de, a).ok()?;
    let verrou = base.verrou_prive(de, a).ok()?;
    let video = |p: &bscreen_store::Voix| p.source != 0 && !p.appel && p.locuteur_id.is_none();
    let trouve = passages.iter().find(|p| p.debut_ms == debut_ms)?;
    let gardes: Vec<&bscreen_store::Voix> = if video(trouve) {
        vec![trouve]
    } else {
        passages
            .iter()
            .filter(|p| p.debut_ms == debut_ms || (!video(p) && p.texte.as_deref().is_some_and(|t| !t.trim().is_empty())))
            .filter(|p| !verrou.pendant(p.debut_ms, p.fin_ms))
            .collect()
    };
    let i = gardes.iter().position(|p| p.debut_ms == debut_ms)?;
    let (premier, dernier) = (i.saturating_sub(DE_CHAQUE_COTE), (i + DE_CHAQUE_COTE + 1).min(gardes.len()));
    let ids: Vec<i64> = gardes[premier..dernier].iter().map(|p| p.id).collect();
    let deduits = base.deduits_de(&ids).unwrap_or_default();
    let lignes = gardes[premier..dernier]
        .iter()
        .map(|p| {
            let ou = if p.source != 0 { origine_de_voix(p) } else { "" };
            format!(
                "  {} · {}{ou} : {}",
                lisible(p.debut_ms).get(11..).unwrap_or_default(),
                nom_de_voix(p),
                dit_avec(p, &deduits)
            )
        })
        .collect();
    Some((gardes[premier..dernier].iter().map(|p| p.debut_ms).collect(), lignes))
}

/// Ce qui était à l'écran pendant une période : apps, fenêtres, pages, regroupées tant qu'elles restent.
pub fn activite(base: &Base, debut: i64, fin: i64) -> String {
    let images = match base.images_entre(debut, fin) {
        Ok(i) => i,
        Err(e) => return format!("lecture impossible : {e}"),
    };
    // Qui parlait pendant chaque plage (jamais pendant une navigation privée) : des épisodes.
    // Le verrou privé se lit sur les images déjà chargées (une requête par passage était trop lente).
    let privee_pendant = |d: i64, f: i64| {
        let i = images.partition_point(|im| im.quand_ms <= d);
        let avant_privee = i.checked_sub(1).is_some_and(|j| images[j].privee);
        avant_privee || images[i..].iter().take_while(|im| im.quand_ms <= f).any(|im| im.privee)
    };
    let passages: Vec<bscreen_store::Voix> = base
        .voix_entre(debut, fin)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !privee_pendant(p.debut_ms, p.fin_ms))
        .collect();
    let mut lignes: Vec<String> = Vec::new();
    let mut courant: Option<(String, i64, i64)> = None;
    let fermer = |c: Option<(String, i64, i64)>, lignes: &mut Vec<String>| {
        if let Some((quoi, a, b)) = c {
            // Les voix du micro (Julien, quelqu'un près de lui) d'un côté, celles du son du Mac
            // (vidéos, appels) de l'autre : sinon une vidéo passe pour une conversation.
            let (mut micro, mut mac): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
            if quoi != "(navigation privée)" {
                for p in passages.iter().filter(|p| p.debut_ms <= b + 2_000 && p.fin_ms >= a) {
                    let qui = nom_de_voix(p);
                    let liste = if du_son_du_mac(p) { &mut mac } else { &mut micro };
                    if !liste.contains(&qui) {
                        liste.push(qui);
                    }
                }
            }
            let mut voix = String::new();
            if !micro.is_empty() {
                voix.push_str(&format!(" · voix : {}", micro.join(", ")));
            }
            if !mac.is_empty() {
                voix.push_str(&format!(" · son du Mac : {}", mac.join(", ")));
            }
            lignes.push(format!("• {} → {} · {quoi}{voix}", lisible(a), &lisible(b)[10..]));
        }
    };
    for image in &images {
        let quoi = if image.privee {
            "(navigation privée)".to_string()
        } else {
            description_image(image)
        };
        match courant.as_mut() {
            Some((q, _, b)) if *q == quoi && image.quand_ms - *b < 300_000 => *b = image.quand_ms,
            _ => {
                fermer(courant.take(), &mut lignes);
                courant = Some((quoi, image.quand_ms, image.quand_ms));
            }
        }
    }
    fermer(courant, &mut lignes);
    if lignes.is_empty() {
        return format!("Rien d'enregistré entre {} et {}.", lisible(debut), lisible(fin));
    }
    let total = lignes.len();
    if total > 300 {
        lignes.drain(..total - 300);
    }
    format!(
        "Activité du {} au {} ({total} plage(s){}) :\n{}",
        lisible(debut),
        lisible(fin),
        if total > 300 { ", les 300 dernières" } else { "" },
        lignes.join("\n")
    )
}

/// Qui parle, en court : la voix de Julien reste « Toi », même s'il l'a nommée (un nom donné par
/// erreur, « camille » le 21/11, faisait croire aux modèles qu'Camille était là). Sans voix : « voix
/// pas reconnue » (pas sûr de qui), ou « vidéo » (le son du Mac hors appel).
pub(crate) fn nom_de_voix(p: &bscreen_store::Voix) -> String {
    match (&p.locuteur, p.moi) {
        (_, true) => "Toi".to_string(),
        (Some(nom), false) => nom.clone(),
        (None, false) if p.source != 0 && !p.appel => "vidéo".to_string(),
        (None, false) => "voix pas reconnue".to_string(),
    }
}

/// Ce qui a été dit, entre guillemets, et sa version explicite déduite la nuit s'il y en a une
/// (« « Il est derrière le truc. » (déduit : le chargeur est derrière le support d'écran) »).
pub(crate) fn dit_avec(p: &bscreen_store::Voix, deduits: &std::collections::HashMap<i64, (String, f32)>) -> String {
    let dit = match p.texte.as_deref().map(str::trim) {
        Some("") => "(voix pas comprise)".to_string(),
        Some(t) => format!("« {t} »"),
        None => "(pas encore transcrit)".to_string(),
    };
    match deduits.get(&p.id) {
        Some((explicite, _)) => format!("{dit} (déduit : {explicite})"),
        None => dit,
    }
}

/// D'où venait la voix, à écrire après son nom : rien pour le micro, « (appel) » pour la
/// personne au bout du fil d'un appel sur le Mac, « (son du Mac) » pour une vidéo.
pub(crate) fn origine_de_voix(p: &bscreen_store::Voix) -> &'static str {
    match (p.source, p.appel) {
        (0, _) => "",
        (_, true) => " (appel)",
        _ => " (son du Mac)",
    }
}

/// Une voix venue du son du Mac (vidéo, appel) plutôt que du micro ?
fn du_son_du_mac(p: &bscreen_store::Voix) -> bool {
    p.source != 0 && !p.moi
}

/// Ce qui a été dit pendant une période, avec qui parlait.
pub fn voix(base: &Base, debut: i64, fin: i64) -> String {
    voix_de(base, debut, fin, None)
}

/// Ce qui a été dit pendant une période, éventuellement par une seule personne.
pub fn voix_de(base: &Base, debut: i64, fin: i64, personne: Option<&str>) -> String {
    let personne = personne.map(plier).filter(|p| !p.trim().is_empty());
    let passages = match base.voix_entre(debut, fin) {
        Ok(p) => p,
        Err(e) => return format!("lecture impossible : {e}"),
    };
    // Le verrou privé se lit sur les images de la période, chargées une fois (une requête par
    // passage était trop lente pour un après-midi de voix).
    let images = base.images_entre(debut - 60 * 60_000, fin).unwrap_or_default();
    let privee_pendant = |d: i64, f: i64| {
        let i = images.partition_point(|im| im.quand_ms <= d);
        let avant_privee = i.checked_sub(1).is_some_and(|j| images[j].privee);
        avant_privee || images[i..].iter().take_while(|im| im.quand_ms <= f).any(|im| im.privee)
    };
    let ids: Vec<i64> = passages.iter().map(|p| p.id).collect();
    let deduits = base.deduits_de(&ids).unwrap_or_default();
    let mut lignes = Vec::new();
    for p in passages {
        if privee_pendant(p.debut_ms, p.fin_ms) {
            continue;
        }
        let qui = nom_de_voix(&p);
        if let Some(cherchee) = personne.as_deref()
            && !plier(&qui).contains(cherchee.trim())
            && !p.locuteur.as_deref().is_some_and(|nom| plier(nom).contains(cherchee.trim()))
            && !(p.moi && ["toi", "moi", "julien", "je"].contains(&cherchee.trim()))
        {
            continue;
        }
        let dit = dit_avec(&p, &deduits);
        let ou = if du_son_du_mac(&p) { origine_de_voix(&p) } else { "" };
        lignes.push(format!("• {} · {qui}{ou} : {dit}", lisible(p.debut_ms)));
    }
    if lignes.is_empty() {
        format!(
            "Aucune voix{} entre {} et {}.",
            personne.map(|p| format!(" de « {p} »")).unwrap_or_default(),
            lisible(debut),
            lisible(fin)
        )
    } else {
        format!("{} passage(s) de voix :\n{}", lignes.len(), lignes.join("\n"))
    }
}

/// Ce que Julien a tapé dans un moteur ou un site (Google, YouTube…) pendant une période.
pub fn recherches(base: &Base, debut: i64, fin: i64) -> String {
    let liste = match base.recherches_entre(debut, fin) {
        Ok(l) => l,
        Err(e) => return format!("lecture impossible : {e}"),
    };
    let lignes: Vec<String> = liste
        .iter()
        .filter(|r| !base.image_avant(r.quand_ms).ok().flatten().is_some_and(|i| i.privee))
        .map(|r| format!("• {} · {} : « {} »", lisible(r.quand_ms), r.moteur, r.requete))
        .collect();
    if lignes.is_empty() {
        format!("Aucune recherche tapée entre {} et {}.", lisible(debut), lisible(fin))
    } else {
        format!("{} recherche(s) tapée(s) :\n{}", lignes.len(), lignes.join("\n"))
    }
}

/// Les recherches tapées d'une période dont la requête (ou le moteur) contient l'un des `mots`
/// (déjà pliés) : (les mots trouvés, la liste) ; rien si aucune n'en parle.
pub fn recherches_parmi(base: &Base, debut: i64, fin: i64, mots: &[String]) -> Option<(String, String)> {
    if mots.is_empty() {
        return None;
    }
    let mut trouves: Vec<&str> = Vec::new();
    let mut lignes: Vec<String> = Vec::new();
    for r in base.recherches_entre(debut, fin).ok()? {
        if base.image_avant(r.quand_ms).ok().flatten().is_some_and(|i| i.privee) {
            continue;
        }
        let texte = plier(&format!("{} {}", r.moteur, r.requete));
        let dedans: Vec<&str> = mots.iter().map(String::as_str).filter(|m| texte.contains(m)).collect();
        if dedans.is_empty() {
            continue;
        }
        for m in dedans {
            if !trouves.contains(&m) {
                trouves.push(m);
            }
        }
        lignes.push(format!("• {} · {} : « {} »", lisible(r.quand_ms), r.moteur, r.requete));
    }
    (!lignes.is_empty()).then(|| (trouves.join(" », « "), format!("{} recherche(s) tapée(s) :\n{}", lignes.len(), lignes.join("\n"))))
}

/// Le texte affiché à un instant (l'image à l'écran à ce moment-là).
pub fn ecran(base: &Base, quand: i64) -> String {
    let Some(image) = base.image_avant(quand).ok().flatten() else {
        return format!("Rien d'enregistré avant {}.", lisible(quand));
    };
    if image.privee {
        return "Moment en navigation privée : rien n'est donné.".to_string();
    }
    // L'écran à cet instant, c'est l'image prise juste avant : son texte, à son heure.
    let lignes = base.texte_visible(image.ecran, image.quand_ms).unwrap_or_default();
    let mut texte = lignes.join("\n");
    if texte.chars().count() > 12_000 {
        texte = texte.chars().take(12_000).collect::<String>() + "\n…";
    }
    format!(
        "Écran à {} (image de {}) — {}\n\n{}",
        lisible(quand),
        lisible(image.quand_ms),
        description_image(&image),
        if texte.is_empty() { "(aucun texte lu)".to_string() } else { texte }
    )
}

#[tool_router(router = tool_router)]
impl Memoire {
    pub fn new(base: Base) -> Self {
        Self::avec_recherches(base, None, None)
    }

    /// Avec la recherche par le sens et dans les images, si cette installation les a.
    pub fn avec_recherches(base: Base, sens: Option<ChercheurSens>, images: Option<ChercheurImages>) -> Self {
        Self {
            base: Arc::new(Mutex::new(base)),
            sens,
            images,
            tool_router: Self::tool_router(),
        }
    }

    fn avec<T>(&self, f: impl FnOnce(&Base) -> T) -> T {
        let base = self.base.lock().unwrap_or_else(|e| e.into_inner());
        f(&base)
    }

    #[tool(description = "Cherche un mot ou une phrase dans tout ce qui a été vu à l'écran ou dit (voix transcrites). \
        Rend l'heure, où (à l'écran, entendu), l'app et la page, et un extrait.")]
    async fn chercher(&self, Parameters(p): Parameters<Chercher>) -> String {
        self.avec(|b| chercher(b, &p.requete, p.limite.unwrap_or(20) as usize))
    }

    #[tool(description = "Cherche par le SENS dans tout ce qui a été vu ou dit, sans connaître les mots exacts : \
        synonymes, paraphrases, idées (« le dessin animé avec des espionnes », « mollusque »). Rend les textes les plus \
        proches avec l'heure, l'app, la page et la proximité (0 à 1 ; au-dessus de 0,4 c'est très proche). \
        À utiliser quand `chercher` ne trouve rien ou pour une question formulée en langage naturel.")]
    async fn chercher_par_le_sens(&self, Parameters(p): Parameters<Chercher>) -> String {
        let Some(sens) = self.sens.clone() else {
            return "La recherche par le sens n'est pas disponible dans cette installation.".to_string();
        };
        let limite = p.limite.unwrap_or(15) as usize;
        self.avec(|b| {
            let proches = sens(b, &p.requete, limite.clamp(1, 100) * 2);
            chercher_sens(b, &proches, &p.requete, limite)
        })
    }

    #[tool(description = "Cherche dans les IMAGES de l'écran ce qui se voyait sans être écrit : une photo, une vidéo, \
        un objet, une couleur (« la voiture rouge », « un chien sur la plage »), en français ou en anglais. Rend les moments \
        les plus ressemblants (seulement ceux proches du meilleur) avec l'heure, l'app et la page ; la ressemblance n'a pas \
        d'échelle fixe (le modèle lit aussi le texte affiché) : à vérifier, une image peut se tromper. Période facultative.")]
    async fn chercher_dans_les_images(&self, Parameters(p): Parameters<Images>) -> String {
        let Some(images) = self.images.clone() else {
            return "La recherche dans les images n'est pas disponible dans cette installation.".to_string();
        };
        let periode = match (p.debut.as_deref(), p.fin.as_deref()) {
            (None, None) => None,
            (debut, fin) => match periode_de(debut, fin) {
                Ok(bornes) => Some(bornes),
                Err(e) => return e,
            },
        };
        let limite = p.limite.unwrap_or(10) as usize;
        self.avec(|b| {
            let proches = images(b, &p.requete, 200);
            chercher_images_entre(b, &proches, &p.requete, limite, periode)
        })
    }

    #[tool(description = "Ce qui était à l'écran pendant une période : apps, titres de fenêtre et pages web, \
        regroupés tant qu'ils restent affichés, avec les voix entendues pendant chaque plage (« voix : Toi, Camille ») \
        — de quoi retrouver un épisode (« l'appel avec Camille »).")]
    async fn activite(&self, Parameters(p): Parameters<Periode>) -> String {
        match bornes(&p) {
            Ok((debut, fin)) => self.avec(|b| activite(b, debut, fin)),
            Err(e) => e,
        }
    }

    #[tool(description = "Ce qui a été dit pendant une période (micro et son du Mac), avec qui parlait \
        (« Toi » ou le nom de la voix reconnue) ; `personne` pour ne garder que ce qu'a dit quelqu'un.")]
    async fn voix(&self, Parameters(p): Parameters<Paroles>) -> String {
        match periode_de(p.debut.as_deref(), p.fin.as_deref()) {
            Ok((debut, fin)) => self.avec(|b| voix_de(b, debut, fin, p.personne.as_deref())),
            Err(e) => e,
        }
    }

    #[tool(description = "Les échanges avec une personne pendant une période (7 derniers jours par défaut) : \
        ce qui a été dit des deux côtés chaque fois qu'on entend sa voix (au téléphone en haut-parleur, à côté \
        de Julien, en appel sur le Mac), et ses messages écrits (Messages, WhatsApp…) avec qui a écrit quoi. \
        `personne` : le nom de sa voix ou de sa conversation.")]
    async fn echanges(&self, Parameters(p): Parameters<AvecQui>) -> String {
        let (debut, fin) = match (p.debut.as_deref(), p.fin.as_deref()) {
            (None, None) => (maintenant_ms() - 7 * 86_400_000, maintenant_ms()),
            (d, f) => match periode_de(d, f) {
                Ok(bornes) => bornes,
                Err(e) => return e,
            },
        };
        self.avec(|b| {
            let personnes = crate::personnes::citees(b, &p.personne, debut, fin);
            if personnes.is_empty() {
                return format!(
                    "Personne de connu sous « {} » entre {} et {} (ni voix nommée, ni conversation).",
                    p.personne,
                    lisible(debut),
                    lisible(fin)
                );
            }
            personnes
                .iter()
                .map(|x| format!("## {}\n{}", x.nom, crate::personnes::echanges(b, x, debut, fin, 24_000)))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }

    #[tool(description = "Ce que Julien a tapé dans Google, YouTube, Wikipédia, Amazon… pendant une période \
        (pratique pour retrouver « le … que je cherchais »).")]
    async fn recherches(&self, Parameters(p): Parameters<Periode>) -> String {
        match bornes(&p) {
            Ok((debut, fin)) => self.avec(|b| recherches(b, debut, fin)),
            Err(e) => e,
        }
    }

    #[tool(description = "Le texte affiché à l'écran à un instant donné (dans l'ordre de lecture), avec l'app et la page.")]
    async fn ecran(&self, Parameters(p): Parameters<Moment>) -> String {
        match periode(&p.quand, maintenant_ms()) {
            Some((quand, _)) => self.avec(|b| ecran(b, quand)),
            None => format!("instant incompris : « {} »", p.quand),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Memoire {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("b-screen", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Sert la mémoire sur l'entrée / la sortie standard jusqu'à la fin de la connexion.
pub fn lancer(fichier_base: &Path) -> Result<()> {
    lancer_avec(fichier_base, None, None)
}

/// Pareil, avec la recherche par le sens et dans les images.
pub fn lancer_avec(fichier_base: &Path, sens: Option<ChercheurSens>, images: Option<ChercheurImages>) -> Result<()> {
    let base = Base::ouvrir_lecture(fichier_base)?;
    let execution = tokio::runtime::Builder::new_current_thread().enable_time().build()?;
    execution.block_on(async {
        let service = Memoire::avec_recherches(base, sens, images).serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bscreen_store::{NouveauMorceau, NouvelleApparition, NouvelleImage};

    fn base_d_essai() -> Base {
        let base = Base::en_memoire().unwrap();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "a.mp4", debut_ms: 0, largeur: 10, hauteur: 10 })
            .unwrap();
        for (i, (quand, app)) in [(1_000i64, "Safari"), (3_000, "Safari"), (5_000, "Chrome")].iter().enumerate() {
            let id = base
                .ajouter_image(&NouvelleImage {
                    morceau: m,
                    quand_ms: *quand,
                    position_ms: i as i64 * 2_000,
                    ecran: 1,
                    app_bundle: None,
                    app_nom: Some(app),
                    aire: 1.0,
                    declenchee: false,
                })
                .unwrap();
            base.noter_fenetre(id, Some("Les dauphins"), Some("https://exemple.fr/dauphins")).unwrap();
        }
        base.noter_privee(1, 4_000, 5_000).unwrap();
        let t = base.texte_id("Les dauphins dorment avec un œil ouvert").unwrap();
        let secret = base.texte_id("dauphin vu en navigation privée").unwrap();
        base.noter_apparitions(
            &[
                NouvelleApparition { texte: t, ecran: 1, quand_ms: 1_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] },
                NouvelleApparition { texte: secret, ecran: 1, quand_ms: 5_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] },
            ],
            &[],
            1_000,
        )
        .unwrap();
        base
    }

    #[test]
    fn recherche_sans_les_moments_prives() {
        let base = base_d_essai();
        let r = chercher(&base, "dauphin", 10);
        assert!(r.contains("dorment"), "{r}");
        assert!(!r.contains("navigation privée"), "un moment privé ne sort pas : {r}");
        assert!(chercher(&base, "girafe", 10).starts_with("Rien trouvé"));
    }

    #[test]
    fn activite_regroupee_et_privee_masquee() {
        let base = base_d_essai();
        let r = activite(&base, 0, 10_000);
        assert!(r.contains("Safari · « Les dauphins » · https://exemple.fr/dauphins"), "{r}");
        assert!(r.contains("(navigation privée)") && !r.contains("Chrome ·"), "{r}");
        assert_eq!(r.matches("• ").count(), 2, "deux plages : Safari, puis la navigation privée");
        // Qui parlait pendant la plage Safari (jamais pendant la navigation privée).
        let camille = base.nouveau_locuteur(&[0u8; 8], false).unwrap();
        base.nommer_locuteur(camille, "Camille").unwrap();
        base.ajouter_voix(1, 1_500, 2_500, "a.aac", None, Some(camille)).unwrap();
        let r = activite(&base, 0, 10_000);
        let safari = r.lines().find(|l| l.contains("Safari")).unwrap_or_default();
        assert!(safari.ends_with("· son du Mac : Camille"), "{r}");
        assert!(!r.lines().any(|l| l.contains("(navigation privée)") && l.contains("voix")), "{r}");
    }

    #[test]
    fn ecran_a_un_instant() {
        let base = base_d_essai();
        assert!(ecran(&base, 2_000).contains("dorment avec un œil ouvert"));
        assert!(ecran(&base, 5_500).contains("navigation privée : rien"));
    }

    #[test]
    fn voix_d_une_personne_et_recherches() {
        let base = base_d_essai();
        let camille = base.nouveau_locuteur(&[0u8; 8], false).unwrap();
        base.nommer_locuteur(camille, "Camille").unwrap();
        let v = base.ajouter_voix(1, 2_000, 2_500, "a.aac", None, Some(camille)).unwrap();
        base.ajouter_voix(0, 2_600, 2_900, "b.aac", None, None).unwrap();
        let passages = base.voix_entre(0, 10_000).unwrap();
        base.noter_transcription(passages.iter().find(|p| p.id == v).unwrap(), "le serveur redémarre", "[]", "essai")
            .unwrap();
        let r = voix_de(&base, 0, 10_000, Some("camille"));
        assert!(r.contains("Camille (son du Mac) : « le serveur redémarre »") && !r.contains("Toi"), "{r}");
        // La voix de Julien, même nommée, reste « Toi » et se trouve par « toi » ou « Julien ».
        let moi = base.nouveau_locuteur(&[1u8; 8], true).unwrap();
        base.nommer_locuteur(moi, "bini").unwrap();
        let v = base.ajouter_voix(0, 3_000, 3_500, "c.aac", None, Some(moi)).unwrap();
        let passages = base.voix_entre(0, 10_000).unwrap();
        base.noter_transcription(passages.iter().find(|p| p.id == v).unwrap(), "on lance demain", "[]", "essai")
            .unwrap();
        for qui in ["toi", "Julien", "bini"] {
            let r = voix_de(&base, 0, 10_000, Some(qui));
            assert!(r.contains("Toi : « on lance demain »") && !r.contains("Camille"), "{qui} : {r}");
        }
        base.noter_recherche(1_500, "google.com", "poisson clown rayures", "https://…").unwrap();
        let r = recherches(&base, 0, 10_000);
        assert!(r.contains("google.com : « poisson clown rayures »"), "{r}");
    }

    #[test]
    fn une_page_par_ligne_et_l_echange_autour_d_une_voix() {
        let base = base_d_essai();
        // Deux textes de la même page, au même moment : une seule ligne.
        let t = base.texte_id("Le grand dauphin garde un œil ouvert").unwrap();
        base.noter_apparitions(
            &[NouvelleApparition { texte: t, ecran: 1, quand_ms: 1_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] }],
            &[],
            1_000,
        )
        .unwrap();
        let r = chercher(&base, "dauphin", 10);
        assert_eq!(r.matches("• ").count(), 1, "{r}");
        assert!(r.contains("« Les dauphins dorment avec un œil ouvert »") && r.contains("« Le grand dauphin garde"), "{r}");
        // La question entendue vient avec sa réponse (la phrase d'après), pas la vidéo d'à côté.
        // (Après la navigation privée de la base d'essai : une image ordinaire.)
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "b.mp4", debut_ms: 50_000, largeur: 10, hauteur: 10 })
            .unwrap();
        base.ajouter_image(&NouvelleImage {
            morceau: m,
            quand_ms: 50_000,
            position_ms: 0,
            ecran: 1,
            app_bundle: None,
            app_nom: Some("Safari"),
            aire: 1.0,
            declenchee: false,
        })
        .unwrap();
        let moi = base.nouveau_locuteur(&[1u8; 8], true).unwrap();
        let autre = base.nouveau_locuteur(&[2u8; 8], false).unwrap();
        for (source, debut, voix, texte) in [
            (0u8, 100_000, Some(moi), "Tu sais où est le chargeur ?"),
            (0, 106_000, Some(autre), "Il est derrière le truc."),
            (1, 110_000, None, "le chargeur le plus rapide du marché"),
        ] {
            let v = base.ajouter_voix(source, debut, debut + 4_000, "a.aac", None, voix).unwrap();
            let p = base.voix_entre(debut, debut + 1).unwrap().into_iter().find(|p| p.id == v).unwrap();
            base.noter_transcription(&p, texte, "[]", "essai").unwrap();
        }
        let r = chercher(&base, "chargeur", 10);
        let echange = r.split("• ").find(|l| l.contains("Tu sais où")).unwrap_or_default();
        assert!(echange.contains("l'échange autour") && echange.contains("« Il est derrière le truc. »"), "{r}");
        assert!(!echange.contains("marché"), "{r}");
        // La vidéo trouvée par ses mots reste un résultat à part.
        assert!(r.contains("vidéo (son du Mac) : « le chargeur le plus rapide du marché »"), "{r}");
    }

    #[test]
    fn extraits() {
        let long = format!("{} les dauphins {}", "a ".repeat(200), "b ".repeat(200));
        let e = extrait(&long, "dauphins", 60);
        assert!(e.contains("dauphins") && e.starts_with('…') && e.ends_with('…'), "{e}");
    }
}
