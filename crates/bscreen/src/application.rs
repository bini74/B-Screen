//! L'app : la boucle de l'interface (gpui), les services de fond (macOS) et la frise.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use bscreen_core::chemins;
use bscreen_core::image::ImageBgra;
use bscreen_store::Base;
use bscreen_core::reglages::Changement;
use bscreen_ui::frise::{self, Moment, Passage, SourceFrise, Trouvee, VoixChoisie};
use bscreen_ui::ask::{self, SourceAsk};
use bscreen_ui::reglages::{self, AppConnue, EtatReglages, Personne, SourceReglages};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, Application};

/// Ce qui arrive de la couche macOS (menu, trackpad) et doit être traité par l'interface.
enum Demande {
    /// Avec l'instant de la demande (raccourci, menu) : on mesure le temps d'ouverture.
    OuvrirFrise(Instant),
    OuvrirReglages,
    OuvrirAsk(Instant),
    Pincer(f64),
    /// Le raccourci de la pause : mettre en pause ou reprendre.
    BasculerPause,
}

/// Combien de temps la frise remonte (⌘← / ⌘→ passent d'un jour à l'autre).
const PERIODE_FRISE_MS: i64 = 7 * 24 * 3600 * 1000;

pub fn lancer() -> Result<()> {
    if bscreen_macos::app::deja_lancee() {
        bail!("B-Screen tourne déjà");
    }
    arret_propre_sur_signal();
    Application::new().run(|cx: &mut App| {
        bscreen_macos::app::mode_agent();
        // Données chiffrées : ouvrir l'image avant tout (clé du Trousseau, sinon clé de secours).
        if let Err(e) = bscreen_macos::coffre::ouvrir(true) {
            tracing::error!(%e, "données chiffrées fermées : B-Screen s'arrête sans rien enregistrer");
            cx.quit();
            return;
        }
        // Recherche par le sens : indexation de fond (secteur + Mac inactif) et questions.
        bscreen_macos::sens::demarrer();
        // Recherche dans les images : une image par scène, regardée en fond, et questions.
        bscreen_macos::images::demarrer_avec(true);
        // La page web (chercher et demander depuis l'iPhone), si Julien l'a allumée.
        let _ = bscreen_macos::barre::PAGE_WEB.set(crate::web::changer);
        let reglages = bscreen_macos::barre::reglages();
        if reglages.page_web {
            crate::web::demarrer(reglages.port_web);
        }
        // Quitter (menu, extinction du Mac) : les vidéos en cours sont terminées proprement.
        cx.on_app_quit(|_| {
            bscreen_macos::moteur::arreter();
            // Le serveur Ollama lancé pour Ask (s'il l'a été par B-Screen) s'arrête avec lui.
            bscreen_macos::ollama::arreter();
            async {}
        })
        .detach();
        frise::lier_touches(cx);
        reglages::lier_touches(cx);
        ask::lier_touches(cx);

        // Le menu et le trackpad parlent hors de gpui : on fait passer leurs demandes par un canal.
        let (envoi, mut reception) = mpsc::unbounded::<Demande>();
        let envoi_menu = envoi.clone();
        let envoi_reglages = envoi.clone();
        let envoi_ask = envoi.clone();
        if let Err(e) = bscreen_macos::app::demarrer_services(
            move || {
                let _ = envoi_menu.unbounded_send(Demande::OuvrirFrise(Instant::now()));
            },
            move || {
                let _ = envoi_reglages.unbounded_send(Demande::OuvrirReglages);
            },
            move || {
                let _ = envoi_ask.unbounded_send(Demande::OuvrirAsk(Instant::now()));
            },
        ) {
            tracing::error!(%e, "services");
        }
        // Raccourcis globaux : ⌥Espace ouvre (ou ferme) la frise, ⌥⇧A Ask, depuis n'importe quelle app.
        raccourci_frise(envoi.clone());
        signaux_d_essai(envoi.clone());

        let pincement = bscreen_macos::gestes::Pincement::ecouter(move |agrandissement| {
            let _ = envoi.unbounded_send(Demande::Pincer(agrandissement));
        });
        // Le pincement est écouté toute la vie de l'app.
        std::mem::forget(pincement);

        // Mode d'essai : `open --env BSCREEN_ESSAI_FRISE=6 B-Screen.app` ouvre la frise 6 s puis la referme.
        if let Some(secondes) = std::env::var("BSCREEN_ESSAI_FRISE")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            tracing::info!(secondes, "essai de la frise");
            frise::ouvrir(cx, Arc::new(SourceMac));
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(secondes))
                    .await;
                let _ = cx.update(|cx| {
                    for fenetre in cx.windows() {
                        let _ = fenetre.update(cx, |_, window, _| window.remove_window());
                    }
                });
                tracing::info!("essai de la frise terminé");
            })
            .detach();
        }

        // Mode d'essai : `BSCREEN_ESSAI_RECHERCHE="bar"` : la frise cherche, montre ses endroits et
        // les photographie (dans BSCREEN_PHOTOS).
        if let Ok(requete) = std::env::var("BSCREEN_ESSAI_RECHERCHE") {
            frise::essai_recherche(cx, Arc::new(SourceMac), requete);
        }
        // Mode d'essai : `open --env BSCREEN_ESSAI_REGLAGES=1 B-Screen.app` ouvre les réglages au lancement.
        // `BSCREEN_ESSAI_ASK="question"` : Ask photographié pendant qu'il répond (dans BSCREEN_PHOTOS).
        if let Ok(question) = std::env::var("BSCREEN_ESSAI_ASK") {
            ask::essai_photos(cx, Arc::new(SourceAskMac), Arc::new(SourceMac), question);
        }
        match std::env::var("BSCREEN_ESSAI_REGLAGES").as_deref() {
            // `BSCREEN_ESSAI_REGLAGES=photos` : chaque partie photographiée dans BSCREEN_PHOTOS.
            Ok("photos") => reglages::essai_photos(cx, Arc::new(SourceReglagesMac)),
            Ok(_) => reglages::ouvrir(cx, Arc::new(SourceReglagesMac)),
            Err(_) => {}
        }

        cx.spawn(async move |cx| {
            while let Some(demande) = reception.next().await {
                let _ = cx.update(|cx| match demande {
                    Demande::OuvrirFrise(quand) => {
                        noter_attente(quand, "frise");
                        frise::basculer(cx, Arc::new(SourceMac));
                    }
                    Demande::OuvrirReglages => reglages::ouvrir(cx, Arc::new(SourceReglagesMac)),
                    Demande::OuvrirAsk(quand) => {
                        noter_attente(quand, "Ask");
                        ask::basculer(cx, Arc::new(SourceAskMac), Arc::new(SourceMac));
                    }
                    Demande::Pincer(agrandissement) => frise::pincer(cx, agrandissement),
                    Demande::BasculerPause => {
                        let pause = !bscreen_macos::barre::reglages().pause;
                        bscreen_macos::barre::changer(bscreen_core::reglages::Changement::Pause(pause));
                    }
                });
            }
        })
        .detach();
    });
    Ok(())
}

