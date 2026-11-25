//! Le texte lu sur une image (OCR), pour le sélectionner dans la frise au caractère près.
//!
//! L'OCR d'Apple donne la boîte de chaque ligne et de chaque mot, mais pas celle des caractères
//! (une lettre reçoit la boîte de tout son mot). La place de chaque caractère est donc estimée
//! dans son mot ([`bords_estimes`]), puis calée sur les vrais blancs entre les lettres, lus
//! dans l'image ([`caler_sur_les_creux`]).
//!
//! Coordonnées normalisées : 0 à 1, origine en haut à gauche de l'image.

/// Un mot lu : son texte, son cadre, et sa place dans le texte de sa ligne (en caractères).
#[derive(Clone, Debug, PartialEq)]
pub struct MotLu {
    pub texte: String,
    pub cadre: [f64; 4],
    pub debut: usize,
    pub fin: usize,
}

/// Une ligne lue : son texte (avec ses espaces), son cadre, ses mots, et la position (x) du bord
/// gauche de chaque caractère, plus le bord droit du dernier (`bords.len()` = caractères + 1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LigneLue {
    pub texte: String,
    pub cadre: [f64; 4],
    pub mots: Vec<MotLu>,
    pub bords: Vec<f64>,
}

/// Une place entre deux caractères : la ligne, et le rang du caractère qui suit (0 = début de ligne).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub ligne: usize,
    pub car: usize,
}

/// Les mots d'une ligne de texte : (premier caractère, caractère après le dernier), en caractères.
pub fn mots_d_une_ligne(texte: &str) -> Vec<(usize, usize)> {
    let mut mots = Vec::new();
    let mut debut: Option<usize> = None;
    let mut n = 0;
    for (i, c) in texte.chars().enumerate() {
        n = i + 1;
        match (c.is_whitespace(), debut) {
            (false, None) => debut = Some(i),
            (true, Some(d)) => {
                mots.push((d, i));
                debut = None;
            }
            _ => {}
        }
    }
    if let Some(d) = debut {
        mots.push((d, n));
    }
    mots
}

/// Largeur relative typique d'un caractère dans une police d'interface (pour une première estimation).
pub fn largeur_relative(c: char) -> f64 {
    match c {
        'i' | 'l' | 'j' | '!' | '|' | '\'' | '.' | ',' | ':' | ';' | 'I' | '`' => 0.32,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '{' | '}' | '/' | '\\' | '"' | '-' => 0.45,
        'm' | 'w' => 0.88,
        'M' | 'W' | '@' | '%' | 'œ' | 'æ' | 'Œ' | 'Æ' => 0.98,
        'A'..='Z' | 'À'..='Ý' => 0.68,
        '0'..='9' => 0.6,
        c if c.is_whitespace() => 0.3,
        c if c.is_ascii() => 0.56,
        // Idéogrammes, emoji : larges.
        c if (c as u32) >= 0x2E80 => 1.0,
        _ => 0.58,
    }
}

/// Les bords des caractères d'un mot de `x0` à `x1`, répartis selon leur largeur relative
/// (`chasse_fixe` : tous de même largeur, comme dans un terminal). `n + 1` valeurs.
pub fn bords_estimes(mot: &str, x0: f64, x1: f64, chasse_fixe: bool) -> Vec<f64> {
    let largeurs: Vec<f64> = mot
        .chars()
        .map(|c| if chasse_fixe { 1.0 } else { largeur_relative(c) })
        .collect();
    bords_selon(&largeurs, x0, x1)
}

/// Les bords de caractères de largeurs relatives données, répartis de `x0` à `x1`.
pub fn bords_selon(largeurs: &[f64], x0: f64, x1: f64) -> Vec<f64> {
    let total: f64 = largeurs.iter().sum::<f64>().max(1e-9);
    let mut bords = Vec::with_capacity(largeurs.len() + 1);
    let mut x = x0;
    bords.push(x);
    for l in largeurs {
        x += (x1 - x0) * l / total;
        bords.push(x);
    }
    if let Some(dernier) = bords.last_mut() {
        *dernier = x1;
    }
    bords
}

