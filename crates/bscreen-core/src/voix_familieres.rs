//! Voix familières : à quelle voix connue rattacher l'empreinte d'un passage (256 nombres,
//! normalisés). Deux passages de la même personne ont des empreintes proches (cosinus élevé).
//!
//! Chaque voix connue garde le centre de ses passages (moyenne normalisée), qui n'apprend que des
//! passages sûrs. Au micro, un passage va à la voix connue la plus proche si elle l'est nettement
//! plus que la deuxième (Julien, Camille au téléphone en haut-parleur…), sinon il reste « ? ». Le
//! son du Mac n'a de voix que pendant un appel ; le reste du temps, c'est une vidéo.
//!
//! Réglé le 22/11/2026 sur les voix de Julien : avant, le micro était Julien dès 50 % de
//! ressemblance, et son centre apprenait de tout ; il avait absorbé Camille au téléphone, et les
//! vidéos avaient créé 946 voix.

/// Taille d'une empreinte.
pub const DIMENSION: usize = 256;
/// Au-dessus : même personne (son du Mac pendant un appel, ou deux inconnus entre eux).
pub const SEUIL_MEME_VOIX: f32 = 0.72;
/// Avance minimale de la meilleure voix sur la deuxième, sinon on ne tranche pas.
pub const MARGE: f32 = 0.04;
/// Au micro : la voix connue la plus proche, si elle l'est au moins autant…
pub const SEUIL_MICRO: f32 = 0.55;
/// … et nettement plus que la deuxième (Julien et Camille au téléphone : 0,14 de ressemblance).
pub const MARGE_MICRO: f32 = 0.08;
/// Au micro, sous ce seuil pour toutes les voix connues : quelqu'un d'autre (une nouvelle voix).
pub const SEUIL_INCONNUE: f32 = 0.45;
/// Une voix n'apprend (son centre bouge) que d'un passage sûr : sinon elle dérive vers une autre.
pub const SEUIL_APPRENDRE: f32 = 0.75;
pub const MARGE_APPRENDRE: f32 = 0.15;
/// Le cœur de la voix de Julien, pour la retrier : ses passages au moins aussi proches.
pub const COEUR_MOI: f32 = 0.80;
/// Passages à partir desquels la voix de Julien est « bien connue ».
const PASSAGES_MOI_CONNU: u32 = 20;
/// Une voix toute neuve apprend de chaque passage qui lui est rattaché, jusqu'à ce nombre.
const PASSAGES_NEUVE: u32 = 5;

/// Une voix connue.
#[derive(Clone, Debug, PartialEq)]
pub struct VoixConnue {
    pub id: i64,
    /// Le centre (normalisé) de ses passages.
    pub centre: Vec<f32>,
    pub passages: u32,
    /// La voix de Julien (déduite du micro).
    pub moi: bool,
}

/// D'où vient un passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origine {
    /// Le micro : Julien, quelqu'un près de lui, ou un téléphone en haut-parleur.
    Micro,
    /// Le son du Mac pendant un appel (une autre app se sert du micro) : la personne au bout du fil.
    Appel,
    /// Le son du Mac hors appel : une vidéo, un film, de la musique. Pas de voix à reconnaître.
    Video,
}

/// Où ranger un passage.
#[derive(Clone, Debug, PartialEq)]
pub enum Attribution {
    /// Une voix déjà connue (son identifiant, la similarité) ; `apprendre` : passage assez sûr
    /// pour affiner le centre de la voix.
    Connue { id: i64, similarite: f32, apprendre: bool },
    /// La voix de Julien, qui n'existe pas encore : à créer.
    NouvelleMoi,
    /// Personne de connu : une nouvelle voix anonyme.
    Nouvelle,
    /// Entre deux voix connues, sans que l'une l'emporte nettement : « ? ».
    Incertaine,
    /// Une vidéo : pas de voix.
    Video,
}