/// La recherche de la frise (sur son fil à elle, jamais le fil principal) : mots entiers d'abord,
/// puis débuts de mots, puis le sens ; la frise les regroupe par endroit. Seulement l'écran montré
/// et les voix (« écran » 0), sauf celles entendues pendant une navigation privée : elles restent
/// sous Touch ID, comme le moment.
pub fn chercher_pour_la_frise(requete: &str) -> Vec<Trouvee> {
    let ecran = bscreen_macos::fenetres::ecran_principal();
    let Ok(base) = Base::ouvrir(&chemins::donnees().join("index.sqlite")) else {
        return Vec::new();
    };
    // Les modèles sont attendus 400 ms au plus : s'ils se chargent encore, rien par eux.
    let attente = std::time::Duration::from_millis(400);
    let debut = Instant::now();
    let trouvees = chercher_dans(
        &base,
        requete,
        ecran,
        &|q| bscreen_macos::sens::vecteur_question(q, attente),
        &|q| bscreen_macos::images::vecteur_question(q, attente),
    );
    bscreen_macos::mesures::noter(bscreen_macos::mesures::Geste::Recherche, debut.elapsed().as_millis());
    trouvees
}

/// La recherche de la frise dans une base donnée (la vraie, ou celle du banc d'essai), pour un
/// écran ; `vecteur` et `vecteur_image` donnent les vecteurs d'une question pour le sens et pour
/// les images (`None` : rien par eux).
pub fn chercher_dans(
    base: &Base,
    requete: &str,
    ecran: u32,
    vecteur: &dyn Fn(&str) -> Option<Vec<f32>>,
    vecteur_image: &dyn Fn(&str) -> Option<Vec<f32>>,
) -> Vec<Trouvee> {
    let debut = Instant::now();
    let trouvailles = match base.chercher(requete, 1_500) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(%e, "recherche impossible");
            return Vec::new();
        }
    };
    let exacts_ms = debut.elapsed().as_millis();
    let trouvailles: Vec<_> = trouvailles.into_iter().filter(|t| t.ecran == ecran || t.ecran == 0).collect();
    // Le verrou privé lu une fois pour toutes les voix trouvées (avant : une requête par voix).
    // Illisible : aucune voix n'est montrée.
    let voix = || trouvailles.iter().filter(|t| t.ecran == 0);
    let verrou = match (voix().map(|t| t.debut_ms).min(), voix().map(|t| t.fin_ms.max(t.debut_ms)).max()) {
        (Some(a), Some(z)) => base.verrou_prive(a, z).ok(),
        _ => None,
    };
    let mut trouvees: Vec<Trouvee> = trouvailles
        .into_iter()
        .filter(|t| t.ecran != 0 || verrou.as_ref().is_some_and(|v| !v.pendant(t.debut_ms, t.fin_ms)))
        .map(|t| Trouvee {
            ecran: t.ecran,
            debut_ms: t.debut_ms,
            fin_ms: t.fin_ms,
            cadre: t.cadre,
            vu: t.vu,
            sens: false,
            entier: t.entier,
            texte: Some(t.texte),
        })
        .collect();
    // Par le sens, puis par l'image, l'un après l'autre (la frise les montre dans cet ordre,
    // après les mots exacts) : « voiture rouge » montre la page qui en parle et la photo.
    let par_le_sens = chercher_par_le_sens(base, requete, ecran, vecteur);
    let par_l_image = chercher_par_l_image(base, requete, ecran, vecteur_image);
    let (mut sens, mut images) = (par_le_sens.into_iter(), par_l_image.into_iter());
    loop {
        match (sens.next(), images.next()) {
            (None, None) => break,
            (a, b) => trouvees.extend(a.into_iter().chain(b)),
        }
    }
    tracing::info!(resultats = trouvees.len(), exacts_ms, ms = debut.elapsed().as_millis(), "recherche");
    trouvees
}

/// Similarité minimale pour qu'un texte soit proposé par le sens (réglée sur le banc d'essai).
const SEUIL_SENS: f32 = 0.25;
/// Au plus ce nombre de textes proposés par le sens, d'au moins ce nombre de mots.
const TEXTES_SENS: usize = 8;
const MOTS_MIN_SENS: usize = 4;

/// La recherche par le sens (phase 7) : les textes proches de la question, même sans ses mots.
/// Seulement à partir de 4 lettres ; si le modèle n'est pas prêt (il se charge), rien.
fn chercher_par_le_sens(base: &Base, requete: &str, ecran: u32, vecteur: &dyn Fn(&str) -> Option<Vec<f32>>) -> Vec<Trouvee> {
    if requete.trim().chars().count() < 4 {
        return Vec::new();
    }
    let debut = std::time::Instant::now();
    let Some(question) = vecteur(requete) else {
        return Vec::new();
    };
    let proches = match bscreen_macos::sens::plus_proches(base, &question, 60, SEUIL_SENS) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e, "recherche par le sens impossible");
            return Vec::new();
        }
    };
    // Les mots isolés d'interface (« Confidentialité », « caché »…) sont proches de tout : on ne
    // garde que les textes d'au moins 4 mots, les 8 plus proches.
    let mut gardes: Vec<i64> = Vec::new();
    let mut trouvees: Vec<Trouvee> = Vec::new();
    for t in base.trouvailles_de_textes(&proches, 2, 240).unwrap_or_default() {
        if t.texte.split_whitespace().filter(|m| m.chars().any(char::is_alphanumeric)).count() < MOTS_MIN_SENS {
            continue;
        }
        if !(t.ecran == ecran || t.ecran == 0) || (t.ecran == 0 && base.privee_pendant(t.debut_ms, t.fin_ms).unwrap_or(true)) {
            continue;
        }
        let cle = bscreen_store::empreinte(&t.texte);
        if !gardes.contains(&cle) {
            if gardes.len() >= TEXTES_SENS {
                break;
            }
            gardes.push(cle);
        }
        trouvees.push(Trouvee {
            ecran: t.ecran,
            debut_ms: t.debut_ms,
            fin_ms: t.fin_ms,
            cadre: t.cadre,
            vu: t.vu,
            sens: true,
            entier: false,
            texte: Some(t.texte),
        });
    }
    tracing::info!(textes = gardes.len(), resultats = trouvees.len(), ms = debut.elapsed().as_millis(), "recherche par le sens");
    trouvees
}

