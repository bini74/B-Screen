//! `bscreen retrier-voix` : retrie toutes les voix du passé à partir des empreintes gardées
//! (voir `bscreen_core::voix_familieres::retrier`). À blanc, rien n'est écrit : seulement le
//! compte de ce qui changerait. Sinon, l'état d'avant est gardé dans la base (`--annuler`).

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use bscreen_core::voix_familieres::{self as vf, Origine, PassageATrier, Place, VoixAvant, depuis_octets, en_octets};
use bscreen_store::Base;

/// Des voix à réunir sous un nom : « 3,743,744=Camille » (la première garde son identifiant).
pub fn lire_nommage(texte: &str) -> Result<(Vec<i64>, String)> {
    let Some((ids, nom)) = texte.split_once('=') else {
        bail!("« {texte} » : il faut identifiants=nom, par exemple 3,743=Camille");
    };
    let ids: Vec<i64> = ids
        .split(',')
        .map(|i| i.trim().parse::<i64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| anyhow::anyhow!("« {ids} » : des identifiants de voix séparés par des virgules"))?;
    let nom = nom.trim().to_string();
    if ids.is_empty() || nom.is_empty() {
        bail!("« {texte} » : identifiants ou nom vide");
    }
    Ok((ids, nom))
}

/// Retrie les voix ; `a_blanc` : rien n'est écrit. Rend le compte rendu.
pub fn retrier(base: &Base, a_blanc: bool, nommer: &[(Vec<i64>, String)]) -> Result<String> {
    let locuteurs = base.locuteurs()?;
    let empreintes = base.empreintes_de_voix()?;
    // Les voix réunies sous un nom : chacune → celle qui reste.
    let mut reunies: HashMap<i64, i64> = HashMap::new();
    let mut noms: Vec<(i64, String)> = Vec::new();
    for (ids, nom) in nommer {
        for id in ids {
            if !locuteurs.iter().any(|l| l.id == *id) {
                bail!("voix {id} inconnue");
            }
            reunies.insert(*id, ids[0]);
        }
        noms.push((ids[0], nom.clone()));
    }
    let nom_de = |id: i64| -> String {
        if let Some((_, nom)) = noms.iter().find(|(i, _)| *i == id) {
            return nom.clone();
        }
        match locuteurs.iter().find(|l| l.id == id) {
            Some(l) if l.moi => "Toi".to_string(),
            Some(l) => l.nom.clone().unwrap_or_else(|| format!("Voix {id}")),
            None => format!("Voix {id}"),
        }
    };
    let voix: Vec<VoixAvant> = locuteurs
        .iter()
        .filter(|l| reunies.get(&l.id).is_none_or(|garde| *garde == l.id))
        .map(|l| VoixAvant {
            id: l.id,
            centre: depuis_octets(&l.centre),
            nommee: l.nom.is_some() || noms.iter().any(|(i, _)| *i == l.id),
            moi: l.moi,
        })
        .collect();
    let passages: Vec<PassageATrier> = empreintes
        .iter()
        .map(|e| PassageATrier {
            id: e.id,
            origine: match (e.source, e.appel) {
                (0, _) => Origine::Micro,
                (_, true) => Origine::Appel,
                _ => Origine::Video,
            },
            empreinte: depuis_octets(&e.empreinte),
            voix: e.locuteur_id.map(|id| *reunies.get(&id).unwrap_or(&id)),
        })
        .collect();
    let tri = vf::retrier(&passages, &voix);

    // Le compte rendu : d'où vient chaque passage, où il va.
    let avant = |p: &PassageATrier| -> String {
        let source = if p.origine == Origine::Micro { "micro" } else { "son du Mac" };
        match p.voix {
            Some(id) if voix.iter().any(|v| v.id == id && (v.moi || v.nommee)) => format!("{} ({source})", nom_de(id)),
            Some(_) => format!("petites voix ({source})"),
            None => format!("sans voix ({source})"),
        }
    };
    let apres = |place: Place| -> String {
        match place {
            Place::Gardee(id) if voix.iter().any(|v| v.id == id && (v.moi || v.nommee)) => nom_de(id),
            Place::Gardee(_) => "voix anonyme gardée".to_string(),
            Place::Nouvelle(_) => "voix anonyme nouvelle".to_string(),
            Place::Incertaine => "?".to_string(),
            Place::Video => "vidéo".to_string(),
        }
    };
    let mut mouvements: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (p, (_, place)) in passages.iter().zip(&tri.places) {
        *mouvements.entry((avant(p), apres(*place))).or_default() += 1;
    }
    let mut lignes = vec![format!(
        "{} passages avec empreinte ({} au micro, {} au son du Mac).",
        passages.len(),
        passages.iter().filter(|p| p.origine == Origine::Micro).count(),
        passages.iter().filter(|p| p.origine != Origine::Micro).count()
    )];
    lignes.push(String::new());
    lignes.push("Avant → après :".to_string());
    for ((de, vers), n) in &mouvements {
        lignes.push(format!("  {n:>5}  {de} → {vers}"));
    }
    let anonymes = tri
        .gardees
        .iter()
        .filter(|(id, _, _)| !voix.iter().any(|v| v.id == *id && (v.moi || v.nommee)))
        .count()
        + tri.nouvelles.len();
    lignes.push(String::new());
    lignes.push(format!(
        "Voix : {} avant → {} après ({} nommées ou à toi, {anonymes} anonymes entendues au moins 2 fois) ; {} effacées.",
        locuteurs.len(),
        tri.gardees.len() + tri.nouvelles.len(),
        tri.gardees.len() + tri.nouvelles.len() - anonymes,
        tri.effacees.len() + reunies.iter().filter(|(a, garde)| a != garde).count()
    ));
    for (id, _, n) in tri.gardees.iter().filter(|(id, _, _)| voix.iter().any(|v| v.id == *id && (v.moi || v.nommee))) {
        let avant = passages.iter().filter(|p| p.voix == Some(*id)).count();
        lignes.push(format!("  {} : {avant} → {n} passages", nom_de(*id)));
    }
    if a_blanc {
        lignes.push(String::new());
        lignes.push("À blanc : rien n'a été écrit.".to_string());
        return Ok(lignes.join("\n"));
    }
    let places: Vec<(i64, Option<i64>)> = tri
        .places
        .iter()
        .map(|(id, place)| {
            let voix = match place {
                Place::Gardee(v) => Some(*v),
                Place::Nouvelle(k) => Some(-(*k as i64) - 1),
                Place::Incertaine | Place::Video => None,
            };
            (*id, voix)
        })
        .collect();
    let gardees: Vec<(i64, Vec<u8>, u32)> = tri.gardees.iter().map(|(id, c, n)| (*id, en_octets(c), *n)).collect();
    let nouvelles: Vec<(Vec<u8>, u32)> = tri.nouvelles.iter().map(|(c, n)| (en_octets(c), *n)).collect();
    let mut effacees = tri.effacees.clone();
    effacees.extend(reunies.iter().filter(|(a, garde)| a != garde).map(|(a, _)| *a));
    base.appliquer_tri_des_voix(&noms, &places, &gardees, &nouvelles, &effacees)?;
    lignes.push(String::new());
    lignes.push("Écrit. L'état d'avant est gardé : `bscreen retrier-voix --annuler` le remet.".to_string());
    Ok(lignes.join("\n"))
}
