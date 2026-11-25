//! Ask (phase 8) : le panneau façon Spotlight qui répond aux questions sur ce qui a été vu et dit.
//!
//! Design « A · Verre — Ask » du canevas : panneau clair de 760 px, la question en haut, la réponse
//! avec ses renvois cliquables ([écran 20/11 21:56] ouvre la frise à ce moment), les sources en
//! cartes, et — choix de Julien — **tout ce qui part du Mac, affiché**.
//!
//! Comme la frise, il ne parle pas à macOS : la question passe par [`SourceAsk`].

use std::sync::Arc;

use bscreen_core::image::ImageBgra;
use bscreen_core::reponse::{self, Ligne, Renvoi};
use chrono::{Datelike, Local, TimeZone};
use futures::StreamExt;
use futures::channel::mpsc as canal;
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, Context, FocusHandle, FontWeight, Global, HighlightStyle, InteractiveText,
    KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, RenderImage, StyledText, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, actions, div, img, point, prelude::*, px, rgb,
    rgba, size,
};

use crate::frise::{self, SourceFrise, en_rendu};

actions!(ask, [FermerAsk, OuvrirDansLaFrise, NouvelleQuestion]);

/// Une lecture de la mémoire, gardée : (outil, quoi, ce qui est parti du Mac).
type LectureGardee = (String, String, Option<String>);

/// Le dernier échange, gardé quand le panneau se ferme (30 minutes) : question, réponse, lectures.
struct DernierEchange(Option<(std::time::Instant, String, String, Vec<LectureGardee>)>);
impl Global for DernierEchange {}

/// Ce qui arrive pendant qu'une question est traitée.
#[derive(Debug, Clone, PartialEq)]
pub enum Evenement {
    /// Claude lit la mémoire : quel outil, et quoi (« activité de 17:00 à 22:53 »).
    Lecture { outil: String, quoi: String },
    /// Ce que la mémoire lui a rendu : c'est ce qui part du Mac.
    Envoye { outil: String, contenu: String },
    /// Un morceau de la réponse, à mettre au bout (elle s'écrit au fil de l'eau).
    Morceau(String),
    Fini { erreur: Option<String>, secondes: f64 },
}

/// Un moment cité par la réponse, pour sa carte de source.
#[derive(Clone)]
pub struct MomentCite {
    pub quand_ms: i64,
    pub audio: bool,
    /// « Safari », « Toi », « Camille »…
    pub qui: String,
    /// Titre de la fenêtre, ou la phrase dite.
    pub titre: String,
    pub vignette: Option<ImageBgra>,
}

/// De quoi arrêter une question en cours (nouvelle question, panneau fermé).
pub struct Arret(pub Box<dyn FnOnce() + Send>);

pub trait SourceAsk: Send + Sync {
    /// Le modèle choisi : « haiku », « sonnet » ou « opus ».
    fn modele(&self) -> String;
    fn changer_modele(&self, modele: &str);
    /// Le panneau s'ouvre : de quoi répondre vite (le modèle de sens et Claude se chargent pendant
    /// qu'on tape).
    fn preparer(&self) {}
    /// Le panneau se ferme, ou la conversation repart de zéro (⌘N).
    fn oublier(&self) {}
    /// Mode d'essai : photographie la fenêtre (`vue` : la vue native) sous ce nom.
    fn photographier(&self, _vue: *mut std::ffi::c_void, _nom: &str) {}
    /// Pose la question (avec l'échange précédent, pour une question de suite ou une correction) ;
    /// les événements arrivent par `envoi`, depuis un autre fil.
    fn demander(&self, question: &str, precedent: Option<(&str, &str)>, envoi: Box<dyn Fn(Evenement) + Send>)
    -> Option<Arret>;
    /// Mode vocal : commencer à enregistrer la question ; faux si le micro ne répond pas.
    fn commencer_dictee(&self) -> bool {
        false
    }
    /// Arrêter et transcrire (sur le Mac ; bloque : appelé hors du fil principal).
    fn finir_dictee(&self) -> Option<String> {
        None
    }
    /// Lire la réponse à voix haute, ou s'arrêter.
    fn lire(&self, _texte: &str) {}
    fn arreter_lecture(&self) {}
    /// Le moment d'un renvoi (jamais un moment privé).
    fn moment(&self, quand_ms: i64, audio: bool) -> Option<MomentCite>;
}

struct AskOuvert(Option<AnyWindowHandle>);
impl Global for AskOuvert {}

pub fn lier_touches(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", FermerAsk, Some("Ask")),
        KeyBinding::new("cmd-w", FermerAsk, Some("Ask")),
        KeyBinding::new("cmd-enter", OuvrirDansLaFrise, Some("Ask")),
        KeyBinding::new("cmd-n", NouvelleQuestion, Some("Ask")),
    ]);
}

const LARGEUR_PANNEAU: f32 = 760.0;
/// Marge autour du panneau, pour son ombre (la fenêtre est transparente).
const MARGE: f32 = 28.0;
const HAUTEUR_VIDE: f32 = 268.0;
const HAUTEUR_REPONSE: f32 = 640.0;

