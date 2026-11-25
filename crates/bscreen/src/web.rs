//! La page web de B-Screen (25/11/2026) : chercher et demander depuis l'iPhone, dans la mémoire
//! du Mac.
//!
//! - Servie par l'app sur 127.0.0.1 seulement (port 47338), éteinte tant que Julien ne l'allume
//!   pas (Réglages › Autres programmes). Depuis dehors, seulement derrière Teleport (un service
//!   d'application sur le Mac, ouvert par un tunnel sortant) : chaque requête porte alors le jeton
//!   signé de Teleport, vérifié ici (clés du proxy, comptes permis ; réglages `page_web_nom`,
//!   `page_web_proxy`, `page_web_comptes`).
//! - Un jeton (fichier `web-jeton` dans les données, 0600) : l'app ouvre elle-même la page avec
//!   lui ; la page le change en cookie (HttpOnly, SameSite=Strict) et l'efface de l'adresse.
//! - Chercher : la recherche de la frise (mots, sens, images), regroupée par endroit, avec
//!   vignettes ; toucher une carte montre l'image en grand. Demander : Ask, la réponse au fil (la
//!   page interroge toutes les 400 ms), renvois touchables.
//! - Jamais rien de privé : les images verrouillées ne sont ni listées, ni servies ; lecture seule.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, anyhow};
use axum::Router;
use axum::extract::Query;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bscreen_core::chemins;
use bscreen_store::{Base, Image};
use bscreen_ui::ask::{Arret, Evenement};
use serde::Deserialize;
use serde_json::{Value, json};

const PAGE: &str = include_str!("web/index.html");
const ICONE: &str = include_str!("../../../assets/logo-strates.svg");
const MANIFESTE: &str = r##"{"name":"B-Screen","short_name":"B-Screen","start_url":"/","display":"standalone","background_color":"#0b0d12","theme_color":"#0b0d12","icons":[{"src":"/icone.svg","sizes":"any","type":"image/svg+xml"}]}"##;
const COOKIE: &str = "bscreen_web";
/// Les noms sous lesquels la page peut être appelée (contre le « DNS rebinding ») : le Mac, et
/// plus tard son nom public derrière Teleport.
const HOTES: &[&str] = &["127.0.0.1", "localhost"];
/// Endroits montrés au plus par recherche.
const ENDROITS_MAX: usize = 60;
/// Deux résultats au même endroit (même page ou fenêtre) à moins de ça : une seule carte.
const MEME_ENDROIT_MS: i64 = 15 * 60_000;

static ARRET: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

fn fichier_jeton() -> PathBuf {
    chemins::donnees().join("web-jeton")
}

/// Allume ou éteint la page (réglage « page web »).
pub fn changer(allumee: bool, port: u16) {
    if allumee {
        demarrer(port);
    } else {
        arreter();
    }
}

/// Démarre la page sur 127.0.0.1:`port` (si elle ne tourne pas déjà).
pub fn demarrer(port: u16) {
    let mut arret = ARRET.lock().unwrap_or_else(|e| e.into_inner());
    if arret.is_some() {
        return;
    }
    let jeton = match bscreen_api::http::jeton(&fichier_jeton()) {
        Ok(j) => j,
        Err(e) => return tracing::warn!(%e, "jeton de la page web impossible"),
    };
    let (envoi, reception) = tokio::sync::oneshot::channel();
    *arret = Some(envoi);
    let _ = std::thread::Builder::new().name("bscreen-web".into()).spawn(move || {
        tracing::info!(port, "page web démarrée (127.0.0.1)");
        if let Err(e) = servir(port, jeton, reception) {
            tracing::warn!(%e, "page web arrêtée");
        }
        *ARRET.lock().unwrap_or_else(|e| e.into_inner()) = None;
    });
}

pub fn arreter() {
    if let Some(envoi) = ARRET.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = envoi.send(());
        tracing::info!("page web arrêtée");
    }
}

/// L'adresse qui ouvre la page sur ce Mac, avec le jeton (dans le fragment : il ne part jamais
/// vers le serveur tel quel, la page le change en cookie).
pub fn adresse_locale(port: u16) -> Option<String> {
    let jeton = bscreen_api::http::jeton(&fichier_jeton()).ok()?;
    Some(format!("http://127.0.0.1:{port}/#jeton={jeton}"))
}

