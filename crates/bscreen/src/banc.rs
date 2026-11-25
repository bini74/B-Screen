//! Banc d'essai de la recherche (phase 7.9 ; PLAN.md, section 3). Une semaine inventée (rien de
//! vrai : `banc/journee-temoin.json`), avec les bonnes réponses et des pièges, est rangée dans
//! une base en mémoire ; ses textes passent par le vrai modèle de sens. Les questions du plan
//! passent ensuite par la vraie recherche de la frise et le vrai contexte d'Ask (celui de Claude
//! et celui du modèle local) ; avec `--reponses`, le modèle local y répond aussi. La vraie base
//! n'est ni lue ni écrite, et rien ne sort du Mac. À passer à chaque version de la recherche.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use bscreen_api::contexte::{Budget, Proches, preparer_avec};
use bscreen_api::heures::plier;
use bscreen_core::image::ImageBgra;
use bscreen_macos::images::{COTE, EncodeurImage, EncodeurTexte};
use bscreen_macos::sens::{Encodeur, quantifier};
use bscreen_store::{Base, NouveauMorceau, NouvelleApparition, NouvelleImage};
use bscreen_ui::frise::Trouvee;
use chrono::{Duration, Local, NaiveDate, NaiveTime, TimeZone};
use serde::Deserialize;

const JOURNEE: &str = include_str!("../banc/journee-temoin.json");
/// Les photos des scènes (CC0 ou domaine public, voir CREDITS.md).
const PHOTOS: [(&str, &[u8]); 4] = [
    ("voiture-rouge.jpg", include_bytes!("../banc/photos/voiture-rouge.jpg")),
    ("voiture-bleue.jpg", include_bytes!("../banc/photos/voiture-bleue.jpg")),
    ("velo-rouge.jpg", include_bytes!("../banc/photos/velo-rouge.jpg")),
    ("pompiers.jpg", include_bytes!("../banc/photos/pompiers.jpg")),
];
/// L'écran de la semaine témoin.
const ECRAN: u32 = 1;
/// Une image toutes les 10 s pendant chaque scène.
const PAS_IMAGES_MS: i64 = 10_000;
/// L'heure qu'il est, aujourd'hui, dans la semaine témoin.
const HEURE_TEMOIN: &str = "16:00";

#[derive(Deserialize)]
struct Journee {
    voix: Vec<VoixTemoin>,
    ecran: Vec<Scene>,
    paroles: Vec<Parole>,
    questions: Vec<Question>,
}

#[derive(Deserialize)]
struct VoixTemoin {
    cle: String,
    #[serde(default)]
    nom: Option<String>,
    #[serde(default)]
    moi: bool,
}

/// Une fenêtre au premier plan pendant un moment, et ce qu'on y lisait.
#[derive(Deserialize)]
struct Scene {
    /// Pour la viser dans une question (la frise).
    #[serde(default)]
    id: Option<String>,
    jour: i64,
    de: String,
    a: String,
    /// Identifiant et nom de l'app.
    app: (String, String),
    fenetre: String,
    #[serde(default)]
    url: Option<String>,
    /// Ce que Julien a tapé dans un moteur : (moteur, requête).
    #[serde(default)]
    recherche: Option<(String, String)>,
    #[serde(default)]
    textes: Vec<String>,
    /// Une conversation : (« recu » | « envoye » | « milieu », texte).
    #[serde(default)]
    bulles: Vec<(String, String)>,
    /// Une photo (de `banc/photos`) posée dans la fenêtre : ce qui se voit sans être écrit.
    #[serde(default)]
    photo: Option<String>,
    /// La part de la largeur de l'écran qu'elle prend.
    #[serde(default)]
    part: Option<f64>,
}

/// Un passage de voix.
#[derive(Deserialize)]
struct Parole {
    jour: i64,
    a: String,
    /// Durée (secondes).
    #[serde(default = "quatre_secondes")]
    s: i64,
    /// La voix (clé de `voix`) ; aucune : une vidéo, ou une voix pas reconnue.
    #[serde(default)]
    qui: Option<String>,
    /// Le son du Mac (sinon le micro) ; hors appel, c'est une vidéo.
    #[serde(default)]
    mac: bool,
    #[serde(default)]
    appel: bool,
    texte: String,
}

fn quatre_secondes() -> i64 {
    4
}

