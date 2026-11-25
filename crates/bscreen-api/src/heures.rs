//! Heures lisibles (heure locale) ↔ millisecondes depuis 1970.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike};

const JOURS: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];

/// « mer. 19/11 21:34:05 » (heure locale).
pub fn lisible(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(d) => format!(
            "{} {:02}/{:02} {:02}:{:02}:{:02}",
            JOURS[d.weekday().num_days_from_monday() as usize],
            d.day(),
            d.month(),
            d.hour(),
            d.minute(),
            d.second()
        ),
        None => ms.to_string(),
    }
}

/// Le jour (heure locale) d'un instant.
pub fn jour_local(ms: i64) -> NaiveDate {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.date_naive())
        .unwrap_or_default()
}

fn en_ms(d: NaiveDateTime) -> Option<i64> {
    Local.from_local_datetime(&d).earliest().map(|d| d.timestamp_millis())
}

/// Une heure tapée par un humain ou un agent, en début et fin de période :
/// « aujourd'hui », « hier », « 2026-11-19 » (la journée), « 2026-11-19 21:30 », « 21:30 » (aujourd'hui),
/// ou un instant RFC 3339. Rend (début, fin) en ms.
pub fn periode(texte: &str, maintenant_ms: i64) -> Option<(i64, i64)> {
    let t = texte.trim().to_lowercase();
    let maintenant = Local.timestamp_millis_opt(maintenant_ms).single()?;
    let jour = |date: NaiveDate| -> Option<(i64, i64)> {
        let debut = en_ms(date.and_time(NaiveTime::MIN))?;
        let fin = en_ms((date + Duration::days(1)).and_time(NaiveTime::MIN))? - 1;
        Some((debut, fin))
    };
    match t.as_str() {
        "aujourd'hui" | "aujourd’hui" | "today" => return jour(maintenant.date_naive()),
        "hier" | "yesterday" => return jour(maintenant.date_naive() - Duration::days(1)),
        "avant-hier" => return jour(maintenant.date_naive() - Duration::days(2)),
        _ => {}
    }
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(texte.trim()) {
        let ms = d.timestamp_millis();
        return Some((ms, ms));
    }
    if let Ok(date) = NaiveDate::parse_from_str(&t, "%Y-%m-%d") {
        return jour(date);
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(&t, format) {
            let ms = en_ms(d)?;
            return Some((ms, ms));
        }
    }
    for format in ["%H:%M:%S", "%H:%M", "%Hh%M", "%Hh"] {
        if let Ok(h) = NaiveTime::parse_from_str(&t, format) {
            let ms = en_ms(maintenant.date_naive().and_time(h))?;
            return Some((ms, ms));
        }
    }
    None
}

