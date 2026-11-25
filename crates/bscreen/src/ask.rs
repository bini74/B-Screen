//! Ask : la question part à Claude (Claude Code, abonnement de Julien — son choix du 20/11), qui
//! lit la mémoire de B-Screen par le serveur MCP en lecture seule (ce binaire lui-même :
//! `bscreen mcp`). Aucun autre outil : ni fichiers, ni commandes, ni les autres serveurs MCP de
//! Julien. Tout ce que la mémoire rend à Claude (ce qui part du Mac) est montré dans le panneau.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use bscreen_store::Base;
use bscreen_ui::ask::{Arret, Evenement};
use chrono::{Datelike, Local};

/// Les outils de la mémoire que Claude peut appeler (tous en lecture seule).
const OUTILS: [&str; 8] =
    ["chercher", "chercher_par_le_sens", "chercher_dans_les_images", "activite", "voix", "echanges", "recherches", "ecran"];
const PREFIXE: &str = "mcp__b-screen__";

const CONSIGNES: &str = "Tu es Ask, l'assistant de B-Screen : la mémoire de l'écran et des voix de Julien, sur son Mac. \
Tu réponds à sa question en t'appuyant seulement sur les outils b-screen, en lecture seule : chercher (le texte vu à \
l'écran ou dit, par ses mots), chercher_par_le_sens (par l'idée, quand les mots exacts ne sont pas connus), activite \
(les apps, fenêtres et pages d'une période), voix (ce qui a été dit, et par qui), echanges (avec une personne : la voix \
des deux côtés et les messages écrits, avec qui a écrit quoi), recherches (ce que Julien a tapé dans \
Google, YouTube…), ecran (le texte affiché à un instant), chercher_dans_les_images (ce qui se voyait sans être écrit : \
une photo, un objet, une couleur). Dans les outils, la voix de Julien s'appelle « Toi » \
(personne : « toi ») ; « vidéo (son du Mac) » : le son d'une vidéo ou d'un film, pas une conversation de Julien ; \
« (appel) » : la personne au bout du fil d'un appel sur le Mac ; une voix nommée au micro (« Camille ») parlait près \
de Julien ou au téléphone en haut-parleur ; « voix pas reconnue » : pas sûr de qui parlait.

Avant la question, B-Screen te donne ce qu'il a déjà trouvé sur le Mac (période comprise, activité, voix, images qui \
ressemblent à la question, textes proches par le sens, mots exacts). Réponds directement à partir de ce contexte ; n'appelle les outils que s'il manque \
quelque chose d'important (un seul appel bien choisi vaut mieux que plusieurs).

