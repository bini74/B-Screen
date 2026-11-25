//! Essai de la frise B-Screen avec gpui (phase 0.3).
//!
//! Les vignettes sont factices. On vérifie seulement : le fond en verre,
//! le glissé à deux doigts (avec l'inertie du trackpad), le pincement pour zoomer,
//! l'affichage d'images brutes et la mémoire.
//!
//!     cargo run --release -p bscreen-ui --example essai_frise
//!
//! Touches : ← → un moment avant / après, Échap pour quitter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, Application, Bounds, Context, FocusHandle, FontWeight, ImageSource, KeyBinding, RenderImage, ScrollWheelEvent,
    Task, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, div, img, point,
    prelude::*, px, rgb, rgba, size,
};
use image::{Frame, Rgba, RgbaImage};

actions!(essai, [Quitter, Precedent, Suivant]);

/// Une heure de moments, une image toutes les 2 secondes.
const NB_MOMENTS: usize = 1800;
const PAS_S: usize = 2;
const DEBUT_S: usize = 14 * 3600;

/// Taille des images factices, en pixels (assez pour les agrandir au centre).
const IMAGE_L: u32 = 400;
const IMAGE_H: u32 = 250;

/// Largeur d'une vignette au zoom 1, et écart entre deux vignettes.
const LARGEUR_BASE: f32 = 132.0;
const ECART: f32 = 12.0;
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 1.6;
const HAUTEUR_BANDE: f32 = 208.0;

#[derive(Clone, Copy)]
struct AppFactice {
    nom: &'static str,
    sigle: &'static str,
    fond: u32,
    badge: u32,
    encre: u32,
}

const APPS: [AppFactice; 5] = [
    AppFactice {
        nom: "Terminal",
        sigle: ">_",
        fond: 0x1E1F24,
        badge: 0x1F2328,
        encre: 0x7CF2A0,
    },
    AppFactice {
        nom: "Safari",
        sigle: "S",
        fond: 0xFFFFFF,
        badge: 0x2F7BF6,
        encre: 0xC9C9CF,
    },
    AppFactice {
        nom: "Messages",
        sigle: "M",
        fond: 0xFFFFFF,
        badge: 0x34C759,
        encre: 0x0A84FF,
    },
    AppFactice {
        nom: "Code",
        sigle: "</>",
        fond: 0x1F1F2B,
        badge: 0x5B5BD6,
        encre: 0x82AAFF,
    },
    AppFactice {
        nom: "Notes",
        sigle: "N",
        fond: 0xFFFDF5,
        badge: 0xF5B83D,
        encre: 0xD9CFAE,
    },
];

/// Quelle app est « au premier plan » au moment `i` (des tranches de longueurs variées).
fn app_de(i: usize) -> usize {
    let tranche = i / 23;
    (tranche * 7 + tranche / 3) % APPS.len()
}