fn servir(port: u16, jeton: String, arret: tokio::sync::oneshot::Receiver<()>) -> Result<()> {
    let jeton = Arc::new(jeton);
    let execution = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build()?;
    execution.block_on(async move {
        let adresse = SocketAddr::from(([127, 0, 0, 1], port));
        let ecoute = tokio::net::TcpListener::bind(adresse)
            .await
            .map_err(|e| anyhow!("port {port} indisponible : {e}"))?;
        axum::serve(ecoute, routes(jeton))
            .with_graceful_shutdown(async move {
                let _ = arret.await;
            })
            .await?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------------------------
// Les demandes.

fn routes(jeton: Arc<String>) -> Router {
    let j = |f: fn(HashMap<String, String>) -> Response| {
        let jeton = Arc::clone(&jeton);
        move |entetes: HeaderMap, Query(q): Query<HashMap<String, String>>| {
            let jeton = Arc::clone(&jeton);
            async move {
                if !hote_permis(&entetes) {
                    return (StatusCode::MISDIRECTED_REQUEST, "nom inconnu").into_response();
                }
                if !autorise(&entetes, &jeton) {
                    return (StatusCode::UNAUTHORIZED, "jeton manquant").into_response();
                }
                tokio::task::spawn_blocking(move || f(q))
                    .await
                    .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "erreur").into_response())
            }
        }
    };
    let entrer = {
        let jeton = Arc::clone(&jeton);
        move |entetes: HeaderMap, Query(q): Query<Jeton>| {
            let jeton = Arc::clone(&jeton);
            async move { entrer(&entetes, &q.j, &jeton) }
        }
    };
    Router::new()
        .route("/", get(|entetes: HeaderMap| async move { statique(&entetes, PAGE, "text/html; charset=utf-8") }))
        .route("/icone.svg", get(|entetes: HeaderMap| async move { statique(&entetes, ICONE, "image/svg+xml") }))
        .route(
            "/manifest.webmanifest",
            get(|entetes: HeaderMap| async move { statique(&entetes, MANIFESTE, "application/manifest+json") }),
        )
        .route("/api/entrer", get(entrer))
        .route("/api/chercher", get(j(chercher)))
        .route("/api/vignette", get(j(|q| image(q, 480, 0.6))))
        .route("/api/image", get(j(|q| image(q, 1800, 0.75))))
        .route("/api/preparer", get(j(preparer)))
        .route("/api/demander", get(j(demander)))
        .route("/api/reponse", get(j(reponse)))
        .route("/api/arreter", get(j(arreter_question)))
}

#[derive(Deserialize)]
struct Jeton {
    j: String,
}

fn egaux(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hote_permis(entetes: &HeaderMap) -> bool {
    let Some(hote) = entetes.get(header::HOST).and_then(|h| h.to_str().ok()) else { return false };
    let nom = hote.rsplit_once(':').map_or(hote, |(n, p)| if p.chars().all(|c| c.is_ascii_digit()) { n } else { hote });
    HOTES.contains(&nom) || {
        let public = bscreen_core::reglages::Reglages::charger().page_web_nom;
        !public.is_empty() && public == nom
    }
}

fn cookie(entetes: &HeaderMap) -> Option<String> {
    entetes
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
}


/// Les clés publiques du cluster Teleport (P-256, non compressées), relues au plus toutes les heures.
fn cles_teleport(forcer: bool) -> Vec<Vec<u8>> {
    use base64::Engine;
    static CLES: Mutex<Option<(Instant, Vec<Vec<u8>>)>> = Mutex::new(None);
    let mut garde = CLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((quand, cles)) = garde.as_ref()
        && !forcer
        && quand.elapsed().as_secs() < 3600
        && !cles.is_empty()
    {
        return cles.clone();
    }
    let proxy = bscreen_core::reglages::Reglages::charger().page_web_proxy;
    if proxy.is_empty() {
        return Vec::new();
    }
    let adresse = format!("https://{proxy}/.well-known/jwks.json");
    let sortie = std::process::Command::new("/usr/bin/curl").args(["-s", "--max-time", "5", &adresse]).output();
    let cles: Vec<Vec<u8>> = sortie
        .ok()
        .and_then(|s| serde_json::from_slice::<Value>(&s.stdout).ok())
        .and_then(|v| v["keys"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|k| k["kty"] == "EC" && k["crv"] == "P-256")
        .filter_map(|k| {
            let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            let (x, y) = (b.decode(k["x"].as_str()?).ok()?, b.decode(k["y"].as_str()?).ok()?);
            (x.len() == 32 && y.len() == 32).then(|| [vec![4u8], x, y].concat())
        })
        .collect();
    if cles.is_empty() {
        tracing::warn!("page web : clés de Teleport illisibles");
    }
    *garde = Some((Instant::now(), cles.clone()));
    cles
}

/// Le jeton signé que Teleport ajoute à chaque requête : signature d'une clé du cluster, pas
/// expiré, et d'un compte permis.
fn teleport_valide(jwt: &str) -> bool {
    use base64::Engine;
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let morceaux: Vec<&str> = jwt.split('.').collect();
    let [entete, charge, signature] = morceaux[..] else { return false };
    let (Ok(charge_json), Ok(signature)) = (b.decode(charge), b.decode(signature)) else { return false };
    let message = format!("{entete}.{charge}");
    let verifier = |cles: &[Vec<u8>]| {
        cles.iter().any(|cle| {
            ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, cle)
                .verify(message.as_bytes(), &signature)
                .is_ok()
        })
    };
    // Une clé inconnue (rotation des clés du cluster) : relire les clés une fois.
    if !verifier(&cles_teleport(false)) && !verifier(&cles_teleport(true)) {
        tracing::warn!("page web : jeton de Teleport à la signature refusée");
        return false;
    }
    let Ok(revendications) = serde_json::from_slice::<Value>(&charge_json) else { return false };
    let maintenant = chrono::Utc::now().timestamp();
    let expire = revendications["exp"].as_i64().unwrap_or(0);
    let compte = revendications["username"].as_str().unwrap_or_default();
    let permis = bscreen_core::reglages::Reglages::charger().page_web_comptes;
    if expire <= maintenant || !permis.iter().any(|p| p == compte) {
        tracing::warn!(compte, "page web : jeton de Teleport refusé (expiré ou compte non permis)");
        return false;
    }
    true
}

fn autorise(entetes: &HeaderMap, jeton: &str) -> bool {
    if let Some(jwt) = entetes.get("teleport-jwt-assertion").and_then(|v| v.to_str().ok()) {
        return teleport_valide(jwt);
    }
    let porteur = entetes
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    porteur.or_else(|| cookie(entetes)).is_some_and(|j| egaux(j.as_bytes(), jeton.as_bytes()))
}

/// Ce que chaque réponse emporte : rien en cache, pas de cadre, pas de « referer ».
fn proteger(mut reponse: Response) -> Response {
    let e = reponse.headers_mut();
    e.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    e.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    e.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    e.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    reponse
}

fn statique(entetes: &HeaderMap, corps: &'static str, genre: &'static str) -> Response {
    if !hote_permis(entetes) {
        return (StatusCode::MISDIRECTED_REQUEST, "nom inconnu").into_response();
    }
    let mut r = ([(header::CONTENT_TYPE, genre)], corps).into_response();
    r.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'",
        ),
    );
    proteger(r)
}