const TEXTE: u32 = 0x1D1D1F;
const SECONDAIRE: u32 = 0x5E5E64;
const DISCRET: u32 = 0x6C6C72;
const BLEU: u32 = 0x0A84FF;
const BLEU_LIEN: u32 = 0x0A63C9;
const ORANGE: u32 = 0xFF9F0A;
const VERT: u32 = 0x30D158;
const ROUGE: u32 = 0xD70015;
const FILET: u32 = 0x3C3C4324;

/// Mode d'essai : ouvre Ask, pose `question`, photographie le panneau pendant et après la réponse.
pub fn essai_photos(cx: &mut App, source: Arc<dyn SourceAsk>, frise: Arc<dyn SourceFrise>, question: String) {
    basculer(cx, Arc::clone(&source), frise);
    let Some(fenetre) = cx.try_global::<AskOuvert>().and_then(|a| a.0) else {
        return;
    };
    cx.spawn(async move |cx| {
        // Comme quelqu'un qui tape pendant 3 s.
        cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
        let _ = fenetre.update(cx, |vue, window, cx| {
            if let Ok(vue) = vue.downcast::<Ask>() {
                vue.update(cx, |ask, cx| {
                    ask.question = question.clone();
                    ask.demander(window, cx);
                });
            }
        });
        // La dernière photo, 25 s après la question : le modèle local a fini lui aussi.
        for (attente, nom) in [(1500u64, "ask-1-en-cours"), (4500, "ask-2-reponse"), (9000, "ask-3-fini"), (10_000, "ask-4-fin")] {
            cx.background_executor().timer(std::time::Duration::from_millis(attente)).await;
            let source = Arc::clone(&source);
            let _ = fenetre.update(cx, |_, window, _| {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(poignee) = window.window_handle()
                    && let RawWindowHandle::AppKit(appkit) = poignee.as_raw()
                {
                    source.photographier(appkit.ns_view.as_ptr(), nom);
                }
            });
        }
    })
    .detach();
}

/// Ouvre Ask (ou le ramène devant, ou le ferme s'il est déjà devant : même raccourci).
pub fn basculer(cx: &mut App, source: Arc<dyn SourceAsk>, frise: Arc<dyn SourceFrise>) {
    if let Some(fenetre) = cx.try_global::<AskOuvert>().and_then(|a| a.0)
        && fenetre.update(cx, |_, window, _| window.remove_window()).is_ok()
    {
        cx.set_global(AskOuvert(None));
        return;
    }
    let debut = std::time::Instant::now();
    let (x, y) = match cx.primary_display() {
        Some(ecran) => {
            let b = ecran.bounds();
            (
                b.origin.x + (b.size.width - px(LARGEUR_PANNEAU + 2.0 * MARGE)) / 2.0,
                b.origin.y + b.size.height * 0.18,
            )
        }
        None => (px(300.0), px(160.0)),
    };
    let limites = Bounds {
        origin: point(x, y),
        size: size(px(LARGEUR_PANNEAU + 2.0 * MARGE), px(HAUTEUR_VIDE)),
    };
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
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| cx.new(|cx| Ask::new(window, cx, source, frise, debut))) {
        Ok(fenetre) => {
            cx.set_global(AskOuvert(Some(fenetre.into())));
            cx.activate(true);
            tracing::info!(ms = debut.elapsed().as_millis(), "Ask : fenêtre ouverte");
        }
        Err(e) => eprintln!("Ask impossible : {e}"),
    }
}

