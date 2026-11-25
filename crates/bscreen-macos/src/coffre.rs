//! Image disque chiffrée (phase 6.1) : B-Screen range toutes ses données dans une image APFS
//! chiffrée en AES-256, ouverte à son lancement avec la clé rangée dans le Trousseau.
//!
//! - Tant que la migration n'est pas faite (`chemins::chiffrement_actif()` faux), rien ne change.
//! - La clé n'est jamais écrite dans un fichier ni dans le journal.
//! - Sans clé dans le Trousseau, l'app demande la clé de secours une fois, puis l'y range.

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use bscreen_core::chemins;

const SERVICE: &str = "B-Screen chiffrement";
const COMPTE: &str = "bscreen";

/// Alphabet de la clé (Crockford : sans I, L, O, U).
const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Remet une clé tapée à la main dans sa forme exacte : majuscules, sans espaces,
/// O → 0, I et L → 1, puis des groupes de 5 séparés par des tirets.
/// `None` si ce n'est pas une clé de 40 caractères.
pub fn normaliser(tapee: &str) -> Option<String> {
    let mut nette = String::new();
    for c in tapee.chars() {
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c if c.is_whitespace() || c == '-' => continue,
            c => c,
        };
        if !ALPHABET.contains(c) {
            return None;
        }
        nette.push(c);
    }
    if nette.chars().count() != 40 {
        return None;
    }
    let groupes: Vec<&str> = nette.as_bytes().chunks(5).filter_map(|g| std::str::from_utf8(g).ok()).collect();
    Some(groupes.join("-"))
}

/// L'image est-elle ouverte au bon endroit ? (le point de montage n'est pas sur le même disque
/// que son dossier parent)
pub fn est_ouverte() -> bool {
    est_un_volume(&chemins::point_de_montage())
}

fn est_un_volume(dossier: &Path) -> bool {
    let (Ok(lui), Some(Ok(parent))) = (dossier.metadata(), dossier.parent().map(Path::metadata)) else {
        return false;
    };
    lui.dev() != parent.dev()
}

/// La clé rangée dans le Trousseau, s'il y en a une.
pub fn cle_du_trousseau() -> Option<String> {
    let octets = security_framework::passwords::get_generic_password(SERVICE, COMPTE).ok()?;
    String::from_utf8(octets).ok()
}

/// Range la clé dans le Trousseau (réservée à B-Screen).
pub fn ranger_cle(cle: &str) -> Result<()> {
    security_framework::passwords::set_generic_password(SERVICE, COMPTE, cle.as_bytes())
        .context("impossible de ranger la clé dans le Trousseau")
}

/// Ouvre l'image chiffrée au point de montage, invisible dans le Finder.
/// La clé passe par l'entrée standard de `hdiutil`, jamais par la ligne de commande.
pub fn monter(cle: &str) -> Result<()> {
    if est_ouverte() {
        return Ok(());
    }
    let image = chemins::image_chiffree();
    if !image.exists() {
        bail!("image chiffrée introuvable : {}", image.display());
    }
    let point = chemins::point_de_montage();
    std::fs::create_dir_all(&point)?;
    let mut hdiutil = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-quiet", "-stdinpass", "-nobrowse", "-noautoopen", "-owners", "on", "-mountpoint"])
        .arg(&point)
        .arg(&image)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("hdiutil introuvable")?;
    if let Some(mut entree) = hdiutil.stdin.take() {
        entree.write_all(cle.as_bytes())?;
        entree.write_all(b"\0")?;
    }
    let sortie = hdiutil.wait_with_output()?;
    if !sortie.status.success() || !est_ouverte() {
        let message = String::from_utf8_lossy(&sortie.stderr);
        if message.to_lowercase().contains("authentication") {
            bail!("clé refusée");
        }
        bail!("ouverture de l'image impossible : {}", message.trim());
    }
    tracing::info!("image chiffrée ouverte");
    Ok(())
}

/// Ferme l'image (à la fin d'un essai ; l'app, elle, la laisse ouverte).
pub fn demonter() -> Result<()> {
    if !est_ouverte() {
        return Ok(());
    }
    let statut = Command::new("/usr/bin/hdiutil").arg("detach").arg("-quiet").arg(chemins::point_de_montage()).status()?;
    if !statut.success() {
        bail!("fermeture de l'image impossible");
    }
    Ok(())
}

/// Au lancement : rien à faire tant que la migration n'est pas faite ; sinon ouvre l'image avec
/// la clé du Trousseau. Sans clé rangée : `interactif` → demande la clé de secours (fil
/// principal, jusqu'à 3 essais) et la range ; sinon (commandes, MCP) → erreur.
pub fn ouvrir(interactif: bool) -> Result<()> {
    if !chemins::chiffrement_actif() || est_ouverte() {
        return Ok(());
    }
    if let Some(cle) = cle_du_trousseau() {
        match monter(&cle) {
            Ok(()) => return Ok(()),
            Err(e) if !interactif => return Err(e),
            Err(e) => tracing::warn!(%e, "la clé du Trousseau n'ouvre pas l'image"),
        }
    }
    if !interactif {
        bail!("les données de B-Screen sont fermées : ouvre B-Screen.app une fois pour donner la clé");
    }
    let mut message = "Tes données sont dans une image chiffrée. Tape ta clé de secours (8 groupes de 5) : B-Screen la rangera dans le Trousseau et ne la redemandera plus.".to_string();
    for _ in 0..3 {
        let Some(tapee) = demander_cle(&message) else { bail!("clé non donnée") };
        let Some(cle) = normaliser(&tapee) else {
            message = "Ce n'est pas une clé de secours : 40 lettres et chiffres, en 8 groupes de 5.".into();
            continue;
        };
        match monter(&cle) {
            Ok(()) => {
                if let Err(e) = ranger_cle(&cle) {
                    tracing::warn!(%e, "clé non rangée : elle sera redemandée au prochain lancement");
                }
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(%e, "clé de secours refusée");
                message = "Cette clé n'ouvre pas l'image. Vérifie-la et réessaie.".into();
            }
        }
    }
    bail!("trois clés refusées")
}

