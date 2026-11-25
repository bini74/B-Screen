//! La réponse d'Ask, lue ligne par ligne : puces, passages en gras (`**…**`) et renvois aux
//! moments (`[écran 20/11 21:56]`, `[audio 20/11 10:12]`), pour l'afficher et rendre les renvois
//! cliquables (ils ouvrent la frise à ce moment).

use std::ops::Range;

/// Un renvoi à un moment, tel qu'écrit dans la réponse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renvoi {
    /// Entendu (voix) plutôt que vu à l'écran.
    pub audio: bool,
    pub jour: u32,
    pub mois: u32,
    pub heure: u32,
    pub minute: u32,
}

/// Une ligne de la réponse, prête à afficher.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ligne {
    /// Une puce (« - » ou « • » en tête), retirée du texte.
    pub puce: bool,
    pub texte: String,
    /// Passages en gras (octets du texte affiché).
    pub gras: Vec<Range<usize>>,
    /// Renvois, avec leur place dans le texte affiché (« écran 20/11 21:56 », sans crochets).
    pub renvois: Vec<(Range<usize>, Renvoi)>,
}

/// Découpe la réponse en lignes (les lignes vides séparent les paragraphes et sont gardées vides).
pub fn analyser(reponse: &str) -> Vec<Ligne> {
    reponse.lines().map(analyser_ligne).collect()
}

fn analyser_ligne(brute: &str) -> Ligne {
    let mut reste = brute.trim_end();
    let mut ligne = Ligne::default();
    let sans_retrait = reste.trim_start();
    for puce in ["- ", "• ", "* "] {
        if let Some(apres) = sans_retrait.strip_prefix(puce) {
            ligne.puce = true;
            reste = apres;
            break;
        }
    }
    if !ligne.puce {
        reste = sans_retrait;
    }
    // Les titres en markdown (« ## … ») deviennent du gras.
    let titre = reste.starts_with('#');
    if titre {
        reste = reste.trim_start_matches('#').trim_start();
    }

    let mut en_gras: Option<usize> = None;
    let mut i = 0;
    while i < reste.len() {
        let suite = &reste[i..];
        if suite.starts_with("**") {
            match en_gras.take() {
                Some(debut) => ligne.gras.push(debut..ligne.texte.len()),
                None => en_gras = Some(ligne.texte.len()),
            }
            i += 2;
            continue;
        }
        if suite.starts_with('[')
            && let Some(fin) = suite.find(']')
            && let Some(renvoi) = lire_renvoi(&suite[1..fin])
        {
            let debut = ligne.texte.len();
            ligne.texte.push_str(&suite[1..fin]);
            ligne.renvois.push((debut..ligne.texte.len(), renvoi));
            i += fin + 1;
            continue;
        }
        let c = suite.chars().next().unwrap_or(' ');
        // Espaces insécables à la française : « » ; : ? ! ne restent jamais seuls en bout de ligne.
        let apres = suite[c.len_utf8()..].chars().next();
        if c == ' ' && (ligne.texte.ends_with('«') || matches!(apres, Some('»' | ';' | ':' | '?' | '!'))) {
            ligne.texte.push('\u{a0}');
        } else {
            ligne.texte.push(c);
        }
        i += c.len_utf8();
    }
    // Un gras jamais fermé : jusqu'au bout.
    if let Some(debut) = en_gras {
        ligne.gras.push(debut..ligne.texte.len());
    }
    if titre && !ligne.texte.is_empty() {
        ligne.gras.clear();
        ligne.gras.push(0..ligne.texte.len());
    }
    ligne.gras.retain(|r| !r.is_empty());
    ligne
}

