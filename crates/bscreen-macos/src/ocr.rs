//! OCR d'Apple (Vision) : lire le texte d'une image et repérer où un mot apparaît.
//!
//! Sert à surligner exactement, sur l'image affichée, le mot cherché (au lieu du
//! cadre d'un élément d'accessibilité, parfois grand comme la fenêtre), et plus
//! tard d'OCR de secours pour les apps sans accessibilité.

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, msg_send};
use objc2_core_foundation::CGRect;
use crate::lecture::ImageGris;
use objc2_core_graphics::CGImage;
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSArray, NSDictionary, NSError, NSRange, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedText, VNRecognizedTextObservation,
    VNRectangleObservation, VNRequest, VNRequestTextRecognitionLevel,
};

/// Minuscules et sans accents, caractère par caractère (la position de chaque caractère est gardée).
pub fn plier(texte: &str) -> Vec<char> {
    texte
        .chars()
        .map(|c| {
            let c = c.to_lowercase().next().unwrap_or(c);
            match c {
                'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'î' | 'ï' | 'í' | 'ì' => 'i',
                'ô' | 'ö' | 'ó' | 'ò' | 'õ' | 'œ' => 'o',
                'ù' | 'û' | 'ü' | 'ú' => 'u',
                'ç' => 'c',
                'ñ' => 'n',
                'ÿ' => 'y',
                autre => autre,
            }
        })
        .collect()
}

/// Les endroits (en caractères) où un mot apparaît dans un texte plié.
fn occurrences(texte: &[char], mot: &[char]) -> Vec<usize> {
    if mot.is_empty() || mot.len() > texte.len() {
        return Vec::new();
    }
    (0..=texte.len() - mot.len())
        .filter(|&i| texte[i..i + mot.len()] == *mot)
        .collect()
}

fn reconnaitre(image: &CGImage, rapide: bool) -> Result<Vec<Retained<VNRecognizedTextObservation>>> {
    let options = NSDictionary::<objc2_vision::VNImageOption, AnyObject>::new();
    let gestionnaire =
        unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), image, &options) };
    executer(&gestionnaire, rapide)
}

/// Le texte d'une image capturée (NV12), ligne par ligne, avec le cadre normalisé de chaque ligne
/// (0 à 1, origine en haut à gauche). OCR rapide : sert de secours quand l'accessibilité ne donne rien.
///
/// Appelée sur le fil de l'indexation, qui dure autant que l'app : sans son bassin de libération,
/// les résultats de Vision s'y accumuleraient sans fin.
pub fn lire_tampon(pixels: &CVPixelBuffer) -> Result<Vec<(String, [f64; 4])>> {
    objc2::rc::autoreleasepool(|_| lire_tampon_seul(pixels))
}

fn lire_tampon_seul(pixels: &CVPixelBuffer) -> Result<Vec<(String, [f64; 4])>> {
    let options = NSDictionary::<objc2_vision::VNImageOption, AnyObject>::new();
    let gestionnaire = unsafe {
        VNImageRequestHandler::initWithCVPixelBuffer_options(VNImageRequestHandler::alloc(), pixels, &options)
    };
    let mut lignes = Vec::new();
    for observation in executer(&gestionnaire, true)? {
        if let Some(texte) = observation.topCandidates(1).firstObject() {
            let texte = texte.string().to_string();
            if texte.trim().chars().count() >= 2 {
                lignes.push((texte, en_haut_a_gauche(unsafe { observation.boundingBox() })));
            }
        }
    }
    Ok(lignes)
}

fn executer(gestionnaire: &VNImageRequestHandler, rapide: bool) -> Result<Vec<Retained<VNRecognizedTextObservation>>> {
    let requete = VNRecognizeTextRequest::new();
    requete.setRecognitionLevel(if rapide {
        VNRequestTextRecognitionLevel::Fast
    } else {
        VNRequestTextRecognitionLevel::Accurate
    });
    requete.setUsesLanguageCorrection(!rapide);
    let langues = NSArray::from_retained_slice(&[NSString::from_str("fr-FR"), NSString::from_str("en-US")]);
    requete.setRecognitionLanguages(&langues);
    // VNRecognizeTextRequest → VNImageBasedRequest → VNRequest.
    let requete_generique: Retained<VNRequest> = Retained::into_super(Retained::into_super(requete.clone()));
    let requetes: Retained<NSArray<VNRequest>> = NSArray::from_retained_slice(&[requete_generique]);
    gestionnaire
        .performRequests_error(&requetes)
        .map_err(|e| anyhow!("OCR : {}", e.localizedDescription()))?;
    Ok(requete.results().map(|r| r.to_vec()).unwrap_or_default())
}