/// Fenêtre de saisie de la clé (champ masqué). Fil principal.
fn demander_cle(message: &str) -> Option<String> {
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSSecureTextField};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    let mtm = objc2::MainThreadMarker::new()?;
    let alerte = NSAlert::new(mtm);
    alerte.setMessageText(&NSString::from_str("Ouvrir les données de B-Screen"));
    alerte.setInformativeText(&NSString::from_str(message));
    alerte.addButtonWithTitle(&NSString::from_str("Ouvrir"));
    alerte.addButtonWithTitle(&NSString::from_str("Quitter"));
    let champ = NSSecureTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 24.0)),
    );
    champ.setPlaceholderString(Some(&NSString::from_str("XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX")));
    alerte.setAccessoryView(Some(&champ));
    alerte.window().setInitialFirstResponder(Some(&champ));
    NSApplication::sharedApplication(mtm).activate();
    if alerte.runModal() != NSAlertFirstButtonReturn {
        return None;
    }
    Some(champ.stringValue().to_string())
}

/// Liste (chemin relatif, taille) de tous les fichiers d'un dossier, triée.
fn inventaire(racine: &Path) -> Result<Vec<(PathBuf, u64)>> {
    fn parcourir(racine: &Path, dossier: &Path, liste: &mut Vec<(PathBuf, u64)>) -> Result<()> {
        for entree in std::fs::read_dir(dossier)? {
            let entree = entree?;
            // Dossiers système d'un volume (réservés à root) : ni lus ni comptés.
            if dossier == racine
                && matches!(entree.file_name().to_str(), Some(".fseventsd" | ".Spotlight-V100" | ".Trashes"))
            {
                continue;
            }
            let chemin = entree.path();
            let meta = entree.metadata()?;
            if meta.is_dir() {
                parcourir(racine, &chemin, liste)?;
            } else if meta.is_file() {
                liste.push((chemin.strip_prefix(racine)?.to_path_buf(), meta.len()));
            }
        }
        Ok(())
    }
    let mut liste = Vec::new();
    parcourir(racine, racine, &mut liste)?;
    liste.retain(|(p, _)| {
        !p.starts_with(".fseventsd") && !p.starts_with(".Spotlight-V100") && !p.starts_with(".Trashes")
    });
    liste.sort();
    Ok(liste)
}

/// Migration (B-Screen arrêté) : copie le dossier en clair dans l'image chiffrée, vérifie que
/// tout est là (mêmes fichiers, mêmes tailles), range la clé dans le Trousseau, puis pose la
/// marque qui fait passer B-Screen sur l'image. Les originaux en clair ne sont PAS effacés.
pub fn migrer(cle: &str) -> Result<usize> {
    if chemins::chiffrement_actif() {
        bail!("la migration est déjà faite");
    }
    let clair = chemins::dossier_clair();
    if !clair.join("index.sqlite").exists() {
        bail!("rien à migrer dans {}", clair.display());
    }
    monter(cle)?;
    let point = chemins::point_de_montage();
    let statut = Command::new("/usr/bin/ditto").arg(&clair).arg(&point).status()?;
    if !statut.success() {
        bail!("copie interrompue");
    }
    let avant = inventaire(&clair)?;
    let apres = inventaire(&point)?;
    if avant != apres {
        bail!(
            "la copie ne correspond pas ({} fichiers en clair, {} dans l'image) : rien n'est changé",
            avant.len(),
            apres.len()
        );
    }
    ranger_cle(cle)?;
    std::fs::write(chemins::marque_chiffrement(), b"")?;
    Ok(avant.len())
}

/// Après vérification par Julien : efface le dossier en clair (la copie chiffrée doit être
/// en service, ouverte, et contenir tous les fichiers).
pub fn effacer_clair() -> Result<usize> {
    if !chemins::chiffrement_actif() || !est_ouverte() {
        bail!("l'image chiffrée doit être en service et ouverte");
    }
    let clair = chemins::dossier_clair();
    if !clair.exists() {
        return Ok(0);
    }
    let avant = inventaire(&clair)?;
    let image = inventaire(&chemins::point_de_montage())?;
    let manquants = avant.iter().filter(|(p, _)| !image.iter().any(|(q, _)| q == p)).count();
    if manquants > 0 {
        bail!("{manquants} fichier(s) en clair absents de l'image : rien n'est effacé");
    }
    std::fs::remove_dir_all(&clair)?;
    Ok(avant.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Clé inventée pour les essais (aucun rapport avec une vraie clé).
    const FAUSSE: &str = "AAAAA-BBBBB-CCCCC-DDDDD-EEEEE-FFFFF-GGGGG-11111";

    #[test]
    fn cle_tapee_a_la_main() {
        assert_eq!(normaliser(FAUSSE).as_deref(), Some(FAUSSE));
        let tapee = "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg IlI1i";
        assert_eq!(normaliser(tapee).as_deref(), Some(FAUSSE));
        assert_eq!(normaliser("trop-court"), None);
        assert_eq!(normaliser(&FAUSSE.replace('A', "U")), None);
    }

    #[test]
    fn dossier_ordinaire_pas_un_volume() {
        assert!(!est_un_volume(&std::env::temp_dir()));
    }
}
