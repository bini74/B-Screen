//! Lecture du texte des fenêtres par l'accessibilité de macOS (phase 3).
//!
//! Pour l'app au premier plan : chaque texte, sa position à l'écran, s'il est
//! visible (« vu ») ou présent mais caché / hors champ (« non vu »), et s'il est
//! dans le champ où Julien tape (« focus »). Les champs de mot de passe ne sont
//! jamais lus. Le parcours est borné en éléments et en temps.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSString, NSURL};

type Ref = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCopyAttributeValue(element: Ref, attribut: Ref, valeur: *mut Ref) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(element: Ref, attributs: Ref, options: u32, valeurs: *mut Ref) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, secondes: f32) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attribut: Ref, valeur: Ref) -> i32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetType(valeur: Ref) -> u32;
    fn AXValueGetValue(valeur: Ref, genre: u32, sortie: *mut c_void) -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(objet: Ref) -> usize;
    fn CFEqual(a: Ref, b: Ref) -> u8;
    static kCFBooleanTrue: Ref;
}

const AX_VALEUR_POINT: u32 = 1;
const AX_VALEUR_TAILLE: u32 = 2;

/// Bornes du parcours, pour rester léger même sur une page web énorme.
const NOEUDS_MAX: usize = 3_000;
const PROFONDEUR_MAX: usize = 64;
const DUREE_MAX: Duration = Duration::from_millis(200);
/// Un texte plus long est découpé en blocs (par lignes) : un fichier de code modifié
/// d'une ligne ne réécrit qu'un bloc.
const BLOC_MAX: usize = 600;

#[derive(Debug, Clone, PartialEq)]
pub struct ElementTexte {
    pub texte: String,
    pub role: String,
    /// x, y, largeur, hauteur, en points d'écran (origine en haut à gauche de l'écran principal).
    pub cadre: [f64; 4],
    pub vu: bool,
    pub focus: bool,
}

#[derive(Debug, Default)]
pub struct Lecture {
    pub fenetre: Option<String>,
    pub url: Option<String>,
    /// La fenêtre active montre un insigne de navigation privée (à croire pour un navigateur seulement).
    pub privee: bool,
    pub elements: Vec<ElementTexte>,
    pub noeuds: usize,
    pub duree: Duration,
    pub tronquee: bool,
}

/// Objet CoreFoundation retenu (+1), relâché automatiquement.
struct Cf(Retained<AnyObject>);

impl Cf {
    /// SAFETY : `brut` doit être une référence +1 (fonction « Copy » / « Create ») ou nulle.
    unsafe fn prendre(brut: Ref) -> Option<Cf> {
        unsafe { Retained::from_raw(brut as *mut AnyObject) }.map(Cf)
    }
    fn ptr(&self) -> Ref {
        Retained::as_ptr(&self.0).cast()
    }
    fn texte(&self) -> Option<String> {
        self.0.downcast_ref::<NSString>().map(|s| s.to_string())
    }
    fn tableau(&self) -> Option<&NSArray<AnyObject>> {
        self.0.downcast_ref::<NSArray>()
    }
    fn url(&self) -> Option<String> {
        self.0
            .downcast_ref::<NSURL>()
            .and_then(|u| u.absoluteString())
            .map(|s| s.to_string())
    }
    fn valeur_ax<T: Default>(&self, genre: u32) -> Option<T> {
        unsafe {
            if CFGetTypeID(self.ptr()) != AXValueGetTypeID() || AXValueGetType(self.ptr()) != genre {
                return None;
            }
            let mut sortie = T::default();
            (AXValueGetValue(self.ptr(), genre, (&mut sortie as *mut T).cast()) != 0).then_some(sortie)
        }
    }
}

fn attribut(element: Ref, nom: &str) -> Option<Cf> {
    let nom = NSString::from_str(nom);
    let mut valeur: Ref = std::ptr::null();
    let erreur = unsafe { AXUIElementCopyAttributeValue(element, Retained::as_ptr(&nom).cast(), &mut valeur) };
    if erreur != 0 {
        return None;
    }
    unsafe { Cf::prendre(valeur) }
}

/// Les attributs lus en un seul aller-retour pour chaque élément.
const ATTRIBUTS: [&str; 7] = [
    "AXRole",
    "AXValue",
    "AXTitle",
    "AXDescription",
    "AXPosition",
    "AXSize",
    "AXChildren",
];

