//! La table des mots d'un modèle de texte (EmbeddingGemma pour le sens, SigLIP 2 pour les
//! images), lue à part depuis le 24/11/2026.
//!
//! Ces deux modèles commencent par une table de plus de 256 000 mots × 768 nombres : les deux tiers
//! de leur poids. Dans Core ML, elle était chargée en entier à chaque fois (≈ 600 Mo pour chaque
//! modèle chargé), et macOS en recopiait autant dans son cache de modèles compilés à chaque
//! recompilation. Elle est maintenant dans un fichier `.mots` à côté du modèle (octets signés, une
//! échelle par ligne ; écrit par `outils/coreml/table_de_mots.py`), projeté en mémoire : seules les
//! pages des mots d'une phrase sont lues sur le disque, et macOS peut les reprendre à tout moment
//! (elles ne comptent pas dans la mémoire de B-Screen). Le modèle reçoit les lignes, plus les jetons.
//!
//! Format (petit-boutiste) : « BSMOTS01 », lignes (u32), dimension (u32), échelles (f32 × lignes),
//! valeurs (i8 × lignes × dimension).

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Result, bail};

const MAGIE: &[u8; 8] = b"BSMOTS01";
const ENTETE: usize = 16;

/// Une table de mots projetée en mémoire (lecture seule).
pub struct TableDeMots {
    adresse: *const u8,
    taille: usize,
    lignes: usize,
    dimension: usize,
}

// SAFETY : la projection est en lecture seule et ne bouge pas tant que la table vit.
unsafe impl Send for TableDeMots {}
unsafe impl Sync for TableDeMots {}

impl TableDeMots {
    pub fn ouvrir(chemin: &Path) -> Result<Self> {
        let fichier = File::open(chemin)?;
        let taille = usize::try_from(fichier.metadata()?.len())?;
        if taille < ENTETE {
            bail!("table de mots {} trop courte", chemin.display());
        }
        // SAFETY : projection en lecture seule d'un fichier ouvert, de sa taille exacte.
        let adresse = unsafe {
            libc::mmap(std::ptr::null_mut(), taille, libc::PROT_READ, libc::MAP_PRIVATE, fichier.as_raw_fd(), 0)
        };
        if adresse == libc::MAP_FAILED {
            bail!("table de mots {} : {}", chemin.display(), std::io::Error::last_os_error());
        }
        // Les mots d'une phrase sont n'importe où dans la table : pas de lecture d'avance.
        // SAFETY : conseil sur la projection qu'on vient de créer.
        unsafe { libc::madvise(adresse, taille, libc::MADV_RANDOM) };
        // La table rend la projection en disparaissant (aussi en cas d'erreur ci-dessous) : c'est
        // elle qu'on rend, complétée (pas une copie de ses champs, la projection partirait avec elle).
        let mut table = Self { adresse: adresse.cast(), taille, lignes: 0, dimension: 0 };
        let entete: [u8; ENTETE] = table.octets()[..ENTETE].try_into()?;
        if &entete[..8] != MAGIE {
            bail!("{} n'est pas une table de mots", chemin.display());
        }
        let lignes = u32::from_le_bytes([entete[8], entete[9], entete[10], entete[11]]) as usize;
        let dimension = u32::from_le_bytes([entete[12], entete[13], entete[14], entete[15]]) as usize;
        if taille != ENTETE + lignes * 4 + lignes * dimension {
            bail!("table de mots {} : taille inattendue ({taille} octets pour {lignes} × {dimension})", chemin.display());
        }
        table.lignes = lignes;
        table.dimension = dimension;
        Ok(table)
    }

    fn octets(&self) -> &[u8] {
        // SAFETY : la projection fait `taille` octets et vit autant que `self`.
        unsafe { std::slice::from_raw_parts(self.adresse, self.taille) }
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn lignes(&self) -> usize {
        self.lignes
    }

    /// Écrit la ligne du mot `id` dans `sortie` (`dimension` nombres).
    pub fn lire(&self, id: u32, sortie: &mut [f32]) -> Result<()> {
        let id = id as usize;
        if id >= self.lignes {
            bail!("mot {id} hors de la table ({} lignes)", self.lignes);
        }
        if sortie.len() != self.dimension {
            bail!("ligne de {} nombres attendue, {} donnés", self.dimension, sortie.len());
        }
        let octets = self.octets();
        let e = ENTETE + id * 4;
        let echelle = f32::from_le_bytes([octets[e], octets[e + 1], octets[e + 2], octets[e + 3]]);
        let debut = ENTETE + self.lignes * 4 + id * self.dimension;
        for (case, &o) in sortie.iter_mut().zip(&octets[debut..debut + self.dimension]) {
            *case = f32::from(o as i8) * echelle;
        }
        Ok(())
    }

    /// Les lignes d'une suite de jetons, complétée par le jeton `remplissage` jusqu'à `longueur` :
    /// `longueur` × `dimension` nombres, ligne après ligne (ce qu'attend le modèle).
    pub fn lignes_de(&self, ids: &[u32], longueur: usize, remplissage: u32) -> Result<Vec<f32>> {
        let mut valeurs = vec![0f32; longueur * self.dimension];
        for (position, ligne) in valeurs.chunks_exact_mut(self.dimension).enumerate() {
            self.lire(ids.get(position).copied().unwrap_or(remplissage), ligne)?;
        }
        Ok(valeurs)
    }
}

impl Drop for TableDeMots {
    fn drop(&mut self) {
        // SAFETY : la projection créée par `ouvrir`, rendue une seule fois.
        unsafe { libc::munmap(self.adresse.cast_mut().cast(), self.taille) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_table_se_relit() {
        let dossier = std::env::temp_dir().join(format!("bscreen-mots-{}", std::process::id()));
        std::fs::create_dir_all(&dossier).unwrap();
        let chemin = dossier.join("essai.mots");
        // 3 mots × 4 nombres.
        let mut f = Vec::new();
        f.extend_from_slice(MAGIE);
        f.extend_from_slice(&3u32.to_le_bytes());
        f.extend_from_slice(&4u32.to_le_bytes());
        for e in [0.5f32, 1.0, 0.25] {
            f.extend_from_slice(&e.to_le_bytes());
        }
        for o in [1i8, -2, 3, -4, 10, 20, 30, 40, -127, 127, 0, 1] {
            f.push(o as u8);
        }
        std::fs::write(&chemin, &f).unwrap();
        let table = TableDeMots::ouvrir(&chemin).unwrap();
        assert_eq!((table.lignes(), table.dimension()), (3, 4));
        let mut ligne = [0f32; 4];
        table.lire(0, &mut ligne).unwrap();
        assert_eq!(ligne, [0.5, -1.0, 1.5, -2.0]);
        table.lire(2, &mut ligne).unwrap();
        assert_eq!(ligne, [-31.75, 31.75, 0.0, 0.25]);
        assert!(table.lire(3, &mut ligne).is_err());
        let suite = table.lignes_de(&[1], 3, 0).unwrap();
        assert_eq!(&suite[..4], &[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(&suite[4..8], &[0.5, -1.0, 1.5, -2.0]);
        // Un fichier coupé est refusé.
        std::fs::write(&chemin, &f[..f.len() - 1]).unwrap();
        assert!(TableDeMots::ouvrir(&chemin).is_err());
        let _ = std::fs::remove_dir_all(&dossier);
    }
}
