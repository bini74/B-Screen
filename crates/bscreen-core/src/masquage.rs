//! Masquage des secrets avant tout stockage du texte (phase 6.3) : numéros de carte (contrôle
//! de Luhn, les 4 derniers chiffres gardés), IBAN (contrôle mod 97), clés d'API reconnaissables,
//! jetons JWT, clés privées, valeurs après « mot de passe : », longs jetons aléatoires, et
//! paramètres sensibles des adresses web.
//!
//! Le texte masqué est ce qui entre dans la base et l'index : le secret n'y est jamais.

use std::borrow::Cow;

/// Ce qui remplace un secret.
pub const MASQUE: &str = "••••••";

/// Préfixes de clés d'API connues (la suite doit faire au moins 16 caractères).
const PREFIXES_DE_CLES: &[&str] = &[
    "sk-", "sk_live_", "sk_test_", "rk_live_", "pk_live_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_",
    "glpat-", "xoxb-", "xoxp-", "xoxa-", "xapp-", "AKIA", "ASIA", "AIza", "ya29.", "hf_", "tskey-", "npm_", "pypi-",
    "dop_v1_", "shpat_", "SG.", "sq0atp-",
];

/// Étiquettes suivies d'un secret (comparées sans majuscules ni accents).
const ETIQUETTES: &[&str] = &[
    "mot de passe",
    "mots de passe",
    "password",
    "passwd",
    "passphrase",
    "mdp",
    "pwd",
    "pass",
    "code pin",
    "pin",
    "secret",
    "token",
    "api key",
    "apikey",
    "api_key",
    "cle api",
    "clé api",
    "cvv",
    "cvc",
    "cryptogramme",
];

/// Paramètres d'adresse web dont la valeur est un secret.
const PARAMETRES_SECRETS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "key",
    "api_key",
    "apikey",
    "secret",
    "client_secret",
    "password",
    "pass",
    "pwd",
    "sig",
    "signature",
    "code",
    "auth",
    "session",
    "sessionid",
    "jwt",
    "otp",
];

/// Le texte avec ses secrets masqués (sans copie s'il n'y en a pas).
/// Le masquage est-il en service (réglage « Secrets masqués », oui par défaut) ?
pub static ACTIF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn masquer(texte: &str) -> Cow<'_, str> {
    if !ACTIF.load(std::sync::atomic::Ordering::Relaxed) {
        return Cow::Borrowed(texte);
    }
    // Rapide : la plupart des textes n'ont ni chiffres en nombre ni signes de secret.
    let chiffres = texte.bytes().filter(u8::is_ascii_digit).count();
    let suspect = chiffres >= 12
        || texte.len() >= 20 && texte.split_whitespace().any(|m| m.len() >= 20)
        || texte.contains(':')
        || texte.contains('=')
        || texte.contains("-----BEGIN");
    if !suspect {
        return Cow::Borrowed(texte);
    }
    let mut sortie = masquer_cles_privees(texte);
    sortie = masquer_cartes(&sortie);
    sortie = masquer_mots(&sortie);
    sortie = masquer_apres_etiquette(&sortie);
    if sortie == texte {
        Cow::Borrowed(texte)
    } else {
        Cow::Owned(sortie)
    }
}

/// Une adresse web sans les valeurs de ses paramètres sensibles (`?token=…` → `?token=••••••`)
/// ni identifiants (`https://jeton@hôte`).
pub fn masquer_url(url: &str) -> Cow<'_, str> {
    if !ACTIF.load(std::sync::atomic::Ordering::Relaxed) {
        return Cow::Borrowed(url);
    }
    match masquer(url) {
        Cow::Borrowed(_) => masquer_mot(url),
        Cow::Owned(m) => Cow::Owned(masquer_mot(&m).into_owned()),
    }
}

/// Les paramètres sensibles d'une adresse, masqués ; `None` s'il n'y en a pas.
fn masquer_parametres(url: &str) -> Option<String> {
    let debut_requete = url.find('?')?;
    let (base, reste) = url.split_at(debut_requete + 1);
    let (requete, ancre) = match reste.find('#') {
        Some(i) => (&reste[..i], &reste[i..]),
        None => (reste, ""),
    };
    let mut change = false;
    let parametres: Vec<String> = requete
        .split('&')
        .map(|p| match p.split_once('=') {
            Some((nom, valeur))
                if !valeur.is_empty()
                    && !valeur.starts_with('•')
                    && PARAMETRES_SECRETS.contains(&nom.to_ascii_lowercase().as_str()) =>
            {
                change = true;
                format!("{nom}={MASQUE}")
            }
            _ => p.to_string(),
        })
        .collect();
    change.then(|| format!("{base}{}{ancre}", parametres.join("&")))
}