Règles :
- Réponds en français, court et précis : quelques phrases, ou une petite liste avec des tirets.
- Cite chaque fait avec son moment, au format [écran JJ/MM HH:MM] ou [audio JJ/MM HH:MM], avec l'heure donnée par les outils.
- Si rien ne répond à la question, dis-le en une phrase et propose une autre piste.
- Si Julien te corrige (« non, c'était mardi »), commence ta réponse par une ligne « Retenu : … » : le fait corrigé en \
une phrase qui se comprend seule, avec la date (« Retenu : le dîner avec Camille était le mardi 17/11, pas le lundi \
16/11. »). B-Screen le gardera pour les prochaines questions. Pas de « Retenu » s'il ne te corrige pas.
- Les « Corrections de Julien » du contexte priment sur tout le reste.
- N'invente rien. Pas de titre, pas de tableau.";

/// Le choix « Sur le Mac » : le modèle local (Ollama), sans Claude.
pub const LOCAL: &str = "local";

const CONSIGNES_LOCALES: &str = "Tu es Ask, l'assistant de B-Screen : la mémoire de l'écran et des voix de Julien, \
sur son Mac. B-Screen te donne ce qu'il a trouvé sur le Mac pour la question : période comprise, résumés des épisodes, \
activité à l'écran, ce qui a été dit, images qui ressemblent à la question, textes proches par le sens. Tu n'as rien \
d'autre : réponds seulement à partir \
de ce contexte. La voix de Julien s'appelle « Toi » ; « vidéo (son du Mac) » : le son d'une vidéo ou d'un film, pas \
une conversation de Julien ; « (appel) » : la personne au bout du fil d'un appel sur le Mac ; une voix nommée au \
micro (« Camille ») parlait près de Julien ou au téléphone en haut-parleur ; « voix pas reconnue » : pas sûr de qui \
parlait.

Règles :
- Réponds en français, court et précis : quelques phrases, ou une petite liste avec des tirets.
- Cite chaque fait avec son moment, au format [écran JJ/MM HH:MM] ou [audio JJ/MM HH:MM], avec l'heure du contexte.
- Si le contexte ne répond pas à la question, dis-le en une phrase.
- Si Julien te corrige (« non, c'était mardi »), commence ta réponse par une ligne « Retenu : … » : le fait corrigé en \
une phrase qui se comprend seule, avec la date (« Retenu : le dîner avec Camille était le mardi 17/11, pas le lundi \
16/11. »). B-Screen le gardera pour les prochaines questions. Pas de « Retenu » s'il ne te corrige pas.
- Les « Corrections de Julien » du contexte priment sur tout le reste.
- N'invente rien. Pas de titre, pas de tableau.";

/// Le modèle choisi pour Ask (`BSCREEN_ASK_MODELE` le remplace, pour les essais).
fn modele_ask() -> String {
    std::env::var("BSCREEN_ASK_MODELE")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| bscreen_core::reglages::Reglages::charger().ask_modele)
}

/// Où est Claude Code ? (une app lancée par le Finder n'a pas le PATH du terminal)
pub fn claude() -> Option<PathBuf> {
    let maison = std::env::var_os("HOME").map(PathBuf::from)?;
    [
        maison.join(".local/bin/claude"),
        maison.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

const JOURS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];

/// Claude Code lancé d'avance (à l'ouverture du panneau) : il démarre et se branche sur la mémoire
/// pendant que Julien tape, puis attend la question sur son entrée. Il garde la conversation : une
/// question de suite y continue.
struct ClaudePret {
    modele: String,
    fils: Child,
    entree: std::process::ChildStdin,
    lignes: std::sync::mpsc::Receiver<String>,
    /// Questions déjà posées dans cette conversation.
    questions: usize,
}

static PRET: Mutex<Option<ClaudePret>> = Mutex::new(None);

fn lancer_claude(modele: &str) -> Result<ClaudePret> {
    let claude = claude().ok_or_else(|| anyhow!("Claude Code introuvable"))?;
    // Un dossier hors du dossier personnel : aucun CLAUDE.md de Julien n'y est lu.
    let dossier = std::env::temp_dir().join("bscreen-ask");
    std::fs::create_dir_all(&dossier)?;
    let binaire = std::env::current_exe().context("chemin de B-Screen")?;
    let config = serde_json::json!({
        "mcpServers": { "b-screen": { "command": binaire, "args": ["mcp"] } }
    });
    let fichier_config = dossier.join("mcp.json");
    std::fs::write(&fichier_config, serde_json::to_vec(&config)?)?;
    let permis: Vec<String> = OUTILS.iter().map(|o| format!("{PREFIXE}{o}")).collect();
    let mut fils = Command::new(&claude)
        .arg("-p")
        .args(["--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .arg("--include-partial-messages")
        .args(["--model", modele])
        .arg("--no-session-persistence")
        .args(["--system-prompt", CONSIGNES])
        // Aucun outil intégré (fichiers, commandes, web) ; seulement la mémoire, et seulement elle.
        .args(["--tools", ""])
        .arg("--mcp-config")
        .arg(&fichier_config)
        .arg("--strict-mcp-config")
        .arg("--allowedTools")
        .arg(permis.join(","))
        .current_dir(&dossier)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("lancement de {}", claude.display()))?;
    let entree = fils.stdin.take().ok_or_else(|| anyhow!("entrée de Claude"))?;
    let sortie = fils.stdout.take().ok_or_else(|| anyhow!("sortie de Claude"))?;
    let (envoi, lignes) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("bscreen-ask-lecture".into()).spawn(move || {
        for ligne in BufReader::new(sortie).lines() {
            let Ok(ligne) = ligne else { break };
            if envoi.send(ligne).is_err() {
                break;
            }
        }
    });
    Ok(ClaudePret {
        modele: modele.to_string(),
        fils,
        entree,
        lignes,
        questions: 0,
    })
}

/// Le panneau s'ouvre : Claude démarre (ou le modèle local se charge) pendant que Julien tape.
pub fn preparer() {
    bscreen_macos::images::prechauffer();
    let modele = modele_ask();
    if modele == LOCAL {
        oublier();
        let _ = std::thread::Builder::new().name("bscreen-ask-local".into()).spawn(|| {
            let debut = Instant::now();
            match bscreen_macos::ollama::prechauffer() {
                Ok(()) => tracing::info!(ms = debut.elapsed().as_millis(), "Ask : modèle local prêt"),
                Err(e) => tracing::warn!(%e, "Ask : modèle local pas prêt"),
            }
        });
        return;
    }
    let mut pret = PRET.lock().unwrap_or_else(|e| e.into_inner());
    if pret.as_ref().is_some_and(|p| p.modele == modele) {
        return;
    }
    if let Some(mut ancien) = pret.take() {
        let _ = ancien.fils.kill();
        let _ = ancien.fils.wait();
    }
    match lancer_claude(&modele) {
        Ok(p) => *pret = Some(p),
        Err(e) => tracing::warn!(%e, "Ask : Claude pas lancé d'avance"),
    }
}

/// Le panneau se ferme (ou ⌘N) : la conversation s'arrête.
pub fn oublier() {
    if let Some(mut ancien) = PRET.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = ancien.fils.kill();
        let _ = ancien.fils.wait();
    }
}

/// Pose la question. B-Screen prépare d'abord le contexte sur le Mac (période, activité, voix,
/// textes proches par le sens, noms cités) : Claude répond le plus souvent d'un seul tour, au fil de
/// l'eau. Les événements arrivent par `envoi` ; l'arrêt arrête Claude s'il répond encore.
pub fn demander(question: &str, precedent: Option<(&str, &str)>, envoi: Box<dyn Fn(Evenement) + Send>) -> Result<Arret> {
    // « Retenu : … » n'est gardé que pour une question de suite qui corrige ; ailleurs (le modèle
    // local en écrit parfois sans raison), la ligne est retirée de la réponse montrée.
    let envoi = if precedent.is_some() && bscreen_api::corrections::est_une_correction(question) {
        retenir_les_corrections(question, envoi)
    } else {
        sans_retenu(envoi)
    };
    let modele = modele_ask();
    if modele == LOCAL {
        return Ok(demander_au_mac(question, precedent, envoi));
    }
    claude().ok_or_else(|| anyhow!("Claude Code introuvable"))?;
    let date = date_du_jour();
    let precedent = precedent.map(|(q, r)| (q.to_string(), r.to_string()));
    let question = question.to_string();
    let annule = Arc::new(std::sync::atomic::AtomicBool::new(false));
    tracing::info!(caracteres = question.chars().count(), modele, "question posée à Claude (Ask)");

    let annule_fil = Arc::clone(&annule);
    let _ = std::thread::Builder::new().name("bscreen-ask".into()).spawn(move || {
        let debut = Instant::now();
        let fini = |erreur: Option<String>| Evenement::Fini {
            erreur,
            secondes: debut.elapsed().as_secs_f64(),
        };
        // 1. Le contexte, préparé ici (le modèle de sens est déjà chargé : préchauffé à l'ouverture).
        envoi(Evenement::Lecture {
            outil: "contexte".into(),
            quoi: "sur le Mac".into(),
        });
        let contexte = preparer_contexte(&question);
        tracing::info!(ms = debut.elapsed().as_millis(), caracteres = contexte.chars().count(), "contexte d'Ask prêt");
        envoi(Evenement::Envoye {
            outil: "contexte".into(),
            contenu: contexte.clone(),
        });
        // 2. Claude, déjà lancé à l'ouverture du panneau (sinon maintenant).
        let pris = PRET
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .filter(|p| p.modele == modele);
        let mut claude = match pris.map(Ok).unwrap_or_else(|| lancer_claude(&modele)) {
            Ok(c) => c,
            Err(e) => {
                envoi(fini(Some(format!("Claude Code ne démarre pas : {e}"))));
                return;
            }
        };
        // La conversation de ce Claude connaît déjà l'échange précédent : inutile de le répéter.
        let invite = match &precedent {
            Some((avant, reponse)) if claude.questions == 0 => format!(
                "{date}\n\nQuestion précédente de Julien : {avant}\nTa réponse : {reponse}\n\n\
                 Ce que B-Screen a trouvé sur le Mac pour la nouvelle question :\n{contexte}\n\n\
                 Nouvelle question (elle peut préciser ou corriger la précédente) : {question}"
            ),
            Some(_) => format!(
                "{date}\n\nCe que B-Screen a trouvé sur le Mac pour la nouvelle question :\n{contexte}\n\n\
                 Nouvelle question (elle peut préciser ou corriger la précédente) : {question}"
            ),
            None => format!(
                "{date}\n\nCe que B-Screen a trouvé sur le Mac pour cette question :\n{contexte}\n\nQuestion de Julien : {question}"
            ),
        };
        if annule_fil.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = claude.fils.kill();
            return;
        }
        let message = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": invite }] }
        });
        use std::io::Write;
        if writeln!(claude.entree, "{message}").and_then(|_| claude.entree.flush()).is_err() {
            envoi(fini(Some("Claude s'est arrêté avant la question".to_string())));
            return;
        }
        claude.questions += 1;
        // 3. La réponse, au fil de l'eau, jusqu'au résultat de ce tour.
        let mut etat = Lecteur::default();
        let mut fin_recue = false;
        while !fin_recue {
            if annule_fil.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = claude.fils.kill();
                let _ = claude.fils.wait();
                return;
            }
            let ligne = match claude.lignes.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(l) => l,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let Ok(evenement) = serde_json::from_str::<serde_json::Value>(&ligne) else {
                continue;
            };
            for e in etat.traduire(&evenement) {
                if let Evenement::Fini { erreur, .. } = e {
                    fin_recue = true;
                    // La durée vue par Julien : depuis la question, contexte compris.
                    envoi(fini(erreur));
                } else {
                    envoi(e);
                }
            }
        }
        if fin_recue {
            // Gardé pour une question de suite (la conversation continue), si rien d'autre n'a été lancé.
            let mut pret = PRET.lock().unwrap_or_else(|e| e.into_inner());
            if pret.is_none() && !annule_fil.load(std::sync::atomic::Ordering::SeqCst) {
                *pret = Some(claude);
            } else {
                let _ = claude.fils.kill();
                let _ = claude.fils.wait();
            }
        } else {
            let code = claude.fils.wait().ok();
            envoi(fini(Some(match code {
                Some(c) if !c.success() => format!("Claude s'est arrêté ({c})"),
                _ => "Claude n'a pas répondu".to_string(),
            })));
        }
        tracing::info!(secondes = debut.elapsed().as_secs(), "réponse de Claude (Ask)");
    });
    Ok(Arret(Box::new(move || {
        annule.store(true, std::sync::atomic::Ordering::SeqCst);
    })))
}

