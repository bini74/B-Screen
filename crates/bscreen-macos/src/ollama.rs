//! Ollama sur ce Mac : le modèle local qui résume les épisodes la nuit (choix de Julien, 22/11).
//! B-Screen lance le serveur s'il ne tourne pas (`ollama serve`, sans ouvrir l'app Ollama) et
//! l'arrête après usage. Tout passe par 127.0.0.1 : rien ne sort du Mac.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

/// Le meilleur du banc d'essai local du 20/11 (lecture ≈ 210 jetons/s sur le M1 Pro de Julien).
pub const MODELE: &str = "qwen3.5:9b";
const ADRESSE: &str = "http://127.0.0.1:11434";
/// La même fenêtre de contexte partout : la changer ferait recharger le modèle (5 à 7 s).
const NUM_CTX: u32 = 8192;

/// Le serveur gardé pour Ask (lancé à l'ouverture du panneau), arrêté quand B-Screen s'arrête.
static SERVEUR_ASK: Mutex<Option<Serveur>> = Mutex::new(None);

fn ollama() -> Option<PathBuf> {
    let maison = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    [
        PathBuf::from("/usr/local/bin/ollama"),
        PathBuf::from("/opt/homebrew/bin/ollama"),
        PathBuf::from("/Applications/Ollama.app/Contents/Resources/ollama"),
        maison.join("Applications/Ollama.app/Contents/Resources/ollama"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

/// Le modèle est-il téléchargé ? (sans lancer le serveur)
pub fn modele_present() -> bool {
    let Some(maison) = std::env::var_os("HOME").map(PathBuf::from) else { return false };
    let (nom, etiquette) = MODELE.split_once(':').unwrap_or((MODELE, "latest"));
    ollama().is_some() && maison.join(".ollama/models/manifests/registry.ollama.ai/library").join(nom).join(etiquette).is_file()
}

fn repond() -> bool {
    Command::new("/usr/bin/curl")
        .args(["-s", "-o", "/dev/null", "--max-time", "1", &format!("{ADRESSE}/api/version")])
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Le serveur Ollama, prêt. S'il ne tournait pas, B-Screen l'a lancé et l'arrête quand ce garde part.
pub struct Serveur {
    fils: Option<Child>,
}

impl Serveur {
    pub fn demarrer() -> Result<Self> {
        if repond() {
            return Ok(Self { fils: None });
        }
        let binaire = ollama().ok_or_else(|| anyhow!("Ollama n'est pas installé sur ce Mac"))?;
        nettoyer_orphelins();
        // Son propre groupe de processus : à l'arrêt, le groupe entier part (le serveur et les
        // « llama-server » qui portent le modèle).
        let mut fils = Command::new(&binaire)
            .arg("serve")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("lancement de {}", binaire.display()))?;
        for _ in 0..80 {
            if repond() {
                tracing::info!("serveur Ollama lancé par B-Screen");
                return Ok(Self { fils: Some(fils) });
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = fils.kill();
        let _ = fils.wait();
        bail!("le serveur Ollama ne répond pas")
    }
}

impl Drop for Serveur {
    fn drop(&mut self) {
        if let Some(mut fils) = self.fils.take() {
            // Tout le groupe : tué seul, le serveur laissait ses « llama-server » derrière lui,
            // avec le modèle en mémoire (≈ 6,5 Go chacun ; vu le 22/11 : trois, 20 Go).
            let groupe = fils.id() as libc::pid_t;
            // SAFETY : signaux envoyés au groupe que B-Screen a créé pour ce serveur.
            unsafe { libc::killpg(groupe, libc::SIGTERM) };
            for _ in 0..50 {
                if fils.try_wait().is_ok_and(|fin| fin.is_some()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            unsafe { libc::killpg(groupe, libc::SIGKILL) };
            let _ = fils.wait();
            tracing::info!("serveur Ollama arrêté");
        }
    }
}

/// Les « llama-server » d'Ollama restés seuls (leur serveur est parti : plantage, arrêt brutal)
/// gardent le modèle en mémoire pour rien : on les arrête. Ceux d'un serveur vivant ne sont pas touchés.
pub fn nettoyer_orphelins() {
    let Ok(sortie) = Command::new("/bin/ps").args(["-axo", "pid=,ppid=,command="]).output() else {
        return;
    };
    let texte = String::from_utf8_lossy(&sortie.stdout);
    let mut arretes = 0;
    for ligne in texte.lines() {
        let mut champs = ligne.split_whitespace();
        let (Some(pid), Some(parent)) = (champs.next(), champs.next()) else { continue };
        let commande: Vec<&str> = champs.collect();
        let est_runner = commande.first().is_some_and(|c| c.ends_with("/llama-server") && c.contains("Ollama.app"))
            && commande.iter().any(|a| a.contains("/.ollama/models/"));
        if est_runner
            && parent == "1"
            && let Ok(pid) = pid.parse::<libc::pid_t>()
        {
            // SAFETY : un processus orphelin d'Ollama, à nous (même utilisateur).
            unsafe { libc::kill(pid, libc::SIGTERM) };
            arretes += 1;
        }
    }
    if arretes > 0 {
        tracing::warn!(processus = arretes, "modèles Ollama orphelins arrêtés (mémoire rendue)");
    }
}

/// Une réponse du modèle local, au format imposé par `schema` (JSON), sans réflexion. Le modèle
/// reste chargé 2 minutes (le temps de l'épisode suivant), puis libère la mémoire.
pub fn discuter(systeme: &str, invite: &str, schema: &serde_json::Value) -> Result<String> {
    let corps = serde_json::json!({
        "model": MODELE,
        "stream": false,
        "think": false,
        "keep_alive": "2m",
        "format": schema,
        "options": { "num_ctx": NUM_CTX, "temperature": 0.2 },
        "messages": [
            { "role": "system", "content": systeme },
            { "role": "user", "content": invite }
        ]
    });
    let mut curl = Command::new("/usr/bin/curl")
        .args(["-s", "--max-time", "600", "-H", "Content-Type: application/json", "-d", "@-"])
        .arg(format!("{ADRESSE}/api/chat"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("curl")?;
    curl.stdin
        .take()
        .ok_or_else(|| anyhow!("entrée de curl"))?
        .write_all(&serde_json::to_vec(&corps)?)?;
    let sortie = curl.wait_with_output()?;
    if !sortie.status.success() {
        bail!("Ollama injoignable ({})", sortie.status);
    }
    let reponse: serde_json::Value = serde_json::from_slice(&sortie.stdout).context("réponse d'Ollama illisible")?;
    if let Some(erreur) = reponse["error"].as_str() {
        bail!("Ollama : {erreur}");
    }
    reponse["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("réponse d'Ollama vide"))
}

fn poster(chemin: &str, corps: &serde_json::Value, flux: bool) -> Result<Child> {
    let mut curl = Command::new("/usr/bin/curl")
        .args([if flux { "-sN" } else { "-s" }, "--max-time", "600", "-H", "Content-Type: application/json", "-d", "@-"])
        .arg(format!("{ADRESSE}{chemin}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("curl")?;
    curl.stdin
        .take()
        .ok_or_else(|| anyhow!("entrée de curl"))?
        .write_all(&serde_json::to_vec(corps)?)?;
    Ok(curl)
}

/// Pour Ask : le serveur prêt et le modèle chargé en mémoire (pendant que Julien tape). Le modèle
/// reste chargé 15 minutes après la dernière question.
pub fn prechauffer() -> Result<()> {
    {
        let mut garde = SERVEUR_ASK.lock().unwrap_or_else(|e| e.into_inner());
        // Un serveur qui n'est pas à nous (celui des résumés de la nuit) a pu s'arrêter depuis.
        if garde.as_ref().is_none_or(|s| s.fils.is_none() && !repond()) {
            *garde = Some(Serveur::demarrer()?);
        }
    }
    let corps = serde_json::json!({ "model": MODELE, "keep_alive": "15m", "options": { "num_ctx": NUM_CTX } });
    let sortie = poster("/api/generate", &corps, false)?.wait_with_output()?;
    let reponse: serde_json::Value = serde_json::from_slice(&sortie.stdout).unwrap_or_default();
    if let Some(erreur) = reponse["error"].as_str() {
        bail!("Ollama : {erreur}");
    }
    Ok(())
}

/// Arrête le serveur lancé pour Ask (B-Screen s'arrête).
pub fn arreter() {
    drop(SERVEUR_ASK.lock().unwrap_or_else(|e| e.into_inner()).take());
}

/// Pose la question au modèle local ; chaque morceau de réponse passe à `morceau`, au fil de l'eau.
/// `arret` : Julien a fermé le panneau ou posé une autre question.
pub fn discuter_au_fil(systeme: &str, invite: &str, arret: &AtomicBool, mut morceau: impl FnMut(&str)) -> Result<()> {
    let corps = serde_json::json!({
        "model": MODELE,
        "stream": true,
        "think": false,
        "keep_alive": "15m",
        "options": { "num_ctx": NUM_CTX, "temperature": 0.3 },
        "messages": [
            { "role": "system", "content": systeme },
            { "role": "user", "content": invite }
        ]
    });
    let mut curl = poster("/api/chat", &corps, true)?;
    let sortie = curl.stdout.take().ok_or_else(|| anyhow!("sortie de curl"))?;
    let (envoi, lignes) = std::sync::mpsc::channel::<String>();
    let _ = std::thread::Builder::new().name("bscreen-ollama-lecture".into()).spawn(move || {
        for ligne in BufReader::new(sortie).lines().map_while(Result::ok) {
            if envoi.send(ligne).is_err() {
                break;
            }
        }
    });
    let resultat = loop {
        if arret.load(Ordering::SeqCst) {
            break Ok(());
        }
        let ligne = match lignes.recv_timeout(Duration::from_millis(250)) {
            Ok(l) => l,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Err(anyhow!("Ollama s'est tu avant la fin")),
        };
        let Ok(valeur) = serde_json::from_str::<serde_json::Value>(&ligne) else { continue };
        if let Some(erreur) = valeur["error"].as_str() {
            break Err(anyhow!("Ollama : {erreur}"));
        }
        if let Some(texte) = valeur["message"]["content"].as_str().filter(|t| !t.is_empty()) {
            morceau(texte);
        }
        if valeur["done"].as_bool().unwrap_or(false) {
            break Ok(());
        }
    };
    let _ = curl.kill();
    let _ = curl.wait();
    resultat
}
