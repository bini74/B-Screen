//! Suppression définitive d'images : retirées de la base, puis effacées des vidéos.
//!
//! On ne peut pas retirer une image d'une vidéo HEVC sans la réencoder (les images
//! dépendent les unes des autres) : le morceau concerné est relu image par image et
//! réécrit sans les images supprimées, puis remplace l'ancien fichier.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use bscreen_core::chemins;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{AVAssetReader, AVAssetReaderTrackOutput, AVMediaTypeVideo, AVURLAsset};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSNumber, NSString, NSURL};

use crate::encodage::MorceauEnCours;
use crate::moteur::avec_base;

/// Une seule réécriture de vidéos à la fois (effacement, moments privés rangés à part).
pub static REECRITURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Compte les effacements : un résumé commencé avant un effacement n'est pas gardé.
pub static EFFACEMENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Supprime définitivement des images. Rend le nombre d'images supprimées.
/// Peut prendre quelques secondes (réencodage) : à appeler hors du fil principal.
pub fn effacer(ids: &[i64]) -> Result<usize> {
    let _seul = REECRITURE.lock().unwrap_or_else(|e| e.into_inner());
    EFFACEMENTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let emplacements = avec_base(|b| b.emplacements(ids)).ok_or_else(|| anyhow!("base inaccessible"))?;
    let heures = avec_base(|b| b.heures_images(ids)).unwrap_or_default();
    let prives = avec_base(|b| b.fichiers_prives(ids)).unwrap_or_default();
    // Les vignettes de ces images (vidéos parties sur l'archive) partent avec elles.
    let vignettes = avec_base(|b| b.vignettes_des_images(ids)).unwrap_or_default();
    let supprimees = avec_base(|b| b.supprimer_images(ids)).unwrap_or(0);
    for v in &vignettes {
        let _ = fs::remove_file(chemins::donnees().join(v));
    }
    // Les moments privés rangés à part partent avec leur fichier chiffré (une suite partagée avec
    // d'autres images est recodée sans eux).
    crate::prive::apres_effacement(&prives);
    // Le texte affiché à ces moments disparaît aussi.
    let mut par_ecran: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    for (ecran, quand) in heures {
        par_ecran.entry(ecran).or_default().push(quand);
    }
    for (ecran, quand) in par_ecran {
        // Les résumés qui couvraient ces moments partent aussi (refaits sans eux la nuit suivante).
        avec_base(|b| b.oublier_episodes_a(&quand));
        avec_base(|b| b.oublier_textes_a(ecran, &quand));
    }
    crate::indexation::textes_oublies();

    // Les positions à retirer, morceau par morceau.
    let mut par_morceau: BTreeMap<i64, (String, HashSet<i64>)> = BTreeMap::new();
    for (_, morceau, fichier, position) in emplacements {
        par_morceau
            .entry(morceau)
            .or_insert_with(|| (fichier, HashSet::new()))
            .1
            .insert(position);
    }
    for (morceau, (fichier, positions)) in par_morceau {
        let chemin = chemins::donnees().join(&fichier);
        let archive = avec_base(|b| b.archive_de(&fichier)).flatten().is_some();
        if avec_base(|b| b.nb_images_morceau(morceau)).unwrap_or(1) == 0 {
            let _ = fs::remove_file(&chemin);
            if archive && let Err(e) = crate::archive::effacer_distant(&fichier) {
                tracing::error!(%e, fichier, "morceau vidé : effacement sur l'archive impossible");
            }
            avec_base(|b| b.supprimer_morceau(morceau));
            tracing::info!(fichier, "morceau vidé : fichier supprimé");
            continue;
        }
        // Partie sur l'archive : ramenée à sa place pour être recodée (elle repartira sans ces images).
        if archive && let Err(e) = crate::archive::ramener_a_sa_place(&chemin) {
            tracing::error!(%e, fichier, "effacement dans une vidéo archivée impossible (archive injoignable)");
            continue;
        }
        match reencoder_sans(&chemin, &positions) {
            Ok(octets) => {
                avec_base(|b| b.changer_taille_morceau(morceau, octets));
                tracing::info!(
                    fichier,
                    retirees = positions.len(),
                    ko = octets / 1024,
                    "images effacées de la vidéo"
                );
            }
            Err(e) => tracing::error!(%e, fichier, "effacement dans la vidéo impossible"),
        }
    }
    // Les vecteurs de sens des textes effacés quittent aussi la mémoire.
    crate::sens::oublier_index();
    crate::images::oublier_index();
    Ok(supprimees)
}