/// Au plus ce nombre d'endroits proposés par l'image, d'une ressemblance d'au moins ce cosinus.
const IMAGES_FRISE: usize = 6;
const SEUIL_IMAGES_FRISE: f32 = 0.08;

/// La recherche par l'image (phase 7.3) : les moments dont l'image ressemble à la requête
/// (« voiture rouge »), un par endroit (même app, fenêtre et page dans le quart d'heure : un film
/// en donnerait des dizaines). Seulement l'écran montré, jamais les moments privés.
fn chercher_par_l_image(base: &Base, requete: &str, ecran: u32, vecteur: &dyn Fn(&str) -> Option<Vec<f32>>) -> Vec<Trouvee> {
    if requete.trim().chars().count() < 4 {
        return Vec::new();
    }
    let debut = Instant::now();
    let Some(question) = vecteur(requete) else {
        return Vec::new();
    };
    let proches = match bscreen_macos::images::plus_proches(base, &question, 60, SEUIL_IMAGES_FRISE) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e, "recherche par l'image impossible");
            return Vec::new();
        }
    };
    let ids: Vec<i64> = proches.iter().map(|p| p.0).collect();
    let images: std::collections::HashMap<i64, bscreen_store::Image> =
        base.images_par_id(&ids).unwrap_or_default().into_iter().map(|i| (i.id, i)).collect();
    let mut endroits: Vec<String> = Vec::new();
    let mut trouvees = Vec::new();
    let mut meilleure: Option<f32> = None;
    for (id, cosinus) in &proches {
        let Some(image) = images.get(id).filter(|i| !i.privee && i.ecran == ecran) else { continue };
        // Seulement les images proches de la plus ressemblante (la ressemblance n'a pas d'échelle fixe).
        if *cosinus < *meilleure.get_or_insert(*cosinus) - bscreen_api::mcp::ECART_IMAGES {
            break;
        }
        let endroit = format!(
            "{}|{}|{}",
            image.app_bundle.as_deref().unwrap_or_default(),
            image.url.as_deref().or(image.fenetre.as_deref()).unwrap_or_default(),
            image.quand_ms / (15 * 60_000)
        );
        if endroits.contains(&endroit) {
            continue;
        }
        endroits.push(endroit);
        trouvees.push(Trouvee {
            ecran: image.ecran,
            debut_ms: image.quand_ms,
            fin_ms: image.quand_ms,
            cadre: [0.0; 4],
            vu: true,
            sens: true,
            entier: false,
            texte: Some(format!("{}« {} »", frise::PAR_L_IMAGE, requete.trim())),
        });
        if trouvees.len() >= IMAGES_FRISE {
            break;
        }
    }
    tracing::info!(images = trouvees.len(), ms = debut.elapsed().as_millis(), "recherche par l'image");
    trouvees
}

/// L'archive du homelab, pour les réglages.
fn etat_archive() -> bscreen_ui::reglages::EtatArchive {
    let base = Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite")).ok();
    let (fichiers, octets) = base.as_ref().and_then(|b| b.bilan_archive().ok()).unwrap_or((0, 0));
    let derniere = base
        .as_ref()
        .and_then(|b| b.meta("archive_derniere").ok().flatten())
        .and_then(|v| v.parse::<i64>().ok())
        .map(bscreen_api::heures::lisible);
    bscreen_ui::reglages::EtatArchive {
        fichiers,
        taille: bscreen_macos::barre::taille_lisible(octets),
        derniere,
        robot: bscreen_macos::archive::disponible(),
    }
}

/// Les vraies captures, lues sur ce Mac.
struct SourceMac;

