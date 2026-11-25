//! L'archive sur un serveur de la maison (phase 10). Les vidéos et la voix plus vieilles que
//! `archive_jours` (30 par défaut) partent sur une machine joignable par Teleport (réglages
//! `archive_proxy` et `archive_hote`), chiffrées sur le Mac (AES-256-GCM, clé tirée de la clé de
//! secours de l'image chiffrée : elle seule permet de les relire), par un robot Teleport dédié
//! (identité dans `~/.tbot/bscreen/identity`, un compte sans root sur cette seule machine).
//!
//! - Avant de partir, chaque vidéo laisse sur le Mac une vignette HEIC par scène (et au moins une par
//!   minute) : la frise montre toujours ces jours-là, et l'index et le texte restent.
//! - Une journée part d'un seul flux (tar dans `tsh ssh`), puis chaque fichier est vérifié par son
//!   empreinte sur la VM avant que l'original soit effacé du Mac.
//! - Revoir ou réécouter un moment archivé le ramène (vérifié, déchiffré) dans un cache de 2 Go.
//! - Effacer des images d'une vidéo archivée la ramène à sa place d'abord ; elle repart ensuite.
//! - Chaque nuit, sur secteur et Mac inactif, comme les résumés.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use bscreen_core::chemins;
use bscreen_store::{AArchiver, Base};
use sha2::{Digest, Sha256};

const TSH: &str = "/opt/homebrew/bin/tsh";
const DISTANT: &str = "/srv/archive/b-screen";
/// Le début d'un fichier chiffré de l'archive.
const MAGIE: &[u8; 4] = b"BSA1";
const LARGEUR_VIGNETTE: u32 = 480;
/// Le cache des fichiers ramenés de l'archive ne dépasse pas ça.
const CACHE_MAX: u64 = 2 << 30;
const CLE_DERNIERE: &str = "archive_derniere";

fn maintenant_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn identite() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".tbot/bscreen/identity")
}

/// Le proxy et la machine de l'archive (réglages), s'ils sont donnés.
fn destination() -> Option<(String, String)> {
    let r = bscreen_core::reglages::Reglages::charger();
    (!r.archive_proxy.is_empty() && !r.archive_hote.is_empty()).then_some((r.archive_proxy, r.archive_hote))
}

/// Le robot de B-Screen est-il installé sur ce Mac, et l'archive réglée ?
pub fn disponible() -> bool {
    identite().is_file() && Path::new(TSH).is_file() && destination().is_some()
}

/// L'archive est-elle en service ? (alors la conservation ne s'applique plus : rien n'est effacé)
pub fn active() -> bool {
    bscreen_core::reglages::Reglages::charger().archive_jours > 0 && disponible()
}

fn hote() -> String {
    destination().map(|d| d.1).unwrap_or_default()
}

fn tsh() -> Command {
    let (proxy, _) = destination().unwrap_or_default();
    let mut commande = Command::new(TSH);
    commande.arg("-i").arg(identite()).arg(format!("--proxy={proxy}"));
    commande
}

/// Un chemin relatif sûr à passer à la VM (pas de guillemet, pas de remontée).
fn chemin_sur(relatif: &str) -> Result<&str> {
    let sur = !relatif.is_empty()
        && !relatif.contains("..")
        && relatif.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
    if sur { Ok(relatif) } else { bail!("chemin refusé pour l'archive : {relatif}") }
}

/// La clé de l'archive, tirée de la clé de secours de l'image chiffrée (HKDF-SHA256). Le Trousseau
/// n'est lu qu'une fois par lancement (hors de l'app, chaque lecture peut demander la permission).
pub fn cle() -> Result<[u8; 32]> {
    static CLE: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    if let Some(cle) = CLE.get() {
        return Ok(*cle);
    }
    let secours = crate::coffre::cle_du_trousseau().context("clé de secours absente du Trousseau")?;
    let cle = cle_depuis(&secours)?;
    Ok(*CLE.get_or_init(|| cle))
}

fn cle_depuis(secours: &str) -> Result<[u8; 32]> {
    let hk = hkdf::Hkdf::<Sha256>::new(Some(b"B-Screen archive"), secours.as_bytes());
    let mut cle = [0u8; 32];
    hk.expand(b"v1", &mut cle).map_err(|_| anyhow!("clé d'archive impossible"))?;
    Ok(cle)
}