struct Attributs {
    role: String,
    valeur: Option<String>,
    titre: Option<String>,
    description: Option<String>,
    cadre: Option<[f64; 4]>,
    enfants: Vec<Cf>,
}

fn lire_attributs(element: Ref, noms: &NSArray<NSString>) -> Option<Attributs> {
    let mut brut: Ref = std::ptr::null();
    let erreur = unsafe {
        AXUIElementCopyMultipleAttributeValues(element, (noms as *const NSArray<NSString>).cast(), 0, &mut brut)
    };
    if erreur != 0 {
        return None;
    }
    let valeurs = unsafe { Cf::prendre(brut) }?;
    let valeurs = valeurs.tableau()?;
    let obtenir = |i: usize| (i < valeurs.count()).then(|| Cf(valeurs.objectAtIndex(i as _)));
    let role = obtenir(0).and_then(|v| v.texte()).unwrap_or_default();
    let position = obtenir(4).and_then(|v| v.valeur_ax::<[f64; 2]>(AX_VALEUR_POINT));
    let taille = obtenir(5).and_then(|v| v.valeur_ax::<[f64; 2]>(AX_VALEUR_TAILLE));
    let enfants = obtenir(6)
        .and_then(|v| v.tableau().map(|t| t.iter().map(Cf).collect::<Vec<_>>()))
        .unwrap_or_default();
    Some(Attributs {
        role,
        valeur: obtenir(1).and_then(|v| v.texte()),
        titre: obtenir(2).and_then(|v| v.texte()),
        description: obtenir(3).and_then(|v| v.texte()),
        cadre: position.zip(taille).map(|(p, t)| [p[0], p[1], t[0], t[1]]),
        enfants,
    })
}

/// Le texte qu'on garde d'un élément, selon son rôle (on évite le bruit des boutons et menus).
fn texte_utile(a: &Attributs) -> Option<String> {
    let texte = match a.role.as_str() {
        "AXSecureTextField" => None,
        "AXStaticText" | "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField" | "AXCell" => {
            a.valeur.clone().or_else(|| a.titre.clone())
        }
        "AXHeading" | "AXLink" | "AXWindow" => a.titre.clone().or_else(|| a.description.clone()),
        "AXImage" => a.description.clone(),
        _ => a.valeur.clone(),
    }?;
    let texte = texte.trim();
    (!texte.is_empty()).then(|| texte.to_string())
}

/// Découpe un long texte en blocs de lignes.
pub fn decouper(texte: &str) -> Vec<String> {
    if texte.len() <= BLOC_MAX {
        return vec![texte.to_string()];
    }
    let mut blocs = Vec::new();
    let mut courant = String::new();
    for ligne in texte.lines() {
        if !courant.is_empty() && courant.len() + ligne.len() + 1 > BLOC_MAX {
            blocs.push(std::mem::take(&mut courant));
        }
        if !courant.is_empty() {
            courant.push('\n');
        }
        courant.push_str(ligne);
    }
    if !courant.trim().is_empty() {
        blocs.push(courant);
    }
    blocs
}

