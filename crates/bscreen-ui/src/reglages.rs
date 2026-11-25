//! La fenêtre de réglages (style Verre) : tout ce que le menu permet, en plus clair, et ce qu'il ne
//! permet pas (vocabulaire mot à mot, apps exclues une à une, voix connues à réécouter et nommer,
//! état du chiffrement, de la recherche par le sens et du disque).
//!
//! Comme la frise, elle ne parle pas à macOS : tout passe par [`SourceReglages`]. Les changements
//! prennent le même chemin que ceux du menu, qui reste donc d'accord avec la fenêtre.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bscreen_core::image::ImageBgra;
use bscreen_core::reglages::{Changement, Reglages};
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, ClipboardItem, Context, FocusHandle, FontWeight, Global, KeyBinding,
    KeyDownEvent, MouseButton, MouseDownEvent, RenderImage, SharedString, Task, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, actions, div, img, point, prelude::*, px, rgb,
    rgba, size,
};

use crate::frise::{en_rendu, icone_loupe};

actions!(reglages, [FermerReglages]);

/// Une app (identifiant et nom affiché).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConnue {
    pub identifiant: String,
    pub nom: String,
}

/// Une voix connue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Personne {
    pub id: i64,
    /// « Toi », « Camille », « Voix 3 ».
    pub nom: String,
    /// Elle a un vrai nom (pas « Voix 3 »).
    pub nommee: bool,
    pub passages: u32,
    /// Un passage à réécouter existe (hors navigation privée).
    pub ecoutable: bool,
}

/// Tout ce que la fenêtre affiche, relu toutes les quelques secondes.
#[derive(Clone, Debug, Default)]
pub struct EtatReglages {
    pub reglages: Reglages,
    /// Ouvrir à la connexion : `None` si impossible ici (app pas lancée depuis son paquet).
    pub connexion: Option<bool>,
    pub connexion_a_approuver: bool,
    pub apps_exclues: Vec<AppConnue>,
    pub apps_toujours_exclues: Vec<AppConnue>,
    pub personnes: Vec<Personne>,
    /// Place prise et libre, déjà lisibles (« 2,1 Go »).
    pub video: String,
    pub voix: String,
    pub base: String,
    pub libre: String,
    /// Processeur et mémoire de B-Screen sur la dernière minute (« 0,6 % », « 74 Mo »).
    pub processeur: String,
    pub memoire: String,
    /// Les données sont dans l'image chiffrée (ouverte).
    pub chiffre: bool,
    /// Recherche par le sens : (textes compris, textes en attente).
    pub sens: Option<(u64, u64)>,
    /// Recherche dans les images : combien d'images comprises (une par scène).
    pub images: Option<u64>,
    /// L'archive du homelab (phase 10).
    pub archive: EtatArchive,
    /// Ce qu'Ask a retenu de tes corrections (« Retenu : … »), du plus récent au plus ancien.
    pub corrections: Vec<CorrectionRetenue>,
    /// Les sites les plus vus ces 30 derniers jours (domaine, images).
    pub sites_frequents: Vec<(String, u64)>,
    /// Les autorisations de macOS : (nom, à quoi elle sert, accordée, refusée).
    pub autorisations: Vec<(String, String, bool, bool)>,
    pub version: String,
    /// La commande qui branche la mémoire dans Claude Code.
    pub commande_mcp: String,
}

/// Une correction retenue par Ask.
#[derive(Clone, Debug)]
pub struct CorrectionRetenue {
    pub id: i64,
    /// « lun. 23/11 21:56 ».
    pub quand: String,
    pub texte: String,
}

/// L'archive du homelab : ce qui est parti, la dernière nuit, le robot Teleport.
#[derive(Clone, Debug, Default)]
pub struct EtatArchive {
    pub fichiers: u64,
    /// Déjà lisible (« 1,2 Go »).
    pub taille: String,
    /// « lun. 23/11 03:10 », ou rien si aucune nuit encore.
    pub derniere: Option<String>,
    pub robot: bool,
}

/// Ce dont la fenêtre a besoin, fourni par l'app (couche macOS).
pub trait SourceReglages: Send + Sync {
    fn etat(&self) -> EtatReglages;
    /// Applique un changement (même chemin que le menu) ; faux si rien n'a changé ou si Julien a refusé.
    fn changer(&self, changement: Changement) -> bool;
    fn changer_connexion(&self, active: bool);
    /// Les apps ouvertes qu'on peut exclure.
    fn apps_ouvertes(&self) -> Vec<AppConnue>;
    fn icone_app(&self, identifiant: &str) -> Option<ImageBgra>;
    /// Les noms propres ajoutés tout seuls au vocabulaire (vus souvent à l'écran). Peut être lent.
    fn vocabulaire_auto(&self) -> Vec<String>;
    fn nommer(&self, personne: i64, nom: &str) -> bool;
    /// Autorisation `rang` : la demander (ou ouvrir les Réglages du Mac si elle est déjà donnée ou refusée).
    fn autorisation(&self, rang: usize);
    /// Oublie une correction retenue par Ask ; faux si elle n'existe plus.
    fn oublier_correction(&self, id: i64) -> bool;
    /// Joue un passage de cette voix ; rend sa durée (s).
    fn ecouter(&self, personne: i64) -> Option<f64>;
    fn arreter_ecoute(&self);
    fn copier_jeton(&self) -> bool;
    /// Ouvre la page web dans le navigateur du Mac (avec son jeton, que la fenêtre ne voit pas).
    fn ouvrir_page_web(&self) -> bool;
    /// « Effacer » (0 : 5 dernières minutes, 1 : dernière heure, 2 : aujourd'hui), avec confirmation.
    fn effacer(&self, rang: usize);
    /// Mode d'essai : photographie la fenêtre (`vue` : la vue native) sous ce nom.
    fn photographier(&self, _vue: *mut std::ffi::c_void, _nom: &str) {}
}

struct ReglagesOuverts(Option<AnyWindowHandle>);
impl Global for ReglagesOuverts {}

pub fn lier_touches(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-w", FermerReglages, Some("Reglages"))]);
}

/// Mode d'essai : ouvre les réglages, photographie chaque partie, puis referme.
pub fn essai_photos(cx: &mut App, source: Arc<dyn SourceReglages>) {
    ouvrir(cx, Arc::clone(&source));
    let Some(fenetre) = cx.try_global::<ReglagesOuverts>().and_then(|r| r.0) else {
        return;
    };
    cx.spawn(async move |cx| {
        for (i, section) in Section::TOUTES.into_iter().enumerate() {
            let _ = fenetre.update(cx, |vue, _, cx| {
                if let Ok(vue) = vue.downcast::<FenetreReglages>() {
                    vue.update(cx, |f, cx| {
                        f.section = section;
                        cx.notify();
                    });
                }
            });
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let source = Arc::clone(&source);
            let _ = fenetre.update(cx, |_, window, _| {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(poignee) = window.window_handle()
                    && let RawWindowHandle::AppKit(appkit) = poignee.as_raw()
                {
                    source.photographier(appkit.ns_view.as_ptr(), &format!("reglages-{}-{section:?}", i + 1));
                }
            });
        }
        let _ = fenetre.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
}

/// Ouvre la fenêtre de réglages (ou la ramène devant si elle est déjà ouverte).
pub fn ouvrir(cx: &mut App, source: Arc<dyn SourceReglages>) {
    if let Some(fenetre) = cx.try_global::<ReglagesOuverts>().and_then(|r| r.0)
        && fenetre.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        cx.activate(true);
        return;
    }
    let limites = Bounds::centered(None, size(px(LARGEUR), px(HAUTEUR)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(limites)),
        titlebar: Some(TitlebarOptions {
            title: Some("Réglages de B-Screen".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(18.0), px(20.0))),
        }),
        kind: WindowKind::Normal,
        window_background: WindowBackgroundAppearance::Opaque,
        focus: true,
        show: true,
        is_movable: true,
        is_resizable: false,
        is_minimizable: true,
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| cx.new(|cx| FenetreReglages::new(window, cx, source))) {
        Ok(fenetre) => {
            cx.set_global(ReglagesOuverts(Some(fenetre.into())));
            cx.activate(true);
        }
        Err(e) => eprintln!("réglages impossibles : {e}"),
    }
}

