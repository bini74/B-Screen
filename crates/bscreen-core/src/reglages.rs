//! Réglages de B-Screen, enregistrés en JSON.

use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reglages {
    /// Capture en pause (écran et audio).
    pub pause: bool,
    /// Durée de conservation des vidéos, en jours (0 = sans limite).
    pub conservation_jours: u32,
    /// Enregistrer la voix captée par le micro (seulement quand quelqu'un parle).
    pub micro: bool,
    /// Enregistrer les voix qui sortent du Mac (appels, vidéos), seulement quand ça parle.
    pub son_du_mac: bool,
    /// Mots à bien reconnaître dans les voix (prénoms, noms de projets…).
    pub vocabulaire: Vec<String>,
    /// API HTTP locale (127.0.0.1, jeton) : éteinte tant que Julien ne l'allume pas.
    pub api_locale: bool,
    pub port_api: u16,
    /// Apps jamais enregistrées (identifiants), en plus des gestionnaires de mots de passe.
    pub apps_exclues: Vec<String>,
    /// Le modèle pour Ask : Claude « haiku » (rapide), « sonnet » (équilibré), « opus » (approfondi),
    /// ou « local » (sur le Mac, par Ollama : rien ne sort du Mac).
    pub ask_modele: String,
    /// Les vidéos et la voix plus vieilles que ça partent, chiffrées, sur le serveur d'archive
    /// (`archive_hote`) ; l'index, le texte et une vignette par scène restent. 0 = pas d'archive.
    /// Quand l'archive est active, la conservation ne s'applique plus : rien n'est effacé.
    pub archive_jours: u32,
    /// La page web (chercher et demander depuis l'iPhone), sur 127.0.0.1 avec un jeton : éteinte
    /// tant que Julien ne l'allume pas.
    pub page_web: bool,
    pub port_web: u16,
    /// Au plus une image toutes les N secondes (quand l'écran change).
    pub intervalle_capture_s: u32,
    /// Combien de minutes la navigation privée reste visible après Touch ID.
    pub deverrouillage_min: u32,
    /// Masquer les secrets (cartes, IBAN, clés, mots de passe affichés) avant tout stockage.
    pub masquer_secrets: bool,
    /// Recherche par le sens (EmbeddingGemma) et dans les images (SigLIP 2).
    pub recherche_sens: bool,
    pub recherche_images: bool,
    /// L'indexation de fond passe au plus toutes les N minutes ; sur batterie aussi (Mac inactif) ?
    pub indexation_min: u32,
    pub indexation_batterie: bool,
    /// Raccourcis globaux : « alt+space », « alt+shift+a »… (lettre telle que tapée sur ce clavier).
    pub raccourci_frise: String,
    pub raccourci_ask: String,
    /// Raccourci pour mettre en pause ou reprendre (vide : aucun).
    pub raccourci_pause: String,
    /// La pause finit seule à cette heure (ms depuis 1970 ; 0 : pause sans fin).
    pub pause_jusqu_ms: i64,
    /// Domaines jamais enregistrés (« banque.fr » couvre aussi « www.banque.fr ») : leurs pages sont
    /// traitées comme la navigation privée.
    pub sites_exclus: Vec<String>,
    /// Enregistrer les voix pendant les appels (FaceTime, WhatsApp, Teams…).
    pub voix_appels: bool,
    /// La nuit (secteur, Mac inactif) : résumés et phrases floues par le modèle local.
    pub travail_nuit: bool,
    /// Garder aussi le texte jamais visible (la suite d'une page hors de l'écran, les fenêtres
    /// derrière) : la moitié des lignes de la base en novembre 2026. Non par défaut.
    pub texte_cache: bool,
    /// Sites gardés mais masqués dans la recherche (frise, page web) : « youtube.com »…
    pub sites_caches: Vec<String>,
    /// L'archive : le proxy Teleport (« proxy.exemple.fr:443 ») et le compte et la machine
    /// (« bscreen@archive ») ; vides : pas d'archive.
    pub archive_proxy: String,
    pub archive_hote: String,
    /// La page web derrière Teleport : son nom public (« bscreen.exemple.fr »), le proxy dont les
    /// clés signent les requêtes (« proxy.exemple.fr »), les comptes Teleport permis. Vides : la page
    /// ne répond que sur le Mac.
    pub page_web_nom: String,
    pub page_web_proxy: String,
    pub page_web_comptes: Vec<String>,
}