/// Enveloppe l'envoi des événements : la réponse est gardée au fil de l'eau ; à la fin, ses
/// lignes « Retenu : … » (Julien a corrigé Ask) sont rangées dans la base, pour les questions
/// suivantes.
/// La réponse sans ses lignes « Retenu : … », au fil.
fn sans_retenu(envoi: Box<dyn Fn(Evenement) + Send>) -> Box<dyn Fn(Evenement) + Send> {
    let filtre = Mutex::new(bscreen_api::corrections::SansRetenu::default());
    Box::new(move |e| {
        match &e {
            Evenement::Morceau(texte) => {
                let montre = filtre.lock().unwrap_or_else(|e| e.into_inner()).passer(texte);
                if !montre.is_empty() {
                    envoi(Evenement::Morceau(montre));
                }
                return;
            }
            Evenement::Fini { .. } => {
                let reste = filtre.lock().unwrap_or_else(|e| e.into_inner()).finir();
                if !reste.is_empty() {
                    envoi(Evenement::Morceau(reste));
                }
            }
            _ => {}
        }
        envoi(e)
    })
}

fn retenir_les_corrections(question: &str, envoi: Box<dyn Fn(Evenement) + Send>) -> Box<dyn Fn(Evenement) + Send> {
    let reponse = std::sync::Mutex::new(String::new());
    let question = question.to_string();
    Box::new(move |e| {
        match &e {
            Evenement::Morceau(texte) => reponse.lock().unwrap_or_else(|e| e.into_inner()).push_str(texte),
            Evenement::Fini { erreur: None, .. } => {
                let texte = std::mem::take(&mut *reponse.lock().unwrap_or_else(|e| e.into_inner()));
                let faits = bscreen_api::corrections::extraire(&texte);
                if !faits.is_empty()
                    && let Ok(base) = Base::ouvrir(&bscreen_core::chemins::donnees().join("index.sqlite"))
                {
                    let maintenant = chrono::Local::now().timestamp_millis();
                    for fait in faits {
                        match base.ajouter_correction(maintenant, &fait, Some(&question)) {
                            Ok(id) => tracing::info!(id, "correction de Julien retenue"),
                            Err(e) => tracing::warn!(%e, "correction pas retenue"),
                        }
                    }
                }
            }
            _ => {}
        }
        envoi(e)
    })
}