/// Chiffre un fichier pour l'archive : « BSA1 », 12 octets de nonce, puis le chiffré (AES-256-GCM),
/// lié à son chemin (un fichier renommé ne se relit pas).
pub fn chiffrer(relatif: &str, clair: &[u8], cle: &[u8; 32]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; 12];
    security_framework::random::SecRandom::default()
        .copy_bytes(&mut nonce)
        .map_err(|_| anyhow!("hasard indisponible"))?;
    let chiffre = Aes256Gcm::new(cle.into())
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: clair, aad: relatif.as_bytes() })
        .map_err(|_| anyhow!("chiffrement impossible : {relatif}"))?;
    let mut sortie = Vec::with_capacity(MAGIE.len() + nonce.len() + chiffre.len());
    sortie.extend_from_slice(MAGIE);
    sortie.extend_from_slice(&nonce);
    sortie.extend(chiffre);
    Ok(sortie)
}

pub fn dechiffrer(relatif: &str, chiffre: &[u8], cle: &[u8; 32]) -> Result<Vec<u8>> {
    if chiffre.len() < 32 || &chiffre[..4] != MAGIE {
        bail!("fichier d'archive illisible : {relatif}");
    }
    Aes256Gcm::new(cle.into())
        .decrypt(Nonce::from_slice(&chiffre[4..16]), Payload { msg: &chiffre[16..], aad: relatif.as_bytes() })
        .map_err(|_| anyhow!("fichier d'archive altéré, ou mauvaise clé : {relatif}"))
}

/// L'empreinte SHA-256 (hexadécimale) d'un contenu.
pub fn empreinte_de(octets: &[u8]) -> String {
    empreinte(octets)
}

fn empreinte(octets: &[u8]) -> String {
    Sha256::digest(octets).iter().map(|o| format!("{o:02x}")).collect()
}

/// Ce que fait (ou ferait) un passage de l'archivage.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Bilan {
    pub jours: usize,
    pub fichiers: usize,
    /// Taille des originaux partis du Mac.
    pub octets: u64,
    pub vignettes: usize,
    /// Fichiers gardés sur le Mac : l'envoi ou la vérification a échoué.
    pub gardes: usize,
    /// Temps passé (ms) : vignettes, chiffrement, envoi, vérification.
    pub ms: [u128; 4],
}

/// Archive les fichiers terminés avant `avant_ms`, au plus `jours_max` journées (les plus anciennes
/// d'abord) ; à blanc : rien n'est envoyé ni effacé, seulement compté.
pub fn archiver(avant_ms: i64, jours_max: usize, a_blanc: bool) -> Result<Bilan> {
    let donnees = chemins::donnees();
    let base = Base::ouvrir(&donnees.join("index.sqlite")).context("base")?;
    let fichiers = base.fichiers_a_archiver(avant_ms)?;
    archiver_ces(&base, fichiers, jours_max, a_blanc)
}

/// Archive ces fichiers (ceux qui sont encore sur le Mac), journée par journée.
pub fn archiver_ces(base: &Base, fichiers: Vec<AArchiver>, jours_max: usize, a_blanc: bool) -> Result<Bilan> {
    let donnees = chemins::donnees();
    let mut par_jour: BTreeMap<String, Vec<AArchiver>> = BTreeMap::new();
    for f in fichiers {
        if donnees.join(&f.fichier).is_file() && chemin_sur(&f.fichier).is_ok() && !f.jour.is_empty() {
            par_jour.entry(f.jour.clone()).or_default().push(f);
        }
    }
    let mut bilan = Bilan::default();
    if a_blanc {
        for fichiers in par_jour.values().take(jours_max) {
            bilan.jours += 1;
            bilan.fichiers += fichiers.len();
            bilan.octets += fichiers.iter().filter_map(|f| fs::metadata(donnees.join(&f.fichier)).ok()).map(|m| m.len()).sum::<u64>();
        }
        return Ok(bilan);
    }
    if !disponible() {
        bail!("le robot Teleport de B-Screen n'est pas installé (~/.tbot/bscreen/identity)");
    }
    let cle = cle()?;
    for (jour, fichiers) in par_jour.into_iter().take(jours_max) {
        archiver_jour(base, &cle, &jour, &fichiers, &mut bilan)?;
        bilan.jours += 1;
    }
    if bilan.fichiers > 0 {
        base.noter_meta(CLE_DERNIERE, &maintenant_ms().to_string())?;
    }
    Ok(bilan)
}

