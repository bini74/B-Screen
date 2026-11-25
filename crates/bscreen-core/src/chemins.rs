//! Où B-Screen range ses fichiers.

use std::path::PathBuf;

fn maison() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn support() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| maison().join("Library/Application Support"))
}

/// Dossier des données : dans l'image chiffrée une fois la migration faite
/// (`B-Screen-volume`), sinon `~/Library/Application Support/B-Screen` (en clair).
pub fn donnees() -> PathBuf {
    if chiffrement_actif() { point_de_montage() } else { dossier_clair() }
}

/// L'ancien dossier, en clair (avant la migration vers l'image chiffrée).
pub fn dossier_clair() -> PathBuf {
    support().join(crate::NOM)
}

/// L'image disque chiffrée (APFS, AES-256) qui contient les données.
pub fn image_chiffree() -> PathBuf {
    support().join("B-Screen-chiffre.sparsebundle")
}

/// Où l'image chiffrée est ouverte (invisible dans le Finder).
pub fn point_de_montage() -> PathBuf {
    support().join("B-Screen-volume")
}

/// Fichier vide posé à la fin de la migration : sa présence fait passer B-Screen sur l'image chiffrée.
pub fn marque_chiffrement() -> PathBuf {
    support().join(".b-screen-chiffre")
}

/// La migration vers l'image chiffrée est faite.
pub fn chiffrement_actif() -> bool {
    marque_chiffrement().exists()
}

/// Dossier des journaux : `~/Library/Logs/B-Screen`.
pub fn journaux() -> PathBuf {
    maison().join("Library/Logs").join(crate::NOM)
}

/// Fichier des réglages.
pub fn reglages() -> PathBuf {
    donnees().join("reglages.json")
}