const LARGEUR: f32 = 800.0;
const HAUTEUR: f32 = 580.0;
const LARGEUR_COTE: f32 = 214.0;
/// Hauteur de la bande du haut (boutons de la fenêtre), où l'on peut la déplacer.
const BANDE_HAUTE: f32 = 52.0;

// Couleurs du style Verre (celles de la frise : système d'Apple en sombre).
const BLANC: u32 = 0xFFFFFF;
const BLEU: u32 = 0x0A84FF;
const ROUGE: u32 = 0xFF453A;
const VERT: u32 = 0x30D158;
const ORANGE: u32 = 0xFF9F0A;
const SECONDAIRE: u32 = 0xEBEBF599;
const TERTIAIRE: u32 = 0xEBEBF566;
const FOND: u32 = 0x1E1E20;
const FOND_COTE: u32 = 0x161618;
const CARTE: u32 = 0xFFFFFF0D;
const FILET: u32 = 0xFFFFFF14;
const CHOISI: u32 = 0xFFFFFF1F;
const BOUTON: u32 = 0xFFFFFF1F;
const BOUTON_SURVOL: u32 = 0xFFFFFF33;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    General,
    Voix,
    Confidentialite,
    Recherche,
    Programmes,
    Stockage,
}

impl Section {
    const TOUTES: [Section; 6] = [
        Section::General,
        Section::Voix,
        Section::Confidentialite,
        Section::Recherche,
        Section::Programmes,
        Section::Stockage,
    ];

    fn titre(self) -> &'static str {
        match self {
            Section::General => "Général",
            Section::Voix => "Voix",
            Section::Confidentialite => "Confidentialité",
            Section::Recherche => "Recherche",
            Section::Programmes => "Autres programmes",
            Section::Stockage => "Stockage",
        }
    }

    /// Une pastille de couleur devant le nom, comme les Réglages du Mac.
    fn couleur(self) -> u32 {
        match self {
            Section::General => 0x8E8E93,
            Section::Voix => 0xFF375F,
            Section::Confidentialite => BLEU,
            Section::Recherche => 0xBF5AF2,
            Section::Programmes => ORANGE,
            Section::Stockage => VERT,
        }
    }

    fn signe(self) -> &'static str {
        match self {
            Section::General => "⚙",
            Section::Voix => "♪",
            Section::Confidentialite => "✋",
            Section::Recherche => "⌕",
            Section::Programmes => "⌘",
            Section::Stockage => "▤",
        }
    }
}

/// Ce que le clavier remplit.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Saisie {
    Mot(String),
    Nom(i64, String),
    /// Le prochain raccourci tapé (0 : la frise, 1 : Ask, 2 : la pause).
    Raccourci(u8),
    /// Un site à masquer dans la recherche.
    SiteCache(String),
    /// Un site à ne jamais enregistrer.
    Site(String),
}

pub struct FenetreReglages {
    source: Arc<dyn SourceReglages>,
    etat: EtatReglages,
    section: Section,
    focus: FocusHandle,
    saisie: Option<Saisie>,
    /// Les apps ouvertes, quand on en choisit une à exclure.
    choix_app: Option<Vec<AppConnue>>,
    icones: HashMap<String, Option<Arc<RenderImage>>>,
    logo: Option<Arc<RenderImage>>,
    vocabulaire_auto: Option<Vec<String>>,
    /// La voix en écoute, et un numéro pour effacer l'état à la fin du passage.
    ecoute: Option<(i64, u64)>,
    numero: u64,
    /// Message bref en bas (« Jeton copié »), et son numéro.
    message: Option<(String, u64)>,
    _rafraichir: Task<()>,
}

impl Drop for FenetreReglages {
    fn drop(&mut self) {
        if self.ecoute.is_some() {
            self.source.arreter_ecoute();
        }
    }
}

