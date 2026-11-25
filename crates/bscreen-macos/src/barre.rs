//! L'icône de B-Screen dans la barre des menus, et son menu.
//!
//! Tout vit sur le fil principal : les éléments de menu ne se partagent pas
//! entre fils, donc la barre est rangée dans une variable propre au fil principal
//! et les clics y sont renvoyés.

use std::cell::RefCell;

use anyhow::Result;
use bscreen_core::reglages::{Changement, Reglages};
use tray_icon::menu::accelerator::{Accelerator, Code, Modifiers};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::autorisations::{Autorisation, Etat};
use crate::capture::STATS;
use crate::connexion::{self, EtatConnexion};
use crate::moteur;

const ICONE: &[u8] = include_bytes!("../../../assets/barre-36.rgba");
const COTE_ICONE: u32 = 36;

thread_local! {
    static BARRE: RefCell<Option<Barre>> = const { RefCell::new(None) };
}

struct Barre {
    _icone: TrayIcon,
    etat: MenuItem,
    disque: MenuItem,
    pause: CheckMenuItem,
    micro: CheckMenuItem,
    son_du_mac: CheckMenuItem,
    /// « Effacer » : les 5 dernières minutes, la dernière heure, aujourd'hui.
    effacer: [MenuItem; 3],
    /// API HTTP locale : allumée ou non, et « Copier le jeton ».
    api: CheckMenuItem,
    copier_jeton: MenuItem,
    /// « Ne plus enregistrer « App » » (l'app au premier plan) et « Tout réenregistrer ».
    exclure: MenuItem,
    reinclure: MenuItem,
    /// Durée de conservation : (jours, case) — 0 = tout garder.
    conservation: Vec<(u32, CheckMenuItem)>,
    autorisations: Vec<(Autorisation, MenuItem)>,
    connexion: CheckMenuItem,
    quitter: MenuItem,
    frise: MenuItem,
    ask: MenuItem,
    ouvrir_reglages: MenuItem,
    reglages: Reglages,
    quand_quitter: Box<dyn Fn()>,
    quand_frise: Box<dyn Fn()>,
    quand_reglages: Box<dyn Fn()>,
    quand_ask: Box<dyn Fn()>,
}

/// Crée l'icône et son menu. À appeler sur le fil principal, une fois l'app lancée.
/// La page web (dans l'app, crate `bscreen`) : allumée (vrai) ou éteinte, sur ce port. Posé par
/// l'app au démarrage.
pub static PAGE_WEB: std::sync::OnceLock<fn(bool, u16)> = std::sync::OnceLock::new();

/// Pose les raccourcis globaux (frise, Ask) : dans l'app (crate `bscreen`), posé au démarrage.
pub static RACCOURCIS: std::sync::OnceLock<fn(&str, &str, &str)> = std::sync::OnceLock::new();