impl Default for Reglages {
    fn default() -> Self {
        Self {
            pause: false,
            conservation_jours: 90,
            micro: true,
            son_du_mac: true,
            vocabulaire: Vec::new(),
            api_locale: false,
            port_api: 47_337,
            apps_exclues: Vec::new(),
            ask_modele: "sonnet".to_string(),
            archive_jours: 30,
            page_web: false,
            port_web: 47_338,
            intervalle_capture_s: 2,
            deverrouillage_min: 5,
            masquer_secrets: true,
            recherche_sens: true,
            recherche_images: true,
            indexation_min: 20,
            indexation_batterie: true,
            raccourci_frise: "alt+space".to_string(),
            raccourci_ask: "alt+shift+a".to_string(),
            raccourci_pause: String::new(),
            pause_jusqu_ms: 0,
            sites_exclus: Vec::new(),
            voix_appels: true,
            travail_nuit: true,
            texte_cache: false,
            sites_caches: Vec::new(),
            archive_proxy: String::new(),
            archive_hote: String::new(),
            page_web_nom: String::new(),
            page_web_proxy: String::new(),
            page_web_comptes: Vec::new(),
        }
    }
}

/// Les touches d'un raccourci (« ctrl+alt+shift+cmd+touche »).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raccourci {
    pub ctrl: bool,
    pub alt: bool,
    pub maj: bool,
    pub cmd: bool,
    /// « space », « f1 »…, ou une lettre ou un chiffre (en minuscule).
    pub touche: String,
}

/// Lit « alt+shift+a » ; refuse un raccourci sans ctrl, alt ni cmd (il prendrait une touche à
/// toutes les apps) ou à touche inconnue.
pub fn lire_raccourci(texte: &str) -> Option<Raccourci> {
    let mut r = Raccourci { ctrl: false, alt: false, maj: false, cmd: false, touche: String::new() };
    for morceau in texte.to_lowercase().split('+') {
        match morceau.trim() {
            "ctrl" | "control" => r.ctrl = true,
            "alt" | "option" => r.alt = true,
            "shift" | "maj" => r.maj = true,
            "cmd" | "command" | "platform" => r.cmd = true,
            "" => return None,
            touche => r.touche = touche.to_string(),
        }
    }
    let touche_connue = r.touche == "space"
        || (r.touche.chars().count() == 1 && r.touche.chars().all(|c| c.is_alphanumeric()))
        || r.touche.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=12).contains(&n));
    ((r.ctrl || r.alt || r.cmd) && touche_connue).then_some(r)
}

/// « ⌥ ⇧ A » : le raccourci comme sur les touches d'un Mac.
pub fn raccourci_lisible(texte: &str) -> String {
    let Some(r) = lire_raccourci(texte) else { return texte.to_string() };
    let mut s: Vec<String> = Vec::new();
    if r.ctrl {
        s.push("⌃".into());
    }
    if r.alt {
        s.push("⌥".into());
    }
    if r.maj {
        s.push("⇧".into());
    }
    if r.cmd {
        s.push("⌘".into());
    }
    s.push(if r.touche == "space" { "Espace".to_string() } else { r.touche.to_uppercase() });
    s.join(" ")
}

