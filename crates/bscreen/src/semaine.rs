//! `bscreen mesures [jours]` : le bilan de la semaine de mesures (phase 11.3), d'après les relevés
//! de 10 minutes (`~/Library/Logs/B-Screen/mesures.jsonl`) et la place prise jour par jour (la
//! base), face aux objectifs du plan.

use std::collections::BTreeMap;

use anyhow::Result;
use bscreen_core::chemins;
use bscreen_macos::mesures::{Geste, Releve};

/// « 1,4 » (une décimale, virgule française).
fn virgule(x: f64, decimales: usize) -> String {
    format!("{x:.decimales$}").replace('.', ",")
}

/// Le processeur moyen pris par B-Screen (en % d'un cœur) sur ces relevés.
fn processeur(releves: &[&Releve]) -> Option<f64> {
    let duree: f64 = releves.iter().map(|r| r.duree_s).sum();
    (duree > 0.0).then(|| releves.iter().map(|r| r.cpu_s).sum::<f64>() / duree * 100.0)
}

/// Le rang `q` (0 à 1) d'une liste triée.
fn quantile(triee: &[u32], q: f64) -> u32 {
    triee[((triee.len() - 1) as f64 * q).round() as usize]
}

/// Une ligne de verdict : ✓ si l'objectif est tenu.
fn verdict(nom: &str, mesure: String, tenu: Option<bool>, objectif: &str) {
    let marque = match tenu {
        Some(true) => "✓",
        Some(false) => "✗",
        None => "·",
    };
    println!("  {marque} {nom:<38} {mesure:<26} objectif : {objectif}");
}