/// Ce qui remplace une clé privée (écrit en deux morceaux : le code ne contient aucun en-tête de clé).
const DEBUT_CLE: &str = concat!("-----BEGIN PRIVATE", " KEY----- ");
const FIN_CLE: &str = concat!(" -----END PRIVATE", " KEY-----");

fn masquer_cles_privees(texte: &str) -> String {
    let mut sortie = String::with_capacity(texte.len());
    let mut reste = texte;
    while let Some(i) = reste.find("-----BEGIN") {
        sortie.push_str(&reste[..i]);
        let apres = &reste[i..];
        // Une clé privée (ou un certificat avec sa clé) : tout jusqu'à « -----END … ----- ».
        let entete_fin = apres.find("-----\n").or_else(|| apres[10..].find("-----").map(|j| j + 10));
        let entete = entete_fin.map(|j| &apres[..j]).unwrap_or(apres);
        if !entete.contains("PRIVATE KEY") {
            sortie.push_str("-----BEGIN");
            reste = &apres[10..];
            continue;
        }
        match apres.find("-----END") {
            Some(j) => {
                let fin = apres[j + 8..].find("-----").map(|k| j + 8 + k + 5).unwrap_or(apres.len());
                sortie.push_str(DEBUT_CLE);
                sortie.push_str(MASQUE);
                sortie.push_str(FIN_CLE);
                reste = &apres[fin..];
            }
            None => {
                sortie.push_str(DEBUT_CLE);
                sortie.push_str(MASQUE);
                reste = "";
            }
        }
    }
    sortie.push_str(reste);
    sortie
}

fn luhn(chiffres: &[u8]) -> bool {
    let somme: u32 = chiffres
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &c)| {
            let d = u32::from(c);
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            }
        })
        .sum();
    somme.is_multiple_of(10)
}

