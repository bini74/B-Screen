//! La frise : l'écran d'un instant passé, en plein écran, et en bas une barre
//! transparente (seulement floue) avec la bande de vignettes qu'on fait glisser
//! au trackpad. Le logo de l'app déborde du coin haut gauche de chaque vignette.
//!
//! Les images sont décodées hors du fil principal ; en attendant l'image en
//! pleine définition, on montre la vignette agrandie.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use bscreen_core::image::ImageBgra;
use bscreen_core::texte_lu::{
    LigneLue, Position, bandes, ligne_autour, mot_autour, position_sous, sur_du_texte, texte_entre,
};
use chrono::{Datelike, Local, TimeZone, Timelike};
use futures::StreamExt;
use futures::channel::mpsc as canal;
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, Context, FocusHandle, FontWeight, Global, HighlightStyle, ImageSource,
    KeyBinding, SharedString, StyledText,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, RenderImage, ScrollWheelEvent, Task,
    WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, actions, deferred, div,
    img, prelude::*, px, rgb, rgba, size,
};
use image::{Frame, RgbaImage};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

actions!(
    frise,
    [
        Fermer,
        Ecouter,
        VoixPrecedente,
        VoixSuivante,
        Precedent,
        Suivant,
        Supprimer,
        SupprimerSelection,
        EtendreGauche,
        EtendreDroite,
        Confirmer,
        Copier,
        OuvrirPage,
        JourPrecedent,
        JourSuivant,
        PassagePrecedent,
        PassageSuivant
    ]
);

/// Largeur des vignettes décodées (en pixels).
const VIGNETTE_L: u32 = 400;
/// Largeur d'une vignette à l'écran au zoom 1, écart entre deux vignettes.
const LARGEUR_BASE: f32 = 150.0;
const ECART: f32 = 14.0;
/// Le zoom va de 0,0025 à 1,6. Au-dessus de 0,5, il agrandit ou réduit les vignettes ; en dessous,
/// elles gardent leur plus petite taille et chacune vaut plusieurs moments (jusqu'à 200 au plus
/// réduit) : on remonte la journée bien plus vite (Julien, 25/11 : pincer ne faisait que réduire).
const ZOOM_MIN: f32 = 0.0025;
const ZOOM_MAX: f32 = 1.6;
const ZOOM_TAILLE_MIN: f32 = 0.5;
/// La barre du bas va d'un bord à l'autre de l'écran, collée en bas.
const HAUTEUR_CARTE: f32 = 262.0;
/// Marge intérieure des lignes de texte et de la barre d'activité.
const MARGE_TEXTE: f32 = 24.0;
const HAUTEUR_BANDE: f32 = 176.0;
/// Vignettes gardées en mémoire de part et d'autre de la tête de lecture.
const GARDE: usize = 60;

/// Un moment affichable.
#[derive(Clone, Debug)]
pub struct Moment {
    pub id: i64,
    pub quand_ms: i64,
    pub fichier: String,
    pub position_ms: i64,
    pub app_bundle: Option<String>,
    pub app_nom: Option<String>,
    pub largeur: u32,
    pub hauteur: u32,
    /// La page web affichée à ce moment, s'il y en a une.
    pub url: Option<String>,
    /// Le titre de la fenêtre au premier plan (pour regrouper les résultats d'une même fenêtre).
    pub fenetre: Option<String>,
    /// Navigation privée : hachuré et cadenassé jusqu'à Touch ID (l'image n'est même pas décodée).
    pub privee: bool,
}

/// Ce dont la frise a besoin, fourni par l'app (et la couche système).
pub trait SourceFrise: Send + Sync + 'static {
    /// Les moments à montrer, du plus ancien au plus récent.
    fn moments(&self) -> Vec<Moment>;
    /// L'image d'un moment (appelé hors du fil principal) ; `taille` : pour une vignette.
    fn image(&self, moment: &Moment, taille: Option<(u32, u32)>) -> Option<ImageBgra>;
    /// L'icône d'une app, carrée (fil principal).
    fn icone_app(&self, identifiant: &str) -> Option<ImageBgra>;
    /// Réglages natifs de la fenêtre : plein écran, par-dessus tout. `vue` : la vue native.
    fn preparer_fenetre(&self, vue: *mut c_void);
    /// La frise s'ouvre / se ferme (l'enregistrement se met en pause pendant ce temps).
    fn quand_ouverte(&self);
    /// Une recherche commence (champ ouvert) : les modèles de recherche peuvent se préparer.
    fn preparer_recherche(&self) {}
    /// Les sites masqués dans la recherche (réglages) : leurs moments ne sont pas des résultats.
    fn sites_caches(&self) -> Vec<String> {
        Vec::new()
    }
    /// Photographie la fenêtre (mode d'essai) ; rien par défaut.
    fn photographier(&self, _vue: *mut c_void, _nom: &str) {}
    fn quand_fermee(&self);
    /// Supprime définitivement des moments (base + vidéo). Appelé après confirmation.
    fn supprimer(&self, ids: Vec<i64>);
    /// Recherche plein texte : où et quand le texte cherché était à l'écran.
    fn chercher(&self, requete: &str) -> Vec<Trouvee>;
    /// Où ces mots sont visibles sur l'image d'un moment (OCR), en cadres normalisés (0 à 1).
    /// Appelé hors du fil principal.
    fn reperer(&self, moment: &Moment, mots: &[String]) -> Vec<[f64; 4]>;
    /// Le texte visible à l'écran à ce moment, dans l'ordre de lecture.
    fn texte_visible(&self, moment: &Moment) -> Vec<String>;
    /// Tout le texte de l'image d'un moment, mot à mot avec sa place (OCR), pour le sélectionner.
    /// Appelé hors du fil principal.
    fn lire_image(&self, moment: &Moment) -> Vec<LigneLue>;
    /// Les passages de voix de la période montrée (micro et son du Mac).
    fn voix(&self) -> Vec<Passage> {
        Vec::new()
    }
    /// Joue un passage à partir de `depuis_s` secondes (fil principal) ; rend la durée jouée.
    fn ecouter(&self, _passage: &Passage, _depuis_s: f64) -> Option<f64> {
        None
    }
    /// Arrête l'écoute en cours.
    fn arreter_ecoute(&self) {}
    /// Demande Touch ID pour voir les moments privés ; `vue` : la vue native de la frise.
    /// `reponse` est appelée sur le fil principal.
    fn deverrouiller(&self, _vue: *mut c_void, reponse: Box<dyn FnOnce(bool) + Send>) {
        reponse(false);
    }
    /// Donne un nom à une voix (toutes ses phrases, et la reconnaître ensuite).
    fn nommer_voix(&self, _locuteur_id: i64, _nom: &str) -> bool {
        false
    }
    /// Les voix qu'on peut donner à un passage : (identifiant, nom affiché), celle de Julien d'abord.
    fn voix_connues(&self) -> Vec<(i64, String)> {
        Vec::new()
    }
    /// Range un passage dans une voix : seul ce passage change, et la voix choisie apprend de lui.
    /// Rend sa voix (identifiant et nom affiché), ou rien si la correction n'a pas pu se faire.
    fn corriger_voix(&self, _passage: i64, _voix: VoixChoisie) -> Option<(Option<i64>, Option<String>)> {
        None
    }
    /// Hauteur cachée en haut de l'écran (encoche de la caméra), en points.
    fn marge_haute(&self) -> f32 {
        0.0
    }
}

/// Un texte trouvé : sur quel écran, pendant quelle plage de temps, à quelle place (en points).
#[derive(Clone, Debug)]
pub struct Trouvee {
    pub ecran: u32,
    pub debut_ms: i64,
    pub fin_ms: i64,
    pub cadre: [f64; 4],
    /// Visible à l'écran à ce moment (sinon : texte présent mais caché ou hors champ).
    pub vu: bool,
    /// Trouvé par le sens (phase 7), pas par les mots exacts.
    pub sens: bool,
    /// Les mots cherchés y sont entiers (« bar »), pas seulement des débuts de mots (« barre »).
    pub entier: bool,
    /// Le texte trouvé (lu ou dit) : un extrait en est montré dans la grille.
    pub texte: Option<String>,
}

/// Des résultats au même endroit (même page, même fenêtre, à moins de 15 minutes l'un de
/// l'autre) ou d'un même échange de voix : une seule carte dans la grille, une étape pour ↓ / ↑.
/// (Demande de Julien, 22/11 : « bar » sortait des milliers de résultats.)
#[derive(Clone, Debug, PartialEq)]
struct Groupe {
    /// Les moments trouvés (rangs dans `moments`), dans l'ordre du temps.
    indices: Vec<usize>,
    /// Le moment montré pour le groupe : le premier où le texte est visible (ou entendu).
    vedette: usize,
    entendu: bool,
    /// Trouvé seulement par le sens : son rang (le plus proche d'abord).
    sens: Option<usize>,
    /// Le mot cherché y est entier (« bar »), pas seulement un début de mot (« barre »).
    entier: bool,
    /// Ce qui a été lu ou dit, autour du mot cherché.
    extrait: Option<String>,
    /// Pour une voix : entendue au micro (Julien, quelqu'un près de lui) ou au son du Mac (vidéo, appel).
    micro: Option<bool>,
}

/// L'aide de la frise (touche ?) : tous les raccourcis, par thème. RACCOURCIS.md dit la même chose.
const AIDE: &[(&str, &[(&str, &str)])] = &[
    ("Partout", &[("⌥Espace", "Ouvrir ou fermer la frise"), ("⌥⇧A", "Ouvrir ou fermer Ask")]),
    (
        "Se déplacer",
        &[
            ("← / →", "Moment précédent / suivant"),
            ("Deux doigts", "Défiler dans le temps"),
            ("Pincer", "Agrandir ou réduire ; réduite au bout, une vignette vaut jusqu'à 200 moments pour remonter vite"),
            ("Clic", "Aller au moment d'une vignette"),
            ("⌘← / ⌘→", "Jour précédent / suivant"),
            ("⌥← / ⌥→", "Là où quelqu'un parlait"),
        ],
    ),
    (
        "Voix",
        &[
            ("Espace", "Écouter ce qui a été dit (Espace : arrêter)"),
            ("↑ / ↓", "Choisir le passage, s'il y en a plusieurs"),
            ("Clic sur une phrase", "L'écouter"),
            ("Clic sur un nom ou « ? »", "Dire qui parle, ou renommer la voix"),
        ],
    ),
    (
        "Texte et pages",
        &[
            ("Glisser", "Sélectionner du texte sur l'image"),
            ("2 / 3 clics", "Un mot / la ligne"),
            ("⌘C", "Copier la sélection, sinon tout le texte"),
            ("⌘O", "Rouvrir la page web"),
        ],
    ),
    (
        "Supprimer",
        &[
            ("⌥-clic · ⌥⇧←→", "Sélectionner des moments"),
            ("⌫ ou ⌦", "Supprimer la sélection"),
            ("⌥⌫", "Supprimer le moment affiché"),
            ("Entrée / Échap", "Confirmer / annuler"),
        ],
    ),
    ("Moments privés", &[("Entrée ou clic", "Déverrouiller avec Touch ID (5\u{a0}min)")]),
    (
        "Rechercher",
        &[
            ("Une lettre", "Ouvrir la recherche"),
            ("↓ ou Entrée", "Endroit suivant"),
            ("↑ ou ⇧Entrée", "Endroit précédent"),
            ("Tab", "Tous les endroits en grille"),
            ("Échap", "Fermer la grille, puis la recherche"),
        ],
    ),
    (
        "Filtres",
        &[
            ("@safari", "Une app (un morceau du nom suffit)"),
            ("@hier, @lundi…", "Un jour (@aujourdhui, @avant-hier…)"),
            ("@dit", "Seulement ce qui a été dit"),
            ("@dit @hier bar", "Les filtres se combinent avec des mots"),
        ],
    ),
    (
        "Ask",
        &[
            ("Entrée", "Poser la question (ou une question de suite)"),
            ("⌘N", "Repartir de zéro"),
            ("⌘Entrée", "Ouvrir le moment cité dans la frise"),
            ("Pastille", "Rapide, Équilibré, Approfondi, Sur le Mac"),
            ("Échap", "Fermer (l'échange reste 30\u{a0}min)"),
        ],
    ),
    ("Fermer", &[("Échap ou ⌘W", "Fermer la frise"), ("?", "Ouvrir ou fermer cette aide")]),
];

/// Les thèmes de l'aide (rangs dans `AIDE`) en quatre colonnes de hauteur voisine. Des colonnes
/// fixes : avec un retour à la ligne automatique, le panneau ne prenait la hauteur que d'une rangée.
const COLONNES_AIDE: [&[usize]; 4] = [&[0, 1], &[2, 3, 5], &[6, 7], &[8, 4, 9]];

/// Le panneau d'aide : les thèmes en colonnes, sur un voile sombre (un clic le ferme).
fn panneau_aide(cx: &mut Context<Frise>) -> gpui::Stateful<gpui::Div> {
    let section = |(titre, lignes): &(&'static str, &'static [(&'static str, &'static str)])| {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgba(0xEBEBF599))
                    .child(titre.to_uppercase()),
            )
            .children(lignes.iter().map(|(touche, quoi)| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div().w(px(118.0)).flex_none().child(
                            div()
                                .flex_none()
                                .px(px(7.0))
                                .py(px(2.0))
                                .rounded(px(6.0))
                                .bg(rgba(0xFFFFFF1A))
                                .border_1()
                                .border_color(rgba(0xFFFFFF2E))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xFFFFFF))
                                .child(*touche),
                        ),
                    )
                    .child(div().flex_1().min_w_0().text_size(px(13.0)).text_color(rgb(0xEBEBF5)).child(*quoi))
            }))
    };
    let colonnes = COLONNES_AIDE.iter().map(|rangs| {
        div()
            .w(px(320.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .children(rangs.iter().map(|&k| section(&AIDE[k])))
    });
    div()
        .id("aide")
        .absolute()
        .left_0()
        .top_0()
        .size_full()
        .occlude()
        .bg(rgba(0x0000009E))
        .flex()
        .items_center()
        .justify_center()
        .on_click(cx.listener(|frise, _: &ClickEvent, _, cx| {
            frise.aide_ouverte = false;
            cx.notify();
        }))
        .child(
            div()
                .flex_none()
                .p(px(28.0))
                .rounded(px(24.0))
                .bg(rgba(0x1C1C1EF2))
                .border_1()
                .border_color(rgba(0xFFFFFF22))
                .shadow_lg()
                .flex()
                .flex_col()
                .gap(px(20.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_end()
                        .gap(px(14.0))
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xFFFFFF))
                                .child("Raccourcis de B-Screen"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgba(0xEBEBF599))
                                .child("? ou Échap pour fermer · tout est aussi dans RACCOURCIS.md"),
                        ),
                )
                .child(div().flex().flex_row().items_start().gap(px(32.0)).children(colonnes)),
        )
}

/// Deux résultats au même endroit à moins de 15 minutes : le même groupe (5 pour les voix : un échange).
const ECART_GROUPE_MS: i64 = 15 * 60_000;
const ECART_VOIX_MS: i64 = 5 * 60_000;
/// Groupes montrés dans la grille (elle défile).
const GROUPES_GRILLE: usize = 60;

/// L'endroit d'un moment : l'app et la page (sans ce qui suit #), ou la fenêtre.
fn cle_d_endroit(m: &Moment) -> String {
    let app = m.app_bundle.as_deref().unwrap_or("");
    match m.url.as_deref() {
        Some(url) => format!("{app}|{}", url.split('#').next().unwrap_or(url)),
        None => format!("{app}|{}", m.fenetre.as_deref().unwrap_or("")),
    }
}

/// Ce qu'on sait des moments trouvés, pour les regrouper.
#[derive(Default)]
struct Marques<'a> {
    entendus: Option<&'a HashSet<i64>>,
    visibles: Option<&'a HashSet<i64>>,
    exacts: Option<&'a HashSet<i64>>,
    rang_sens: Option<&'a HashMap<i64, usize>>,
    entiers: Option<&'a HashSet<i64>>,
    extraits: Option<&'a HashMap<i64, String>>,
    /// Pour les voix : au micro (vrai) ou au son du Mac (faux).
    micro: Option<&'a HashMap<i64, bool>>,
}

/// Regroupe les résultats (rangs dans `moments`, dans l'ordre du temps) par endroit, puis range les
/// groupes : mots entiers d'abord, puis débuts de mots (les plus récents d'abord), puis ceux trouvés
/// seulement par le sens (le plus proche d'abord).
fn regrouper(moments: &[Moment], resultats: &[usize], marques: &Marques) -> Vec<Groupe> {
    let contient = |e: Option<&HashSet<i64>>, id: i64| e.is_some_and(|e| e.contains(&id));
    let mut groupes: Vec<Groupe> = Vec::new();
    let mut ouverts: HashMap<String, usize> = HashMap::new();
    for &i in resultats {
        let m = &moments[i];
        let entendu = contient(marques.entendus, m.id);
        let sens = if contient(marques.exacts, m.id) { None } else { marques.rang_sens.and_then(|r| r.get(&m.id).copied()) };
        let (cle, ecart) = if entendu {
            ("voix".to_string(), ECART_VOIX_MS)
        } else if let Some(rang) = sens {
            (format!("sens {rang}"), 0)
        } else {
            (cle_d_endroit(m), ECART_GROUPE_MS)
        };
        let suite = ouverts.get(&cle).copied().filter(|&g| {
            groupes[g].indices.last().is_some_and(|&d| m.quand_ms - moments[d].quand_ms <= ecart)
        });
        let g = match suite {
            Some(g) => {
                groupes[g].indices.push(i);
                g
            }
            None => {
                groupes.push(Groupe {
                    indices: vec![i],
                    vedette: i,
                    entendu,
                    sens,
                    entier: false,
                    extrait: None,
                    micro: None,
                });
                ouverts.insert(cle, groupes.len() - 1);
                groupes.len() - 1
            }
        };
        let groupe = &mut groupes[g];
        groupe.entier |= contient(marques.entiers, m.id);
        if entendu && groupe.micro.is_none() {
            groupe.micro = marques.micro.and_then(|v| v.get(&m.id).copied());
        }
        if groupe.extrait.is_none() {
            groupe.extrait = marques.extraits.and_then(|e| e.get(&m.id).cloned());
        }
    }
    for g in &mut groupes {
        if let Some(&v) = g
            .indices
            .iter()
            .find(|&&i| contient(marques.visibles, moments[i].id) || contient(marques.entendus, moments[i].id))
        {
            g.vedette = v;
        }
    }
    let rang = |g: &Groupe| match (g.sens, g.entier) {
        (Some(_), _) => 2,
        (None, true) => 0,
        (None, false) => 1,
    };
    groupes.sort_by(|a, b| {
        rang(a).cmp(&rang(b)).then_with(|| match (a.sens, b.sens) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => moments[b.vedette].quand_ms.cmp(&moments[a.vedette].quand_ms),
        })
    });
    groupes
}

/// Quelques mots autour du premier mot cherché (« …au bar du port… »), sur une ligne.
fn extrait_autour(texte: &str, mots: &[String], largeur: usize) -> String {
    let texte: Vec<char> = texte.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    let plie: Vec<char> = texte.iter().map(|&c| plier_car(c)).collect();
    let debut_mot = mots
        .iter()
        .filter_map(|m| {
            let m: Vec<char> = m.chars().map(plier_car).collect();
            (!m.is_empty()).then(|| plie.windows(m.len()).position(|w| w == m.as_slice())).flatten()
        })
        .min()
        .unwrap_or(0);
    let debut = debut_mot.saturating_sub(largeur / 3);
    let fin = (debut + largeur).min(texte.len());
    let debut = fin.saturating_sub(largeur).min(debut);
    format!(
        "{}{}{}",
        if debut > 0 { "…" } else { "" },
        texte[debut..fin].iter().collect::<String>().trim(),
        if fin < texte.len() { "…" } else { "" }
    )
}

