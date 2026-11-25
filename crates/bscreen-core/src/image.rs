//! Une image en mémoire, au format BGRA (celui que l'interface affiche).

#[derive(Clone)]
pub struct ImageBgra {
    pub largeur: u32,
    pub hauteur: u32,
    /// `largeur × hauteur × 4` octets, ligne par ligne, du haut vers le bas.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for ImageBgra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageBgra({} × {})", self.largeur, self.hauteur)
    }
}

impl ImageBgra {
    /// Un morceau de l'image : rectangle en fractions de la taille (0 à 1).
    pub fn rognee(&self, x: f32, y: f32, l: f32, h: f32) -> ImageBgra {
        let (lt, ht) = (self.largeur as f32, self.hauteur as f32);
        let x0 = ((x * lt).floor() as u32).min(self.largeur.saturating_sub(1));
        let y0 = ((y * ht).floor() as u32).min(self.hauteur.saturating_sub(1));
        let largeur = ((l * lt).ceil() as u32).clamp(1, self.largeur - x0);
        let hauteur = ((h * ht).ceil() as u32).clamp(1, self.hauteur - y0);
        let mut pixels = Vec::with_capacity((largeur * hauteur * 4) as usize);
        for ligne in y0..y0 + hauteur {
            let debut = ((ligne * self.largeur + x0) * 4) as usize;
            pixels.extend_from_slice(&self.pixels[debut..debut + (largeur * 4) as usize]);
        }
        ImageBgra {
            largeur,
            hauteur,
            pixels,
        }
    }

    /// Luminance moyenne (0 à 255) : pour choisir un texte clair ou foncé par-dessus.
    pub fn luminance(&self) -> f32 {
        let mut somme = 0f64;
        let mut n = 0f64;
        for p in self.pixels.chunks_exact(4) {
            somme += 0.0722 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.2126 * f64::from(p[2]);
            n += 1.0;
        }
        if n == 0.0 { 0.0 } else { (somme / n) as f32 }
    }

    /// Version floutée (flou de boîte répété : proche d'un flou gaussien), à la même taille.
    /// Sert de fond « dépoli » sous la barre de la frise, sans voile ni teinte.
    pub fn floutee(&self, rayon: usize, passes: usize) -> ImageBgra {
        let (l, h) = (self.largeur as usize, self.hauteur as usize);
        let mut a: Vec<f32> = self.pixels.iter().map(|&o| f32::from(o)).collect();
        let mut b = vec![0f32; a.len()];
        for _ in 0..passes {
            flou_boite(&a, &mut b, l, h, rayon, true);
            flou_boite(&b, &mut a, l, h, rayon, false);
        }
        ImageBgra {
            largeur: self.largeur,
            hauteur: self.hauteur,
            pixels: a.iter().map(|&v| v.round() as u8).collect(),
        }
    }
}

/// Moyenne glissante sur une ligne (`horizontal`) ou une colonne, bords répétés.
fn flou_boite(source: &[f32], cible: &mut [f32], l: usize, h: usize, rayon: usize, horizontal: bool) {
    let (n, lignes) = if horizontal { (l, h) } else { (h, l) };
    let fenetre = (2 * rayon + 1) as f32;
    let indice = |ligne: usize, i: usize| {
        if horizontal {
            (ligne * l + i) * 4
        } else {
            (i * l + ligne) * 4
        }
    };
    for ligne in 0..lignes {
        for canal in 0..4 {
            let mut somme = 0f32;
            for k in 0..=2 * rayon {
                let i = k.saturating_sub(rayon).min(n - 1);
                somme += source[indice(ligne, i) + canal];
            }
            for i in 0..n {
                cible[indice(ligne, i) + canal] = somme / fenetre;
                let sortant = i.saturating_sub(rayon);
                let entrant = (i + rayon + 1).min(n - 1);
                somme += source[indice(ligne, entrant) + canal] - source[indice(ligne, sortant) + canal];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoupe_et_luminance() {
        // 4 × 2 : moitié gauche noire, moitié droite blanche.
        let mut pixels = vec![0u8; 4 * 2 * 4];
        for y in 0..2 {
            for x in 2..4 {
                let i = (y * 4 + x) * 4;
                pixels[i..i + 4].copy_from_slice(&[255; 4]);
            }
        }
        let image = ImageBgra {
            largeur: 4,
            hauteur: 2,
            pixels,
        };
        let droite = image.rognee(0.5, 0.0, 0.5, 1.0);
        assert_eq!((droite.largeur, droite.hauteur), (2, 2));
        assert!(droite.luminance() > 250.0);
        assert!(image.rognee(0.0, 0.0, 0.5, 1.0).luminance() < 1.0);
    }

    #[test]
    fn le_flou_garde_une_image_unie_et_adoucit_un_contraste() {
        let unie = ImageBgra {
            largeur: 8,
            hauteur: 4,
            pixels: vec![100; 8 * 4 * 4],
        };
        assert!(unie.floutee(2, 3).pixels.iter().all(|&o| o == 100));

        // Moitié noire, moitié blanche : après flou, le milieu est gris.
        let mut pixels = vec![0u8; 16 * 4];
        for x in 8..16 {
            pixels[x * 4..x * 4 + 4].copy_from_slice(&[255; 4]);
        }
        let contraste = ImageBgra {
            largeur: 16,
            hauteur: 1,
            pixels,
        };
        let flou = contraste.floutee(3, 2);
        let milieu = flou.pixels[8 * 4];
        assert!(milieu > 60 && milieu < 200, "milieu = {milieu}");
    }
}