fn date_du_jour() -> String {
    date_de(Local::now())
}

fn date_de(maintenant: chrono::DateTime<Local>) -> String {
    format!(
        "Nous sommes le {} {}, il est {}.",
        JOURS[maintenant.weekday().num_days_from_monday() as usize],
        maintenant.format("%d/%m/%Y"),
        maintenant.format("%H:%M")
    )
}

/// « Sur le Mac » : la question va au modèle local (Ollama), avec un contexte resserré (les
/// résumés de la nuit d'abord). Rien ne sort du Mac. Plus lent que Claude : ≈ 15 à 25 s.
fn demander_au_mac(question: &str, precedent: Option<(&str, &str)>, envoi: Box<dyn Fn(Evenement) + Send>) -> Arret {
    let date = date_du_jour();
    let precedent = precedent.map(|(q, r)| (q.to_string(), r.to_string()));
    let question = question.to_string();
    let annule = Arc::new(std::sync::atomic::AtomicBool::new(false));
    tracing::info!(caracteres = question.chars().count(), "question posée au modèle local (Ask)");
    let annule_fil = Arc::clone(&annule);
    let _ = std::thread::Builder::new().name("bscreen-ask-local".into()).spawn(move || {
        let debut = Instant::now();
        let fini = |erreur: Option<String>| Evenement::Fini {
            erreur,
            secondes: debut.elapsed().as_secs_f64(),
        };
        envoi(Evenement::Lecture {
            outil: "contexte".into(),
            quoi: "sur le Mac".into(),
        });
        let contexte = preparer_contexte_avec(&question, bscreen_api::contexte::Budget::LOCAL);
        tracing::info!(ms = debut.elapsed().as_millis(), caracteres = contexte.chars().count(), "contexte d'Ask (local) prêt");
        // Pas d'« Envoyé » : rien ne part du Mac.
        envoi(Evenement::Lecture {
            outil: LOCAL.into(),
            quoi: bscreen_macos::ollama::MODELE.into(),
        });
        let precedent = precedent.as_ref().map(|(q, r)| (q.as_str(), r.as_str()));
        let invite = invite_locale(&date, precedent, &contexte, &question);
        if let Err(e) = bscreen_macos::ollama::prechauffer() {
            envoi(fini(Some(format!("Le modèle local ne démarre pas : {e}"))));
            return;
        }
        let mut premier_mot = None;
        let mut resultat = bscreen_macos::ollama::discuter_au_fil(CONSIGNES_LOCALES, &invite, &annule_fil, |morceau| {
            premier_mot.get_or_insert_with(|| debut.elapsed().as_secs_f64());
            envoi(Evenement::Morceau(morceau.to_string()));
        });
        // Le serveur a disparu avant le premier mot (les résumés de la nuit s'arrêtaient avec le
        // leur) : on relance le nôtre et on repose la question, une fois.
        if resultat.is_err()
            && premier_mot.is_none()
            && !annule_fil.load(std::sync::atomic::Ordering::SeqCst)
            && bscreen_macos::ollama::prechauffer().is_ok()
        {
            resultat = bscreen_macos::ollama::discuter_au_fil(CONSIGNES_LOCALES, &invite, &annule_fil, |morceau| {
                premier_mot.get_or_insert_with(|| debut.elapsed().as_secs_f64());
                envoi(Evenement::Morceau(morceau.to_string()));
            });
        }
        if annule_fil.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        envoi(fini(resultat.err().map(|e| format!("Modèle local : {e}"))));
        tracing::info!(
            premiers_mots_s = premier_mot.unwrap_or_default(),
            secondes = debut.elapsed().as_secs_f64(),
            "réponse du modèle local (Ask)"
        );
    });
    Arret(Box::new(move || {
        annule.store(true, std::sync::atomic::Ordering::SeqCst);
    }))
}