fn maintenant_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl SourceFrise for SourceMac {
    fn moments(&self) -> Vec<Moment> {
        let debut = std::time::Instant::now();
        let ecran = bscreen_macos::fenetres::ecran_principal();
        let base = match Base::ouvrir(&chemins::donnees().join("index.sqlite")) {
            Ok(base) => base,
            Err(e) => {
                tracing::warn!(%e, "base illisible pour la frise");
                return Vec::new();
            }
        };
        let ouverte_ms = debut.elapsed().as_millis();
        let fin = maintenant_ms();
        let images = base.images_ecran_entre(ecran, fin - PERIODE_FRISE_MS, fin);
        if let Ok(images) = &images {
            tracing::info!(ecran, moments = images.len(), base_ms = ouverte_ms, ms = debut.elapsed().as_millis(), "frise ouverte");
        }
        match images {
            Ok(images) => images
                .into_iter()
                .map(|i| Moment {
                    id: i.id,
                    quand_ms: i.quand_ms,
                    fichier: i.fichier,
                    position_ms: i.position_ms,
                    app_bundle: i.app_bundle,
                    app_nom: i.app_nom,
                    largeur: i.largeur,
                    hauteur: i.hauteur,
                    url: i.url,
                    fenetre: i.fenetre,
                    privee: i.privee,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(%e, "moments illisibles");
                Vec::new()
            }
        }
    }

    fn image(&self, moment: &Moment, taille: Option<(u32, u32)>) -> Option<ImageBgra> {
        let fichier = chemins::donnees().join(&moment.fichier);
        let debut = std::time::Instant::now();
        match bscreen_macos::lecture::image_enregistree(&fichier, moment.position_ms, taille) {
            Ok(image) => {
                let ms = debut.elapsed().as_millis();
                if taille.is_none() {
                    bscreen_macos::mesures::noter(bscreen_macos::mesures::Geste::LectureImage, ms);
                    tracing::info!(
                        ms,
                        largeur = image.largeur,
                        hauteur = image.hauteur,
                        "grande image décodée"
                    );
                } else {
                    tracing::debug!(ms, "vignette décodée");
                }
                Some(image)
            }
            Err(e) => {
                tracing::warn!(%e, "image illisible");
                None
            }
        }
    }

    fn icone_app(&self, identifiant: &str) -> Option<ImageBgra> {
        bscreen_macos::lecture::icone_app(identifiant, 64)
    }

    fn preparer_fenetre(&self, vue: *mut c_void) {
        bscreen_macos::fenetres::plein_ecran(vue);
    }

    fn photographier(&self, vue: *mut c_void, nom: &str) {
        SourceReglagesMac.photographier(vue, nom);
    }

    fn quand_ouverte(&self) {
        bscreen_macos::moteur::interface_ouverte(true);
    }

    /// Les modèles de recherche ne se chargent que quand une recherche commence (pas à chaque
    /// ouverture de la frise : ≈ 370 Mo et 1 à 2 s de processeur pour rien quand on ne fait que
    /// remonter le temps).
    fn preparer_recherche(&self) {
        bscreen_macos::sens::prechauffer();
        bscreen_macos::images::prechauffer();
    }

    fn sites_caches(&self) -> Vec<String> {
        bscreen_macos::barre::reglages().sites_caches
    }

    fn quand_fermee(&self) {
        bscreen_macos::audio::arreter_ecoute();
        bscreen_macos::prive::verrouiller();
        bscreen_macos::moteur::interface_ouverte(false);
        // Les images de la frise (≈ 30 Mo chacune en pleine définition) sont libérées juste après ;
        // malloc les garderait pour plus tard : on les rend au système une fois les fils arrêtés.
        let _ = std::thread::Builder::new().name("bscreen-memoire".into()).spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            bscreen_macos::moteur::rendre_la_memoire();
        });
    }

    fn ecouter(&self, passage: &Passage, depuis_s: f64) -> Option<f64> {
        match bscreen_macos::audio::ecouter(&chemins::donnees().join(&passage.fichier), depuis_s) {
            Ok(duree) => Some(duree),
            Err(e) => {
                tracing::warn!(%e, fichier = passage.fichier, "écoute impossible");
                None
            }
        }
    }

    fn arreter_ecoute(&self) {
        bscreen_macos::audio::arreter_ecoute();
    }

    fn deverrouiller(&self, vue: *mut c_void, reponse: Box<dyn FnOnce(bool) + Send>) {
        // Touch ID réussi : la clé des moments privés rangés à part est lue, pour 5 minutes.
        bscreen_macos::touchid::demander(
            vue,
            "voir tes moments en navigation privée",
            Box::new(move |accorde| {
                if accorde {
                    bscreen_macos::prive::deverrouiller();
                }
                reponse(accorde);
            }),
        );
    }

    fn nommer_voix(&self, locuteur_id: i64, nom: &str) -> bool {
        let Ok(base) = Base::ouvrir(&chemins::donnees().join("index.sqlite")) else {
            return false;
        };
        match base.nommer_locuteur(locuteur_id, nom) {
            Ok(garde) => {
                tracing::info!(locuteur_id, garde, "voix nommée");
                true
            }
            Err(e) => {
                tracing::warn!(%e, "voix pas nommée");
                false
            }
        }
    }

    fn chercher(&self, requete: &str) -> Vec<Trouvee> {
        chercher_pour_la_frise(requete)
    }

    fn voix_connues(&self) -> Vec<(i64, String)> {
        let Ok(base) = Base::ouvrir(&chemins::donnees().join("index.sqlite")) else {
            return Vec::new();
        };
        let mut voix: Vec<bscreen_store::Locuteur> = base
            .locuteurs()
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.moi || l.nom.is_some() || l.passages >= 3)
            .collect();
        // La voix de Julien d'abord, puis les plus entendues.
        voix.sort_by(|a, b| b.moi.cmp(&a.moi).then(b.passages.cmp(&a.passages)));
        voix.into_iter()
            .map(|l| {
                let nom = l.nom.unwrap_or_else(|| if l.moi { "Toi".to_string() } else { format!("Voix {}", l.id) });
                (l.id, nom)
            })
            .collect()
    }

    fn corriger_voix(&self, passage: i64, voix: VoixChoisie) -> Option<(Option<i64>, Option<String>)> {
        use bscreen_core::voix_familieres::{depuis_octets, en_octets, nouveau_centre};
        let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).ok()?;
        let empreinte = base.empreinte_du_passage(passage).ok()?;
        let locuteurs = base.locuteurs().ok()?;
        // La voix choisie ; une nouvelle naît de ce passage (son empreinte pour centre).
        let (cible, nouvelle) = match voix {
            VoixChoisie::Aucune => (None, false),
            VoixChoisie::Connue(id) => (Some(id), false),
            VoixChoisie::Nouvelle(nom) => {
                let nom = nom.trim().to_string();
                match locuteurs.iter().find(|l| l.nom.as_deref().is_some_and(|n| n.to_lowercase() == nom.to_lowercase())) {
                    Some(l) => (Some(l.id), false),
                    None => {
                        let centre = empreinte.clone().unwrap_or_else(|| vec![0u8; bscreen_core::voix_familieres::DIMENSION * 4]);
                        let id = base.nouveau_locuteur(&centre, false).ok()?;
                        base.nommer_locuteur(id, &nom).ok()?;
                        (Some(id), true)
                    }
                }
            }
        };
        let ancienne = base.deplacer_passage(passage, cible).ok()?;
        // La voix choisie apprend de ce passage (une correction de Julien est sûre).
        if let (Some(id), Some(e), false) = (cible, empreinte.as_ref(), nouvelle)
            && ancienne != Some(id)
            && let Some(l) = locuteurs.iter().find(|l| l.id == id)
        {
            let centre = nouveau_centre(&depuis_octets(&l.centre), l.passages, &depuis_octets(e));
            if let Err(e) = base.maj_locuteur(id, &en_octets(&centre), l.passages + 1) {
                tracing::warn!(%e, "voix pas mise à jour");
            }
        }
        tracing::info!(passage, ?ancienne, ?cible, "voix d'un passage corrigée");
        let nom = match cible {
            Some(id) => base.locuteurs().ok()?.into_iter().find(|l| l.id == id).map(|l| {
                l.nom.unwrap_or_else(|| if l.moi { "Toi".to_string() } else { format!("Voix {}", l.id) })
            }),
            None => None,
        };
        Some((cible, nom))
    }

    fn voix(&self) -> Vec<Passage> {
        let debut = std::time::Instant::now();
        let Ok(base) = Base::ouvrir(&chemins::donnees().join("index.sqlite")) else {
            return Vec::new();
        };
        let fin = maintenant_ms();
        match base.voix_entre(fin - PERIODE_FRISE_MS, fin) {
            Ok(voix) => {
                // Le verrou privé lu une fois pour toute la semaine : une requête par passage
                // bloquait l'ouverture de la frise jusqu'à 14 s. Illisible : tout reste verrouillé.
                let depuis = voix.iter().map(|v| v.debut_ms).min().unwrap_or(fin);
                let verrou = base.verrou_prive(depuis, fin).ok();
                let passages: Vec<Passage> = voix
                    .into_iter()
                    .map(|v| Passage {
                        id: v.id,
                        debut_ms: v.debut_ms,
                        fin_ms: v.fin_ms,
                        micro: v.source == 0,
                        fichier: v.fichier,
                        locuteur_id: v.locuteur_id,
                        locuteur: v.locuteur,
                        appel: v.appel,
                        privee: verrou.as_ref().is_none_or(|p| p.pendant(v.debut_ms, v.fin_ms)),
                        texte: v.texte,
                    })
                    .collect();
                tracing::info!(passages = passages.len(), ms = debut.elapsed().as_millis(), "voix de la frise");
                passages
            }
            Err(e) => {
                tracing::warn!(%e, "voix illisibles");
                Vec::new()
            }
        }
    }

    fn reperer(&self, moment: &Moment, mots: &[String]) -> Vec<[f64; 4]> {
        let fichier = chemins::donnees().join(&moment.fichier);
        let debut = std::time::Instant::now();
        match bscreen_macos::lecture::reperer_mots(&fichier, moment.position_ms, mots) {
            Ok(cadres) => {
                tracing::info!(
                    ms = debut.elapsed().as_millis(),
                    trouves = cadres.len(),
                    "mots repérés par l'OCR"
                );
                cadres
            }
            Err(e) => {
                tracing::warn!(%e, "OCR impossible");
                Vec::new()
            }
        }
    }

    fn texte_visible(&self, moment: &Moment) -> Vec<String> {
        let ecran = bscreen_macos::fenetres::ecran_principal();
        Base::ouvrir(&chemins::donnees().join("index.sqlite"))
            .and_then(|base| base.texte_visible(ecran, moment.quand_ms))
            .unwrap_or_default()
    }

    fn lire_image(&self, moment: &Moment) -> Vec<bscreen_core::texte_lu::LigneLue> {
        let fichier = chemins::donnees().join(&moment.fichier);
        let debut = std::time::Instant::now();
        match bscreen_macos::lecture::lire_image_enregistree(&fichier, moment.position_ms) {
            Ok(lignes) => {
                tracing::info!(
                    ms = debut.elapsed().as_millis(),
                    lignes = lignes.len(),
                    "image lue mot à mot"
                );
                lignes
            }
            Err(e) => {
                tracing::warn!(%e, "lecture de l'image impossible");
                Vec::new()
            }
        }
    }

    fn marge_haute(&self) -> f32 {
        let marge = bscreen_macos::fenetres::marge_haute_ecran() as f32;
        tracing::info!(marge, "hauteur cachée en haut de l'écran (encoche)");
        marge
    }

    fn supprimer(&self, ids: Vec<i64>) {
        // Le réencodage prend quelques secondes : en arrière-plan.
        let _ = std::thread::Builder::new()
            .name("bscreen-effacement".into())
            .spawn(move || match bscreen_macos::effacement::effacer(&ids) {
                Ok(n) => tracing::info!(images = n, "suppression terminée"),
                Err(e) => tracing::error!(%e, "suppression impossible"),
            });
    }
}