/// Cadre Vision (normalisé, origine en bas à gauche) → cadre normalisé, origine en haut à gauche.
fn en_haut_a_gauche(r: CGRect) -> [f64; 4] {
    [
        r.origin.x,
        1.0 - r.origin.y - r.size.height,
        r.size.width,
        r.size.height,
    ]
}

/// Cadre d'un passage d'un texte reconnu (au mot près), si Vision sait le donner.
fn cadre_du_passage(texte: &VNRecognizedText, debut_utf16: usize, longueur_utf16: usize) -> Option<CGRect> {
    let passage = NSRange {
        location: debut_utf16,
        length: longueur_utf16,
    };
    let resultat: Result<Retained<VNRectangleObservation>, Retained<NSError>> =
        unsafe { msg_send![texte, boundingBoxForRange: passage, error: _] };
    resultat.ok().map(|o| unsafe { o.boundingBox() })
}

/// Où les mots cherchés sont visibles sur l'image. Cadres normalisés (0 à 1, origine en haut à gauche).
pub fn reperer(image: &CGImage, mots: &[String]) -> Result<Vec<[f64; 4]>> {
    let mots: Vec<Vec<char>> = mots.iter().map(|m| plier(m)).filter(|m| m.len() >= 2).collect();
    if mots.is_empty() {
        return Ok(Vec::new());
    }
    // D'abord l'OCR rapide ; le précis seulement s'il ne trouve rien (petit texte, police fine…).
    let cadres = reperer_avec(image, &mots, true)?;
    if !cadres.is_empty() {
        return Ok(cadres);
    }
    reperer_avec(image, &mots, false)
}

fn reperer_avec(image: &CGImage, mots: &[Vec<char>], rapide: bool) -> Result<Vec<[f64; 4]>> {
    let mut cadres = Vec::new();
    for ligne in reconnaitre(image, rapide)? {
        let candidats = ligne.topCandidates(1);
        let Some(texte) = candidats.firstObject() else { continue };
        let original = texte.string().to_string();
        let plie = plier(&original);
        // Index UTF-16 de chaque caractère (Vision compte en UTF-16).
        let mut utf16 = Vec::with_capacity(plie.len() + 1);
        let mut n = 0usize;
        for c in original.chars() {
            utf16.push(n);
            n += c.len_utf16();
        }
        utf16.push(n);
        for mot in mots {
            for debut in occurrences(&plie, mot) {
                let (a, b) = (utf16[debut], utf16[(debut + mot.len()).min(utf16.len() - 1)]);
                let cadre = cadre_du_passage(&texte, a, b - a).unwrap_or_else(|| unsafe { ligne.boundingBox() });
                cadres.push(en_haut_a_gauche(cadre));
            }
        }
    }
    Ok(cadres)
}

/// Niveau de gris d'un pixel.
fn gris(image: &ImageGris, x: usize, y: usize) -> f32 {
    f32::from(image.pixels[y * image.largeur + x])
}

/// L'encre de chaque colonne d'une zone (normalisée) de l'image : écart moyen au fond, le fond étant
/// la médiane des lignes du haut et du bas de la zone. Rend (première colonne, encre par colonne).
fn profil_encre(image: &ImageGris, zone: [f64; 4]) -> Option<(usize, Vec<f32>)> {
    let (l, h) = (image.largeur, image.hauteur);
    let x0 = ((zone[0] * l as f64).floor().max(0.0) as usize).min(l);
    let x1 = (((zone[0] + zone[2]) * l as f64).ceil().max(0.0) as usize).min(l);
    let y0 = ((zone[1] * h as f64).floor().max(0.0) as usize).min(h);
    let y1 = (((zone[1] + zone[3]) * h as f64).ceil().max(0.0) as usize).min(h);
    if x1 <= x0 + 1 || y1 <= y0 + 2 {
        return None;
    }
    let mut bord: Vec<f32> = (x0..x1).flat_map(|x| [gris(image, x, y0), gris(image, x, y1 - 1)]).collect();
    bord.sort_by(f32::total_cmp);
    let fond = bord[bord.len() / 2];
    let hauteur = (y1 - y0) as f32;
    let encre = (x0..x1)
        .map(|x| (y0..y1).map(|y| (gris(image, x, y) - fond).abs()).sum::<f32>() / hauteur)
        .collect();
    Some((x0, encre))
}