pub fn installer(
    quand_quitter: impl Fn() + 'static,
    quand_frise: impl Fn() + 'static,
    quand_reglages: impl Fn() + 'static,
    quand_ask: impl Fn() + 'static,
) -> Result<()> {
    let reglages = Reglages::charger();

    let titre = MenuItem::new(format!("{} {}", bscreen_core::NOM, bscreen_core::VERSION), false, None);
    let etat = MenuItem::new("", false, None);
    let disque = MenuItem::new("", false, None);
    let pause = CheckMenuItem::new("Mettre en pause", true, reglages.pause, None);
    let micro = CheckMenuItem::new("Voix captées par le micro", true, reglages.micro, None);
    let son_du_mac = CheckMenuItem::new("Voix qui sortent du Mac", true, reglages.son_du_mac, None);
    let sous_menu_son = Submenu::with_items("Enregistrer les voix", true, &[&micro, &son_du_mac])?;
    let effacer = [
        MenuItem::new("Les 5 dernières minutes…", true, None),
        MenuItem::new("La dernière heure…", true, None),
        MenuItem::new("Aujourd'hui…", true, None),
    ];
    let sous_menu_effacer = Submenu::with_items("Effacer", true, &[&effacer[0], &effacer[1], &effacer[2]])?;
    let api = CheckMenuItem::new(
        format!("API locale (127.0.0.1:{})", reglages.port_api),
        true,
        reglages.api_locale,
        None,
    );
    let copier_jeton = MenuItem::new("Copier le jeton de l'API", reglages.api_locale, None);
    let sous_menu_api = Submenu::with_items("Pour les autres programmes", true, &[&api, &copier_jeton])?;
    let exclure = MenuItem::new("Ne plus enregistrer l'app au premier plan", false, None);
    let reinclure = MenuItem::new("Réenregistrer toutes les apps", false, None);
    let conservation: Vec<(u32, CheckMenuItem)> = [(30, "30 jours"), (90, "90 jours"), (365, "1 an"), (0, "Tout garder")]
        .into_iter()
        .map(|(jours, nom)| (jours, CheckMenuItem::new(nom, true, reglages.conservation_jours == jours, None)))
        .collect();
    let sous_menu_conservation = Submenu::with_items(
        "Garder",
        true,
        &conservation
            .iter()
            .map(|(_, item)| item as &dyn tray_icon::menu::IsMenuItem)
            .collect::<Vec<_>>(),
    )?;
    let sous_menu_apps = Submenu::with_items(
        "Apps exclues",
        true,
        &[&exclure, &PredefinedMenuItem::separator(), &reinclure],
    )?;
    let autorisations: Vec<(Autorisation, MenuItem)> = Autorisation::TOUTES
        .iter()
        .map(|&a| (a, MenuItem::new(a.nom(), true, None)))
        .collect();
    let sous_menu = Submenu::with_items(
        "Autorisations",
        true,
        &autorisations
            .iter()
            .map(|(_, item)| item as &dyn tray_icon::menu::IsMenuItem)
            .collect::<Vec<_>>(),
    )?;
    let connexion = CheckMenuItem::new("Ouvrir à la connexion", true, false, None);
    let frise = MenuItem::new("Ouvrir la frise   ⌥Espace", true, None);
    let ask = MenuItem::new("Demander…   ⌥⇧A", true, None);
    let ouvrir_reglages = MenuItem::new(
        "Réglages…",
        true,
        Some(Accelerator::new(Modifiers::META, Code::Comma)),
    );
    let quitter = MenuItem::new(
        format!("Quitter {}", bscreen_core::NOM),
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
    );

    // Un menu court (Julien, 25/11/2026 : trop d'options) : le reste est dans les Réglages
    // (voix, effacer, garder, apps exclues, autorisations, API, ouverture à la connexion). Ces
    // éléments restent tenus à jour ici sans être montrés.
    let _ = (&titre, &disque, &sous_menu_apps, &sous_menu_effacer, &sous_menu_conservation, &sous_menu_son, &sous_menu, &sous_menu_api, &connexion);
    let menu = Menu::with_items(&[
        &etat,
        &PredefinedMenuItem::separator(),
        &frise,
        &ask,
        &pause,
        &PredefinedMenuItem::separator(),
        &ouvrir_reglages,
        &quitter,
    ])?;

    let icone = TrayIconBuilder::new()
        .with_icon_templated(Icon::from_rgba(ICONE.to_vec(), COTE_ICONE, COTE_ICONE)?)
        .with_tooltip(bscreen_core::NOM)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .build()?;

    let barre = Barre {
        _icone: icone,
        etat,
        disque,
        pause,
        micro,
        son_du_mac,
        effacer,
        api,
        copier_jeton,
        exclure,
        reinclure,
        conservation,
        autorisations,
        connexion,
        quitter,
        frise,
        ask,
        ouvrir_reglages,
        reglages,
        quand_quitter: Box::new(quand_quitter),
        quand_frise: Box::new(quand_frise),
        quand_reglages: Box::new(quand_reglages),
        quand_ask: Box::new(quand_ask),
    };
    barre.rafraichir();
    BARRE.with(|b| *b.borrow_mut() = Some(barre));

    // Les clics arrivent sur le fil principal : on les renvoie à la barre.
    MenuEvent::set_event_handler(Some(|evenement: MenuEvent| {
        BARRE.with(|b| {
            if let Some(barre) = b.borrow_mut().as_mut() {
                barre.gerer(&evenement);
            }
        });
    }));
    tracing::info!("icône de la barre des menus installée");
    Ok(())
}