/// Efface tout ce qui a été enregistré pendant une période (menu « Effacer… ») : images (retirées
/// des vidéos), texte lu à l'écran ou dit, passages de voix et leurs fichiers, recherches tapées.
/// Rend (images, passages de voix) effacés. Peut prendre quelques secondes : hors du fil principal.
pub fn effacer_periode(debut_ms: i64, fin_ms: i64) -> Result<(usize, usize)> {
    EFFACEMENTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    crate::moteur::suspendre_pour_effacement(true);
    crate::audio::oublier_en_cours();
    let resultat = (|| {
        let ids: Vec<i64> = avec_base(|b| b.images_entre(debut_ms, fin_ms))
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.id)
            .collect();
        let images = if ids.is_empty() { 0 } else { effacer(&ids)? };
        let fichiers = avec_base(|b| b.oublier_periode(debut_ms, fin_ms)).ok_or_else(|| anyhow!("base inaccessible"))?;
        crate::indexation::textes_oublies();
        for fichier in &fichiers {
            let _ = fs::remove_file(chemins::donnees().join(fichier));
        }
        // Les pages libérées de la base sont déjà mises à zéro (secure_delete) ; le journal aussi.
        avec_base(|b| b.vider_journal());
        Ok((images, fichiers.len()))
    })();
    crate::moteur::suspendre_pour_effacement(false);
    crate::sens::oublier_index();
    crate::images::oublier_index();
    resultat
}

/// Réécrit la vidéo `chemin` sans les images placées à `positions` (en ms).
pub fn reencoder_sans(chemin: &Path, positions: &HashSet<i64>) -> Result<u64> {
    match reecrire_en_separant(chemin, positions, &mut |_, _| Ok(()))? {
        Some(temporaire) => {
            fs::rename(&temporaire, chemin)?;
            Ok(fs::metadata(chemin).map(|m| m.len()).unwrap_or(0))
        }
        None => bail!("plus aucune image à garder"),
    }
}

/// Relit la vidéo `chemin` image par image et en écrit une copie à côté, sans les images placées
/// à `positions` (en ms) : celles-là passent à `a_part` (tampon, position), dans l'ordre. Rend la
/// copie, à mettre à la place de la vidéo ; rien si plus aucune image n'y reste.
pub fn reecrire_en_separant(
    chemin: &Path,
    positions: &HashSet<i64>,
    a_part: &mut dyn FnMut(&CVPixelBuffer, i64) -> Result<()>,
) -> Result<Option<PathBuf>> {
    let texte = chemin.to_str().ok_or_else(|| anyhow!("chemin non UTF-8"))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(texte));
    let video = unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) };
    let type_video = unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("type vidéo absent"))?;
    #[allow(deprecated)]
    let pistes = unsafe { video.tracksWithMediaType(type_video) };
    let piste = pistes
        .firstObject()
        .ok_or_else(|| anyhow!("aucune piste vidéo dans {texte}"))?;

    let lecteur = unsafe { AVAssetReader::assetReaderWithAsset_error(&video) }
        .map_err(|e| anyhow!("lecture de {texte} : {}", e.localizedDescription()))?;
    let reglages = NSMutableDictionary::<NSString, AnyObject>::new();
    unsafe {
        let cle: &NSString = &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString);
        reglages.insert(
            cle,
            &*NSNumber::numberWithUnsignedInt(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
        );
    }
    let reglages: Retained<NSDictionary<NSString, AnyObject>> = Retained::into_super(reglages);
    let sortie =
        unsafe { AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&piste, Some(&reglages)) };
    unsafe { sortie.setAlwaysCopiesSampleData(false) };
    if !unsafe { lecteur.canAddOutput(&sortie) } {
        bail!("lecture image par image impossible");
    }
    unsafe { lecteur.addOutput(&sortie) };
    if !unsafe { lecteur.startReading() } {
        bail!("lecture de {texte} impossible");
    }

    let temporaire = chemin.with_extension("reencodage.mp4");
    let _ = fs::remove_file(&temporaire);
    let mut nouveau: Option<MorceauEnCours> = None;
    let mut gardees = 0usize;
    while let Some(echantillon) = unsafe { sortie.copyNextSampleBuffer() } {
        let quand = unsafe { echantillon.presentation_time_stamp() };
        if quand.timescale <= 0 {
            continue;
        }
        let position = (quand.value as f64 * 1000.0 / f64::from(quand.timescale)).round() as i64;
        let Some(tampon) = (unsafe { echantillon.image_buffer() }) else {
            continue;
        };
        if positions.iter().any(|p| (p - position).abs() <= 2) {
            if let Err(e) = a_part(&tampon, position) {
                drop(nouveau);
                let _ = fs::remove_file(&temporaire);
                return Err(e);
            }
            continue;
        }
        if nouveau.is_none() {
            let (largeur, hauteur) = (CVPixelBufferGetWidth(&tampon), CVPixelBufferGetHeight(&tampon));
            nouveau = Some(MorceauEnCours::ouvrir(
                temporaire.clone(),
                0,
                UNIX_EPOCH,
                largeur,
                hauteur,
                false,
            )?);
        }
        if let Some(morceau) = nouveau.as_mut() {
            morceau.ajouter_tampon(&tampon, position, true)?;
            gardees += 1;
        }
    }
    let Some(morceau) = nouveau else {
        return Ok(None);
    };
    let fin = morceau.fermer(|_| {});
    fin.recv_timeout(Duration::from_secs(60))
        .map_err(|_| anyhow!("réencodage trop long"))?;
    tracing::debug!(gardees, "réencodage terminé");
    Ok(Some(temporaire))
}