pub fn cosinus(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn normaliser(v: &mut [f32]) {
    let norme = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norme > 1e-9 {
        v.iter_mut().for_each(|x| *x /= norme);
    }
}

/// Les seuils selon l'origine : (même voix, marge sur la deuxième, sous lequel c'est quelqu'un d'autre).
fn seuils(origine: Origine) -> (f32, f32, f32) {
    match origine {
        Origine::Micro => (SEUIL_MICRO, MARGE_MICRO, SEUIL_INCONNUE),
        _ => (SEUIL_MEME_VOIX, MARGE, SEUIL_MEME_VOIX),
    }
}

/// La voix la plus proche parmi `candidates` (identifiant, centre) : rend (identifiant,
/// similarité, avance sur la deuxième), ou rien s'il n'y a aucune candidate.
fn plus_proche<'a>(candidates: impl Iterator<Item = (i64, &'a [f32])>, empreinte: &[f32]) -> Option<(i64, f32, f32)> {
    let mut premiere: Option<(i64, f32)> = None;
    let mut deuxieme = -1.0f32;
    for (id, centre) in candidates {
        let s = cosinus(centre, empreinte);
        match premiere {
            Some((_, s1)) if s <= s1 => deuxieme = deuxieme.max(s),
            _ => {
                if let Some((_, s1)) = premiere {
                    deuxieme = deuxieme.max(s1);
                }
                premiere = Some((id, s));
            }
        }
    }
    premiere.map(|(id, s)| (id, s, s - deuxieme))
}

/// À quelle voix rattacher l'empreinte (normalisée) d'un passage.
pub fn attribuer(voix: &[VoixConnue], empreinte: &[f32], origine: Origine) -> Attribution {
    if origine == Origine::Video {
        return Attribution::Video;
    }
    if origine == Origine::Micro {
        match voix.iter().find(|v| v.moi) {
            None => return Attribution::NouvelleMoi,
            // Tant que la voix de Julien est peu connue, le micro, c'est lui.
            Some(moi) if moi.passages < PASSAGES_MOI_CONNU => {
                return Attribution::Connue {
                    id: moi.id,
                    similarite: cosinus(&moi.centre, empreinte),
                    apprendre: true,
                };
            }
            Some(_) => {}
        }
    }
    // Pendant un appel, la voix du son du Mac n'est jamais celle de Julien : il parle au micro.
    let candidates = voix
        .iter()
        .filter(|v| !(origine == Origine::Appel && v.moi))
        .map(|v| (v.id, v.centre.as_slice()));
    let Some((id, s, avance)) = plus_proche(candidates, empreinte) else {
        return Attribution::Nouvelle;
    };
    let (seuil, marge, inconnue) = seuils(origine);
    if s >= seuil && avance >= marge {
        let passages = voix.iter().find(|v| v.id == id).map_or(0, |v| v.passages);
        let apprendre = passages < PASSAGES_NEUVE || (s >= SEUIL_APPRENDRE && avance >= MARGE_APPRENDRE);
        Attribution::Connue { id, similarite: s, apprendre }
    } else if s < inconnue {
        Attribution::Nouvelle
    } else {
        Attribution::Incertaine
    }
}

/// Le centre mis à jour après un passage de plus (moyenne courante, renormalisée).
/// Le poids d'un nouveau passage est plafonné : une voix suit doucement les changements (micro, pièce).
pub fn nouveau_centre(centre: &[f32], passages: u32, empreinte: &[f32]) -> Vec<f32> {
    let n = passages.min(200) as f32;
    let mut c: Vec<f32> = centre
        .iter()
        .zip(empreinte)
        .map(|(c, e)| (c * n + e) / (n + 1.0))
        .collect();
    normaliser(&mut c);
    c
}

/// La moyenne normalisée d'empreintes (le centre d'une voix).
fn moyenne<'a>(empreintes: impl Iterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut somme = vec![0f32; DIMENSION];
    let mut n = 0;
    for e in empreintes {
        somme.iter_mut().zip(e).for_each(|(s, x)| *s += x);
        n += 1;
    }
    (n > 0).then(|| {
        normaliser(&mut somme);
        somme
    })
}