/// Met à jour les libellés (autorisations, ouverture à la connexion).
pub fn rafraichir() {
    // Une pause pour un temps qui est finie : la capture reprend.
    let r = reglages();
    if r.pause && r.pause_jusqu_ms > 0 && chrono_ms() >= r.pause_jusqu_ms {
        tracing::info!("pause finie : la capture reprend");
        changer(Changement::Pause(false));
    }
    BARRE.with(|b| {
        if let Ok(barre) = b.try_borrow()
            && let Some(barre) = barre.as_ref()
        {
            barre.rafraichir();
        }
    });
}

fn chrono_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Dernier état connu de chaque autorisation, pour noter les changements dans le journal.
static ETATS_VUS: std::sync::Mutex<Vec<(Autorisation, Etat)>> = std::sync::Mutex::new(Vec::new());

fn noter_changement(autorisation: Autorisation, etat: Etat) {
    let Ok(mut vus) = ETATS_VUS.lock() else { return };
    match vus.iter_mut().find(|(a, _)| *a == autorisation) {
        Some((_, ancien)) if *ancien == etat => {}
        Some((_, ancien)) => {
            tracing::info!(autorisation = autorisation.nom(), avant = ?*ancien, maintenant = ?etat, "autorisation changée");
            *ancien = etat;
        }
        None => {
            tracing::info!(autorisation = autorisation.nom(), etat = ?etat, "autorisation");
            vus.push((autorisation, etat));
        }
    }
}

impl Barre {
    fn rafraichir(&self) {
        let mut manquantes = 0;
        for (autorisation, item) in &self.autorisations {
            let etat = autorisation.etat();
            noter_changement(*autorisation, etat);
            let texte = match etat {
                Etat::Accordee => format!("✓  {}", autorisation.nom()),
                Etat::ADemander => format!("○  {} : autoriser… ({})", autorisation.nom(), autorisation.raison()),
                Etat::Refusee => format!("✕  {} : refusée, ouvrir les Réglages…", autorisation.nom()),
            };
            if !etat.accordee() {
                manquantes += 1;
            }
            item.set_text(texte);
        }

        // Démarre la capture dès que c'est possible (autorisation donnée, pas de pause).
        moteur::demarrer_si_possible(self.reglages.pause);

        let etat = if self.reglages.pause {
            "En pause".to_string()
        } else if moteur::DISQUE_PLEIN.load(std::sync::atomic::Ordering::SeqCst) {
            let libre = moteur::place_libre().map(taille_lisible).unwrap_or_default();
            format!("En pause : disque presque plein ({libre} libres) — reprise au-dessus de 8 Go")
        } else if moteur::en_marche() {
            etat_capture()
        } else if let Some(echec) = moteur::dernier_echec() {
            format!("Capture impossible : {echec}")
        } else if !Autorisation::Ecran.etat().accordee() {
            "Donne l'autorisation « Enregistrement de l'écran »".to_string()
        } else if manquantes > 0 {
            format!("{manquantes} autorisation(s) à donner")
        } else {
            "Démarrage de la capture…".to_string()
        };
        self.etat.set_text(etat);
        self.disque.set_text(place_sur_le_disque());

        // L'app au premier plan, à exclure (sauf B-Screen et celles déjà exclues).
        let (bundle, nom) = moteur::app_au_premier_plan();
        let excluable = bundle.as_deref().filter(|b| !crate::capture::est_exclue(b));
        self.exclure.set_text(match (excluable, nom.as_deref()) {
            (Some(_), Some(nom)) => format!("Ne plus enregistrer « {nom} »"),
            _ => "Ne plus enregistrer l'app au premier plan".to_string(),
        });
        self.exclure.set_enabled(excluable.is_some());
        let n = self.reglages.apps_exclues.len();
        self.reinclure.set_text(if n == 0 {
            "Aucune app exclue par toi".to_string()
        } else {
            format!("Réenregistrer les {n} app{} exclues", if n > 1 { "s" } else { "" })
        });
        self.reinclure.set_enabled(n > 0);

        let connexion = connexion::etat();
        self.connexion
            .set_checked(matches!(connexion, EtatConnexion::Active | EtatConnexion::AApprouver));
        self.connexion.set_enabled(connexion != EtatConnexion::Indisponible);
        self.connexion.set_text(match connexion {
            EtatConnexion::AApprouver => "Ouvrir à la connexion (à approuver dans les Réglages)",
            EtatConnexion::Indisponible => "Ouvrir à la connexion (seulement depuis B-Screen.app)",
            _ => "Ouvrir à la connexion",
        });
    }