thread_local! {
    /// Le gestionnaire des raccourcis globaux (fil principal) et les deux raccourcis posés.
    static RACCOURCIS: std::cell::RefCell<Option<(global_hotkey::GlobalHotKeyManager, Vec<global_hotkey::hotkey::HotKey>)>> =
        const { std::cell::RefCell::new(None) };
}
static ID_FRISE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static ID_ASK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static ID_PAUSE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// « alt+shift+a » → le raccourci de global_hotkey (une lettre : la touche qui la tape sur ce
/// clavier, AZERTY compris).
fn hotkey_de(texte: &str) -> Option<global_hotkey::hotkey::HotKey> {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    let r = bscreen_core::reglages::lire_raccourci(texte)?;
    let mut m = Modifiers::empty();
    if r.ctrl {
        m |= Modifiers::CONTROL;
    }
    if r.alt {
        m |= Modifiers::ALT;
    }
    if r.maj {
        m |= Modifiers::SHIFT;
    }
    if r.cmd {
        m |= Modifiers::META;
    }
    let code = match r.touche.as_str() {
        "space" => Code::Space,
        f if f.starts_with('f') && f.len() > 1 => match f[1..].parse::<u8>().ok()? {
            1 => Code::F1,
            2 => Code::F2,
            3 => Code::F3,
            4 => Code::F4,
            5 => Code::F5,
            6 => Code::F6,
            7 => Code::F7,
            8 => Code::F8,
            9 => Code::F9,
            10 => Code::F10,
            11 => Code::F11,
            12 => Code::F12,
            _ => return None,
        },
        lettre => {
            let c = lettre.chars().next()?;
            match c {
                '0'..='9' => [Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9]
                    [c as usize - '0' as usize],
                _ => bscreen_macos::clavier::touche_de(c).and_then(code_de_la_touche)?,
            }
        }
    };
    Some(HotKey::new(Some(m), code))
}

