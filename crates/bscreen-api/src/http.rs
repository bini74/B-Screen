//! API HTTP locale (phase 9.1), en lecture seule : n'écoute que sur 127.0.0.1, et chaque demande
//! doit porter le jeton (`Authorization: Bearer …`). Désactivée tant que Julien ne l'allume pas.
//!
//! - `GET /chercher?q=dauphins&limite=20`
//! - `GET /activite?debut=hier` · `GET /voix?debut=2026-11-19 21:00&fin=2026-11-19 22:00&personne=camille`
//! - `GET /recherches?debut=hier` (ce qui a été tapé dans Google, YouTube…)
//! - `GET /ecran?quand=21:30`
//! - `GET /export?jour=hier` (JSON Lines, format « moment »)
//!
//! Mêmes règles que le serveur MCP : moments privés jamais donnés, texte déjà masqué.

use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bscreen_store::Base;
use serde::Deserialize;

use crate::mcp;

#[derive(Clone)]
struct Etat {
    base: Arc<Mutex<Base>>,
    jeton: Arc<String>,
}

impl Etat {
    fn avec<T>(&self, f: impl FnOnce(&Base) -> T) -> T {
        let base = self.base.lock().unwrap_or_else(|e| e.into_inner());
        f(&base)
    }
}

/// Comparaison sans fuite de temps (le jeton ne se devine pas caractère par caractère).
fn egaux(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn autorise(entetes: &HeaderMap, jeton: &str) -> bool {
    entetes
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|donne| egaux(donne.trim().as_bytes(), jeton.as_bytes()))
}

fn refus() -> Response {
    (StatusCode::UNAUTHORIZED, "jeton manquant ou faux\n").into_response()
}

fn texte(corps: String) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], corps + "\n").into_response()
}

#[derive(Deserialize)]
struct QChercher {
    q: String,
    limite: Option<u32>,
}

#[derive(Deserialize)]
struct QPeriode {
    debut: Option<String>,
    fin: Option<String>,
}

#[derive(Deserialize)]
struct QParoles {
    debut: Option<String>,
    fin: Option<String>,
    personne: Option<String>,
}

#[derive(Deserialize)]
struct QInstant {
    quand: String,
}

#[derive(Deserialize)]
struct QJour {
    jour: Option<String>,
}

async fn chercher(State(e): State<Etat>, entetes: HeaderMap, Query(q): Query<QChercher>) -> Response {
    if !autorise(&entetes, &e.jeton) {
        return refus();
    }
    texte(e.avec(|b| mcp::chercher(b, &q.q, q.limite.unwrap_or(20) as usize)))
}

async fn activite(State(e): State<Etat>, entetes: HeaderMap, Query(q): Query<QPeriode>) -> Response {
    if !autorise(&entetes, &e.jeton) {
        return refus();
    }
    match mcp::periode_de(q.debut.as_deref(), q.fin.as_deref()) {
        Ok((d, f)) => texte(e.avec(|b| mcp::activite(b, d, f))),
        Err(m) => (StatusCode::BAD_REQUEST, m).into_response(),
    }
}

async fn voix(State(e): State<Etat>, entetes: HeaderMap, Query(q): Query<QParoles>) -> Response {
    if !autorise(&entetes, &e.jeton) {
        return refus();
    }
    match mcp::periode_de(q.debut.as_deref(), q.fin.as_deref()) {
        Ok((d, f)) => texte(e.avec(|b| mcp::voix_de(b, d, f, q.personne.as_deref()))),
        Err(m) => (StatusCode::BAD_REQUEST, m).into_response(),
    }
}

async fn recherches(State(e): State<Etat>, entetes: HeaderMap, Query(q): Query<QPeriode>) -> Response {
    if !autorise(&entetes, &e.jeton) {
        return refus();
    }
    match mcp::periode_de(q.debut.as_deref(), q.fin.as_deref()) {
        Ok((d, f)) => texte(e.avec(|b| mcp::recherches(b, d, f))),
        Err(m) => (StatusCode::BAD_REQUEST, m).into_response(),
    }
}

async fn ecran(State(e): State<Etat>, entetes: HeaderMap, Query(q): Query<QInstant>) -> Response {
    if !autorise(&entetes, &e.jeton) {
        return refus();
    }
    match crate::heures::periode(&q.quand, chrono::Local::now().timestamp_millis()) {
        Some((quand, _)) => texte(e.avec(|b| mcp::ecran(b, quand))),
        None => (StatusCode::BAD_REQUEST, format!("instant incompris : « {} »", q.quand)).into_response(),
    }
}