    fn gerer(&mut self, evenement: &MenuEvent) {
        let id = evenement.id();
        if id == self.pause.id() {
            self.changer(&Changement::Pause(self.pause.is_checked()));
            return;
        } else if let Some(jours) = self.conservation.iter().find(|(_, item)| item.id() == id).map(|(j, _)| *j) {
            // Raccourcir efface tout de suite ce qui est plus vieux : on demande d'abord.
            if conservation_refusee(self.reglages.conservation_jours, jours) {
                self.cocher_conservation();
                return;
            }
            self.changer(&Changement::Conservation(jours));
            return;
        } else if id == self.exclure.id() {
            let (bundle, nom) = moteur::app_au_premier_plan();
            if let Some(bundle) = bundle.filter(|b| !crate::capture::est_exclue(b)) {
                tracing::info!(app = nom.as_deref().unwrap_or(&bundle), "app exclue de l'enregistrement");
                self.changer(&Changement::ExclureApp(bundle));
            }
            return;
        } else if id == self.reinclure.id() {
            tracing::info!(apps = self.reglages.apps_exclues.len(), "apps réenregistrées");
            let mut change = false;
            for app in self.reglages.apps_exclues.clone() {
                change |= self.changer_sans_suite(&Changement::ReinclureApp(app));
            }
            if change {
                self.apres_apps_exclues();
            }
        } else if id == self.api.id() {
            self.changer(&Changement::Api(self.api.is_checked()));
            return;
        } else if id == self.copier_jeton.id() {
            if crate::api_locale::copier_jeton() {
                tracing::info!("jeton de l'API copié dans le presse-papiers");
            }
        } else if let Some(rang) = self.effacer.iter().position(|item| item.id() == id) {
            demander_effacement(rang);
        } else if id == self.micro.id() {
            self.changer(&Changement::Micro(self.micro.is_checked()));
            return;
        } else if id == self.son_du_mac.id() {
            self.changer(&Changement::SonDuMac(self.son_du_mac.is_checked()));
            return;
        } else if id == self.connexion.id() {
            changer_connexion(self.connexion.is_checked());
        } else if id == self.frise.id() {
            (self.quand_frise)();
            return;
        } else if id == self.ouvrir_reglages.id() {
            (self.quand_reglages)();
            return;
        } else if id == self.ask.id() {
            (self.quand_ask)();
            return;
        } else if id == self.quitter.id() {
            moteur::arreter();
            (self.quand_quitter)();
            return;
        } else if let Some((autorisation, _)) = self.autorisations.iter().find(|(_, item)| item.id() == id) {
            match autorisation.etat() {
                Etat::Accordee => autorisation.ouvrir_reglages(),
                _ => autorisation.demander(),
            }
        }
        self.rafraichir();
    }

    fn cocher_conservation(&self) {
        for (j, item) in &self.conservation {
            item.set_checked(*j == self.reglages.conservation_jours);
        }
    }

