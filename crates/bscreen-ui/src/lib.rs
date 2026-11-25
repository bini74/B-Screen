//! Interface de B-Screen (gpui) : la frise, puis Ask et les réglages.
//!
//! Cette couche ne parle pas à macOS directement : ce dont elle a besoin
//! (images enregistrées, icônes, réglages de fenêtre) lui est fourni par l'app
//! à travers [`frise::SourceFrise`], [`reglages::SourceReglages`] et [`ask::SourceAsk`].

pub mod ask;
pub mod frise;
pub mod reglages;