impl FenetreReglages {
    fn new(window: &mut Window, cx: &mut Context<Self>, source: Arc<dyn SourceReglages>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        // Les chiffres (place, processeur, index du sens) bougent : relus toutes les 4 s.
        let rafraichir = cx.spawn(async move |vue, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(4)).await;
                let suite = vue.update(cx, |fenetre, cx| {
                    fenetre.etat = fenetre.source.etat();
                    cx.notify();
                });
                if suite.is_err() {
                    break;
                }
            }
        });
        // Le vocabulaire automatique se calcule sur une semaine de texte : hors du fil principal.
        let pour_calcul = Arc::clone(&source);
        let calcul = cx.background_executor().spawn(async move { pour_calcul.vocabulaire_auto() });
        cx.spawn(async move |vue, cx| {
            let mots = calcul.await;
            let _ = vue.update(cx, |fenetre, cx| {
                fenetre.vocabulaire_auto = Some(mots);
                cx.notify();
            });
        })
        .detach();
        let logo = source.icone_app(bscreen_core::IDENTIFIANT).and_then(en_rendu);
        Self {
            etat: source.etat(),
            source,
            section: Section::General,
            focus,
            saisie: None,
            choix_app: None,
            icones: HashMap::new(),
            logo,
            vocabulaire_auto: None,
            ecoute: None,
            numero: 0,
            message: None,
            _rafraichir: rafraichir,
        }
    }

    fn changer(&mut self, changement: Changement, cx: &mut Context<Self>) {
        self.source.changer(changement);
        self.etat = self.source.etat();
        cx.notify();
    }

    fn annoncer(&mut self, texte: impl Into<String>, cx: &mut Context<Self>) {
        self.numero += 1;
        let numero = self.numero;
        self.message = Some((texte.into(), numero));
        cx.notify();
        cx.spawn(async move |vue, cx| {
            cx.background_executor().timer(Duration::from_millis(2200)).await;
            let _ = vue.update(cx, |fenetre, cx| {
                if fenetre.message.as_ref().is_some_and(|(_, n)| *n == numero) {
                    fenetre.message = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn icone(&mut self, identifiant: &str) -> Option<Arc<RenderImage>> {
        if !self.icones.contains_key(identifiant) {
            let icone = self.source.icone_app(identifiant).and_then(en_rendu);
            self.icones.insert(identifiant.to_string(), icone);
        }
        self.icones.get(identifiant).cloned().flatten()
    }

    fn ecouter(&mut self, personne: i64, cx: &mut Context<Self>) {
        if self.ecoute.take().is_some_and(|(id, _)| id == personne) {
            self.source.arreter_ecoute();
            cx.notify();
            return;
        }
        let Some(duree) = self.source.ecouter(personne) else {
            self.annoncer("Rien à réécouter pour cette voix", cx);
            return;
        };
        self.numero += 1;
        let numero = self.numero;
        self.ecoute = Some((personne, numero));
        cx.notify();
        cx.spawn(async move |vue, cx| {
            cx.background_executor().timer(Duration::from_secs_f64(duree.max(0.0) + 0.2)).await;
            let _ = vue.update(cx, |fenetre, cx| {
                if fenetre.ecoute.is_some_and(|(_, n)| n == numero) {
                    fenetre.ecoute = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Le clavier : seulement pour un champ en cours de saisie (mot du vocabulaire, nom d'une voix).
    fn touche(&mut self, evenement: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let touche = &evenement.keystroke;
        if touche.key == "escape" {
            if self.saisie.take().is_some() || self.choix_app.take().is_some() {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let Some(saisie) = self.saisie.as_mut() else { return };
        if let Saisie::Raccourci(quel) = *saisie {
            // Les touches seules de modification n'arrivent pas ici : on attend la touche finale.
            let m = &touche.modifiers;
            let mut morceaux: Vec<&str> = Vec::new();
            if m.control {
                morceaux.push("ctrl");
            }
            if m.alt {
                morceaux.push("alt");
            }
            if m.shift {
                morceaux.push("shift");
            }
            if m.platform {
                morceaux.push("cmd");
            }
            let cle = touche.key.to_lowercase();
            morceaux.push(&cle);
            let texte = morceaux.join("+");
            let changement = match quel {
                0 => Changement::RaccourciFrise(texte.clone()),
                1 => Changement::RaccourciAsk(texte.clone()),
                _ => Changement::RaccourciPause(texte.clone()),
            };
            self.saisie = None;
            if self.source.changer(changement) {
                self.annoncer(format!("Raccourci : {}", bscreen_core::reglages::raccourci_lisible(&texte)), cx);
            } else {
                self.annoncer("Il faut ⌃, ⌥ ou ⌘ avec une lettre, un chiffre, Espace ou F1-F12 (et pas l'autre raccourci)", cx);
            }
            self.etat = self.source.etat();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let texte = match saisie {
            Saisie::Mot(t) | Saisie::Nom(_, t) | Saisie::Site(t) | Saisie::SiteCache(t) => t,
            Saisie::Raccourci(_) => return,
        };
        match touche.key.as_str() {
            "enter" => {
                let fini = self.saisie.take();
                match fini {
                    Some(Saisie::Mot(mot)) if !mot.trim().is_empty() => {
                        let mot = mot.trim().to_string();
                        if self.source.changer(Changement::AjouterMot(mot.clone())) {
                            self.annoncer(format!("« {mot} » ajouté au vocabulaire"), cx);
                        } else {
                            self.annoncer(format!("« {mot} » y est déjà"), cx);
                        }
                        // On reste dans le champ pour enchaîner les mots.
                        self.saisie = Some(Saisie::Mot(String::new()));
                        self.etat = self.source.etat();
                    }
                    Some(Saisie::SiteCache(site)) if !site.trim().is_empty() => {
                        let site = site.trim().to_string();
                        if self.source.changer(Changement::CacherSite(site.clone())) {
                            self.annoncer(format!("« {site} » masqué dans la recherche"), cx);
                        } else {
                            self.annoncer("Pas un site (exemple : youtube.com), ou déjà masqué", cx);
                        }
                        self.etat = self.source.etat();
                    }
                    Some(Saisie::Site(site)) if !site.trim().is_empty() => {
                        let site = site.trim().to_string();
                        if self.source.changer(Changement::ExclureSite(site.clone())) {
                            self.annoncer(format!("« {site} » ne sera plus enregistré"), cx);
                        } else {
                            self.annoncer("Pas un site (exemple : banque.fr), ou déjà dans la liste", cx);
                        }
                        self.etat = self.source.etat();
                    }
                    Some(Saisie::Nom(id, nom)) if !nom.trim().is_empty() => {
                        let nom = nom.trim().to_string();
                        if self.source.nommer(id, &nom) {
                            self.annoncer(format!("Voix reconnue : {nom}"), cx);
                        } else {
                            self.annoncer("Nom pas enregistré", cx);
                        }
                        self.etat = self.source.etat();
                    }
                    _ => {}
                }
            }
            "backspace" => {
                texte.pop();
            }
            _ => {
                if !touche.modifiers.platform
                    && !touche.modifiers.control
                    && let Some(caractere) = touche.key_char.as_ref()
                    && !caractere.chars().any(char::is_control)
                    && texte.chars().count() < 40
                {
                    texte.push_str(caractere);
                } else {
                    return;
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Des segments (une valeur au choix) qui envoient `changement(valeur)`.
    fn choix_segments(
        &mut self,
        id: &'static str,
        options: Vec<(u32, &'static str)>,
        actuel: u32,
        changement: fn(u32) -> Changement,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let choix: Vec<gpui::AnyElement> = options
            .into_iter()
            .map(|(valeur, nom)| {
                segment((id, valeur as usize), nom, actuel == valeur, cx.listener(move |f, _: &ClickEvent, _, cx| {
                    f.changer(changement(valeur), cx)
                }))
                .into_any_element()
            })
            .collect();
        div().flex().flex_row().p(px(2.0)).rounded(px(8.0)).bg(rgba(0x0000002E)).children(choix)
    }

    // ---------- les sections ----------

    fn general(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let r = self.etat.reglages.clone();
        let mut lignes = vec![
            ligne(
                "Enregistrer l'écran et les voix",
                if r.pause && r.pause_jusqu_ms > 0 {
                    let reprise = chrono::TimeZone::timestamp_millis_opt(&chrono::Local, r.pause_jusqu_ms)
                        .single()
                        .map_or(String::new(), |d| d.format("%d/%m à %H:%M").to_string());
                    format!("En pause jusqu'au {reprise}, puis la capture reprend")
                } else if r.pause {
                    "En pause : rien n'est enregistré".to_string()
                } else {
                    format!("Une image toutes les {} s au plus, quand l'écran change", r.intervalle_capture_s)
                },
                interrupteur("pause", !r.pause, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                    f.changer(Changement::Pause(!r.pause), cx)
                })),
            ),
        ];
        let connexion = self.etat.connexion;
        let detail_connexion = match connexion {
            None => "Seulement depuis B-Screen.app".to_string(),
            Some(_) if self.etat.connexion_a_approuver => "À approuver dans les Réglages du Mac".to_string(),
            Some(_) => "B-Screen démarre avec ta session".to_string(),
        };
        lignes.push(ligne(
            "Ouvrir à la connexion",
            detail_connexion,
            interrupteur("connexion", connexion == Some(true), connexion.is_some(), cx.listener(
                move |f, _: &ClickEvent, _, cx| {
                    f.source.changer_connexion(connexion != Some(true));
                    f.etat = f.source.etat();
                    cx.notify();
                },
            )),
        ));
        let intervalle = self.choix_segments(
            "intervalle",
            vec![(1, "1 s"), (2, "2 s"), (5, "5 s"), (10, "10 s")],
            r.intervalle_capture_s,
            Changement::IntervalleCapture,
            cx,
        );
        lignes.push(ligne(
            "Écart entre deux images",
            "Plus court : plus de moments à revoir, un peu plus de place et de processeur",
            intervalle,
        ));
        // Mettre en pause pour un temps, puis la capture reprend seule.
        if !r.pause {
            let maintenant = chrono::Local::now();
            let demain_8h = (maintenant.date_naive() + chrono::Duration::days(1))
                .and_hms_opt(8, 0, 0)
                .and_then(|d| d.and_local_timezone(chrono::Local).earliest())
                .map_or(0, |d| d.timestamp_millis());
            let ms = maintenant.timestamp_millis();
            let boutons = [("15 min", ms + 15 * 60_000), ("1 h", ms + 3_600_000), ("Jusqu'à demain 8 h", demain_8h)]
                .into_iter()
                .enumerate()
                .fold(div().flex().flex_row().gap(px(6.0)), |d, (rang, (nom, quand))| {
                    d.child(bouton(("pause-pour", rang), nom).on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.changer(Changement::PauseJusqua(quand), cx)
                    })))
                });
            lignes.push(ligne("Mettre en pause pendant", "La capture reprend toute seule", boutons));
        }
        for (quel, titre, detail, actuel) in [
            (0u8, "Ouvrir la frise", "Depuis n'importe quelle app (ou le menu de B-Screen)", r.raccourci_frise.clone()),
            (1, "Demander (Ask)", "Une question sur ce que tu as vu ou entendu", r.raccourci_ask.clone()),
            (2, "Pause / reprise", "Mettre en pause ou reprendre sans ouvrir de fenêtre", r.raccourci_pause.clone()),
        ] {
            let en_saisie = matches!(self.saisie, Some(Saisie::Raccourci(q)) if q == quel);
            let touches = if en_saisie {
                "Tape le raccourci…".to_string()
            } else if actuel.is_empty() {
                "Aucun".to_string()
            } else {
                bscreen_core::reglages::raccourci_lisible(&actuel)
            };
            lignes.push(ligne_libre(
                div().flex().flex_col().gap(px(3.0)).child(div().text_size(px(13.5)).child(titre)).child(texte_secondaire(detail)),
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(touche_clavier_texte(touches))
                    .when(quel == 2 && !actuel.is_empty() && !en_saisie, |d| {
                        d.child(bouton("retirer-raccourci-pause", "Retirer").on_click(cx.listener(
                            |f, _: &ClickEvent, _, cx| f.changer(Changement::RaccourciPause(String::new()), cx),
                        )))
                    })
                    .child(
                        bouton(("raccourci", usize::from(quel)), if en_saisie { "Annuler" } else if actuel.is_empty() { "Définir" } else { "Changer" })
                            .on_click(cx.listener(move |f, _: &ClickEvent, window, cx| {
                                f.saisie = if en_saisie { None } else { Some(Saisie::Raccourci(quel)) };
                                f.focus.focus(window);
                                cx.notify();
                            })),
                    ),
            ));
        }
        let jours = r.conservation_jours;
        let choix: Vec<gpui::AnyElement> = [(30, "30 jours"), (90, "90 jours"), (365, "1 an"), (0, "Tout")]
            .into_iter()
            .map(|(valeur, nom)| {
                segment(("garder", valeur as usize), nom, jours == valeur, cx.listener(move |f, _: &ClickEvent, _, cx| {
                    f.changer(Changement::Conservation(valeur), cx)
                }))
                .into_any_element()
            })
            .collect();
        let autorisations: Vec<gpui::AnyElement> = self
            .etat
            .autorisations
            .clone()
            .into_iter()
            .enumerate()
            .map(|(rang, (nom, raison, accordee, refusee))| {
                let (badge_texte, couleur, action) = if accordee {
                    ("Accordée", VERT, "Réglages du Mac")
                } else if refusee {
                    ("Refusée", ORANGE, "Réglages du Mac")
                } else {
                    ("À donner", ORANGE, "Autoriser…")
                };
                ligne_libre(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(div().flex().flex_row().items_center().gap(px(8.0)).child(div().text_size(px(13.5)).child(nom)).child(badge(badge_texte, couleur)))
                        .child(texte_secondaire(raison)),
                    bouton(("autorisation", rang), action).on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.source.autorisation(rang);
                        f.etat = f.source.etat();
                        cx.notify();
                    })),
                )
                .into_any_element()
            })
            .collect();
        vec![
            carte(None, lignes).into_any_element(),
            carte(Some("Autorisations"), autorisations).into_any_element(),
            carte(
                Some("Garder"),
                vec![ligne(
                    "Combien de temps garder images, texte et voix",
                    if r.archive_jours > 0 && self.etat.archive.robot {
                        "L'archive du homelab est en service : rien n'est effacé (voir Stockage)"
                    } else {
                        "Raccourcir efface tout de suite le plus vieux (on te le demande avant)"
                    },
                    div().flex().flex_row().p(px(2.0)).rounded(px(8.0)).bg(rgba(0x0000002E)).children(choix),
                )],
            )
            .into_any_element(),
        ]
    }

    fn voix(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let r = self.etat.reglages.clone();
        let sources = carte(
            Some("Enregistrer"),
            vec![
                ligne(
                    "Les voix captées par le micro",
                    "Seulement quand quelqu'un parle · micro intégré, jamais celui des AirPods",
                    interrupteur("micro", r.micro, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.changer(Changement::Micro(!r.micro), cx)
                    })),
                ),
                ligne(
                    "Les voix qui sortent du Mac",
                    "Appels, vidéos… seulement quand ça parle, pas la musique",
                    interrupteur("son", r.son_du_mac, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.changer(Changement::SonDuMac(!r.son_du_mac), cx)
                    })),
                ),
                ligne(
                    "Les appels",
                    if r.voix_appels {
                        "FaceTime, WhatsApp, Teams… : les deux côtés sont enregistrés"
                    } else {
                        "Pendant un appel, aucune voix n'est enregistrée (ni la tienne, ni celle d'en face)"
                    },
                    interrupteur("appels", r.voix_appels, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.changer(Changement::VoixAppels(!r.voix_appels), cx)
                    })),
                ),
            ],
        );

        // Vocabulaire : les mots de Julien (retirables), le champ pour en ajouter, puis ceux ajoutés tout seuls.
        let mut puces: Vec<gpui::AnyElement> = r
            .vocabulaire
            .iter()
            .enumerate()
            .map(|(i, mot)| {
                let a_retirer = mot.clone();
                puce(mot.clone())
                    .child(
                        div()
                            .id(("retirer-mot", i))
                            .ml(px(6.0))
                            .px(px(4.0))
                            .rounded(px(4.0))
                            .text_color(rgba(SECONDAIRE))
                            .hover(|s| s.bg(rgba(BOUTON_SURVOL)).text_color(rgb(BLANC)))
                            .cursor_pointer()
                            .child("✕")
                            .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                                f.changer(Changement::RetirerMot(a_retirer.clone()), cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let saisie_mot = match &self.saisie {
            Some(Saisie::Mot(t)) => Some(t.clone()),
            _ => None,
        };
        puces.push(
            champ("nouveau-mot", saisie_mot.as_deref(), "Ajouter un mot…", 150.0)
                .on_click(cx.listener(|f, _: &ClickEvent, window, cx| {
                    f.saisie = Some(Saisie::Mot(String::new()));
                    f.focus.focus(window);
                    cx.notify();
                }))
                .into_any_element(),
        );
        let auto: gpui::AnyElement = match &self.vocabulaire_auto {
            None => texte_secondaire("Calcul des mots vus souvent à l'écran…").into_any_element(),
            Some(mots) if mots.is_empty() => {
                texte_secondaire("Aucun nom propre vu assez souvent cette semaine.").into_any_element()
            }
            Some(mots) => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(texte_secondaire(
                    "Ajoutés tout seuls, parce que tu les vois souvent à l'écran (relu chaque heure) :",
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(6.0))
                        .children(mots.iter().take(40).map(|m| puce_discrete(m.clone()))),
                )
                .into_any_element(),
        };
        let vocabulaire = carte(
            Some("Vocabulaire"),
            vec![
                bloc(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(texte_secondaire(
                            "Les mots à bien reconnaître dans les voix : prénoms, noms de projets… (Entrée pour ajouter)",
                        ))
                        .child(div().flex().flex_row().flex_wrap().items_center().gap(px(6.0)).children(puces)),
                ),
                bloc(auto),
            ],
        );

        // Voix connues : réécouter un passage, puis nommer.
        let personnes = self.etat.personnes.clone();
        let lignes_voix: Vec<gpui::AnyElement> = if personnes.is_empty() {
            vec![bloc(texte_secondaire("Aucune voix encore : elles apparaissent quand quelqu'un parle.")).into_any_element()]
        } else {
            personnes
                .into_iter()
                .map(|p| {
                    let id = p.id;
                    let en_ecoute = self.ecoute.is_some_and(|(e, _)| e == id);
                    let saisie_nom = match &self.saisie {
                        Some(Saisie::Nom(n, t)) if *n == id => Some(t.clone()),
                        _ => None,
                    };
                    let titre: gpui::AnyElement = match saisie_nom {
                        Some(t) => champ(("nom", id as usize), Some(&t), "Qui parle ?", 180.0).into_any_element(),
                        None => div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .size(px(10.0))
                                    .rounded_full()
                                    .bg(rgb(if p.nommee { VERT } else { 0x8E8E93 })),
                            )
                            .child(div().text_size(px(13.5)).child(p.nom.clone()))
                            .into_any_element(),
                    };
                    let detail = format!("{} passage{}", p.passages, if p.passages > 1 { "s" } else { "" });
                    let mut boutons = div().flex().flex_row().gap(px(8.0));
                    if p.ecoutable {
                        boutons = boutons.child(
                            bouton(("ecouter", id as usize), if en_ecoute { "■  Arrêter" } else { "▶  Écouter" })
                                .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| f.ecouter(id, cx))),
                        );
                    }
                    let nom_actuel = if p.nommee { p.nom.clone() } else { String::new() };
                    boutons = boutons.child(
                        bouton(("nommer", id as usize), if p.nommee { "Renommer" } else { "Nommer" }).on_click(
                            cx.listener(move |f, _: &ClickEvent, window, cx| {
                                f.saisie = Some(Saisie::Nom(id, nom_actuel.clone()));
                                f.focus.focus(window);
                                cx.notify();
                            }),
                        ),
                    );
                    ligne_libre(
                        div().flex().flex_col().gap(px(3.0)).child(titre).child(texte_secondaire(detail)),
                        boutons,
                    )
                    .into_any_element()
                })
                .collect()
        };
        vec![
            sources.into_any_element(),
            vocabulaire.into_any_element(),
            carte(Some("Voix connues"), lignes_voix).into_any_element(),
        ]
    }

    fn confidentialite(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut lignes_apps: Vec<gpui::AnyElement> = Vec::new();
        for app in self.etat.apps_exclues.clone() {
            let icone = self.icone(&app.identifiant);
            let identifiant = app.identifiant.clone();
            lignes_apps.push(
                ligne_libre(
                    nom_d_app(icone, &app.nom),
                    bouton(SharedString::from(format!("reinclure-{identifiant}")), "Réenregistrer").on_click(
                        cx.listener(move |f, _: &ClickEvent, _, cx| {
                            f.changer(Changement::ReinclureApp(identifiant.clone()), cx)
                        }),
                    ),
                )
                .into_any_element(),
            );
        }
        if self.etat.apps_exclues.is_empty() {
            lignes_apps.push(bloc(texte_secondaire("Aucune app exclue par toi.")).into_any_element());
        }
        match self.choix_app.clone() {
            Some(apps) => {
                let mut liste = div().flex().flex_col().gap(px(2.0));
                if apps.is_empty() {
                    liste = liste.child(texte_secondaire("Aucune autre app ouverte."));
                }
                for app in apps {
                    let icone = self.icone(&app.identifiant);
                    let identifiant = app.identifiant.clone();
                    liste = liste.child(
                        div()
                            .id(SharedString::from(format!("exclure-{identifiant}")))
                            .px(px(8.0))
                            .py(px(5.0))
                            .rounded(px(7.0))
                            .hover(|s| s.bg(rgba(CHOISI)))
                            .cursor_pointer()
                            .child(nom_d_app(icone, &app.nom))
                            .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                                f.choix_app = None;
                                f.changer(Changement::ExclureApp(identifiant.clone()), cx);
                            })),
                    );
                }
                lignes_apps.push(
                    bloc(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_between()
                                    .items_center()
                                    .child(texte_secondaire("Choisis l'app à ne plus enregistrer :"))
                                    .child(bouton("annuler-choix", "Annuler").on_click(cx.listener(
                                        |f, _: &ClickEvent, _, cx| {
                                            f.choix_app = None;
                                            cx.notify();
                                        },
                                    ))),
                            )
                            .child(liste),
                    )
                    .into_any_element(),
                );
            }
            None => lignes_apps.push(
                bloc(
                    div().flex().flex_row().child(bouton("ajouter-app", "Ajouter une app…").on_click(cx.listener(
                        |f, _: &ClickEvent, _, cx| {
                            f.choix_app = Some(f.source.apps_ouvertes());
                            cx.notify();
                        },
                    ))),
                )
                .into_any_element(),
            ),
        }
        let toujours: Vec<String> = self.etat.apps_toujours_exclues.iter().map(|a| a.nom.clone()).collect();
        if !toujours.is_empty() {
            lignes_apps.push(
                bloc(texte_secondaire(format!(
                    "Toujours exclues : {} (et B-Screen).",
                    toujours.join(", ")
                )))
                .into_any_element(),
            );
        }

        let masquer = self.etat.reglages.masquer_secrets;
        let deverrouillage = self.choix_segments(
            "deverrouillage",
            vec![(1, "1 min"), (5, "5 min"), (15, "15 min"), (30, "30 min")],
            self.etat.reglages.deverrouillage_min,
            Changement::Deverrouillage,
            cx,
        );
        let protections = carte(
            Some("Protections"),
            vec![
                ligne(
                    "Navigation privée",
                    format!(
                        "Images verrouillées, visibles {} min après Touch ID ; ni texte, ni adresse, ni voix gardés",
                        self.etat.reglages.deverrouillage_min
                    ),
                    deverrouillage,
                ),
                ligne(
                    "Secrets masqués",
                    if masquer {
                        "Cartes, IBAN, clés d'API, mots de passe affichés : masqués avant tout stockage"
                    } else {
                        "Éteint : les secrets affichés sont gardés tels quels dans la mémoire"
                    },
                    interrupteur("masquer", masquer, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.changer(Changement::MasquerSecrets(!masquer), cx)
                    })),
                ),
                ligne(
                    "Chiffrement",
                    if self.etat.chiffre {
                        "Image chiffrée AES-256, ouverte au lancement avec la clé du Trousseau"
                    } else {
                        "Les données ne sont pas dans l'image chiffrée"
                    },
                    if self.etat.chiffre {
                        badge("Chiffré", VERT)
                    } else {
                        badge("Non chiffré", ORANGE)
                    },
                ),
            ],
        );
        let effacer: Vec<gpui::AnyElement> = ["Les 5 dernières minutes…", "La dernière heure…", "Aujourd'hui…"]
            .into_iter()
            .enumerate()
            .map(|(rang, nom)| {
                bouton(("effacer", rang), nom)
                    .text_color(rgb(ROUGE))
                    .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.source.effacer(rang);
                        f.etat = f.source.etat();
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        vec![
            carte(Some("Apps jamais enregistrées"), lignes_apps).into_any_element(),
            carte(Some("Sites jamais enregistrés"), self.lignes_sites(cx)).into_any_element(),
            protections.into_any_element(),
            carte(
                Some("Effacer"),
                vec![bloc(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(texte_secondaire(
                            "Images, texte, voix et recherches de la période, définitivement (on te le demande avant).",
                        ))
                        .child(div().flex().flex_row().gap(px(8.0)).children(effacer)),
                )],
            )
            .into_any_element(),
        ]
    }

    fn recherche(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let sens = match self.etat.sens {
            Some((compris, 0)) => format!("{} textes compris, tout est à jour", nombre(compris)),
            Some((compris, attente)) => format!("{} textes compris, {} en attente", nombre(compris), nombre(attente)),
            None => "État de l'index illisible".to_string(),
        };
        let r = self.etat.reglages.clone();
        let (sens_actif, images_actif, batterie) = (r.recherche_sens, r.recherche_images, r.indexation_batterie);
        let quand = self.choix_segments(
            "indexation",
            vec![(5, "5 min"), (20, "20 min"), (60, "1 h")],
            r.indexation_min,
            Changement::Indexation,
            cx,
        );
        let modeles: Vec<gpui::AnyElement> = [("haiku", "Rapide"), ("sonnet", "Équilibré"), ("opus", "Approfondi"), ("local", "Sur le Mac")]
            .into_iter()
            .enumerate()
            .map(|(rang, (valeur, nom))| {
                segment(("modele", rang), nom, r.ask_modele == valeur, cx.listener(move |f, _: &ClickEvent, _, cx| {
                    f.changer(Changement::ModeleAsk(valeur.to_string()), cx)
                }))
                .into_any_element()
            })
            .collect();
        vec![
            carte(
                None,
                vec![
                    ligne(
                        "Recherche par le sens",
                        if sens_actif { sens } else { "Éteinte : seulement les mots exacts".to_string() },
                        interrupteur("sens", sens_actif, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                            f.changer(Changement::RechercheSens(!sens_actif), cx)
                        })),
                    ),
                    ligne(
                        "Recherche dans les images",
                        match (images_actif, self.etat.images) {
                            (false, _) => "Éteinte : les images ne sont plus regardées".to_string(),
                            (true, Some(n)) => format!("{} images comprises (une par scène)", nombre(n)),
                            (true, None) => "Pas encore disponible".to_string(),
                        },
                        interrupteur("images", images_actif, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                            f.changer(Changement::RechercheImages(!images_actif), cx)
                        })),
                    ),
                    ligne(
                        "Texte hors de l'écran",
                        if r.texte_cache {
                            "Gardé aussi : la suite d'une page, les fenêtres derrière (deux fois plus de place)"
                        } else {
                            "Seulement le texte visible à l'écran (la moitié de la place)"
                        },
                        interrupteur("texte-cache", r.texte_cache, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                            f.changer(Changement::TexteCache(!r.texte_cache), cx)
                        })),
                    ),
                    ligne(
                        "Indexer au plus toutes les",
                        "Seulement s'il y a du nouveau ; tout de suite à l'ouverture de la frise",
                        quand,
                    ),
                    ligne(
                        "Indexer aussi sur batterie",
                        if batterie { "Quand le Mac est inactif depuis 2 min" } else { "Seulement sur secteur" },
                        interrupteur("batterie", batterie, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                            f.changer(Changement::IndexationBatterie(!batterie), cx)
                        })),
                    ),
                ],
            )
            .into_any_element(),
            carte(Some("Sites dans la recherche"), self.lignes_sites_recherche(cx)).into_any_element(),
            carte(
                Some("La nuit"),
                vec![ligne(
                    "Résumés et phrases floues",
                    if r.travail_nuit {
                        "Sur secteur, Mac inactif : le modèle local résume la journée et rend explicites les phrases floues (rien ne sort du Mac)"
                    } else {
                        "Éteint : rien n'est fait la nuit"
                    },
                    interrupteur("nuit", r.travail_nuit, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.changer(Changement::TravailNuit(!r.travail_nuit), cx)
                    })),
                )],
            )
            .into_any_element(),
            carte(
                Some("Ask"),
                vec![ligne(
                    "Modèle",
                    if r.ask_modele == "local" {
                        "Sur le Mac (Ollama) : rien ne sort du Mac"
                    } else {
                        "Claude, par ton abonnement (seulement la mémoire de B-Screen, en lecture seule)"
                    },
                    div().flex().flex_row().p(px(2.0)).rounded(px(8.0)).bg(rgba(0x0000002E)).children(modeles),
                )],
            )
            .into_any_element(),
            carte(
                Some("Dans la frise"),
                vec![bloc(texte_secondaire(
                    "Tape une idée sans ses mots exacts (4 lettres au moins) : les moments proches s'ajoutent aux \
                     résultats, avec le texte trouvé. « @safari », « @hier », « @lundi » filtrent ; Tab montre la grille.",
                ))],
            )
            .into_any_element(),
            carte(Some("Ce qu'Ask a retenu"), self.lignes_corrections(cx)).into_any_element(),
        ]
    }

    /// Les sites exclus, chacun avec « Réenregistrer », et le champ pour en ajouter.
    fn lignes_sites(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut lignes = vec![bloc(texte_secondaire(
            "Leurs pages sont traitées comme la navigation privée : verrouillées sous Touch ID, ni texte ni adresse gardés \
             (le domaine couvre ses sous-domaines).",
        ))
        .into_any_element()];
        for site in self.etat.reglages.sites_exclus.clone() {
            let s = site.clone();
            lignes.push(
                ligne_libre(
                    div().text_size(px(13.5)).child(site.clone()),
                    bouton(SharedString::from(format!("reinclure-site-{site}")), "Réenregistrer").on_click(cx.listener(
                        move |f, _: &ClickEvent, _, cx| f.changer(Changement::ReinclureSite(s.clone()), cx),
                    )),
                )
                .into_any_element(),
            );
        }
        let saisie = match &self.saisie {
            Some(Saisie::Site(t)) => Some(t.clone()),
            _ => None,
        };
        lignes.push(
            bloc(
                champ("nouveau-site", saisie.as_deref(), "Ajouter un site (banque.fr)…", 260.0).on_click(cx.listener(
                    |f, _: &ClickEvent, window, cx| {
                        f.saisie = Some(Saisie::Site(String::new()));
                        f.focus.focus(window);
                        cx.notify();
                    },
                )),
            )
            .into_any_element(),
        );
        lignes
    }

    /// Les sites les plus vus et ceux masqués, chacun montré ou masqué dans la recherche.
    fn lignes_sites_recherche(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let caches = self.etat.reglages.sites_caches.clone();
        let mut sites: Vec<(String, Option<u64>)> = self.etat.sites_frequents.iter().map(|(s, n)| (s.clone(), Some(*n))).collect();
        for c in &caches {
            if !sites.iter().any(|(s, _)| s == c) {
                sites.push((c.clone(), None));
            }
        }
        let mut lignes = vec![bloc(texte_secondaire(
            "Un site masqué reste enregistré, mais ne sort plus dans les résultats de la frise ni de la page web \
             (ni ce qui a été entendu pendant qu'il était à l'écran, comme le son d'une vidéo).",
        ))
        .into_any_element()];
        for (rang, (site, images)) in sites.into_iter().enumerate() {
            let montre = !caches.contains(&site);
            let s = site.clone();
            lignes.push(
                ligne(
                    site.clone(),
                    match images {
                        Some(n) => format!("{} images ces 30 derniers jours", nombre(n)),
                        None => "Masqué".to_string(),
                    },
                    interrupteur(("site-recherche", rang), montre, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                        let changement = if montre { Changement::CacherSite(s.clone()) } else { Changement::MontrerSite(s.clone()) };
                        f.changer(changement, cx)
                    })),
                )
                .into_any_element(),
            );
        }
        let saisie = match &self.saisie {
            Some(Saisie::SiteCache(t)) => Some(t.clone()),
            _ => None,
        };
        lignes.push(
            bloc(
                champ("nouveau-site-cache", saisie.as_deref(), "Masquer un autre site…", 260.0).on_click(cx.listener(
                    |f, _: &ClickEvent, window, cx| {
                        f.saisie = Some(Saisie::SiteCache(String::new()));
                        f.focus.focus(window);
                        cx.notify();
                    },
                )),
            )
            .into_any_element(),
        );
        lignes
    }

    /// Les corrections retenues par Ask, chacune avec « Oublier ».
    fn lignes_corrections(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut lignes = vec![bloc(texte_secondaire(
            "Quand tu corriges Ask dans une question de suite, il retient le fait et le redonne en tête de chaque \
             question. Oublie ici ce qui n'est plus vrai.",
        ))
        .into_any_element()];
        if self.etat.corrections.is_empty() {
            lignes.push(bloc(texte_secondaire("Rien de retenu pour l'instant.")).into_any_element());
        }
        for c in self.etat.corrections.clone() {
            let id = c.id;
            lignes.push(
                ligne_libre(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(div().text_size(px(13.5)).child(c.texte.clone()))
                        .child(texte_secondaire(c.quand.clone())),
                    bouton(("oublier-correction", id as usize), "Oublier").on_click(cx.listener(
                        move |f, _: &ClickEvent, _, cx| {
                            if f.source.oublier_correction(id) {
                                f.etat.corrections.retain(|c| c.id != id);
                                cx.notify();
                            }
                        },
                    )),
                )
                .into_any_element(),
            );
        }
        lignes
    }

    fn programmes(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let api = self.etat.reglages.api_locale;
        let commande = self.etat.commande_mcp.clone();
        vec![
            carte(
                Some("Claude Code (MCP)"),
                vec![
                    bloc(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(texte_secondaire(
                                "La mémoire de B-Screen pour Claude, en lecture seule : ce qui a été vu, dit, cherché ; \
                                 jamais les moments privés.",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(10.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .px(px(10.0))
                                            .py(px(7.0))
                                            .rounded(px(7.0))
                                            .bg(rgba(0x00000040))
                                            .font_family("Menlo")
                                            .text_size(px(11.5))
                                            .text_color(rgba(SECONDAIRE))
                                            .truncate()
                                            .child(commande.clone()),
                                    )
                                    .child(bouton("copier-mcp", "Copier").on_click(cx.listener(
                                        move |f, _: &ClickEvent, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(commande.clone()));
                                            f.annoncer("Commande copiée", cx);
                                        },
                                    ))),
                            ),
                    ),
                ],
            )
            .into_any_element(),
            {
                let page = self.etat.reglages.page_web;
                carte(
                    Some("Page web (iPhone)"),
                    vec![
                        ligne(
                            "Allumer la page web",
                            format!(
                                "Chercher et demander depuis un navigateur : 127.0.0.1:{} seulement, jeton obligatoire, jamais les moments privés",
                                self.etat.reglages.port_web
                            ),
                            interrupteur("page-web", page, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                                f.changer(Changement::PageWeb(!page), cx)
                            })),
                        ),
                        ligne_libre(
                            div().flex().flex_col().gap(px(3.0)).child(div().text_size(px(13.5)).child("Sur ce Mac")).child(
                                texte_secondaire("Ouvre la page dans le navigateur, déjà autorisée"),
                            ),
                            bouton("ouvrir-page-web", "Ouvrir la page")
                                .when(!page, |b| b.opacity(0.4))
                                .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                                    if page && f.source.ouvrir_page_web() {
                                        f.annoncer("Page ouverte dans le navigateur", cx);
                                    }
                                })),
                        ),
                    ],
                )
                .into_any_element()
            },
            carte(
                Some("API locale"),
                vec![
                    ligne(
                        "Allumer l'API HTTP",
                        format!(
                            "127.0.0.1:{} seulement, en lecture seule, jeton obligatoire",
                            self.etat.reglages.port_api
                        ),
                        interrupteur("api", api, true, cx.listener(move |f, _: &ClickEvent, _, cx| {
                            f.changer(Changement::Api(!api), cx)
                        })),
                    ),
                    ligne_libre(
                        div().flex().flex_col().gap(px(3.0)).child(div().text_size(px(13.5)).child("Jeton")).child(
                            texte_secondaire("À donner au programme : « Authorization: Bearer … »"),
                        ),
                        bouton("copier-jeton", "Copier le jeton")
                            .when(!api, |b| b.opacity(0.4))
                            .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                                if api && f.source.copier_jeton() {
                                    f.annoncer("Jeton copié", cx);
                                }
                            })),
                    ),
                ],
            )
            .into_any_element(),
        ]
    }

    fn stockage(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let e = &self.etat;
        let jours = e.reglages.archive_jours;
        let choix: Vec<gpui::AnyElement> = [(0, "Jamais"), (30, "30 jours"), (60, "60 jours"), (90, "90 jours")]
            .into_iter()
            .map(|(valeur, nom)| {
                segment(("archive", valeur as usize), nom, jours == valeur, cx.listener(move |f, _: &ClickEvent, _, cx| {
                    f.changer(Changement::Archive(valeur), cx)
                }))
                .into_any_element()
            })
            .collect();
        let a = &e.archive;
        let archive = carte(
            Some("Archive du homelab"),
            vec![
                ligne(
                    "Le serveur d'archive",
                    "Les vieilles vidéos et la voix y partent chiffrées ; l'index, le texte et une vignette par scène restent ici",
                    if a.robot { badge("Robot Teleport prêt", VERT) } else { badge("Robot Teleport absent", ORANGE) },
                ),
                ligne(
                    "Envoyer après",
                    "Chaque nuit, sur secteur et Mac inactif ; revoir ou réécouter un moment archivé le ramène",
                    div().flex().flex_row().p(px(2.0)).rounded(px(8.0)).bg(rgba(0x0000002E)).children(choix),
                ),
                ligne(
                    "Déjà parti",
                    match &a.derniere {
                        Some(quand) => format!("Dernière nuit : {quand}"),
                        None => "Rien encore : la première nuit viendra quand des vidéos auront passé l'âge".to_string(),
                    },
                    valeur(&format!("{} fichier(s) · {}", a.fichiers, a.taille)),
                ),
            ],
        )
        .into_any_element();
        vec![
            carte(
                Some("Sur le disque"),
                vec![
                    ligne("Vidéo", "Images de l'écran (HEVC)", valeur(&e.video)),
                    ligne("Voix", "Passages où quelqu'un parle (AAC)", valeur(&e.voix)),
                    ligne("Base", "Texte, index, recherche par le sens", valeur(&e.base)),
                    ligne(
                        "Libre sur le Mac",
                        "B-Screen se met en pause sous 5 Go libres, et repart au-dessus de 8 Go",
                        valeur(&e.libre),
                    ),
                ],
            )
            .into_any_element(),
            carte(
                Some("B-Screen en ce moment"),
                vec![
                    ligne("Processeur", "Moyenne de la dernière minute (objectif : moins de 3 %)", valeur(&e.processeur)),
                    ligne("Mémoire", "Comme dans le Moniteur d'activité (objectif : moins de 300 Mo)", valeur(&e.memoire)),
                ],
            )
            .into_any_element(),
            archive,
        ]
    }
}