fn archiver_jour(base: &Base, cle: &[u8; 32], jour: &str, fichiers: &[AArchiver], bilan: &mut Bilan) -> Result<()> {
    let donnees = chemins::donnees();
    let t = std::time::Instant::now();
    // 1. Les vignettes, tant que la vidéo est là.
    for f in fichiers {
        let Some(morceau) = f.morceau else { continue };
        let video = donnees.join(&f.fichier);
        for (image, position, _) in base.images_pour_vignettes(morceau)? {
            let relatif = format!("vignettes/{jour}/{image}.heic");
            let chemin = donnees.join(&relatif);
            if !chemin.is_file() {
                match crate::lecture::vignette_heic(&video, position, LARGEUR_VIGNETTE) {
                    Ok(octets) => {
                        fs::create_dir_all(chemin.parent().unwrap_or(&donnees))?;
                        fs::write(&chemin, octets)?;
                    }
                    Err(e) => {
                        tracing::debug!(%e, image, "vignette impossible");
                        continue;
                    }
                }
            }
            base.ajouter_vignette(image, morceau, position, &relatif)?;
            bilan.vignettes += 1;
        }
    }
    bilan.ms[0] += t.elapsed().as_millis();
    let t = std::time::Instant::now();
    // 2. Chiffrés dans le dossier d'envoi.
    let envoi = donnees.join("archive-envoi");
    let mut prets: Vec<(String, u64, String, u64)> = Vec::new();
    for f in fichiers {
        let clair = fs::read(donnees.join(&f.fichier)).with_context(|| format!("lecture de {}", f.fichier))?;
        let chiffre = chiffrer(&f.fichier, &clair, cle)?;
        let cible = envoi.join(format!("{}.bsa", f.fichier));
        fs::create_dir_all(cible.parent().unwrap_or(&envoi))?;
        fs::write(&cible, &chiffre)?;
        prets.push((f.fichier.clone(), chiffre.len() as u64, empreinte(&chiffre), clair.len() as u64));
    }
    bilan.ms[1] += t.elapsed().as_millis();
    let t = std::time::Instant::now();
    // 3. Un seul flux pour la journée.
    let noms: Vec<String> = prets.iter().map(|p| format!("{}.bsa", p.0)).collect();
    let mut tar = Command::new("/usr/bin/tar")
        .env("COPYFILE_DISABLE", "1")
        .arg("--no-mac-metadata")
        .arg("-C")
        .arg(&envoi)
        .arg("-cf")
        .arg("-")
        .args(&noms)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("tar")?;
    let flux = tar.stdout.take().context("tar sans sortie")?;
    let envoye = tsh()
        .args(["ssh", &hote(), &format!("mkdir -p {DISTANT} && tar -xf - -C {DISTANT}")])
        .stdin(Stdio::from(flux))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("tsh")?;
    let tar_ok = tar.wait().map(|s| s.success()).unwrap_or(false);
    if !envoye.status.success() || !tar_ok {
        let _ = fs::remove_dir_all(&envoi);
        bail!("envoi du {jour} refusé : {}", String::from_utf8_lossy(&envoye.stderr).trim());
    }
    bilan.ms[2] += t.elapsed().as_millis();
    let t = std::time::Instant::now();
    // 4. Vérifiés sur la VM, par leur empreinte, avant d'effacer quoi que ce soit.
    let verification = tsh()
        .args(["ssh", &hote(), &format!("cd {DISTANT} && sha256sum -- {}", noms.join(" "))])
        .stderr(Stdio::null())
        .output()
        .context("tsh")?;
    let recues: BTreeMap<String, String> = String::from_utf8_lossy(&verification.stdout)
        .lines()
        .filter_map(|l| l.split_once("  ").map(|(h, n)| (n.trim().to_string(), h.trim().to_string())))
        .collect();
    bilan.ms[3] += t.elapsed().as_millis();
    let quand = maintenant_ms();
    for (relatif, octets, attendue, taille) in &prets {
        if recues.get(&format!("{relatif}.bsa")) == Some(attendue) {
            base.noter_archive(relatif, *octets, attendue, quand)?;
            fs::remove_file(donnees.join(relatif)).with_context(|| format!("effacement de {relatif}"))?;
            bilan.fichiers += 1;
            bilan.octets += taille;
        } else {
            tracing::warn!(relatif, "archive : empreinte différente sur la VM, l'original reste sur le Mac");
            bilan.gardes += 1;
        }
    }
    let _ = fs::remove_dir_all(&envoi);
    tracing::info!(
        jour,
        fichiers = prets.len(),
        gardes = bilan.gardes,
        vignettes_ms = bilan.ms[0],
        chiffrement_ms = bilan.ms[1],
        envoi_ms = bilan.ms[2],
        verification_ms = bilan.ms[3],
        "journée archivée"
    );
    Ok(())
}