/// Resserre les mots d'une ligne sur leur encre (Vision déborde ou rogne de quelques pixels).
/// Entre deux mots voisins, la plus large bande de colonnes vides est l'espace qui les sépare :
/// le premier finit à son bord gauche, le suivant commence à son bord droit. Le premier et le
/// dernier mot sont resserrés sur leur première / dernière colonne encrée.
fn resserrer_ligne(image: &ImageGris, ligne: [f64; 4], mots: &mut [bscreen_core::texte_lu::MotLu]) {
    if mots.is_empty() {
        return;
    }
    let l = image.largeur as f64;
    let marge = 12.0 / l;
    let Some((debut, encre)) = profil_encre(image, [ligne[0] - marge, ligne[1], ligne[2] + 2.0 * marge, ligne[3]])
    else {
        return;
    };
    let maximum = encre.iter().copied().fold(0f32, f32::max);
    if maximum < 8.0 {
        return;
    }
    let (seuil, blanc) = (maximum * 0.12, maximum * 0.03);
    let fin = debut + encre.len();
    let px = |x: f64| ((x * l).round().max(debut as f64) as usize).min(fin.saturating_sub(1));
    let a_l_encre = |c: usize| encre[c - debut] > seuil;
    // Bords bruts (pixels) de chaque mot.
    let mut x0: Vec<usize> = mots.iter().map(|m| px(m.cadre[0])).collect();
    let mut x1: Vec<usize> = mots.iter().map(|m| px(m.cadre[0] + m.cadre[2])).collect();
    // Le premier mot : sa première colonne encrée, à 4 px près autour de son bord.
    let premier_tiers = x0[0] + (x1[0].saturating_sub(x0[0])) / 3;
    if let Some(c) = (x0[0].saturating_sub(4).max(debut)..=premier_tiers.min(fin - 1)).find(|&c| a_l_encre(c)) {
        x0[0] = c;
    }
    // Le dernier mot : sa dernière colonne encrée.
    let n = mots.len();
    let dernier_tiers = x1[n - 1].saturating_sub((x1[n - 1].saturating_sub(x0[n - 1])) / 3);
    if let Some(c) = (dernier_tiers.max(debut)..=(x1[n - 1] + 4).min(fin - 1)).rev().find(|&c| a_l_encre(c)) {
        x1[n - 1] = c + 1;
    }
    // Entre deux mots : l'espace.
    for k in 0..n - 1 {
        let (a, b) = (x1[k].min(x0[k + 1]), x1[k].max(x0[k + 1]));
        let gauche = (x0[k] + (x1[k].saturating_sub(x0[k])) * 2 / 3).max(a.saturating_sub(8)).max(debut);
        let droite = (x0[k + 1] + (x1[k + 1].saturating_sub(x0[k + 1])) / 3).min(b + 8).min(fin - 1);
        if gauche >= droite {
            continue;
        }
        // Les bandes de colonnes vides, et la plus large (à largeur égale, la plus proche du milieu).
        let milieu = (a + b) as f64 / 2.0;
        let mut meilleure: Option<(usize, usize)> = None;
        let mut c = gauche;
        while c <= droite {
            if encre[c - debut] < blanc {
                let depart = c;
                while c <= droite && encre[c - debut] < blanc {
                    c += 1;
                }
                let bande = (depart, c);
                let mieux = match meilleure {
                    None => true,
                    Some((d, f)) => {
                        let (l1, l2) = (bande.1 - bande.0, f - d);
                        l1 > l2
                            || (l1 == l2
                                && ((bande.0 + bande.1) as f64 / 2.0 - milieu).abs()
                                    < ((d + f) as f64 / 2.0 - milieu).abs())
                    }
                };
                if mieux {
                    meilleure = Some(bande);
                }
            } else {
                c += 1;
            }
        }
        if let Some((d, f)) = meilleure.filter(|(d, f)| f - d >= 2) {
            x1[k] = d;
            x0[k + 1] = f;
        }
    }
    for (k, mot) in mots.iter_mut().enumerate() {
        if x1[k] > x0[k] {
            mot.cadre[0] = x0[k] as f64 / l;
            mot.cadre[2] = (x1[k] - x0[k]) as f64 / l;
        }
    }
}