impl Render for FenetreReglages {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self.section;
        let contenu = match section {
            Section::General => self.general(cx),
            Section::Voix => self.voix(cx),
            Section::Confidentialite => self.confidentialite(cx),
            Section::Recherche => self.recherche(cx),
            Section::Programmes => self.programmes(cx),
            Section::Stockage => self.stockage(cx),
        };

        let mut cote = div()
            .w(px(LARGEUR_COTE))
            .h_full()
            .flex()
            .flex_col()
            .pt(px(BANDE_HAUTE + 4.0))
            .px(px(12.0))
            .gap(px(2.0))
            .flex_shrink_0()
            .bg(rgb(FOND_COTE))
            .border_r_1()
            .border_color(rgba(FILET))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(8.0))
                    .pb(px(16.0))
                    .children(self.logo.clone().map(|l| img(l).size(px(34.0))))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("B-Screen"))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(rgba(TERTIAIRE))
                                    .child(self.etat.version.clone()),
                            ),
                    ),
            );
        for s in Section::TOUTES {
            cote = cote.child(
                div()
                    .id(SharedString::from(format!("section-{s:?}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .when(s == section, |d| d.bg(rgba(CHOISI)))
                    .when(s != section, |d| d.hover(|st| st.bg(rgba(0xFFFFFF0F))))
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded(px(6.0))
                            .bg(rgb(s.couleur()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.0))
                            .text_color(rgb(BLANC))
                            .child(icone_section(s)),
                    )
                    .child(div().text_size(px(13.5)).child(s.titre()))
                    .on_click(cx.listener(move |f, _: &ClickEvent, _, cx| {
                        f.section = s;
                        f.saisie = None;
                        f.choix_app = None;
                        cx.notify();
                    })),
            );
        }

        // `min_w_0` : sans lui, une ligne de texte trop longue élargit la colonne au-delà de la
        // fenêtre (interrupteurs et boutons hors de vue, barre de gauche écrasée).
        let droite = div()
            .id("contenu")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .pt(px(BANDE_HAUTE))
            .px(px(28.0))
            .pb(px(28.0))
            .child(
                div()
                    .text_size(px(22.0))
                    .font_weight(FontWeight::BOLD)
                    .pb(px(16.0))
                    .child(section.titre()),
            )
            .children(contenu);

        div()
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .overflow_hidden()
            .bg(rgb(FOND))
            .text_color(rgb(BLANC))
            .text_size(px(13.0))
            .track_focus(&self.focus)
            .key_context("Reglages")
            .on_action(cx.listener(|_, _: &FermerReglages, window, _| window.remove_window()))
            .on_key_down(cx.listener(|f, e: &KeyDownEvent, window, cx| f.touche(e, window, cx)))
            .child(cote)
            .child(droite)
            // La bande du haut déplace la fenêtre (la barre de titre est transparente).
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(90.0))
                    .right_0()
                    .h(px(BANDE_HAUTE - 14.0))
                    .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, window, _| window.start_window_move()),
            )
            .children(self.message.as_ref().map(|(texte, _)| {
                div()
                    .absolute()
                    .bottom(px(18.0))
                    .left(px(LARGEUR_COTE))
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(14.0))
                            .py(px(7.0))
                            .rounded(px(10.0))
                            .bg(rgba(0x2C2C2EF2))
                            .border_1()
                            .border_color(rgba(FILET))
                            .shadow_lg()
                            .child(texte.clone()),
                    )
            }))
    }
}