/// Pose (ou repose) les raccourcis globaux de la frise et d'Ask (fil principal).
fn poser_raccourcis(frise: &str, ask: &str, pause: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    RACCOURCIS.with(|r| {
        let mut r = r.borrow_mut();
        if r.is_none() {
            match global_hotkey::GlobalHotKeyManager::new() {
                Ok(g) => *r = Some((g, Vec::new())),
                Err(e) => return tracing::warn!(%e, "raccourcis globaux indisponibles"),
            }
        }
        let Some((gestionnaire, poses)) = r.as_mut() else { return };
        for ancien in poses.drain(..) {
            let _ = gestionnaire.unregister(ancien);
        }
        ID_PAUSE.store(0, Relaxed);
        for (texte, id, nom) in [(frise, &ID_FRISE, "frise"), (ask, &ID_ASK, "Ask"), (pause, &ID_PAUSE, "pause")] {
            if texte.is_empty() {
                continue;
            }
            let lisible = bscreen_core::reglages::raccourci_lisible(texte);
            let Some(raccourci) = hotkey_de(texte) else {
                tracing::warn!(raccourci = texte, "raccourci illisible");
                continue;
            };
            match gestionnaire.register(raccourci) {
                Ok(()) => {
                    id.store(raccourci.id(), Relaxed);
                    poses.push(raccourci);
                    tracing::info!(raccourci = lisible, quoi = nom, "raccourci actif");
                }
                Err(e) => tracing::warn!(%e, raccourci = lisible, quoi = nom, "raccourci déjà pris par une autre app"),
            }
        }
    });
}

fn raccourci_frise(envoi: mpsc::UnboundedSender<Demande>) {
    use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
    use std::sync::atomic::Ordering::Relaxed;
    let reglages = bscreen_macos::barre::reglages();
    poser_raccourcis(&reglages.raccourci_frise, &reglages.raccourci_ask, &reglages.raccourci_pause);
    let _ = bscreen_macos::barre::RACCOURCIS.set(poser_raccourcis);
    GlobalHotKeyEvent::set_event_handler(Some(move |evenement: GlobalHotKeyEvent| {
        if evenement.state() != HotKeyState::Pressed {
            return;
        }
        if evenement.id() == ID_FRISE.load(Relaxed) {
            let _ = envoi.unbounded_send(Demande::OuvrirFrise(Instant::now()));
        } else if evenement.id() == ID_ASK.load(Relaxed) {
            let _ = envoi.unbounded_send(Demande::OuvrirAsk(Instant::now()));
        } else if evenement.id() != 0 && evenement.id() == ID_PAUSE.load(Relaxed) {
            let _ = envoi.unbounded_send(Demande::BasculerPause);
        }
    }));
}

/// La touche (position QWERTY des raccourcis) d'un code de touche macOS (kVK_ANSI_…), pour les lettres.
fn code_de_la_touche(touche: u16) -> Option<global_hotkey::hotkey::Code> {
    use global_hotkey::hotkey::Code;
    Some(match touche {
        0x00 => Code::KeyA,
        0x01 => Code::KeyS,
        0x02 => Code::KeyD,
        0x03 => Code::KeyF,
        0x04 => Code::KeyH,
        0x05 => Code::KeyG,
        0x06 => Code::KeyZ,
        0x07 => Code::KeyX,
        0x08 => Code::KeyC,
        0x09 => Code::KeyV,
        0x0B => Code::KeyB,
        0x0C => Code::KeyQ,
        0x0D => Code::KeyW,
        0x0E => Code::KeyE,
        0x0F => Code::KeyR,
        0x10 => Code::KeyY,
        0x11 => Code::KeyT,
        0x1F => Code::KeyO,
        0x20 => Code::KeyU,
        0x22 => Code::KeyI,
        0x23 => Code::KeyP,
        0x25 => Code::KeyL,
        0x26 => Code::KeyJ,
        0x28 => Code::KeyK,
        0x2D => Code::KeyN,
        0x2E => Code::KeyM,
        _ => return None,
    })
}

/// Une demande d'ouverture qui a attendu (l'interface était occupée) : on le note.
fn noter_attente(quand: Instant, quoi: &str) {
    let ms = quand.elapsed().as_millis();
    if ms >= 50 {
        tracing::warn!(ms, quoi, "ouverture demandée pendant que l'interface était occupée");
    }
}

/// Essai à chaud, dans l'app qui tourne déjà : `kill -USR1 <pid>` ouvre (ou ferme) la frise et
/// `kill -USR2 <pid>` Ask, comme leurs raccourcis, pour mesurer leur ouverture.
fn signaux_d_essai(envoi: mpsc::UnboundedSender<Demande>) {
    use signal_hook::consts::{SIGUSR1, SIGUSR2};
    let Ok(mut signaux) = signal_hook::iterator::Signals::new([SIGUSR1, SIGUSR2]) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("bscreen-essais".into())
        .spawn(move || {
            for signal in signaux.forever() {
                let demande = if signal == SIGUSR1 {
                    Demande::OuvrirFrise(Instant::now())
                } else {
                    Demande::OuvrirAsk(Instant::now())
                };
                if envoi.unbounded_send(demande).is_err() {
                    return;
                }
            }
        });
}

/// Un arrêt demandé par le système (réinstallation, `kill`) termine aussi proprement les vidéos.
fn arret_propre_sur_signal() {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let Ok(mut signaux) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT]) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("bscreen-signaux".into())
        .spawn(move || {
            if let Some(signal) = signaux.forever().next() {
                tracing::info!(signal, "arrêt demandé par le système");
                bscreen_macos::moteur::arreter();
                bscreen_macos::ollama::arreter();
                std::process::exit(0);
            }
        });
}

/// Ce dont la fenêtre de réglages a besoin, pris dans la couche macOS.
struct SourceReglagesMac;