/// Une lecture de la mémoire faite pour la réponse : ce qui a été demandé, et ce qui est parti.
struct Lecture {
    outil: String,
    quoi: String,
    envoye: Option<String>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Etat {
    Vide,
    EnCours,
    Fini,
}

pub struct Ask {
    source: Arc<dyn SourceAsk>,
    frise: Arc<dyn SourceFrise>,
    focus: FocusHandle,
    question: String,
    /// La question posée (le champ peut changer ensuite).
    posee: String,
    etat: Etat,
    reponse: String,
    lectures: Vec<Lecture>,
    erreur: Option<String>,
    secondes: Option<f64>,
    /// Ce qui est parti du Mac : déplié ?
    envoi_deplie: bool,
    deplie: Option<usize>,
    /// Les moments cités (dans l'ordre de la réponse) et leurs vignettes.
    sources: Vec<(MomentCite, Option<Arc<RenderImage>>)>,
    arret: Option<Arret>,
    numero: u64,
    /// L'échange précédent, envoyé avec la question suivante (question de suite, correction).
    precedent: Option<(String, String)>,
    /// Le menu des modèles est ouvert.
    choix_modele: bool,
    /// Le micro : 0 rien, 1 enregistre la question, 2 la transcrit.
    micro: u8,
    /// La question a été dite : la réponse sera lue à voix haute.
    vocale: bool,
    _activation: Option<Subscription>,
    /// Quand l'ouverture a commencé (jusqu'au premier affichage, pour la mesurer).
    ouverte_a: Option<std::time::Instant>,
}

impl Drop for Ask {
    fn drop(&mut self) {
        self.source.arreter_lecture();
        if let Some(Arret(arreter)) = self.arret.take() {
            arreter();
        }
        self.source.oublier();
    }
}

/// Des questions pour commencer (cliquables).
const EXEMPLES: [&str; 3] = [
    "Qu'est-ce que je regardais hier soir ?",
    "De quoi a-t-on parlé aujourd'hui ?",
    "Quelles pages ai-je lues sur le chiffrement cette semaine ?",
];

impl Ask {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        source: Arc<dyn SourceAsk>,
        frise: Arc<dyn SourceFrise>,
        ouverte_a: std::time::Instant,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        // Comme Spotlight : un clic ailleurs le ferme.
        let activation = cx.observe_window_activation(window, |_, window, _| {
            if !window.is_window_active() {
                window.remove_window();
            }
        });
        let mut ask = Self {
            source,
            frise,
            focus,
            question: String::new(),
            posee: String::new(),
            etat: Etat::Vide,
            reponse: String::new(),
            lectures: Vec::new(),
            erreur: None,
            secondes: None,
            envoi_deplie: false,
            deplie: None,
            sources: Vec::new(),
            arret: None,
            numero: 0,
            precedent: None,
            choix_modele: false,
            micro: 0,
            vocale: false,
            _activation: Some(activation),
            ouverte_a: Some(ouverte_a),
        };
        ask.source.preparer();
        // Rouvert peu après : le dernier échange est là, et la question suivante en est la suite.
        if let Some((quand, question, reponse, lectures)) = cx.try_global::<DernierEchange>().and_then(|d| d.0.clone())
            && quand.elapsed() < std::time::Duration::from_secs(30 * 60)
        {
            ask.posee = question;
            ask.reponse = reponse;
            ask.lectures = lectures
                .into_iter()
                .map(|(outil, quoi, envoye)| Lecture { outil, quoi, envoye })
                .collect();
            ask.etat = Etat::Fini;
            ask.chercher_sources(cx);
            window.resize(size(px(LARGEUR_PANNEAU + 2.0 * MARGE), px(HAUTEUR_REPONSE)));
        }
        ask
    }