/// Le domaine d'un site tapé ou d'une adresse : « https://www.Banque.fr/compte » → « banque.fr ».
pub fn domaine(texte: &str) -> Option<String> {
    let t = texte.trim().to_lowercase();
    let t = t.split_once("://").map_or(t.as_str(), |(_, reste)| reste);
    let hote = t.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let hote = hote.split(':').next()?.trim_end_matches('.');
    let hote = hote.strip_prefix("www.").unwrap_or(hote);
    (hote.contains('.') && hote.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-')).then(|| hote.to_string())
}

/// Cette adresse est-elle sur un des sites exclus (le domaine ou un de ses sous-domaines) ?
pub fn site_exclu(url: &str, sites: &[String]) -> bool {
    let Some(hote) = domaine(url) else { return false };
    sites.iter().any(|s| hote == *s || hote.ends_with(&format!(".{s}")))
}

/// Un changement de réglage demandé par le menu ou la fenêtre de réglages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changement {
    Pause(bool),
    /// En jours (0 = tout garder).
    Conservation(u32),
    Micro(bool),
    SonDuMac(bool),
    AjouterMot(String),
    RetirerMot(String),
    ExclureApp(String),
    ReinclureApp(String),
    Api(bool),
    /// « haiku », « sonnet », « opus » ou « local ».
    ModeleAsk(String),
    /// Après combien de jours les vidéos et la voix partent sur l'archive (0 = jamais).
    Archive(u32),
    /// La page web allumée ou éteinte.
    PageWeb(bool),
    IntervalleCapture(u32),
    Deverrouillage(u32),
    MasquerSecrets(bool),
    RechercheSens(bool),
    RechercheImages(bool),
    Indexation(u32),
    IndexationBatterie(bool),
    RaccourciFrise(String),
    RaccourciAsk(String),
    /// Vide : plus de raccourci pour la pause.
    RaccourciPause(String),
    /// En pause jusqu'à cette heure (ms depuis 1970), puis la capture reprend seule.
    PauseJusqua(i64),
    ExclureSite(String),
    ReinclureSite(String),
    VoixAppels(bool),
    TravailNuit(bool),
    TexteCache(bool),
    CacherSite(String),
    MontrerSite(String),
}

impl Reglages {
    /// Applique un changement ; rend faux s'il ne change rien (mot vide ou déjà là, app déjà exclue…).
    pub fn appliquer(&mut self, changement: &Changement) -> bool {
        let avant = self.clone();
        match changement {
            Changement::Pause(oui) => {
                self.pause = *oui;
                self.pause_jusqu_ms = 0;
            }
            Changement::PauseJusqua(quand) => {
                self.pause = true;
                self.pause_jusqu_ms = *quand;
            }
            Changement::RaccourciPause(r) => {
                if r.is_empty() || (lire_raccourci(r).is_some() && *r != self.raccourci_frise && *r != self.raccourci_ask) {
                    self.raccourci_pause = r.clone();
                }
            }
            Changement::ExclureSite(site) => {
                if let Some(d) = domaine(site)
                    && !self.sites_exclus.contains(&d)
                {
                    self.sites_exclus.push(d);
                }
            }
            Changement::ReinclureSite(site) => self.sites_exclus.retain(|s| s != site),
            Changement::VoixAppels(oui) => self.voix_appels = *oui,
            Changement::TravailNuit(oui) => self.travail_nuit = *oui,
            Changement::TexteCache(oui) => self.texte_cache = *oui,
            Changement::CacherSite(site) => {
                if let Some(d) = domaine(site)
                    && !self.sites_caches.contains(&d)
                {
                    self.sites_caches.push(d);
                }
            }
            Changement::MontrerSite(site) => self.sites_caches.retain(|s| s != site),
            Changement::Conservation(jours) => self.conservation_jours = *jours,
            Changement::Micro(oui) => self.micro = *oui,
            Changement::SonDuMac(oui) => self.son_du_mac = *oui,
            Changement::AjouterMot(mot) => {
                let mot = mot.trim();
                if !mot.is_empty() && !self.vocabulaire.iter().any(|m| m.eq_ignore_ascii_case(mot)) {
                    self.vocabulaire.push(mot.to_string());
                }
            }
            Changement::RetirerMot(mot) => self.vocabulaire.retain(|m| m != mot),
            Changement::ExclureApp(app) => {
                if !app.is_empty() && !self.apps_exclues.contains(app) {
                    self.apps_exclues.push(app.clone());
                }
            }
            Changement::ReinclureApp(app) => self.apps_exclues.retain(|a| a != app),
            Changement::Api(oui) => self.api_locale = *oui,
            Changement::PageWeb(oui) => self.page_web = *oui,
            Changement::IntervalleCapture(s) => self.intervalle_capture_s = (*s).clamp(1, 60),
            Changement::Deverrouillage(m) => self.deverrouillage_min = (*m).clamp(1, 120),
            Changement::MasquerSecrets(oui) => self.masquer_secrets = *oui,
            Changement::RechercheSens(oui) => self.recherche_sens = *oui,
            Changement::RechercheImages(oui) => self.recherche_images = *oui,
            Changement::Indexation(m) => self.indexation_min = (*m).clamp(1, 24 * 60),
            Changement::IndexationBatterie(oui) => self.indexation_batterie = *oui,
            Changement::RaccourciFrise(r) => {
                if lire_raccourci(r).is_some() && *r != self.raccourci_ask && *r != self.raccourci_pause {
                    self.raccourci_frise = r.clone();
                }
            }
            Changement::RaccourciAsk(r) => {
                if lire_raccourci(r).is_some() && *r != self.raccourci_frise && *r != self.raccourci_pause {
                    self.raccourci_ask = r.clone();
                }
            }
            Changement::ModeleAsk(modele) => {
                if ["haiku", "sonnet", "opus", "local"].contains(&modele.as_str()) {
                    self.ask_modele = modele.clone();
                }
            }
            Changement::Archive(jours) => self.archive_jours = *jours,
        }
        *self != avant
    }