fn heure(i: usize) -> String {
    let s = DEBUT_S + i * PAS_S;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn heure_courte(i: usize) -> String {
    let s = DEBUT_S + i * PAS_S;
    format!("{:02}:{:02}", s / 3600, (s / 60) % 60)
}

// ---------- images factices ----------

/// gpui attend du BGRA : on range les couleurs dans cet ordre.
fn bgra(c: u32) -> Rgba<u8> {
    Rgba([(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8, 255])
}

fn rect(image: &mut RgbaImage, x: u32, y: u32, l: u32, h: u32, c: Rgba<u8>) {
    let (il, ih) = image.dimensions();
    for yy in y.min(ih)..(y + h).min(ih) {
        for xx in x.min(il)..(x + l).min(il) {
            image.put_pixel(xx, yy, c);
        }
    }
}

fn hasard(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

fn fabriquer_image(i: usize) -> Arc<RenderImage> {
    let app = APPS[app_de(i)];
    let mut image = RgbaImage::from_pixel(IMAGE_L, IMAGE_H, bgra(app.fond));
    let mut h = hasard(i as u64 * 2_654_435_761 + 17);
    let mut alea = |min: u32, max: u32| {
        h = hasard(h);
        min + (h % u64::from(max - min)) as u32
    };
    match app.nom {
        "Safari" => {
            rect(&mut image, 0, 0, IMAGE_L, 26, bgra(0xECECEF));
            rect(&mut image, 120, 7, 160, 12, bgra(0xDADADF));
            let titre = alea(120, 230);
            rect(&mut image, 34, 46, titre, 14, bgra(0x3A3A40));
            let mut y = 80;
            while y < IMAGE_H - 16 {
                let l = alea(220, 330);
                rect(&mut image, 34, y, l, 6, bgra(app.encre));
                y += 16;
            }
        }
        "Notes" => {
            rect(&mut image, 0, 0, IMAGE_L, 30, bgra(0xF5D36A));
            let titre = alea(110, 200);
            rect(&mut image, 26, 48, titre, 14, bgra(0x5A4A1C));
            let mut y = 80;
            while y < IMAGE_H - 16 {
                let l = alea(180, 340);
                rect(&mut image, 26, y, l, 6, bgra(app.encre));
                y += 18;
            }
        }
        "Messages" => {
            let mut y = 20;
            let mut a_droite = false;
            while y < IMAGE_H - 30 {
                let l = alea(110, 230);
                if a_droite {
                    rect(&mut image, IMAGE_L - 20 - l, y, l, 26, bgra(app.encre));
                } else {
                    rect(&mut image, 20, y, l, 26, bgra(0xE5E5EA));
                }
                a_droite = !a_droite;
                y += 38;
            }
        }
        _ => {
            let couleurs = if app.nom == "Code" {
                [0xC792EA, 0x82AAFF, 0xF7C873, 0x89DDFF]
            } else {
                [0x7CF2A0, 0x5D6470, 0x5D6470, 0xC9D1D9]
            };
            let mut y = 20;
            let mut n = 0usize;
            while y < IMAGE_H - 14 {
                let retrait = if app.nom == "Code" { alea(0, 4) * 18 } else { 0 };
                let l = alea(60, 300);
                rect(&mut image, 22 + retrait, y, l, 8, bgra(couleurs[n % couleurs.len()]));
                n += 1;
                y += 18;
            }
        }
    }
    // Une petite barre qui avance : deux moments voisins ne sont jamais identiques.
    rect(&mut image, 0, IMAGE_H - 4, (i as u32 * 7) % IMAGE_L, 4, bgra(app.badge));
    Arc::new(RenderImage::new(vec![Frame::new(image)]))
}

// ---------- la vue ----------

struct Frise {
    focus: FocusHandle,
    /// Position de la tête de lecture, en moments (avec une partie décimale pendant le glissé).
    position: f32,
    zoom: f32,
    cache: HashMap<usize, Arc<RenderImage>>,
    tranches: Vec<(usize, usize, usize)>,
    images_par_s: u32,
    compteur: u32,
    depuis: Instant,
    _pincement: Option<bscreen_macos::gestes::Pincement>,
    _ecoute_pincement: Task<()>,
}

impl Frise {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);

        // Le pincement vient de la couche macOS ; on le fait passer à la vue par un canal.
        let (envoi, mut reception) = mpsc::unbounded::<f64>();
        let pincement = bscreen_macos::gestes::Pincement::ecouter(move |m| {
            let _ = envoi.unbounded_send(m);
        });
        let ecoute = cx.spawn(async move |vue, cx| {
            while let Some(m) = reception.next().await {
                let resultat = vue.update(cx, |frise, cx| {
                    frise.zoom = (frise.zoom * (1.0 + m as f32)).clamp(ZOOM_MIN, ZOOM_MAX);
                    cx.notify();
                });
                if resultat.is_err() {
                    break;
                }
            }
        });

        // Les tranches d'apps, pour la barre d'activité.
        let mut tranches = Vec::new();
        let mut debut = 0;
        for i in 1..=NB_MOMENTS {
            if i == NB_MOMENTS || app_de(i) != app_de(debut) {
                tranches.push((debut, i, app_de(debut)));
                debut = i;
            }
        }

        Self {
            focus,
            position: 940.0,
            zoom: 1.0,
            cache: HashMap::new(),
            tranches,
            images_par_s: 0,
            compteur: 0,
            depuis: Instant::now(),
            _pincement: pincement,
            _ecoute_pincement: ecoute,
        }
    }

    fn defiler(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let d = e.delta.pixel_delta(px(16.0));
        let (dx, dy) = (f32::from(d.x), f32::from(d.y));
        let principal = if dx.abs() >= dy.abs() { dx } else { dy };
        let pas = LARGEUR_BASE * self.zoom + ECART;
        self.position = (self.position - principal / pas).clamp(0.0, (NB_MOMENTS - 1) as f32);
        cx.notify();
    }

    fn sauter(&mut self, de: f32, cx: &mut Context<Self>) {
        self.position = (self.position.round() + de).clamp(0.0, (NB_MOMENTS - 1) as f32);
        cx.notify();
    }
}

fn badge(app: AppFactice, cote: f32) -> gpui::Div {
    div()
        .w(px(cote))
        .h(px(cote))
        .rounded(px(cote * 0.3))
        .bg(rgb(app.badge))
        .border_1()
        .border_color(rgb(0xFFFFFF))
        .shadow_md()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(cote * 0.42))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(if app.nom == "Terminal" { 0x7CF2A0 } else { 0xFFFFFF }))
        .child(app.sigle)
}

