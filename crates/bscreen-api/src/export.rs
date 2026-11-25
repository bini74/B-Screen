//! Export d'une journée en JSON Lines (phase 9.3), au format « moment » du plan (section 4) :
//! une ligne par moment — `ecran` (une plage où la même app, la même fenêtre et la même page
//! restent affichées, avec le texte vu), `voix` (un passage, qui parlait, le texte, les mots
//! avec leur heure), `recherche` (une recherche tapée). Les moments privés sont signalés sans
//! rien de leur contenu ; le texte est celui déjà masqué.

use std::io::Write;

use anyhow::Result;
use bscreen_store::{Base, Image};
use chrono::{Local, NaiveDate, NaiveTime, SecondsFormat, TimeZone};
use serde::Serialize;
use serde_json::json;

/// Deux images d'une même plage ne sont jamais séparées de plus de 5 minutes.
const TROU_MAX_MS: i64 = 300_000;

fn horodatage(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, false))
        .unwrap_or_default()
}

#[derive(Serialize)]
struct TexteExporte<'a> {
    texte: &'a str,
    source: &'static str,
    cadre: [f64; 4],
    vu: bool,
    focus: bool,
}

/// La journée `jour` (heure locale), écrite ligne à ligne dans `sortie`. Rend le nombre de moments.
pub fn exporter_jour(base: &Base, jour: NaiveDate, sortie: &mut impl Write) -> Result<usize> {
    let debut = Local
        .from_local_datetime(&jour.and_time(NaiveTime::MIN))
        .earliest()
        .map(|d| d.timestamp_millis())
        .unwrap_or(0);
    let fin = Local
        .from_local_datetime(&(jour + chrono::Duration::days(1)).and_time(NaiveTime::MIN))
        .earliest()
        .map(|d| d.timestamp_millis())
        .unwrap_or(i64::MAX)
        - 1;
    // Tous les moments de la journée, puis triés par heure de début.
    let mut moments: Vec<(i64, serde_json::Value)> = Vec::new();

    // Écran : des plages (même écran, même app, même fenêtre, même page, même confidentialité).
    let images = base.images_entre(debut, fin)?;
    let mut plages: Vec<Vec<&Image>> = Vec::new();
    for image in &images {
        let suite = plages.iter_mut().rev().find(|p| p[0].ecran == image.ecran).filter(|p| {
            let d = p[p.len() - 1];
            d.app_bundle == image.app_bundle
                && d.fenetre == image.fenetre
                && d.url == image.url
                && d.privee == image.privee
                && image.quand_ms - d.quand_ms <= TROU_MAX_MS
        });
        match suite {
            Some(plage) => plage.push(image),
            None => plages.push(vec![image]),
        }
    }
    for plage in plages {
        let (premiere, derniere) = (plage[0], plage[plage.len() - 1]);
        let (a, b) = (premiere.quand_ms, derniere.quand_ms);
        let textes = if premiere.privee {
            Vec::new()
        } else {
            base.textes_entre(premiere.ecran, a, b)?
        };
        let textes: Vec<TexteExporte> = textes
            .iter()
            .map(|t| TexteExporte {
                texte: &t.texte,
                source: if t.source == 1 { "ocr" } else { "ax" },
                cadre: t.cadre,
                vu: t.vu,
                focus: t.focus,
            })
            .collect();
        let ecran = if premiere.privee {
            json!({ "ecran": premiere.ecran, "app": premiere.app_nom, "bundle": premiere.app_bundle, "prive": true })
        } else {
            json!({
                "ecran": premiere.ecran,
                "app": premiere.app_nom,
                "bundle": premiere.app_bundle,
                "fenetre": premiere.fenetre,
                "url": premiere.url,
                "prive": false,
                "images": plage.len(),
                "textes": textes,
                "image": { "fichier": premiere.fichier, "position_ms": premiere.position_ms },
            })
        };
        moments.push((
            a,
            json!({
                "id": format!("ec_{}_{}", premiere.ecran, premiere.id),
                "debut": horodatage(a),
                "fin": horodatage(b),
                "type": "ecran",
                "ecran": ecran,
            }),
        ));
    }

    // Voix : un passage par moment (sauf pendant une navigation privée).
    for v in base.voix_entre(debut, fin)? {
        if base.privee_pendant(v.debut_ms, v.fin_ms)? {
            continue;
        }
        let mots: Vec<serde_json::Value> = base
            .mots_voix(v.id)?
            .and_then(|j| serde_json::from_str::<Vec<(i64, i64, String)>>(&j).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(a, b, m)| json!({ "m": m, "debut_ms": a - v.debut_ms, "fin_ms": b - v.debut_ms }))
            .collect();
        moments.push((
            v.debut_ms,
            json!({
                "id": format!("vo_{}", v.id),
                "debut": horodatage(v.debut_ms),
                "fin": horodatage(v.fin_ms),
                "type": "voix",
                "voix": {
                    "source": if v.source == 0 { "micro" } else { "sortie" },
                    "appel": v.appel,
                    "locuteur": v.locuteur_id.map(|id| json!({ "id": format!("voix_{id}"), "nom": v.locuteur })),
                    "texte": v.texte,
                    "mots": mots,
                    "fichier": v.fichier,
                },
            }),
        ));
    }

    // Recherches tapées.
    for r in base.recherches_entre(debut, fin)? {
        moments.push((
            r.quand_ms,
            json!({
                "id": format!("re_{}", r.id),
                "debut": horodatage(r.quand_ms),
                "fin": horodatage(r.quand_ms),
                "type": "recherche",
                "recherche": { "moteur": r.moteur, "requete": r.requete, "url": r.url },
            }),
        ));
    }

    moments.sort_by_key(|(quand, _)| *quand);
    for (_, moment) in &moments {
        serde_json::to_writer(&mut *sortie, moment)?;
        sortie.write_all(b"\n")?;
    }
    Ok(moments.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bscreen_store::{NouveauMorceau, NouvelleApparition, NouvelleImage};

    #[test]
    fn une_journee_en_json_lines() {
        let base = Base::en_memoire().unwrap();
        let midi = Local.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap().timestamp_millis();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "video/a.mp4", debut_ms: midi, largeur: 10, hauteur: 10 })
            .unwrap();
        for (i, (decalage, app, prive)) in [(0i64, "Safari", false), (2_000, "Safari", false), (4_000, "Chrome", true)]
            .iter()
            .enumerate()
        {
            let id = base
                .ajouter_image(&NouvelleImage {
                    morceau: m,
                    quand_ms: midi + decalage,
                    position_ms: i as i64 * 2_000,
                    ecran: 1,
                    app_bundle: Some(app),
                    app_nom: Some(app),
                    aire: 1.0,
                    declenchee: false,
                })
                .unwrap();
            base.noter_fenetre(id, Some("Dauphins"), Some("https://exemple.fr")).unwrap();
            if *prive {
                base.noter_privee(1, midi + decalage - 1, midi + decalage).unwrap();
            }
        }
        let t = base.texte_id("Les dauphins dorment").unwrap();
        base.noter_apparitions(
            &[NouvelleApparition { texte: t, ecran: 1, quand_ms: midi, source: 0, vu: true, focus: false, cadre: [1.0, 2.0, 3.0, 4.0] }],
            &[],
            midi,
        )
        .unwrap();
        let v = base.ajouter_voix(1, midi + 500, midi + 1_500, "audio/x.aac", None, None).unwrap();
        let voix = base.voix_entre(midi, midi + 2_000).unwrap();
        assert_eq!(voix[0].id, v);
        base.noter_transcription(&voix[0], "on parle des dauphins", "[[1500000,1500100,\"on\"]]", "essai").unwrap();
        base.noter_recherche(midi + 1_000, "google.com", "dauphins sommeil", "https://google.com/…").unwrap();

        let mut sortie = Vec::new();
        let n = exporter_jour(&base, NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(), &mut sortie).unwrap();
        let lignes: Vec<serde_json::Value> = String::from_utf8(sortie)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(n, 4, "une plage Safari, la voix, la recherche, la plage privée");
        let types: Vec<&str> = lignes.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["ecran", "voix", "recherche", "ecran"]);
        assert_eq!(lignes[0]["ecran"]["images"], 2);
        assert_eq!(lignes[0]["ecran"]["textes"][0]["texte"], "Les dauphins dorment");
        assert_eq!(lignes[1]["voix"]["source"], "sortie");
        assert_eq!(lignes[1]["voix"]["texte"], "on parle des dauphins");
        assert_eq!(lignes[3]["ecran"]["prive"], true);
        assert!(lignes[3]["ecran"].get("fenetre").is_none(), "rien d'un moment privé");
        assert!(lignes[0]["debut"].as_str().unwrap().starts_with("2026-11-19T12:00:00.000"));
    }
}