// ---------------------------------------------------------------------------------------------
// Retrier tout le passé, d'un coup, à partir des empreintes gardées.

/// Un passage à retrier, dans l'ordre du temps.
#[derive(Clone, Debug)]
pub struct PassageATrier {
    pub id: i64,
    pub origine: Origine,
    pub empreinte: Vec<f32>,
    /// Sa voix avant le tri.
    pub voix: Option<i64>,
}

/// Une voix avant le tri.
#[derive(Clone, Debug)]
pub struct VoixAvant {
    pub id: i64,
    pub centre: Vec<f32>,
    /// Nommée par Julien : elle reste, même sans aucun passage.
    pub nommee: bool,
    pub moi: bool,
}

/// Où va un passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Une voix d'avant, gardée (son identifiant).
    Gardee(i64),
    /// Une voix nouvelle (son rang dans [`Tri::nouvelles`]).
    Nouvelle(usize),
    /// « ? » : entre deux voix, ou quelqu'un entendu une seule fois.
    Incertaine,
    /// Une vidéo : pas de voix.
    Video,
}

/// Le résultat d'un tri.
#[derive(Clone, Debug, Default)]
pub struct Tri {
    /// Chaque passage (identifiant) et sa place.
    pub places: Vec<(i64, Place)>,
    /// Les voix d'avant gardées : identifiant, nouveau centre, nombre de passages.
    pub gardees: Vec<(i64, Vec<f32>, u32)>,
    /// Les voix nouvelles (des inconnus entendus plusieurs fois) : centre, nombre de passages.
    pub nouvelles: Vec<(Vec<f32>, u32)>,
    /// Les voix d'avant qui disparaissent (ni nommées, ni à Julien, sans passage).
    pub effacees: Vec<i64>,
}