/// Un passage de voix : qui parlait (le micro, donc toi, ou le son du Mac), quand, et ce qui a été dit.
#[derive(Clone, Debug)]
pub struct Passage {
    /// Son identifiant dans la base (pour corriger sa voix).
    pub id: i64,
    pub debut_ms: i64,
    pub fin_ms: i64,
    pub micro: bool,
    /// Le fichier audio (tel que la source le comprend).
    pub fichier: String,
    /// La voix reconnue : son identifiant et son nom affiché (« Toi », « Camille », « Voix 3 »).
    pub locuteur_id: Option<i64>,
    pub locuteur: Option<String>,
    /// Le son du Mac pendant un appel (sinon, sans voix, c'est une vidéo).
    pub appel: bool,
    /// La transcription ; `None` tant qu'elle n'est pas faite.
    pub texte: Option<String>,
    /// Entendu pendant une navigation privée : caché et muet tant que Touch ID n'a pas déverrouillé.
    pub privee: bool,
}

/// Le début du texte d'un résultat trouvé par l'image (« Image qui ressemble à « voiture rouge » ») :
/// la frise dit alors « trouvé par l'image » plutôt que « par le sens ».
pub const PAR_L_IMAGE: &str = "Image qui ressemble à ";

/// La voix qu'on donne à un passage, pour le corriger (clic sur son nom).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoixChoisie {
    /// Une voix connue (son identifiant).
    Connue(i64),
    /// Une nouvelle voix, avec ce nom (ou la voix qui le porte déjà).
    Nouvelle(String),
    /// Pas sûr de qui parle : « ? ».
    Aucune,
}

/// Ce que vise le nom tapé : renommer une voix (tous ses passages), ou une nouvelle voix pour un
/// seul passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CibleNom {
    Voix(i64),
    Passage(i64),
}

/// Le choix de la voix d'un passage (clic sur son nom) : le passage, sa voix, et les voix proposées.
struct ChoixVoix {
    passage: i64,
    debut_ms: i64,
    locuteur_id: Option<i64>,
    nom: Option<String>,
    proposees: Vec<(i64, String)>,
}

/// La frise ouverte, s'il y en a une.
#[derive(Default)]
struct FriseOuverte(Option<(WeakEntity<Frise>, AnyWindowHandle)>);
impl Global for FriseOuverte {}