/// Le jeton (donné une fois par l'adresse) devient un cookie.
fn entrer(entetes: &HeaderMap, donne: &str, jeton: &str) -> Response {
    if !hote_permis(entetes) || !egaux(donne.as_bytes(), jeton.as_bytes()) {
        tracing::warn!("page web : jeton refusé");
        return proteger((StatusCode::UNAUTHORIZED, "jeton refusé").into_response());
    }
    let mut r = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("{COOKIE}={jeton}; HttpOnly; SameSite=Strict; Path=/; Max-Age=31536000")) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    tracing::info!("page web : nouvel appareil entré");
    proteger(r)
}

fn json_(valeur: Value) -> Response {
    proteger(([(header::CONTENT_TYPE, "application/json; charset=utf-8")], valeur.to_string()).into_response())
}

fn base() -> Option<Base> {
    Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite")).ok()
}

// ---------------------------------------------------------------------------------------------
// Chercher.

/// Un extrait de `texte` autour du premier mot cherché trouvé (200 caractères environ).
fn extrait(texte: &str, requete: &str) -> String {
    let plat = texte.split_whitespace().collect::<Vec<_>>().join(" ");
    let minuscules = plat.to_lowercase();
    let debut = requete
        .split_whitespace()
        .filter(|m| !m.starts_with('@') && m.chars().count() >= 3)
        .filter_map(|m| minuscules.find(&m.to_lowercase()))
        .min()
        .unwrap_or(0);
    let caracteres: Vec<char> = plat.chars().collect();
    let rang = plat[..debut.min(plat.len())].chars().count();
    let (a, z) = (rang.saturating_sub(70), (rang + 130).min(caracteres.len()));
    let mut s: String = caracteres[a..z].iter().collect();
    if a > 0 {
        s.insert(0, '…');
    }
    if z < caracteres.len() {
        s.push('…');
    }
    s
}