/// Numéros de carte : 13 à 19 chiffres (groupés par espaces ou tirets), valides selon Luhn.
fn masquer_cartes(texte: &str) -> String {
    let octets = texte.as_bytes();
    let mut sortie = String::with_capacity(texte.len());
    let mut i = 0;
    let mut copie_depuis = 0;
    while i < octets.len() {
        if !octets[i].is_ascii_digit() || (i > 0 && octets[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        // Une suite de chiffres, avec au plus un espace ou un tiret entre deux groupes.
        let debut = i;
        let mut chiffres: Vec<u8> = Vec::new();
        let mut j = i;
        let mut fin = i;
        while j < octets.len() && chiffres.len() < 20 {
            let c = octets[j];
            if c.is_ascii_digit() {
                chiffres.push(c - b'0');
                j += 1;
                fin = j;
            } else if (c == b' ' || c == b'-') && j + 1 < octets.len() && octets[j + 1].is_ascii_digit() {
                j += 1;
            } else {
                break;
            }
        }
        let apres_ok = fin >= octets.len() || !octets[fin].is_ascii_alphanumeric();
        if (13..=19).contains(&chiffres.len()) && apres_ok && luhn(&chiffres) && chiffres.iter().any(|&c| c != 0) {
            sortie.push_str(&texte[copie_depuis..debut]);
            let derniers: String = chiffres[chiffres.len() - 4..].iter().map(|c| char::from(b'0' + c)).collect();
            sortie.push_str("•••• •••• •••• ");
            sortie.push_str(&derniers);
            copie_depuis = fin;
        }
        i = fin.max(i + 1);
    }
    sortie.push_str(&texte[copie_depuis..]);
    sortie
}

fn iban_valide(compact: &str) -> bool {
    let b = compact.as_bytes();
    if !(15..=34).contains(&b.len())
        || !b[..2].iter().all(u8::is_ascii_uppercase)
        || !b[2..4].iter().all(u8::is_ascii_digit)
        || !b.iter().all(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    // Les 4 premiers à la fin, lettres → nombres (A = 10), reste de la division par 97 = 1.
    let mut reste: u32 = 0;
    for &c in b[4..].iter().chain(&b[..4]) {
        let valeur = if c.is_ascii_digit() {
            u32::from(c - b'0')
        } else {
            u32::from(c.to_ascii_uppercase() - b'A') + 10
        };
        reste = if valeur >= 10 { (reste * 100 + valeur) % 97 } else { (reste * 10 + valeur) % 97 };
    }
    reste == 1
}

/// Un « mot » qui ressemble à une clé : préfixe connu, jeton JWT, ou longue suite aléatoire.
fn est_un_secret(mot: &str) -> bool {
    let mot = mot.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '<' | '>'));
    if mot.len() < 20 {
        return false;
    }
    if PREFIXES_DE_CLES
        .iter()
        .any(|p| mot.starts_with(p) && mot.len() >= p.len() + 16)
    {
        return true;
    }
    // JWT : trois parties en base64url, les deux premières commencent par « eyJ ».
    let parties: Vec<&str> = mot.split('.').collect();
    if parties.len() == 3 && parties[0].starts_with("eyJ") && parties[1].starts_with("eyJ") && parties[2].len() >= 16 {
        return true;
    }
    // Un chemin de fichier n'est pas un secret, même long et varié.
    if mot.starts_with(['/', '~', '.', '$']) || mot.contains("://") {
        return false;
    }
    // Longue suite aléatoire : minuscules, majuscules et chiffres mêlés, très variée.
    if mot.len() >= 32
        && mot.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
        && mot.chars().any(|c| c.is_ascii_lowercase())
        && mot.chars().any(|c| c.is_ascii_uppercase())
        && mot.chars().any(|c| c.is_ascii_digit())
    {
        return entropie(mot) >= 4.2;
    }
    false
}

/// Bits par caractère (Shannon).
fn entropie(mot: &str) -> f64 {
    let mut comptes = [0u32; 128];
    let mut n = 0u32;
    for c in mot.bytes().filter(u8::is_ascii) {
        comptes[c as usize] += 1;
        n += 1;
    }
    comptes
        .iter()
        .filter(|&&k| k > 0)
        .map(|&k| {
            let p = f64::from(k) / f64::from(n);
            -p * p.log2()
        })
        .sum()
}

/// Les mots un par un : clés, jetons, IBAN (ceux écrits par groupes de 4 sont recollés),
/// identifiants dans les adresses web.
fn masquer_mots(texte: &str) -> String {
    let mots: Vec<&str> = texte.split(' ').collect();
    let mut sortie: Vec<String> = Vec::with_capacity(mots.len());
    let mut i = 0;
    while i < mots.len() {
        if let Some((suivant, masque)) = iban_groupe(&mots, i) {
            sortie.push(masque);
            i = suivant;
            continue;
        }
        sortie.push(masquer_mot(mots[i]).into_owned());
        i += 1;
    }
    sortie.join(" ")
}

/// Un IBAN écrit par groupes (« FR76 3000 6000 0112 3456 7890 189 ») qui commence au mot `i` :
/// l'indice du mot qui suit, et sa version masquée.
fn iban_groupe(mots: &[&str], i: usize) -> Option<(usize, String)> {
    let tete = mots[i].as_bytes();
    if tete.len() != 4 || !tete[..2].iter().all(u8::is_ascii_uppercase) || !tete[2..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut compact = String::new();
    let mut trouve = None;
    for (k, mot) in mots.iter().enumerate().skip(i) {
        let m = mot.trim_end_matches([',', '.', ';']);
        if m.is_empty() || m.len() > 4 || !m.chars().all(|c| c.is_ascii_alphanumeric()) {
            break;
        }
        compact.push_str(m);
        if compact.len() > 34 {
            break;
        }
        if compact.len() >= 15 && iban_valide(&compact) {
            trouve = Some((k + 1, format!("{} {MASQUE} {}", &compact[..4], &compact[compact.len() - 4..])));
        }
        // Le dernier groupe peut être plus court : il termine l'IBAN.
        if m.len() < 4 {
            break;
        }
    }
    trouve
}

/// Un mot : identifiants d'une adresse web, IBAN collé, ou clé (seule la valeur de « clé=valeur »).
fn masquer_mot(mot: &str) -> Cow<'_, str> {
    if let Some(debut) = mot.find("://") {
        let apres = &mot[debut + 3..];
        let fin_hote = apres.find('/').unwrap_or(apres.len());
        // https://jeton@hôte ou https://nom:motdepasse@hôte
        let mot: Cow<'_, str> = match apres[..fin_hote].rfind('@') {
            Some(arobase) => Cow::Owned(format!("{}{MASQUE}{}", &mot[..debut + 3], &apres[arobase..])),
            None => Cow::Borrowed(mot),
        };
        return match masquer_parametres(&mot) {
            Some(m) => Cow::Owned(m),
            None => mot,
        };
    }
    let propre = mot.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if iban_valide(propre) {
        return Cow::Owned(format!("{} {MASQUE} {}", &propre[..4], &propre[propre.len() - 4..]));
    }
    // « clé=valeur » avec une clé qui est un nom (pas le bourrage « == » d'un jeton base64) :
    // seule la valeur est jugée.
    if let Some(separateur) = mot.find(['=', ':']) {
        let (cle, valeur) = (&mot[..=separateur], &mot[separateur + 1..]);
        let nom = &cle[..cle.len() - 1];
        let est_un_nom = !nom.is_empty()
            && nom.len() <= 40
            && nom.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$'))
            && !est_un_secret(nom);
        if est_un_nom && !valeur.trim_matches('=').is_empty() {
            return if est_un_secret(valeur) {
                Cow::Owned(format!("{cle}{MASQUE}"))
            } else {
                Cow::Borrowed(mot)
            };
        }
    }
    if est_un_secret(mot) {
        Cow::Owned(MASQUE.to_string())
    } else {
        Cow::Borrowed(mot)
    }
}