    /// Applique un changement (menu ou fenêtre de réglages) : réglages enregistrés, cases du menu
    /// remises d'accord, puis ce qu'il faut relancer. Rend faux si rien n'a changé.
    fn changer(&mut self, changement: &Changement) -> bool {
        if !self.changer_sans_suite(changement) {
            // Le menu a pu cocher sa case tout seul : on la remet d'accord avec les réglages.
            self.cocher();
            return false;
        }
        match changement {
            Changement::Pause(pause) => {
                if *pause {
                    moteur::arreter();
                }
                tracing::info!(pause, "pause changée");
            }
            Changement::PauseJusqua(quand) => {
                moteur::arreter();
                tracing::info!(jusqu_ms = quand, "pause pour un temps");
            }
            Changement::Conservation(jours) => {
                tracing::info!(jours, "durée de conservation changée");
                // Plus court : le plus vieux part tout de suite.
                let _ = std::thread::Builder::new().name("bscreen-purge".into()).spawn(moteur::purger);
            }
            Changement::Micro(_) | Changement::SonDuMac(_) => {
                tracing::info!(micro = self.reglages.micro, son_du_mac = self.reglages.son_du_mac, "voix enregistrées changées");
                if self.reglages.micro && Autorisation::Micro.etat() != Etat::Accordee {
                    Autorisation::Micro.demander();
                }
                // La capture repart avec les nouveaux choix.
                if moteur::en_marche() {
                    moteur::relancer("choix des voix changé");
                }
            }
            Changement::ModeleAsk(modele) => tracing::info!(modele, "modèle d'Ask changé"),
            Changement::Archive(jours) => tracing::info!(jours, "archive du homelab changée"),
            Changement::AjouterMot(_) | Changement::RetirerMot(_) => {
                // La transcription relit le vocabulaire à chaque passage.
                tracing::info!(mots = self.reglages.vocabulaire.len(), "vocabulaire changé");
            }
            Changement::ExclureApp(_) | Changement::ReinclureApp(_) => self.apres_apps_exclues(),
            Changement::Api(allumee) => {
                if *allumee {
                    crate::api_locale::demarrer(self.reglages.port_api);
                } else {
                    crate::api_locale::arreter();
                }
                tracing::info!(allumee, "API locale changée");
            }
            Changement::PageWeb(allumee) => {
                if let Some(page) = PAGE_WEB.get() {
                    page(*allumee, self.reglages.port_web);
                }
                tracing::info!(allumee, "page web changée");
            }
            Changement::RaccourciFrise(_) | Changement::RaccourciAsk(_) | Changement::RaccourciPause(_) => {
                if let Some(poser) = RACCOURCIS.get() {
                    poser(&self.reglages.raccourci_frise, &self.reglages.raccourci_ask, &self.reglages.raccourci_pause);
                }
                tracing::info!(frise = self.reglages.raccourci_frise, ask = self.reglages.raccourci_ask, "raccourcis changés");
            }
            Changement::IntervalleCapture(_)
            | Changement::Deverrouillage(_)
            | Changement::MasquerSecrets(_)
            | Changement::RechercheSens(_)
            | Changement::RechercheImages(_)
            | Changement::Indexation(_)
            | Changement::IndexationBatterie(_)
            | Changement::VoixAppels(_)
            | Changement::TravailNuit(_)
            | Changement::TexteCache(_) => tracing::info!(?changement, "réglage changé"),
            Changement::CacherSite(_) | Changement::MontrerSite(_) => {
                tracing::info!(sites = self.reglages.sites_caches.len(), "sites masqués dans la recherche changés")
            }
            Changement::ExclureSite(_) | Changement::ReinclureSite(_) => {
                tracing::info!(sites = self.reglages.sites_exclus.len(), "sites exclus changés")
            }
        }
        crate::vivants::appliquer(&self.reglages);
        self.rafraichir();
        true
    }

    /// Change et enregistre les réglages, coche le menu ; sans rien relancer.
    fn changer_sans_suite(&mut self, changement: &Changement) -> bool {
        if !self.reglages.appliquer(changement) {
            return false;
        }
        if let Err(e) = self.reglages.enregistrer() {
            tracing::warn!(%e, "réglages non enregistrés");
        }
        self.cocher();
        true
    }

    /// Les cases du menu, d'après les réglages.
    fn cocher(&self) {
        self.pause.set_checked(self.reglages.pause);
        self.micro.set_checked(self.reglages.micro);
        self.son_du_mac.set_checked(self.reglages.son_du_mac);
        self.api.set_checked(self.reglages.api_locale);
        self.copier_jeton.set_enabled(self.reglages.api_locale);
        self.cocher_conservation();
    }

    fn apres_apps_exclues(&self) {
        moteur::noter_apps_exclues();
        // La capture repart avec le nouveau filtre (les fenêtres exclues disparaissent des images).
        if moteur::en_marche() {
            moteur::relancer("apps exclues changées");
        }
    }
}