fn carte(image: &Image, texte: &str, dit: bool, sens: bool) -> Value {
    json!({
        "id": image.id,
        "quand_ms": image.quand_ms,
        "quand": bscreen_api::heures::lisible(image.quand_ms),
        "app": image.app_nom.clone().unwrap_or_default(),
        "fenetre": image.fenetre.clone().unwrap_or_default(),
        "url": image.url.clone().unwrap_or_default(),
        "texte": texte,
        "dit": dit,
        "sens": sens,
        "nombre": 1,
    })
}

fn chercher(q: HashMap<String, String>) -> Response {
    let requete = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if requete.chars().count() < 2 {
        return json_(json!({ "endroits": [], "ms": 0 }));
    }
    let debut = Instant::now();
    let trouvees = crate::application::chercher_pour_la_frise(&requete);
    let Some(base) = base() else { return json_(json!({ "erreur": "base illisible" })) };
    let principal = bscreen_macos::fenetres::ecran_principal();
    let sites_caches = bscreen_core::reglages::Reglages::charger().sites_caches;
    let mut endroits: Vec<(String, i64, Value)> = Vec::new();
    for t in trouvees {
        let dit = t.ecran == 0;
        let ecran = if dit { principal } else { t.ecran };
        let Ok(Some(image)) = base.image_proche(t.debut_ms, ecran) else { continue };
        if image.privee {
            continue;
        }
        // Un site masqué dans la recherche (réglages).
        if image.url.as_deref().is_some_and(|u| bscreen_core::reglages::site_exclu(u, &sites_caches)) {
            continue;
        }
        let cle = if dit {
            "voix".to_string()
        } else {
            image.url.clone().or(image.fenetre.clone()).or(image.app_bundle.clone()).unwrap_or_default()
        };
        if let Some((_, _, carte)) = endroits
            .iter_mut()
            .find(|(c, quand, _)| *c == cle && (image.quand_ms - quand).abs() < MEME_ENDROIT_MS)
        {
            carte["nombre"] = json!(carte["nombre"].as_u64().unwrap_or(1) + 1);
            continue;
        }
        if endroits.len() >= ENDROITS_MAX {
            continue;
        }
        let texte = t.texte.as_deref().map(|x| extrait(x, &requete)).unwrap_or_default();
        endroits.push((cle, image.quand_ms, carte(&image, &texte, dit, t.sens)));
    }
    let ms = debut.elapsed().as_millis() as u64;
    tracing::info!(endroits = endroits.len(), ms, "page web : recherche");
    json_(json!({ "endroits": endroits.into_iter().map(|e| e.2).collect::<Vec<_>>(), "ms": ms }))
}