/// Une question, et ce qu'on attend du contexte d'Ask, de la frise et de la réponse.
#[derive(Deserialize)]
struct Question {
    question: String,
    #[serde(default)]
    verifie: String,
    /// La partie du contexte où regarder (« Échanges avec Camille ») ; aucune : tout le contexte.
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    contient: Vec<String>,
    #[serde(default)]
    absent: Vec<String>,
    /// Le texte visé parmi les lignes de la partie, et son rang maximal.
    #[serde(default)]
    rang: Option<(String, usize)>,
    /// La scène visée dans la grille de la frise, et son rang maximal.
    #[serde(default)]
    frise: Option<(String, usize)>,
    /// La réponse doit contenir un mot de chaque groupe (« disque|SSD »).
    #[serde(default)]
    reponse: Vec<String>,
    /// Ce qui manque encore à B-Screen pour y répondre (un échec attendu).
    #[serde(default)]
    manque: Option<String>,
    /// Seulement après la relecture de la nuit (`--nuit`) : les sous-entendus rendus explicites.
    #[serde(default)]
    apres_la_nuit: bool,
    /// Un passage « (déduit : …) » du contexte doit contenir ces mots.
    #[serde(default)]
    deduit: Option<String>,
}

/// « HH:MM » ou « HH:MM:SS », `jour` jours avant `aujourdhui`, en ms.
fn instant(aujourdhui: NaiveDate, jour: i64, heure: &str) -> Result<i64> {
    let h = NaiveTime::parse_from_str(heure, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(heure, "%H:%M"))
        .with_context(|| format!("heure « {heure} »"))?;
    let date = aujourdhui - Duration::days(jour);
    Local
        .from_local_datetime(&date.and_time(h))
        .earliest()
        .map(|d| d.timestamp_millis())
        .ok_or_else(|| anyhow!("heure « {heure} » du {date} introuvable"))
}

/// Une scène rangée : son identifiant, sa plage et sa première image.
struct Plage {
    id: Option<String>,
    debut_ms: i64,
    fin_ms: i64,
    premiere_image: i64,
}