    /// Lit les réglages ; en cas d'absence ou d'erreur, renvoie les réglages par défaut.
    pub fn charger() -> Self {
        Self::charger_depuis(&crate::chemins::reglages())
    }

    pub fn charger_depuis(fichier: &Path) -> Self {
        fs::read(fichier)
            .ok()
            .and_then(|octets| serde_json::from_slice(&octets).ok())
            .unwrap_or_default()
    }

    /// Enregistre les réglages (écriture atomique : fichier temporaire puis renommage).
    pub fn enregistrer(&self) -> io::Result<()> {
        self.enregistrer_dans(&crate::chemins::reglages())
    }

    pub fn enregistrer_dans(&self, fichier: &Path) -> io::Result<()> {
        if let Some(dossier) = fichier.parent() {
            fs::create_dir_all(dossier)?;
        }
        let temporaire = fichier.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&temporaire, json)?;
        fs::rename(&temporaire, fichier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour() {
        let dossier = std::env::temp_dir().join(format!("bscreen-test-{}", std::process::id()));
        let fichier = dossier.join("reglages.json");
        let r = Reglages {
            pause: true,
            conservation_jours: 30,
            micro: false,
            son_du_mac: true,
            vocabulaire: vec!["Camille".into()],
            api_locale: true,
            port_api: 47_000,
            apps_exclues: vec!["com.exemple.app".into()],
            ask_modele: "haiku".into(),
            archive_jours: 45,
            page_web: true,
            port_web: 47_001,
            intervalle_capture_s: 5,
            deverrouillage_min: 15,
            masquer_secrets: false,
            recherche_sens: false,
            recherche_images: true,
            indexation_min: 60,
            indexation_batterie: false,
            raccourci_frise: "ctrl+alt+f".into(),
            raccourci_ask: "alt+shift+a".into(),
            raccourci_pause: "alt+shift+p".into(),
            pause_jusqu_ms: 1_800_000_000_000,
            sites_exclus: vec!["banque.fr".into()],
            voix_appels: false,
            travail_nuit: false,
            texte_cache: true,
            sites_caches: vec!["youtube.com".into()],
            archive_proxy: "proxy.exemple.fr:443".into(),
            archive_hote: "bscreen@archive".into(),
            page_web_nom: "bscreen.exemple.fr".into(),
            page_web_proxy: "proxy.exemple.fr".into(),
            page_web_comptes: vec!["moi".into()],
        };
        r.enregistrer_dans(&fichier).unwrap();
        assert_eq!(Reglages::charger_depuis(&fichier), r);
        fs::remove_dir_all(&dossier).unwrap();
    }

    #[test]
    fn changements() {
        let mut r = Reglages::default();
        assert!(r.appliquer(&Changement::AjouterMot("  Camille ".into())));
        assert_eq!(r.vocabulaire, vec!["Camille".to_string()]);
        // Déjà là (majuscules ignorées), ou vide : rien ne change.
        assert!(!r.appliquer(&Changement::AjouterMot("camille".into())));
        assert!(!r.appliquer(&Changement::AjouterMot("   ".into())));
        assert!(r.appliquer(&Changement::RetirerMot("Camille".into())));
        assert!(r.vocabulaire.is_empty());

        assert!(r.appliquer(&Changement::ExclureApp("com.exemple.app".into())));
        assert!(!r.appliquer(&Changement::ExclureApp("com.exemple.app".into())));
        assert!(r.appliquer(&Changement::ReinclureApp("com.exemple.app".into())));
        assert!(r.apps_exclues.is_empty());

        assert!(r.appliquer(&Changement::Conservation(30)));
        assert!(!r.appliquer(&Changement::Conservation(30)));
        assert!(r.appliquer(&Changement::Archive(60)) && r.archive_jours == 60);
        assert!(r.appliquer(&Changement::Pause(true)) && r.pause);
        assert!(r.appliquer(&Changement::Micro(false)) && !r.micro);
        assert!(r.appliquer(&Changement::SonDuMac(false)) && !r.son_du_mac);
        assert!(r.appliquer(&Changement::Api(true)) && r.api_locale);
        // Raccourcis : lus, montrés comme sur les touches ; sans modificateur ou en double, refusés.
        assert_eq!(raccourci_lisible("alt+shift+a"), "⌥ ⇧ A");
        assert_eq!(raccourci_lisible("ctrl+cmd+space"), "⌃ ⌘ Espace");
        assert!(lire_raccourci("a").is_none() && lire_raccourci("shift+a").is_none() && lire_raccourci("alt+").is_none());
        assert!(!r.appliquer(&Changement::RaccourciFrise("shift+x".into())));
        assert!(!r.appliquer(&Changement::RaccourciFrise(r.raccourci_ask.clone())));
        assert!(r.appliquer(&Changement::RaccourciFrise("ctrl+alt+f".into())) && r.raccourci_frise == "ctrl+alt+f");
        // Sites exclus : le domaine seul, sous-domaines compris ; pas « faussebanque.fr ».
        assert_eq!(domaine("https://www.Banque.fr/compte?x=1").as_deref(), Some("banque.fr"));
        assert_eq!(domaine("pas un site"), None);
        assert!(r.appliquer(&Changement::ExclureSite("https://www.banque.fr/".into())));
        assert!(!r.appliquer(&Changement::ExclureSite("banque.fr".into())));
        assert!(site_exclu("https://espace.banque.fr/releve", &r.sites_exclus));
        assert!(!site_exclu("https://faussebanque.fr/", &r.sites_exclus));
        // Pause pour un temps, puis pause simple : l'heure de reprise s'efface.
        assert!(r.appliquer(&Changement::PauseJusqua(5)) && r.pause && r.pause_jusqu_ms == 5);
        assert!(r.appliquer(&Changement::Pause(false)) && !r.pause && r.pause_jusqu_ms == 0);
        assert!(r.appliquer(&Changement::ModeleAsk("haiku".into())) && r.ask_modele == "haiku");
        assert!(!r.appliquer(&Changement::ModeleAsk("gpt".into())), "modèle inconnu refusé");
        assert!(r.appliquer(&Changement::ModeleAsk("local".into())) && r.ask_modele == "local");
    }

    #[test]
    fn fichier_absent_ou_abime() {
        let absent = Path::new("/chemin/qui/n/existe/pas/reglages.json");
        assert_eq!(Reglages::charger_depuis(absent), Reglages::default());

        let abime = std::env::temp_dir().join(format!("bscreen-abime-{}.json", std::process::id()));
        fs::write(&abime, b"{ pas du json").unwrap();
        assert_eq!(Reglages::charger_depuis(&abime), Reglages::default());
        fs::remove_file(&abime).unwrap();
    }
}