fn se_croisent(a: [f64; 4], b: [f64; 4]) -> bool {
    a[2] > 0.0 && a[3] > 0.0 && a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

/// Apps où l'accessibilité complète a déjà été demandée (Electron, Chrome…).
static ACCESSIBILITE_DEMANDEE: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Lit le texte de toutes les fenêtres de l'app `pid`. `ecran` : le rectangle de l'écran, en points.
/// Avec son bassin de libération : le fil de l'indexation dure autant que l'app.
pub fn lire_app(pid: i32, ecran: [f64; 4]) -> Lecture {
    objc2::rc::autoreleasepool(|_| lire_app_seule(pid, ecran))
}

fn lire_app_seule(pid: i32, ecran: [f64; 4]) -> Lecture {
    let debut = Instant::now();
    let mut lecture = Lecture::default();
    let Some(app) = (unsafe { Cf::prendre(AXUIElementCreateApplication(pid)) }) else {
        return lecture;
    };
    unsafe { AXUIElementSetMessagingTimeout(app.ptr(), 0.25) };

    // Electron (VSCodium…) et Chrome n'exposent leur contenu qu'à la demande.
    if let Ok(mut deja) = ACCESSIBILITE_DEMANDEE.lock()
        && !deja.contains(&pid)
    {
        let nom = NSString::from_str("AXManualAccessibility");
        unsafe { AXUIElementSetAttributeValue(app.ptr(), Retained::as_ptr(&nom).cast(), kCFBooleanTrue) };
        deja.push(pid);
    }

    let focus = attribut(app.ptr(), "AXFocusedUIElement");
    let fenetre_active = attribut(app.ptr(), "AXFocusedWindow");
    lecture.fenetre = fenetre_active
        .as_ref()
        .and_then(|f| attribut(f.ptr(), "AXTitle"))
        .and_then(|t| t.texte());
    lecture.privee = lecture.fenetre.as_deref().is_some_and(titre_prive);
    // Le document ouvert dans la fenêtre (fichier), à défaut d'une page web : ⌘O le rouvre.
    let document = fenetre_active
        .as_ref()
        .and_then(|f| attribut(f.ptr(), "AXDocument"))
        .and_then(|d| d.texte().or_else(|| d.url()))
        .filter(|d| d.starts_with("file://"));

    // La fenêtre active d'abord, puis les autres fenêtres de l'app (leur texte est « non vu » si cachées).
    let mut fenetres: Vec<Cf> = fenetre_active.into_iter().collect();
    if let Some(toutes) = attribut(app.ptr(), "AXWindows")
        && let Some(toutes) = toutes.tableau()
    {
        for f in toutes.iter().map(Cf) {
            if !fenetres.iter().any(|g| unsafe { CFEqual(g.ptr(), f.ptr()) } != 0) {
                fenetres.push(f);
            }
        }
    }

    let noms: Retained<NSArray<NSString>> =
        NSArray::from_retained_slice(&ATTRIBUTS.iter().map(|n| NSString::from_str(n)).collect::<Vec<_>>());
    let mut deja_vus: HashSet<(String, [i64; 2])> = HashSet::new();

    'fenetres: for (rang, fenetre) in fenetres.into_iter().enumerate() {
        let cadre_fenetre = lire_attributs(fenetre.ptr(), &noms)
            .and_then(|a| a.cadre)
            .unwrap_or(ecran);
        let fenetre_visible = rang == 0 && se_croisent(cadre_fenetre, ecran);
        let mut file: Vec<(Cf, usize)> = vec![(fenetre, 0)];
        while let Some((element, profondeur)) = file.pop() {
            if lecture.noeuds >= NOEUDS_MAX || debut.elapsed() > DUREE_MAX {
                lecture.tronquee = true;
                break 'fenetres;
            }
            lecture.noeuds += 1;
            let Some(a) = lire_attributs(element.ptr(), &noms) else {
                continue;
            };
            if a.role == "AXSecureTextField" {
                continue;
            }
            if a.role == "AXWebArea" && lecture.url.is_none() {
                lecture.url = attribut(element.ptr(), "AXURL").and_then(|u| u.url());
            }
            if let Some(texte) = texte_utile(&a) {
                let cadre = a.cadre.unwrap_or(cadre_fenetre);
                // Insigne « Navigation privée / Incognito » dans la fenêtre active (petit élément).
                if rang == 0 && !lecture.privee && cadre[3] < 60.0 && insigne_prive(&a.role, &texte) {
                    lecture.privee = true;
                }
                let cle = (texte.clone(), [cadre[0].round() as i64, cadre[1].round() as i64]);
                if deja_vus.insert(cle) {
                    let vu = fenetre_visible && se_croisent(cadre, cadre_fenetre) && se_croisent(cadre, ecran);
                    let est_focus = focus
                        .as_ref()
                        .is_some_and(|f| unsafe { CFEqual(f.ptr(), element.ptr()) } != 0);
                    for bloc in decouper(&texte) {
                        lecture.elements.push(ElementTexte {
                            texte: bloc,
                            role: a.role.clone(),
                            cadre,
                            vu,
                            focus: est_focus,
                        });
                    }
                }
            }
            if profondeur < PROFONDEUR_MAX {
                // En pile : on garde l'ordre de lecture en inversant les enfants.
                for enfant in a.enfants.into_iter().rev() {
                    file.push((enfant, profondeur + 1));
                }
            }
        }
    }
    if lecture.url.is_none() {
        lecture.url = document;
    }
    lecture.duree = debut.elapsed();
    lecture
}