/// Le fichier, lisible sur le Mac : à sa place, sinon ramené de l'archive dans le cache.
pub fn lisible(chemin: &Path) -> Result<PathBuf> {
    if chemin.is_file() {
        return Ok(chemin.to_path_buf());
    }
    let donnees = chemins::donnees();
    let relatif = chemin
        .strip_prefix(&donnees)
        .ok()
        .and_then(|r| r.to_str())
        .ok_or_else(|| anyhow!("fichier absent : {}", chemin.display()))?
        .to_string();
    let cache = donnees.join("archive-cache").join(&relatif);
    if cache.is_file() {
        return Ok(cache);
    }
    ramener_dans(&relatif, &cache)?;
    nettoyer_cache(&donnees.join("archive-cache"));
    Ok(cache)
}

/// Ramène un fichier archivé (vérifié, déchiffré) à `cible`.
fn ramener_dans(relatif: &str, cible: &Path) -> Result<()> {
    let relatif = chemin_sur(relatif)?;
    let base = Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite")).context("base")?;
    let (_, attendue) = base.archive_de(relatif)?.ok_or_else(|| anyhow!("fichier absent et pas archivé : {relatif}"))?;
    if !disponible() {
        bail!("le robot Teleport de B-Screen n'est pas installé");
    }
    let sortie = tsh()
        .args(["ssh", &hote(), &format!("cat -- {DISTANT}/{relatif}.bsa")])
        .stderr(Stdio::piped())
        .output()
        .context("tsh")?;
    if !sortie.status.success() {
        bail!("archive injoignable : {}", String::from_utf8_lossy(&sortie.stderr).trim());
    }
    if empreinte(&sortie.stdout) != attendue {
        bail!("fichier de l'archive différent de celui envoyé : {relatif}");
    }
    let clair = dechiffrer(relatif, &sortie.stdout, &cle()?)?;
    fs::create_dir_all(cible.parent().unwrap_or(Path::new("/")))?;
    let temporaire = cible.with_extension("ramene");
    fs::File::create(&temporaire)?.write_all(&clair)?;
    fs::rename(&temporaire, cible)?;
    tracing::info!(relatif, ko = clair.len() / 1024, "ramené de l'archive");
    Ok(())
}

/// Garde le cache sous 2 Go : les fichiers les plus anciennement ramenés partent d'abord.
fn nettoyer_cache(dossier: &Path) {
    fn fichiers(d: &Path, sortie: &mut Vec<(SystemTime, u64, PathBuf)>) {
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let chemin = e.path();
            match e.metadata() {
                Ok(m) if m.is_dir() => fichiers(&chemin, sortie),
                Ok(m) => sortie.push((m.modified().unwrap_or(UNIX_EPOCH), m.len(), chemin)),
                Err(_) => {}
            }
        }
    }
    let mut liste = Vec::new();
    fichiers(dossier, &mut liste);
    let mut total: u64 = liste.iter().map(|f| f.1).sum();
    liste.sort_by_key(|f| f.0);
    for (_, taille, chemin) in liste {
        if total <= CACHE_MAX {
            break;
        }
        if fs::remove_file(&chemin).is_ok() {
            total -= taille;
        }
    }
}

/// Avant d'effacer des images d'une vidéo archivée : la ramène à sa place (elle n'est plus
/// archivée ; l'archivage suivant la renverra, sans ces images).
pub fn ramener_a_sa_place(chemin: &Path) -> Result<()> {
    if chemin.is_file() {
        return Ok(());
    }
    let donnees = chemins::donnees();
    let relatif = chemin.strip_prefix(&donnees).ok().and_then(|r| r.to_str()).context("hors des données")?.to_string();
    ramener_dans(&relatif, chemin)?;
    Base::ouvrir(&donnees.join("index.sqlite"))?.oublier_archive(&relatif)?;
    let _ = fs::remove_file(donnees.join("archive-cache").join(&relatif));
    Ok(())
}