/// La période dont parle une question, en français courant : « ce soir », « hier matin »,
/// « lundi », « cette semaine », « tout à l'heure », « il y a 3 heures »… Rend (début, fin, libellé).
pub fn periode_dans_question(question: &str, maintenant_ms: i64) -> Option<(i64, i64, String)> {
    let q = plier(question);
    let maintenant = Local.timestamp_millis_opt(maintenant_ms).single()?;
    // La nuit, la journée qu'on vient de vivre est encore « aujourd'hui » (à 1 h, « ce soir » est le
    // soir qui vient de passer) : elle commence à 5 h. « Hier », « lundi » suivent le calendrier
    // (c'est ainsi que Claude les lit aussi : le contexte et sa lecture restent d'accord).
    let calendrier = maintenant.date_naive();
    let aujourdhui = if maintenant.hour() < 5 { calendrier - Duration::days(1) } else { calendrier };
    // De h0 à h1 heures ce jour-là ; h1 au-delà de 24 : le lendemain (le soir va jusqu'à 5 h).
    let plage = |date: NaiveDate, h0: u32, h1: u32, libelle: &str| -> Option<(i64, i64, String)> {
        let debut = en_ms(date.and_hms_opt(h0, 0, 0)?)?;
        let fin = en_ms((date + Duration::days(i64::from(h1 / 24))).and_hms_opt(h1 % 24, 0, 0)?)? - 1;
        Some((debut, fin.min(maintenant_ms.max(debut)), libelle.to_string()))
    };
    let moment_du_jour = |date: NaiveDate, nom: &str| -> Option<(i64, i64, String)> {
        if q.contains(&format!("{nom} matin")) {
            plage(date, 5, 12, &format!("{nom} matin"))
        } else if q.contains(&format!("{nom} apres-midi")) || q.contains(&format!("{nom} aprem")) {
            plage(date, 12, 18, &format!("{nom} après-midi"))
        } else if q.contains(&format!("{nom} soir")) {
            plage(date, 18, 29, &format!("{nom} soir"))
        } else {
            plage(date, 0, 24, nom)
        }
    };
    // « il y a 3 heures / 2 jours / 20 minutes »
    if let Some(pos) = q.find("il y a ") {
        let reste: Vec<&str> = q[pos + 7..].split_whitespace().collect();
        let nombre = match reste.first().copied() {
            Some("une") | Some("un") => Some(1),
            Some(n) => n.parse::<i64>().ok(),
            None => None,
        };
        if let (Some(n), Some(unite)) = (nombre, reste.get(1)) {
            let ms = if unite.starts_with("minute") {
                Some(n * 60_000)
            } else if unite.starts_with("heure") {
                Some(n * 3_600_000)
            } else if unite.starts_with("jour") {
                Some(n * 86_400_000)
            } else {
                None
            };
            if let Some(ms) = ms {
                let vers = maintenant_ms - ms;
                let marge = (ms / 3).max(30 * 60_000);
                return Some((vers - marge, (vers + marge).min(maintenant_ms), format!("il y a {n} {unite}")));
            }
        }
    }
    // « ces 20 dernières minutes », « la dernière heure », « depuis 10 minutes » : jusqu'à maintenant.
    if let Some((ms, libelle)) = duree_recente(&q) {
        return Some((maintenant_ms - ms, maintenant_ms, libelle));
    }
    if q.contains("avant-hier") || q.contains("avant hier") {
        return moment_du_jour(calendrier - Duration::days(2), "avant-hier");
    }
    if q.contains("hier") {
        return moment_du_jour(calendrier - Duration::days(1), "hier");
    }
    if q.contains("tout a l'heure") || q.contains("tout a lheure") {
        return Some((maintenant_ms - 4 * 3_600_000, maintenant_ms, "tout à l'heure".to_string()));
    }
    if q.contains("ce matin") {
        return plage(aujourdhui, 5, 12, "ce matin");
    }
    if q.contains("cet apres-midi") || q.contains("cet aprem") || q.contains("cette apres-midi") {
        return plage(aujourdhui, 12, 18, "cet après-midi");
    }
    if q.contains("ce soir") {
        return plage(aujourdhui, 18, 29, "ce soir");
    }
    if q.contains("cette nuit") {
        // La nuit en cours (après minuit), sinon la dernière nuit passée.
        let veille = if maintenant.hour() < 12 { maintenant.date_naive() - Duration::days(1) } else { maintenant.date_naive() };
        let debut = en_ms(veille.and_hms_opt(22, 0, 0)?)?;
        let fin = en_ms((veille + Duration::days(1)).and_hms_opt(7, 0, 0)?)?.min(maintenant_ms);
        return Some((debut, fin.max(debut), "cette nuit".to_string()));
    }
    if q.contains("aujourd'hui") || q.contains("aujourdhui") {
        return plage(aujourdhui, 0, 24, "aujourd'hui");
    }
    let lundi = calendrier - Duration::days(i64::from(calendrier.weekday().num_days_from_monday()));
    if q.contains("semaine derniere") {
        let debut = en_ms((lundi - Duration::days(7)).and_time(NaiveTime::MIN))?;
        let fin = en_ms(lundi.and_time(NaiveTime::MIN))? - 1;
        return Some((debut, fin, "la semaine dernière".to_string()));
    }
    if q.contains("cette semaine") {
        return Some((en_ms(lundi.and_time(NaiveTime::MIN))?, maintenant_ms, "cette semaine".to_string()));
    }
    if q.contains("week-end") || q.contains("weekend") {
        // Le dernier samedi (celui d'aujourd'hui si on est le week-end).
        let recul = (i64::from(calendrier.weekday().num_days_from_monday()) - 5).rem_euclid(7);
        let samedi = calendrier - Duration::days(recul);
        let debut = en_ms(samedi.and_time(NaiveTime::MIN))?;
        let fin = (en_ms((samedi + Duration::days(2)).and_time(NaiveTime::MIN))? - 1).min(maintenant_ms);
        return Some((debut, fin, "le week-end".to_string()));
    }
    const NOMS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
    for (i, nom) in NOMS.iter().enumerate() {
        if q.split(|c: char| !c.is_alphanumeric()).any(|mot| mot == *nom) {
            // Le dernier jour de ce nom (aujourd'hui compris).
            let recul = (i64::from(calendrier.weekday().num_days_from_monday()) - i as i64).rem_euclid(7);
            return moment_du_jour(calendrier - Duration::days(recul), nom);
        }
    }
    None
}