/// Le contexte de la question, cherché sur le Mac (jamais les moments privés).
pub fn preparer_contexte(question: &str) -> String {
    preparer_contexte_avec(question, bscreen_api::contexte::Budget::COMPLET)
}

fn preparer_contexte_avec(question: &str, budget: bscreen_api::contexte::Budget) -> String {
    let Ok(base) = Base::ouvrir_lecture(&bscreen_core::chemins::donnees().join("index.sqlite")) else {
        return "Base de B-Screen illisible.".to_string();
    };
    let t0 = Instant::now();
    // Le modèle de sens n'est attendu que 1,5 s : s'il se recharge encore (4 s), on s'en passe —
    // Claude peut toujours chercher par le sens lui-même.
    let vecteur = bscreen_macos::sens::vecteur_question(question, std::time::Duration::from_millis(1500));
    let t1 = Instant::now();
    let proches = vecteur.and_then(|v| proches(&base, &v));
    // Les images qui ressemblent à la question (le modèle des textes se charge en 1 s).
    let images = bscreen_macos::images::vecteur_question(question, std::time::Duration::from_millis(1500))
        .and_then(|v| images_proches(&base, &v));
    let t2 = Instant::now();
    let maintenant = chrono::Local::now().timestamp_millis();
    let trouve = bscreen_api::contexte::Proches { textes: proches.as_deref(), images: images.as_deref() };
    let contexte = bscreen_api::contexte::preparer_avec(&base, question, maintenant, trouve, budget);
    tracing::info!(
        vecteur_ms = (t1 - t0).as_millis(),
        proches_ms = (t2 - t1).as_millis(),
        base_ms = t2.elapsed().as_millis(),
        "contexte d'Ask : étapes"
    );
    bscreen_macos::mesures::noter(bscreen_macos::mesures::Geste::ContexteAsk, t0.elapsed().as_millis());
    contexte
}

