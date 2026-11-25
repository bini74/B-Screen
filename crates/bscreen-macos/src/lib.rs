//! Couche système macOS de B-Screen.
//!
//! Tout ce qui parle à macOS est ici ; le reste du code n'en dépend pas,
//! pour pouvoir un jour porter B-Screen ailleurs en réécrivant seulement cette couche.

pub mod api_locale;
pub mod appels;
pub mod archive;
pub mod app;
pub mod audio;
pub mod autorisations;
pub mod barre;
pub mod capture;
pub mod classement;
pub mod clavier;
pub mod coffre;
pub mod connexion;
pub mod declencheurs;
pub mod effacement;
pub mod dictee;
pub mod empreintes;
pub mod encodage;
pub mod fenetres;
pub mod gestes;
pub mod images;
pub mod indexation;
pub mod lecture;
pub mod mesures;
pub mod moteur;
pub mod mots;
pub mod ocr;
pub mod ollama;
pub mod prive;
pub mod rattrapage;
pub mod resumes;
pub mod sens;
pub mod sous_entendus;
pub mod suite;
pub mod texte;
pub mod touchid;
pub mod transcription;
pub mod vivants;
