//! Cœur portable de B-Screen pour les autres programmes (phase 9).
//!
//! - [`mcp`] : serveur MCP en lecture seule, pour que Claude Code (ou un autre agent) fouille la
//!   mémoire — rien n'est jamais modifié, les moments privés ne sortent pas, et le texte est celui
//!   déjà masqué (aucun secret).
//! - [`export`] : une journée en JSON Lines, au format « moment » du plan.
//! - [`http`] : API HTTP locale (127.0.0.1, jeton), éteinte par défaut.

pub mod contexte;
pub mod corrections;
pub mod episodes;
pub mod export;
pub mod heures;
pub mod http;
pub mod mcp;
pub mod personnes;
pub mod sous_entendus;