/// Les navigateurs, seuls à avoir une navigation privée qu'on sait reconnaître.
pub const NAVIGATEURS: &[&str] = &[
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "company.thebrowser.Browser",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "app.zen-browser.zen",
    "org.torproject.torbrowser",
];

/// Le titre d'une fenêtre privée (Firefox, Edge et d'autres l'écrivent dans le titre).
pub fn titre_prive(titre: &str) -> bool {
    let t = titre.to_lowercase();
    ["navigation privée", "private browsing", "inprivate", "(incognito)", "— incognito", "- incognito"]
        .iter()
        .any(|m| t.contains(m))
}

/// L'insigne de navigation privée d'une barre d'outils (Chrome, Safari, Brave, Arc…).
pub fn insigne_prive(role: &str, texte: &str) -> bool {
    const ROLES: &[&str] = &["AXButton", "AXImage", "AXStaticText", "AXGroup", "AXMenuButton", "AXPopUpButton"];
    const INSIGNES: &[&str] = &[
        "navigation privée",
        "fenêtre de navigation privée",
        "fenêtre privée",
        "private browsing",
        "private browsing window",
        "private window",
        "private",
        "privé",
        "privée",
        "incognito",
        "inprivate",
        "mode navigation privée",
    ];
    ROLES.contains(&role) && INSIGNES.contains(&texte.trim().to_lowercase().as_str())
}

/// Une recherche tapée, reconnue dans l'URL d'un moteur ou d'un site (« c'était quoi le poisson… »).
pub fn recherche_dans_url(url: &str) -> Option<(String, String)> {
    let url = url::Url::parse(url).ok()?;
    let hote = url.host_str()?.trim_start_matches("www.").to_string();
    let cle = match hote.as_str() {
        h if h.starts_with("google.") => "q",
        "bing.com" | "duckduckgo.com" | "ecosia.org" | "qwant.com" | "search.brave.com" | "startpage.com" => "q",
        "youtube.com" | "m.youtube.com" => "search_query",
        "search.yahoo.com" => "p",
        "fr.wikipedia.org" | "en.wikipedia.org" | "wikipedia.org" => "search",
        "amazon.fr" | "amazon.com" => "k",
        _ => return None,
    };
    let requete = url
        .query_pairs()
        .find(|(k, _)| k == cle)
        .map(|(_, v)| v.trim().to_string())?;
    (!requete.is_empty()).then_some((hote, requete))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_privee_reconnue() {
        assert!(titre_prive("Résultats — Navigation privée de Mozilla Firefox"));
        assert!(titre_prive("Nouvel onglet - Profil 1 - Microsoft Edge - InPrivate"));
        assert!(!titre_prive("La vie privée sur internet — Wikipédia"));
        assert!(insigne_prive("AXButton", "Navigation privée"));
        assert!(insigne_prive("AXImage", " Incognito "));
        assert!(!insigne_prive("AXStaticText", "Ma vie privée"), "phrase, pas un insigne");
        assert!(!insigne_prive("AXLink", "Private"), "un lien dans la page n'est pas l'insigne");
    }

    #[test]
    fn recherches_reconnues() {
        assert_eq!(
            recherche_dans_url("https://www.google.com/search?q=poisson+rayures+jaunes&hl=fr"),
            Some(("google.com".into(), "poisson rayures jaunes".into()))
        );
        assert_eq!(
            recherche_dans_url("https://www.youtube.com/results?search_query=dauphins"),
            Some(("youtube.com".into(), "dauphins".into()))
        );
        assert_eq!(recherche_dans_url("https://fr.wikipedia.org/wiki/Lac_d'Annecy"), None);
        assert_eq!(recherche_dans_url("https://exemple.fr/?q=x"), None);
    }

    #[test]
    fn decoupage_des_longs_textes() {
        let court = "une ligne";
        assert_eq!(decouper(court), vec![court.to_string()]);
        let long: String = (0..100).map(|i| format!("ligne numéro {i}\n")).collect();
        let blocs = decouper(&long);
        assert!(blocs.len() > 1);
        assert!(blocs.iter().all(|b| b.len() <= BLOC_MAX));
        assert_eq!(blocs.join("\n").lines().count(), 100);
    }
}
