//! Cœur portable de B-Screen.
//!
//! Rien ici ne dépend de macOS : ce sont les types et les règles communes
//! (format des moments, réglages, emplacement des données).

pub mod chemins;
pub mod image;
pub mod index_compact;
pub mod masquage;
pub mod reglages;
pub mod reponse;
pub mod texte_lu;
pub mod vocabulaire;
pub mod voix_familieres;

/// Nom affiché de l'application.
pub const NOM: &str = "B-Screen";

/// Identifiant du paquet macOS (le même que dans `Info.plist`).
pub const IDENTIFIANT: &str = "com.julienbonzi.bscreen";

/// Version de l'application.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