/// Les textes proches d'une question par le sens, pour son contexte : les 60 plus proches, à
/// partir d'une similarité de 0,25.
pub fn proches(base: &Base, vecteur: &[f32]) -> Option<Vec<(i64, f32)>> {
    bscreen_macos::sens::plus_proches(base, vecteur, 60, 0.25).ok()
}

/// Les images qui ressemblent à une question, pour son contexte : les 60 plus proches, à partir
/// d'un cosinus de 0,06 (le contexte n'en garde que si la question parle de voir, ou si l'une
/// ressemble nettement).
pub fn images_proches(base: &Base, vecteur: &[f32]) -> Option<Vec<(i64, f32)>> {
    bscreen_macos::images::plus_proches(base, vecteur, 60, 0.06).ok()
}

/// Ce que reçoit le modèle local : la date, le contexte trouvé sur le Mac et la question (avec la
/// question précédente et sa réponse, pour une question de suite).
fn invite_locale(date: &str, precedent: Option<(&str, &str)>, contexte: &str, question: &str) -> String {
    match precedent {
        Some((avant, reponse)) => format!(
            "{date}\n\nQuestion précédente de Julien : {avant}\nTa réponse : {reponse}\n\n\
             Ce que B-Screen a trouvé sur le Mac pour la nouvelle question :\n{contexte}\n\n\
             Nouvelle question (elle peut préciser ou corriger la précédente) : {question}"
        ),
        None => format!("{date}\n\nCe que B-Screen a trouvé sur le Mac pour cette question :\n{contexte}\n\nQuestion de Julien : {question}"),
    }
}