/// « écran 20/11 21:56 », « audio 20/11 10:12:30 », « écran 21:56 » (sans date : aujourd'hui → 0/0),
/// et les plages : « écran 20/11 21:59–22:06 », « écran 21/11 01:18 à 01:22 » (le début compte).
fn lire_renvoi(dedans: &str) -> Option<Renvoi> {
    // Une plage : seul le début compte (« 21:59–22:06 », « 21:59→22:06 », « 01:18 à 01:22 »,
    // « 21:59 puis 22:53 »).
    let dedans = dedans.split(['–', '—', '→']).next()?;
    let dedans = dedans.split(" à ").next()?;
    let dedans = dedans.split(" puis ").next()?;
    let dedans = match dedans.rfind('-') {
        Some(i) if dedans[i + 1..].contains(':') => &dedans[..i],
        _ => dedans,
    };
    let mut morceaux = dedans.split_whitespace();
    let audio = match morceaux.next()?.to_lowercase().as_str() {
        "écran" | "ecran" | "recherche" | "recherches" => false,
        "audio" | "voix" => true,
        _ => return None,
    };
    let premier = morceaux.next()?;
    let (date, heure) = match morceaux.next() {
        Some(h) => (Some(premier), h),
        None => (None, premier),
    };
    if morceaux.next().is_some() {
        return None;
    }
    let (jour, mois) = match date {
        Some(d) => {
            let (j, m) = d.split_once('/')?;
            (j.parse().ok()?, m.parse().ok()?)
        }
        None => (0, 0),
    };
    let mut hms = heure.split(':');
    let h: u32 = hms.next()?.parse().ok()?;
    let m: u32 = hms.next()?.parse().ok()?;
    if h > 23 || m > 59 || jour > 31 || mois > 12 {
        return None;
    }
    Some(Renvoi {
        audio,
        jour,
        mois,
        heure: h,
        minute: m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puces_gras_et_renvois() {
        let lignes = analyser(
            "Ce soir sur YouTube :\n\n- **« Panique en Corée »**, de 21:56 à 21:59 [écran 20/11 21:56].\n- Luis Enrique [audio 20/11 21:59:12] puis [écran 21:59]",
        );
        assert_eq!(lignes.len(), 4);
        assert!(!lignes[0].puce && lignes[1].texte.is_empty());
        let l = &lignes[2];
        assert!(l.puce);
        assert_eq!(l.texte.replace('\u{a0}', " "), "« Panique en Corée », de 21:56 à 21:59 écran 20/11 21:56.");
        assert!(l.texte.starts_with("«\u{a0}Panique"), "« reste collé au mot qui suit");
        assert_eq!(l.texte[l.gras[0].clone()].replace('\u{a0}', " "), "« Panique en Corée »");
        let (place, renvoi) = &l.renvois[0];
        assert_eq!(&l.texte[place.clone()], "écran 20/11 21:56");
        assert_eq!(
            *renvoi,
            Renvoi {
                audio: false,
                jour: 20,
                mois: 11,
                heure: 21,
                minute: 56
            }
        );
        let l = &lignes[3];
        assert_eq!(l.renvois.len(), 2);
        assert!(l.renvois[0].1.audio);
        assert_eq!((l.renvois[1].1.jour, l.renvois[1].1.heure), (0, 21));
    }

    #[test]
    fn renvois_en_plage() {
        for texte in [
            "[écran 20/11 21:59–22:06]",
            "[écran 21/11 01:18 à 01:22]",
            "[recherches 21/11 01:14-01:23]",
            "[écran 20/11 21:59→22:06]",
            "[écran 20/11 21:59 puis 22:53→23:07]",
        ] {
            let l = &analyser(texte)[0];
            assert_eq!(l.renvois.len(), 1, "{texte}");
            assert_eq!(l.texte.replace('\u{a0}', " "), texte.trim_matches(['[', ']']), "le texte affiché reste entier");
        }
        let l = &analyser("[écran 20/11 21:59–22:06]")[0];
        assert_eq!((l.renvois[0].1.heure, l.renvois[0].1.minute), (21, 59));
    }

    #[test]
    fn crochets_ordinaires_gardes() {
        let l = &analyser("Voir [la doc] et [écran 99/11 21:56]")[0];
        assert_eq!(l.texte.replace('\u{a0}', " "), "Voir [la doc] et [écran 99/11 21:56]");
        assert!(l.renvois.is_empty());
    }

    #[test]
    fn titres_en_gras() {
        let l = &analyser("## Résumé")[0];
        assert_eq!(l.texte, "Résumé");
        assert_eq!(l.gras, vec![0..l.texte.len()]);
    }
}