// ---------- les petites pièces ----------

/// Le dessin blanc d'une section (dans sa pastille de couleur).
fn icone_section(s: Section) -> gpui::AnyElement {
    match s {
        // Un cadenas : l'anse (trois côtés arrondis en haut), puis le corps.
        Section::Confidentialite => div()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w(px(8.0))
                    .h(px(6.0))
                    .border_t_2()
                    .border_l_2()
                    .border_r_2()
                    .border_color(rgb(BLANC))
                    .rounded_t(px(4.0)),
            )
            .child(div().w(px(12.0)).h(px(8.0)).rounded(px(2.0)).bg(rgb(BLANC)))
            .into_any_element(),
        Section::Recherche => img(icone_loupe(BLANC)).size(px(13.0)).into_any_element(),
        _ => div().child(s.signe()).into_any_element(),
    }
}

/// Une carte (groupe de lignes) avec son titre au-dessus.
fn carte(titre: Option<&str>, lignes: Vec<impl IntoElement>) -> gpui::Div {
    let mut corps = div()
        .flex()
        .flex_col()
        .rounded(px(12.0))
        .bg(rgba(CARTE))
        .border_1()
        .border_color(rgba(FILET));
    let n = lignes.len();
    for (i, l) in lignes.into_iter().enumerate() {
        corps = corps.child(div().when(i + 1 < n, |d| d.border_b_1().border_color(rgba(FILET))).child(l));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .mb(px(22.0))
        .children(titre.map(|t| {
            div()
                .px(px(4.0))
                .text_size(px(12.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(SECONDAIRE))
                .child(t.to_string())
        }))
        .child(corps)
}

/// Une ligne : titre et détail à gauche, la commande à droite.
fn ligne(titre: impl Into<SharedString>, detail: impl Into<SharedString>, commande: impl IntoElement) -> gpui::Div {
    ligne_libre(
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(div().text_size(px(13.5)).child(insecables(&titre.into())))
            .child(texte_secondaire(detail)),
        commande,
    )
}

fn ligne_libre(gauche: impl IntoElement, droite: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.0))
        .px(px(16.0))
        .py(px(11.0))
        .child(div().flex_1().min_w_0().child(gauche))
        .child(div().flex_shrink_0().child(droite))
}