/// La réponse du modèle local à une question, avec un contexte déjà prêt (le banc d'essai), comme
/// « Sur le Mac » la donnerait ; `maintenant_ms` : l'heure qu'on lui dit.
pub fn repondre_sur_le_mac(question: &str, contexte: &str, maintenant_ms: i64) -> Result<String> {
    let maintenant = chrono::TimeZone::timestamp_millis_opt(&Local, maintenant_ms).single().unwrap_or_else(Local::now);
    let invite = invite_locale(&date_de(maintenant), None, contexte, question);
    bscreen_macos::ollama::prechauffer()?;
    let mut reponse = String::new();
    let jamais = std::sync::atomic::AtomicBool::new(false);
    bscreen_macos::ollama::discuter_au_fil(CONSIGNES_LOCALES, &invite, &jamais, |morceau| reponse.push_str(morceau))?;
    Ok(reponse)
}

/// Lit la sortie de `claude -p --output-format stream-json --include-partial-messages`.
#[derive(Default)]
struct Lecteur {
    outils_par_id: std::collections::HashMap<String, String>,
    /// Du texte est déjà arrivé morceau par morceau pour le message en cours.
    morceaux_recus: bool,
    /// Un bloc de texte a déjà été écrit (les suivants commencent par un saut de paragraphe).
    deja_du_texte: bool,
}