/// Range la semaine témoin dans la base : images, fenêtres, textes, recherches tapées, voix.
fn remplir(base: &Base, journee: &Journee, aujourdhui: NaiveDate) -> Result<Vec<Plage>> {
    let mut plages = Vec::new();
    for (n, scene) in journee.ecran.iter().enumerate() {
        let debut = instant(aujourdhui, scene.jour, &scene.de)?;
        let fin = instant(aujourdhui, scene.jour, &scene.a)?.max(debut);
        let fichier = format!("banc-{n}.mp4");
        let morceau = base.nouveau_morceau(&NouveauMorceau {
            ecran: ECRAN,
            fichier: &fichier,
            debut_ms: debut,
            largeur: 3024,
            hauteur: 1964,
        })?;
        let mut quand = debut;
        let mut premiere_image = None;
        while quand <= fin {
            let image = base.ajouter_image(&NouvelleImage {
                morceau,
                quand_ms: quand,
                position_ms: quand - debut,
                ecran: ECRAN,
                app_bundle: Some(&scene.app.0),
                app_nom: Some(&scene.app.1),
                aire: 1.0,
                declenchee: true,
            })?;
            base.noter_fenetre(image, Some(&scene.fenetre), scene.url.as_deref())?;
            premiere_image.get_or_insert(image);
            quand += PAS_IMAGES_MS;
        }
        // Les textes de haut en bas ; une conversation : le titre en haut (large), les bulles
        // reçues collées à gauche, les envoyées à droite, les libellés au milieu.
        let mut vus: Vec<(&str, [f64; 4])> =
            scene.textes.iter().enumerate().map(|(i, t)| (t.as_str(), [120.0, 140.0 + 28.0 * i as f64, 900.0, 20.0])).collect();
        if !scene.bulles.is_empty() {
            vus.push((&scene.fenetre, [426.0, 302.0, 1272.0, 20.0]));
            for (i, (cote, texte)) in scene.bulles.iter().enumerate() {
                let y = 400.0 + 40.0 * i as f64;
                let l = (texte.chars().count() as f64 * 8.0).clamp(60.0, 480.0);
                let cadre = match cote.as_str() {
                    "recu" => [773.0, y, l, 20.0],
                    "envoye" => [1678.0 - l, y, l, 20.0],
                    _ => [1185.0, y, 75.0, 14.0],
                };
                vus.push((texte, cadre));
            }
        }
        let nouvelles = vus
            .iter()
            .map(|(texte, cadre)| {
                Ok(NouvelleApparition {
                    texte: base.texte_id(texte)?,
                    ecran: ECRAN,
                    quand_ms: debut,
                    source: 0,
                    vu: true,
                    focus: false,
                    cadre: *cadre,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let ids = base.noter_apparitions(&nouvelles, &[], debut)?;
        base.noter_apparitions(&[], &ids, fin)?;
        if let Some((moteur, requete)) = &scene.recherche {
            base.noter_recherche(debut, moteur, requete, scene.url.as_deref().unwrap_or_default())?;
        }
        plages.push(Plage {
            id: scene.id.clone(),
            debut_ms: debut,
            fin_ms: fin,
            premiere_image: premiere_image.context("scène sans image")?,
        });
    }
    let mut voix: HashMap<&str, i64> = HashMap::new();
    for (k, v) in journee.voix.iter().enumerate() {
        let mut id = base.nouveau_locuteur(&[k as u8 + 1; 8], v.moi)?;
        if let Some(nom) = &v.nom {
            id = base.nommer_locuteur(id, nom)?;
        }
        voix.insert(&v.cle, id);
    }
    for p in &journee.paroles {
        let debut = instant(aujourdhui, p.jour, &p.a)?;
        let fin = debut + p.s * 1_000;
        let locuteur = match &p.qui {
            Some(cle) => Some(*voix.get(cle.as_str()).ok_or_else(|| anyhow!("voix « {cle} » inconnue"))?),
            None => None,
        };
        let id = base.ajouter_voix_avec(u8::from(p.mac), debut, fin, "banc.m4a", None, locuteur, p.appel)?;
        let passage = base.voix_entre(debut, fin)?.into_iter().find(|v| v.id == id).context("passage introuvable")?;
        base.noter_transcription(&passage, &p.texte, "[]", "banc")?;
    }
    Ok(plages)
}

/// Les vecteurs de sens des textes (vus, dits ou déduits) après `apres_id`, comme l'indexation de
/// fond ; rend (combien, le dernier identifiant).
fn indexer(base: &Base, encodeur: &Encodeur, apres_id: i64) -> Result<(usize, i64)> {
    let textes = base.textes_sans_sens(apres_id, 1_000_000)?;
    // Comme l'app : pas de vecteur pour un texte de moins de 3 mots.
    for (id, contenu) in textes.iter().filter(|(_, c)| bscreen_macos::sens::a_comprendre(c)) {
        let vecteurs: Vec<(f32, Vec<i8>)> = encodeur.texte(contenu)?.iter().map(|v| quantifier(v)).collect();
        base.ranger_sens(*id, &vecteurs)?;
    }
    Ok((textes.len(), textes.last().map_or(apres_id, |t| t.0)))
}

/// L'image d'une scène telle que le modèle la reçoit (l'écran de 1512 × 945 écrasé à 256 × 256) :
/// une fenêtre blanche avec des lignes de texte, et sa photo s'il y en a une, à droite.
fn image_de_scene(scene: &Scene, rang: usize) -> Result<ImageBgra> {
    const L: f64 = 1512.0;
    const H: f64 = 945.0;
    let c = COTE as usize;
    let (sx, sy) = (c as f64 / L, c as f64 / H);
    let mut pixels = vec![0u8; c * c * 4];
    let remplir = |x0: f64, y0: f64, x1: f64, y1: f64, (r, v, b): (u8, u8, u8), pixels: &mut Vec<u8>| {
        let (x0, x1) = ((x0 * sx).round() as usize, ((x1 * sx).round() as usize).min(c));
        let (y0, y1) = ((y0 * sy).round() as usize, ((y1 * sy).round() as usize).min(c));
        for y in y0..y1 {
            for x in x0..x1 {
                pixels[(y * c + x) * 4..(y * c + x) * 4 + 4].copy_from_slice(&[b, v, r, 255]);
            }
        }
    };
    remplir(0.0, 0.0, L, H, (236, 236, 238), &mut pixels);
    remplir(60.0, 60.0, L - 60.0, H - 40.0, (255, 255, 255), &mut pixels);
    for i in 0..14 {
        // Des lignes de longueurs variées, différentes d'une scène à l'autre.
        let longueur = 300.0 + ((rang * 7 + i * 13) % 11) as f64 * 34.0;
        let y = 120.0 + 28.0 * i as f64;
        remplir(90.0, y, 90.0 + longueur, y + 10.0, (70, 70, 75), &mut pixels);
    }
    if let Some(nom) = &scene.photo {
        let octets = PHOTOS.iter().find(|(n, _)| n == nom).map(|(_, o)| *o).with_context(|| format!("photo « {nom} » absente"))?;
        let originale = bscreen_macos::lecture::image_depuis_octets(octets, None).context("photo illisible")?;
        let largeur = scene.part.unwrap_or(0.4) * L;
        let hauteur = largeur * f64::from(originale.hauteur) / f64::from(originale.largeur);
        let (pl, ph) = ((largeur * sx).round().max(1.0) as u32, (hauteur * sy).round().max(1.0) as u32);
        let photo = bscreen_macos::lecture::image_depuis_octets(octets, Some((pl, ph))).context("photo illisible")?;
        let (x0, y0) = (((L - 90.0 - largeur) * sx).round() as usize, (110.0 * sy).round() as usize);
        for y in 0..(ph as usize).min(c.saturating_sub(y0)) {
            for x in 0..(pl as usize).min(c.saturating_sub(x0)) {
                let source = (y * pl as usize + x) * 4;
                let cible = ((y0 + y) * c + x0 + x) * 4;
                pixels[cible..cible + 4].copy_from_slice(&photo.pixels[source..source + 4]);
            }
        }
    }
    Ok(ImageBgra { largeur: COTE, hauteur: COTE, pixels })
}

/// Le vecteur d'image de chaque scène, rangé sur sa première image (une scène, une image regardée).
fn regarder_les_scenes(base: &Base, journee: &Journee, plages: &[Plage], encodeur: &EncodeurImage) -> Result<usize> {
    for (rang, (scene, plage)) in journee.ecran.iter().zip(plages).enumerate() {
        let vecteur = encodeur.vecteur(&image_de_scene(scene, rang)?)?;
        let (echelle, octets) = quantifier(&vecteur);
        base.ranger_vue(plage.premiere_image, echelle, &octets)?;
    }
    Ok(plages.len())
}

/// La partie du contexte qui commence par « ## {titre} », jusqu'à la suivante.
fn partie<'a>(contexte: &'a str, titre: &str) -> Option<&'a str> {
    let debut = contexte.find(&format!("## {titre}"))?;
    let reste = &contexte[debut..];
    let fin = reste[3..].find("\n## ").map_or(reste.len(), |i| i + 3);
    Some(&reste[..fin])
}

fn guillemets(mots: &[&str]) -> String {
    mots.iter().map(|m| format!("« {m} »")).collect::<Vec<_>>().join(", ")
}

/// Le contexte répond-il ? (et pourquoi pas)
fn verifier(contexte: &str, q: &Question) -> (bool, Vec<String>) {
    let zone = match &q.section {
        Some(titre) => match partie(contexte, titre) {
            Some(z) => z,
            None => return (false, vec![format!("pas de partie « {titre} »")]),
        },
        None => contexte,
    };
    let pliee = plier(zone);
    let manquants: Vec<&str> = q.contient.iter().filter(|c| !pliee.contains(&plier(c))).map(String::as_str).collect();
    let en_trop: Vec<&str> = q.absent.iter().filter(|c| pliee.contains(&plier(c))).map(String::as_str).collect();
    let mut notes = Vec::new();
    if !manquants.is_empty() {
        notes.push(format!("il manque {}", guillemets(&manquants)));
    }
    if !en_trop.is_empty() {
        notes.push(format!("il y a {}", guillemets(&en_trop)));
    }
    let mut juste = manquants.is_empty() && en_trop.is_empty();
    if let Some(mots) = &q.deduit {
        // Les passages « (déduit : …) » du contexte : l'un d'eux doit parler de ça.
        let trouve = pliee
            .split("(deduit : ")
            .skip(1)
            .filter_map(|suite| suite.split(')').next())
            .any(|deduit| deduit.contains(&plier(mots)));
        if !trouve {
            notes.push(format!("pas de version déduite qui parle de « {mots} »"));
            juste = false;
        }
    }
    if let Some((cible, max)) = &q.rang {
        let cible = plier(cible);
        match zone.lines().filter(|l| l.starts_with("• ")).position(|l| plier(l).contains(&cible)) {
            Some(r) => {
                notes.push(format!("{} des lignes", rang(r + 1)));
                juste &= r < *max;
            }
            None => juste = false,
        }
    }
    (juste, notes)
}

/// « 1er », « 2e »…
fn rang(n: usize) -> String {
    if n == 1 { "1er".to_string() } else { format!("{n}e") }
}

/// Le rang de la scène visée dans la grille de la frise : les endroits trouvés par les mots
/// (entiers d'abord, puis débuts de mots, les plus récents d'abord), puis ceux trouvés par le
/// sens, un par texte, du plus proche au moins proche (comme la frise les range).
fn rang_dans_la_frise(trouvees: &[Trouvee], plages: &[Plage], cible: &str) -> Option<usize> {
    let scene = |t: &Trouvee| (t.ecran != 0).then(|| plages.iter().position(|p| t.debut_ms >= p.debut_ms && t.debut_ms <= p.fin_ms)).flatten();
    // (endroit, sorte : 0 mots entiers, 1 débuts de mots, 2 sens ; ordre ; scène)
    let mut endroits: Vec<(String, u8, i64, Option<usize>)> = Vec::new();
    for t in trouvees {
        let s = scene(t);
        let (cle, sorte, ordre) = if t.sens {
            let ordre = endroits.iter().filter(|e| e.1 == 2).count() as i64;
            (format!("sens {}", t.texte.as_deref().unwrap_or_default()), 2, ordre)
        } else if t.ecran == 0 {
            (format!("voix {}", t.debut_ms / 300_000), u8::from(!t.entier), -t.debut_ms)
        } else {
            (format!("scène {s:?}"), u8::from(!t.entier), -t.debut_ms)
        };
        match endroits.iter_mut().find(|e| e.0 == cle) {
            Some(e) if sorte < e.1 => (e.1, e.2) = (sorte, ordre),
            Some(_) => {}
            None => endroits.push((cle, sorte, ordre, s)),
        }
    }
    endroits.sort_by_key(|e| (e.1, e.2));
    endroits.iter().position(|e| e.3.and_then(|i| plages[i].id.as_deref()) == Some(cible)).map(|r| r + 1)
}

/// La réponse contient-elle un mot de chaque groupe ? Rend les groupes absents.
fn groupes_absents<'a>(reponse: &str, groupes: &'a [String]) -> Vec<&'a str> {
    let r = plier(reponse);
    groupes.iter().filter(|g| !g.split('|').any(|mot| r.contains(&plier(mot)))).map(String::as_str).collect()
}