/// Largeur relative (avance, en fraction de la taille) de chaque caractère dans la police du système,
/// mesurée par CoreText une fois par caractère. À défaut, la table générique.
fn largeurs_systeme(mot: &str) -> Vec<f64> {
    use objc2_core_text::{CTFont, CTFontOrientation, CTFontUIFontType};
    thread_local! {
        static POLICE: Option<objc2_core_foundation::CFRetained<CTFont>> =
            unsafe { CTFont::new_ui_font_for_language(CTFontUIFontType::System, 100.0, None) };
    }
    static CACHE: std::sync::Mutex<Option<std::collections::HashMap<char, f64>>> = std::sync::Mutex::new(None);
    let mesurer = |c: char| -> Option<f64> {
        POLICE.with(|police| {
            let police = police.as_ref()?;
            let mut unites = [0u16; 2];
            let unites = c.encode_utf16(&mut unites);
            let mut glyphes = [0u16; 2];
            let n = unites.len();
            // SAFETY : tableaux de `n` éléments, lus / écrits par CoreText seulement.
            let ok = unsafe {
                police.glyphs_for_characters(
                    std::ptr::NonNull::new(unites.as_mut_ptr())?,
                    std::ptr::NonNull::new(glyphes.as_mut_ptr())?,
                    n as isize,
                )
            };
            if !ok {
                return None;
            }
            let avance = unsafe {
                police.advances_for_glyphs(
                    CTFontOrientation::Horizontal,
                    std::ptr::NonNull::new(glyphes.as_mut_ptr())?,
                    std::ptr::null_mut(),
                    1,
                )
            };
            (avance > 0.0).then_some(avance / 100.0)
        })
    };
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(std::collections::HashMap::new);
    mot.chars()
        .map(|c| {
            *cache
                .entry(c)
                .or_insert_with(|| mesurer(c).unwrap_or_else(|| bscreen_core::texte_lu::largeur_relative(c)))
        })
        .collect()
}