impl Lecteur {
    fn traduire(&mut self, e: &serde_json::Value) -> Vec<Evenement> {
        let mut sortie = Vec::new();
        match e["type"].as_str() {
            Some("system") if e["subtype"] == "init" => {
                let branchee = e["mcp_servers"]
                    .as_array()
                    .is_some_and(|s| s.iter().any(|m| m["name"] == "b-screen" && m["status"] == "connected"));
                if !branchee {
                    tracing::warn!("Ask : la mémoire de B-Screen n'est pas branchée");
                }
            }
            Some("stream_event") => {
                let ev = &e["event"];
                match ev["type"].as_str() {
                    Some("message_start") => self.morceaux_recus = false,
                    Some("content_block_start") if ev["content_block"]["type"] == "text" => {
                        if self.deja_du_texte {
                            sortie.push(Evenement::Morceau("\n\n".into()));
                        }
                    }
                    Some("content_block_delta") if ev["delta"]["type"] == "text_delta" => {
                        if let Some(t) = ev["delta"]["text"].as_str() {
                            self.morceaux_recus = true;
                            self.deja_du_texte = true;
                            sortie.push(Evenement::Morceau(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                for bloc in e["message"]["content"].as_array().into_iter().flatten() {
                    match bloc["type"].as_str() {
                        Some("tool_use") => {
                            let outil = bloc["name"].as_str().unwrap_or("").trim_start_matches(PREFIXE).to_string();
                            if let Some(id) = bloc["id"].as_str() {
                                self.outils_par_id.insert(id.to_string(), outil.clone());
                            }
                            sortie.push(Evenement::Lecture {
                                quoi: decrire(&bloc["input"]),
                                outil,
                            });
                        }
                        // Déjà arrivé morceau par morceau : rien à rajouter.
                        Some("text") if !self.morceaux_recus => {
                            let texte = bloc["text"].as_str().unwrap_or("").trim();
                            if !texte.is_empty() {
                                if self.deja_du_texte {
                                    sortie.push(Evenement::Morceau("\n\n".into()));
                                }
                                self.deja_du_texte = true;
                                sortie.push(Evenement::Morceau(texte.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for bloc in e["message"]["content"].as_array().into_iter().flatten() {
                    if bloc["type"] == "tool_result" {
                        let outil = bloc["tool_use_id"]
                            .as_str()
                            .and_then(|id| self.outils_par_id.get(id))
                            .cloned()
                            .unwrap_or_default();
                        let contenu = match &bloc["content"] {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Array(morceaux) => morceaux
                                .iter()
                                .filter_map(|m| m["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        sortie.push(Evenement::Envoye { outil, contenu });
                    }
                }
            }
            Some("result") => {
                let erreur = if e["is_error"].as_bool().unwrap_or(false) || e["subtype"] != "success" {
                    Some(
                        e["result"]
                            .as_str()
                            .filter(|r| !r.is_empty())
                            .map(|r| format!("Claude : {r}"))
                            .unwrap_or_else(|| "Claude n'a pas pu répondre".to_string()),
                    )
                } else {
                    None
                };
                sortie.push(Evenement::Fini {
                    erreur,
                    secondes: e["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0,
                });
            }
            _ => {}
        }
        sortie
    }
}

/// Ce que Claude demande à la mémoire, en clair (« « appel Léa » », « 17:00 → 22:53 »).
fn decrire(entree: &serde_json::Value) -> String {
    let texte = |cle: &str| entree[cle].as_str().map(str::to_string);
    if let Some(requete) = texte("requete") {
        return format!("« {requete} »");
    }
    let mut morceaux = Vec::new();
    match (texte("debut"), texte("fin")) {
        (Some(d), Some(f)) => morceaux.push(format!("{} → {}", court(&d), court(&f))),
        (Some(d), None) => morceaux.push(format!("depuis {}", court(&d))),
        (None, Some(f)) => morceaux.push(format!("jusqu'à {}", court(&f))),
        (None, None) => {}
    }
    if let Some(p) = texte("personne") {
        morceaux.push(format!("par {p}"));
    }
    if let Some(q) = texte("quand") {
        morceaux.push(court(&q));
    }
    morceaux.join(" · ")
}

/// « 2026-11-20 17:00 » → « 20/11 17:00 » ; le reste tel quel.
fn court(heure: &str) -> String {
    let (date, reste) = heure.split_once(' ').unwrap_or((heure, ""));
    let parties: Vec<&str> = date.split('-').collect();
    if parties.len() == 3 && parties[0].len() == 4 {
        format!("{}/{} {reste}", parties[2], parties[1]).trim().to_string()
    } else {
        heure.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evenements_de_claude() {
        let mut lecteur = Lecteur::default();
        let appel = serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"t1","name":"mcp__b-screen__activite","input":{"debut":"2026-11-20 17:00","fin":"2026-11-20 22:53"}}]}});
        assert_eq!(
            lecteur.traduire(&appel),
            vec![Evenement::Lecture {
                outil: "activite".into(),
                quoi: "20/11 17:00 → 20/11 22:53".into()
            }]
        );
        let resultat = serde_json::json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"• Safari"}]}]}});
        assert_eq!(
            lecteur.traduire(&resultat),
            vec![Evenement::Envoye {
                outil: "activite".into(),
                contenu: "• Safari".into()
            }]
        );
        let fin = serde_json::json!({"type":"result","subtype":"success","is_error":false,"duration_ms":9075,"result":"ok"});
        assert_eq!(
            lecteur.traduire(&fin),
            vec![Evenement::Fini {
                erreur: None,
                secondes: 9.075
            }]
        );
        assert_eq!(decrire(&serde_json::json!({"requete":"appel Léa"})), "« appel Léa »");
    }
}