/// Retrie tous les passages :
/// 1. la voix de Julien repart de son cœur (ses passages très proches, ce qui laisse de côté ceux
///    d'Camille qu'elle avait absorbés), les voix nommées de la moyenne de leurs passages ;
/// 2. chaque passage va à la plus proche comme en direct (deux tours : les voix nommées
///    s'affinent entre les deux) ; une vidéo n'a pas de voix, sauf si Julien l'avait nommée ;
/// 3. ceux qui ne ressemblent à personne sont regroupés entre eux : un groupe d'au moins deux
///    passages devient une voix (qui garde son identifiant d'avant si c'est surtout elle), un
///    passage seul reste « ? ».
pub fn retrier(passages: &[PassageATrier], voix: &[VoixAvant]) -> Tri {
    let nommees: Vec<&VoixAvant> = voix.iter().filter(|v| v.nommee && !v.moi).collect();
    let moi = voix.iter().find(|v| v.moi);
    // 1. Les références.
    let mut references: Vec<(i64, Vec<f32>, bool)> = Vec::new();
    if let Some(moi) = moi {
        let mut centre = moi.centre.clone();
        for _ in 0..3 {
            let coeur = passages.iter().filter(|p| {
                p.voix == Some(moi.id) && p.origine == Origine::Micro && cosinus(&p.empreinte, &centre) >= COEUR_MOI
            });
            match moyenne(coeur.map(|p| p.empreinte.as_slice())) {
                Some(c) => centre = c,
                None => break,
            }
        }
        references.push((moi.id, centre, true));
    }
    for v in &nommees {
        let centre = moyenne(
            passages
                .iter()
                .filter(|p| p.voix == Some(v.id))
                .map(|p| p.empreinte.as_slice()),
        )
        .unwrap_or_else(|| v.centre.clone());
        references.push((v.id, centre, false));
    }
    // 2. Chaque passage, vers la référence la plus proche (deux tours).
    let mut places: Vec<Option<Place>> = vec![None; passages.len()];
    for tour in 0..2 {
        for (rang, p) in passages.iter().enumerate() {
            places[rang] = match p.origine {
                // Une vidéo nommée par Julien (une voix qu'il connaît) garde son nom.
                Origine::Video => Some(match p.voix {
                    Some(id) if nommees.iter().any(|v| v.id == id) => Place::Gardee(id),
                    _ => Place::Video,
                }),
                origine => {
                    let candidates = references
                        .iter()
                        .filter(|(_, _, est_moi)| !(origine == Origine::Appel && *est_moi))
                        .map(|(id, c, _)| (*id, c.as_slice()));
                    let (seuil, marge, inconnue) = seuils(origine);
                    match plus_proche(candidates, &p.empreinte) {
                        Some((id, s, avance)) if s >= seuil && avance >= marge => Some(Place::Gardee(id)),
                        Some((_, s, _)) if s >= inconnue => Some(Place::Incertaine),
                        // Personne de proche : à regrouper avec les autres inconnus.
                        _ => None,
                    }
                }
            };
        }
        if tour == 0 {
            // Les voix nommées repartent de tous leurs passages (Camille, retrouvé aussi dans
            // la voix de Julien et dans de petites voix anonymes).
            for (id, centre, _) in references.iter_mut().filter(|(_, _, est_moi)| !*est_moi) {
                let membres = passages
                    .iter()
                    .zip(&places)
                    .filter(|(_, place)| **place == Some(Place::Gardee(*id)))
                    .map(|(p, _)| p.empreinte.as_slice());
                if let Some(c) = moyenne(membres) {
                    *centre = c;
                }
            }
        }
    }
    // 3. Les inconnus, regroupés entre eux dans l'ordre du temps.
    let mut groupes: Vec<(Vec<f32>, Vec<usize>)> = Vec::new();
    for (rang, p) in passages.iter().enumerate().filter(|(rang, _)| places[*rang].is_none()) {
        let mut centres_normes: Vec<(i64, Vec<f32>)> = Vec::with_capacity(groupes.len());
        for (k, (somme, _)) in groupes.iter().enumerate() {
            let mut c = somme.clone();
            normaliser(&mut c);
            centres_normes.push((k as i64, c));
        }
        let proche = plus_proche(centres_normes.iter().map(|(k, c)| (*k, c.as_slice())), &p.empreinte);
        match proche {
            Some((k, s, avance)) if s >= SEUIL_MEME_VOIX && avance >= MARGE => {
                let (somme, membres) = &mut groupes[k as usize];
                somme.iter_mut().zip(&p.empreinte).for_each(|(s, x)| *s += x);
                membres.push(rang);
            }
            _ => groupes.push((p.empreinte.clone(), vec![rang])),
        }
    }
    let mut tri = Tri::default();
    let mut reprises: Vec<i64> = Vec::new();
    for (somme, membres) in groupes {
        if membres.len() < 2 {
            for rang in membres {
                places[rang] = Some(Place::Incertaine);
            }
            continue;
        }
        let mut centre = somme;
        normaliser(&mut centre);
        // L'identifiant d'avant de la plupart de ses passages (une voix anonyme), s'il est libre.
        let mut comptes: Vec<(i64, usize)> = Vec::new();
        for &rang in &membres {
            if let Some(id) = passages[rang].voix
                && !voix.iter().any(|v| v.id == id && (v.moi || v.nommee))
            {
                match comptes.iter_mut().find(|(i, _)| *i == id) {
                    Some((_, n)) => *n += 1,
                    None => comptes.push((id, 1)),
                }
            }
        }
        let reprise = comptes
            .into_iter()
            .filter(|(id, n)| *n * 2 >= membres.len() && !reprises.contains(id))
            .max_by_key(|(_, n)| *n)
            .map(|(id, _)| id);
        let place = match reprise {
            Some(id) => {
                reprises.push(id);
                tri.gardees.push((id, centre, membres.len() as u32));
                Place::Gardee(id)
            }
            None => {
                tri.nouvelles.push((centre, membres.len() as u32));
                Place::Nouvelle(tri.nouvelles.len() - 1)
            }
        };
        for rang in membres {
            places[rang] = Some(place);
        }
    }
    // Les références gardées (Julien et les voix nommées), même sans passage.
    for (id, centre, _) in &references {
        let n = places.iter().filter(|p| **p == Some(Place::Gardee(*id))).count() as u32;
        tri.gardees.push((*id, centre.clone(), n));
    }
    tri.places = passages
        .iter()
        .zip(places)
        .map(|(p, place)| (p.id, place.unwrap_or(Place::Incertaine)))
        .collect();
    let gardees: Vec<i64> = tri.gardees.iter().map(|(id, _, _)| *id).collect();
    tri.effacees = voix
        .iter()
        .filter(|v| !gardees.contains(&v.id) && !v.nommee && !v.moi)
        .map(|v| v.id)
        .collect();
    tri
}

