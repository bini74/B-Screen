//! L'API HTTP locale (phase 9.1) dans l'app : allumée seulement si Julien la coche dans le menu.

use std::sync::Mutex;

use bscreen_core::chemins;

static ARRET: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

fn fichier_jeton() -> std::path::PathBuf {
    chemins::donnees().join("api-jeton")
}

/// Démarre l'API sur 127.0.0.1:`port` (si elle ne tourne pas déjà).
pub fn demarrer(port: u16) {
    let mut arret = ARRET.lock().unwrap_or_else(|e| e.into_inner());
    if arret.is_some() {
        return;
    }
    let jeton = match bscreen_api::http::jeton(&fichier_jeton()) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(%e, "jeton de l'API impossible");
            return;
        }
    };
    let (envoi, reception) = tokio::sync::oneshot::channel();
    *arret = Some(envoi);
    let _ = std::thread::Builder::new().name("bscreen-api".into()).spawn(move || {
        tracing::info!(port, "API locale démarrée (127.0.0.1)");
        if let Err(e) = bscreen_api::http::lancer(port, &chemins::donnees().join("index.sqlite"), jeton, reception) {
            tracing::warn!(%e, "API locale arrêtée");
        }
        *ARRET.lock().unwrap_or_else(|e| e.into_inner()) = None;
    });
}

/// Arrête l'API (les demandes en cours se terminent).
pub fn arreter() {
    if let Some(envoi) = ARRET.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = envoi.send(());
        tracing::info!("API locale arrêtée");
    }
}

/// Copie le jeton dans le presse-papiers (fil principal). Le jeton n'est écrit nulle part ailleurs.
pub fn copier_jeton() -> bool {
    let Ok(jeton) = bscreen_api::http::jeton(&fichier_jeton()) else { return false };
    let presse_papiers = objc2_app_kit::NSPasteboard::generalPasteboard();
    presse_papiers.clearContents();
    let genre = unsafe { objc2_app_kit::NSPasteboardTypeString };
    presse_papiers.setString_forType(&objc2_foundation::NSString::from_str(&jeton), genre)
}