/// Raccourcir la durée de conservation efface le plus vieux tout de suite : vrai si Julien refuse.
fn conservation_refusee(avant: u32, jours: u32) -> bool {
    let raccourcit = jours != 0 && (avant == 0 || jours < avant);
    raccourcit && !confirmer_conservation(jours)
}

/// Ouvrir B-Screen à la connexion, ou plus.
pub fn changer_connexion(voulu: bool) {
    let resultat = if voulu {
        connexion::activer()
    } else {
        connexion::desactiver()
    };
    match resultat {
        Ok(()) if connexion::etat() == EtatConnexion::AApprouver => connexion::ouvrir_reglages(),
        Ok(()) => tracing::info!(active = voulu, "ouverture à la connexion changée"),
        Err(e) => tracing::warn!(%e, "ouverture à la connexion"),
    }
}

/// Applique un changement venu de la fenêtre de réglages (sur le fil principal). Pour une durée de
/// conservation plus courte, demande d'abord confirmation. Rend faux si rien n'a changé.
pub fn changer(changement: Changement) -> bool {
    if let Changement::Conservation(jours) = changement {
        let avant = reglages().conservation_jours;
        if conservation_refusee(avant, jours) {
            return false;
        }
    }
    let fait = BARRE.with(|b| match b.try_borrow_mut() {
        Ok(mut barre) => barre.as_mut().map(|barre| barre.changer(&changement)),
        Err(_) => None,
    });
    match fait {
        Some(fait) => fait,
        None => {
            // Pas de menu (ou occupé) : les réglages sont quand même changés et enregistrés.
            let mut r = Reglages::charger();
            let fait = r.appliquer(&changement);
            if fait && let Err(e) = r.enregistrer() {
                tracing::warn!(%e, "réglages non enregistrés");
            }
            fait
        }
    }
}

/// Les réglages en cours (ceux du menu, toujours enregistrés).
pub fn reglages() -> Reglages {
    BARRE
        .with(|b| b.try_borrow().ok().and_then(|b| b.as_ref().map(|b| b.reglages.clone())))
        .unwrap_or_else(Reglages::charger)
}

/// « Effacer » : demande confirmation, puis efface la période en arrière-plan.
pub fn demander_effacement(rang: usize) {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let maintenant = crate::encodage::ms_depuis_1970(std::time::SystemTime::now());
    let (debut, quoi) = match rang {
        0 => (maintenant - 5 * 60_000, "les 5 dernières minutes"),
        1 => (maintenant - 3_600_000, "la dernière heure"),
        _ => (debut_du_jour(maintenant), "tout ce qui a été enregistré aujourd'hui"),
    };
    let alerte = objc2_app_kit::NSAlert::new(mtm);
    alerte.setAlertStyle(objc2_app_kit::NSAlertStyle::Warning);
    alerte.setMessageText(&objc2_foundation::NSString::from_str(&format!("Effacer {quoi} ?")));
    alerte.setInformativeText(&objc2_foundation::NSString::from_str(
        "Images, texte lu à l'écran, voix et recherches de cette période seront effacés définitivement.",
    ));
    alerte.addButtonWithTitle(&objc2_foundation::NSString::from_str("Effacer"));
    alerte.addButtonWithTitle(&objc2_foundation::NSString::from_str("Annuler"));
    // B-Screen n'a pas de fenêtre : on passe devant pour que la question se voie.
    objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
    if alerte.runModal() != objc2_app_kit::NSAlertFirstButtonReturn {
        return;
    }
    let _ = std::thread::Builder::new().name("bscreen-effacement".into()).spawn(move || {
        match crate::effacement::effacer_periode(debut, i64::MAX) {
            Ok((images, voix)) => tracing::info!(images, voix, quoi, "période effacée"),
            Err(e) => tracing::error!(%e, "effacement de la période impossible"),
        }
    });
}

