//! Ouverture de B-Screen à la connexion, par SMAppService (Réglages › Général › Ouverture).

use anyhow::{Result, anyhow};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EtatConnexion {
    Active,
    /// Enregistrée mais Julien doit l'approuver dans les Réglages.
    AApprouver,
    Inactive,
    /// L'app ne tourne pas depuis un paquet `.app` (par exemple `cargo run`).
    Indisponible,
}

pub fn etat() -> EtatConnexion {
    let statut = unsafe { SMAppService::mainAppService().status() };
    if statut == SMAppServiceStatus::Enabled {
        EtatConnexion::Active
    } else if statut == SMAppServiceStatus::RequiresApproval {
        EtatConnexion::AApprouver
    } else if statut == SMAppServiceStatus::NotRegistered {
        EtatConnexion::Inactive
    } else {
        EtatConnexion::Indisponible
    }
}

pub fn activer() -> Result<()> {
    unsafe { SMAppService::mainAppService().registerAndReturnError() }
        .map_err(|e| anyhow!("ouverture à la connexion impossible : {}", e.localizedDescription()))
}

pub fn desactiver() -> Result<()> {
    unsafe { SMAppService::mainAppService().unregisterAndReturnError() }.map_err(|e| {
        anyhow!(
            "retrait de l'ouverture à la connexion impossible : {}",
            e.localizedDescription()
        )
    })
}

/// Ouvre Réglages › Général › Ouverture.
pub fn ouvrir_reglages() {
    unsafe { SMAppService::openSystemSettingsLoginItems() };
}