/// Tout le texte de l'image, ligne par ligne (OCR précis), avec la place de chaque mot et de
/// chaque caractère, dans l'ordre de lecture — pour sélectionner au caractère près dans la frise.
pub fn lire_mots(image: &CGImage) -> Result<Vec<bscreen_core::texte_lu::LigneLue>> {
    use bscreen_core::texte_lu::{
        LigneLue, MotLu, bords_selon, caler_sur_les_creux, chasse_fixe_probable, grille_par_creux_detaillee, mots_d_une_ligne,
        pas_par_debuts,
    };
    let diagnostic = std::env::var_os("BSCREEN_DIAGNOSTIC_SELECTION").is_some();
    let chrono = std::time::Instant::now();
    let pixels = crate::lecture::en_gris((image as *const CGImage).cast());
    let duree_pixels = chrono.elapsed();
    let chrono = std::time::Instant::now();
    let observations = reconnaitre(image, false)?;
    let duree_ocr = chrono.elapsed();
    let chrono = std::time::Instant::now();
    let mut lignes = Vec::new();
    for observation in observations {
        let Some(texte) = observation.topCandidates(1).firstObject() else {
            continue;
        };
        let original = texte.string().to_string();
        let ligne_cadre = en_haut_a_gauche(unsafe { observation.boundingBox() });
        let caracteres: Vec<char> = original.chars().collect();
        let n = caracteres.len();
        if n == 0 {
            continue;
        }
        let mut utf16 = Vec::with_capacity(n + 1);
        let mut u = 0usize;
        for c in &caracteres {
            utf16.push(u);
            u += c.len_utf16();
        }
        utf16.push(u);
        let total = n as f64;
        // Les mots, avec leur boîte (Vision)…
        let mut mots: Vec<MotLu> = mots_d_une_ligne(&original)
            .into_iter()
            .map(|(debut, fin)| MotLu {
                texte: caracteres[debut..fin].iter().collect(),
                cadre: cadre_du_passage(&texte, utf16[debut], utf16[fin] - utf16[debut])
                    .map(en_haut_a_gauche)
                    .unwrap_or([
                        ligne_cadre[0] + ligne_cadre[2] * debut as f64 / total,
                        ligne_cadre[1],
                        ligne_cadre[2] * (fin - debut) as f64 / total,
                        ligne_cadre[3],
                    ]),
                debut,
                fin,
            })
            .collect();
        mots.sort_by_key(|m| m.debut);
        // … resserrée sur l'encre : les espaces entre les mots sont lus dans l'image.
        if let Some(p) = pixels.as_ref() {
            resserrer_ligne(p, ligne_cadre, &mut mots);
        }
        // Chasse fixe (terminal, code) ? Les deux modèles sont ajustés sur les mots de la ligne.
        let largeur_px = pixels.as_ref().map(|p| p.largeur as f64).unwrap_or(1.0);
        let mesures: Vec<(usize, f64, f64)> = mots
            .iter()
            .map(|m| {
                (
                    m.fin - m.debut,
                    m.cadre[2] * largeur_px,
                    largeurs_systeme(&m.texte).iter().sum::<f64>(),
                )
            })
            .collect();
        // En chasse fixe, la grille des cases est lue dans l'image (pas et position) : chaque mot s'y
        // cale, même si le texte lu a une espace en trop ou en moins. À défaut, le pas vient des
        // débuts de mots.
        let chasse_fixe: Option<(f64, f64, Option<f64>)> = chasse_fixe_probable(&mesures).map(|(pas, approches)| {
            let debuts: Vec<(usize, f64)> = mots.iter().map(|m| (m.debut, m.cadre[0] * largeur_px)).collect();
            let estime = pas_par_debuts(&debuts).unwrap_or(pas);
            let grille = pixels.as_ref().and_then(|p| {
                let marge = 6.0 / largeur_px;
                let (debut_px, encre) = profil_encre(
                    p,
                    [ligne_cadre[0] - marge, ligne_cadre[1], ligne_cadre[2] + 2.0 * marge, ligne_cadre[3]],
                )?;
                let (grille, meilleur, moyenne) = grille_par_creux_detaillee(&encre, estime);
                if std::env::var_os("BSCREEN_DIAGNOSTIC_SELECTION").is_some() {
                    let encrees = encre.iter().filter(|&&e| e > 0.0).count();
                    eprintln!(
                        "   grille : meilleur {meilleur:.2}, moyenne {moyenne:.2}, colonnes encrées {encrees}/{}, encre min {:.2}",
                        encre.len(),
                        encre.iter().copied().fold(f32::MAX, f32::min)
                    );
                }
                grille.map(|(pas, decalage)| (pas, debut_px as f64 + decalage))
            });
            if std::env::var_os("BSCREEN_DIAGNOSTIC_SELECTION").is_some() {
                eprintln!("chasse fixe : pas largeurs {pas:.2}, débuts {estime:.2}, grille {grille:?} — « {original} »");
            }
            match grille {
                Some((pas, origine)) => (pas, approches, Some(origine)),
                None => (estime, approches, None),
            }
        });
        // Les bords de chaque caractère.
        let mut bords = vec![f64::NAN; n + 1];
        for mot in &mots {
            let (x0, x1) = (mot.cadre[0], mot.cadre[0] + mot.cadre[2]);
            let nb = mot.fin - mot.debut;
            let estimes: Vec<f64> = match chasse_fixe {
                // Une grille au pas de la ligne, ancrée sur le mot (une erreur de lecture ailleurs
                // ne décale rien ici) ; l'encre commence une demi-approche après le bord.
                Some((pas, approches, origine)) => {
                    let depart = match origine {
                        // La limite de case juste avant l'encre du mot.
                        Some(o) => o + (((x0 * largeur_px - o) / pas) + 0.15).floor() * pas,
                        None => x0 * largeur_px - approches / 2.0,
                    };
                    (0..=nb).map(|j| (depart + j as f64 * pas) / largeur_px).collect()
                }
                None => {
                    let mut estimes = bords_selon(&largeurs_systeme(&mot.texte), x0, x1);
                    if let Some(p) = pixels.as_ref()
                        && nb >= 2
                        && let Some((debut_px, encre)) = profil_encre(p, mot.cadre)
                    {
                        let interieurs: Vec<f64> =
                            estimes[1..estimes.len() - 1].iter().map(|x| x * largeur_px).collect();
                        let fenetre = 0.55 * mot.cadre[2] * largeur_px / nb as f64;
                        let calees = caler_sur_les_creux(&interieurs, &encre, debut_px, fenetre);
                        for (k, x) in calees.into_iter().enumerate() {
                            estimes[k + 1] = x / largeur_px;
                        }
                    }
                    estimes
                }
            };
            for (k, x) in estimes.into_iter().enumerate() {
                bords[mot.debut + k] = x;
            }
        }
        // Les espaces : entre la fin d'un mot et le début du suivant (et aux bouts de la ligne).
        let mut connu: Vec<usize> = (0..=n).filter(|&i| !bords[i].is_nan()).collect();
        if bords[0].is_nan() {
            bords[0] = ligne_cadre[0];
            connu.insert(0, 0);
        }
        if bords[n].is_nan() {
            bords[n] = ligne_cadre[0] + ligne_cadre[2];
            connu.push(n);
        }
        for paire in connu.windows(2) {
            let (a, z) = (paire[0], paire[1]);
            for i in a + 1..z {
                bords[i] = bords[a] + (bords[z] - bords[a]) * (i - a) as f64 / (z - a) as f64;
            }
        }
        // Les bords ne reculent jamais.
        for i in 1..=n {
            if bords[i] < bords[i - 1] {
                bords[i] = bords[i - 1];
            }
        }
        lignes.push(LigneLue {
            texte: original,
            cadre: ligne_cadre,
            mots,
            bords,
        });
    }
    // Ordre de lecture : de haut en bas, puis de gauche à droite.
    // (Pas de comparaison « à peu près à la même hauteur » dans le tri : elle n'est pas
    // transitive et Rust panique. On trie par hauteur, on forme les rangées, puis on trie chaque
    // rangée de gauche à droite.)
    lignes.sort_by(|a, b| a.cadre[1].total_cmp(&b.cadre[1]));
    let mut debut = 0;
    while debut < lignes.len() {
        let haut = lignes[debut].cadre[1];
        let fin = lignes[debut..].iter().position(|l| l.cadre[1] - haut >= 0.008).map_or(lignes.len(), |p| debut + p);
        lignes[debut..fin].sort_by(|a, b| a.cadre[0].total_cmp(&b.cadre[0]));
        debut = fin;
    }
    if diagnostic {
        eprintln!(
            "durées : pixels {} ms · OCR {} ms · caractères {} ms",
            duree_pixels.as_millis(),
            duree_ocr.as_millis(),
            chrono.elapsed().as_millis()
        );
    }
    Ok(lignes)
}