/// Un bloc libre dans une carte (texte, puces…).
fn bloc(contenu: impl IntoElement) -> gpui::Div {
    div().px(px(16.0)).py(px(12.0)).child(contenu)
}

fn texte_secondaire(texte: impl Into<SharedString>) -> gpui::Div {
    div().text_size(px(12.0)).text_color(rgba(SECONDAIRE)).child(insecables(&texte.into()))
}

/// Espaces insécables à la française : « ; », « : », « ? », « ! » et les guillemets ne passent
/// jamais seuls à la ligne.
fn insecables(texte: &str) -> String {
    texte
        .replace(" ;", "\u{202F};")
        .replace(" :", "\u{A0}:")
        .replace(" ?", "\u{202F}?")
        .replace(" !", "\u{202F}!")
        .replace("« ", "«\u{A0}")
        .replace(" »", "\u{A0}»")
}

/// Un interrupteur façon macOS (bleu quand il est allumé).
fn interrupteur(
    id: impl Into<gpui::ElementId>,
    allume: bool,
    actif: bool,
    quand_clic: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(40.0))
        .h(px(24.0))
        .p(px(2.0))
        .rounded_full()
        .flex()
        .flex_row()
        .when(allume, |d| d.justify_end())
        .bg(if allume { rgb(BLEU) } else { rgba(0xFFFFFF29) })
        .when(!actif, |d| d.opacity(0.4))
        .when(actif, |d| d.cursor_pointer().on_click(quand_clic))
        .child(div().size(px(20.0)).rounded_full().bg(rgb(BLANC)).shadow_md())
}