    /// Repartir de zéro (⌘N) : la question suivante n'est plus une suite.
    fn nouvelle_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Arret(arreter)) = self.arret.take() {
            arreter();
        }
        self.numero += 1;
        self.etat = Etat::Vide;
        self.question.clear();
        self.posee.clear();
        self.reponse.clear();
        self.lectures.clear();
        self.sources.clear();
        self.erreur = None;
        self.secondes = None;
        self.precedent = None;
        // Nouvelle conversation : un Claude neuf, lancé tout de suite.
        self.source.oublier();
        self.source.preparer();
        cx.set_global(DernierEchange(None));
        window.resize(size(px(LARGEUR_PANNEAU + 2.0 * MARGE), px(HAUTEUR_VIDE)));
        cx.notify();
    }

    /// Le micro : un clic enregistre la question, le suivant la transcrit et la pose ; la réponse
    /// sera lue à voix haute.
    fn basculer_micro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.micro {
            0 => {
                self.source.arreter_lecture();
                if self.source.commencer_dictee() {
                    self.micro = 1;
                } else {
                    self.erreur = Some("Le micro ne répond pas".to_string());
                }
            }
            1 => {
                self.micro = 2;
                let source = Arc::clone(&self.source);
                cx.spawn_in(window, async move |vue, cx| {
                    let texte = cx.background_executor().spawn(async move { source.finir_dictee() }).await;
                    let _ = vue.update_in(cx, |ask, window, cx| {
                        ask.micro = 0;
                        match texte.filter(|t| !t.is_empty()) {
                            Some(t) => {
                                ask.question = t;
                                ask.vocale = true;
                                ask.demander(window, cx);
                            }
                            None => ask.erreur = Some("Rien compris : réessaie".to_string()),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            _ => {}
        }
        cx.notify();
    }

    fn demander(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let question = self.question.trim().to_string();
        if question.is_empty() {
            return;
        }
        if let Some(Arret(arreter)) = self.arret.take() {
            arreter();
        }
        self.numero += 1;
        let numero = self.numero;
        // La réponse affichée devient le contexte de la question suivante.
        self.precedent = (self.etat == Etat::Fini && self.erreur.is_none() && !self.reponse.trim().is_empty())
            .then(|| (self.posee.clone(), self.reponse.clone()));
        self.posee = question.clone();
        self.question.clear();
        self.etat = Etat::EnCours;
        self.reponse.clear();
        self.lectures.clear();
        self.sources.clear();
        self.erreur = None;
        self.secondes = None;
        self.envoi_deplie = false;
        self.deplie = None;
        window.resize(size(px(LARGEUR_PANNEAU + 2.0 * MARGE), px(HAUTEUR_REPONSE)));

        let (envoi, mut reception) = canal::unbounded::<Evenement>();
        let precedent = self.precedent.as_ref().map(|(q, r)| (q.as_str(), r.as_str()));
        self.arret = self.source.demander(
            &question,
            precedent,
            Box::new(move |e| {
                let _ = envoi.unbounded_send(e);
            }),
        );
        if self.arret.is_none() {
            self.etat = Etat::Fini;
            self.erreur = Some("Claude Code introuvable sur ce Mac".to_string());
        }
        cx.spawn(async move |vue, cx| {
            while let Some(evenement) = reception.next().await {
                let suite = vue.update(cx, |ask, cx| {
                    if ask.numero == numero {
                        ask.recevoir(evenement, cx);
                    }
                });
                if suite.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn recevoir(&mut self, evenement: Evenement, cx: &mut Context<Self>) {
        match evenement {
            Evenement::Lecture { outil, quoi } => self.lectures.push(Lecture {
                outil,
                quoi,
                envoye: None,
            }),
            Evenement::Envoye { outil, contenu } => {
                if let Some(l) = self
                    .lectures
                    .iter_mut()
                    .rev()
                    .find(|l| l.outil == outil && l.envoye.is_none())
                {
                    l.envoye = Some(contenu);
                } else {
                    self.lectures.push(Lecture {
                        outil,
                        quoi: String::new(),
                        envoye: Some(contenu),
                    });
                }
            }
            Evenement::Morceau(texte) => self.reponse.push_str(&texte),
            Evenement::Fini { erreur, secondes } => {
                self.etat = Etat::Fini;
                self.erreur = erreur;
                self.secondes = Some(secondes);
                self.arret = None;
                self.chercher_sources(cx);
                if self.vocale && self.erreur.is_none() {
                    self.source.lire(&self.reponse);
                }
                self.vocale = false;
                if self.erreur.is_none() {
                    let lectures = self
                        .lectures
                        .iter()
                        .map(|l| (l.outil.clone(), l.quoi.clone(), l.envoye.clone()))
                        .collect();
                    cx.set_global(DernierEchange(Some((
                        std::time::Instant::now(),
                        self.posee.clone(),
                        self.reponse.clone(),
                        lectures,
                    ))));
                }
            }
        }
        cx.notify();
    }

    /// Les moments cités, dans l'ordre de la réponse (6 au plus, sans doublon). Lus en
    /// arrière-plan (la base et une vignette chacun) : rouvert, le panneau s'affiche tout de suite,
    /// les cartes arrivent juste après.
    fn chercher_sources(&mut self, cx: &mut Context<Self>) {
        let mut vus: Vec<(i64, bool)> = Vec::new();
        for ligne in reponse::analyser(&self.reponse) {
            for (_, renvoi) in ligne.renvois {
                let Some(quand) = quand_du_renvoi(&renvoi) else { continue };
                if vus.iter().any(|(v, _)| (v - quand).abs() < 60_000) || vus.len() >= 6 {
                    continue;
                }
                vus.push((quand, renvoi.audio));
            }
        }
        self.sources.clear();
        if vus.is_empty() {
            return;
        }
        let (envoi, mut reception) = canal::unbounded::<Option<MomentCite>>();
        let source = Arc::clone(&self.source);
        let _ = std::thread::Builder::new()
            .name("bscreen-ask-sources".into())
            .spawn(move || {
                for (quand, audio) in vus {
                    if envoi.unbounded_send(source.moment(quand, audio)).is_err() {
                        return;
                    }
                }
            });
        let numero = self.numero;
        cx.spawn(async move |vue, cx| {
            while let Some(moment) = reception.next().await {
                let suite = vue.update(cx, |ask, cx| {
                    if ask.numero == numero
                        && let Some(moment) = moment
                    {
                        let vignette = moment.vignette.clone().and_then(en_rendu);
                        ask.sources.push((moment, vignette));
                        cx.notify();
                    }
                });
                if suite.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn premier_renvoi(&self) -> Option<i64> {
        reponse::analyser(&self.reponse)
            .into_iter()
            .flat_map(|l| l.renvois)
            .find_map(|(_, r)| quand_du_renvoi(&r))
    }

    fn ouvrir_dans_la_frise(&mut self, quand: i64, window: &mut Window, cx: &mut Context<Self>) {
        window.remove_window();
        frise::ouvrir_au_moment(cx, Arc::clone(&self.frise), quand);
    }

    fn touche(&mut self, evenement: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let touche = &evenement.keystroke;
        if touche.modifiers.platform {
            if touche.key == "v"
                && let Some(colle) = cx.read_from_clipboard().and_then(|c| c.text())
            {
                let colle: String = colle.chars().filter(|c| !c.is_control()).collect();
                self.question.push_str(&colle);
                cx.notify();
            }
            return;
        }
        match touche.key.as_str() {
            "enter" => self.demander(window, cx),
            "backspace" => {
                if touche.modifiers.alt {
                    // ⌥⌫ : le dernier mot.
                    let sans_fin = self.question.trim_end();
                    let coupe = sans_fin.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    self.question.truncate(coupe);
                } else {
                    self.question.pop();
                }
            }
            _ => {
                if !touche.modifiers.control
                    && let Some(caractere) = touche.key_char.as_ref()
                    && !caractere.chars().any(char::is_control)
                    && self.question.chars().count() < 400
                {
                    self.question.push_str(caractere);
                } else {
                    return;
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ---------- l'affichage ----------

    fn en_tete(&self, cx: &mut Context<Self>) -> gpui::Div {
        let modele = self.source.modele();
        let local = modele == "local";
        let champ = if self.question.is_empty() {
            div()
                .flex_1()
                .text_color(rgba(0x3C3C4399))
                .child("Demander à B-Screen…")
        } else {
            div()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .overflow_hidden()
                .child(div().truncate().child(self.question.clone()))
                .child(div().w(px(2.0)).h(px(24.0)).ml(px(1.0)).bg(rgb(BLEU)))
        };
        div()
            .h(px(64.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .pl(px(22.0))
            .pr(px(16.0))
            .border_b_1()
            .border_color(rgba(FILET))
            .child(div().text_size(px(22.0)).text_color(rgb(BLEU)).child("✦"))
            .child(champ.text_size(px(20.0)))
            .child(
                div()
                    .id("micro")
                    .size(px(32.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(15.0))
                    .bg(match self.micro {
                        1 => rgba(0xFF3B30FF),
                        _ => rgba(0x78788029),
                    })
                    .text_color(if self.micro == 1 { rgb(0xFFFFFF) } else { rgb(DISCRET) })
                    .child(match self.micro {
                        1 => "■",
                        2 => "…",
                        _ => "🎙",
                    })
                    .on_click(cx.listener(|ask, _: &ClickEvent, window, cx| ask.basculer_micro(window, cx))),
            )
            .child(
                div()
                    .id("modele")
                    .h(px(32.0))
                    .px(px(12.0))
                    .rounded(px(16.0))
                    .bg(rgba(0x78788029))
                    .hover(|s| s.bg(rgba(0x78788040)))
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(13.0))
                    // Orange : la question et le contexte partent du Mac ; vert : rien ne sort.
                    .child(div().size(px(7.0)).rounded_full().bg(rgb(if local { VERT } else { ORANGE })))
                    .child(if local {
                        format!("{} ▾", nom_modele(&modele).0)
                    } else {
                        format!("Claude · {} ▾", nom_modele(&modele).0)
                    })
                    .on_click(cx.listener(|ask, _: &ClickEvent, _, cx| {
                        ask.choix_modele = !ask.choix_modele;
                        cx.notify();
                    })),
            )
    }

    fn vide(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut corps = div().flex().flex_col().gap(px(4.0)).px(px(14.0)).py(px(12.0)).child(
            div()
                .px(px(10.0))
                .pb(px(4.0))
                .text_size(px(12.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(DISCRET))
                .child("ESSAIE"),
        );
        for (i, exemple) in EXEMPLES.iter().enumerate() {
            corps = corps.child(
                div()
                    .id(("exemple", i))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(9.0))
                    .text_size(px(14.5))
                    .text_color(rgb(SECONDAIRE))
                    .hover(|s| s.bg(rgba(0x7878801F)).text_color(rgb(TEXTE)))
                    .cursor_pointer()
                    .child(*exemple)
                    .on_click(cx.listener(move |ask, _: &ClickEvent, window, cx| {
                        ask.question = exemple.to_string();
                        ask.demander(window, cx);
                    })),
            );
        }
        corps
    }

    fn lectures(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let mut bloc = div().flex().flex_col().gap(px(6.0));
        // Tant que la réponse n'a pas commencé : ce qui se passe (contexte, lectures de la mémoire).
        if self.etat == Etat::EnCours && self.reponse.trim().is_empty() {
            let derniere = self
                .lectures
                .last()
                .map(|l| format!("{} {}…", nom_outil(&l.outil), l.quoi))
                .unwrap_or_else(|| "Claude lit ta question…".to_string());
            bloc = bloc.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(14.0))
                    .text_color(rgb(SECONDAIRE))
                    .child(div().size(px(8.0)).rounded_full().bg(rgb(BLEU)))
                    .child(derniere),
            );
        }
        let envoyes: Vec<(usize, &Lecture)> = self.lectures.iter().enumerate().filter(|(_, l)| l.envoye.is_some()).collect();
        if envoyes.is_empty() {
            // Le modèle local : rien n'est parti, on le dit.
            if self.etat == Etat::Fini && self.lectures.iter().any(|l| l.outil == "local") {
                bloc = bloc.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(DISCRET))
                        .child("Rien n'est parti du Mac : réponse du modèle local"),
                );
            }
            return bloc;
        }
        let caracteres: usize = envoyes.iter().map(|(_, l)| l.envoye.as_ref().map_or(0, |e| e.chars().count())).sum();
        let deplie = self.envoi_deplie;
        bloc = bloc.child(
            div()
                .id("envoye")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.5))
                .text_color(rgb(DISCRET))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(TEXTE)))
                .child(if deplie { "▾" } else { "▸" })
                .child(format!(
                    "Parti du Mac vers Claude : ta question et {} lecture{} de ta mémoire ({} caractères)",
                    envoyes.len(),
                    if envoyes.len() > 1 { "s" } else { "" },
                    nombre(caracteres)
                ))
                .on_click(cx.listener(|ask, _: &ClickEvent, _, cx| {
                    ask.envoi_deplie = !ask.envoi_deplie;
                    cx.notify();
                })),
        );
        if deplie {
            for (i, l) in envoyes {
                let ouvert = self.deplie == Some(i);
                let contenu = l.envoye.clone().unwrap_or_default();
                let mut ligne = div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .ml(px(18.0))
                    .child(
                        div()
                            .id(("lecture", i))
                            .text_size(px(12.5))
                            .text_color(rgb(SECONDAIRE))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgb(TEXTE)))
                            .child(format!(
                                "{} {}  {} · {} caractères",
                                if ouvert { "▾" } else { "▸" },
                                nom_outil(&l.outil),
                                l.quoi,
                                nombre(contenu.chars().count())
                            ))
                            .on_click(cx.listener(move |ask, _: &ClickEvent, _, cx| {
                                ask.deplie = if ask.deplie == Some(i) { None } else { Some(i) };
                                cx.notify();
                            })),
                    );
                if ouvert {
                    ligne = ligne.child(
                        div()
                            .id(("contenu", i))
                            .max_h(px(180.0))
                            .overflow_y_scroll()
                            .p(px(10.0))
                            .rounded(px(8.0))
                            .bg(rgba(0x7878801A))
                            .font_family("Menlo")
                            .text_size(px(11.0))
                            .text_color(rgb(SECONDAIRE))
                            .child(contenu),
                    );
                }
                bloc = bloc.child(ligne);
            }
        }
        bloc
    }

    fn reponse(&self) -> gpui::Div {
        let mut bloc = div().flex().flex_col().gap(px(8.0)).text_size(px(15.0)).line_height(px(23.0));
        let frise = Arc::clone(&self.frise);
        for (n, ligne) in reponse::analyser(&self.reponse).into_iter().enumerate() {
            if ligne.texte.trim().is_empty() {
                continue;
            }
            let texte = texte_de_ligne(n, &ligne, Arc::clone(&frise));
            bloc = bloc.child(if ligne.puce {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.0))
                    .child(div().flex_shrink_0().text_color(rgb(BLEU)).font_weight(FontWeight::BOLD).child("•"))
                    // `min_w_0` : sans lui, une longue ligne ne passe pas à la ligne et sort du panneau.
                    .child(div().flex_1().min_w_0().child(texte))
            } else {
                div().w_full().min_w_0().child(texte)
            });
        }
        if let Some(erreur) = &self.erreur {
            bloc = bloc.child(div().text_color(rgb(ROUGE)).text_size(px(13.5)).child(erreur.clone()));
        }
        bloc
    }

    fn cartes(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut grille = div().flex().flex_row().flex_wrap().gap(px(12.0));
        for (i, (moment, vignette)) in self.sources.iter().enumerate() {
            let quand = moment.quand_ms;
            let apercu: gpui::AnyElement = match (vignette, moment.audio) {
                (Some(image), false) => img(image.clone()).w_full().h(px(92.0)).rounded(px(9.0)).into_any_element(),
                (_, true) => onde().into_any_element(),
                (None, false) => div().w_full().h(px(92.0)).rounded(px(9.0)).bg(rgb(0xEEF4FF)).into_any_element(),
            };
            grille = grille.child(
                div()
                    .id(("source", i))
                    .w(px(222.0))
                    .p(px(10.0))
                    .rounded(px(14.0))
                    .bg(rgb(0xFFFFFF))
                    .border_1()
                    .border_color(rgba(0x3C3C431F))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .cursor_pointer()
                    .hover(|s| s.border_color(rgb(BLEU)))
                    .child(apercu)
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(moment.titre.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(SECONDAIRE))
                            .truncate()
                            .child(format!("{} · {}", heure_lisible(quand), moment.qui)),
                    )
                    .on_click(cx.listener(move |ask, _: &ClickEvent, window, cx| ask.ouvrir_dans_la_frise(quand, window, cx))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(DISCRET))
                    .child("SOURCES"),
            )
            .child(grille)
    }

    fn pied(&self) -> gpui::Div {
        let touche = |t: &'static str, quoi: &'static str| {
            let texte = format!("{t}\u{a0}\u{a0}{quoi}");
            let gras = HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                color: Some(rgb(TEXTE).into()),
                ..Default::default()
            };
            div()
                .flex_shrink_0()
                .mr(px(18.0))
                .child(StyledText::new(texte).with_highlights([(0..t.len(), gras)]))
        };
        let mut pied = div()
            .h(px(44.0))
            .flex()
            .flex_row()
            .items_center()
            .px(px(22.0))
            .border_t_1()
            .border_color(rgba(FILET))
            .text_size(px(12.0))
            .text_color(rgb(SECONDAIRE))
            .child(touche("↩", if self.etat == Etat::Fini { "Question de suite" } else { "Demander" }))
            .child(touche("⌘↩", "Ouvrir dans la frise"));
        if self.etat == Etat::Fini {
            pied = pied.child(touche("⌘N", "Nouvelle question"));
        }
        if let Some(s) = self.secondes {
            pied = pied.child(div().flex_shrink_0().child(format!("{s:.0} s")));
        }
        pied.child(div().ml_auto().child(touche("esc", "Fermer")))
    }
}

impl Render for Ask {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ouverte) = self.ouverte_a.take() {
            tracing::info!(ms = ouverte.elapsed().as_millis(), "Ask : premier affichage");
        }
        let mut corps = div()
            .id("corps")
            .flex_1()
            .overflow_y_scroll()
            .px(px(24.0))
            .pt(px(18.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(14.0));
        if self.etat == Etat::Vide {
            corps = corps.px(px(0.0)).pt(px(0.0)).child(self.vide(cx));
        } else {
            let titre = match &self.precedent {
                Some((avant, _)) if self.etat != Etat::Vide => format!("« {} » — suite de « {avant} »", self.posee),
                _ => format!("« {} »", self.posee),
            };
            corps = corps.child(div().text_size(px(13.0)).text_color(rgb(DISCRET)).child(titre));
            corps = corps.child(self.reponse());
            corps = corps.child(self.lectures(cx));
            if !self.sources.is_empty() {
                corps = corps.child(self.cartes(cx));
            }
        }
        let menu = self.choix_modele.then(|| {
            let actuel = self.source.modele();
            let mut menu = div()
                .absolute()
                .top(px(58.0))
                .right(px(12.0))
                .w(px(270.0))
                .p(px(8.0))
                .rounded(px(16.0))
                .bg(rgb(0xFAFAFC))
                .border_1()
                .border_color(rgba(0x3C3C4324))
                .shadow_lg()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .text_size(px(13.0))
                .child(
                    div()
                        .px(px(10.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(DISCRET))
                        .child("MODÈLE (CLAUDE, TON ABONNEMENT)"),
                );
            for id in ["haiku", "sonnet", "opus", "local"] {
                if id == "local" {
                    menu = menu.child(
                        div()
                            .px(px(10.0))
                            .pt(px(10.0))
                            .pb(px(6.0))
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(DISCRET))
                            .child("SUR LE MAC (RIEN NE SORT)"),
                    );
                }
                let (nom, detail) = nom_modele(id);
                let choisi = id == actuel;
                menu = menu.child(
                    div()
                        .id(id)
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(10.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .cursor_pointer()
                        .when(choisi, |d| d.bg(rgb(BLEU)).text_color(rgb(0xFFFFFF)))
                        .when(!choisi, |d| d.hover(|s| s.bg(rgba(0x7878801F))))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(nom))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(if choisi { rgb(0xE1EEFF) } else { rgb(SECONDAIRE) })
                                .child(detail),
                        )
                        .on_click(cx.listener(move |ask, _: &ClickEvent, _, cx| {
                            ask.source.changer_modele(id);
                            ask.choix_modele = false;
                            // Le Claude lancé d'avance prend le nouveau modèle.
                            ask.source.preparer();
                            cx.notify();
                        })),
                );
            }
            menu
        });
        let panneau = div()
            .relative()
            .w(px(LARGEUR_PANNEAU))
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(24.0))
            .bg(rgba(0xFAFAFCF5))
            .border_1()
            .border_color(rgba(0xFFFFFFCC))
            .shadow_lg()
            .overflow_hidden()
            .text_color(rgb(TEXTE))
            .child(self.en_tete(cx))
            .child(corps)
            .child(self.pied())
            .children(menu)
            // Un clic dans le panneau ne le ferme pas.
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation());
        div()
            .size_full()
            .p(px(MARGE))
            .pt(px(8.0))
            .track_focus(&self.focus)
            .key_context("Ask")
            .on_action(cx.listener(|_, _: &FermerAsk, window, _| window.remove_window()))
            .on_action(cx.listener(|ask, _: &NouvelleQuestion, window, cx| ask.nouvelle_question(window, cx)))
            .on_action(cx.listener(|ask, _: &OuvrirDansLaFrise, window, cx| {
                if let Some(quand) = ask.premier_renvoi() {
                    ask.ouvrir_dans_la_frise(quand, window, cx);
                }
            }))
            .on_key_down(cx.listener(|ask, e: &KeyDownEvent, window, cx| ask.touche(e, window, cx)))
            // Un clic à côté du panneau (sur la marge transparente) le ferme.
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, window, _| window.remove_window())
            .child(panneau)
    }
}