fn pastille(texte: String) -> gpui::Div {
    div()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(16.0))
        .bg(rgba(0x7878801F))
        .text_size(px(13.0))
        .text_color(rgb(0x1D1D1F))
        .child(texte)
}

impl Render for Frise {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Combien d'images par seconde l'interface dessine-t-elle ?
        self.compteur += 1;
        let ecoule = self.depuis.elapsed();
        if ecoule >= Duration::from_millis(500) {
            self.images_par_s = (self.compteur as f32 / ecoule.as_secs_f32()).round() as u32;
            self.compteur = 0;
            self.depuis = Instant::now();
        }

        let fenetre = window.viewport_size();
        let (largeur_fenetre, hauteur_fenetre) = (f32::from(fenetre.width), f32::from(fenetre.height));
        let marge = 28.0;
        let largeur_bande = largeur_fenetre - 2.0 * marge - 2.0 * 22.0;
        let centre = largeur_bande / 2.0;

        let largeur_vignette = LARGEUR_BASE * self.zoom;
        let hauteur_vignette = largeur_vignette * 0.625;
        let pas = largeur_vignette + ECART;
        let actuel = (self.position.round() as usize).min(NB_MOMENTS - 1);

        // Seules les vignettes visibles existent (plus une marge).
        let demi = (centre / pas).ceil() as isize + 2;
        let premier = (self.position.floor() as isize - demi).max(0) as usize;
        let dernier = ((self.position.ceil() as isize + demi).max(0) as usize).min(NB_MOMENTS - 1);

        // On rend au GPU la place des vignettes éloignées.
        let garder = premier.saturating_sub(30)..=dernier + 30;
        let eloignees: Vec<usize> = self.cache.keys().copied().filter(|k| !garder.contains(k)).collect();
        for k in eloignees {
            if let Some(image) = self.cache.remove(&k) {
                let _ = window.drop_image(image);
            }
        }