/// « mot de passe : xyz », « password=xyz », « PIN 1234 » : la valeur qui suit est masquée.
fn masquer_apres_etiquette(texte: &str) -> String {
    let plie: Vec<char> = texte.chars().map(plier_car).collect();
    let originaux: Vec<char> = texte.chars().collect();
    let mut masquer_de: Vec<(usize, usize)> = Vec::new();
    for etiquette in ETIQUETTES {
        let e: Vec<char> = etiquette.chars().map(plier_car).collect();
        let mut i = 0;
        while i + e.len() <= plie.len() {
            let avant_ok = i == 0 || !plie[i - 1].is_alphanumeric();
            if avant_ok && plie[i..i + e.len()] == e[..] {
                let mut j = i + e.len();
                // Séparateur obligatoire : « : », « = » (avec ou sans espaces).
                while j < plie.len() && plie[j] == ' ' {
                    j += 1;
                }
                if j < plie.len() && (plie[j] == ':' || plie[j] == '=') {
                    j += 1;
                    while j < plie.len() && plie[j] == ' ' {
                        j += 1;
                    }
                    let debut = j;
                    while j < plie.len() && !plie[j].is_whitespace() {
                        j += 1;
                    }
                    if j > debut {
                        masquer_de.push((debut, j));
                    }
                }
                i += e.len();
            } else {
                i += 1;
            }
        }
    }
    if masquer_de.is_empty() {
        return texte.to_string();
    }
    masquer_de.sort();
    let mut sortie = String::with_capacity(texte.len());
    let mut position = 0;
    for (debut, fin) in masquer_de {
        if debut < position {
            continue;
        }
        sortie.extend(&originaux[position..debut]);
        // Déjà masqué : on le laisse tel quel.
        let valeur: String = originaux[debut..fin].iter().collect();
        sortie.push_str(if valeur.starts_with('•') { &valeur } else { MASQUE });
        position = fin;
    }
    sortie.extend(&originaux[position..]);
    sortie
}

fn plier_car(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'â' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' => 'i',
        'ô' | 'ö' => 'o',
        'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        autre => autre,
    }
}