fn image(q: HashMap<String, String>, largeur: u32, qualite: f64) -> Response {
    let refus = || proteger((StatusCode::NOT_FOUND, "pas d'image").into_response());
    let Some(id) = q.get("id").and_then(|i| i.parse::<i64>().ok()) else { return refus() };
    let Some(base) = base() else { return refus() };
    let Some(image) = base.images_par_id(&[id]).ok().and_then(|mut v| v.pop()) else { return refus() };
    if image.privee {
        return refus();
    }
    match bscreen_macos::lecture::image_jpeg(&chemins::donnees().join(&image.fichier), image.position_ms, largeur, qualite) {
        Ok(octets) => {
            let mut r = ([(header::CONTENT_TYPE, "image/jpeg")], octets).into_response();
            r = proteger(r);
            // Une image enregistrée ne change pas : le navigateur peut la garder (en privé).
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
            r
        }
        Err(e) => {
            tracing::debug!(%e, id, "page web : image illisible");
            refus()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Demander (Ask) : la page pose la question, puis relit la réponse au fil.

struct Session {
    question: String,
    texte: String,
    lectures: Vec<String>,
    fini: Option<(Option<String>, f64)>,
    arret: Option<Arret>,
    cree: Instant,
}

static SESSIONS: Mutex<Option<HashMap<u64, Session>>> = Mutex::new(None);
static NUMERO: AtomicU64 = AtomicU64::new(1);

fn sessions<T>(f: impl FnOnce(&mut HashMap<u64, Session>) -> T) -> T {
    let mut garde = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(garde.get_or_insert_with(HashMap::new))
}

fn preparer(_q: HashMap<String, String>) -> Response {
    crate::ask::preparer();
    json_(json!({ "ok": true }))
}

fn demander(q: HashMap<String, String>) -> Response {
    let question = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if question.is_empty() {
        return json_(json!({ "erreur": "question vide" }));
    }
    let suite = q.get("suite").and_then(|s| s.parse::<u64>().ok());
    let id = NUMERO.fetch_add(1, Ordering::SeqCst);
    let precedent = sessions(|s| {
        // Les vieilles questions s'en vont (30 min).
        s.retain(|_, v| v.cree.elapsed().as_secs() < 1800);
        let precedent = suite.and_then(|p| s.get(&p)).filter(|p| p.fini.is_some()).map(|p| (p.question.clone(), p.texte.clone()));
        s.insert(id, Session { question: question.clone(), texte: String::new(), lectures: Vec::new(), fini: None, arret: None, cree: Instant::now() });
        precedent
    });
    tracing::info!(caracteres = question.chars().count(), suite = precedent.is_some(), "page web : question posée");
    let envoi: Box<dyn Fn(Evenement) + Send> = Box::new(move |e: Evenement| {
        sessions(|s| {
            let Some(session) = s.get_mut(&id) else { return };
            match e {
                Evenement::Lecture { quoi, .. } => session.lectures.push(quoi),
                Evenement::Envoye { .. } => {}
                Evenement::Morceau(m) => session.texte.push_str(&m),
                Evenement::Fini { erreur, secondes } => session.fini = Some((erreur, secondes)),
            }
        });
    });
    match crate::ask::demander(&question, precedent.as_ref().map(|(a, b)| (a.as_str(), b.as_str())), envoi) {
        Ok(arret) => sessions(|s| {
            if let Some(session) = s.get_mut(&id) {
                session.arret = Some(arret);
            }
        }),
        Err(e) => sessions(|s| {
            if let Some(session) = s.get_mut(&id) {
                session.fini = Some((Some(e.to_string()), 0.0));
            }
        }),
    }
    json_(json!({ "id": id }))
}

/// Les renvois d'une réponse finie (« écran 20/11 21:56 ») et l'image de chacun (jamais privée).
fn liens(texte: &str) -> Vec<Value> {
    let Some(base) = base() else { return Vec::new() };
    let principal = bscreen_macos::fenetres::ecran_principal();
    let mut liens = Vec::new();
    for ligne in bscreen_core::reponse::analyser(texte) {
        for (place, renvoi) in &ligne.renvois {
            let Some(quand) = bscreen_ui::ask::quand_du_renvoi(renvoi) else { continue };
            let image = base.image_proche(quand, principal).ok().flatten().filter(|i| !i.privee);
            liens.push(json!({
                "texte": ligne.texte.get(place.clone()).unwrap_or_default(),
                "audio": renvoi.audio,
                "image": image.as_ref().map(|i| i.id),
                "quand": bscreen_api::heures::lisible(quand),
            }));
        }
    }
    liens
}

fn reponse(q: HashMap<String, String>) -> Response {
    let Some(id) = q.get("id").and_then(|i| i.parse::<u64>().ok()) else { return json_(json!({ "erreur": "question inconnue" })) };
    let etat = sessions(|s| s.get(&id).map(|v| (v.texte.clone(), v.lectures.clone(), v.fini.clone())));
    let Some((texte, lectures, fini)) = etat else { return json_(json!({ "erreur": "question inconnue" })) };
    let mut v = json!({ "texte": texte, "lectures": lectures, "fini": fini.is_some() });
    if let Some((erreur, secondes)) = fini {
        v["erreur"] = json!(erreur);
        v["secondes"] = json!(secondes);
        v["liens"] = json!(liens(&texte));
    }
    json_(v)
}

fn arreter_question(q: HashMap<String, String>) -> Response {
    if let Some(id) = q.get("id").and_then(|i| i.parse::<u64>().ok())
        && let Some(arret) = sessions(|s| s.get_mut(&id).and_then(|v| v.arret.take()))
    {
        (arret.0)();
    }
    json_(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotes_et_jetons() {
        let mut e = HeaderMap::new();
        e.insert(header::HOST, HeaderValue::from_static("127.0.0.1:47338"));
        assert!(hote_permis(&e));
        e.insert(header::HOST, HeaderValue::from_static("attaquant.example:47338"));
        assert!(!hote_permis(&e));
        e.insert(header::COOKIE, HeaderValue::from_static("autre=1; bscreen_web=abc"));
        assert!(autorise(&e, "abc"));
        assert!(!autorise(&e, "abd"));
        assert!(!autorise(&HeaderMap::new(), "abc"));
    }

    #[test]
    fn extraits() {
        let texte = format!("{} la voiture rouge est garée {}", "début ".repeat(40), "fin ".repeat(60));
        let e = extrait(&texte, "voiture");
        assert!(e.starts_with('…') && e.ends_with('…') && e.contains("voiture rouge"), "{e}");
        assert_eq!(extrait("court", "rien"), "court");
    }
}