async fn export(State(e): State<Etat>, entetes: HeaderMap, Query(q): Query<QJour>) -> Response {
    if !autorise(&entetes, &e.jeton) {
        return refus();
    }
    let quand = q.jour.unwrap_or_else(|| "aujourd'hui".into());
    let Some((debut, _)) = crate::heures::periode(&quand, chrono::Local::now().timestamp_millis()) else {
        return (StatusCode::BAD_REQUEST, format!("jour incompris : « {quand} »")).into_response();
    };
    let mut corps = Vec::new();
    match e.avec(|b| crate::export::exporter_jour(b, crate::heures::jour_local(debut), &mut corps)) {
        Ok(_) => ([(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")], corps).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

fn routes(etat: Etat) -> Router {
    Router::new()
        .route("/chercher", get(chercher))
        .route("/activite", get(activite))
        .route("/voix", get(voix))
        .route("/recherches", get(recherches))
        .route("/ecran", get(ecran))
        .route("/export", get(export))
        .with_state(etat)
}

/// Le jeton de l'API : lu dans `fichier`, ou créé (32 octets au hasard, fichier lisible par Julien seul).
pub fn jeton(fichier: &Path) -> Result<String> {
    if let Ok(existant) = std::fs::read_to_string(fichier) {
        let existant = existant.trim().to_string();
        if existant.len() >= 32 {
            return Ok(existant);
        }
    }
    let mut octets = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut octets)?;
    let jeton: String = octets.iter().map(|o| format!("{o:02x}")).collect();
    if let Some(dossier) = fichier.parent() {
        std::fs::create_dir_all(dossier)?;
    }
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(fichier)?;
        std::io::Write::write_all(&mut f, jeton.as_bytes())?;
    }
    Ok(jeton)
}

/// Sert l'API sur 127.0.0.1:`port` jusqu'à ce que `arret` reçoive un signal (ou soit abandonné).
pub fn lancer(port: u16, fichier_base: &Path, jeton: String, arret: tokio::sync::oneshot::Receiver<()>) -> Result<()> {
    let base = Base::ouvrir_lecture(fichier_base)?;
    let etat = Etat {
        base: Arc::new(Mutex::new(base)),
        jeton: Arc::new(jeton),
    };
    let execution = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    execution.block_on(async move {
        let adresse = SocketAddr::from(([127, 0, 0, 1], port));
        let ecoute = tokio::net::TcpListener::bind(adresse)
            .await
            .map_err(|e| anyhow!("port {port} indisponible : {e}"))?;
        axum::serve(ecoute, routes(etat))
            .with_graceful_shutdown(async move {
                let _ = arret.await;
            })
            .await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jeton_obligatoire_et_compare_en_entier() {
        let mut entetes = HeaderMap::new();
        assert!(!autorise(&entetes, "abc"));
        entetes.insert(header::AUTHORIZATION, "Bearer abd".parse().unwrap());
        assert!(!autorise(&entetes, "abc"));
        entetes.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert!(autorise(&entetes, "abc"));
        entetes.insert(header::AUTHORIZATION, "Bearer ab".parse().unwrap());
        assert!(!autorise(&entetes, "abc"), "un début de jeton ne suffit pas");
    }

    #[test]
    fn jeton_cree_une_fois_et_prive() {
        use std::os::unix::fs::PermissionsExt;
        let dossier = std::env::temp_dir().join(format!("bscreen-jeton-{}", std::process::id()));
        let fichier = dossier.join("api-jeton");
        let premier = jeton(&fichier).unwrap();
        assert_eq!(premier.len(), 64);
        assert_eq!(jeton(&fichier).unwrap(), premier, "le même jeton à chaque lancement");
        let mode = std::fs::metadata(&fichier).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    #[test]
    fn serveur_de_bout_en_bout() {
        use std::io::{Read as _, Write as _};
        let dossier = std::env::temp_dir().join(format!("bscreen-api-{}", std::process::id()));
        let fichier = dossier.join("index.sqlite");
        drop(Base::ouvrir(&fichier).unwrap());
        let port = 40_000 + (std::process::id() % 20_000) as u16;
        let (envoi, reception) = tokio::sync::oneshot::channel();
        let base = fichier.clone();
        let serveur = std::thread::spawn(move || lancer(port, &base, "secret-de-test".into(), reception));
        let demander = |jeton: Option<&str>| -> String {
            for _ in 0..50 {
                if let Ok(mut flux) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                    let autorisation = jeton.map(|j| format!("Authorization: Bearer {j}\r\n")).unwrap_or_default();
                    write!(
                        flux,
                        "GET /chercher?q=dauphins HTTP/1.1\r\nHost: 127.0.0.1\r\n{autorisation}Connection: close\r\n\r\n"
                    )
                    .unwrap();
                    let mut reponse = String::new();
                    flux.read_to_string(&mut reponse).unwrap();
                    return reponse;
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            panic!("le serveur ne répond pas");
        };
        assert!(demander(None).starts_with("HTTP/1.1 401"), "sans jeton : refusé");
        assert!(demander(Some("faux")).starts_with("HTTP/1.1 401"));
        let ok = demander(Some("secret-de-test"));
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("Rien trouvé pour « dauphins »"), "{ok}");
        envoi.send(()).unwrap();
        serveur.join().unwrap().unwrap();
        std::fs::remove_dir_all(&dossier).unwrap();
    }
}