/// Le texte d'une ligne : gras, et renvois en bleu cliquables (ils ouvrent la frise).
fn texte_de_ligne(n: usize, ligne: &Ligne, frise: Arc<dyn SourceFrise>) -> InteractiveText {
    let mut styles: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    let renvois: Vec<std::ops::Range<usize>> = ligne.renvois.iter().map(|(r, _)| r.clone()).collect();
    // Le gras, sans ce qui chevauche un renvoi (les styles ne doivent pas se croiser).
    for gras in &ligne.gras {
        let mut debut = gras.start;
        for r in renvois.iter().filter(|r| r.start < gras.end && r.end > gras.start) {
            if r.start > debut {
                styles.push((
                    debut..r.start,
                    HighlightStyle {
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                ));
            }
            debut = debut.max(r.end);
        }
        if debut < gras.end {
            styles.push((
                debut..gras.end,
                HighlightStyle {
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                },
            ));
        }
    }
    for r in &renvois {
        styles.push((
            r.clone(),
            HighlightStyle {
                color: Some(rgb(BLEU_LIEN).into()),
                ..Default::default()
            },
        ));
    }
    styles.sort_by_key(|(r, _)| r.start);
    let quands: Vec<Option<i64>> = ligne.renvois.iter().map(|(_, r)| quand_du_renvoi(r)).collect();
    InteractiveText::new(("ligne", n), StyledText::new(ligne.texte.clone()).with_highlights(styles)).on_click(
        renvois,
        move |i, window, cx| {
            if let Some(Some(quand)) = quands.get(i) {
                window.remove_window();
                frise::ouvrir_au_moment(cx, Arc::clone(&frise), *quand);
            }
        },
    )
}

/// L'heure d'un renvoi (heure locale) ; sans date : aujourd'hui ; une date à venir : l'an dernier.
pub fn quand_du_renvoi(renvoi: &Renvoi) -> Option<i64> {
    let maintenant = Local::now();
    let (jour, mois) = if renvoi.jour == 0 {
        (maintenant.day(), maintenant.month())
    } else {
        (renvoi.jour, renvoi.mois)
    };
    let mut annee = maintenant.year();
    let date = |annee: i32| {
        Local
            .with_ymd_and_hms(annee, mois, jour, renvoi.heure, renvoi.minute, 0)
            .earliest()
    };
    let mut d = date(annee)?;
    if d > maintenant + chrono::Duration::days(1) {
        annee -= 1;
        d = date(annee)?;
    }
    Some(d.timestamp_millis())
}

fn heure_lisible(quand_ms: i64) -> String {
    match Local.timestamp_millis_opt(quand_ms).single() {
        Some(d) if d.date_naive() == Local::now().date_naive() => d.format("%H:%M").to_string(),
        Some(d) => d.format("%d/%m %H:%M").to_string(),
        None => String::new(),
    }
}

/// Le nom d'un modèle et ce qu'il vaut.
fn nom_modele(id: &str) -> (&'static str, &'static str) {
    match id {
        "haiku" => ("Rapide", "Haiku : 2 à 5 s, bien pour retrouver"),
        "opus" => ("Approfondi", "Opus : plus lent, pour les questions difficiles"),
        "local" => ("Sur le Mac", "Qwen, en local : rien ne sort, mais ≈ 20 s"),
        _ => ("Équilibré", "Sonnet : rapide et précis"),
    }
}

/// Le nom d'un outil de la mémoire, pour le dire (« Regarde l'activité »).
fn nom_outil(outil: &str) -> &'static str {
    match outil {
        "contexte" => "Prépare le contexte",
        "local" => "Le modèle local lit et répond",
        "chercher" => "Cherche",
        "chercher_par_le_sens" => "Cherche par le sens",
        "activite" => "Regarde l'activité",
        "voix" => "Écoute ce qui a été dit",
        "recherches" => "Regarde tes recherches",
        "ecran" => "Lit l'écran",
        _ => "Lit la mémoire",
    }
}