pub fn bilan(jours: u32) -> Result<()> {
    let tous = bscreen_macos::mesures::releves();
    let depuis = chrono::Local::now().timestamp_millis() - i64::from(jours) * 86_400_000;
    let releves: Vec<&Releve> = tous.iter().filter(|r| r.quand_ms >= depuis).collect();
    if releves.is_empty() {
        println!(
            "Pas encore de relevé sur les {jours} derniers jours : B-Screen en écrit un toutes les 10 minutes \
             dans ~/Library/Logs/B-Screen/mesures.jsonl (depuis le 22/11/2026)."
        );
        return Ok(());
    }
    let heures: f64 = releves.iter().map(|r| r.duree_s).sum::<f64>() / 3600.0;
    let premier = bscreen_api::heures::lisible(releves[0].quand_ms - (releves[0].duree_s * 1000.0) as i64);
    let dernier = bscreen_api::heures::lisible(releves[releves.len() - 1].quand_ms);
    println!("Semaine de mesures : {} relevés, {} h, du {premier} au {dernier}\n", releves.len(), virgule(heures, 1));

    // Processeur : en moyenne, Mac au repos (personne ne s'en sert : la capture seule, ou presque),
    // pendant qu'on s'en sert, pendant un appel.
    let au_repos: Vec<&Releve> = releves.iter().copied().filter(|r| r.actives == 0 && r.en_appel == 0).collect();
    let actifs: Vec<&Releve> = releves.iter().copied().filter(|r| r.actives * 2 >= r.minutes.max(1) && r.en_appel == 0).collect();
    let appels: Vec<&Releve> = releves.iter().copied().filter(|r| r.en_appel * 2 >= r.minutes.max(1) && r.minutes > 0).collect();
    let pct = |r: Option<f64>| r.map_or("—".to_string(), |p| format!("{} %", virgule(p, 2)));
    println!("Processeur (en % d'un cœur)");
    verdict("en moyenne", pct(processeur(&releves)), None, "—");
    verdict(&format!("Mac au repos ({} relevés)", au_repos.len()), pct(processeur(&au_repos)), processeur(&au_repos).map(|p| p < 1.0), "< 1 % (la capture)");
    verdict(&format!("pendant qu'on s'en sert ({})", actifs.len()), pct(processeur(&actifs)), None, "—");
    verdict(&format!("pendant un appel ({})", appels.len()), pct(processeur(&appels)), processeur(&appels).map(|p| p < 15.0), "< 15 %");

    // Énergie et mémoire.
    let duree: f64 = releves.iter().map(|r| r.duree_s).sum();
    let watts = releves.iter().map(|r| r.energie_j).sum::<f64>() / duree.max(1.0);
    let mut memoires: Vec<f64> = releves.iter().map(|r| r.memoire_mo).collect();
    memoires.sort_by(f64::total_cmp);
    let mediane = memoires[memoires.len() / 2];
    let pointe = releves.iter().map(|r| r.memoire_max_mo).fold(0.0, f64::max);
    println!("\nÉnergie et mémoire");
    verdict("énergie comptée à B-Screen", format!("{} W en moyenne", virgule(watts, 2)), None, "—");
    verdict("mémoire (médiane / pointe)", format!("{} / {} Mo", virgule(mediane, 0), virgule(pointe, 0)), None, "—");
    let ecrit_par_jour = releves.iter().map(|r| r.ecrit_mo).sum::<f64>() / (heures / 24.0).max(1.0 / 24.0);
    verdict("écrit sur le disque", format!("{} Go par jour", virgule(ecrit_par_jour / 1024.0, 2)), None, "—");

    // Place prise : vidéo et voix, jour par jour (la base).
    println!("\nPlace sur le disque");
    match bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite")).and_then(|b| b.bilan_par_jour(depuis)) {
        Ok(par_jour) if !par_jour.is_empty() => {
            // La voix n'est pas comptée jour par jour : sa place totale, répartie sur les jours.
            let voix = releves.last().map_or(0.0, |r| r.voix_mo) / par_jour.len().max(1) as f64;
            let mo_par_jour: Vec<f64> = par_jour.iter().map(|j| j.octets_video as f64 / 1_048_576.0 + voix).collect();
            let moyenne = mo_par_jour.iter().sum::<f64>() / mo_par_jour.len() as f64;
            let par_mois = moyenne * 30.0 / 1024.0;
            verdict(
                &format!("vidéo et voix ({} jours)", par_jour.len()),
                format!("{} Mo par jour → {} Go par mois", virgule(moyenne, 0), virgule(par_mois, 1)),
                Some(par_mois < 10.0),
                "< 10 Go par mois",
            );
        }
        Ok(_) => println!("  · rien d'enregistré sur la période"),
        Err(e) => println!("  · base illisible : {e}"),
    }

    // Les gestes chronométrés.
    let mut temps: BTreeMap<Geste, Vec<u32>> = BTreeMap::new();
    for r in &releves {
        for (geste, liste) in &r.temps {
            temps.entry(*geste).or_default().extend(liste);
        }
    }
    println!("\nTemps des gestes (médiane, 95 % des cas, pire)");
    for (geste, nom, objectif, seuil) in [
        (Geste::LectureImage, "relire une image (frise)", "< 50 ms", Some(50)),
        (Geste::Recherche, "chercher (frise)", "< 100 ms", Some(100)),
        (Geste::ContexteAsk, "préparer le contexte d'Ask", "—", None),
    ] {
        match temps.get_mut(&geste).filter(|l| !l.is_empty()) {
            Some(liste) => {
                liste.sort_unstable();
                let (p50, p95, pire) = (quantile(liste, 0.5), quantile(liste, 0.95), liste[liste.len() - 1]);
                verdict(
                    &format!("{nom} ({} fois)", liste.len()),
                    format!("{p50} ms, {p95} ms, {pire} ms"),
                    seuil.map(|s| p50 < s),
                    objectif,
                );
            }
            None => verdict(nom, "pas encore mesuré".to_string(), None, objectif),
        }
    }
    println!("\nRelevés : ~/Library/Logs/B-Screen/mesures.jsonl (seulement des chiffres).");
    Ok(())
}
