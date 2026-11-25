//! Rattrapage du texte : les images enregistrées avant que B-Screen sache lire le texte (la
//! première heure, le 19/11) sont relues par l'OCR, puis leur texte est noté comme le fait la
//! capture (secrets masqués, apparitions prolongées d'une image à la suivante). Les images de
//! navigation privée ne sont jamais lues.

use std::collections::HashMap;

use anyhow::{Context, Result};
use bscreen_core::chemins;
use bscreen_core::masquage::masquer;
use bscreen_store::{Base, NouvelleApparition};
use objc2_core_foundation::CGRect;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayBounds(ecran: u32) -> CGRect;
}

/// Écart au-delà duquel deux images ne se suivent plus (le texte n'est pas prolongé).
const ECART_MAX_MS: i64 = 60_000;

/// Relit les images sans texte ; `progression(faites, total)` est appelée après chaque image.
/// Rend (images lues, textes notés).
pub fn rattraper(mut progression: impl FnMut(usize, usize)) -> Result<(usize, usize)> {
    let base = Base::ouvrir(&chemins::donnees().join("index.sqlite")).context("base")?;
    let images: Vec<_> = base
        .images_entre(0, i64::MAX)?
        .into_iter()
        .filter(|i| !i.privee)
        .filter(|i| !base.a_du_texte(i.ecran, i.quand_ms).unwrap_or(true))
        .collect();
    let total = images.len();
    // Par écran : les apparitions de l'image précédente (texte → id) et son heure.
    let mut precedentes: HashMap<u32, (i64, HashMap<i64, i64>)> = HashMap::new();
    let mut notes = 0;
    for (n, image) in images.iter().enumerate() {
        let lignes = objc2::rc::autoreleasepool(|_| {
            crate::lecture::lire_image_enregistree(&chemins::donnees().join(&image.fichier), image.position_ms)
        })
        .unwrap_or_default();
        let bornes = unsafe { CGDisplayBounds(image.ecran) };
        let ecran = [bornes.origin.x, bornes.origin.y, bornes.size.width, bornes.size.height];
        let (heure_prec, actives) = precedentes.remove(&image.ecran).unwrap_or((i64::MIN, HashMap::new()));
        let suit = image.quand_ms - heure_prec <= ECART_MAX_MS;
        let mut encore: HashMap<i64, i64> = HashMap::new();
        let mut nouvelles: Vec<NouvelleApparition> = Vec::new();
        let mut prolongees: Vec<i64> = Vec::new();
        for ligne in &lignes {
            let texte = masquer(ligne.texte.trim());
            if texte.chars().count() < 2 {
                continue;
            }
            let texte_id = base.texte_id(&texte)?;
            if encore.contains_key(&texte_id) || nouvelles.iter().any(|a| a.texte == texte_id) {
                continue;
            }
            match actives.get(&texte_id).filter(|_| suit) {
                Some(&id) => {
                    prolongees.push(id);
                    encore.insert(texte_id, id);
                }
                None => {
                    let c = ligne.cadre;
                    nouvelles.push(NouvelleApparition {
                        texte: texte_id,
                        ecran: image.ecran,
                        quand_ms: image.quand_ms,
                        source: 1,
                        vu: true,
                        focus: false,
                        cadre: [ecran[0] + c[0] * ecran[2], ecran[1] + c[1] * ecran[3], c[2] * ecran[2], c[3] * ecran[3]],
                    });
                }
            }
        }
        let ids = base.noter_apparitions(&nouvelles, &prolongees, image.quand_ms)?;
        notes += ids.len();
        for (a, id) in nouvelles.iter().zip(ids) {
            encore.insert(a.texte, id);
        }
        precedentes.insert(image.ecran, (image.quand_ms, encore));
        progression(n + 1, total);
    }
    Ok((total, notes))
}