/// Une petite onde, pour une source entendue.
fn onde() -> gpui::Div {
    let hauteurs = [12.0, 26.0, 18.0, 34.0, 22.0, 10.0, 28.0, 36.0, 20.0, 14.0, 30.0, 16.0, 24.0, 12.0];
    div()
        .w_full()
        .h(px(92.0))
        .rounded(px(9.0))
        .bg(rgb(0xEEF4FF))
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(3.0))
        .children(
            hauteurs
                .iter()
                .enumerate()
                .map(|(i, h)| div().w(px(3.0)).h(px(*h)).rounded(px(2.0)).bg(rgb(if i % 5 < 3 { BLEU } else { ORANGE }))),
        )
}

/// « 12 345 » (espace fine entre les milliers).
fn nombre(n: usize) -> String {
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
    fn heure_d_un_renvoi() {
        let r = Renvoi {
            audio: false,
            jour: 1,
            mois: 1,
            heure: 9,
            minute: 30,
        };
        let quand = quand_du_renvoi(&r).unwrap();
        let d = Local.timestamp_millis_opt(quand).single().unwrap();
        assert_eq!((d.day(), d.month(), d.hour_minute()), (1, 1, (9, 30)));
        assert!(d <= Local::now() + chrono::Duration::days(1), "jamais dans le futur");
    }

    trait HeureMinute {
        fn hour_minute(&self) -> (u32, u32);
    }
    impl HeureMinute for chrono::DateTime<Local> {
        fn hour_minute(&self) -> (u32, u32) {
            use chrono::Timelike;
            (self.hour(), self.minute())
        }
    }
}