/// Empreinte ↔ octets (f32 petit-boutiste), pour la base.
pub fn en_octets(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn depuis_octets(octets: &[u8]) -> Vec<f32> {
    octets
        .chunks_exact(4)
        .map(|o| f32::from_le_bytes([o[0], o[1], o[2], o[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axe(i: usize, bruit: f32) -> Vec<f32> {
        let mut v = vec![bruit; DIMENSION];
        v[i] = 1.0;
        normaliser(&mut v);
        v
    }

    /// Un mélange normalisé de deux axes (une voix « entre deux »).
    fn entre(i: usize, j: usize, part_j: f32) -> Vec<f32> {
        let mut v = vec![0.0; DIMENSION];
        v[i] = 1.0 - part_j;
        v[j] = part_j;
        normaliser(&mut v);
        v
    }

    fn connue(id: i64, i: usize, passages: u32, moi: bool) -> VoixConnue {
        VoixConnue {
            id,
            centre: axe(i, 0.0),
            passages,
            moi,
        }
    }

    #[test]
    fn micro_d_abord_moi_puis_le_plus_proche() {
        assert_eq!(attribuer(&[], &axe(0, 0.0), Origine::Micro), Attribution::NouvelleMoi);
        // Peu connue : le micro, c'est Julien, même si la voix diffère.
        let debut = [connue(1, 0, 3, true)];
        assert!(matches!(attribuer(&debut, &axe(5, 0.0), Origine::Micro), Attribution::Connue { id: 1, .. }));
        // Bien connue : une voix très différente au micro n'est pas lui.
        let voix = [connue(1, 0, 50, true), connue(7, 3, 30, false)];
        assert_eq!(attribuer(&voix, &axe(5, 0.0), Origine::Micro), Attribution::Nouvelle);
        // Camille au téléphone, en haut-parleur : c'est lui, pas Julien.
        assert!(matches!(
            attribuer(&voix, &axe(3, 0.02), Origine::Micro),
            Attribution::Connue { id: 7, apprendre: true, .. }
        ));
        // Entre les deux : « ? », plutôt que Julien d'office.
        assert_eq!(attribuer(&voix, &entre(0, 3, 0.5), Origine::Micro), Attribution::Incertaine);
        // Plutôt Julien sans être sûr : rattaché, mais son centre n'en apprend rien.
        assert!(matches!(
            attribuer(&voix, &entre(0, 3, 0.45), Origine::Micro),
            Attribution::Connue { id: 1, apprendre: false, .. }
        ));
    }

    #[test]
    fn son_du_mac_appel_ou_video() {
        let voix = [connue(1, 0, 50, true), connue(7, 3, 10, false), connue(8, 9, 4, false)];
        // Une vidéo : pas de voix, même si elle ressemble à quelqu'un.
        assert_eq!(attribuer(&voix, &axe(3, 0.03), Origine::Video), Attribution::Video);
        // Un appel : la voix connue la plus proche, jamais Julien.
        assert!(matches!(attribuer(&voix, &axe(3, 0.03), Origine::Appel), Attribution::Connue { id: 7, .. }));
        assert_eq!(attribuer(&voix, &axe(0, 0.0), Origine::Appel), Attribution::Nouvelle);
        assert_eq!(attribuer(&voix, &axe(42, 0.0), Origine::Appel), Attribution::Nouvelle);
    }

    #[test]
    fn retri_rend_camille_et_les_videos() {
        let passage = |id: i64, origine: Origine, empreinte: Vec<f32>, voix: Option<i64>| PassageATrier {
            id,
            origine,
            empreinte,
            voix,
        };
        let mut passages = Vec::new();
        // Julien au micro (rangé chez lui), et Camille au téléphone, absorbé par sa voix.
        for id in 0..10 {
            passages.push(passage(id, Origine::Micro, axe(0, 0.01), Some(1)));
        }
        for id in 10..13 {
            passages.push(passage(id, Origine::Micro, axe(3, 0.01), Some(1)));
        }
        // Camille aussi dans une petite voix anonyme, et deux vidéos avec leur voix.
        passages.push(passage(13, Origine::Micro, axe(3, 0.02), Some(5)));
        passages.push(passage(14, Origine::Video, axe(9, 0.0), Some(6)));
        passages.push(passage(15, Origine::Video, axe(9, 0.0), Some(6)));
        // Un inconnu entendu deux fois au micro, un autre une seule fois, un passage entre deux.
        passages.push(passage(16, Origine::Micro, axe(20, 0.0), Some(8)));
        passages.push(passage(17, Origine::Micro, axe(20, 0.01), Some(8)));
        passages.push(passage(18, Origine::Micro, axe(30, 0.0), Some(9)));
        passages.push(passage(19, Origine::Micro, entre(0, 3, 0.5), Some(1)));
        // La voix de Julien, contaminée par Camille ; Camille nommé (l'ancienne voix 4).
        let voix = [
            VoixAvant { id: 1, centre: entre(0, 3, 0.2), nommee: true, moi: true },
            VoixAvant { id: 4, centre: axe(3, 0.0), nommee: true, moi: false },
            VoixAvant { id: 5, centre: axe(3, 0.02), nommee: false, moi: false },
            VoixAvant { id: 6, centre: axe(9, 0.0), nommee: false, moi: false },
            VoixAvant { id: 8, centre: axe(20, 0.0), nommee: false, moi: false },
            VoixAvant { id: 9, centre: axe(30, 0.0), nommee: false, moi: false },
        ];
        let tri = retrier(&passages, &voix);
        let place = |id: i64| tri.places.iter().find(|(p, _)| *p == id).unwrap().1;
        assert!((0..10).all(|id| place(id) == Place::Gardee(1)));
        // Camille récupère ses passages, ceux de Julien comme ceux de la petite voix.
        assert!((10..14).all(|id| place(id) == Place::Gardee(4)), "{:?}", tri.places);
        assert_eq!((place(14), place(15)), (Place::Video, Place::Video));
        // L'inconnu entendu deux fois garde sa voix ; celui entendu une fois, et le passage entre deux : « ? ».
        assert_eq!((place(16), place(17)), (Place::Gardee(8), Place::Gardee(8)));
        assert_eq!((place(18), place(19)), (Place::Incertaine, Place::Incertaine));
        // Le centre de Julien ne ressemble plus à Camille.
        let centre_moi = &tri.gardees.iter().find(|(id, _, _)| *id == 1).unwrap().1;
        assert!(cosinus(centre_moi, &axe(3, 0.0)) < 0.1);
        assert_eq!(tri.gardees.iter().find(|(id, _, _)| *id == 4).unwrap().2, 4);
        let mut effacees = tri.effacees.clone();
        effacees.sort();
        assert_eq!(effacees, vec![5, 6, 9]);
        assert!(tri.nouvelles.is_empty());
    }

    #[test]
    fn centre_et_octets() {
        let c = nouveau_centre(&axe(0, 0.0), 1, &axe(1, 0.0));
        assert!((c[0] - c[1]).abs() < 1e-6 && (cosinus(&c, &c) - 1.0).abs() < 1e-5);
        let v = axe(2, 0.1);
        assert_eq!(depuis_octets(&en_octets(&v)), v);
    }
}