/// Efface un fichier de l'archive (toutes ses images ont été effacées, ou un essai).
pub fn effacer_distant(relatif: &str) -> Result<()> {
    let relatif = chemin_sur(relatif)?;
    let sortie = tsh().args(["ssh", &hote(), &format!("rm -f -- {DISTANT}/{relatif}.bsa")]).output().context("tsh")?;
    if !sortie.status.success() {
        bail!("effacement sur l'archive impossible : {}", String::from_utf8_lossy(&sortie.stderr).trim());
    }
    Base::ouvrir(&chemins::donnees().join("index.sqlite"))?.oublier_archive(relatif)?;
    let _ = fs::remove_file(chemins::donnees().join("archive-cache").join(relatif));
    Ok(())
}

/// La vignette d'un moment d'une vidéo archivée (HEIC), à défaut de la vidéo.
pub fn vignette(chemin_video: &Path, position_ms: i64) -> Option<Vec<u8>> {
    let donnees = chemins::donnees();
    let relatif = chemin_video.strip_prefix(&donnees).ok()?.to_str()?;
    let base = Base::ouvrir_lecture(&donnees.join("index.sqlite")).ok()?;
    let fichier = base.vignette_proche(relatif, position_ms).ok()??;
    fs::read(donnees.join(fichier)).ok()
}

/// Chaque nuit (sur secteur, Mac inactif depuis 15 min, frise et Ask fermés, au plus une fois par
/// jour) : la journée qui a passé l'âge de l'archive part.
pub fn demarrer() {
    let _ = std::thread::Builder::new().name("bscreen-archive".into()).spawn(|| {
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
        }
        loop {
            std::thread::sleep(Duration::from_secs(30 * 60));
            let jours = bscreen_core::reglages::Reglages::charger().archive_jours;
            if jours == 0
                || !disponible()
                || !crate::sens::sur_secteur()
                || crate::moteur::secondes_d_inactivite() < 15.0 * 60.0
                || crate::moteur::interface_est_ouverte()
            {
                continue;
            }
            let derniere = Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))
                .ok()
                .and_then(|b| b.meta(CLE_DERNIERE).ok().flatten())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            if maintenant_ms() - derniere < 20 * 3_600_000 {
                continue;
            }
            match archiver(maintenant_ms() - i64::from(jours) * 86_400_000, usize::MAX, false) {
                Ok(b) if b.fichiers > 0 => tracing::info!(
                    jours = b.jours,
                    fichiers = b.fichiers,
                    mo = b.octets / 1_048_576,
                    vignettes = b.vignettes,
                    gardes = b.gardes,
                    "archive de la nuit"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(%e, "archive de la nuit interrompue"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chiffre_lie_a_son_chemin() {
        let cle = cle_depuis("ABCDE-12345-FGHJK-67890-MNPQR-STVWX-YZ012-34567").unwrap();
        assert_eq!(cle, cle_depuis("ABCDE-12345-FGHJK-67890-MNPQR-STVWX-YZ012-34567").unwrap());
        let clair = b"une video de cinq minutes".repeat(100);
        let chiffre = chiffrer("video/2026-11-19/ecran1-1000.mp4", &clair, &cle).unwrap();
        assert_eq!(&chiffre[..4], MAGIE);
        assert_eq!(dechiffrer("video/2026-11-19/ecran1-1000.mp4", &chiffre, &cle).unwrap(), clair);
        // Un autre chemin, une autre clé, un octet changé : refusé.
        assert!(dechiffrer("video/2026-11-19/ecran1-1005.mp4", &chiffre, &cle).is_err());
        assert!(dechiffrer("video/2026-11-19/ecran1-1000.mp4", &chiffre, &cle_depuis("autre").unwrap()).is_err());
        let mut abime = chiffre.clone();
        abime[40] ^= 1;
        assert!(dechiffrer("video/2026-11-19/ecran1-1000.mp4", &abime, &cle).is_err());
        // Deux chiffrements du même fichier diffèrent (nonce au hasard).
        assert_ne!(chiffre, chiffrer("video/2026-11-19/ecran1-1000.mp4", &clair, &cle).unwrap());
    }

    #[test]
    fn chemins_surs() {
        assert!(chemin_sur("video/2026-11-19/ecran1-1000.mp4").is_ok());
        assert!(chemin_sur("audio/2026-11-19/v-12.m4a").is_ok());
        assert!(chemin_sur("../etc/passwd").is_err());
        assert!(chemin_sur("video/x'; rm -rf /").is_err());
        assert!(chemin_sur("").is_err());
    }
}