/// Minuscules sans accents, apostrophe typographique ramenée à « ' », pour reconnaître les mots
/// d'une question.
/// Une durée qui finit maintenant, et son nom : « ces 20 dernières minutes », « les 2 dernières
/// heures », « les 3 derniers jours », « la dernière heure », « le dernier quart d'heure »,
/// « la dernière demi-heure », « depuis 10 minutes ». `q` est déjà plié (minuscules, sans accents).
fn duree_recente(q: &str) -> Option<(i64, String)> {
    if q.contains("dernier quart d'heure") || q.contains("dernier quart dheure") {
        return Some((15 * 60_000, "le dernier quart d'heure".to_string()));
    }
    if q.contains("derniere demi-heure") || q.contains("derniere demi heure") {
        return Some((30 * 60_000, "la dernière demi-heure".to_string()));
    }
    let nombre = |mot: &str| -> Option<i64> {
        match mot {
            "un" | "une" => Some(1),
            "deux" => Some(2),
            "trois" => Some(3),
            "quatre" => Some(4),
            "cinq" => Some(5),
            "dix" => Some(10),
            "quinze" => Some(15),
            "vingt" => Some(20),
            "trente" => Some(30),
            _ => mot.parse::<i64>().ok().filter(|n| *n > 0),
        }
    };
    let duree = |n: i64, unite: &str| -> Option<(i64, String)> {
        let (ms, une, plusieurs) = if unite.starts_with("minute") {
            (60_000, "la dernière minute", "dernières minutes")
        } else if unite.starts_with("heure") {
            (3_600_000, "la dernière heure", "dernières heures")
        } else if unite.starts_with("jour") {
            (86_400_000, "le dernier jour", "derniers jours")
        } else {
            return None;
        };
        let libelle = if n == 1 { une.to_string() } else { format!("les {n} {plusieurs}") };
        Some((n * ms, libelle))
    };
    let mots: Vec<&str> = q
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '?' | '.' | '!' | ';'))
        .filter(|m| !m.is_empty())
        .collect();
    for (i, mot) in mots.iter().enumerate() {
        let suivant = mots.get(i + 1).copied().unwrap_or_default();
        if mot.starts_with("dernier") {
            // « les 20 dernières minutes » (le nombre avant), « la dernière heure » (sans nombre).
            let avant = i.checked_sub(1).map(|j| mots[j]).unwrap_or_default();
            if let Some(n) = nombre(avant)
                && let Some(trouvee) = duree(n, suivant)
            {
                return Some(trouvee);
            }
            if matches!(avant, "la" | "cette" | "le" | "ce")
                && let Some(trouvee) = duree(1, suivant)
            {
                return Some(trouvee);
            }
        }
        // « depuis 10 minutes » (pas « depuis 10 heures » : ce serait aussi « depuis 10 h »).
        if *mot == "depuis"
            && let Some(n) = nombre(suivant)
            && mots.get(i + 2).is_some_and(|u| u.starts_with("minute"))
        {
            return duree(n, "minutes");
        }
    }
    None
}