impl SourceReglages for SourceReglagesMac {
    fn etat(&self) -> EtatReglages {
        use bscreen_macos::barre::taille_lisible;
        use bscreen_macos::connexion::EtatConnexion;
        use bscreen_macos::moteur;
        use std::sync::atomic::Ordering::Relaxed;

        let reglages = bscreen_macos::barre::reglages();
        let connexion = bscreen_macos::connexion::etat();
        let app = |identifiant: &str| AppConnue {
            identifiant: identifiant.to_string(),
            nom: bscreen_macos::lecture::nom_app(identifiant).unwrap_or_else(|| identifiant.to_string()),
        };
        let apps_exclues = reglages.apps_exclues.iter().map(|a| app(a)).collect();
        // Les apps toujours exclues : seulement celles installées sur ce Mac (et pas B-Screen).
        let apps_toujours_exclues = bscreen_macos::capture::APPS_EXCLUES_FIXES
            .iter()
            .filter(|a| **a != bscreen_core::IDENTIFIANT)
            .filter_map(|a| {
                bscreen_macos::lecture::nom_app(a).map(|nom| AppConnue {
                    identifiant: a.to_string(),
                    nom,
                })
            })
            .collect();
        let personnes = Base::ouvrir(&chemins::donnees().join("index.sqlite"))
            .and_then(|base| {
                let locuteurs = base.locuteurs()?;
                Ok(locuteurs
                    .into_iter()
                    // Les voix qui comptent : la tienne, celles nommées, celles entendues 3 fois ou plus.
                    .filter(|l| l.moi || l.nom.is_some() || l.passages >= 3)
                    .map(|l| Personne {
                        id: l.id,
                        nommee: l.nom.is_some(),
                        nom: l.nom.clone().unwrap_or_else(|| {
                            if l.moi {
                                "Toi".to_string()
                            } else {
                                format!("Voix {}", l.id)
                            }
                        }),
                        passages: l.passages,
                        ecoutable: base.passage_exemple(l.id).ok().flatten().is_some(),
                    })
                    .collect())
            })
            .unwrap_or_default();
        let cpu = moteur::CPU_POUR_MILLE.load(Relaxed);
        let binaire = std::env::current_exe()
            .map(|c| c.display().to_string())
            .unwrap_or_else(|_| "~/Applications/B-Screen.app/Contents/MacOS/bscreen".to_string());
        EtatReglages {
            connexion: match connexion {
                EtatConnexion::Indisponible => None,
                EtatConnexion::Active | EtatConnexion::AApprouver => Some(true),
                EtatConnexion::Inactive => Some(false),
            },
            connexion_a_approuver: connexion == EtatConnexion::AApprouver,
            apps_exclues,
            apps_toujours_exclues,
            personnes,
            video: taille_lisible(moteur::PLACE_VIDEO.load(Relaxed)),
            voix: taille_lisible(moteur::PLACE_VOIX.load(Relaxed)),
            base: taille_lisible(moteur::PLACE_BASE.load(Relaxed)),
            libre: moteur::place_libre().map(taille_lisible).unwrap_or_else(|| "?".to_string()),
            processeur: if cpu == u64::MAX {
                "mesure en cours".to_string()
            } else {
                format!("{},{} %", cpu / 10, cpu % 10)
            },
            memoire: taille_lisible(moteur::MEMOIRE.load(Relaxed)),
            chiffre: bscreen_macos::coffre::est_ouverte(),
            sens: bscreen_macos::sens::etat_index(),
            images: bscreen_macos::images::etat_index(),
            archive: etat_archive(),
            sites_frequents: Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))
                .and_then(|base| base.adresses_depuis(chrono::Local::now().timestamp_millis() - 30 * 86_400_000))
                .map(|adresses| {
                    let mut par_site: HashMap<String, u64> = HashMap::new();
                    for (url, n) in adresses {
                        if let Some(d) = bscreen_core::reglages::domaine(&url) {
                            *par_site.entry(d).or_default() += n as u64;
                        }
                    }
                    let mut liste: Vec<(String, u64)> = par_site.into_iter().collect();
                    liste.sort_by(|a, b| b.1.cmp(&a.1));
                    liste.truncate(12);
                    liste
                })
                .unwrap_or_default(),
            corrections: Base::ouvrir(&chemins::donnees().join("index.sqlite"))
                .and_then(|base| base.corrections(200))
                .map(|liste| {
                    liste
                        .into_iter()
                        .map(|c| bscreen_ui::reglages::CorrectionRetenue {
                            id: c.id,
                            quand: bscreen_api::heures::lisible(c.cree_ms),
                            texte: c.texte,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            autorisations: bscreen_macos::autorisations::Autorisation::TOUTES
                .iter()
                .map(|a| {
                    let etat = a.etat();
                    (
                        a.nom().to_string(),
                        a.raison().to_string(),
                        etat.accordee(),
                        etat == bscreen_macos::autorisations::Etat::Refusee,
                    )
                })
                .collect(),
            version: format!("version {}", bscreen_core::VERSION),
            commande_mcp: format!("claude mcp add b-screen -- {binaire} mcp"),
            reglages,
        }
    }

    fn changer(&self, changement: Changement) -> bool {
        bscreen_macos::barre::changer(changement)
    }

    fn changer_connexion(&self, active: bool) {
        bscreen_macos::barre::changer_connexion(active);
        bscreen_macos::barre::rafraichir();
    }

    fn apps_ouvertes(&self) -> Vec<AppConnue> {
        bscreen_macos::moteur::apps_ouvertes()
            .into_iter()
            .filter(|(identifiant, _)| !bscreen_macos::capture::est_exclue(identifiant))
            .map(|(identifiant, nom)| AppConnue { identifiant, nom })
            .collect()
    }

    fn icone_app(&self, identifiant: &str) -> Option<ImageBgra> {
        bscreen_macos::lecture::icone_app(identifiant, 64)
    }

    fn vocabulaire_auto(&self) -> Vec<String> {
        let Ok(base) = Base::ouvrir(&chemins::donnees().join("index.sqlite")) else {
            return Vec::new();
        };
        let depuis = maintenant_ms() - 7 * 86_400_000;
        let textes = base.textes_vus_depuis(depuis, 20_000).unwrap_or_default();
        // Mêmes règles que la transcription : vu au moins 5 fois, 50 au plus, sans les mots de Julien.
        let siens = bscreen_macos::barre::reglages().vocabulaire;
        bscreen_core::vocabulaire::noms_propres(&textes, 5, 50)
            .into_iter()
            .filter(|m| !siens.iter().any(|s| s.eq_ignore_ascii_case(m)))
            .collect()
    }

    fn nommer(&self, personne: i64, nom: &str) -> bool {
        SourceMac.nommer_voix(personne, nom)
    }

    fn autorisation(&self, rang: usize) {
        use bscreen_macos::autorisations::{Autorisation, Etat};
        let Some(a) = Autorisation::TOUTES.get(rang) else { return };
        match a.etat() {
            Etat::ADemander => a.demander(),
            _ => a.ouvrir_reglages(),
        }
        bscreen_macos::barre::rafraichir();
    }

    fn oublier_correction(&self, id: i64) -> bool {
        Base::ouvrir(&chemins::donnees().join("index.sqlite"))
            .and_then(|base| base.oublier_correction(id))
            .unwrap_or(false)
    }

    fn ecouter(&self, personne: i64) -> Option<f64> {
        let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).ok()?;
        let (fichier, _, _) = base.passage_exemple(personne).ok()??;
        match bscreen_macos::audio::ecouter(&chemins::donnees().join(&fichier), 0.0) {
            Ok(duree) => Some(duree),
            Err(e) => {
                tracing::warn!(%e, fichier, "écoute impossible");
                None
            }
        }
    }

    fn arreter_ecoute(&self) {
        bscreen_macos::audio::arreter_ecoute();
    }

    fn copier_jeton(&self) -> bool {
        bscreen_macos::api_locale::copier_jeton()
    }

    fn ouvrir_page_web(&self) -> bool {
        let port = bscreen_macos::barre::reglages().port_web;
        let Some(adresse) = crate::web::adresse_locale(port) else { return false };
        std::process::Command::new("/usr/bin/open").arg(adresse).status().is_ok_and(|s| s.success())
    }

    fn effacer(&self, rang: usize) {
        bscreen_macos::barre::demander_effacement(rang);
    }

    fn photographier(&self, vue: *mut c_void, nom: &str) {
        let dossier = std::env::var_os("BSCREEN_PHOTOS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("bscreen-photos"));
        let _ = std::fs::create_dir_all(&dossier);
        let fichier = dossier.join(format!("{nom}.png"));
        let fait = bscreen_macos::fenetres::photographier(vue, &fichier);
        tracing::info!(fait, fichier = %fichier.display(), "photo de fenêtre (essai)");
    }
}

/// Ce dont Ask a besoin : Claude (par Claude Code) et les moments cités.
struct SourceAskMac;

impl SourceAsk for SourceAskMac {
    fn commencer_dictee(&self) -> bool {
        match bscreen_macos::dictee::commencer() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(%e, "Ask : micro indisponible");
                false
            }
        }
    }