/// Les touches de la frise. À appeler une fois au démarrage.
pub fn lier_touches(cx: &mut App) {
    cx.bind_keys([
        // Hors du champ de recherche seulement (là, ces touches servent à taper).
        KeyBinding::new("escape", Fermer, Some("Frise && !Recherche && !Nommage")),
        // Espace : écouter ce qui a été dit à ce moment (ou arrêter).
        KeyBinding::new("space", Ecouter, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("cmd-w", Fermer, Some("Frise")),
        KeyBinding::new("left", Precedent, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("right", Suivant, Some("Frise && !Recherche && !Nommage")),
        // Suppression : ⌥⌫ l'image en cours (ou la sélection), ⌫ / ⌦ la sélection seulement (en
        // effaçant une recherche, le ⌫ de trop ne propose plus de supprimer le moment affiché).
        KeyBinding::new("alt-backspace", Supprimer, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("backspace", SupprimerSelection, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("delete", SupprimerSelection, Some("Frise && !Recherche && !Nommage")),
        // Sélection : ⌥⇧← / ⌥⇧→ (et ⌥-clic sur une vignette).
        KeyBinding::new("alt-shift-left", EtendreGauche, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("alt-shift-right", EtendreDroite, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("enter", Confirmer, Some("Frise && !Recherche && !Nommage")),
        // ⌘C : copier le texte visible ; ⌘O : rouvrir la page web ; ⌘← / ⌘→ : jour précédent / suivant.
        KeyBinding::new("cmd-c", Copier, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("cmd-o", OuvrirPage, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("cmd-left", JourPrecedent, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("cmd-right", JourSuivant, Some("Frise && !Recherche && !Nommage")),
        // ⌥← / ⌥→ : le moment précédent / suivant où quelqu'un parlait.
        KeyBinding::new("alt-left", VoixPrecedente, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("alt-right", VoixSuivante, Some("Frise && !Recherche && !Nommage")),
        // ↑ / ↓ : choisir le passage à écouter quand plusieurs tombent sur ce moment.
        KeyBinding::new("up", PassagePrecedent, Some("Frise && !Recherche && !Nommage")),
        KeyBinding::new("down", PassageSuivant, Some("Frise && !Recherche && !Nommage")),
    ]);
}

/// Ouvre la frise sur le moment le plus proche de `quand_ms` (un renvoi d'Ask, par exemple).
pub fn ouvrir_au_moment(cx: &mut App, source: Arc<dyn SourceFrise>, quand_ms: i64) {
    ouvrir(cx, source);
    let Some((frise, _)) = cx.try_global::<FriseOuverte>().and_then(|f| f.0.clone()) else {
        return;
    };
    let _ = frise.update(cx, |frise, cx| {
        if let Some(i) = frise
            .moments
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| (m.quand_ms - quand_ms).abs())
            .map(|(i, _)| i)
        {
            frise.position = i as f32;
            cx.notify();
        }
    });
}

/// Ouvre la frise en plein écran (ou la ramène devant si elle est déjà ouverte).
pub fn ouvrir(cx: &mut App, source: Arc<dyn SourceFrise>) {
    if let Some((frise, _)) = cx.try_global::<FriseOuverte>().and_then(|f| f.0.clone())
        && frise.upgrade().is_some()
    {
        cx.activate(true);
        return;
    }
    let debut = std::time::Instant::now();
    source.quand_ouverte();
    let ouverte_ms = debut.elapsed().as_millis();
    let moments = source.moments();
    let moments_ms = debut.elapsed().as_millis();
    let ecran = cx.primary_display();
    let limites = ecran
        .as_ref()
        .map(|e| e.bounds())
        .unwrap_or_else(|| Bounds::centered(None, size(px(1440.0), px(900.0)), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(limites)),
        titlebar: None,
        kind: WindowKind::PopUp,
        window_background: WindowBackgroundAppearance::Transparent,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: ecran.as_ref().map(|e| e.id()),
        ..Default::default()
    };
    let source_fenetre = Arc::clone(&source);
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| Frise::new(window, cx, source_fenetre, moments, debut))
    }) {
        Ok(fenetre) => {
            if let Ok(entite) = fenetre.entity(cx) {
                cx.set_global(FriseOuverte(Some((entite.downgrade(), fenetre.into()))));
            }
            cx.activate(true);
            tracing::info!(
                videos_fermees_ms = ouverte_ms,
                moments_ms,
                ms = debut.elapsed().as_millis(),
                "frise : fenêtre ouverte"
            );
        }
        Err(e) => {
            eprintln!("frise impossible : {e}");
            source.quand_fermee();
        }
    }
}

/// Mode d'essai : ouvre la frise, cherche `requete` (« ? » : ouvre l'aide), ouvre la grille des
/// endroits, la photographie (dans BSCREEN_PHOTOS), puis referme la frise.
pub fn essai_recherche(cx: &mut App, source: Arc<dyn SourceFrise>, requete: String) {
    ouvrir(cx, Arc::clone(&source));
    let Some((frise, fenetre)) = cx.try_global::<FriseOuverte>().and_then(|f| f.0.clone()) else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
        let _ = frise.update(cx, |frise, cx| {
            // « ? » : l'aide plutôt qu'une recherche.
            if requete == "?" {
                frise.aide_ouverte = true;
            } else {
                frise.recherche_ouverte = true;
                frise.recherche = requete.clone();
                frise.lancer_recherche();
                frise.ouvrir_grille();
            }
            cx.notify();
        });
        cx.background_executor().timer(std::time::Duration::from_secs(6)).await;
        let _ = fenetre.update(cx, |_, window, _| {
            if let Ok(poignee) = window.window_handle()
                && let RawWindowHandle::AppKit(appkit) = poignee.as_raw()
            {
                source.photographier(appkit.ns_view.as_ptr(), "frise-recherche");
            }
        });
        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
        let _ = fenetre.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
}

/// Ouvre la frise, ou la ferme si elle est déjà ouverte (raccourci global).
pub fn basculer(cx: &mut App, source: Arc<dyn SourceFrise>) {
    let ouverte = cx.try_global::<FriseOuverte>().and_then(|f| f.0.clone());
    match ouverte {
        Some((frise, fenetre)) if frise.upgrade().is_some() => {
            let _ = fenetre.update(cx, |_, window, _| window.remove_window());
            cx.set_global(FriseOuverte(None));
        }
        _ => ouvrir(cx, source),
    }
}

/// Pincement du trackpad (agrandissement relatif), venu de la couche système.
pub fn pincer(cx: &mut App, agrandissement: f64) {
    let Some((frise, _)) = cx.try_global::<FriseOuverte>().and_then(|f| f.0.clone()) else {
        return;
    };
    let _ = frise.update(cx, |frise, cx| {
        // Sous la plus petite taille, le pincement va trois fois plus vite (de 1 à 200 moments par
        // vignette en quelques gestes).
        let facteur = (1.0 + agrandissement as f32).max(0.1);
        let facteur = if frise.zoom < ZOOM_TAILLE_MIN { facteur.powi(3) } else { facteur };
        frise.zoom = (frise.zoom * facteur).clamp(ZOOM_MIN, ZOOM_MAX);
        cx.notify();
    });
}

// ---------- décodage en arrière-plan ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Genre {
    Vignette,
    Grande,
    /// Grande image du moment suivant dans le sens du déplacement, préparée d'avance.
    Prechargee,
    /// Vignette d'un résultat de la grille (décodée même loin de la tête de lecture).
    Grille,
}

struct Demande {
    index: usize,
    moment: Moment,
    genre: Genre,
}

struct Resultat {
    id: i64,
    genre: Genre,
    image: Option<ImageBgra>,
    /// Pas décodée parce que trop loin de la tête de lecture (à redemander plus tard).
    sautee: bool,
}

fn taille_vignette(m: &Moment) -> (u32, u32) {
    let hauteur = (VIGNETTE_L as f32 * m.hauteur.max(1) as f32 / m.largeur.max(1) as f32).round() as u32;
    (VIGNETTE_L, hauteur.max(1))
}

/// Un fil qui ne fait que la demande la plus récente : celles arrivées pendant qu'il travaillait
/// sont sautées, sauf la dernière (OCR et lecture du moment affiché, recherche du texte tapé).
/// Rend l'envoi des demandes et la réception des réponses ; le fil s'arrête quand l'envoi est
/// lâché (frise fermée).
fn fil_du_plus_recent<D: Send + 'static, R: Send + 'static>(
    nom: &str,
    travail: impl Fn(D) -> R + Send + 'static,
) -> (mpsc::Sender<D>, canal::UnboundedReceiver<R>) {
    let (envoi, demandes) = mpsc::channel::<D>();
    let (envoi_reponses, reponses) = canal::unbounded::<R>();
    let _ = std::thread::Builder::new().name(nom.to_string()).spawn(move || {
        while let Ok(mut demande) = demandes.recv() {
            while let Ok(plus_recente) = demandes.try_recv() {
                demande = plus_recente;
            }
            if envoi_reponses.unbounded_send(travail(demande)).is_err() {
                return;
            }
        }
    });
    (envoi, reponses)
}

/// Deux fils pour les vignettes, un pour la grande image. Ils s'arrêtent quand la frise se ferme.
fn lancer_decodeurs(
    source: Arc<dyn SourceFrise>,
    courant: Arc<AtomicI64>,
    resultats: canal::UnboundedSender<Resultat>,
) -> (mpsc::Sender<Demande>, mpsc::Sender<Demande>) {
    let (envoi_vignettes, reception_vignettes) = mpsc::channel::<Demande>();
    let reception_vignettes = Arc::new(Mutex::new(reception_vignettes));
    for n in 0..2 {
        let (source, courant, resultats, reception) = (
            Arc::clone(&source),
            Arc::clone(&courant),
            resultats.clone(),
            Arc::clone(&reception_vignettes),
        );
        let _ = std::thread::Builder::new()
            .name(format!("bscreen-vignettes-{n}"))
            .spawn(move || {
                loop {
                    let demande = match reception.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => return,
                    };
                    let Ok(demande) = demande else { return };
                    // Trop loin de la tête de lecture (on a glissé vite) : inutile de décoder
                    // (sauf pour la grille des résultats).
                    if demande.genre == Genre::Vignette
                        && (demande.index as i64 - courant.load(Ordering::Relaxed)).unsigned_abs() as usize > GARDE
                    {
                        let _ = resultats.unbounded_send(Resultat {
                            id: demande.moment.id,
                            genre: demande.genre,
                            image: None,
                            sautee: true,
                        });
                        continue;
                    }
                    let image = source.image(&demande.moment, Some(taille_vignette(&demande.moment)));
                    if resultats
                        .unbounded_send(Resultat {
                            id: demande.moment.id,
                            genre: demande.genre,
                            image,
                            sautee: false,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
    }

    let (envoi_grande, reception_grande) = mpsc::channel::<Demande>();
    let _ = std::thread::Builder::new()
        .name("bscreen-grande".into())
        .spawn(move || {
            while let Ok(mut demande) = reception_grande.recv() {
                // Seule la dernière demande compte.
                while let Ok(plus_recente) = reception_grande.try_recv() {
                    demande = plus_recente;
                }
                let image = source.image(&demande.moment, None);
                if resultats
                    .unbounded_send(Resultat {
                        id: demande.moment.id,
                        genre: demande.genre,
                        image,
                        sautee: false,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    (envoi_vignettes, envoi_grande)
}

pub(crate) fn en_rendu(image: ImageBgra) -> Option<Arc<RenderImage>> {
    let tampon = RgbaImage::from_raw(image.largeur, image.hauteur, image.pixels)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(tampon)])))
}

// ---------- la vue ----------

pub struct Frise {
    source: Arc<dyn SourceFrise>,
    moments: Vec<Moment>,
    focus: FocusHandle,
    /// Position de la tête de lecture, en moments (partie décimale pendant le glissé).
    position: f32,
    zoom: f32,
    /// Le défilement aimanté : numéro du dernier geste (une glissade en cours s'arrête quand un
    /// nouveau geste arrive) et doigt levé (l'élan du trackpad suit).
    aimant: u64,
    doigt_leve: bool,
    /// La vignette vers laquelle on glisse, si une glissade est en cours.
    glissade: Option<f32>,
    vignettes: HashMap<i64, Arc<RenderImage>>,
    demandees: HashSet<i64>,
    /// Images illisibles (perdues dans un arrêt brutal) : on ne les redemande pas.
    echouees: HashSet<i64>,
    grande: Option<(i64, Arc<RenderImage>)>,
    grande_demandee: Option<i64>,
    /// Fonds flous (sans voile) de la barre du bas et du champ de recherche, pour le moment courant.
    /// Pour le champ : vrai si le fond est clair (texte foncé par-dessus).
    flou_carte: Option<(i64, Arc<RenderImage>)>,
    flou_champ: Option<(i64, Arc<RenderImage>, bool)>,
    /// Où le mot cherché est visible sur l'image courante (OCR) : (moment, recherche, cadres normalisés).
    reperes: Option<(i64, String, Vec<[f64; 4]>)>,
    reperes_demandes: Option<(i64, String)>,
    envoi_reperes: mpsc::Sender<(Moment, String)>,
    /// Moments où le texte cherché était visible (pastille jaune) ; les autres : texte caché (gris).
    resultats_vus: HashSet<i64>,
    /// Hauteur cachée en haut de l'écran (encoche) : le champ de recherche se place dessous.
    marge_haute: f32,
    /// Loupe du champ de recherche, claire et foncée.
    loupes: [Arc<RenderImage>; 2],
    _reception_ocr: Task<()>,
    /// Message bref au-dessus de la barre (« Texte copié »), et son numéro (pour l'effacer à temps).
    message: Option<(String, u64)>,
    /// Le passage en écoute (son début, en ms) et son numéro d'écoute, pour effacer l'état à la fin
    /// (le numéro change à chaque écoute : passer d'un passage à l'autre ne mélange pas leurs fins).
    ecoute: Option<(i64, u64)>,
    numero_ecoute: u64,
    /// Le passage choisi (↑ / ↓ ou clic) parmi ceux du moment affiché : (moment, rang).
    passage_choisi: Option<(usize, usize)>,
    /// Moments privés visibles (après Touch ID, pour 5 minutes) ; demande en cours ; numéro
    /// du déverrouillage (pour reverrouiller au bon moment) ; images à libérer au reverrouillage.
    deverrouille: bool,
    deverrouillage_en_cours: bool,
    /// Grande image préparée d'avance, et le sens du dernier déplacement (+1 / -1) et d'où l'on vient.
    prechargee: Option<(i64, Arc<RenderImage>)>,
    sens: isize,
    dernier_actuel: Option<usize>,
    numero_deverrouillage: u64,
    a_liberer: Vec<Arc<RenderImage>>,
    /// Barre d'activité, calculée une fois : (x, largeur, couleur) ; recalculée si les moments changent.
    activite: Vec<(f32, f32, u32)>,
    activite_pour: (usize, u32),
    /// Quand le mot trouvé est caché sous la barre, la barre se range d'elle-même ;
    /// un clic sur « ▲ » la remet pour ce moment-là.
    barre_forcee_pour: Option<i64>,
    /// Texte lu sur l'image du moment (OCR, mot à mot), pour le sélectionner à la souris.
    lecture: Option<(i64, Vec<LigneLue>)>,
    lecture_demandee: Option<i64>,
    envoi_lecture: mpsc::Sender<Moment>,
    _reception_lecture: Task<()>,
    /// Où sont les secrets lus sur l'image du moment (cadres normalisés), recouverts à l'affichage.
    caviardage: Option<(i64, Vec<[f64; 4]>)>,
    /// Sélection de texte sur l'image : premier et dernier mot (dans l'ordre de lecture), et pour quel moment.
    selection_texte: Option<(Position, Position, i64)>,
    glisse: bool,
    curseur_texte: bool,
    taille_ecran: (f32, f32),
    icones: HashMap<String, Option<(Arc<RenderImage>, u32)>>,
    envoi_vignettes: mpsc::Sender<Demande>,
    envoi_grande: mpsc::Sender<Demande>,
    courant: Arc<AtomicI64>,
    fenetre_preparee: bool,
    /// Quand l'ouverture a commencé (jusqu'à la première grande image, pour la mesurer).
    ouverte_a: Option<std::time::Instant>,
    /// Vignette sous la souris.
    survolee: Option<usize>,
    /// Moments sélectionnés (pour les supprimer ensemble), et le point de départ de la sélection.
    selection: BTreeSet<i64>,
    ancre: Option<usize>,
    /// Suppression en attente de confirmation (Entrée) ou d'annulation (Échap).
    confirmation: Option<Vec<i64>>,
    /// Recherche (⌘F) : le texte tapé, les moments trouvés et les textes à surligner.
    focus_recherche: FocusHandle,
    /// Nom de voix en cours de saisie : quelle voix, et le texte tapé.
    nommage: Option<(CibleNom, String)>,
    /// Le choix de la voix d'un passage, ouvert par un clic sur son nom.
    choix_voix: Option<ChoixVoix>,
    focus_nommage: FocusHandle,
    recherche_ouverte: bool,
    recherche: String,
    resultats: Vec<usize>,
    resultats_ids: HashSet<i64>,
    /// Le texte cherché part sur le fil de recherche (la base et le sens : 0,1 à 0,4 s, qui
    /// figeaient la frise à chaque lettre). Seule la réponse à la dernière demande est prise ;
    /// en attendant, les résultats d'avant restent affichés.
    envoi_recherche: mpsc::Sender<(u64, String)>,
    numero_recherche: u64,
    recherche_en_cours: bool,
    _reception_recherche: Task<()>,
    /// Les résultats regroupés par endroit (ordre de la grille et de ↓ / ↑), et celui affiché.
    groupes: Vec<Groupe>,
    groupe_courant: usize,
    /// Les passages de voix de la période, triés par début, et leurs marques sur la barre d'activité.
    passages: Vec<Passage>,
    activite_voix: Vec<(f32, f32, bool)>,
    /// Moments trouvés dans ce qui a été dit (pastille verte) plutôt qu'à l'écran.
    resultats_entendus: HashSet<i64>,
    /// Moments trouvés seulement par le sens (pas les mots tapés).
    resultats_sens: HashMap<i64, Vec<String>>,
    /// Moments trouvés par les mots tapés, et rang des moments trouvés par le sens (0 = le plus proche).
    resultats_exacts: HashSet<i64>,
    rang_sens: HashMap<i64, usize>,
    /// Grille des résultats (Tab dans la recherche) : tous les résultats d'un coup, en vignettes.
    grille_ouverte: bool,
    /// L'aide (touche ?) : tous les raccourcis.
    aide_ouverte: bool,
    _reception: Task<()>,
}

impl Drop for Frise {
    fn drop(&mut self) {
        self.source.quand_fermee();
    }
}

impl Frise {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        source: Arc<dyn SourceFrise>,
        moments: Vec<Moment>,
        ouverte_a: std::time::Instant,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let courant = Arc::new(AtomicI64::new(moments.len() as i64 - 1));
        let (envoi_resultats, mut reception_resultats) = canal::unbounded::<Resultat>();
        let (envoi_vignettes, envoi_grande) =
            lancer_decodeurs(Arc::clone(&source), Arc::clone(&courant), envoi_resultats);
        let reception = cx.spawn(async move |vue, cx| {
            while let Some(resultat) = reception_resultats.next().await {
                let image = resultat.image.and_then(en_rendu);
                let sautee = resultat.sautee;
                let suite = vue.update(cx, |frise, cx| {
                    if sautee {
                        frise.demandees.remove(&resultat.id);
                        return;
                    }
                    match resultat.genre {
                        Genre::Vignette | Genre::Grille => {
                            frise.demandees.remove(&resultat.id);
                            match image {
                                Some(image) => {
                                    frise.vignettes.insert(resultat.id, image);
                                }
                                None => {
                                    frise.echouees.insert(resultat.id);
                                }
                            }
                        }
                        Genre::Grande => match image {
                            // Arrivée trop tard (on est déjà ailleurs) : elle ne remplace rien.
                            Some(image) if frise.moments.get(frise.actuel()).map(|m| m.id) != Some(resultat.id) => {
                                frise.a_liberer.push(image);
                            }
                            Some(image) => {
                                if let Some((_, ancienne)) = frise.grande.replace((resultat.id, image)) {
                                    frise.a_liberer.push(ancienne);
                                }
                                if let Some(ouverte) = frise.ouverte_a.take() {
                                    tracing::info!(ms = ouverte.elapsed().as_millis(), "frise : première image");
                                }
                                // Le décodeur est libre : on prépare le moment suivant.
                                frise.precharger();
                            }
                            None => {
                                frise.echouees.insert(resultat.id);
                            }
                        },
                        Genre::Prechargee => {
                            if let Some(image) = image {
                                let actuel = frise.moments.get(frise.actuel()).map(|m| m.id);
                                if actuel == Some(resultat.id) && frise.grande.as_ref().map(|g| g.0) != actuel {
                                    // On y est déjà arrivé : elle sert tout de suite.
                                    if let Some((_, ancienne)) = frise.grande.replace((resultat.id, image)) {
                                        frise.a_liberer.push(ancienne);
                                    }
                                } else if let Some((_, ancienne)) = frise.prechargee.replace((resultat.id, image)) {
                                    frise.a_liberer.push(ancienne);
                                }
                            }
                        }
                    }
                    cx.notify();
                });
                if suite.is_err() {
                    break;
                }
            }
        });
        // OCR à la demande : où le mot cherché est visible sur l'image courante (le plus récent seulement).
        let source_ocr = Arc::clone(&source);
        let (envoi_reperes, mut reception_trouves) =
            fil_du_plus_recent("bscreen-ocr", move |(moment, requete): (Moment, String)| {
                let mots: Vec<String> = requete
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|m| m.chars().count() >= 2)
                    .map(str::to_string)
                    .collect();
                let cadres = source_ocr.reperer(&moment, &mots);
                (moment.id, requete, cadres)
            });
        let reception_ocr = cx.spawn(async move |vue, cx| {
            while let Some((id, requete, cadres)) = reception_trouves.next().await {
                let suite = vue.update(cx, |frise, cx| {
                    frise.reperes = Some((id, requete, cadres));
                    cx.notify();
                });
                if suite.is_err() {
                    break;
                }
            }
        });
        // Recherche du texte tapé, hors du fil principal : seule la dernière demande est faite
        // (pendant qu'on tape vite, les lettres intermédiaires sont sautées).
        let source_recherche = Arc::clone(&source);
        let (envoi_recherche, mut reception_trouvees) =
            fil_du_plus_recent("bscreen-recherche", move |(numero, texte): (u64, String)| {
                (numero, source_recherche.chercher(&texte))
            });
        let reception_recherche = cx.spawn(async move |vue, cx| {
            while let Some((numero, trouvees)) = reception_trouvees.next().await {
                let suite = vue.update(cx, |frise, cx| {
                    // La réponse à une recherche dépassée (une lettre de plus, champ vidé) : ignorée.
                    if numero == frise.numero_recherche && frise.recherche_en_cours {
                        frise.appliquer_trouvees(trouvees);
                        frise.suivre_avec_la_grille();
                        cx.notify();
                    }
                });
                if suite.is_err() {
                    break;
                }
            }
        });
        // Lecture mot à mot du moment affiché (le plus récent seulement), pour la sélection à la souris.
        let source_lecture = Arc::clone(&source);
        let (envoi_lecture, mut reception_lu) = fil_du_plus_recent("bscreen-lecture", move |moment: Moment| {
            let lignes = source_lecture.lire_image(&moment);
            (moment.id, lignes)
        });
        let reception_lecture = cx.spawn(async move |vue, cx| {
            while let Some((id, lignes)) = reception_lu.next().await {
                let suite = vue.update(cx, |frise, cx| {
                    // Les secrets lus sur l'image (mots de passe, clés, cartes…) sont cachés
                    // à l'affichage et ne peuvent être ni sélectionnés ni copiés.
                    let (lignes, caches) = cacher_secrets(lignes);
                    frise.caviardage = Some((id, caches));
                    frise.lecture = Some((id, lignes));
                    cx.notify();
                });
                if suite.is_err() {
                    break;
                }
            }
        });
        let marge_haute = source.marge_haute();
        // Les voix de la semaine (≈ 0,1 s) se chargent pendant que la frise s'affiche : leurs
        // marques arrivent juste après l'image, sans la retarder.
        let (envoi_voix, mut reception_voix) = canal::unbounded::<Vec<Passage>>();
        let source_voix = Arc::clone(&source);
        let _ = std::thread::Builder::new().name("bscreen-voix".into()).spawn(move || {
            let mut passages = source_voix.voix();
            passages.sort_by_key(|p| p.debut_ms);
            let _ = envoi_voix.unbounded_send(passages);
        });
        cx.spawn(async move |vue, cx| {
            if let Some(passages) = reception_voix.next().await {
                let _ = vue.update(cx, |frise, cx| {
                    frise.passages = passages;
                    frise.activite_pour = (0, 0);
                    cx.notify();
                });
            }
        })
        .detach();
        Self {
            passages: Vec::new(),
            activite_voix: Vec::new(),
            resultats_entendus: HashSet::new(),
            resultats_sens: HashMap::new(),
            resultats_exacts: HashSet::new(),
            rang_sens: HashMap::new(),
            grille_ouverte: false,
            aide_ouverte: false,
            source,
            position: moments.len().saturating_sub(1) as f32,
            moments,
            focus,
            zoom: 1.0,
            aimant: 0,
            doigt_leve: false,
            glissade: None,
            vignettes: HashMap::new(),
            demandees: HashSet::new(),
            echouees: HashSet::new(),
            grande: None,
            grande_demandee: None,
            flou_carte: None,
            flou_champ: None,
            reperes: None,
            reperes_demandes: None,
            envoi_reperes,
            resultats_vus: HashSet::new(),
            loupes: [icone_loupe(0xFFFFFF), icone_loupe(0x1D1D1F)],
            marge_haute,
            _reception_ocr: reception_ocr,
            message: None,
            ecoute: None,
            numero_ecoute: 0,
            passage_choisi: None,
            deverrouille: false,
            deverrouillage_en_cours: false,
            prechargee: None,
            sens: 1,
            dernier_actuel: None,
            numero_deverrouillage: 0,
            a_liberer: Vec::new(),
            activite: Vec::new(),
            activite_pour: (usize::MAX, 0),
            barre_forcee_pour: None,
            lecture: None,
            lecture_demandee: None,
            envoi_lecture,
            _reception_lecture: reception_lecture,
            selection_texte: None,
            glisse: false,
            curseur_texte: false,
            taille_ecran: (0.0, 0.0),
            icones: HashMap::new(),
            envoi_vignettes,
            envoi_grande,
            courant,
            fenetre_preparee: false,
            ouverte_a: Some(ouverte_a),
            survolee: None,
            selection: BTreeSet::new(),
            ancre: None,
            confirmation: None,
            focus_recherche: cx.focus_handle(),
            nommage: None,
            choix_voix: None,
            caviardage: None,
            focus_nommage: cx.focus_handle(),
            recherche_ouverte: false,
            recherche: String::new(),
            resultats: Vec::new(),
            resultats_ids: HashSet::new(),
            envoi_recherche,
            numero_recherche: 0,
            recherche_en_cours: false,
            _reception_recherche: reception_recherche,
            groupes: Vec::new(),
            groupe_courant: 0,
            _reception: reception,
        }
    }

    /// Combien de moments vaut une vignette (1 tant que le zoom ne descend pas sous la plus petite taille).
    fn densite(&self) -> usize {
        // Des pas ronds : les vignettes ne sautent pas d'un moment à l'autre à chaque geste.
        const PAS: [usize; 12] = [1, 2, 3, 5, 10, 15, 20, 30, 50, 75, 100, 200];
        let brute = (ZOOM_TAILLE_MIN / self.zoom).max(1.0) as usize;
        PAS.iter().copied().rev().find(|&p| p <= brute).unwrap_or(1)
    }

    /// La place d'une vignette à l'écran (sa largeur et l'écart).
    fn pas(&self) -> f32 {
        LARGEUR_BASE * self.zoom.max(ZOOM_TAILLE_MIN) + ECART
    }

    fn actuel(&self) -> usize {
        (self.position.round().max(0.0) as usize).min(self.moments.len().saturating_sub(1))
    }

    fn defiler(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let d = e.delta.pixel_delta(px(16.0));
        let (dx, dy) = (f32::from(d.x), f32::from(d.y));
        let principal = if dx.abs() >= dy.abs() { dx } else { dy };
        match e.touch_phase {
            gpui::TouchPhase::Started => self.doigt_leve = false,
            gpui::TouchPhase::Ended => self.doigt_leve = true,
            gpui::TouchPhase::Moved => {}
        }
        // Aimanté (Julien, 28/11) : l'élan qui faiblit ne traîne pas entre deux vignettes, on va
        // tout de suite à la plus proche.
        if self.doigt_leve && principal.abs() < 3.0 {
            self.aimanter(cx);
            return;
        }
        self.aimant += 1;
        self.glissade = None;
        let dernier = self.moments.len().saturating_sub(1) as f32;
        // Une vignette de défilement vaut `densite` moments.
        self.position = (self.position - principal / self.pas() * self.densite() as f32).clamp(0.0, dernier);
        cx.notify();
        // Plus rien pendant 120 ms (molette, fin de l'élan) : on se pose sur la vignette la plus proche.
        let numero = self.aimant;
        cx.spawn(async move |frise, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
            let _ = frise.update(cx, |frise, cx| {
                if frise.aimant == numero {
                    frise.aimanter(cx);
                }
            });
        })
        .detach();
    }

    /// Glisse (140 ms, ralenti à la fin) jusqu'à la vignette la plus proche ; un nouveau geste
    /// l'arrête.
    fn aimanter(&mut self, cx: &mut Context<Self>) {
        let densite = self.densite() as f32;
        let dernier = self.moments.len().saturating_sub(1) as f32;
        let cible = ((self.position / densite).round() * densite).clamp(0.0, dernier);
        let depart = self.position;
        if (cible - depart).abs() < 0.001 || self.glissade == Some(cible) {
            return;
        }
        self.glissade = Some(cible);
        self.aimant += 1;
        let numero = self.aimant;
        cx.spawn(async move |frise, cx| {
            const ETAPES: u32 = 14;
            for i in 1..=ETAPES {
                cx.background_executor().timer(std::time::Duration::from_millis(10)).await;
                let t = i as f32 / ETAPES as f32;
                let lisse = 1.0 - (1.0 - t).powi(3);
                let encore = frise
                    .update(cx, |frise, cx| {
                        if frise.aimant != numero {
                            return false;
                        }
                        frise.position = depart + (cible - depart) * lisse;
                        if i == ETAPES {
                            frise.glissade = None;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !encore {
                    return;
                }
            }
        })
        .detach();
    }

    /// ⌘F : ouvre le champ de recherche et lui donne le clavier.
    fn ouvrir_recherche(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.recherche_ouverte {
            self.source.preparer_recherche();
        }
        self.recherche_ouverte = true;
        self.focus_recherche.focus(window);
        cx.notify();
    }

    /// Les touches tapées dans le champ de recherche.
    /// Une lettre tapée dans la frise (hors recherche) ouvre la recherche avec cette lettre.
    fn touche_frise(&mut self, evenement: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.recherche_ouverte {
            return;
        }
        let touche = &evenement.keystroke;
        if touche.modifiers.platform || touche.modifiers.control || touche.modifiers.alt {
            return;
        }
        let Some(caractere) = touche.key_char.clone() else {
            return;
        };
        if caractere.trim().is_empty() || caractere.chars().any(|c| c.is_control()) {
            return;
        }
        // ? : l'aide (ouvrir ou fermer).
        if caractere == "?" {
            self.aide_ouverte = !self.aide_ouverte;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        self.aide_ouverte = false;
        if !self.recherche_ouverte {
            self.source.preparer_recherche();
        }
        self.recherche_ouverte = true;
        self.recherche = caractere;
        self.focus_recherche.focus(window);
        self.lancer_recherche();
        cx.stop_propagation();
        cx.notify();
    }

    /// Les moments montrés dans la grille : celui de chaque groupe, dans l'ordre des groupes.
    fn resultats_grille(&self) -> Vec<usize> {
        self.groupes.iter().take(GROUPES_GRILLE).map(|g| g.vedette).collect()
    }

    /// Ouvre la grille et demande les vignettes de ses résultats (même loin de la tête de lecture).
    fn ouvrir_grille(&mut self) {
        self.grille_ouverte = true;
        for i in self.resultats_grille() {
            let moment = &self.moments[i];
            if self.verrouille(moment)
                || self.vignettes.contains_key(&moment.id)
                || self.echouees.contains(&moment.id)
                || !self.demandees.insert(moment.id)
            {
                continue;
            }
            let _ = self.envoi_vignettes.send(Demande {
                index: i,
                moment: moment.clone(),
                genre: Genre::Grille,
            });
        }
    }

    /// La recherche a changé : la grille ouverte montre les nouveaux résultats (ou se ferme s'il n'y en a plus).
    fn suivre_avec_la_grille(&mut self) {
        if self.grille_ouverte {
            if self.resultats.is_empty() {
                self.grille_ouverte = false;
            } else {
                self.ouvrir_grille();
            }
        }
    }

    /// Clic sur un résultat de la grille : on y va.
    fn choisir_dans_la_grille(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(rang) = self.groupes.iter().position(|g| g.vedette == index) {
            self.groupe_courant = rang;
        }
        self.position = index as f32;
        self.grille_ouverte = false;
        cx.notify();
    }

    /// Ferme la recherche : le champ disparaît, la frise reprend le clavier.
    fn fermer_recherche(&mut self, window: &mut Window) {
        self.grille_ouverte = false;
        self.recherche.clear();
        self.lancer_recherche();
        self.recherche_ouverte = false;
        self.focus.focus(window);
    }

    fn touche_recherche(&mut self, evenement: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let touche = &evenement.keystroke;
        match touche.key.as_str() {
            // Échap ferme d'abord la grille, puis la recherche.
            "escape" if self.grille_ouverte => self.grille_ouverte = false,
            "escape" => self.fermer_recherche(window),
            "tab" => {
                if self.grille_ouverte {
                    self.grille_ouverte = false;
                } else if !self.resultats.is_empty() || self.recherche_en_cours {
                    // Recherche en cours : la grille s'ouvre d'avance (vide, elle se refermera).
                    self.ouvrir_grille();
                }
            }
            "backspace" => {
                self.recherche.pop();
                if self.recherche.is_empty() {
                    self.fermer_recherche(window);
                } else {
                    self.lancer_recherche();
                    self.suivre_avec_la_grille();
                }
            }
            "enter" | "down" => self.aller_resultat(if touche.modifiers.shift { -1 } else { 1 }),
            "up" => self.aller_resultat(-1),
            _ => {
                if touche.modifiers.platform || touche.modifiers.control {
                    return;
                }
                let Some(caractere) = touche.key_char.clone() else {
                    return;
                };
                if caractere.chars().all(|c| !c.is_control()) {
                    self.recherche.push_str(&caractere);
                    self.lancer_recherche();
                    self.suivre_avec_la_grille();
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Le texte cherché, sans les filtres d'app (« @safari dauphins » → « dauphins »).
    fn requete_texte(&self) -> String {
        separer_filtres(&self.recherche).0
    }

    /// « @dit » (ou « @voix ») : seulement ce qui a été dit.
    fn filtre_voix(&self) -> bool {
        separer_filtres(&self.recherche).3
    }

    /// Les filtres d'app tapés (« @safari » → « safari »), en minuscules.
    fn filtres_apps(&self) -> Vec<String> {
        separer_filtres(&self.recherche).1
    }

    /// Les jours filtrés (« @hier » → la journée d'hier), en plages (début, fin) en ms.
    fn filtres_jours(&self) -> Vec<(i64, i64)> {
        let aujourd_hui = Local::now().date_naive();
        separer_filtres(&self.recherche)
            .2
            .into_iter()
            .filter_map(|j| {
                let jour = aujourd_hui - chrono::Duration::days(i64::from(j));
                let debut = Local.from_local_datetime(&jour.and_time(chrono::NaiveTime::MIN)).earliest()?;
                Some((debut.timestamp_millis(), debut.timestamp_millis() + 86_400_000 - 1))
            })
            .collect()
    }

    /// Le moment passe-t-il les filtres (apps et jours) ?
    fn passe_les_filtres(moment: &Moment, apps: &[String], jours: &[(i64, i64)]) -> bool {
        Self::dans_les_apps(moment, apps)
            && (jours.is_empty() || jours.iter().any(|&(a, z)| moment.quand_ms >= a && moment.quand_ms <= z))
    }

    /// Le moment vient-il d'une des apps filtrées (nom ou identifiant) ?
    fn dans_les_apps(moment: &Moment, filtres: &[String]) -> bool {
        filtres.is_empty()
            || filtres.iter().any(|f| {
                moment.app_nom.as_deref().is_some_and(|n| n.to_lowercase().contains(f.as_str()))
                    || moment.app_bundle.as_deref().is_some_and(|b| b.to_lowercase().contains(f.as_str()))
            })
    }

    /// Cherche ce qui est tapé. Les filtres seuls (« @hier », « @dit ») se calculent ici, sur les
    /// moments en mémoire ; le texte part au fil de recherche, et ses résultats arrivent dans
    /// [`Self::appliquer_trouvees`] (la grille suit ; sans résultat, elle se ferme).
    fn lancer_recherche(&mut self) {
        // Toute réponse encore attendue est dépassée.
        self.numero_recherche += 1;
        self.recherche_en_cours = false;
        let filtres = self.filtres_apps();
        let jours = self.filtres_jours();
        let voix_seulement = self.filtre_voix();
        let texte = self.requete_texte();
        if texte.chars().count() >= 2 {
            self.recherche_en_cours = self.envoi_recherche.send((self.numero_recherche, texte)).is_ok();
            return;
        }
        self.vider_resultats();
        if filtres.is_empty() && jours.is_empty() && !voix_seulement {
            return;
        }
        let mut extraits: HashMap<i64, String> = HashMap::new();
        let mut micro: HashMap<i64, bool> = HashMap::new();
        if voix_seulement {
            // « @dit » seul : chaque échange de voix (pas les moments privés).
            let mut indices: BTreeSet<usize> = BTreeSet::new();
            for p in self.passages.iter().filter(|p| !p.privee) {
                let i = self.moments.partition_point(|m| m.quand_ms <= p.debut_ms).saturating_sub(1);
                if self.moments.get(i).is_some_and(|m| Self::passe_les_filtres(m, &filtres, &jours)) {
                    indices.insert(i);
                    let id = self.moments[i].id;
                    self.resultats_entendus.insert(id);
                    micro.entry(id).or_insert(p.micro);
                    if let Some(t) = p.texte.as_deref().filter(|t| !t.is_empty()) {
                        extraits.entry(id).or_insert_with(|| extrait_autour(t, &[], 90));
                    }
                }
            }
            self.resultats = indices.into_iter().collect();
        } else {
            // « @safari » ou « @hier » seuls : tous les moments qui passent les filtres.
            self.resultats = (0..self.moments.len())
                .filter(|&i| Self::passe_les_filtres(&self.moments[i], &filtres, &jours))
                .collect();
        }
        self.resultats_ids = self.resultats.iter().map(|&i| self.moments[i].id).collect();
        self.resultats_vus = self.resultats_ids.clone();
        let marques = Marques {
            entendus: Some(&self.resultats_entendus),
            visibles: Some(&self.resultats_vus),
            extraits: Some(&extraits),
            micro: Some(&micro),
            ..Default::default()
        };
        self.groupes = regrouper(&self.moments, &self.resultats, &marques);
        if let Some(g) = self.groupes.first() {
            self.position = g.vedette as f32;
        }
    }

    /// Plus aucun résultat (avant d'en mettre de nouveaux).
    fn vider_resultats(&mut self) {
        self.resultats.clear();
        self.resultats_ids.clear();
        self.groupes.clear();
        self.groupe_courant = 0;
        self.resultats_vus.clear();
        self.resultats_entendus.clear();
        self.resultats_sens.clear();
        self.resultats_exacts.clear();
        self.rang_sens.clear();
    }

    /// Les textes trouvés par le fil de recherche pour la recherche en cours : gardés selon les
    /// filtres, regroupés par endroit, et on va au premier endroit (mot entier, le plus récent).
    fn appliquer_trouvees(&mut self, trouvees: Vec<Trouvee>) {
        self.recherche_en_cours = false;
        self.vider_resultats();
        self.reperes = None;
        self.reperes_demandes = None;
        let filtres = self.filtres_apps();
        let jours = self.filtres_jours();
        let voix_seulement = self.filtre_voix();
        let texte = self.requete_texte();
        let sites_caches = self.source.sites_caches();
        let mut indices: BTreeSet<usize> = BTreeSet::new();
        let mut entiers: HashSet<i64> = HashSet::new();
        let mut extraits: HashMap<i64, String> = HashMap::new();
        let mut micro: HashMap<i64, bool> = HashMap::new();
        let mots: Vec<String> = texte
            .split(|c: char| !c.is_alphanumeric())
            .filter(|m| m.chars().count() >= 2)
            .map(str::to_string)
            .collect();
        for t in &trouvees {
            // « @dit » : seulement ce qui a été dit (les voix sont l'« écran » 0).
            if voix_seulement && (t.ecran != 0 || t.sens) {
                continue;
            }
            let debut = self.moments.partition_point(|m| m.quand_ms < t.debut_ms);
            let mut retenus: Vec<usize> = Vec::new();
            for (i, m) in self.moments.iter().enumerate().skip(debut) {
                if m.quand_ms > t.fin_ms {
                    break;
                }
                retenus.push(i);
            }
            // Rien de capturé pendant la plage (l'écran ne bougeait pas pendant qu'on parlait) :
            // c'est l'image d'avant qui était à l'écran.
            if retenus.is_empty()
                && let Some(i) = debut.checked_sub(1)
            {
                retenus.push(i);
            }
            // Trouvé par le sens : un seul moment (le début), sinon un texte resté affiché une heure
            // (un titre, un menu) remplirait la grille.
            if t.sens {
                retenus.truncate(1);
            }
            for i in retenus {
                if !Self::passe_les_filtres(&self.moments[i], &filtres, &jours) {
                    continue;
                }
                if !sites_caches.is_empty()
                    && self.moments[i].url.as_deref().is_some_and(|u| bscreen_core::reglages::site_exclu(u, &sites_caches))
                {
                    continue;
                }
                indices.insert(i);
                let id = self.moments[i].id;
                if t.entier {
                    entiers.insert(id);
                }
                if let Some(lu) = &t.texte {
                    extraits
                        .entry(id)
                        .or_insert_with(|| extrait_autour(lu, if t.sens { &[] } else { &mots }, 90));
                }
                if t.sens {
                    let rang = self.rang_sens.len();
                    self.rang_sens.entry(id).or_insert(rang);
                } else {
                    self.resultats_exacts.insert(id);
                }
                if t.sens {
                    // Plusieurs textes peuvent mener au même moment (une page avec ses suggestions) :
                    // on les garde tous, dans l'ordre (le plus proche d'abord).
                    let textes = self.resultats_sens.entry(id).or_default();
                    if let Some(texte) = &t.texte
                        && !textes.contains(texte)
                    {
                        textes.push(texte.clone());
                    }
                } else if t.ecran == 0 {
                    self.resultats_entendus.insert(id);
                    // Au micro ou au son du Mac ? (le passage qui contient ce moment de voix)
                    let debut_p = self.passages.partition_point(|p| p.debut_ms <= t.debut_ms);
                    if let Some(p) = debut_p.checked_sub(1).map(|k| &self.passages[k]).filter(|p| p.fin_ms >= t.debut_ms) {
                        micro.entry(id).or_insert(p.micro);
                    }
                } else if t.vu {
                    self.resultats_vus.insert(id);
                }
            }
        }
        self.resultats = indices.into_iter().collect();
        self.resultats_ids = self.resultats.iter().map(|&i| self.moments[i].id).collect();
        // Regroupés par endroit ; on commence par le premier groupe (mot entier, le plus récent).
        let marques = Marques {
            entendus: Some(&self.resultats_entendus),
            visibles: Some(&self.resultats_vus),
            exacts: Some(&self.resultats_exacts),
            rang_sens: Some(&self.rang_sens),
            entiers: Some(&entiers),
            extraits: Some(&extraits),
            micro: Some(&micro),
        };
        self.groupes = regrouper(&self.moments, &self.resultats, &marques);
        tracing::info!(moments = self.resultats.len(), endroits = self.groupes.len(), "recherche regroupée");
        if let Some(g) = self.groupes.first() {
            self.position = g.vedette as f32;
        }
    }

    /// Le moment où quelqu'un parlait juste après (+1) ou juste avant (-1) celui affiché.
    fn aller_a_voix(&mut self, sens: isize, cx: &mut Context<Self>) {
        if self.moments.is_empty() {
            return;
        }
        let actuel = self.actuel();
        let quand = self.moments[actuel].quand_ms;
        // Le moment à l'écran quand un passage commence.
        let moment_de = |debut: i64| self.moments.partition_point(|m| m.quand_ms <= debut).saturating_sub(1);
        let cible = if sens > 0 {
            self.passages
                .iter()
                .filter(|p| p.debut_ms > quand)
                .map(|p| moment_de(p.debut_ms))
                .find(|&i| i > actuel)
        } else {
            self.passages
                .iter()
                .rev()
                .filter(|p| p.debut_ms < quand)
                .map(|p| moment_de(p.debut_ms))
                .find(|&i| i < actuel)
        };
        match cible {
            Some(i) => {
                self.position = i as f32;
                self.selection_texte = None;
            }
            None => self.annoncer(
                if sens > 0 { "Aucune voix après ce moment" } else { "Aucune voix avant ce moment" }.to_string(),
                cx,
            ),
        }
        cx.notify();
    }

    /// Endroit suivant (+1) ou précédent (-1), dans l'ordre de la grille.
    fn aller_resultat(&mut self, sens: isize) {
        if self.groupes.is_empty() {
            return;
        }
        let n = self.groupes.len() as isize;
        self.groupe_courant = ((self.groupe_courant as isize + sens).rem_euclid(n)) as usize;
        self.position = self.groupes[self.groupe_courant].vedette as f32;
    }

    /// Demande au décodeur (libre) la grande image du moment suivant, dans le sens du déplacement.
    fn precharger(&mut self) {
        let suivant = self.actuel() as isize + self.sens;
        let Some(moment) = usize::try_from(suivant).ok().and_then(|i| self.moments.get(i)) else {
            return;
        };
        let deja = self.prechargee.as_ref().map(|p| p.0) == Some(moment.id)
            || self.grande.as_ref().map(|g| g.0) == Some(moment.id);
        if deja || self.verrouille(moment) || self.echouees.contains(&moment.id) {
            return;
        }
        let _ = self.envoi_grande.send(Demande {
            index: suivant as usize,
            moment: moment.clone(),
            genre: Genre::Prechargee,
        });
    }

    /// Ce moment n'est-il un résultat que par le sens (pas par les mots tapés) ?
    fn seulement_par_le_sens(&self, id: i64) -> bool {
        self.resultats_sens.contains_key(&id)
            && !self.resultats_vus.contains(&id)
            && !self.resultats_entendus.contains(&id)
    }

    /// Un moment privé encore verrouillé ?
    fn verrouille(&self, moment: &Moment) -> bool {
        moment.privee && !self.deverrouille
    }

    /// Touch ID pour voir les moments privés (5 minutes, ou jusqu'à la fermeture de la frise).
    fn deverrouiller(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deverrouille || self.deverrouillage_en_cours {
            return;
        }
        let Ok(poignee) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(appkit) = poignee.as_raw() else { return };
        let vue = appkit.ns_view.as_ptr();
        self.deverrouillage_en_cours = true;
        let (envoi, reception) = futures::channel::oneshot::channel::<bool>();
        self.source.deverrouiller(
            vue,
            Box::new(move |accorde| {
                let _ = envoi.send(accorde);
            }),
        );
        cx.spawn(async move |vue, cx| {
            let accorde = reception.await.unwrap_or(false);
            let _ = vue.update(cx, |frise, cx| {
                frise.deverrouillage_en_cours = false;
                if accorde {
                    frise.deverrouille = true;
                    frise.numero_deverrouillage += 1;
                    let numero = frise.numero_deverrouillage;
                    frise.annoncer("Moments privés visibles pendant 5 minutes".to_string(), cx);
                    cx.spawn(async move |vue, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(300)).await;
                        let _ = vue.update(cx, |frise, cx| {
                            if frise.deverrouille && frise.numero_deverrouillage == numero {
                                frise.reverrouiller();
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Les moments privés redeviennent hachurés ; leurs images décodées sont oubliées.
    fn reverrouiller(&mut self) {
        self.deverrouille = false;
        let privees: HashSet<i64> = self.moments.iter().filter(|m| m.privee).map(|m| m.id).collect();
        for id in &privees {
            if let Some(image) = self.vignettes.remove(id) {
                self.a_liberer.push(image);
            }
            self.demandees.remove(id);
        }
        if let Some((id, _)) = &self.grande
            && privees.contains(id)
            && let Some((_, image)) = self.grande.take()
        {
            self.a_liberer.push(image);
        }
        if let Some((id, _)) = &self.prechargee
            && privees.contains(id)
            && let Some((_, image)) = self.prechargee.take()
        {
            self.a_liberer.push(image);
        }
        if self.flou_carte.as_ref().is_some_and(|(id, _)| privees.contains(id)) {
            self.flou_carte = None;
            self.flou_champ = None;
        }
        self.lecture = None;
        self.selection_texte = None;
    }

    /// On tape un nom : celui d'une voix (« Renommer… »), ou d'une nouvelle voix pour un passage.
    fn commencer_nommage(&mut self, cible: CibleNom, nom: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // « Voix 3 » n'est pas un nom : on part d'un champ vide.
        let depart = nom.filter(|n| !n.starts_with("Voix ") && n != "Toi").unwrap_or_default();
        self.choix_voix = None;
        self.nommage = Some((cible, depart));
        self.focus_nommage.focus(window);
        cx.notify();
    }

    /// Clic sur le nom de la voix d'un passage : qui parle ? (les voix connues, une nouvelle, « ? »).
    fn ouvrir_choix_voix(&mut self, passage: &Passage, cx: &mut Context<Self>) {
        self.choix_voix = Some(ChoixVoix {
            passage: passage.id,
            debut_ms: passage.debut_ms,
            locuteur_id: passage.locuteur_id,
            nom: passage.locuteur.clone(),
            proposees: self.source.voix_connues(),
        });
        cx.notify();
    }

    /// Corrige la voix d'un passage (seulement lui) ; la frise le montre tout de suite.
    fn corriger_voix(&mut self, passage: i64, voix: VoixChoisie, cx: &mut Context<Self>) {
        self.choix_voix = None;
        match self.source.corriger_voix(passage, voix) {
            Some((locuteur_id, nom)) => {
                if let Some(p) = self.passages.iter_mut().find(|p| p.id == passage) {
                    p.locuteur_id = locuteur_id;
                    p.locuteur = nom.clone();
                }
                self.annoncer(format!("Passage rangé : {}", nom.unwrap_or_else(|| "?".to_string())), cx);
            }
            None => self.annoncer("Pas pu corriger ce passage".to_string(), cx),
        }
        cx.notify();
    }

    fn touche_nommage(&mut self, evenement: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cible, saisie)) = self.nommage.as_mut() else { return };
        let touche = &evenement.keystroke;
        match touche.key.as_str() {
            "escape" => {
                self.nommage = None;
                self.focus.focus(window);
            }
            "enter" => {
                let (cible, nom) = (*cible, saisie.trim().to_string());
                self.nommage = None;
                self.focus.focus(window);
                if let CibleNom::Passage(passage) = cible {
                    if !nom.is_empty() {
                        self.corriger_voix(passage, VoixChoisie::Nouvelle(nom), cx);
                    }
                } else if let CibleNom::Voix(id) = cible
                    && !nom.is_empty()
                {
                    if self.source.nommer_voix(id, &nom) {
                        self.passages = self.source.voix();
                        self.passages.sort_by_key(|p| p.debut_ms);
                        self.activite_pour = (0, 0);
                        self.annoncer(format!("Voix reconnue : {nom}"), cx);
                    } else {
                        self.annoncer("Nom pas enregistré".to_string(), cx);
                    }
                }
            }
            "backspace" => {
                saisie.pop();
            }
            _ => {
                if !touche.modifiers.platform
                    && !touche.modifiers.control
                    && let Some(caractere) = touche.key_char.as_ref()
                    && !caractere.chars().any(|c| c.is_control())
                    && saisie.chars().count() < 40
                {
                    saisie.push_str(caractere);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Le passage choisi parmi ceux du moment affiché (↑ / ↓ ou clic) ; sinon celui où est le mot
    /// cherché, sinon le premier.
    fn rang_choisi(&self, actuel: usize, dits: &[Passage]) -> usize {
        if let Some((moment, rang)) = self.passage_choisi
            && moment == actuel
            && rang < dits.len()
        {
            return rang;
        }
        let requete = self.requete_texte();
        dits.iter()
            .position(|p| {
                !requete.is_empty() && p.texte.as_deref().is_some_and(|t| !plages_cherchees(t, &requete).is_empty())
            })
            .unwrap_or(0)
    }

    /// Espace : joue le passage choisi de ce moment ; une deuxième fois : arrête.
    fn ecouter(&mut self, cx: &mut Context<Self>) {
        if self.ecoute.take().is_some() {
            self.source.arreter_ecoute();
            cx.notify();
            return;
        }
        let actuel = self.actuel();
        let dits: Vec<Passage> = self.passages_du_moment(actuel).into_iter().cloned().collect();
        if dits.is_empty() {
            let cache = self.moments.get(actuel).is_some_and(|m| self.verrouille(m));
            let message = if cache { "Moment privé : Entrée pour déverrouiller" } else { "Rien n'a été dit à ce moment" };
            self.annoncer(message.to_string(), cx);
            return;
        }
        let rang = self.rang_choisi(actuel, &dits);
        self.jouer(dits[rang].clone(), cx);
    }

    /// ↑ / ↓ : un autre passage de ce moment, s'il y en a plusieurs ; pendant l'écoute, on passe à lui.
    fn choisir_passage(&mut self, sens: isize, cx: &mut Context<Self>) {
        let actuel = self.actuel();
        let dits: Vec<Passage> = self.passages_du_moment(actuel).into_iter().cloned().collect();
        if dits.len() < 2 {
            return;
        }
        let rang = (self.rang_choisi(actuel, &dits) as isize + sens).rem_euclid(dits.len() as isize) as usize;
        self.passage_choisi = Some((actuel, rang));
        if self.ecoute.is_some() {
            self.jouer(dits[rang].clone(), cx);
        }
        cx.notify();
    }

    /// Clic sur une phrase : l'écouter (ou arrêter, si c'est elle qu'on écoute).
    fn cliquer_passage(&mut self, rang: usize, cx: &mut Context<Self>) {
        let actuel = self.actuel();
        let Some(passage) = self.passages_du_moment(actuel).get(rang).map(|p| (*p).clone()) else {
            return;
        };
        self.passage_choisi = Some((actuel, rang));
        if self.ecoute.is_some_and(|(debut, _)| debut == passage.debut_ms) {
            self.ecoute = None;
            self.source.arreter_ecoute();
            cx.notify();
            return;
        }
        self.jouer(passage, cx);
    }

    /// Joue un passage depuis son début (celui en cours s'arrête).
    fn jouer(&mut self, passage: Passage, cx: &mut Context<Self>) {
        if self.ecoute.take().is_some() {
            self.source.arreter_ecoute();
        }
        let Some(duree) = self.source.ecouter(&passage, 0.0) else {
            self.annoncer("Ce passage ne peut pas être lu".to_string(), cx);
            return;
        };
        self.numero_ecoute += 1;
        let numero = self.numero_ecoute;
        self.ecoute = Some((passage.debut_ms, numero));
        cx.spawn(async move |vue, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs_f64(duree.max(0.1) + 0.2))
                .await;
            let _ = vue.update(cx, |frise, cx| {
                if frise.ecoute.is_some_and(|(_, n)| n == numero) {
                    frise.ecoute = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// La rangée « Qui parle à 16:42 ? » : les voix connues (celle du passage en évidence), une
    /// nouvelle, « ? », renommer la voix (tous ses passages), annuler.
    fn rangee_choix_voix(&self, choix: &ChoixVoix, cx: &mut Context<Self>) -> gpui::Div {
        let bouton = |id: SharedString, texte: String, choisi: bool| {
            div()
                .id(id)
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(9.0))
                .cursor_pointer()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .when(choisi, |d| d.bg(rgb(0xFFFFFF)).text_color(rgb(0x1D1D1F)))
                .when(!choisi, |d| d.bg(rgba(0xFFFFFF26)).text_color(rgb(0xFFFFFF)).hover(|s| s.bg(rgba(0xFFFFFF4D))))
                .child(texte)
        };
        let passage = choix.passage;
        let mut rangee = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(14.0))
            .bg(rgba(0x000000D9))
            .shadow_lg()
            .child(
                div()
                    .pr(px(4.0))
                    .text_size(px(13.0))
                    .text_color(rgb(0xEBEBF5))
                    .child(format!("Qui parle à {} ?", heure(choix.debut_ms, false))),
            );
        for (id, nom) in &choix.proposees {
            let id = *id;
            rangee = rangee.child(
                bouton(format!("voix-{id}").into(), nom.clone(), Some(id) == choix.locuteur_id).on_click(cx.listener(
                    move |frise, _: &ClickEvent, _, cx| frise.corriger_voix(passage, VoixChoisie::Connue(id), cx),
                )),
            );
        }
        rangee = rangee.child(bouton("voix-nouvelle".into(), "Nouvelle voix…".to_string(), false).on_click(
            cx.listener(move |frise, _: &ClickEvent, window, cx| {
                frise.commencer_nommage(CibleNom::Passage(passage), None, window, cx)
            }),
        ));
        if let Some(id) = choix.locuteur_id {
            let nom = choix.nom.clone();
            rangee = rangee
                .child(bouton("voix-aucune".into(), "?".to_string(), false).on_click(cx.listener(
                    move |frise, _: &ClickEvent, _, cx| frise.corriger_voix(passage, VoixChoisie::Aucune, cx),
                )))
                .child(
                    bouton(
                        "voix-renommer".into(),
                        format!("Renommer « {} »…", nom.clone().unwrap_or_else(|| format!("Voix {id}"))),
                        false,
                    )
                    .on_click(cx.listener(move |frise, _: &ClickEvent, window, cx| {
                        frise.commencer_nommage(CibleNom::Voix(id), nom.clone(), window, cx)
                    })),
                );
        }
        rangee.child(
            bouton("voix-annuler".into(), "Annuler".to_string(), false).on_click(cx.listener(
                |frise, _: &ClickEvent, _, cx| {
                    frise.choix_voix = None;
                    cx.notify();
                },
            )),
        )
    }

    /// Affiche un message bref (2 s) au-dessus de la barre.
    fn annoncer(&mut self, texte: String, cx: &mut Context<Self>) {
        let numero = self.message.as_ref().map(|(_, n)| n + 1).unwrap_or(0);
        self.message = Some((texte, numero));
        cx.spawn(async move |vue, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let _ = vue.update(cx, |frise, cx| {
                if frise.message.as_ref().is_some_and(|(_, n)| *n == numero) {
                    frise.message = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// ⌘C : copie le texte visible de l'image affichée.
    fn copier(&mut self, cx: &mut Context<Self>) {
        if self.moments.is_empty() {
            return;
        }
        if self.verrouille(&self.moments[self.actuel()]) {
            self.annoncer("Moment privé : Entrée pour le déverrouiller".to_string(), cx);
            return;
        }
        // Une sélection sur l'image : on copie exactement elle.
        if let Some(texte) = self.texte_selectionne() {
            let n = texte.split_whitespace().count();
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(texte));
            self.annoncer(
                format!("Sélection copiée ({n} mot{})", if n > 1 { "s" } else { "" }),
                cx,
            );
            return;
        }
        let lignes = self.source.texte_visible(&self.moments[self.actuel()]);
        if lignes.is_empty() {
            self.annoncer("Pas de texte lu à ce moment".to_string(), cx);
            return;
        }
        let n = lignes.len();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(lignes.join("\n")));
        self.annoncer(format!("Texte copié ({n} ligne{})", if n > 1 { "s" } else { "" }), cx);
    }

    /// ⌘O : rouvre la page web affichée à ce moment.
    fn ouvrir_page(&mut self, cx: &mut Context<Self>) {
        if self.moments.is_empty() {
            return;
        }
        match self.moments[self.actuel()].url.clone() {
            Some(url) => {
                cx.open_url(&url);
                self.annoncer("Page ouverte dans le navigateur".to_string(), cx);
            }
            None => self.annoncer("Pas de page web à ce moment".to_string(), cx),
        }
    }

    /// ⌘← / ⌘→ : la fin du jour précédent, ou le début du jour suivant.
    fn changer_de_jour(&mut self, sens: i32, cx: &mut Context<Self>) {
        if self.moments.is_empty() {
            return;
        }
        let jour = |ms: i64| Local.timestamp_millis_opt(ms).single().map(|d| d.date_naive());
        let Some(aujourdhui) = jour(self.moments[self.actuel()].quand_ms) else {
            return;
        };
        let cible = if sens < 0 {
            self.moments[..self.actuel()]
                .iter()
                .rposition(|m| jour(m.quand_ms).is_some_and(|j| j < aujourdhui))
        } else {
            self.moments
                .iter()
                .enumerate()
                .skip(self.actuel())
                .find(|(_, m)| jour(m.quand_ms).is_some_and(|j| j > aujourdhui))
                .map(|(i, _)| i)
        };
        match cible {
            Some(i) => {
                self.position = i as f32;
                cx.notify();
            }
            None => {
                let texte = if sens < 0 {
                    "Pas d'enregistrement plus ancien"
                } else {
                    "C'est le jour le plus récent"
                };
                self.annoncer(texte.to_string(), cx);
            }
        }
    }

    /// Barre d'activité regroupée par tranches de 3 points (couleur de l'app dominante),
    /// pour rester légère même avec des dizaines de milliers de moments.
    fn calculer_activite(&mut self, largeur: f32) {
        let cle = (self.moments.len(), largeur as u32);
        if self.activite_pour == cle {
            return;
        }
        self.activite_pour = cle;
        self.activite.clear();
        let total = self.moments.len();
        if total == 0 {
            return;
        }
        let nb = ((largeur / 3.0).floor() as usize).clamp(1, total);
        let mut couleurs_apps: HashMap<String, u32> = HashMap::new();
        for i in 0..nb {
            let milieu = ((2 * i + 1) * total / (2 * nb)).min(total - 1);
            let app = self.moments[milieu].app_bundle.clone().unwrap_or_default();
            let couleur = match couleurs_apps.get(&app) {
                Some(c) => *c,
                None => {
                    let c = if app.is_empty() {
                        0x8E8E93
                    } else {
                        self.icone(&app)
                            .map(|(_, c)| c)
                            .unwrap_or_else(|| couleur_par_defaut(&app))
                    };
                    couleurs_apps.insert(app, c);
                    c
                }
            };
            let x = i as f32 * largeur / nb as f32;
            let l = largeur / nb as f32;
            match self.activite.last_mut() {
                Some((_, largeur_prec, c)) if *c == couleur => *largeur_prec += l,
                _ => self.activite.push((x, l, couleur)),
            }
        }
        // Les voix : un trait sous les tranches, là où quelqu'un parlait (fusionné quand c'est contigu).
        self.activite_voix.clear();
        for p in &self.passages {
            let a = self.moments.partition_point(|m| m.quand_ms <= p.debut_ms).saturating_sub(1);
            let b = self
                .moments
                .partition_point(|m| m.quand_ms <= p.fin_ms)
                .saturating_sub(1)
                .max(a);
            let x = a as f32 / total as f32 * largeur;
            let l = ((b - a + 1) as f32 / total as f32 * largeur).max(2.0);
            match self.activite_voix.last_mut() {
                Some((x0, l0, micro)) if *micro == p.micro && x <= *x0 + *l0 + 1.0 => *l0 = (x + l - *x0).max(*l0),
                _ => self.activite_voix.push((x, l, p.micro)),
            }
        }
    }

    /// Les passages de voix pendant qu'un moment était à l'écran (jusqu'au moment suivant, 2 min au plus).
    fn passages_du_moment(&self, actuel: usize) -> Vec<&Passage> {
        let Some(moment) = self.moments.get(actuel) else {
            return Vec::new();
        };
        let debut = moment.quand_ms - 1_000;
        let fin = self
            .moments
            .get(actuel + 1)
            .map(|m| m.quand_ms)
            .unwrap_or(moment.quand_ms + 10_000)
            .min(moment.quand_ms + 120_000);
        // Un passage dure 30 s au plus : on part de là.
        let depart = self.passages.partition_point(|p| p.debut_ms < debut - 30_000);
        self.passages[depart..]
            .iter()
            .take_while(|p| p.debut_ms < fin)
            .filter(|p| p.fin_ms > debut)
            .filter(|p| !p.privee || self.deverrouille)
            .collect()
    }

    /// ⌥⌫ : la sélection, sinon l'image en cours ; ⌫ / ⌦ (`moment_affiche` faux) : la sélection
    /// seulement. On demande confirmation.
    fn demander_suppression(&mut self, moment_affiche: bool, cx: &mut Context<Self>) {
        if self.moments.is_empty() {
            return;
        }
        let ids: Vec<i64> = if !self.selection.is_empty() {
            self.selection.iter().copied().collect()
        } else if moment_affiche {
            vec![self.moments[self.actuel()].id]
        } else {
            return;
        };
        self.confirmation = Some(ids);
        cx.notify();
    }

    /// Entrée : la suppression confirmée part (base + vidéo), la frise se met à jour tout de suite.
    fn confirmer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ids) = self.confirmation.take() else {
            // Rien à confirmer : Entrée déverrouille le moment privé affiché.
            if self.moments.get(self.actuel()).is_some_and(|m| self.verrouille(m)) {
                self.deverrouiller(window, cx);
            }
            return;
        };
        let a_retirer: HashSet<i64> = ids.iter().copied().collect();
        let restants_avant = self.moments[..self.actuel()]
            .iter()
            .filter(|m| !a_retirer.contains(&m.id))
            .count();
        self.moments.retain(|m| !a_retirer.contains(&m.id));
        for id in &ids {
            if let Some(image) = self.vignettes.remove(id) {
                let _ = window.drop_image(image);
            }
            self.demandees.remove(id);
            self.echouees.remove(id);
        }
        if let Some((id, _)) = &self.grande
            && a_retirer.contains(id)
            && let Some((_, image)) = self.grande.take()
        {
            let _ = window.drop_image(image);
        }
        if self.flou_carte.as_ref().is_some_and(|(id, _)| a_retirer.contains(id))
            && let Some((_, image)) = self.flou_carte.take()
        {
            let _ = window.drop_image(image);
        }
        if self
            .flou_champ
            .as_ref()
            .is_some_and(|(id, _, _)| a_retirer.contains(id))
            && let Some((_, image, _)) = self.flou_champ.take()
        {
            let _ = window.drop_image(image);
        }
        self.grande_demandee = None;
        self.selection.clear();
        self.ancre = None;
        self.survolee = None;
        self.position = (restants_avant as f32).min(self.moments.len().saturating_sub(1) as f32);
        self.source.supprimer(ids);
        cx.notify();
    }

    /// ⌥⇧← / ⌥⇧→ : étend la sélection depuis son point de départ.
    fn etendre(&mut self, de: isize, cx: &mut Context<Self>) {
        if self.moments.is_empty() {
            return;
        }
        let actuel = self.actuel();
        let ancre = *self.ancre.get_or_insert(actuel);
        let nouveau = (actuel as isize + de).clamp(0, self.moments.len() as isize - 1) as usize;
        self.position = nouveau as f32;
        let (debut, fin) = (ancre.min(nouveau), ancre.max(nouveau));
        self.selection = self.moments[debut..=fin].iter().map(|m| m.id).collect();
        cx.notify();
    }

    /// ⌥-clic : ajoute ou retire une vignette de la sélection.
    fn basculer_selection(&mut self, i: usize, cx: &mut Context<Self>) {
        let id = self.moments[i].id;
        if !self.selection.remove(&id) {
            self.selection.insert(id);
        }
        self.ancre = Some(i);
        cx.notify();
    }

    /// Échap : efface la sélection de texte ou annule la confirmation, puis la sélection
    /// de vignettes, puis ferme la frise.
    fn fermer_ou_annuler(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choix_voix.take().is_some() {
            cx.notify();
        } else if self.aide_ouverte {
            self.aide_ouverte = false;
            cx.notify();
        } else if self.selection_texte.take().is_some() || self.confirmation.take().is_some() {
            cx.notify();
        } else if !self.selection.is_empty() {
            self.selection.clear();
            self.ancre = None;
            cx.notify();
        } else {
            window.remove_window();
        }
    }

    fn sauter(&mut self, de: f32, cx: &mut Context<Self>) {
        let dernier = self.moments.len().saturating_sub(1) as f32;
        // Réduite, une flèche passe à la vignette suivante (plusieurs moments).
        self.position = (self.position.round() + de * self.densite() as f32).clamp(0.0, dernier);
        cx.notify();
    }

    /// Icône (et couleur moyenne) d'une app, chargée une fois.
    fn icone(&mut self, identifiant: &str) -> Option<(Arc<RenderImage>, u32)> {
        if !self.icones.contains_key(identifiant) {
            let icone = self.source.icone_app(identifiant).and_then(|image| {
                let couleur = couleur_moyenne(&image);
                en_rendu(image).map(|rendu| (rendu, couleur))
            });
            self.icones.insert(identifiant.to_string(), icone);
        }
        self.icones.get(identifiant).cloned().flatten()
    }

    /// Demande les images qui manquent autour de la tête de lecture, libère les lointaines.
    fn demander_images(&mut self, visibles: &[usize], ecran: (f32, f32), window: &mut Window) {
        let actuel = self.actuel();
        self.courant.store(actuel as i64, Ordering::Relaxed);
        // La plus proche d'abord.
        let mut indices: Vec<usize> = visibles.to_vec();
        indices.sort_by_key(|&i| (i as i64 - actuel as i64).unsigned_abs());
        for i in indices {
            let moment = &self.moments[i];
            if self.verrouille(moment) {
                continue;
            }
            if !self.vignettes.contains_key(&moment.id)
                && !self.echouees.contains(&moment.id)
                && self.demandees.insert(moment.id)
            {
                let _ = self.envoi_vignettes.send(Demande {
                    index: i,
                    moment: moment.clone(),
                    genre: Genre::Vignette,
                });
            }
        }
        let id_actuel = self.moments[actuel].id;
        let actuel_verrouille = self.verrouille(&self.moments[actuel]);
        // Le sens du déplacement (pour préparer le moment suivant).
        if let Some(avant) = self.dernier_actuel
            && avant != actuel
        {
            self.sens = if actuel > avant { 1 } else { -1 };
        }
        self.dernier_actuel = Some(actuel);
        // Déjà préparée : elle sert tout de suite, sans nouveau décodage.
        if !actuel_verrouille
            && self.grande.as_ref().map(|(id, _)| *id) != Some(id_actuel)
            && self.prechargee.as_ref().map(|(id, _)| *id) == Some(id_actuel)
            && let Some(prete) = self.prechargee.take()
        {
            if let Some((_, ancienne)) = self.grande.replace(prete) {
                let _ = window.drop_image(ancienne);
            }
            self.precharger();
        }
        if !actuel_verrouille
            && self.grande.as_ref().map(|(id, _)| *id) != Some(id_actuel)
            && self.grande_demandee != Some(id_actuel)
            && !self.echouees.contains(&id_actuel)
        {
            self.grande_demandee = Some(id_actuel);
            let _ = self.envoi_grande.send(Demande {
                index: actuel,
                moment: self.moments[actuel].clone(),
                genre: Genre::Grande,
            });
        }
        // Libère la mémoire des vignettes lointaines (et leur place dans le GPU).
        let mut garder: HashSet<i64> = self.moments
            [actuel.saturating_sub(GARDE)..(actuel + GARDE + 1).min(self.moments.len())]
            .iter()
            .map(|m| m.id)
            .collect();
        // Réduite, la bande montre des moments plus lointains : ceux-là restent aussi.
        garder.extend(visibles.iter().map(|&i| self.moments[i].id));
        // La grille ouverte garde les vignettes de ses résultats.
        if self.grille_ouverte {
            garder.extend(self.resultats_grille().into_iter().map(|i| self.moments[i].id));
        }
        let lointaines: Vec<i64> = self
            .vignettes
            .keys()
            .copied()
            .filter(|id| !garder.contains(id))
            .collect();
        for id in lointaines {
            if let Some(image) = self.vignettes.remove(&id) {
                let _ = window.drop_image(image);
            }
        }
        // Fonds flous : la vignette du moment courant, floutée, puis découpée pour la barre
        // et pour le champ (chacun a ses coins arrondis à lui).
        if self.flou_carte.as_ref().map(|(id, _)| *id) != Some(id_actuel)
            && let Some(vignette) = self.vignettes.get(&id_actuel)
            && let Some(octets) = vignette.as_bytes(0)
        {
            let taille = vignette.size(0);
            let floue = ImageBgra {
                largeur: u32::from(taille.width),
                hauteur: u32::from(taille.height),
                pixels: octets.to_vec(),
            }
            .floutee(12, 3);
            let (largeur_ecran, hauteur_ecran) = ecran;
            let haut_carte = (hauteur_ecran - HAUTEUR_CARTE) / hauteur_ecran;
            if let Some(rendu) = en_rendu(floue.rognee(0.0, haut_carte, 1.0, 1.0 - haut_carte))
                && let Some((_, ancien)) = self.flou_carte.replace((id_actuel, rendu))
            {
                let _ = window.drop_image(ancien);
            }
            let (x, y, l, h) = rect_champ(largeur_ecran, self.marge_haute);
            let fond = floue.rognee(
                x / largeur_ecran,
                y / hauteur_ecran,
                l / largeur_ecran,
                h / hauteur_ecran,
            );
            let clair = fond.luminance() > 150.0;
            if let Some(rendu) = en_rendu(fond)
                && let Some((_, ancien, _)) = self.flou_champ.replace((id_actuel, rendu, clair))
            {
                let _ = window.drop_image(ancien);
            }
        }
        // Recherche : demande à l'OCR où le mot est visible sur l'image courante.
        let requete = self.requete_texte();
        // (Pas pour un moment trouvé seulement par le sens : ses mots n'y sont pas, on surlignerait « avec ».)
        if !actuel_verrouille
            && self.recherche_ouverte
            && requete.chars().count() >= 2
            && self.resultats_ids.contains(&id_actuel)
            && !self.seulement_par_le_sens(id_actuel)
        {
            let deja = self
                .reperes
                .as_ref()
                .is_some_and(|(id, r, _)| *id == id_actuel && *r == requete)
                || self
                    .reperes_demandes
                    .as_ref()
                    .is_some_and(|(id, r)| *id == id_actuel && *r == requete);
            if !deja {
                self.reperes_demandes = Some((id_actuel, requete.clone()));
                let _ = self.envoi_reperes.send((self.moments[actuel].clone(), requete));
            }
        }
        // Lecture mot à mot, une fois l'image nette affichée (pour sélectionner le texte).
        if self.grande.as_ref().is_some_and(|(id, _)| *id == id_actuel)
            && self.lecture.as_ref().map(|(id, _)| *id) != Some(id_actuel)
            && self.lecture_demandee != Some(id_actuel)
        {
            self.lecture_demandee = Some(id_actuel);
            let _ = self.envoi_lecture.send(self.moments[actuel].clone());
        }
    }

    /// Les lignes lues sur l'image courante (si la lecture est pour ce moment).
    fn lignes_courantes(&self) -> Option<&[LigneLue]> {
        let (id, lignes) = self.lecture.as_ref()?;
        (self.moments.get(self.actuel()).map(|m| m.id) == Some(*id)).then_some(lignes.as_slice())
    }

    /// Un point de l'écran, en coordonnées normalisées de l'image, et le rapport largeur / hauteur.
    fn normaliser(&self, x: f32, y: f32) -> (f64, f64, f64) {
        let (l, h) = self.taille_ecran;
        let (l, h) = (f64::from(l.max(1.0)), f64::from(h.max(1.0)));
        (f64::from(x) / l, f64::from(y) / h, l / h)
    }

    /// Le texte sélectionné sur l'image, au caractère près (une ligne lue par ligne).
    fn texte_selectionne(&self) -> Option<String> {
        let (a, b, id) = self.selection_texte?;
        if self.moments.get(self.actuel()).map(|m| m.id) != Some(id) || a == b {
            return None;
        }
        let texte = texte_entre(self.lignes_courantes()?, a, b);
        (!texte.trim().is_empty()).then_some(texte)
    }

    /// Clic sur l'image : place le début de la sélection (rien n'est sélectionné tant qu'on ne glisse
    /// pas) ; double-clic : le mot ; triple-clic : la ligne.
    fn souris_appuyee(&mut self, e: &MouseDownEvent, cx: &mut Context<Self>) {
        let (x, y, rapport) = self.normaliser(f32::from(e.position.x), f32::from(e.position.y));
        let id = self.moments[self.actuel()].id;
        let position = self.lignes_courantes().and_then(|l| position_sous(l, x, y, rapport));
        let Some(p) = position else {
            self.selection_texte = None;
            self.glisse = false;
            cx.notify();
            return;
        };
        let lignes = self.lignes_courantes().unwrap_or(&[]);
        let (a, b) = match e.click_count {
            2 => mot_autour(lignes, p),
            n if n >= 3 => ligne_autour(lignes, p),
            _ => (p, p),
        };
        self.selection_texte = Some((a, b, id));
        self.glisse = e.click_count < 2;
        cx.notify();
    }

    fn souris_deplacee(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let (x, y, rapport) = self.normaliser(f32::from(e.position.x), f32::from(e.position.y));
        let sur_texte = self.lignes_courantes().is_some_and(|l| sur_du_texte(l, x, y));
        let mut change = sur_texte != self.curseur_texte;
        self.curseur_texte = sur_texte;
        if self.glisse
            && e.pressed_button == Some(MouseButton::Left)
            && let Some((debut, fin, id)) = self.selection_texte
            && let Some(p) = self.lignes_courantes().and_then(|l| position_sous(l, x, y, rapport))
            && p != fin
        {
            self.selection_texte = Some((debut, p, id));
            change = true;
        }
        if change {
            cx.notify();
        }
    }
}

/// Le champ de recherche : (x, y, largeur, hauteur), en points — sous l'encoche de la caméra.
fn rect_champ(largeur_ecran: f32, marge_haute: f32) -> (f32, f32, f32, f32) {
    let largeur = (largeur_ecran - 160.0).clamp(320.0, 860.0);
    (
        (largeur_ecran - largeur) / 2.0,
        marge_haute.max(24.0) + 18.0,
        largeur,
        58.0,
    )
}

/// Une loupe dessinée (anneau + manche) dans la couleur donnée, 96 × 96 pixels, bords lissés.
pub(crate) fn icone_loupe(couleur: u32) -> Arc<RenderImage> {
    let cote = 96usize;
    let (r, v, b) = ((couleur >> 16) as u8, (couleur >> 8) as u8, couleur as u8);
    let mut pixels = vec![0u8; cote * cote * 4];
    let (cx, cy, rayon, epaisseur) = (40.0f32, 40.0f32, 27.0f32, 8.0f32);
    let (ax, ay, bx, by, manche) = (60.0f32, 60.0f32, 86.0f32, 86.0f32, 10.0f32);
    for y in 0..cote {
        for x in 0..cote {
            let (px_, py_) = (x as f32 + 0.5, y as f32 + 0.5);
            let d_anneau = (((px_ - cx).powi(2) + (py_ - cy).powi(2)).sqrt() - rayon).abs() - epaisseur / 2.0;
            let (dx, dy) = (bx - ax, by - ay);
            let t = (((px_ - ax) * dx + (py_ - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let d_manche = ((px_ - (ax + t * dx)).powi(2) + (py_ - (ay + t * dy)).powi(2)).sqrt() - manche / 2.0;
            let couverture = (0.5 - d_anneau.min(d_manche)).clamp(0.0, 1.0);
            if couverture > 0.0 {
                let i = (y * cote + x) * 4;
                pixels[i..i + 4].copy_from_slice(&[b, v, r, (couverture * 255.0) as u8]);
            }
        }
    }
    en_rendu(ImageBgra {
        largeur: cote as u32,
        hauteur: cote as u32,
        pixels,
    })
    .expect("loupe : tampon valide")
}

fn couleur_moyenne(image: &ImageBgra) -> u32 {
    let (mut r, mut v, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for p in image.pixels.chunks_exact(4) {
        if p[3] > 200 {
            b += u64::from(p[0]);
            v += u64::from(p[1]);
            r += u64::from(p[2]);
            n += 1;
        }
    }
    if n == 0 {
        return 0x8E8E93;
    }
    ((r / n) << 16 | (v / n) << 8 | (b / n)) as u32
}

/// Couleur stable pour une app sans icône.
fn couleur_par_defaut(identifiant: &str) -> u32 {
    const PALETTE: [u32; 6] = [0x2F7BF6, 0x34C759, 0x5B5BD6, 0xF5B83D, 0xFF6B6B, 0x1F2328];
    let somme: usize = identifiant.bytes().map(usize::from).sum();
    PALETTE[somme % PALETTE.len()]
}

const JOURS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
const MOIS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

fn date_lisible(quand_ms: i64) -> String {
    match Local.timestamp_millis_opt(quand_ms).single() {
        Some(d) => format!(
            "{} {} {}",
            JOURS[d.weekday().num_days_from_monday() as usize],
            d.day(),
            MOIS[d.month0() as usize]
        ),
        None => String::new(),
    }
}

fn heure(quand_ms: i64, secondes: bool) -> String {
    match Local.timestamp_millis_opt(quand_ms).single() {
        Some(d) if secondes => format!("{:02}:{:02}:{:02}", d.hour(), d.minute(), d.second()),
        Some(d) => format!("{:02}:{:02}", d.hour(), d.minute()),
        None => String::new(),
    }
}

/// Petite étiquette lisible sur n'importe quel fond (le texte blanc seul disparaîtrait sur du blanc).
/// Remplace les mots secrets lus sur une image par des « • » (dans le mot et dans le texte de la
/// ligne, caractère pour caractère : la sélection reste alignée) et rend leurs cadres (à recouvrir).
fn cacher_secrets(mut lignes: Vec<LigneLue>) -> (Vec<LigneLue>, Vec<[f64; 4]>) {
    let mut cadres = Vec::new();
    for ligne in &mut lignes {
        let mots: Vec<&str> = ligne.mots.iter().map(|m| m.texte.as_str()).collect();
        let secrets = bscreen_core::masquage::mots_secrets(&mots);
        let mut caracteres: Vec<char> = ligne.texte.chars().collect();
        for (mot, secret) in ligne.mots.iter_mut().zip(secrets) {
            if secret {
                cadres.push(mot.cadre);
                mot.texte = "•".repeat(mot.fin - mot.debut);
                for c in caracteres.iter_mut().take(mot.fin).skip(mot.debut) {
                    *c = '•';
                }
            }
        }
        ligne.texte = caracteres.into_iter().collect();
    }
    (lignes, cadres)
}

/// Sépare une recherche en texte et filtres : « @safari @hier les dauphins » → (« les dauphins »,
/// [« safari »], [1 jour en arrière]). Un « @ » seul reste du texte.
fn separer_filtres(recherche: &str) -> (String, Vec<String>, Vec<u32>, bool) {
    let mut texte = Vec::new();
    let mut apps = Vec::new();
    let mut jours = Vec::new();
    let mut voix = false;
    for mot in recherche.split_whitespace() {
        match mot.strip_prefix('@') {
            Some(filtre) if !filtre.is_empty() => {
                let filtre = filtre.to_lowercase();
                if ["dit", "voix", "entendu", "paroles"].contains(&filtre.as_str()) {
                    voix = true;
                    continue;
                }
                match jours_en_arriere(&filtre, Local::now().weekday()) {
                    Some(j) => jours.push(j),
                    None => apps.push(filtre),
                }
            }
            _ => texte.push(mot),
        }
    }
    (texte.join(" "), apps, jours, voix)
}

/// « hier » → 1, « aujourdhui » → 0, « lundi » → le dernier lundi (0 si c'est aujourd'hui).
fn jours_en_arriere(filtre: &str, aujourd_hui: chrono::Weekday) -> Option<u32> {
    const JOURS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
    match filtre {
        "aujourdhui" | "aujourd'hui" | "aujourd’hui" => Some(0),
        "hier" => Some(1),
        "avant-hier" | "avanthier" => Some(2),
        jour => {
            let cible = JOURS.iter().position(|j| *j == jour)? as u32;
            Some((aujourd_hui.num_days_from_monday() + 7 - cible) % 7)
        }
    }
}

/// Couleurs des voix : le micro (toi) et le son du Mac.
const COULEUR_MICRO: u32 = 0x30D158;
const COULEUR_MAC: u32 = 0x64D2FF;

/// Minuscule et sans accent (pour surligner le mot cherché dans une phrase).
fn plier_car(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' | 'í' | 'ì' => 'i',
        'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ÿ' => 'y',
        autre => autre,
    }
}

/// Où les mots cherchés apparaissent dans un texte (en octets, dans l'ordre, sans chevauchement).
fn plages_cherchees(texte: &str, requete: &str) -> Vec<std::ops::Range<usize>> {
    let caracteres: Vec<(usize, char)> = texte.char_indices().map(|(i, c)| (i, plier_car(c))).collect();
    let mut plages: Vec<std::ops::Range<usize>> = Vec::new();
    for mot in requete.split_whitespace() {
        let mot: Vec<char> = mot.chars().map(plier_car).collect();
        if mot.len() < 2 {
            continue;
        }
        let mut i = 0;
        while i + mot.len() <= caracteres.len() {
            if caracteres[i..i + mot.len()].iter().map(|&(_, c)| c).eq(mot.iter().copied()) {
                let fin = caracteres.get(i + mot.len()).map(|&(o, _)| o).unwrap_or(texte.len());
                plages.push(caracteres[i].0..fin);
                i += mot.len();
            } else {
                i += 1;
            }
        }
    }
    plages.sort_by_key(|r| r.start);
    let mut fusion: Vec<std::ops::Range<usize>> = Vec::new();
    for r in plages {
        match fusion.last_mut() {
            Some(d) if r.start <= d.end => d.end = d.end.max(r.end),
            _ => fusion.push(r),
        }
    }
    fusion
}

/// Qui parle (le nom de la voix reconnue, sinon « Toi » / « Mac ») et la couleur de sa pastille.
/// Le nom de la voix et sa couleur (micro vert, son du Mac bleu). Sans voix : « ? » (pas sûr de
/// qui parlait), ou « Vidéo » pour le son du Mac hors appel.
fn qui_parle(p: &Passage) -> (String, u32) {
    let couleur = if p.micro { COULEUR_MICRO } else { COULEUR_MAC };
    let qui = p
        .locuteur
        .clone()
        .unwrap_or_else(|| if p.micro || p.appel { "?" } else { "Vidéo" }.to_string());
    (qui, couleur)
}

fn pastille_voix(qui: &str, couleur: u32) -> gpui::Div {
    div()
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(8.0))
        .bg(rgb(couleur))
        .text_color(rgb(0x1D1D1F))
        .text_size(px(13.0))
        .font_weight(FontWeight::BOLD)
        .child(qui.to_string())
}

/// Le contour blanc de la frise, sur tout le bord de l'écran.
fn contour_frise() -> gpui::Div {
    // 8 px, et les coins du haut arrondis au moins autant que ceux de l'écran du MacBook (sinon le
    // liseré y est rogné). 14 px ne suffisait pas (Julien, 20/11).
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .border(px(8.0))
        .border_color(rgb(0xFFFFFF))
        .rounded_t(px(26.0))
}

/// Une phrase dite : qui parle (pastille), l'heure, puis le texte, le mot cherché surligné.
fn ligne_parole(p: &Passage, requete: &str, en_ecoute: bool, choisie: bool, pastille: gpui::AnyElement) -> gpui::Div {
    let couleur = qui_parle(p).1;
    let comprise = p.texte.as_deref().is_some_and(|t| !t.is_empty());
    let texte: SharedString = match &p.texte {
        Some(t) if !t.is_empty() => format!("« {t} »").into(),
        Some(_) => "(voix pas comprise)".into(),
        None => "(transcription en attente…)".into(),
    };
    let surlignage = HighlightStyle {
        color: Some(rgb(0x1D1D1F).into()),
        background_color: Some(rgb(0xFFD60A).into()),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    let plages: Vec<(std::ops::Range<usize>, HighlightStyle)> = plages_cherchees(&texte, requete)
        .into_iter()
        .map(|r| (r, surlignage))
        .collect();
    div()
        .max_w(px(960.0))
        .px(px(16.0))
        .py(px(8.0))
        .rounded(px(14.0))
        .bg(rgba(0x000000B8))
        .shadow_lg()
        .when(en_ecoute, |d| d.border_2().border_color(rgb(couleur)))
        // Plusieurs passages à ce moment : celui qu'Espace jouera est entouré.
        .when(choisie && !en_ecoute, |d| d.border_2().border_color(rgba(0xFFFFFF8C)))
        .flex()
        .items_start()
        .gap(px(10.0))
        .child(pastille)
        .child(
            div()
                .pt(px(2.0))
                .text_size(px(13.0))
                .text_color(rgb(0xEBEBF5))
                .child(heure(p.debut_ms, true)),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(16.0))
                .text_color(rgb(if comprise { 0xFFFFFF } else { 0xAEAEB2 }))
                .child(StyledText::new(texte).with_highlights(plages)),
        )
}

fn etiquette(texte: String, gras: bool) -> gpui::Div {
    div()
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(9.0))
        .bg(rgba(0x00000059))
        .text_size(px(11.0))
        .text_color(rgb(0xFFFFFF))
        .when(gras, |d| d.font_weight(FontWeight::SEMIBOLD))
        .child(texte)
}

fn badge(icone: Option<(Arc<RenderImage>, u32)>, nom: &str, identifiant: &str, cote: f32) -> gpui::Div {
    let base = div().w(px(cote)).h(px(cote)).rounded(px(cote * 0.24)).shadow_md();
    match icone {
        Some((image, _)) => base.child(img(ImageSource::Render(image)).size_full()),
        None => base
            .bg(rgb(couleur_par_defaut(identifiant)))
            .border_1()
            .border_color(rgb(0xFFFFFF))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(cote * 0.45))
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(0xFFFFFF))
            .child(
                nom.chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default(),
            ),
    }
}

impl Render for Frise {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Images de moments privés reverrouillés : rendues au GPU.
        for image in self.a_liberer.drain(..) {
            let _ = window.drop_image(image);
        }
        if !self.fenetre_preparee {
            self.fenetre_preparee = true;
            if let Some(ouverte) = self.ouverte_a {
                tracing::info!(ms = ouverte.elapsed().as_millis(), "frise : premier affichage");
            }
            if let Ok(poignee) = window.window_handle()
                && let RawWindowHandle::AppKit(appkit) = poignee.as_raw()
            {
                self.source.preparer_fenetre(appkit.ns_view.as_ptr());
            }
        }

        let ecran = window.viewport_size();
        let (largeur_ecran, hauteur_ecran) = (f32::from(ecran.width), f32::from(ecran.height));

        let racine = div()
            .id("frise")
            .key_context("Frise")
            .track_focus(&self.focus)
            .on_action(cx.listener(|frise, _: &Fermer, window, cx| frise.fermer_ou_annuler(window, cx)))
            .on_action(cx.listener(|frise, _: &Supprimer, _, cx| frise.demander_suppression(true, cx)))
            .on_action(cx.listener(|frise, _: &SupprimerSelection, _, cx| frise.demander_suppression(false, cx)))
            .on_action(cx.listener(|frise, _: &Copier, _, cx| frise.copier(cx)))
            .on_action(cx.listener(|frise, _: &Ecouter, _, cx| frise.ecouter(cx)))
            .on_action(cx.listener(|frise, _: &PassagePrecedent, _, cx| frise.choisir_passage(-1, cx)))
            .on_action(cx.listener(|frise, _: &PassageSuivant, _, cx| frise.choisir_passage(1, cx)))
            .on_action(cx.listener(|frise, _: &VoixPrecedente, _, cx| frise.aller_a_voix(-1, cx)))
            .on_action(cx.listener(|frise, _: &VoixSuivante, _, cx| frise.aller_a_voix(1, cx)))
            .on_action(cx.listener(|frise, _: &OuvrirPage, _, cx| frise.ouvrir_page(cx)))
            .on_action(cx.listener(|frise, _: &JourPrecedent, _, cx| frise.changer_de_jour(-1, cx)))
            .on_action(cx.listener(|frise, _: &JourSuivant, _, cx| frise.changer_de_jour(1, cx)))
            .on_action(cx.listener(|frise, _: &Confirmer, window, cx| frise.confirmer(window, cx)))
            .on_action(cx.listener(|frise, _: &EtendreGauche, _, cx| frise.etendre(-1, cx)))
            .on_action(cx.listener(|frise, _: &EtendreDroite, _, cx| frise.etendre(1, cx)))
            .on_action(cx.listener(|frise, _: &Precedent, _, cx| frise.sauter(-1.0, cx)))
            .on_action(cx.listener(|frise, _: &Suivant, _, cx| frise.sauter(1.0, cx)))
            .on_scroll_wheel(cx.listener(|frise, e: &ScrollWheelEvent, _, cx| frise.defiler(e, cx)))
            // Taper une lettre ouvre la recherche.
            .on_key_down(cx.listener(|frise, e: &KeyDownEvent, window, cx| frise.touche_frise(e, window, cx)))
            .size_full()
            .relative();

        if self.moments.is_empty() {
            return racine.flex().items_center().justify_center().child(
                div()
                    .px(px(18.0))
                    .py(px(10.0))
                    .rounded(px(14.0))
                    .bg(rgba(0x00000080))
                    .text_color(rgb(0xFFFFFF))
                    .child("Rien d'enregistré pour l'instant · Échap pour fermer"),
            );
        }

        let actuel = self.actuel();
        // La bande de vignettes occupe toute la largeur de l'écran.
        let largeur_bande = largeur_ecran;
        let largeur_activite = largeur_ecran - 2.0 * MARGE_TEXTE;
        let centre = largeur_bande / 2.0;
        let largeur_vignette = LARGEUR_BASE * self.zoom.max(ZOOM_TAILLE_MIN);
        let hauteur_vignette = largeur_vignette * 0.647;
        let pas = largeur_vignette + ECART;
        let densite = self.densite();
        let demi = ((centre / pas).ceil() as isize + 2) * densite as isize;
        let premier = (self.position.floor() as isize - demi).max(0) as usize;
        let dernier = ((self.position.ceil() as isize + demi).max(0) as usize).min(self.moments.len() - 1);
        // Réduite : une vignette tous les `densite` moments, plus le moment courant (sans en
        // chevaucher une autre).
        let visibles: Vec<usize> = (premier..=dernier)
            .filter(|&i| {
                i == actuel
                    || (i % densite == 0 && (densite == 1 || (i as isize - actuel as isize).unsigned_abs() * 2 >= densite))
            })
            .collect();
        self.demander_images(&visibles, (largeur_ecran, hauteur_ecran), window);

        // Vignettes visibles.
        let mut vignettes = Vec::new();
        for i in visibles {
            let moment = self.moments[i].clone();
            let est_actuel = i == actuel;
            let est_survolee = self.survolee == Some(i);
            let est_selectionnee = self.selection.contains(&moment.id);
            let est_resultat = self.resultats_ids.contains(&moment.id);
            let resultat_visible =
                self.resultats_vus.contains(&moment.id) || self.resultats_sens.contains_key(&moment.id);
            let resultat_entendu = self.resultats_entendus.contains(&moment.id);
            let agrandi = if est_actuel {
                1.22
            } else if est_survolee {
                1.08
            } else {
                1.0
            };
            let (l, h) = (largeur_vignette * agrandi, hauteur_vignette * agrandi);
            let x = centre + (i as f32 - self.position) / densite as f32 * pas - l / 2.0;
            let y = HAUTEUR_BANDE - h - 26.0;
            let identifiant = moment.app_bundle.clone().unwrap_or_default();
            let nom = moment.app_nom.clone().unwrap_or_default();
            let icone = if identifiant.is_empty() {
                None
            } else {
                self.icone(&identifiant)
            };
            let cote = if est_actuel { 30.0 } else { 24.0 };
            let image = self.vignettes.get(&moment.id).cloned();
            let verrouillee = self.verrouille(&moment);
            let texte = if est_survolee && !nom.is_empty() {
                format!("{} · {}", heure(moment.quand_ms, true), nom)
            } else if est_survolee || (est_actuel && nom.is_empty()) {
                heure(moment.quand_ms, true)
            } else if est_actuel {
                nom.clone()
            } else {
                heure(moment.quand_ms, false)
            };
            vignettes.push(
                div()
                    .id(("vignette", i))
                    .cursor_pointer()
                    // Survol : la vignette s'éclaire et montre son heure et son app.
                    .on_hover(cx.listener(move |frise, survol: &bool, _, cx| {
                        if *survol {
                            frise.survolee = Some(i);
                        } else if frise.survolee == Some(i) {
                            frise.survolee = None;
                        }
                        cx.notify();
                    }))
                    // Clic : on va à ce moment. ⌥-clic : sélection.
                    .on_click(cx.listener(move |frise, evenement: &ClickEvent, _, cx| {
                        if evenement.modifiers().alt {
                            frise.basculer_selection(i, cx);
                        } else {
                            frise.position = i as f32;
                            frise.selection.clear();
                            frise.ancre = None;
                            cx.notify();
                        }
                    }))
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(l))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .relative()
                            .w(px(l))
                            .h(px(h))
                            .child(
                                div()
                                    .size_full()
                                    .rounded(px(8.0))
                                    .overflow_hidden()
                                    .bg(rgba(0xFFFFFF26))
                                    .shadow_md()
                                    .when(est_actuel, |d| d.border_3().border_color(rgb(0x0A84FF)))
                                    .when(est_survolee && !est_actuel, |d| {
                                        d.border_2().border_color(rgb(0xFFFFFF))
                                    })
                                    .when(est_selectionnee, |d| d.border_3().border_color(rgb(0xFF453A)))
                                    .children(
                                        image.map(|image| img(ImageSource::Render(image)).size_full().rounded(px(8.0))),
                                    )
                                    // Navigation privée : hachures et cadenas, jamais l'image.
                                    .when(verrouillee, |d| {
                                        d.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .left_0()
                                                .size_full()
                                                .rounded(px(8.0))
                                                .bg(rgb(0x2C2C2E))
                                                .child(
                                                    div()
                                                        .size_full()
                                                        .rounded(px(8.0))
                                                        .bg(gpui::pattern_slash(rgba(0xFFFFFF1F).into(), 2.0, 10.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .text_size(px(22.0))
                                                        .child("🔒"),
                                                ),
                                        )
                                    })
                                    .when(est_selectionnee, |d| {
                                        d.child(div().absolute().top_0().left_0().size_full().bg(rgba(0xFF453A3D)))
                                    })
                                    // Résultat de recherche : une pastille jaune.
                                    .when(est_resultat, |d| {
                                        d.child(
                                            div()
                                                .absolute()
                                                .top(px(6.0))
                                                .right(px(6.0))
                                                .w(px(10.0))
                                                .h(px(10.0))
                                                .rounded(px(5.0))
                                                .bg(rgb(if resultat_visible {
                                                    0xFFD60A
                                                } else if resultat_entendu {
                                                    0x30D158
                                                } else {
                                                    0x8E8E93
                                                }))
                                                .border_1()
                                                .border_color(rgb(0xFFFFFF)),
                                        )
                                    }),
                            )
                            // Le logo de l'app déborde du coin haut gauche.
                            .child(
                                badge(icone, &nom, &identifiant, cote)
                                    .absolute()
                                    .left(px(-cote * 0.35))
                                    .top(px(-cote * 0.35)),
                            ),
                    )
                    .child(etiquette(texte, est_actuel || est_survolee)),
            );
        }

        // Barre d'activité : des tranches par app, avec la zone visible et la tête de lecture.
        let total = self.moments.len() as f32;
        self.calculer_activite(largeur_activite);
        let activite: Vec<gpui::Div> = self
            .activite
            .iter()
            .map(|&(x, l, couleur)| {
                div()
                    .absolute()
                    .left(px(x))
                    .top_0()
                    .h_full()
                    .w(px(l + 0.5))
                    .bg(rgb(couleur))
                    .opacity(0.85)
            })
            .collect();
        let marques_voix: Vec<gpui::Div> = self
            .activite_voix
            .iter()
            .map(|&(x, l, micro)| {
                div()
                    .absolute()
                    .left(px(x))
                    .bottom_0()
                    .w(px(l))
                    .h(px(3.0))
                    .bg(rgb(if micro { COULEUR_MICRO } else { COULEUR_MAC }))
            })
            .collect();
        let visible_x = premier as f32 / total * largeur_activite;
        let visible_l = ((dernier - premier + 1) as f32 / total * largeur_activite).max(6.0);
        let tete_x = (self.position / total * largeur_activite).min(largeur_activite - 2.0);

        let moment = self.moments[actuel].clone();
        let actuel_verrouille = self.verrouille(&moment);
        // Grande image : la pleine définition si elle est prête, sinon la vignette agrandie.
        let grande = match &self.grande {
            _ if actuel_verrouille => None,
            Some((id, image)) if *id == moment.id => Some(Arc::clone(image)),
            _ => self.vignettes.get(&moment.id).cloned(),
        };
        // Moment privé verrouillé : un grand panneau hachuré à la place de l'image.
        let panneau_prive = actuel_verrouille.then(|| {
            let en_cours = self.deverrouillage_en_cours;
            div()
                .id("moment-prive")
                .absolute()
                .left_0()
                .top_0()
                .size_full()
                .bg(rgb(0x1C1C1E))
                .child(
                    div()
                        .size_full()
                        .bg(gpui::pattern_slash(rgba(0xFFFFFF14).into(), 3.0, 18.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(14.0))
                        .child(div().text_size(px(64.0)).child("🔒"))
                        .child(
                            div()
                                .text_size(px(22.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xFFFFFF))
                                .child("Navigation privée"),
                        )
                        .child(
                            div()
                                .px(px(16.0))
                                .py(px(8.0))
                                .rounded(px(14.0))
                                .bg(rgba(0xFFFFFF26))
                                .text_size(px(15.0))
                                .text_color(rgb(0xFFFFFF))
                                .child(if en_cours {
                                    "Touch ID…"
                                } else {
                                    "Clique ou appuie sur Entrée pour voir avec Touch ID"
                                }),
                        ),
                )
                .cursor_pointer()
                .on_click(cx.listener(|frise, _: &ClickEvent, window, cx| frise.deverrouiller(window, cx)))
        });
        let carte_y = hauteur_ecran - HAUTEUR_CARTE;
        // Sélection de texte sur l'image : des rectangles bleus, comme dans Aperçu.
        self.taille_ecran = (largeur_ecran, hauteur_ecran);
        // Les secrets lus sur l'image : recouverts d'un pavé (seulement sur l'image nette).
        let mut caches: Vec<gpui::Div> = Vec::new();
        if let Some((id, cadres)) = &self.caviardage
            && *id == moment.id
            && self.grande.as_ref().is_some_and(|(g, _)| *g == moment.id)
        {
            for c in cadres {
                let (x, y) = (c[0] as f32 * largeur_ecran, c[1] as f32 * hauteur_ecran);
                let (l, h) = (c[2] as f32 * largeur_ecran, c[3] as f32 * hauteur_ecran);
                caches.push(
                    div()
                        .absolute()
                        .left(px(x - 2.0))
                        .top(px(y - 2.0))
                        .w(px(l + 4.0))
                        .h(px(h + 4.0))
                        .rounded(px(3.0))
                        .bg(rgb(0x2C2C2E)),
                );
            }
        }
        let mut surlignages_selection: Vec<gpui::Div> = Vec::new();
        // Une bande par ligne, à la hauteur de la ligne, du premier au dernier caractère choisi.
        if let Some((a, b, id)) = self.selection_texte
            && id == moment.id
            && a != b
            && let Some(lignes) = self.lignes_courantes()
        {
            for r in bandes(lignes, a, b) {
                let (x, y) = (r[0] as f32 * largeur_ecran, r[1] as f32 * hauteur_ecran);
                let (l, h) = (r[2] as f32 * largeur_ecran, r[3] as f32 * hauteur_ecran);
                surlignages_selection.push(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y - 1.0))
                        .w(px(l.max(1.0)))
                        .h(px(h + 2.0))
                        .rounded(px(2.0))
                        .bg(rgba(0x0A84FF59)),
                );
            }
        }
        // Message bref (« Texte copié »), au-dessus de la barre.
        let annonce = self.message.as_ref().map(|(texte, _)| {
            div()
                .absolute()
                .left_0()
                .top(px(carte_y - 64.0))
                .w(px(largeur_ecran))
                .flex()
                .justify_center()
                .child(
                    div()
                        .px(px(16.0))
                        .py(px(8.0))
                        .rounded(px(14.0))
                        .bg(rgba(0x000000A6))
                        .shadow_lg()
                        .text_color(rgb(0xFFFFFF))
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(texte.clone()),
                )
        });
        // Bandeau de confirmation, juste au-dessus de la barre.
        let banniere =
            self.confirmation.as_ref().map(|ids| {
                let n = ids.len();
                let pluriel = if n > 1 { "s" } else { "" };
                div().absolute().left_0().top(px(carte_y - 64.0)).w(px(largeur_ecran)).flex().justify_center().child(
                div()
                    .px(px(18.0))
                    .py(px(10.0))
                    .rounded(px(16.0))
                    .bg(rgb(0xFF453A))
                    .shadow_lg()
                    .text_color(rgb(0xFFFFFF))
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "Supprimer {n} image{pluriel} définitivement ? Entrée pour confirmer · Échap pour annuler"
                    )),
            )
            });

        // Le mot cherché, encadré là où l'OCR le voit sur l'image. S'il est caché sous la barre du bas,
        // la barre se range d'elle-même (une poignée « ▲ » la remet) ; sinon un repère « ▼ plus bas ».
        let mut cadres_ecran: Vec<(f32, f32, f32, f32)> = Vec::new();
        let mut note_recherche: Option<String> = None;
        if self.recherche_ouverte
            && self.seulement_par_le_sens(moment.id)
            && let Some(textes) = self.resultats_sens.get(&moment.id)
        {
            let courts: Vec<String> = textes
                .iter()
                .take(3)
                .map(|t| {
                    let t = t.replace('\n', " ");
                    let court: String = t.chars().take(60).collect();
                    format!("« {court}{} »", if t.chars().count() > 60 { "…" } else { "" })
                })
                .collect();
            note_recherche = Some(match textes.first().and_then(|t| t.strip_prefix(PAR_L_IMAGE)) {
                Some(ressemble) => format!("Trouvé par l'image : elle ressemble à {ressemble}"),
                None => format!("Trouvé par le sens : {}", courts.join(", ")),
            });
        } else if self.recherche_ouverte
            && self.resultats_ids.contains(&moment.id)
            && let Some((id, requete, cadres)) = &self.reperes
            && *id == moment.id
            && *requete == self.requete_texte()
        {
            for c in cadres {
                let (x, y) = (c[0] as f32 * largeur_ecran, c[1] as f32 * hauteur_ecran);
                let (l, h) = (c[2] as f32 * largeur_ecran, c[3] as f32 * hauteur_ecran);
                if l <= largeur_ecran * 0.9 && h <= hauteur_ecran * 0.5 {
                    cadres_ecran.push((x, y, l, h));
                }
            }
            if cadres.is_empty() {
                note_recherche = Some(if self.resultats_vus.contains(&moment.id) {
                    "Trouvé à ce moment, mais pas lisible sur l'image"
                } else if self.resultats_entendus.contains(&moment.id) {
                    "Entendu à ce moment : la phrase est en bas"
                } else {
                    "Trouvé dans du texte caché ou hors de l'écran"
                }
                .to_string());
            }
        }
        let sous_la_barre = |&(_, y, _, h): &(f32, f32, f32, f32)| y + h > carte_y - 6.0;
        let mot_sous_la_barre = cadres_ecran.iter().any(sous_la_barre);
        let barre_visible = !mot_sous_la_barre || self.barre_forcee_pour == Some(moment.id);
        let mut surlignages: Vec<gpui::Div> = Vec::new();
        let mut caches_par_la_barre: Vec<f32> = Vec::new();
        for &(x, y, l, h) in &cadres_ecran {
            let h = if barre_visible {
                if y >= carte_y - 6.0 {
                    caches_par_la_barre.push(x + l / 2.0);
                    continue;
                }
                h.min(carte_y - 6.0 - y)
            } else {
                h
            };
            surlignages.push(
                div()
                    .absolute()
                    .left(px(x - 4.0))
                    .top(px(y - 3.0))
                    .w(px(l + 8.0))
                    .h(px(h + 6.0))
                    .rounded(px(6.0))
                    .border_2()
                    .border_color(rgb(0xFFD60A))
                    .bg(rgba(0xFFD60A1F)),
            );
        }
        let reperes_bas: Vec<gpui::Stateful<gpui::Div>> = caches_par_la_barre
            .into_iter()
            .enumerate()
            .map(|(i, x)| {
                div()
                    .id(("plus-bas", i))
                    .cursor_pointer()
                    .on_click(cx.listener(|frise, _: &ClickEvent, _, cx| {
                        frise.barre_forcee_pour = None;
                        cx.notify();
                    }))
                    .absolute()
                    .left(px(x - 48.0))
                    .top(px(carte_y - 32.0))
                    .w(px(96.0))
                    .h(px(24.0))
                    .rounded(px(12.0))
                    .bg(rgb(0xFFD60A))
                    .shadow_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(0x1D1D1F))
                    .child("▼ plus bas")
            })
            .collect();
        // Barre rangée : une poignée en bas pour la faire revenir.
        let id_moment = moment.id;
        let poignee = (!barre_visible).then(|| {
            div()
                .id("poignee")
                .cursor_pointer()
                .on_click(cx.listener(move |frise, _: &ClickEvent, _, cx| {
                    frise.barre_forcee_pour = Some(id_moment);
                    cx.notify();
                }))
                .absolute()
                .left(px(largeur_ecran / 2.0 - 90.0))
                .top(px(hauteur_ecran - 44.0))
                .w(px(180.0))
                .h(px(30.0))
                .rounded(px(15.0))
                .bg(rgba(0x000000A6))
                .shadow_lg()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(0xFFFFFF))
                .child("▲ Afficher la frise")
        });

        // Champ de recherche, en haut au centre : une pilule transparente, seulement floue,
        // avec un texte clair ou foncé selon ce qu'il y a derrière.
        let (champ_x, champ_y, champ_l, champ_h) = rect_champ(largeur_ecran, self.marge_haute);
        let (fond_champ, clair) = match &self.flou_champ {
            Some((id, image, clair)) if *id == moment.id => (Some(Arc::clone(image)), *clair),
            _ => (None, false),
        };
        let (couleur_texte, couleur_discrete) = if clair {
            (0x1D1D1F, 0x3C3C43)
        } else {
            (0xFFFFFF, 0xEBEBF5)
        };
        let loupe = Arc::clone(&self.loupes[usize::from(clair)]);
        let compteur = if self.resultats.is_empty() {
            if self.recherche_en_cours {
                "…".to_string()
            } else if self.recherche.trim().chars().count() >= 2 {
                "aucun résultat".to_string()
            } else {
                String::new()
            }
        } else {
            let n = self.groupes.len();
            format!("{} / {n} endroit{}", self.groupe_courant + 1, if n > 1 { "s" } else { "" })
        };
        let (texte_champ, discret) = if !self.recherche.is_empty() {
            (format!("{}▏", self.recherche), false)
        } else if self.recherche_ouverte {
            ("▏Tape ce que tu cherches…".to_string(), true)
        } else {
            ("Rechercher dans ton passé  ·  ? pour l'aide".to_string(), true)
        };
        let champ = div()
            .id("recherche")
            .key_context("Recherche")
            .track_focus(&self.focus_recherche)
            .on_key_down(cx.listener(|frise, e: &KeyDownEvent, window, cx| frise.touche_recherche(e, window, cx)))
            .on_click(cx.listener(|frise, _: &ClickEvent, window, cx| frise.ouvrir_recherche(window, cx)))
            .cursor_text()
            .absolute()
            .left(px(champ_x))
            .top(px(champ_y))
            .w(px(champ_l))
            .h(px(champ_h))
            .rounded(px(champ_h / 2.0))
            .border_1()
            .border_color(rgba(if clair { 0x00000030 } else { 0xFFFFFF55 }))
            .shadow_lg()
            .children(fond_champ.map(|image| {
                img(ImageSource::Render(image))
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    .rounded(px(champ_h / 2.0))
            }))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    .px(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(img(ImageSource::Render(loupe)).w(px(22.0)).h(px(22.0)))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(20.0))
                            .text_color(rgb(if discret { couleur_discrete } else { couleur_texte }))
                            .child(texte_champ),
                    )
                    .children((!compteur.is_empty()).then(|| {
                        div()
                            .px(px(12.0))
                            .py(px(4.0))
                            .rounded(px(12.0))
                            .bg(rgb(0xFFD60A))
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x1D1D1F))
                            .child(compteur)
                    }))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(rgb(couleur_discrete))
                            .child("Échap"),
                    ),
            );
        // Sous le champ : comment naviguer, et pourquoi rien n'est encadré quand c'est le cas.
        let aide = (self.recherche_ouverte && !self.resultats.is_empty()).then(|| {
            div()
                .absolute()
                .left(px(champ_x))
                .top(px(champ_y + champ_h + 10.0))
                .w(px(champ_l))
                .flex()
                .justify_center()
                .gap(px(8.0))
                .child(etiquette(
                    "↩ endroit suivant · ⇧↩ précédent · Tab : tous les endroits · @app @hier @dit pour filtrer · Échap".to_string(),
                    false,
                ))
                .children(note_recherche.map(|note| etiquette(note, true)))
        });

        // Ce qui a été dit pendant que ce moment était à l'écran : deux phrases au plus, au-dessus de la barre.
        // (Rien n'est montré d'un moment privé verrouillé, pas même ce qui y a été dit.)
        let dits: Vec<Passage> = if actuel_verrouille {
            Vec::new()
        } else {
            self.passages_du_moment(actuel).into_iter().cloned().collect()
        };
        let requete = if self.recherche_ouverte {
            self.requete_texte()
        } else {
            String::new()
        };
        // Trois phrases au plus, autour du passage choisi (↑ / ↓ ou clic pour en changer).
        let choisi = self.rang_choisi(actuel, &dits);
        let premier = choisi.saturating_sub(1).min(dits.len().saturating_sub(3));
        let plusieurs = dits.len() > 1;
        let paroles = (!dits.is_empty()).then(|| {
            div()
                .absolute()
                .left_0()
                .bottom(px(if barre_visible { HAUTEUR_CARTE + 76.0 } else { 76.0 }))
                .w(px(largeur_ecran))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.0))
                .children(self.choix_voix.as_ref().filter(|c| dits.iter().any(|p| p.id == c.passage)).map(|c| self.rangee_choix_voix(c, cx)))
                .children(dits.iter().enumerate().skip(premier).take(3).map(|(rang, p)| {
                    let (qui, couleur) = qui_parle(p);
                    let en_saisie = self.nommage.as_ref().filter(|(cible, _)| match cible {
                        CibleNom::Voix(id) => Some(*id) == p.locuteur_id,
                        CibleNom::Passage(id) => *id == p.id,
                    });
                    let pastille = match en_saisie {
                        Some((_, saisie)) => div()
                            .id(("nom-voix", rang))
                            .key_context("Nommage")
                            .track_focus(&self.focus_nommage)
                            .on_key_down(cx.listener(|frise, e: &KeyDownEvent, window, cx| {
                                frise.touche_nommage(e, window, cx)
                            }))
                            // Un clic dans le champ ne lance pas l'écoute de la phrase.
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                            .px(px(8.0))
                            .py(px(2.0))
                            .rounded(px(8.0))
                            .bg(rgb(0xFFFFFF))
                            .text_color(rgb(0x1D1D1F))
                            .text_size(px(13.0))
                            .font_weight(FontWeight::BOLD)
                            .child(if saisie.is_empty() {
                                "Qui parle ?▏".to_string()
                            } else {
                                format!("{saisie}▏")
                            })
                            .into_any_element(),
                        None => {
                            let passage = p.clone();
                            pastille_voix(&qui, couleur)
                                .id(("nom-voix", rang))
                                .cursor_pointer()
                                .on_click(cx.listener(move |frise, _: &ClickEvent, _, cx| {
                                    // Dire qui parle, pas écouter la phrase.
                                    cx.stop_propagation();
                                    frise.ouvrir_choix_voix(&passage, cx)
                                }))
                                .into_any_element()
                        }
                    };
                    let en_ecoute = self.ecoute.is_some_and(|(d, _)| d == p.debut_ms);
                    ligne_parole(p, &requete, en_ecoute, plusieurs && rang == choisi, pastille)
                        .id(("parole", rang))
                        .cursor_pointer()
                        .on_click(cx.listener(move |frise, _: &ClickEvent, _, cx| frise.cliquer_passage(rang, cx)))
                }))
                .child(etiquette(
                    if self.nommage.is_some() {
                        "Tape le nom · ↩ pour valider · Échap pour annuler".to_string()
                    } else if self.choix_voix.is_some() {
                        "Seul ce passage change, et la voix choisie apprend de lui · Échap pour annuler".to_string()
                    } else {
                        let action = if self.ecoute.is_some() { "■ Espace pour arrêter" } else { "▶ Espace pour écouter" };
                        if plusieurs {
                            format!(
                                "{action} · passage {} sur {} : ↑↓ ou clic pour choisir · clic sur le nom pour dire qui parle",
                                choisi + 1,
                                dits.len()
                            )
                        } else {
                            format!("{action} · clic sur le nom pour dire qui parle")
                        }
                    },
                    false,
                ))
        });

        // Grille des résultats (Tab) : un endroit par carte (mots entiers d'abord, les plus récents
        // d'abord), avec ce qui a été lu ou dit autour du mot. Elle défile.
        let grille = (self.grille_ouverte && self.recherche_ouverte && !self.groupes.is_empty()).then(|| {
            let (marge, carte_l, ecart) = (48.0f32, 300.0f32, 22.0f32);
            let haut = champ_y + champ_h + 64.0;
            let carte_h = carte_l * 0.647;
            let courant = self.groupes.get(self.groupe_courant).map(|g| g.vedette);
            let groupes: Vec<Groupe> = self.groupes.iter().take(GROUPES_GRILLE).cloned().collect();
            let mut cartes: Vec<gpui::AnyElement> = Vec::new();
            for groupe in groupes {
                let i = groupe.vedette;
                let moment = self.moments[i].clone();
                let identifiant = moment.app_bundle.clone().unwrap_or_default();
                let nom = moment.app_nom.clone().unwrap_or_default();
                let icone = if identifiant.is_empty() { None } else { self.icone(&identifiant) };
                let verrouillee = self.verrouille(&moment);
                let image = if verrouillee { None } else { self.vignettes.get(&moment.id).cloned() };
                let est_courant = courant == Some(i);
                let premier = self.moments[groupe.indices[0]].quand_ms;
                let dernier = groupe.indices.last().map_or(premier, |&d| self.moments[d].quand_ms);
                let (h1, h2) = (heure(premier, false), heure(dernier, false));
                let plage = if h1 == h2 { h1 } else { format!("{h1} → {h2}") };
                let du_mac = groupe.entendu && groupe.micro == Some(false);
                let titre = format!(
                    "{} {plage}{}{}",
                    date_lisible(premier),
                    if nom.is_empty() { String::new() } else { format!(" · {nom}") },
                    match (groupe.entendu, du_mac) {
                        (true, true) => " · son du Mac",
                        (true, false) => " · dit",
                        _ => "",
                    }
                );
                // Où : le titre de la fenêtre (ou la page) ; pour une voix, ce qui a été dit suffit.
                let lieu = (!groupe.entendu)
                    .then(|| moment.fenetre.clone().filter(|f| !f.trim().is_empty()).or_else(|| moment.url.clone()))
                    .flatten();
                let nombre = groupe.indices.len();
                let couleur_voix = if du_mac { COULEUR_MAC } else { COULEUR_MICRO };
                let couleur_extrait = if groupe.entendu { couleur_voix } else { 0xEBEBF5 };
                cartes.push(
                    div()
                        .id(("grille", i))
                        .cursor_pointer()
                        .on_click(cx.listener(move |frise, _: &ClickEvent, _, cx| frise.choisir_dans_la_grille(i, cx)))
                        .w(px(carte_l))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            div()
                                .relative()
                                .w(px(carte_l))
                                .h(px(carte_h))
                                .child(
                                    div()
                                        .size_full()
                                        .rounded(px(10.0))
                                        .bg(rgb(0x2C2C2E))
                                        .shadow_lg()
                                        .when(est_courant, |d| d.border_3().border_color(rgb(0x0A84FF)))
                                        .when(groupe.entendu && !est_courant, |d| d.border_2().border_color(rgb(couleur_voix)))
                                        .children(image.map(|im| img(ImageSource::Render(im)).size_full().rounded(px(10.0))))
                                        .when(verrouillee, |d| {
                                            d.child(
                                                div()
                                                    .size_full()
                                                    .rounded(px(10.0))
                                                    .bg(gpui::pattern_slash(rgba(0xFFFFFF1F).into(), 2.0, 10.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_size(px(26.0))
                                                    .child("🔒"),
                                            )
                                        }),
                                )
                                .child(badge(icone, &nom, &identifiant, 28.0).absolute().left(px(-10.0)).top(px(-10.0)))
                                .children((nombre > 1).then(|| {
                                    etiquette(format!("{nombre} moments"), true).absolute().right(px(8.0)).top(px(8.0))
                                })),
                        )
                        .child(etiquette(titre, est_courant))
                        .children(lieu.map(|l| {
                            div().px(px(8.0)).text_size(px(11.0)).text_color(rgb(0xEBEBF5)).truncate().child(l)
                        }))
                        .children(groupe.extrait.clone().map(|e| {
                            div()
                                .px(px(8.0))
                                .text_size(px(12.0))
                                .text_color(rgb(couleur_extrait))
                                .line_clamp(2)
                                .child(format!("« {e} »"))
                        }))
                        .into_any_element(),
                );
            }
            let n = self.groupes.len();
            let moments_trouves = self.resultats.len();
            let pluriel = if n > 1 { "s" } else { "" };
            div()
                .absolute()
                .left_0()
                .top_0()
                .size_full()
                // Les clics à côté des vignettes ne traversent pas jusqu'à l'image du dessous.
                .occlude()
                .bg(rgba(0x000000C7))
                .child(
                    div()
                        .absolute()
                        .left(px(marge))
                        .top(px(haut - 34.0))
                        .w(px(largeur_ecran - 2.0 * marge))
                        .flex()
                        .justify_center()
                        .child(etiquette(
                            format!(
                                "{n} endroit{pluriel} ({moments_trouves} moment{}){} · ↓ ou Entrée : le suivant · clic pour y aller · Tab ou Échap : la frise",
                                if moments_trouves > 1 { "s" } else { "" },
                                if n > GROUPES_GRILLE { format!(", les {GROUPES_GRILLE} premiers") } else { String::new() }
                            ),
                            true,
                        )),
                )
                .child(
                    div()
                        .id("grille-cartes")
                        .absolute()
                        .left(px(marge))
                        .top(px(haut))
                        .w(px(largeur_ecran - 2.0 * marge))
                        .h(px((hauteur_ecran - haut - 30.0).max(carte_h)))
                        .overflow_y_scroll()
                        .child(div().w_full().flex().flex_wrap().gap(px(ecart)).pb(px(40.0)).children(cartes)),
                )
        });

        racine
            .children(grille.map(|g| deferred(g).with_priority(1)))
            .children(paroles.filter(|_| !self.grille_ouverte).map(|p| deferred(p).with_priority(1)))
            .when(self.recherche_ouverte, |vue| {
                vue.child(deferred(champ).with_priority(2))
            })
            .children(aide.map(|a| deferred(a).with_priority(2)))
            .children(reperes_bas.into_iter().map(|r| deferred(r).with_priority(2)))
            .children(surlignages.into_iter().map(|s| deferred(s).with_priority(1)))
            .children(banniere.map(|b| deferred(b).with_priority(3)))
            .children(self.aide_ouverte.then(|| deferred(panneau_aide(cx)).with_priority(5)))
            .children(annonce.map(|a| deferred(a).with_priority(3)))
            // Contour blanc tout autour de l'écran : on voit d'un coup d'œil qu'on est dans la frise
            // (demande de Julien, 20/11). Purement visuel : il ne capte ni clic ni défilement.
            .child(deferred(contour_frise()).with_priority(4))
            .children(panneau_prive)
            .children(grande.map(|image| {
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    // Sélection du texte de l'image à la souris (glisser, double-clic, triple-clic).
                    .when(self.curseur_texte, |d| d.cursor_text())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|frise, e: &MouseDownEvent, _, cx| frise.souris_appuyee(e, cx)),
                    )
                    .on_mouse_move(cx.listener(|frise, e: &MouseMoveEvent, _, cx| frise.souris_deplacee(e, cx)))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|frise, _: &MouseUpEvent, _, _| frise.glisse = false),
                    )
                    .child(img(ImageSource::Render(image)).size_full())
            }))
            .children(caches.into_iter().map(|c| deferred(c).with_priority(1)))
            .children(surlignages_selection.into_iter().map(|s| deferred(s).with_priority(1)))
            // Barre du bas : transparente, seulement floue (le fond flou est l'image elle-même).
            .when(barre_visible, |vue| {
                vue.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(carte_y))
                        .w(px(largeur_ecran))
                        .h(px(HAUTEUR_CARTE))
                        .rounded_t(px(26.0))
                        // Fond : l'image elle-même, floutée et découpée (coins du haut arrondis), sans voile.
                        .children(
                            self.flou_carte
                                .as_ref()
                                .filter(|(id, _)| *id == moment.id)
                                .map(|(_, image)| {
                                    img(ImageSource::Render(Arc::clone(image)))
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .size_full()
                                        .rounded_t(px(26.0))
                                }),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .size_full()
                                .py(px(14.0))
                                .flex()
                                .flex_col()
                                .gap(px(10.0))
                                .child(
                                    div()
                                        .px(px(MARGE_TEXTE))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(etiquette(date_lisible(moment.quand_ms), true))
                                        .child(
                                            div()
                                                .px(px(14.0))
                                                .py(px(5.0))
                                                .rounded(px(14.0))
                                                .bg(rgb(0x0A84FF))
                                                .text_color(rgb(0xFFFFFF))
                                                .text_size(px(15.0))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(heure(moment.quand_ms, true)),
                                        )
                                        .child(etiquette(
                                            if self.selection.is_empty() {
                                                format!(
                                                    "{} moments · tape pour chercher · ⌥←→ voix · ⌘C copier · Échap",
                                                    self.moments.len()
                                                )
                                            } else {
                                                format!(
                                                    "{} sélectionnée(s) · ⌫ pour supprimer · Échap pour annuler",
                                                    self.selection.len()
                                                )
                                            },
                                            false,
                                        )),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .w(px(largeur_bande))
                                        .h(px(HAUTEUR_BANDE))
                                        .children(vignettes),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .ml(px(MARGE_TEXTE))
                                        .w(px(largeur_activite))
                                        .h(px(10.0))
                                        .rounded(px(5.0))
                                        .overflow_hidden()
                                        .flex()
                                        .children(activite)
                                        .children(marques_voix)
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px(visible_x))
                                                .top_0()
                                                .w(px(visible_l))
                                                .h_full()
                                                .rounded(px(4.0))
                                                .border_2()
                                                .border_color(rgb(0xFFFFFF)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px(tete_x))
                                                .top_0()
                                                .w(px(2.0))
                                                .h_full()
                                                .bg(rgb(0x0A84FF)),
                                        ),
                                ),
                        ),
                )
            })
            .children(poignee.map(|p| deferred(p).with_priority(3)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On tape « b », « ba », « bar »… pendant une recherche : seule la dernière lettre compte.
    #[test]
    fn fil_ne_fait_que_la_demande_la_plus_recente() {
        let (commence, a_commence) = mpsc::channel::<()>();
        let (libere, liberation) = mpsc::channel::<()>();
        let (envoi, mut reponses) = fil_du_plus_recent("essai", move |n: u32| {
            if n == 1 {
                // La première demande travaille jusqu'à ce que les suivantes soient toutes arrivées.
                commence.send(()).unwrap();
                liberation.recv().unwrap();
            }
            n * 10
        });
        envoi.send(1).unwrap();
        a_commence.recv().unwrap();
        for n in 2..=5 {
            envoi.send(n).unwrap();
        }
        libere.send(()).unwrap();
        let premiere = futures::executor::block_on(reponses.next());
        let suivante = futures::executor::block_on(reponses.next());
        assert_eq!((premiere, suivante), (Some(10), Some(50)));
        // La frise fermée (l'envoi lâché) : le fil s'arrête, plus aucune réponse.
        drop(envoi);
        assert_eq!(futures::executor::block_on(reponses.next()), None);
    }

    #[test]
    fn mot_cherche_surligne_sans_accents_ni_majuscules() {
        let texte = "« Hier on a parlé des Dauphins et du dauphin rose »";
        let plages = plages_cherchees(texte, "dauphin PARLE");
        let morceaux: Vec<&str> = plages.iter().map(|r| &texte[r.clone()]).collect();
        assert_eq!(morceaux, vec!["parlé", "Dauphin", "dauphin"]);
        assert!(plages_cherchees(texte, "x").is_empty());
    }

    #[test]
    fn secrets_caches_sur_l_image() {
        use bscreen_core::texte_lu::{MotLu, mots_d_une_ligne};
        let texte = "mdp : hunter2 ok";
        let mots = mots_d_une_ligne(texte)
            .into_iter()
            .map(|(d, f)| MotLu {
                texte: texte.chars().skip(d).take(f - d).collect(),
                cadre: [0.01 * d as f64, 0.5, 0.01 * (f - d) as f64, 0.02],
                debut: d,
                fin: f,
            })
            .collect();
        let lignes = vec![LigneLue {
            texte: texte.into(),
            cadre: [0.0, 0.5, 0.16, 0.02],
            mots,
            bords: (0..=16).map(|i| 0.01 * i as f64).collect(),
        }];
        let (lignes, cadres) = cacher_secrets(lignes);
        assert_eq!(cadres, vec![[0.06, 0.5, 0.07, 0.02]]);
        assert_eq!(lignes[0].texte, "mdp : ••••••• ok", "même nombre de caractères : la sélection reste alignée");
        assert_eq!(lignes[0].mots[3].texte, "ok");
    }

    #[test]
    fn filtres_d_app_dans_la_recherche() {
        let (texte, apps, jours, voix) = separer_filtres("@Safari les dauphins @code @hier");
        assert!(!voix);
        assert!(separer_filtres("@dit bar").3 && separer_filtres("@dit bar").0 == "bar");
        assert_eq!(texte, "les dauphins");
        assert_eq!(apps, vec!["safari".to_string(), "code".to_string()]);
        assert_eq!(jours, vec![1]);
        assert_eq!(separer_filtres("email @ maison").0, "email @ maison");
        assert_eq!(jours_en_arriere("lundi", chrono::Weekday::Fri), Some(4));
        assert_eq!(jours_en_arriere("vendredi", chrono::Weekday::Fri), Some(0));
        assert_eq!(jours_en_arriere("samedi", chrono::Weekday::Fri), Some(6));
        assert_eq!(jours_en_arriere("safari", chrono::Weekday::Fri), None);
        let m = |nom: &str, bundle: &str| Moment {
            id: 1,
            quand_ms: 0,
            fichier: String::new(),
            position_ms: 0,
            app_bundle: Some(bundle.into()),
            app_nom: Some(nom.into()),
            largeur: 1,
            hauteur: 1,
            url: None,
            fenetre: None,
            privee: false,
        };
        assert!(Frise::dans_les_apps(&m("VSCodium", "com.vscodium"), &["codium".into()]));
        assert!(!Frise::dans_les_apps(&m("Safari", "com.apple.Safari"), &["code".into()]));
        assert!(Frise::dans_les_apps(&m("Safari", "com.apple.Safari"), &[]));
    }

    fn moment(id: i64, minute: i64, url: Option<&str>) -> Moment {
        Moment {
            id,
            quand_ms: minute * 60_000,
            fichier: String::new(),
            position_ms: 0,
            app_bundle: Some("com.apple.Safari".into()),
            app_nom: Some("Safari".into()),
            largeur: 1,
            hauteur: 1,
            url: url.map(str::to_string),
            fenetre: None,
            privee: false,
        }
    }

    #[test]
    fn resultats_regroupes_par_endroit() {
        // 0-4 : la même page ; 5 : une autre page ; 6 : la première page 40 min plus tard ;
        // 7-8 : deux voix à 2 minutes d'écart (un échange) ; 9 : « barre » seulement (début de mot).
        let moments = vec![
            moment(0, 0, Some("https://a.fr/bar#haut")),
            moment(1, 2, Some("https://a.fr/bar")),
            moment(2, 3, Some("https://a.fr/bar")),
            moment(3, 4, Some("https://a.fr/bar#bas")),
            moment(4, 5, Some("https://a.fr/bar")),
            moment(5, 6, Some("https://b.fr")),
            moment(6, 45, Some("https://a.fr/bar")),
            moment(7, 50, None),
            moment(8, 52, None),
            moment(9, 60, Some("https://c.fr")),
        ];
        let resultats: Vec<usize> = (0..10).collect();
        let entendus: HashSet<i64> = [7, 8].into_iter().collect();
        let visibles: HashSet<i64> = [2, 5, 6, 9].into_iter().collect();
        let exacts: HashSet<i64> = (0..10).collect();
        let entiers: HashSet<i64> = (0..9).collect();
        let extraits: HashMap<i64, String> = [(7, "on va au bar".to_string())].into_iter().collect();
        let marques = Marques {
            entendus: Some(&entendus),
            visibles: Some(&visibles),
            exacts: Some(&exacts),
            entiers: Some(&entiers),
            extraits: Some(&extraits),
            ..Default::default()
        };
        let groupes = regrouper(&moments, &resultats, &marques);
        let vues: Vec<(Vec<usize>, usize)> = groupes.iter().map(|g| (g.indices.clone(), g.vedette)).collect();
        assert_eq!(
            vues,
            vec![
                (vec![7, 8], 7),          // l'échange de voix, le plus récent des mots entiers
                (vec![6], 6),             // la première page, revue 40 min plus tard
                (vec![5], 5),             // l'autre page
                (vec![0, 1, 2, 3, 4], 2), // la page, montrée là où le mot est visible
                (vec![9], 9),             // « barre » : après les mots entiers
            ]
        );
        assert!(groupes[0].entendu && groupes[0].extrait.as_deref() == Some("on va au bar"));
        assert!(!groupes[4].entier);
    }

    #[test]
    fn extrait_autour_du_mot() {
        let long = format!("{} on se retrouve au Bär du port ce soir {}", "début ".repeat(30), "fin ".repeat(30));
        let e = extrait_autour(&long, &["bar".to_string()], 40);
        assert!(e.contains("Bär du port") && e.starts_with('…') && e.ends_with('…'), "{e}");
        assert_eq!(extrait_autour("court  texte", &["absent".to_string()], 40), "court texte");
    }
}