/// Minuscules sans accents (apostrophe typographique ramenée à « ' »), pour comparer des mots.
pub fn plier(texte: &str) -> String {
    texte
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            '\u{2019}' => '\'',
            autre => autre,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodes_dans_une_question() {
        // Vendredi 20/11/2026, 23 h.
        let soir = Local.with_ymd_and_hms(2026, 11, 20, 23, 0, 0).unwrap().timestamp_millis();
        let p = |q: &str| periode_dans_question(q, soir).map(|(d, f, l)| (lisible(d), lisible(f), l));
        assert_eq!(
            p("Qu'est-ce que je regardais hier soir ?"),
            Some(("jeu. 19/11 18:00:00".into(), "ven. 20/11 04:59:59".into(), "hier soir".into()))
        );
        let nuit = Local.with_ymd_and_hms(2026, 11, 21, 1, 30, 0).unwrap().timestamp_millis();
        let n = |q: &str| periode_dans_question(q, nuit).map(|(d, f, _)| (lisible(d), lisible(f)));
        assert_eq!(n("ce soir"), Some(("ven. 20/11 18:00:00".into(), "sam. 21/11 01:30:00".into())));
        assert_eq!(n("hier après-midi").map(|x| x.0), Some("ven. 20/11 12:00:00".into()), "le calendrier");
        assert_eq!(n("cette nuit").map(|x| x.0), Some("ven. 20/11 22:00:00".into()));
        assert_eq!(p("de quoi on a parlé ce matin").map(|x| x.0), Some("ven. 20/11 05:00:00".into()));
        assert_eq!(
            p("avec qui j'ai parlé lundi ?").map(|x| (x.0, x.2)),
            Some(("lun. 16/11 00:00:00".into(), "lundi".into()))
        );
        assert_eq!(p("la vidéo de cette semaine").map(|x| x.0), Some("lun. 16/11 00:00:00".into()));
        assert_eq!(p("il y a 3 heures").map(|x| x.0), Some("ven. 20/11 19:00:00".into()));
        assert_eq!(p("Ce soir, quelle série ?").map(|x| x.1), Some("ven. 20/11 23:00:00".into()), "jamais après maintenant");
        assert_eq!(p("quelle page sur les dauphins ?"), None);
        assert_eq!(p("le week-end").map(|x| x.0), Some("sam. 14/11 00:00:00".into()));
        assert_eq!(p("Qu\u{2019}ai-je fait aujourd\u{2019}hui ?").map(|x| x.2), Some("aujourd'hui".into()));
        // Les durées qui finissent maintenant.
        let recent = |q: &str| p(q).map(|x| (x.0, x.1, x.2));
        assert_eq!(
            recent("Qu'est-ce que j'ai fait ces 20 dernières minutes ?"),
            Some(("ven. 20/11 22:40:00".into(), "ven. 20/11 23:00:00".into(), "les 20 dernières minutes".into()))
        );
        assert_eq!(recent("les deux dernières heures").map(|x| x.0), Some("ven. 20/11 21:00:00".into()));
        assert_eq!(recent("pendant la dernière heure").map(|x| x.2), Some("la dernière heure".into()));
        assert_eq!(recent("les 3 derniers jours").map(|x| x.0), Some("mar. 17/11 23:00:00".into()));
        assert_eq!(recent("le dernier quart d'heure").map(|x| x.0), Some("ven. 20/11 22:45:00".into()));
        assert_eq!(recent("depuis 10 minutes").map(|x| x.0), Some("ven. 20/11 22:50:00".into()));
        assert_eq!(p("depuis 10 heures"), None, "ambigu : on laisse faire");
        assert_eq!(p("la dernière fois que j'ai vu Camille"), None);
    }

    #[test]
    fn periodes() {
        let midi = Local.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap().timestamp_millis();
        let (d, f) = periode("hier", midi).unwrap();
        assert_eq!(lisible(d), "jeu. 19/11 00:00:00");
        assert_eq!(lisible(f), "jeu. 19/11 23:59:59");
        let (d, f) = periode("2026-11-18 21:30", midi).unwrap();
        assert_eq!((lisible(d), d == f), ("mer. 18/11 21:30:00".to_string(), true));
        assert_eq!(lisible(periode("14h30", midi).unwrap().0), "ven. 20/11 14:30:00");
        assert!(periode("n'importe quoi", midi).is_none());
    }
}