    fn finir_dictee(&self) -> Option<String> {
        bscreen_macos::dictee::finir().map_err(|e| tracing::warn!(%e, "Ask : question pas transcrite")).ok()
    }

    fn lire(&self, texte: &str) {
        bscreen_macos::dictee::lire(texte);
    }

    fn arreter_lecture(&self) {
        bscreen_macos::dictee::arreter_lecture();
    }

    fn modele(&self) -> String {
        bscreen_macos::barre::reglages().ask_modele
    }

    fn changer_modele(&self, modele: &str) {
        bscreen_macos::barre::changer(bscreen_core::reglages::Changement::ModeleAsk(modele.to_string()));
    }

    fn preparer(&self) {
        // Pendant que Julien tape : le modèle de sens se charge (le contexte sera prêt tout de
        // suite) et Claude démarre et se branche sur la mémoire.
        bscreen_macos::sens::prechauffer();
        let _ = std::thread::Builder::new()
            .name("bscreen-ask-prepare".into())
            .spawn(crate::ask::preparer);
    }

    fn oublier(&self) {
        // Tout de suite (pas dans un fil) : un « préparer » qui suit trouve la place libre.
        crate::ask::oublier();
    }

    fn photographier(&self, vue: *mut c_void, nom: &str) {
        SourceReglagesMac.photographier(vue, nom);
    }

    fn demander(
        &self,
        question: &str,
        precedent: Option<(&str, &str)>,
        envoi: Box<dyn Fn(ask::Evenement) + Send>,
    ) -> Option<ask::Arret> {
        match crate::ask::demander(question, precedent, envoi) {
            Ok(arret) => Some(arret),
            Err(e) => {
                tracing::warn!(%e, "Ask : question pas posée");
                None
            }
        }
    }

    fn moment(&self, quand_ms: i64, audio: bool) -> Option<ask::MomentCite> {
        let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).ok()?;
        // Un renvoi est à la minute près : on cherche dans cette minute.
        let milieu = quand_ms + 30_000;
        if audio {
            let voix = base.voix_entre(quand_ms - 60_000, quand_ms + 120_000).ok()?;
            let v = voix
                .into_iter()
                .filter(|v| !base.privee_pendant(v.debut_ms, v.fin_ms).unwrap_or(true))
                .min_by_key(|v| (v.debut_ms - milieu).abs())?;
            let texte = v.texte.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| "Passage de voix".to_string());
            return Some(ask::MomentCite {
                quand_ms: v.debut_ms,
                audio: true,
                qui: v.locuteur.clone().unwrap_or_else(|| if v.source == 0 { "Toi".into() } else { "Mac".into() }),
                titre: texte.chars().take(80).collect(),
                vignette: None,
            });
        }
        let ecran = bscreen_macos::fenetres::ecran_principal();
        let image = base.image_proche(milieu, ecran).ok()??;
        if image.privee || (image.quand_ms - milieu).abs() > 5 * 60_000 {
            return None;
        }
        let hauteur = (440 * image.hauteur / image.largeur.max(1)).max(1);
        let vignette = bscreen_macos::lecture::image_enregistree(
            &chemins::donnees().join(&image.fichier),
            image.position_ms,
            Some((440, hauteur)),
        )
        .ok();
        Some(ask::MomentCite {
            quand_ms: image.quand_ms,
            audio: false,
            qui: image.app_nom.clone().unwrap_or_default(),
            titre: image
                .fenetre
                .clone()
                .filter(|f| !f.is_empty())
                .or_else(|| image.app_nom.clone())
                .unwrap_or_else(|| "Écran".to_string()),
            vignette,
        })
    }
}