/// Ajuste `largeur ≈ a × x − b` aux moindres carrés ; rend (a, b, somme des carrés des écarts).
fn ajuster(points: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    let n = points.len() as f64;
    if points.len() < 3 {
        return None;
    }
    let (sx, sy) = points.iter().fold((0.0, 0.0), |(a, b), &(x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let sxx: f64 = points.iter().map(|&(x, _)| (x - mx) * (x - mx)).sum();
    if sxx < 1e-9 {
        return None;
    }
    let sxy: f64 = points.iter().map(|&(x, y)| (x - mx) * (y - my)).sum();
    let a = sxy / sxx;
    let b = a * mx - my;
    let residus = points.iter().map(|&(x, y)| (a * x - b - y).powi(2)).sum();
    Some((a, b, residus))
}

/// La ligne est-elle en chasse fixe (terminal, code) ? Pour chaque mot : (nombre de caractères,
/// largeur de son encre, somme des avances relatives dans la police du système). On ajuste les
/// deux modèles (largeur = pas × caractères − approches, ou = échelle × avances − approches) et on
/// garde le meilleur. Rend (pas, approches) si la chasse fixe l'emporte nettement.
pub fn chasse_fixe_probable(mots: &[(usize, f64, f64)]) -> Option<(f64, f64)> {
    let fixe: Vec<(f64, f64)> = mots.iter().map(|&(n, l, _)| (n as f64, l)).collect();
    let proportionnelle: Vec<(f64, f64)> = mots.iter().map(|&(_, l, s)| (s, l)).collect();
    let (pas, approches, ecart_fixe) = ajuster(&fixe)?;
    let ecart_proportionnel = ajuster(&proportionnelle).map(|(_, _, e)| e).unwrap_or(f64::MAX);
    (pas > 0.0 && approches > -pas && ecart_fixe < ecart_proportionnel * 0.6).then_some((pas, approches.max(0.0)))
}

/// Le pas d'une ligne en chasse fixe, d'après le début de chaque mot : (rang du premier caractère,
/// position de son encre). Médiane des pentes entre toutes les paires (Theil-Sen) : quelques mots
/// décalés par une erreur de lecture ne la font pas bouger.
pub fn pas_par_debuts(debuts: &[(usize, f64)]) -> Option<f64> {
    let mut pentes: Vec<f64> = Vec::new();
    for (i, &(di, xi)) in debuts.iter().enumerate() {
        for &(dj, xj) in &debuts[i + 1..] {
            if dj != di {
                pentes.push((xj - xi) / (dj as f64 - di as f64));
            }
        }
    }
    if pentes.len() < 3 {
        return None;
    }
    pentes.sort_by(f64::total_cmp);
    Some(pentes[pentes.len() / 2])
}

/// Le pas d'une ligne en chasse fixe, lu dans l'image : le profil d'encre de la ligne se répète à
/// ce pas (une case par caractère). Pic d'autocorrélation entre 0,8 et 1,25 fois l'estimation,
/// affiné au sous-pixel. `None` si la répétition n'est pas nette.
pub fn pas_par_periodicite(encre: &[f32], estime: f64) -> Option<f64> {
    if encre.len() < 64 || estime < 3.0 {
        return None;
    }
    let moyenne = encre.iter().map(|&e| f64::from(e)).sum::<f64>() / encre.len() as f64;
    let centre: Vec<f64> = encre.iter().map(|&e| f64::from(e) - moyenne).collect();
    let energie: f64 = centre.iter().map(|e| e * e).sum();
    if energie < 1e-9 {
        return None;
    }
    let correlation = |decalage: usize| -> f64 {
        if decalage >= centre.len() {
            return 0.0;
        }
        let n = centre.len() - decalage;
        let s: f64 = (0..n).map(|i| centre[i] * centre[i + decalage]).sum();
        s / energie * centre.len() as f64 / n as f64
    };
    let (mini, maxi) = ((estime * 0.8).floor() as usize, (estime * 1.25).ceil() as usize);
    let (meilleur, r) = (mini.max(2)..=maxi)
        .map(|d| (d, correlation(d)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if r < 0.25 {
        return None;
    }
    // Sommet de la parabole passant par les trois points autour du pic.
    let (r0, r2) = (correlation(meilleur - 1), correlation(meilleur + 1));
    let denominateur = r0 - 2.0 * r + r2;
    let affinage = if denominateur.abs() > 1e-9 { 0.5 * (r0 - r2) / denominateur } else { 0.0 };
    Some(meilleur as f64 + affinage.clamp(-0.5, 0.5))
}

/// La grille d'une ligne en chasse fixe, lue dans l'image : au bon pas, les limites des cases tombent
/// toutes dans les blancs entre les lettres. Pour chaque pas entre 0,8 et 1,25 fois l'estimation
/// (au 1/50 de pixel) et chaque décalage (au quart de pixel), on mesure l'encre autour des limites
/// (à un dixième de case de part et d'autre). Les blancs étant larges, plusieurs pas voisins sont
/// presque aussi bons : on prend le milieu de cette vallée. Rend (pas, décalage) en pixels du
/// profil (la limite k est à `décalage + k × pas`), si la grille est nettement plus blanche que
/// la moyenne.
pub fn grille_par_creux(encre: &[f32], estime: f64) -> Option<(f64, f64)> {
    grille_par_creux_detaillee(encre, estime).0
}

/// Comme [`grille_par_creux`], avec le meilleur score et l'encre moyenne (pour le diagnostic).
pub fn grille_par_creux_detaillee(encre: &[f32], estime: f64) -> (Option<(f64, f64)>, f64, f64) {
    let mut detail = (f64::NAN, f64::NAN);
    let grille = grille_interne(encre, estime, &mut detail);
    (grille, detail.0, detail.1)
}

fn grille_interne(encre: &[f32], estime: f64, detail: &mut (f64, f64)) -> Option<(f64, f64)> {
    let encrees: Vec<usize> = (0..encre.len()).filter(|&i| encre[i] > 0.0).collect();
    let (&premiere, &derniere) = (encrees.first()?, encrees.last()?);
    if derniere <= premiere + 40 || estime < 3.0 {
        return None;
    }
    let moyenne =
        (premiere..=derniere).map(|i| f64::from(encre[i])).sum::<f64>() / (derniere - premiere + 1) as f64;
    if moyenne <= 0.0 {
        return None;
    }
    // Somme cumulée : l'encre d'un intervalle en temps constant.
    let mut cumul = vec![0f64; encre.len() + 1];
    for (i, &e) in encre.iter().enumerate() {
        cumul[i + 1] = cumul[i] + f64::from(e);
    }
    let encre_entre = |a: f64, b: f64| -> f64 {
        let a = a.max(0.0).min(encre.len() as f64);
        let b = b.max(a).min(encre.len() as f64);
        let lire = |x: f64| {
            let i = x.floor() as usize;
            if i >= encre.len() {
                return cumul[encre.len()];
            }
            cumul[i] + (x - x.floor()) * f64::from(encre[i])
        };
        lire(b) - lire(a)
    };
    // L'encre moyenne autour des limites d'une grille (pas, décalage).
    let mesurer = |pas: f64, decalage: f64| -> Option<f64> {
        let demi = pas / 10.0;
        let mut x = premiere as f64 - pas + decalage;
        let (mut somme, mut n) = (0.0, 0usize);
        while x <= derniere as f64 + pas {
            if x >= premiere as f64 - 1.0 && x <= derniere as f64 + 1.0 {
                somme += encre_entre(x - demi, x + demi) / (2.0 * demi);
                n += 1;
            }
            x += pas;
        }
        (n >= 4).then(|| somme / n as f64)
    };
    // Le meilleur décalage d'un pas (au demi-pixel, puis au quart de pixel autour).
    let meilleur_decalage = |pas: f64| -> Option<(f64, f64)> {
        let grossier = (0..(pas / 0.5) as usize)
            .filter_map(|k| mesurer(pas, k as f64 * 0.5).map(|s| (s, k as f64 * 0.5)))
            .min_by(|a, b| a.0.total_cmp(&b.0))?;
        [-0.25, 0.0, 0.25]
            .iter()
            .filter_map(|d| {
                let decalage = (grossier.1 + d).rem_euclid(pas);
                mesurer(pas, decalage).map(|s| (s, decalage))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
    };
    // Grossier (pas de 0,1 px), puis fin (0,02 px) autour des meilleurs.
    let (mini, maxi) = (estime * 0.8, estime * 1.25);
    let mut grossiers: Vec<(f64, f64)> = Vec::new();
    let mut pas = mini;
    while pas <= maxi {
        if let Some((s, _)) = meilleur_decalage(pas) {
            grossiers.push((s, pas));
        }
        pas += 0.1;
    }
    let plancher = grossiers.iter().map(|g| g.0).fold(f64::MAX, f64::min);
    let mut resultats: Vec<(f64, f64, f64)> = Vec::new();
    for &(s, centre) in &grossiers {
        if s > plancher + moyenne * 0.05 {
            continue;
        }
        let mut pas = (centre - 0.1).max(mini);
        while pas <= (centre + 0.1).min(maxi) {
            if let Some((score, decalage)) = meilleur_decalage(pas) {
                resultats.push((score, pas, decalage + premiere as f64 - pas));
            }
            pas += 0.02;
        }
    }
    let meilleur = resultats.iter().map(|r| r.0).fold(f64::MAX, f64::min);
    *detail = (meilleur, moyenne);
    // (La ligne est déjà reconnue en chasse fixe : on peut être tolérant — `_`, `m`, `w` touchent
    // presque les limites de leur case.)
    if meilleur >= moyenne * 0.5 {
        return None;
    }
    // Le milieu de la vallée : la médiane des pas presque aussi bons que le meilleur.
    let tolerance = meilleur + moyenne * 0.02;
    let mut bons: Vec<f64> = resultats.iter().filter(|r| r.0 <= tolerance).map(|r| r.1).collect();
    bons.sort_by(f64::total_cmp);
    let pas = bons[bons.len() / 2];
    let decalage = resultats
        .iter()
        .filter(|r| (r.1 - pas).abs() < 1e-9)
        .min_by(|a, b| a.0.total_cmp(&b.0))?
        .2;
    // Affinage : chaque blanc entre deux lettres est centré sur une limite de case. Les centres des
    // blancs, rattachés à leur limite (k), donnent pas et origine par moindres carrés. Les blancs
    // d'une espace (centrés au milieu d'une case) sont écartés.
    let maximum = encre.iter().copied().fold(0f32, f32::max);
    let vide = maximum * 0.03;
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut i = premiere;
    while i <= derniere {
        if encre[i] <= vide {
            let depart = i;
            while i <= derniere && encre[i] <= vide {
                i += 1;
            }
            let centre = (depart + i) as f64 / 2.0;
            let k = ((centre - decalage) / pas).round();
            if (centre - (decalage + k * pas)).abs() < pas / 3.0 {
                points.push((k, centre));
            }
        } else {
            i += 1;
        }
    }
    if let Some((pente, moins_origine, _)) = ajuster(&points)
        && (pente - pas).abs() < pas * 0.05
    {
        return Some((pente, -moins_origine));
    }
    Some((pas, decalage))
}

/// Cale des séparations attendues (en pixels, croissantes) sur les creux d'encre les plus proches.
/// `encre[k]` est la quantité d'encre de la colonne `debut + k` ; on cherche à ± `fenetre` pixels,
/// en préférant, à encre égale, la colonne la plus proche de l'attendu. Les séparations restent
/// dans l'ordre et à au moins un pixel l'une de l'autre.
pub fn caler_sur_les_creux(attendues: &[f64], encre: &[f32], debut: usize, fenetre: f64) -> Vec<f64> {
    if encre.is_empty() {
        return attendues.to_vec();
    }
    let fin = debut + encre.len();
    let mut calees: Vec<f64> = Vec::with_capacity(attendues.len());
    let maximum = encre.iter().copied().fold(0f32, f32::max).max(1e-6);
    for &b in attendues {
        let plancher = calees.last().map(|&p| p + 1.0).unwrap_or(f64::MIN);
        let a = ((b - fenetre).floor().max(debut as f64).max(plancher.ceil())) as usize;
        let z = ((b + fenetre).ceil() as usize).min(fin.saturating_sub(1));
        if a > z {
            calees.push(b.max(plancher));
            continue;
        }
        let meilleur = (a..=z)
            .min_by(|&p, &q| {
                // Encre (normalisée ; presque rien = un vrai blanc) d'abord, puis la distance à
                // l'attendu, en léger appoint.
                let cout = |c: usize| {
                    let e = f64::from(encre[c - debut] / maximum);
                    let e = if e < 0.03 { 0.0 } else { e };
                    e + 0.08 * ((c as f64 + 0.5) - b).abs() / fenetre.max(1.0)
                };
                cout(p).total_cmp(&cout(q))
            })
            .unwrap_or(a);
        calees.push((meilleur as f64 + 0.5).max(plancher));
    }
    calees
}

/// La place la plus proche d'un point (x, y normalisés) : la ligne d'abord (à la verticale, en
/// préférant celles qui couvrent x), puis le bord de caractère le plus proche de x.
/// `rapport` = largeur / hauteur de l'image, pour comparer les distances en vrais points.
pub fn position_sous(lignes: &[LigneLue], x: f64, y: f64, rapport: f64) -> Option<Position> {
    let distance = |l: &LigneLue| {
        let [lx, ly, ll, lh] = l.cadre;
        let dy = if y < ly {
            ly - y
        } else if y > ly + lh {
            y - ly - lh
        } else {
            0.0
        };
        let dx = if x < lx {
            lx - x
        } else if x > lx + ll {
            x - lx - ll
        } else {
            0.0
        };
        // Une ligne à côté (même hauteur) compte moins qu'une ligne au-dessus ou au-dessous.
        dy * 4.0 + dx * rapport * 0.25
    };
    let (rang, ligne) = lignes
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.bords.is_empty())
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))?;
    if distance(ligne) > 0.2 {
        return None;
    }
    let car = ligne
        .bords
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
        .map(|(i, _)| i)
        .unwrap_or(0);
    Some(Position { ligne: rang, car })
}

/// Le point (x, y normalisés) est-il sur une ligne de texte (à un quart de hauteur près) ?
pub fn sur_du_texte(lignes: &[LigneLue], x: f64, y: f64) -> bool {
    lignes.iter().any(|l| {
        let [lx, ly, ll, lh] = l.cadre;
        let marge = lh * 0.25;
        x >= lx - marge && x <= lx + ll + marge && y >= ly - marge && y <= ly + lh + marge
    })
}

/// Le mot autour d'une place (double-clic) : (début, fin) dans sa ligne, ou la place elle-même.
pub fn mot_autour(lignes: &[LigneLue], p: Position) -> (Position, Position) {
    let Some(ligne) = lignes.get(p.ligne) else { return (p, p) };
    let mot = ligne
        .mots
        .iter()
        .find(|m| p.car >= m.debut && p.car < m.fin)
        .or_else(|| ligne.mots.iter().min_by_key(|m| m.debut.abs_diff(p.car).min(m.fin.abs_diff(p.car))));
    match mot {
        Some(m) => (
            Position { ligne: p.ligne, car: m.debut },
            Position { ligne: p.ligne, car: m.fin },
        ),
        None => (p, p),
    }
}

/// Toute la ligne d'une place (triple-clic).
pub fn ligne_autour(lignes: &[LigneLue], p: Position) -> (Position, Position) {
    let n = lignes.get(p.ligne).map(|l| l.texte.chars().count()).unwrap_or(0);
    (Position { ligne: p.ligne, car: 0 }, Position { ligne: p.ligne, car: n })
}

/// Une ligne entre le début et la fin d'une sélection en fait-elle partie ? Oui si elle chevauche
/// (au moins à moitié) la colonne formée par la première et la dernière ligne : une barre latérale
/// à côté d'un texte n'est pas prise avec lui.
fn dans_la_colonne(lignes: &[LigneLue], debut: Position, fin: Position, rang: usize) -> bool {
    if rang == debut.ligne || rang == fin.ligne {
        return true;
    }
    let bornes = |l: &LigneLue| (l.cadre[0], l.cadre[0] + l.cadre[2]);
    let (Some(a), Some(z), Some(l)) = (lignes.get(debut.ligne), lignes.get(fin.ligne), lignes.get(rang)) else {
        return false;
    };
    let (a0, a1) = bornes(a);
    let (z0, z1) = bornes(z);
    let (c0, c1) = (a0.min(z0), a1.max(z1));
    let (l0, l1) = bornes(l);
    let commun = (l1.min(c1) - l0.max(c0)).max(0.0);
    commun >= 0.5 * (l1 - l0)
}

/// Le texte entre deux places (dans l'ordre de lecture), une ligne par ligne lue.
pub fn texte_entre(lignes: &[LigneLue], a: Position, b: Position) -> String {
    let (debut, fin) = if a <= b { (a, b) } else { (b, a) };
    let mut morceaux = Vec::new();
    for (rang, ligne) in lignes.iter().enumerate().take(fin.ligne + 1).skip(debut.ligne) {
        if !dans_la_colonne(lignes, debut, fin, rang) {
            continue;
        }
        let caracteres: Vec<char> = ligne.texte.chars().collect();
        let de = if rang == debut.ligne { debut.car } else { 0 };
        let a = if rang == fin.ligne { fin.car } else { caracteres.len() };
        let (de, a) = (de.min(caracteres.len()), a.min(caracteres.len()));
        if de < a {
            morceaux.push(caracteres[de..a].iter().collect::<String>());
        } else if rang != debut.ligne && rang != fin.ligne {
            morceaux.push(String::new());
        }
    }
    morceaux.join("\n")
}

/// Les bandes à surligner entre deux places : une par ligne, à la hauteur de la ligne.
pub fn bandes(lignes: &[LigneLue], a: Position, b: Position) -> Vec<[f64; 4]> {
    let (debut, fin) = if a <= b { (a, b) } else { (b, a) };
    let mut rects = Vec::new();
    for (rang, ligne) in lignes.iter().enumerate().take(fin.ligne + 1).skip(debut.ligne) {
        if !dans_la_colonne(lignes, debut, fin, rang) {
            continue;
        }
        let n = ligne.bords.len().saturating_sub(1);
        let de = if rang == debut.ligne { debut.car.min(n) } else { 0 };
        let a = if rang == fin.ligne { fin.car.min(n) } else { n };
        if a <= de || ligne.bords.is_empty() {
            continue;
        }
        let (x0, x1) = (ligne.bords[de], ligne.bords[a]);
        rects.push([x0, ligne.cadre[1], x1 - x0, ligne.cadre[3]]);
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoupe_en_mots() {
        assert_eq!(mots_d_une_ligne("Lac  d'Annecy est"), vec![(0, 3), (5, 13), (14, 17)]);
        assert!(mots_d_une_ligne("   ").is_empty());
        assert_eq!(mots_d_une_ligne("éclair"), vec![(0, 6)]);
    }

    #[test]
    fn estimation_des_bords() {
        let b = bords_estimes("mil", 0.0, 1.0, false);
        assert_eq!(b.len(), 4);
        assert!(b[1] - b[0] > 2.0 * (b[2] - b[1]), "« m » bien plus large que « i » : {b:?}");
        assert_eq!(bords_estimes("abcd", 0.0, 4.0, true), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn chasse_fixe_reconnue() {
        // Menlo : pas de 14,45 px, ~5 px d'approches en tout par mot ; avances système variées.
        let mono = [(6, 82.0, 3.6), (3, 38.5, 1.4), (4, 52.8, 2.1), (8, 110.6, 4.0), (2, 23.9, 0.9)];
        let (pas, _) = chasse_fixe_probable(&mono).expect("chasse fixe");
        assert!((pas - 14.45).abs() < 0.5, "pas {pas}");
        // Police proportionnelle : la largeur suit les avances, pas le nombre de lettres.
        let prop = [(6, 70.0, 3.6), (3, 27.0, 1.4), (4, 41.0, 2.1), (8, 78.0, 4.0), (2, 17.0, 0.9)];
        assert!(chasse_fixe_probable(&prop).is_none());
    }

    #[test]
    fn pas_robuste_aux_erreurs_de_lecture() {
        // Pas de 14,45 px, petites approches variables ; le 4e mot a été lu avec une espace en trop
        // avant lui (rang décalé de 1).
        let rangs = [0usize, 3, 10, 16, 20, 27, 33, 41];
        let approches = [1.5, 0.5, 2.5, 1.0, 3.0, 0.5, 2.0, 1.5];
        let debuts: Vec<(usize, f64)> = rangs
            .iter()
            .zip(approches)
            .enumerate()
            .map(|(k, (&d, a))| (if k == 3 { d + 1 } else { d }, 40.0 + d as f64 * 14.45 + a))
            .collect();
        let pas = pas_par_debuts(&debuts).unwrap();
        assert!((pas - 14.45).abs() < 0.4, "pas {pas}");
    }

    #[test]
    fn pas_lu_dans_l_image() {
        // Des « lettres » de 9 px tous les 14,45 px (une case sur six vide : une espace).
        let mut encre = vec![0f32; 900];
        for k in 0..60 {
            if k % 6 == 5 {
                continue;
            }
            let debut = (10.0 + k as f64 * 14.45) as usize;
            for c in debut + 2..debut + 11 {
                if c < encre.len() {
                    encre[c] = 3.0 + (k % 3) as f32;
                }
            }
        }
        let pas = pas_par_periodicite(&encre, 13.8).unwrap();
        assert!((pas - 14.45).abs() < 0.15, "pas {pas}");
        assert!(pas_par_periodicite(&vec![1.0; 900], 13.8).is_none(), "pas de répétition");
    }

    #[test]
    fn grille_lue_dans_les_blancs() {
        // Des lettres de largeurs variées (5 à 11 px) dans des cases de 14,45 px, une espace sur six.
        let mut encre = vec![0f32; 900];
        for k in 0..58 {
            if k % 6 == 5 {
                continue;
            }
            let case = 10.0 + k as f64 * 14.45;
            let largeur = 5 + (k * 7) % 7;
            let debut = (case + (14.45 - largeur as f64) / 2.0) as usize;
            encre[debut..debut + largeur].fill(2.0 + (k % 4) as f32);
        }
        let (pas, decalage) = grille_par_creux(&encre, 13.8).unwrap();
        assert!((pas - 14.45).abs() < 0.1, "pas {pas}");
        // Une limite de case près de 10 + k × 14,45 (au demi-pixel près, modulo le pas).
        let phase = (decalage - 10.0).rem_euclid(pas);
        assert!(phase < 1.0 || pas - phase < 1.0, "décalage {decalage} (phase {phase})");
    }

    #[test]
    fn calage_sur_les_blancs() {
        // Trois lettres de 10 px séparées par des blancs aux colonnes 9 et 21 (pas 10 et 20).
        let mut encre = vec![5.0f32; 32];
        encre[9] = 0.0;
        encre[21] = 0.0;
        let calees = caler_sur_les_creux(&[10.0, 20.0], &encre, 0, 3.0);
        assert_eq!(calees, vec![9.5, 21.5]);
        // Pas de creux dans la fenêtre : on reste près de l'attendu.
        let plat = vec![5.0f32; 32];
        assert_eq!(caler_sur_les_creux(&[10.0], &plat, 0, 3.0), vec![9.5]);
    }

    fn ligne(texte: &str, y: f64) -> LigneLue {
        let n = texte.chars().count();
        LigneLue {
            texte: texte.into(),
            cadre: [0.1, y, 0.01 * n as f64, 0.02],
            mots: mots_d_une_ligne(texte)
                .into_iter()
                .map(|(d, f)| MotLu {
                    texte: texte.chars().skip(d).take(f - d).collect(),
                    cadre: [0.1 + 0.01 * d as f64, y, 0.01 * (f - d) as f64, 0.02],
                    debut: d,
                    fin: f,
                })
                .collect(),
            bords: (0..=n).map(|i| 0.1 + 0.01 * i as f64).collect(),
        }
    }

    #[test]
    fn selection_au_caractere_pres() {
        let lignes = vec![ligne("Bonjour Camille", 0.10), ligne("les dauphins", 0.13)];
        // Au milieu de « Camille » (entre « Ca » et « mille »).
        let p = position_sous(&lignes, 0.1 + 0.01 * 10.0 + 0.002, 0.11, 1.5).unwrap();
        assert_eq!(p, Position { ligne: 0, car: 10 });
        // Entre les deux lignes, un peu plus près de la seconde : la seconde.
        assert_eq!(position_sous(&lignes, 0.12, 0.1275, 1.5).unwrap().ligne, 1);
        let fin = Position { ligne: 1, car: 3 };
        assert_eq!(texte_entre(&lignes, p, fin), "mille\nles");
        assert_eq!(texte_entre(&lignes, fin, p), "mille\nles", "dans les deux sens");
        let b = bandes(&lignes, p, fin);
        assert_eq!(b.len(), 2);
        assert!((b[0][0] - 0.2).abs() < 1e-9 && (b[1][2] - 0.03).abs() < 1e-9, "{b:?}");
        let (a, z) = mot_autour(&lignes, p);
        assert_eq!(texte_entre(&lignes, a, z), "Camille");
        let (a, z) = ligne_autour(&lignes, Position { ligne: 1, car: 2 });
        assert_eq!(texte_entre(&lignes, a, z), "les dauphins");
        assert!(position_sous(&lignes, 0.9, 0.9, 1.5).is_none(), "loin de tout texte");
    }

    #[test]
    fn une_barre_laterale_n_est_pas_prise() {
        // Ordre de lecture : barre (x 0,02) et éditeur (x 0,3) alternent, ligne par ligne.
        let mut barre = ligne("Fichiers", 0.20);
        barre.cadre[0] = 0.02;
        barre.bords = (0..=8).map(|i| 0.02 + 0.01 * i as f64).collect();
        let lignes = vec![
            ligne_a("let a = 1;", 0.3, 0.20),
            barre,
            ligne_a("let b = 2;", 0.3, 0.23),
            ligne_a("let c = 3;", 0.3, 0.26),
        ];
        let texte = texte_entre(&lignes, Position { ligne: 0, car: 4 }, Position { ligne: 3, car: 5 });
        assert_eq!(texte, "a = 1;\nlet b = 2;\nlet c");
        assert_eq!(bandes(&lignes, Position { ligne: 0, car: 0 }, Position { ligne: 3, car: 3 }).len(), 3);
    }

    fn ligne_a(texte: &str, x: f64, y: f64) -> LigneLue {
        let mut l = ligne(texte, y);
        let decalage = x - l.cadre[0];
        l.cadre[0] = x;
        l.bords.iter_mut().for_each(|b| *b += decalage);
        l.mots.iter_mut().for_each(|m| m.cadre[0] += decalage);
        l
    }
}