/// Un morceau de choix segmenté (« 30 jours | 90 jours | … »).
fn segment(
    id: impl Into<gpui::ElementId>,
    nom: &'static str,
    choisi: bool,
    quand_clic: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .text_size(px(12.5))
        .when(choisi, |d| d.bg(rgba(0xFFFFFF3D)).font_weight(FontWeight::SEMIBOLD))
        .when(!choisi, |d| d.text_color(rgba(SECONDAIRE)).hover(|s| s.bg(rgba(0xFFFFFF14))))
        .cursor_pointer()
        .on_click(quand_clic)
        .child(nom)
}

fn bouton(id: impl Into<gpui::ElementId>, texte: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(7.0))
        .bg(rgba(BOUTON))
        .hover(|s| s.bg(rgba(BOUTON_SURVOL)))
        .cursor_pointer()
        .text_size(px(12.5))
        .whitespace_nowrap()
        .child(texte)
}

/// Un champ de saisie : le texte tapé (avec un curseur) s'il a le clavier, sinon l'invitation.
fn champ(
    id: impl Into<gpui::ElementId>,
    saisie: Option<&str>,
    invitation: &'static str,
    largeur: f32,
) -> gpui::Stateful<gpui::Div> {
    let corps = match saisie {
        Some(t) => div()
            .flex()
            .flex_row()
            .items_center()
            .child(t.to_string())
            .child(div().w(px(1.5)).h(px(15.0)).ml(px(1.0)).bg(rgb(BLEU))),
        None => div().text_color(rgba(TERTIAIRE)).child(invitation),
    };
    div()
        .id(id)
        .min_w(px(largeur))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(7.0))
        .bg(rgba(0x00000040))
        .border_1()
        .border_color(if saisie.is_some() { rgb(BLEU) } else { rgba(FILET) })
        .cursor_text()
        .text_size(px(12.5))
        .child(corps)
}