/// Lit une image enregistrée sur le disque (PNG, JPEG…) mot à mot et caractère par caractère
/// (pour l'essai de précision de la sélection).
pub fn lire_mots_fichier(chemin: &std::path::Path) -> Result<Vec<bscreen_core::texte_lu::LigneLue>> {
    use objc2::AllocAnyThread;
    let image = objc2_app_kit::NSImage::initWithContentsOfFile(
        objc2_app_kit::NSImage::alloc(),
        &NSString::from_str(&chemin.to_string_lossy()),
    )
    .ok_or_else(|| anyhow!("image illisible : {}", chemin.display()))?;
    let cg = unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }
        .ok_or_else(|| anyhow!("image sans pixels"))?;
    lire_mots(&cg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pliage_sans_accents_et_meme_longueur() {
        let texte = "Élève à l'école, ÇA VA";
        let plie: String = plier(texte).into_iter().collect();
        assert_eq!(plie, "eleve a l'ecole, ca va");
        assert_eq!(plier(texte).len(), texte.chars().count());
    }

    #[test]
    fn occurrences_d_un_mot() {
        let texte = plier("Yo soy Franky — yo SOY la meilleure");
        assert_eq!(occurrences(&texte, &plier("soy")), vec![3, 19]);
        assert!(occurrences(&texte, &plier("dauphin")).is_empty());
    }
}