/// Confirme une durée de conservation plus courte (ce qui est plus vieux sera effacé tout de suite).
fn confirmer_conservation(jours: u32) -> bool {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
    let duree = match jours {
        30 => "30 jours".to_string(),
        365 => "1 an".to_string(),
        j => format!("{j} jours"),
    };
    let alerte = objc2_app_kit::NSAlert::new(mtm);
    alerte.setAlertStyle(objc2_app_kit::NSAlertStyle::Warning);
    alerte.setMessageText(&objc2_foundation::NSString::from_str(&format!("Ne garder que {duree} ?")));
    alerte.setInformativeText(&objc2_foundation::NSString::from_str(&format!(
        "Tout ce qui a plus de {duree} (images, texte, voix) sera effacé définitivement, tout de suite."
    )));
    alerte.addButtonWithTitle(&objc2_foundation::NSString::from_str("Effacer le plus vieux"));
    alerte.addButtonWithTitle(&objc2_foundation::NSString::from_str("Annuler"));
    objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
    alerte.runModal() == objc2_app_kit::NSAlertFirstButtonReturn
}

/// Minuit (heure locale) du jour de `ms`.
fn debut_du_jour(ms: i64) -> i64 {
    let secondes = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY : localtime_r et mktime n'écrivent que dans `tm`.
    unsafe {
        libc::localtime_r(&secondes, &mut tm);
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        libc::mktime(&mut tm) * 1000
    }
}

/// « 1,2 Go », « 340 Mo », « 12 Ko ».
pub fn taille_lisible(octets: u64) -> String {
    const MO: f64 = 1_048_576.0;
    let mo = octets as f64 / MO;
    if mo >= 1024.0 {
        format!("{:.1} Go", mo / 1024.0).replace('.', ",")
    } else if mo >= 1.0 {
        format!("{mo:.0} Mo")
    } else {
        format!("{} Ko", octets / 1024)
    }
}

/// « Disque : 2,1 Go de vidéo · 34 Mo de voix · base 90 Mo »
fn place_sur_le_disque() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let (video, voix, base) = (
        moteur::PLACE_VIDEO.load(Relaxed),
        moteur::PLACE_VOIX.load(Relaxed),
        moteur::PLACE_BASE.load(Relaxed),
    );
    let cpu = moteur::CPU_POUR_MILLE.load(Relaxed);
    let processeur = if cpu == u64::MAX {
        String::new()
    } else {
        format!(
            " · {},{} % de CPU · {} de mémoire",
            cpu / 10,
            cpu % 10,
            taille_lisible(moteur::MEMOIRE.load(Relaxed))
        )
    };
    if video + voix + base == 0 {
        return format!("Disque : mesure en cours…{processeur}");
    }
    format!(
        "Disque : {} de vidéo · {} de voix · base {}{processeur}",
        taille_lisible(video),
        taille_lisible(voix),
        taille_lisible(base)
    )
}

/// « Capture : 1 écran · 214 images · dernière il y a 3 s »
fn etat_capture() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let ecrans = STATS.ecrans.load(Relaxed);
    let gardees = STATS.gardees.load(Relaxed);
    let derniere = STATS.derniere_ms.load(Relaxed);
    let maintenant = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let depuis = if derniere == 0 {
        "aucune image encore".to_string()
    } else {
        let s = maintenant.saturating_sub(derniere) / 1000;
        if s < 60 {
            format!("dernière il y a {s} s")
        } else {
            format!("dernière il y a {} min", s / 60)
        }
    };
    let pluriel = if ecrans > 1 { "s" } else { "" };
    let son = if crate::capture::SON_EN_ECHEC.load(Relaxed) { " · son indisponible" } else { "" };
    format!("Capture : {ecrans} écran{pluriel} · {gardees} images · {depuis}{son}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailles() {
        assert_eq!(taille_lisible(2_254_857_830), "2,1 Go");
        assert_eq!(taille_lisible(35_651_584), "34 Mo");
        assert_eq!(taille_lisible(12_288), "12 Ko");
    }

    #[test]
    fn minuit_heure_locale() {
        let maintenant = crate::encodage::ms_depuis_1970(std::time::SystemTime::now());
        let minuit = debut_du_jour(maintenant);
        assert!(minuit <= maintenant && maintenant - minuit < 86_400_000);
        let secondes = (minuit / 1000) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&secondes, &mut tm) };
        assert_eq!((tm.tm_hour, tm.tm_min, tm.tm_sec), (0, 0, 0));
    }
}