#[derive(Default)]
struct Compte {
    justes: usize,
    total: usize,
}

impl Compte {
    fn noter(&mut self, juste: bool) {
        self.total += 1;
        self.justes += usize::from(juste);
    }
}

impl std::fmt::Display for Compte {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.justes, self.total)
    }
}

/// `bscreen banc-d-essai [--reponses] [--nuit] [--contexte N]`.
pub fn passer(arguments: &[String]) -> Result<()> {
    let reponses = arguments.iter().any(|a| a == "--reponses");
    let nuit = arguments.iter().any(|a| a == "--nuit");
    let montrer: Option<usize> = arguments
        .iter()
        .position(|a| a == "--contexte")
        .and_then(|i| arguments.get(i + 1))
        .and_then(|n| n.parse().ok());
    let journee: Journee = serde_json::from_str(JOURNEE).context("semaine témoin illisible")?;
    let aujourdhui = Local::now().date_naive();
    let maintenant_ms = instant(aujourdhui, 0, HEURE_TEMOIN)?;
    let debut = Instant::now();
    let base = Base::en_memoire()?;
    let plages = remplir(&base, &journee, aujourdhui)?;
    let encodeur = Encodeur::charger().context("modèle de sens")?;
    let (mut textes, dernier_texte) = indexer(&base, &encodeur, 0)?;
    // La relecture de la nuit (7.6) : les phrases floues rendues explicites par le modèle local.
    if nuit {
        let t = Instant::now();
        let _serveur = bscreen_macos::ollama::Serveur::demarrer()?;
        let deduites = bscreen_macos::sous_entendus::relire(&base, maintenant_ms, usize::MAX, &|| true)?;
        textes += indexer(&base, &encodeur, dernier_texte)?.0;
        println!("Relecture de la nuit : {deduites} phrase(s) rendue(s) explicite(s) en {:.0} s.", t.elapsed().as_secs_f64());
        let ids: Vec<i64> = base.voix_entre(0, i64::MAX)?.iter().map(|v| v.id).collect();
        let deduits = base.deduits_de(&ids)?;
        for v in base.voix_entre(0, i64::MAX)? {
            if let Some((explicite, confiance)) = deduits.get(&v.id) {
                println!("  « {} » → « {explicite} » ({confiance:.2})", v.texte.as_deref().unwrap_or_default());
            }
        }
    }
    let images_texte = EncodeurTexte::charger().context("modèle des textes d'images")?;
    let vues = regarder_les_scenes(&base, &journee, &plages, &EncodeurImage::charger().context("modèle d'images")?)?;
    println!(
        "Banc d'essai de la recherche, sur une semaine inventée (rien de vrai, rien ne sort du Mac) :\n\
         {} scènes à l'écran ({vues} images comprises), {} passages de voix, {textes} textes compris par le sens — prêt en {:.1} s.\n",
        journee.ecran.len(),
        journee.paroles.len(),
        debut.elapsed().as_secs_f64()
    );
    let (mut complet_compte, mut local_compte, mut frise_compte, mut reponses_compte) =
        (Compte::default(), Compte::default(), Compte::default(), Compte::default());
    let mut pas_encore: Vec<String> = Vec::new();
    let marque = |juste: bool, q: &Question| match (juste, &q.manque) {
        (true, _) => "✓",
        (false, Some(_)) => "○",
        (false, None) => "✗",
    };
    let mut sautees = 0;
    for (n, q) in journee.questions.iter().enumerate() {
        if q.apres_la_nuit && !nuit {
            println!("{}. « {} » — après la relecture de la nuit seulement (--nuit)\n", n + 1, q.question);
            sautees += 1;
            continue;
        }
        let t = Instant::now();
        let vecteur = encodeur.question(&q.question)?;
        let proches = crate::ask::proches(&base, &vecteur).unwrap_or_default();
        let images = crate::ask::images_proches(&base, &images_texte.vecteur(&q.question)?).unwrap_or_default();
        let trouve = Proches { textes: Some(&proches), images: Some(&images) };
        let complet = preparer_avec(&base, &q.question, maintenant_ms, trouve, Budget::COMPLET);
        let local = preparer_avec(&base, &q.question, maintenant_ms, trouve, Budget::LOCAL);
        let ms = t.elapsed().as_millis();
        println!("{}. « {} »", n + 1, q.question);
        if !q.verifie.is_empty() {
            println!("   attendu : {}", q.verifie);
        }
        let mut une_faute = false;
        for (nom, contexte, compte) in
            [("contexte d'Ask        ", &complet, &mut complet_compte), ("contexte « Sur le Mac »", &local, &mut local_compte)]
        {
            let (juste, notes) = verifier(contexte, q);
            compte.noter(juste);
            une_faute |= !juste;
            let notes = if notes.is_empty() { String::new() } else { format!(" — {}", notes.join(" ; ")) };
            println!("   {} {nom} {} car.{notes}", marque(juste, q), contexte.chars().count());
        }
        println!("     (préparés en {ms} ms)");
        if let Some((cible, max)) = &q.frise {
            let trouvees = crate::application::chercher_dans(
                &base,
                &q.question,
                ECRAN,
                &|texte| encodeur.question(texte).ok(),
                &|texte| images_texte.vecteur(texte).ok(),
            );
            let r = rang_dans_la_frise(&trouvees, &plages, cible);
            let juste = r.is_some_and(|r| r <= *max);
            frise_compte.noter(juste);
            une_faute |= !juste;
            let ou = r.map_or("pas trouvée".to_string(), |r| format!("{} endroit de la grille", rang(r)));
            println!("   {} frise                   « {cible} » : {ou} ({} résultats)", marque(juste, q), trouvees.len());
        }
        if reponses {
            let t = Instant::now();
            match crate::ask::repondre_sur_le_mac(&q.question, &local, maintenant_ms) {
                Ok(brute) => {
                    // Ce qu'Ask montrerait : sans les lignes « Retenu » (ce n'est pas une correction).
                    let mut filtre = bscreen_api::corrections::SansRetenu::default();
                    let reponse = format!("{}{}", filtre.passer(&brute), filtre.finir());
                    if filtre.ecartees > 0 {
                        println!("     ({} ligne(s) « Retenu » écrite(s) sans correction, retirée(s) comme dans Ask)", filtre.ecartees);
                    }
                    let absents = groupes_absents(&reponse, &q.reponse);
                    let juste = absents.is_empty();
                    reponses_compte.noter(juste);
                    une_faute |= !juste;
                    let notes = if juste { String::new() } else { format!(" — il manque {}", guillemets(&absents)) };
                    println!("   {} réponse du modèle local, {:.0} s{notes}", marque(juste, q), t.elapsed().as_secs_f64());
                    for ligne in reponse.trim().lines().filter(|l| !l.trim().is_empty()) {
                        println!("       │ {ligne}");
                    }
                }
                Err(e) => {
                    reponses_compte.noter(false);
                    println!("   ✗ réponse du modèle local : {e}");
                }
            }
        }
        if une_faute && let Some(manque) = &q.manque {
            println!("     pas encore : {manque}");
            pas_encore.push(format!("« {} » ({manque})", q.question));
        }
        if montrer == Some(n + 1) {
            println!("\n――― contexte d'Ask ―――\n{complet}\n\n――― contexte « Sur le Mac » ―――\n{local}\n―――");
        }
        println!();
    }
    if reponses {
        bscreen_macos::ollama::arreter();
    }
    let mut bilan = format!("Bilan — contexte d'Ask {complet_compte}, « Sur le Mac » {local_compte}");
    if frise_compte.total > 0 {
        bilan.push_str(&format!(", frise {frise_compte}"));
    }
    if reponses_compte.total > 0 {
        bilan.push_str(&format!(", réponses du modèle local {reponses_compte}"));
    }
    println!("{bilan}.");
    if !pas_encore.is_empty() {
        println!("Pas encore possible : {}.", pas_encore.join(" ; "));
    }
    if sautees > 0 {
        println!("{sautees} question(s) sautée(s) : elles demandent la relecture de la nuit (--nuit, modèle local).");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semaine_temoin_lisible_et_rangee() {
        let journee: Journee = serde_json::from_str(JOURNEE).unwrap();
        assert!(journee.questions.len() >= 6);
        let base = Base::en_memoire().unwrap();
        let aujourdhui = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        let plages = remplir(&base, &journee, aujourdhui).unwrap();
        assert_eq!(plages.len(), journee.ecran.len());
        // Chaque scène visée par une question existe.
        for q in &journee.questions {
            if let Some((cible, _)) = &q.frise {
                assert!(plages.iter().any(|p| p.id.as_deref() == Some(cible.as_str())), "{cible}");
            }
        }
        // Les voix : Camille nommé, la vidéo sans voix, la conversation de Messages lisible.
        let maintenant = instant(aujourdhui, 0, HEURE_TEMOIN).unwrap();
        let contexte = preparer_avec(&base, "on a parlé de quoi hier avec Camille ?", maintenant, Proches::default(), Budget::COMPLET);
        let echanges = partie(&contexte, "Échanges avec Camille").unwrap();
        assert!(echanges.contains("Camille : « Le disque du serveur"), "{echanges}");
        assert!(echanges.contains("Toi : « Oui, livré jeudi »"), "{echanges}");
        assert!(!echanges.contains("Samsung") && !echanges.contains("match"), "{echanges}");
    }

    #[test]
    fn verifications() {
        let q = Question {
            question: String::new(),
            verifie: String::new(),
            section: Some("Textes proches".into()),
            contient: vec!["hémisphère".into()],
            absent: vec!["orques".into()],
            rang: Some(("hemisphere".into(), 1)),
            frise: None,
            reponse: vec!["disque|SSD".into(), "sauvegarde".into()],
            manque: None,
            apres_la_nuit: false,
            deduit: None,
        };
        let contexte = "## Période\nrien\n\n## Textes proches par le sens\n• un seul Hémisphère dort\n• les orques\n\n## Mots exacts\nx";
        let (juste, notes) = verifier(contexte, &q);
        assert!(!juste && notes.iter().any(|n| n.contains("orques")), "{notes:?}");
        let sans_orques = contexte.replace("• les orques\n", "");
        assert!(verifier(&sans_orques, &q).0);
        assert_eq!(partie(contexte, "Mots exacts"), Some("## Mots exacts\nx"));
        assert_eq!(groupes_absents("Il faut changer le SSD.", &q.reponse), vec!["sauvegarde"]);
    }
}