fn puce(texte: String) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .pl(px(10.0))
        .pr(px(4.0))
        .py(px(4.0))
        .rounded_full()
        .bg(rgba(0x0A84FF40))
        .border_1()
        .border_color(rgba(0x0A84FF66))
        .text_size(px(12.5))
        .child(texte)
}

fn puce_discrete(texte: String) -> gpui::Div {
    div()
        .px(px(9.0))
        .py(px(3.0))
        .rounded_full()
        .bg(rgba(0xFFFFFF0F))
        .text_size(px(12.0))
        .text_color(rgba(SECONDAIRE))
        .child(texte)
}

fn badge(texte: &'static str, couleur: u32) -> gpui::Div {
    div()
        .px(px(9.0))
        .py(px(3.0))
        .rounded_full()
        .bg(rgba((couleur << 8) | 0x33))
        .text_color(rgb(couleur))
        .text_size(px(12.0))
        .font_weight(FontWeight::SEMIBOLD)
        .child(texte)
}

fn touche_clavier_texte(texte: String) -> gpui::Div {
    touche_clavier_avec(SharedString::from(texte))
}

fn touche_clavier_avec(texte: SharedString) -> gpui::Div {
    div()
        .px(px(9.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .bg(rgba(0xFFFFFF1F))
        .border_1()
        .border_color(rgba(0xFFFFFF29))
        .text_size(px(12.5))
        .child(texte)
}

fn valeur(texte: &str) -> gpui::Div {
    div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).child(texte.to_string())
}

fn nom_d_app(icone: Option<Arc<RenderImage>>, nom: &str) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(match icone {
            Some(i) => img(i).size(px(22.0)).into_any_element(),
            None => div().size(px(22.0)).rounded(px(5.0)).bg(rgba(0xFFFFFF1F)).into_any_element(),
        })
        .child(div().text_size(px(13.5)).child(nom.to_string()))
}

/// « 12 345 » (espace fine entre les milliers, à la française).
fn nombre(n: u64) -> String {
    let chiffres = n.to_string();
    let mut sortie = String::new();
    for (i, c) in chiffres.chars().enumerate() {
        if i > 0 && (chiffres.len() - i).is_multiple_of(3) {
            sortie.push('\u{202F}');
        }
        sortie.push(c);
    }
    sortie
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_a_la_francaise() {
        assert_eq!(nombre(7), "7");
        assert_eq!(nombre(3298), "3\u{202F}298");
        assert_eq!(nombre(1_234_567), "1\u{202F}234\u{202F}567");
    }
}