/// Pour les mots d'une ligne lue à l'écran (OCR), lesquels font partie d'un secret : la ligne
/// entière passe par [`masquer`], puis la version masquée est alignée sur l'originale (plus
/// longue sous-suite commune) ; les mots qui n'y sont plus sont des secrets.
pub fn mots_secrets(mots: &[&str]) -> Vec<bool> {
    let ligne = mots.join(" ");
    let masquee = masquer(&ligne);
    if matches!(masquee, Cow::Borrowed(_)) {
        return vec![false; mots.len()];
    }
    let apres: Vec<&str> = masquee.split(' ').filter(|m| !m.is_empty()).collect();
    let (n, m) = (mots.len(), apres.len());
    // Plus longue sous-suite commune (les lignes sont courtes).
    let mut table = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if mots[i] == apres[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut secrets = vec![true; n];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if mots[i] == apres[j] {
            secrets[i] = false;
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    secrets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rien_a_masquer() {
        for t in [
            "Bonjour Camille, on se voit demain à 14 h ?",
            "Commande n° 2026-11-20 livrée",
            "commit 6f2da3a1c0de5b7e9f8a7b6c5d4e3f2a1b0c9d8e",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "Le prix : 1 299,00 €",
            "cd /private/tmp/claude-501/-Users-julienbonzi/83378e55-b600-413d-bf56-70858471995d/scratchpad",
            "error: `/Users/moi/Developer/b-screen/target/debug/bscreen` (exit status: 1)",
            "F=\"$HOME/Library/Application Support/B-Screen/index.sqlite\"",
            "S=/private/tmp/claude-501/-Users-julienbonzi/83378e55-b600-413d-bf56-70858471995d/scratchpad; mkdir -p $S",
        ] {
            assert_eq!(masquer(t), t, "{t}");
        }
    }

    #[test]
    fn cartes() {
        assert_eq!(masquer("Carte 4970 1012 3456 7893 exp 12/28"), "Carte •••• •••• •••• 7893 exp 12/28");
        assert_eq!(masquer("4111111111111111"), "•••• •••• •••• 1111");
        // Faux numéro (Luhn invalide) : laissé.
        assert_eq!(masquer("4970 1012 3456 7890"), "4970 1012 3456 7890");
    }

    #[test]
    fn iban() {
        assert_eq!(masquer("IBAN : FR76 3000 6000 0112 3456 7890 189"), "IBAN : FR76 •••••• 0189");
        assert_eq!(masquer("FR7630006000011234567890189"), "FR76 •••••• 0189");
    }

    #[test]
    fn cles_et_jetons() {
        // Faux secrets, assemblés à l'exécution : le dépôt n'en contient aucun tel quel.
        let cle_openai = ["sk", "-proj-A1b2C3d4E5f6G7h8I9j0K1l2"].concat();
        let jeton_github = ["gh", "p_16C7e42F292c6912E7710c838347Ae178B4a"].concat();
        let cle_aws = ["AK", "IAIOSFODNN7EXAMPLE1234"].concat();
        assert_eq!(
            masquer(&format!("export OPENAI_API_KEY={cle_openai}")),
            format!("export OPENAI_API_KEY={MASQUE}")
        );
        assert_eq!(
            masquer(&format!("git push https://{jeton_github}@github.com/bini74/b-screen.git")),
            format!("git push https://{MASQUE}@github.com/bini74/b-screen.git")
        );
        let jwt = ["ey", "JhbGciOiJIUzI1NiJ9.", "ey", "JzdWIiOiJqdWxpZW4ifQ.4pcPyMD09olPSyXnrXCjTwXyr4BsezdI1AVTmud2fU4"].concat();
        assert_eq!(masquer(&format!("Bearer {jwt}")), format!("Bearer {MASQUE}"));
        assert_eq!(masquer(&format!("aws {cle_aws} ok")), format!("aws {MASQUE} ok"));
        // Clé secrète AWS (base64 avec des « / ») et jeton base64 terminé par « == ».
        assert_eq!(masquer("secret wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"), format!("secret {MASQUE}"));
        assert_eq!(masquer("jeton Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MEFCQ0RFRkdISUpL=="), format!("jeton {MASQUE}"));
    }

    #[test]
    fn apres_une_etiquette() {
        assert_eq!(masquer("Mot de passe : Chaton2026!"), format!("Mot de passe : {MASQUE}"));
        assert_eq!(masquer("mdp=hunter2 et le reste"), format!("mdp={MASQUE} et le reste"));
        assert_eq!(masquer("Code PIN: 4821"), format!("Code PIN: {MASQUE}"));
        // « passe » sans séparateur : rien.
        assert_eq!(masquer("je passe demain"), "je passe demain");
    }

    #[test]
    fn cle_privee() {
        let t = ["-----BEGIN OPENSSH PRIV", "ATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIV", "ATE KEY-----"].concat();
        let m = masquer(&t);
        assert!(!m.contains("b3BlbnNz"), "{m}");
        assert!(m.contains(MASQUE));
        let certificat = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert!(masquer(certificat).contains("MIIB"), "un certificat public n'est pas un secret");
    }

    #[test]
    fn mots_secrets_d_une_ligne() {
        let ligne = ["Mot", "de", "passe", ":", "Chaton2026!", "(ne", "pas", "partager)"];
        assert_eq!(
            mots_secrets(&ligne),
            vec![false, false, false, false, true, false, false, false]
        );
        let iban = ["IBAN", "FR76", "3000", "6000", "0112", "3456", "7890", "189", "merci"];
        assert_eq!(
            mots_secrets(&iban),
            vec![false, false, true, true, true, true, true, true, false]
        );
        let carte = ["carte", "4970", "1012", "3456", "7893"];
        assert_eq!(mots_secrets(&carte), vec![false, true, true, true, false], "les 4 derniers chiffres restent");
        assert_eq!(mots_secrets(&["Bonjour", "Camille"]), vec![false, false]);
    }

    #[test]
    fn adresses_web() {
        assert_eq!(
            masquer_url("https://exemple.fr/reset?email=a@b.fr&token=abc123&lang=fr#haut"),
            format!("https://exemple.fr/reset?email=a@b.fr&token={MASQUE}&lang=fr#haut")
        );
        assert_eq!(masquer_url("https://www.google.com/search?q=dauphins"), "https://www.google.com/search?q=dauphins");
    }
}