        let mut vignettes = Vec::with_capacity(dernier - premier + 1);
        for i in premier..=dernier {
            let image = self.cache.entry(i).or_insert_with(|| fabriquer_image(i)).clone();
            let est_actuel = i == actuel;
            let agrandi = if est_actuel { 1.25 } else { 1.0 };
            let (l, h) = (largeur_vignette * agrandi, hauteur_vignette * agrandi);
            let x = centre + (i as f32 - self.position) * pas - l / 2.0;
            let y = HAUTEUR_BANDE - h - 24.0;
            let app = APPS[app_de(i)];
            let cote = if est_actuel { 28.0 } else { 22.0 };
            let etiquette = if est_actuel {
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(0x0A63C9))
                    .child(app.nom)
            } else {
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(0x48484E))
                    .child(heure_courte(i))
            };
            vignettes.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(l))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(5.0))
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
                                    .shadow_md()
                                    .when(est_actuel, |d| d.border_3().border_color(rgb(0x0A84FF)))
                                    .child(img(ImageSource::Render(image)).size_full()),
                            )
                            // Le logo de l'app déborde du coin haut gauche de la vignette.
                            .child(badge(app, cote).absolute().left(px(-cote * 0.35)).top(px(-cote * 0.35))),
                    )
                    .child(etiquette),
            );
        }

        // Grande image au centre : pour l'essai, la vignette agrandie (la vraie viendra du décodeur vidéo).
        let image_actuelle = self.cache.get(&actuel).cloned();
        let hauteur_dispo = hauteur_fenetre - 64.0 - (HAUTEUR_BANDE + 110.0) - 40.0;
        let largeur_dispo = largeur_fenetre - 2.0 * 80.0;
        let (gl, gh) = if largeur_dispo / hauteur_dispo > 1.6 {
            (hauteur_dispo * 1.6, hauteur_dispo)
        } else {
            (largeur_dispo, largeur_dispo / 1.6)
        };

        // Barre d'activité : toute l'heure, colorée par app, avec la zone visible.
        let largeur_activite = largeur_bande;
        let activite = self.tranches.iter().map(|&(debut, fin, app)| {
            div()
                .h_full()
                .w(px((fin - debut) as f32 / NB_MOMENTS as f32 * largeur_activite))
                .bg(rgb(APPS[app].badge))
                .opacity(0.75)
        });
        let visible_x = premier as f32 / NB_MOMENTS as f32 * largeur_activite;
        let visible_l = ((dernier - premier) as f32 / NB_MOMENTS as f32 * largeur_activite).max(4.0);
        let tete_x = self.position / NB_MOMENTS as f32 * largeur_activite;

        div()
            .id("frise")
            .track_focus(&self.focus)
            .on_action(|_: &Quitter, _, cx| cx.quit())
            .on_action(cx.listener(|frise, _: &Precedent, _, cx| frise.sauter(-1.0, cx)))
            .on_action(cx.listener(|frise, _: &Suivant, _, cx| frise.sauter(1.0, cx)))
            .on_scroll_wheel(cx.listener(|frise, e: &ScrollWheelEvent, _, cx| frise.defiler(e, cx)))
            .size_full()
            .relative()
            .bg(rgba(0xDDE3EE55))
            .text_color(rgb(0x1D1D1F))
            .flex()
            .flex_col()
            .items_center()
            // Barre du haut
            .child(
                div()
                    .mt(px(14.0))
                    .px(px(18.0))
                    .h(px(40.0))
                    .rounded(px(20.0))
                    .bg(rgba(0xFFFFFF9E))
                    .border_1()
                    .border_color(rgba(0xFFFFFFB3))
                    .shadow_md()
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .text_size(px(13.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("B-Screen"))
                    .child(
                        div()
                            .text_color(rgb(0x3C3C43))
                            .child("essai de la frise · vignettes factices"),
                    )
                    .child(
                        div()
                            .text_color(rgb(0x3C3C43))
                            .child(format!("{} images/s", self.images_par_s)),
                    ),
            )
            // Grande image
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(image_actuelle.map(|image| {
                        let app = APPS[app_de(actuel)];
                        div()
                            .relative()
                            .w(px(gl))
                            .h(px(gh))
                            .child(
                                div()
                                    .size_full()
                                    .rounded(px(14.0))
                                    .overflow_hidden()
                                    .shadow_lg()
                                    .child(img(ImageSource::Render(image)).size_full()),
                            )
                            // Le logo de l'app déborde du coin haut gauche de la fenêtre capturée.
                            .child(badge(app, 44.0).absolute().left(px(-15.0)).top(px(-15.0)))
                    })),
            )
            // Panneau de verre
            .child(
                div()
                    .mb(px(20.0))
                    .w(px(largeur_fenetre - 2.0 * marge))
                    .rounded(px(28.0))
                    .bg(rgba(0xFFFFFF94))
                    .border_1()
                    .border_color(rgba(0xFFFFFFBF))
                    .shadow_lg()
                    .px(px(22.0))
                    .py(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Mardi 17 novembre"),
                            )
                            .child(
                                div()
                                    .px(px(12.0))
                                    .py(px(5.0))
                                    .rounded(px(14.0))
                                    .bg(rgb(0x0A84FF))
                                    .text_color(rgb(0xFFFFFF))
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(heure(actuel)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.0))
                                    .child(pastille(format!("zoom ×{:.2}", self.zoom)))
                                    .child(pastille(format!("{} vignettes en mémoire", self.cache.len()))),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(largeur_bande))
                            .h(px(HAUTEUR_BANDE))
                            .overflow_hidden()
                            .children(vignettes),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(largeur_activite))
                            .h(px(10.0))
                            .rounded(px(5.0))
                            .overflow_hidden()
                            .flex()
                            .children(activite)
                            .child(
                                div()
                                    .absolute()
                                    .left(px(visible_x))
                                    .top_0()
                                    .w(px(visible_l))
                                    .h_full()
                                    .rounded(px(4.0))
                                    .border_2()
                                    .border_color(rgb(0x0A84FF))
                                    .bg(rgba(0x0A84FF33)),
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
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(11.0))
                            .text_color(rgb(0x3C3C43))
                            .child("Glisse à deux doigts · pince pour zoomer · ← → un moment · Échap pour quitter")
                            .child("14:00 — 15:00"),
                    ),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("escape", Quitter, None),
            KeyBinding::new("cmd-q", Quitter, None),
            KeyBinding::new("left", Precedent, None),
            KeyBinding::new("right", Suivant, None),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1320.0), px(860.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("B-Screen — essai de la frise".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.0), px(14.0))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Frise::new(window, cx)),
        )
        .expect("ouverture de la fenêtre impossible");
        cx.activate(true);
    });
}
