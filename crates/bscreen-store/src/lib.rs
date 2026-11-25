//! La base de B-Screen (SQLite, portable).
//!
//! Phase 2 : les morceaux vidéo et les images qu'ils contiennent.
//! Les textes, voix, vecteurs et épisodes viendront s'ajouter dans les phases suivantes
//! (chaque ajout de tables passe par une nouvelle version du schéma).
//!
//! Les heures sont en millisecondes depuis le 1er janvier 1970 (UTC).

use std::path::Path;

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
pub use rusqlite::{Error, Result};

/// Version du schéma, rangée dans `PRAGMA user_version`.
const VERSION_SCHEMA: i32 = 16;

const SCHEMA_V1: &str = "
CREATE TABLE morceaux (
    id        INTEGER PRIMARY KEY,
    ecran     INTEGER NOT NULL,
    -- chemin relatif au dossier des données, ex. video/2026-11-19/ecran1-215700.mp4
    fichier   TEXT    NOT NULL UNIQUE,
    debut_ms  INTEGER NOT NULL,
    fin_ms    INTEGER,
    largeur   INTEGER NOT NULL,
    hauteur   INTEGER NOT NULL,
    octets    INTEGER
);
CREATE INDEX morceaux_debut ON morceaux(debut_ms);

CREATE TABLE images (
    id           INTEGER PRIMARY KEY,
    morceau      INTEGER NOT NULL REFERENCES morceaux(id) ON DELETE CASCADE,
    quand_ms     INTEGER NOT NULL,
    -- position de l'image dans la vidéo du morceau
    position_ms  INTEGER NOT NULL,
    ecran        INTEGER NOT NULL,
    app_bundle   TEXT,
    app_nom      TEXT,
    -- part de l'écran qui a changé depuis l'image précédente (0 à 1)
    aire         REAL    NOT NULL,
    -- gardée tout de suite à cause d'un changement d'app
    declenchee   INTEGER NOT NULL
);
CREATE INDEX images_quand ON images(quand_ms);
CREATE INDEX images_morceau ON images(morceau);
";

/// Phase 3 : le texte. Chaque texte distinct est rangé une seule fois (empreinte) ;
/// une « apparition » dit où et pendant quelle plage de temps il était à l'écran.
const SCHEMA_V2: &str = "
ALTER TABLE images ADD COLUMN fenetre TEXT;
ALTER TABLE images ADD COLUMN url TEXT;

CREATE TABLE textes (
    id         INTEGER PRIMARY KEY,
    empreinte  INTEGER NOT NULL UNIQUE,
    contenu    TEXT    NOT NULL
);
-- Recherche plein texte (BM25), accents ignorés, préfixes de 3 lettres et plus.
CREATE VIRTUAL TABLE textes_fts USING fts5(
    contenu, content='textes', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2', prefix='3'
);
CREATE TRIGGER textes_ajout AFTER INSERT ON textes BEGIN
    INSERT INTO textes_fts(rowid, contenu) VALUES (new.id, new.contenu);
END;
CREATE TRIGGER textes_retrait AFTER DELETE ON textes BEGIN
    INSERT INTO textes_fts(textes_fts, rowid, contenu) VALUES ('delete', old.id, old.contenu);
END;

CREATE TABLE apparitions (
    id        INTEGER PRIMARY KEY,
    texte     INTEGER NOT NULL REFERENCES textes(id),
    ecran     INTEGER NOT NULL,
    debut_ms  INTEGER NOT NULL,
    fin_ms    INTEGER NOT NULL,
    -- 0 = accessibilité, 1 = OCR
    source    INTEGER NOT NULL,
    vu        INTEGER NOT NULL,
    focus     INTEGER NOT NULL,
    -- position à l'écran, en points
    x REAL, y REAL, l REAL, h REAL
);
CREATE INDEX apparitions_texte ON apparitions(texte);
CREATE INDEX apparitions_temps ON apparitions(ecran, fin_ms);

CREATE TABLE recherches (
    id        INTEGER PRIMARY KEY,
    quand_ms  INTEGER NOT NULL,
    moteur    TEXT    NOT NULL,
    requete   TEXT    NOT NULL,
    url       TEXT
);
CREATE INDEX recherches_quand ON recherches(quand_ms);
";

/// Schéma v3 : les passages de voix (micro ou son du Mac), leur fichier audio et leur transcription.
const SCHEMA_V3: &str = "
CREATE TABLE voix (
    id        INTEGER PRIMARY KEY,
    -- 0 = micro, 1 = son du Mac
    source    INTEGER NOT NULL,
    debut_ms  INTEGER NOT NULL,
    fin_ms    INTEGER NOT NULL,
    fichier   TEXT    NOT NULL,
    -- transcription (NULL tant qu'elle n'est pas faite, '' si rien de compris)
    texte     TEXT,
    -- les mots avec leur heure, en JSON : [[debut_ms, fin_ms, \"mot\"], ...]
    mots      TEXT,
    locuteur  TEXT,
    modele    TEXT
);
CREATE INDEX voix_temps ON voix(debut_ms);
CREATE INDEX voix_a_transcrire ON voix(id) WHERE texte IS NULL;
";

/// Schéma v4 : voix familières. Chaque passage a son empreinte de voix (256 f32) et la voix
/// connue à laquelle il est rattaché ; chaque voix garde le centre de ses passages.
const SCHEMA_V4: &str = "
CREATE TABLE locuteurs (
    id        INTEGER PRIMARY KEY,
    -- NULL tant que Julien ne l'a pas nommée (affichée « Voix 3 »)
    nom       TEXT,
    centre    BLOB    NOT NULL,
    passages  INTEGER NOT NULL DEFAULT 0,
    -- la voix de Julien (déduite du micro)
    moi       INTEGER NOT NULL DEFAULT 0
);
ALTER TABLE voix ADD COLUMN empreinte BLOB;
ALTER TABLE voix ADD COLUMN locuteur_id INTEGER REFERENCES locuteurs(id);
CREATE INDEX voix_locuteur ON voix(locuteur_id);
";

/// Schéma v5 : petites notes de la base (travaux faits une fois, comme le masquage rétroactif).
const SCHEMA_V5: &str = "
CREATE TABLE meta (
    cle     TEXT PRIMARY KEY,
    valeur  TEXT NOT NULL
);
";

/// Schéma v6 : moments de navigation privée (verrouillés dans la frise jusqu'à Touch ID).
const SCHEMA_V6: &str = "
ALTER TABLE images ADD COLUMN privee INTEGER NOT NULL DEFAULT 0;
";

/// Schéma v7 : le « sens » des textes (phase 7) : un vecteur par morceau de texte (EmbeddingGemma,
/// 768 nombres ramenés à des octets signés + leur échelle), effacé avec son texte.
const SCHEMA_V7: &str = "
CREATE TABLE sens (
    id       INTEGER PRIMARY KEY,
    texte    INTEGER NOT NULL REFERENCES textes(id) ON DELETE CASCADE,
    morceau  INTEGER NOT NULL,
    echelle  REAL    NOT NULL,
    vecteur  BLOB    NOT NULL,
    UNIQUE (texte, morceau)
);
CREATE INDEX sens_texte ON sens(texte);
";

/// Schéma v8 : les images de navigation privée sortent des vidéos, chiffrées une à une
/// (`prive/AAAA-MM-JJ/<id>.bsp`) ; la vidéo ne les contient plus.
const SCHEMA_V8: &str = "
ALTER TABLE images ADD COLUMN fichier_prive TEXT;
";

const SCHEMA_V9: &str = "
-- Les épisodes de la journée, résumés la nuit par un modèle sur le Mac (jamais les moments privés).
CREATE TABLE episodes (
    id        INTEGER PRIMARY KEY,
    debut_ms  INTEGER NOT NULL,
    fin_ms    INTEGER NOT NULL,
    titre     TEXT    NOT NULL,
    resume    TEXT    NOT NULL,
    modele    TEXT    NOT NULL,
    cree_ms   INTEGER NOT NULL
);
CREATE INDEX episodes_temps ON episodes(debut_ms);
";

/// Schéma v10 : le son du Mac entendu pendant un appel (une autre app se servait du micro). Ses
/// voix sont des personnes ; hors appel, c'est une vidéo, sans voix (22/11).
const SCHEMA_V10: &str = "
ALTER TABLE voix ADD COLUMN appel INTEGER NOT NULL DEFAULT 0;
";

/// Schéma v11 : les corrections de Julien (« non, c'était mardi »), retenues par Ask en une
/// phrase et redonnées en tête des questions suivantes.
const SCHEMA_V11: &str = "
CREATE TABLE corrections (
    id        INTEGER PRIMARY KEY,
    cree_ms   INTEGER NOT NULL,
    texte     TEXT    NOT NULL,
    question  TEXT
);
";

/// Recherche dans les images (phase 7.3) : le vecteur d'une image retenue (un changement de scène),
/// effacé avec elle.
const SCHEMA_V12: &str = "
CREATE TABLE vues (
    id       INTEGER PRIMARY KEY,
    image    INTEGER NOT NULL UNIQUE REFERENCES images(id) ON DELETE CASCADE,
    echelle  REAL    NOT NULL,
    vecteur  BLOB    NOT NULL
);
";

/// Les sous-entendus (phase 7.6) : la version explicite d'une phrase floue (« il est derrière le
/// truc » → « le chargeur est derrière le support d'écran »), déduite du contexte la nuit ; le
/// passage d'origine n'est jamais modifié. `relue` : le passage est passé par cette relecture.
const SCHEMA_V13: &str = "
ALTER TABLE voix ADD COLUMN relue INTEGER NOT NULL DEFAULT 0;
CREATE TABLE deduits (
    id         INTEGER PRIMARY KEY,
    voix       INTEGER NOT NULL REFERENCES voix(id) ON DELETE CASCADE,
    texte      TEXT    NOT NULL,
    confiance  REAL    NOT NULL,
    modele     TEXT    NOT NULL,
    cree_ms    INTEGER NOT NULL
);
CREATE INDEX deduits_voix ON deduits(voix);
";

/// L'archive du homelab (phase 10) : les fichiers partis sur le serveur d'archive (chiffrés), et les
/// vignettes de scène gardées sur le Mac pour la frise.
const SCHEMA_V14: &str = "
CREATE TABLE archives (
    fichier    TEXT    PRIMARY KEY,
    octets     INTEGER NOT NULL,
    empreinte  TEXT    NOT NULL,
    quand_ms   INTEGER NOT NULL
);
CREATE TABLE vignettes (
    image        INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
    morceau      INTEGER NOT NULL,
    position_ms  INTEGER NOT NULL,
    fichier      TEXT    NOT NULL
);
CREATE INDEX vignettes_morceau ON vignettes(morceau, position_ms);
";

/// L'index compact (24/11/2026, voir `bscreen_core::index_compact`) : le signe de chaque nombre
/// des vecteurs de sens et d'images (96 octets au lieu de 768), seul gardé en mémoire ; les vrais
/// vecteurs restent dans `sens` et `vues` pour départager les meilleurs candidats.
const SCHEMA_V15: &str = "
CREATE TABLE codes_sens (
    id    INTEGER PRIMARY KEY REFERENCES sens(id) ON DELETE CASCADE,
    code  BLOB    NOT NULL
);
CREATE TABLE codes_vues (
    id    INTEGER PRIMARY KEY REFERENCES vues(id) ON DELETE CASCADE,
    code  BLOB    NOT NULL
);
";

/// Le code d'un vecteur rangé (octets signés), prêt pour la base.
fn code_de(vecteur: &[u8]) -> Vec<u8> {
    let octets: Vec<i8> = vecteur.iter().map(|&b| b as i8).collect();
    bscreen_core::index_compact::en_octets(&bscreen_core::index_compact::code_octets(&octets))
}

/// Passage au schéma 15 : les codes des vecteurs déjà rangés ; les vecteurs de sens des textes de
/// moins de 3 mots, qui n'en auront plus, partent.
fn remplir_codes(connexion: &Connection) -> Result<()> {
    let courts: Vec<i64> = {
        let mut requete =
            connexion.prepare("SELECT t.id, t.contenu FROM textes t WHERE EXISTS (SELECT 1 FROM sens s WHERE s.texte = t.id)")?;
        let lignes = requete.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut courts = Vec::new();
        for ligne in lignes {
            let (id, contenu) = ligne?;
            if !bscreen_core::index_compact::a_comprendre(&contenu) {
                courts.push(id);
            }
        }
        courts
    };
    {
        let mut retrait = connexion.prepare("DELETE FROM sens WHERE texte = ?1")?;
        for id in courts {
            retrait.execute(params![id])?;
        }
    }
    completer_codes(connexion).map(|_| ())
}

/// Range le code des vecteurs qui n'en ont pas (rangés par une version d'avant le schéma 15) ;
/// rend leur nombre.
fn completer_codes(connexion: &Connection) -> Result<usize> {
    let mut total = 0;
    for (table, codes) in [("sens", "codes_sens"), ("vues", "codes_vues")] {
        let lignes: Vec<(i64, Vec<u8>)> = {
            let mut requete =
                connexion.prepare(&format!("SELECT id, vecteur FROM {table} WHERE id NOT IN (SELECT id FROM {codes})"))?;
            requete.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_>>()?
        };
        let mut ajout = connexion.prepare(&format!("INSERT OR REPLACE INTO {codes} (id, code) VALUES (?1, ?2)"))?;
        for (id, vecteur) in &lignes {
            ajout.execute(params![id, code_de(vecteur)])?;
        }
        total += lignes.len();
    }
    Ok(total)
}

/// Un fichier à archiver : chemin relatif (video/AAAA-MM-JJ/… ou audio/…), le jour, et le morceau
/// vidéo s'il en est un.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AArchiver {
    pub fichier: String,
    pub jour: String,
    pub morceau: Option<i64>,
}

/// Une correction retenue : le fait corrigé, et la question où Julien l'a donnée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub id: i64,
    pub cree_ms: i64,
    pub texte: String,
    pub question: Option<String>,
}

/// Un épisode résumé : une plage d'activité, son titre et son résumé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    pub id: i64,
    pub debut_ms: i64,
    pub fin_ms: i64,
    pub titre: String,
    pub resume: String,
    pub modele: String,
}

/// Une image privée encore dans sa vidéo, à chiffrer à part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriveeARanger {
    pub id: i64,
    pub morceau: i64,
    /// La vidéo qui la contient.
    pub fichier: String,
    pub position_ms: i64,
    pub quand_ms: i64,
}

/// Un vecteur de sens stocké (octets signés ; valeur réelle = octet × échelle).
#[derive(Debug, Clone, PartialEq)]
pub struct Sens {
    pub id: i64,
    pub texte: i64,
    pub echelle: f32,
    pub vecteur: Vec<i8>,
}

/// Le vecteur d'une image retenue (octets signés ; valeur réelle = octet × échelle).
#[derive(Debug, Clone, PartialEq)]
pub struct Vue {
    pub id: i64,
    pub image: i64,
    pub echelle: f32,
    pub vecteur: Vec<i8>,
}

/// Une voix connue.
#[derive(Debug, Clone, PartialEq)]
pub struct Locuteur {
    pub id: i64,
    pub nom: Option<String>,
    /// Le centre de ses passages, en octets (f32 petit-boutiste).
    pub centre: Vec<u8>,
    pub passages: u32,
    pub moi: bool,
}

/// Le nom affiché d'une voix : son nom, « Toi », ou « Voix 3 ».
const NOM_AFFICHE: &str = "CASE WHEN l.id IS NULL THEN NULL WHEN l.nom IS NOT NULL THEN l.nom \
    WHEN l.moi = 1 THEN 'Toi' ELSE 'Voix ' || l.id END";

/// Ce qui a été enregistré un jour donné (heure locale).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BilanJour {
    /// « 2026-11-20 »
    pub jour: String,
    pub images: u64,
    pub images_privees: u64,
    /// Heures où au moins une image a été prise.
    pub heures_actives: u32,
    pub octets_video: u64,
    pub passages_voix: u64,
    pub secondes_voix: u64,
    pub passages_transcrits: u64,
    pub textes_vus: u64,
}

/// Une recherche tapée (reconnue dans l'adresse d'un moteur ou d'un site).
#[derive(Debug, Clone, PartialEq)]
pub struct Recherche {
    pub id: i64,
    pub quand_ms: i64,
    pub moteur: String,
    pub requete: String,
    pub url: Option<String>,
}

/// Un texte vu à l'écran pendant une plage (pour l'export).
#[derive(Debug, Clone, PartialEq)]
pub struct TexteVu {
    pub texte: String,
    /// 0 = accessibilité, 1 = OCR.
    pub source: u8,
    pub vu: bool,
    pub focus: bool,
    pub cadre: [f64; 4],
}

/// Un passage de voix enregistré.
#[derive(Debug, Clone, PartialEq)]
pub struct Voix {
    pub id: i64,
    /// 0 = micro, 1 = son du Mac.
    pub source: u8,
    pub debut_ms: i64,
    pub fin_ms: i64,
    /// Chemin relatif au dossier des données.
    pub fichier: String,
    pub texte: Option<String>,
    /// La voix connue (identifiant) et son nom affiché.
    pub locuteur_id: Option<i64>,
    pub locuteur: Option<String>,
    /// La voix de Julien (« Toi »), même s'il lui a donné un nom.
    pub moi: bool,
    /// Le son du Mac pendant un appel (sans voix connue : « ? ») ; sinon, sans voix : une vidéo.
    pub appel: bool,
}

/// Un passage et son empreinte de voix, pour retrier les voix (dans l'ordre du temps).
#[derive(Debug, Clone, PartialEq)]
pub struct EmpreinteDeVoix {
    pub id: i64,
    /// 0 = micro, 1 = son du Mac.
    pub source: u8,
    pub appel: bool,
    /// En octets (f32 petit-boutiste).
    pub empreinte: Vec<u8>,
    pub locuteur_id: Option<i64>,
}

/// Empreinte stable d'un texte (FNV-1a 64 bits), identique d'une version de Rust à l'autre.
pub fn empreinte(texte: &str) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for octet in texte.as_bytes() {
        h ^= u64::from(*octet);
        h = h.wrapping_mul(0x100000001b3);
    }
    h as i64
}

#[derive(Debug, Clone, PartialEq)]
pub struct NouvelleApparition {
    pub texte: i64,
    pub ecran: u32,
    pub quand_ms: i64,
    pub source: u8,
    pub vu: bool,
    pub focus: bool,
    pub cadre: [f64; 4],
}

/// Un résultat de recherche : un texte, et quand / où il était à l'écran.
#[derive(Debug, Clone, PartialEq)]
pub struct Trouvaille {
    pub texte: String,
    pub ecran: u32,
    pub debut_ms: i64,
    pub fin_ms: i64,
    pub vu: bool,
    pub cadre: [f64; 4],
    /// Plus petit = plus pertinent (BM25).
    pub score: f64,
    /// Les mots cherchés y sont entiers (« bar »), pas seulement des débuts de mots (« barre »).
    pub entier: bool,
    /// L'apparition (pour ne pas compter deux fois la même).
    pub apparition: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NouveauMorceau<'a> {
    pub ecran: u32,
    pub fichier: &'a str,
    pub debut_ms: i64,
    pub largeur: u32,
    pub hauteur: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Morceau {
    pub id: i64,
    pub ecran: u32,
    pub fichier: String,
    pub debut_ms: i64,
    pub fin_ms: Option<i64>,
    pub largeur: u32,
    pub hauteur: u32,
    pub octets: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NouvelleImage<'a> {
    pub morceau: i64,
    pub quand_ms: i64,
    pub position_ms: i64,
    pub ecran: u32,
    pub app_bundle: Option<&'a str>,
    pub app_nom: Option<&'a str>,
    pub aire: f64,
    pub declenchee: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub id: i64,
    pub morceau: i64,
    pub fichier: String,
    pub quand_ms: i64,
    pub position_ms: i64,
    pub ecran: u32,
    pub app_bundle: Option<String>,
    pub app_nom: Option<String>,
    pub aire: f64,
    pub declenchee: bool,
    /// Taille de l'image (celle du morceau vidéo).
    pub largeur: u32,
    pub hauteur: u32,
    /// Titre de la fenêtre et page web à ce moment (phase 3).
    pub fenetre: Option<String>,
    pub url: Option<String>,
    /// Navigation privée : verrouillée dans la frise jusqu'à Touch ID.
    pub privee: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bilan {
    pub images: u64,
    pub morceaux: u64,
    pub octets: u64,
    pub premiere_ms: Option<i64>,
    pub derniere_ms: Option<i64>,
}

/// Les images d'une période (heure, privée ou non), tous écrans confondus, et celle juste avant :
/// de quoi dire vite si un passage de voix touche une navigation privée.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VerrouPrive {
    images: Vec<(i64, bool)>,
}

impl VerrouPrive {
    /// Même règle que [`Base::privee_pendant`] : une image privée pendant la période, ou une
    /// image privée juste avant son début (l'écran était privé quand le passage a commencé).
    pub fn pendant(&self, debut_ms: i64, fin_ms: i64) -> bool {
        let i = self.images.partition_point(|(quand, _)| *quand <= debut_ms);
        let avant = i.checked_sub(1).is_some_and(|j| self.images[j].1);
        avant
            || self.images[i..]
                .iter()
                .take_while(|(quand, _)| *quand <= fin_ms.max(debut_ms))
                .any(|(_, privee)| *privee)
    }
}

pub struct Base {
    connexion: Connection,
}

/// Transforme ce que Julien tape en requête FTS5 sûre : chaque mot entre guillemets,
/// en début de mot (« annec » trouve « Annecy »), tous les mots requis.
pub fn requete_fts(requete: &str) -> Option<String> {
    requete_fts_avec(requete, true)
}

/// Comme [`requete_fts`] ; `debut_de_mot` faux : des mots entiers seulement (« bar », pas « barre »).
pub fn requete_fts_avec(requete: &str, debut_de_mot: bool) -> Option<String> {
    let mots: Vec<String> = mots_de(requete);
    requete_fts_mots(&mots, debut_de_mot)
}

/// Les mots d'une requête (lettres et chiffres).
fn mots_de(requete: &str) -> Vec<String> {
    requete.split(|c: char| !c.is_alphanumeric()).filter(|m| !m.is_empty()).map(str::to_string).collect()
}

fn requete_fts_mots(mots: &[String], debut_de_mot: bool) -> Option<String> {
    let mots: Vec<String> = mots
        .iter()
        .map(|m| format!("\"{}\"{}", m.replace('"', ""), if debut_de_mot { "*" } else { "" }))
        .collect();
    (!mots.is_empty()).then(|| mots.join(" AND "))
}

/// Un mot tel que l'index plein texte le range (minuscules, sans accents : « unicode61
/// remove_diacritics »).
fn comme_l_index(mot: &str) -> String {
    mot.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            autre => autre,
        })
        .collect()
}

/// Au-dessus de cette part des textes, un mot est trop courant pour chercher (« le », « de », « un » :
/// 5 à 14 % des textes en novembre 2026 ; un nom, un objet : moins de 0,5 %).
const MOT_COURANT: f64 = 0.02;

impl Base {
    /// Ouvre (ou crée) la base dans `fichier`.
    pub fn ouvrir(fichier: &Path) -> Result<Self> {
        if let Some(dossier) = fichier.parent() {
            let _ = std::fs::create_dir_all(dossier);
        }
        Self::preparer(Connection::open(fichier)?)
    }

    /// Ouvre la base en lecture seule (serveur MCP, export) : rien ne peut y être écrit.
    pub fn ouvrir_lecture(fichier: &Path) -> Result<Self> {
        use rusqlite::OpenFlags;
        let connexion = Connection::open_with_flags(
            fichier,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
        )?;
        connexion.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { connexion })
    }

    /// Base en mémoire, pour les tests.
    pub fn en_memoire() -> Result<Self> {
        Self::preparer(Connection::open_in_memory()?)
    }

    fn preparer(mut connexion: Connection) -> Result<Self> {
        // WAL : lectures (la frise) et écritures (la capture) en même temps.
        connexion.pragma_update(None, "journal_mode", "WAL")?;
        connexion.pragma_update(None, "synchronous", "NORMAL")?;
        connexion.pragma_update(None, "foreign_keys", true)?;
        // Ce qui est effacé est vraiment effacé : les pages libérées sont mises à zéro.
        connexion.pragma_update(None, "secure_delete", true)?;
        connexion.busy_timeout(std::time::Duration::from_secs(5))?;
        // Toute transaction prend le verrou d'écriture dès son début (BEGIN IMMEDIATE), en
        // attendant son tour. Une transaction différée qui lit avant d'écrire (ranger_sens,
        // remasquer) échouait d'un coup, « database is locked », si la capture écrivait
        // entre-temps : SQLite ne fait pas passer une lecture en écriture et n'attend pas dans
        // ce cas (27 fois le 22/11 dans l'indexation par le sens).
        connexion.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        let version: i32 = connexion.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            connexion.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            connexion.execute_batch(SCHEMA_V2)?;
        }
        if version < 3 {
            connexion.execute_batch(SCHEMA_V3)?;
        }
        if version < 4 {
            connexion.execute_batch(SCHEMA_V4)?;
        }
        if version < 5 {
            connexion.execute_batch(SCHEMA_V5)?;
        }
        if version < 6 {
            connexion.execute_batch(SCHEMA_V6)?;
        }
        if version < 7 {
            connexion.execute_batch(SCHEMA_V7)?;
        }
        if version < 8 {
            connexion.execute_batch(SCHEMA_V8)?;
        }
        if version < 9 {
            connexion.execute_batch(SCHEMA_V9)?;
        }
        if version < 10 {
            connexion.execute_batch(SCHEMA_V10)?;
        }
        if version < 11 {
            connexion.execute_batch(SCHEMA_V11)?;
        }
        if version < 12 {
            connexion.execute_batch(SCHEMA_V12)?;
        }
        if version < 13 {
            connexion.execute_batch(SCHEMA_V13)?;
        }
        if version < 14 {
            connexion.execute_batch(SCHEMA_V14)?;
        }
        if version < 15 {
            let transaction = connexion.transaction()?;
            transaction.execute_batch(SCHEMA_V15)?;
            remplir_codes(&transaction)?;
            transaction.commit()?;
        }
        if version < 16 {
            // La fréquence de chaque mot dans l'index (sans stockage : lue dans l'index plein texte).
            connexion.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS textes_fts_vocab USING fts5vocab('textes_fts', 'row');",
            )?;
        }
        if version < VERSION_SCHEMA {
            connexion.pragma_update(None, "user_version", VERSION_SCHEMA)?;
        }
        // Même chose pour l'index plein texte : un texte effacé n'y laisse aucun mot. Le réglage
        // reste dans la base : on ne l'écrit qu'une fois. L'écrire à chaque ouverture demandait un
        // verrou d'écriture, et la frise attendait jusqu'à 5 s que la capture ait fini d'écrire.
        let deja: bool = connexion
            .query_row("SELECT v = 1 FROM textes_fts_config WHERE k = 'secure-delete'", [], |r| r.get(0))
            .unwrap_or(false);
        if !deja {
            let _ = connexion.execute("INSERT INTO textes_fts(textes_fts, rank) VALUES ('secure-delete', 1)", []);
        }
        Ok(Self { connexion })
    }

    pub fn nouveau_morceau(&self, m: &NouveauMorceau) -> Result<i64> {
        self.connexion.execute(
            "INSERT INTO morceaux (ecran, fichier, debut_ms, largeur, hauteur) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.ecran, m.fichier, m.debut_ms, m.largeur, m.hauteur],
        )?;
        Ok(self.connexion.last_insert_rowid())
    }

    pub fn fermer_morceau(&self, id: i64, fin_ms: i64, octets: u64) -> Result<()> {
        self.connexion.execute(
            "UPDATE morceaux SET fin_ms = ?2, octets = ?3 WHERE id = ?1",
            params![id, fin_ms, octets as i64],
        )?;
        Ok(())
    }

    pub fn ajouter_image(&self, i: &NouvelleImage) -> Result<i64> {
        self.connexion.execute(
            "INSERT INTO images (morceau, quand_ms, position_ms, ecran, app_bundle, app_nom, aire, declenchee)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                i.morceau,
                i.quand_ms,
                i.position_ms,
                i.ecran,
                i.app_bundle,
                i.app_nom,
                i.aire,
                i.declenchee
            ],
        )?;
        Ok(self.connexion.last_insert_rowid())
    }

    /// Les images entre deux heures (incluses), dans l'ordre.
    pub fn images_entre(&self, debut_ms: i64, fin_ms: i64) -> Result<Vec<Image>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT i.id, i.morceau, COALESCE(i.fichier_prive, m.fichier), i.quand_ms, i.position_ms, i.ecran,
                    i.app_bundle, i.app_nom, i.aire, i.declenchee, m.largeur, m.hauteur, i.fenetre, i.url, i.privee
             FROM images i JOIN morceaux m ON m.id = i.morceau
             WHERE i.quand_ms BETWEEN ?1 AND ?2
             ORDER BY i.quand_ms, i.ecran",
        )?;
        let lignes = requete.query_map(params![debut_ms, fin_ms], |r| {
            Ok(Image {
                id: r.get(0)?,
                morceau: r.get(1)?,
                fichier: r.get(2)?,
                quand_ms: r.get(3)?,
                position_ms: r.get(4)?,
                ecran: r.get(5)?,
                app_bundle: r.get(6)?,
                app_nom: r.get(7)?,
                aire: r.get(8)?,
                declenchee: r.get(9)?,
                largeur: r.get(10)?,
                hauteur: r.get(11)?,
                fenetre: r.get(12)?,
                url: r.get(13)?,
                privee: r.get(14)?,
            })
        })?;
        lignes.collect()
    }

    /// Les images d'un écran entre deux heures (incluses), dans l'ordre : ce que montre la frise.
    pub fn images_ecran_entre(&self, ecran: u32, debut_ms: i64, fin_ms: i64) -> Result<Vec<Image>> {
        Ok(self
            .images_entre(debut_ms, fin_ms)?
            .into_iter()
            .filter(|i| i.ecran == ecran)
            .collect())
    }

    /// Les images enregistrées après `apres_id`, dans l'ordre (pour la recherche dans les images,
    /// qui les regarde une à une).
    pub fn images_apres(&self, apres_id: i64, limite: usize) -> Result<Vec<Image>> {
        self.images_ou("i.id > ?1 ORDER BY i.id LIMIT ?2", params![apres_id, limite as i64])
    }

    /// Des images par leur identifiant (celles qui existent encore), dans l'ordre du temps.
    pub fn images_par_id(&self, ids: &[i64]) -> Result<Vec<Image>> {
        let mut images = Vec::with_capacity(ids.len());
        for id in ids {
            images.extend(self.images_ou("i.id = ?1 AND ?2 > 0", params![id, 1])?);
        }
        images.sort_by_key(|i| i.quand_ms);
        Ok(images)
    }

    fn images_ou(&self, condition: &str, valeurs: impl rusqlite::Params) -> Result<Vec<Image>> {
        let mut requete = self.connexion.prepare_cached(&format!(
            "SELECT i.id, i.morceau, COALESCE(i.fichier_prive, m.fichier), i.quand_ms, i.position_ms, i.ecran,
                    i.app_bundle, i.app_nom, i.aire, i.declenchee, m.largeur, m.hauteur, i.fenetre, i.url, i.privee
             FROM images i JOIN morceaux m ON m.id = i.morceau
             WHERE {condition}"
        ))?;
        let lignes = requete.query_map(valeurs, |r| {
            Ok(Image {
                id: r.get(0)?,
                morceau: r.get(1)?,
                fichier: r.get(2)?,
                quand_ms: r.get(3)?,
                position_ms: r.get(4)?,
                ecran: r.get(5)?,
                app_bundle: r.get(6)?,
                app_nom: r.get(7)?,
                aire: r.get(8)?,
                declenchee: r.get(9)?,
                largeur: r.get(10)?,
                hauteur: r.get(11)?,
                fenetre: r.get(12)?,
                url: r.get(13)?,
                privee: r.get(14)?,
            })
        })?;
        lignes.collect()
    }

    /// Range le vecteur d'une image ; une image effacée ou devenue privée entre-temps est ignorée.
    pub fn ranger_vue(&self, image: i64, echelle: f32, vecteur: &[i8]) -> Result<()> {
        let octets: Vec<u8> = vecteur.iter().map(|&x| x as u8).collect();
        let transaction = self.connexion.unchecked_transaction()?;
        let ajoutees = transaction.execute(
            "INSERT OR REPLACE INTO vues (image, echelle, vecteur)
             SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM images WHERE id = ?1 AND privee = 0)",
            params![image, f64::from(echelle), octets],
        )?;
        if ajoutees > 0 {
            transaction.execute(
                "INSERT OR REPLACE INTO codes_vues (id, code) VALUES (?1, ?2)",
                params![transaction.last_insert_rowid(), code_de(&octets)],
            )?;
        }
        transaction.commit()
    }

    /// Range le code des vecteurs qui n'en ont pas (au cas où une version d'avant le schéma 15 en
    /// aurait rangé depuis) ; rend leur nombre.
    pub fn completer_codes(&self) -> Result<usize> {
        let transaction = self.connexion.unchecked_transaction()?;
        let n = completer_codes(&transaction)?;
        transaction.commit()?;
        Ok(n)
    }

    /// Les codes des vecteurs rangés après `apres_id` (`sens` : ceux du sens, sinon des images),
    /// dans l'ordre (pour tenir l'index compact à jour).
    pub fn codes_depuis(&self, sens: bool, apres_id: i64, limite: usize) -> Result<Vec<(i64, Vec<u64>)>> {
        let mut requete = self.connexion.prepare_cached(if sens {
            "SELECT id, code FROM codes_sens WHERE id > ?1 ORDER BY id LIMIT ?2"
        } else {
            "SELECT id, code FROM codes_vues WHERE id > ?1 ORDER BY id LIMIT ?2"
        })?;
        let lignes = requete.query_map(params![apres_id, limite as i64], |r| {
            let octets: Vec<u8> = r.get(1)?;
            Ok((r.get(0)?, bscreen_core::index_compact::depuis_octets(&octets)))
        })?;
        lignes.collect()
    }

    /// Les vecteurs d'images de ces identifiants (ceux qui existent encore).
    pub fn vues_par_ids(&self, ids: &[i64]) -> Result<Vec<Vue>> {
        let mut requete = self.connexion.prepare_cached("SELECT id, image, echelle, vecteur FROM vues WHERE id = ?1")?;
        let mut vues = Vec::with_capacity(ids.len());
        for id in ids {
            let vue = requete
                .query_row(params![id], |r| {
                    let octets: Vec<u8> = r.get(3)?;
                    Ok(Vue {
                        id: r.get(0)?,
                        image: r.get(1)?,
                        echelle: r.get::<_, f64>(2)? as f32,
                        vecteur: octets.into_iter().map(|b| b as i8).collect(),
                    })
                })
                .optional()?;
            vues.extend(vue);
        }
        Ok(vues)
    }

    /// Les vecteurs de sens de ces identifiants (ceux qui existent encore).
    pub fn sens_par_ids(&self, ids: &[i64]) -> Result<Vec<Sens>> {
        let mut requete = self.connexion.prepare_cached("SELECT id, texte, echelle, vecteur FROM sens WHERE id = ?1")?;
        let mut sens = Vec::with_capacity(ids.len());
        for id in ids {
            let s = requete
                .query_row(params![id], |r| {
                    let octets: Vec<u8> = r.get(3)?;
                    Ok(Sens {
                        id: r.get(0)?,
                        texte: r.get(1)?,
                        echelle: r.get::<_, f64>(2)? as f32,
                        vecteur: octets.into_iter().map(|b| b as i8).collect(),
                    })
                })
                .optional()?;
            sens.extend(s);
        }
        Ok(sens)
    }

    /// Les vecteurs d'images rangés après `apres_id` (pour tenir un index en mémoire à jour).
    pub fn vues_depuis(&self, apres_id: i64, limite: usize) -> Result<Vec<Vue>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT id, image, echelle, vecteur FROM vues WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        let lignes = requete.query_map(params![apres_id, limite as i64], |r| {
            let octets: Vec<u8> = r.get(3)?;
            Ok(Vue {
                id: r.get(0)?,
                image: r.get(1)?,
                echelle: r.get::<_, f64>(2)? as f32,
                vecteur: octets.into_iter().map(|b| b as i8).collect(),
            })
        })?;
        lignes.collect()
    }

    /// Combien d'images ont un vecteur.
    pub fn compter_vues(&self) -> Result<u64> {
        self.connexion.query_row("SELECT COUNT(*) FROM vues", [], |r| r.get::<_, i64>(0)).map(|n| n as u64)
    }

    /// Les morceaux commencés avant une heure donnée (pour la purge).
    pub fn morceaux_avant(&self, avant_ms: i64) -> Result<Vec<Morceau>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, ecran, fichier, debut_ms, fin_ms, largeur, hauteur, octets
             FROM morceaux WHERE debut_ms < ?1 ORDER BY debut_ms",
        )?;
        let lignes = requete.query_map(params![avant_ms], |r| {
            Ok(Morceau {
                id: r.get(0)?,
                ecran: r.get(1)?,
                fichier: r.get(2)?,
                debut_ms: r.get(3)?,
                fin_ms: r.get(4)?,
                largeur: r.get(5)?,
                hauteur: r.get(6)?,
                octets: r.get::<_, Option<i64>>(7)?.map(|o| o as u64),
            })
        })?;
        lignes.collect()
    }

    /// Les morceaux jamais fermés (l'app s'est arrêtée pendant leur écriture).
    pub fn morceaux_non_fermes(&self) -> Result<Vec<Morceau>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, ecran, fichier, debut_ms, fin_ms, largeur, hauteur, octets
             FROM morceaux WHERE fin_ms IS NULL ORDER BY debut_ms",
        )?;
        let lignes = requete.query_map([], |r| {
            Ok(Morceau {
                id: r.get(0)?,
                ecran: r.get(1)?,
                fichier: r.get(2)?,
                debut_ms: r.get(3)?,
                fin_ms: r.get(4)?,
                largeur: r.get(5)?,
                hauteur: r.get(6)?,
                octets: r.get::<_, Option<i64>>(7)?.map(|o| o as u64),
            })
        })?;
        lignes.collect()
    }

    /// Retire les images d'un morceau placées après `position_ms` (jamais arrivées sur le disque).
    pub fn supprimer_images_apres(&self, morceau: i64, position_ms: i64) -> Result<usize> {
        self.connexion.execute(
            "DELETE FROM images WHERE morceau = ?1 AND position_ms > ?2",
            params![morceau, position_ms],
        )
    }

    /// L'id d'un texte, ajouté s'il est nouveau.
    pub fn texte_id(&self, contenu: &str) -> Result<i64> {
        let cle = empreinte(contenu);
        self.connexion.execute(
            "INSERT INTO textes (empreinte, contenu) VALUES (?1, ?2) ON CONFLICT(empreinte) DO NOTHING",
            params![cle, contenu],
        )?;
        self.connexion
            .query_row("SELECT id FROM textes WHERE empreinte = ?1", params![cle], |r| r.get(0))
    }

    /// Note de nouvelles apparitions et prolonge celles qui sont toujours à l'écran, en une transaction.
    /// Rend les ids des nouvelles apparitions, dans l'ordre.
    pub fn noter_apparitions(
        &self,
        nouvelles: &[NouvelleApparition],
        prolongees: &[i64],
        quand_ms: i64,
    ) -> Result<Vec<i64>> {
        let transaction = self.connexion.unchecked_transaction()?;
        let mut ids = Vec::with_capacity(nouvelles.len());
        {
            let mut ajout = transaction.prepare_cached(
                "INSERT INTO apparitions (texte, ecran, debut_ms, fin_ms, source, vu, focus, x, y, l, h)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for n in nouvelles {
                ajout.execute(params![
                    n.texte, n.ecran, n.quand_ms, n.source, n.vu, n.focus, n.cadre[0], n.cadre[1], n.cadre[2],
                    n.cadre[3]
                ])?;
                ids.push(transaction.last_insert_rowid());
            }
            let mut prolonge = transaction.prepare_cached("UPDATE apparitions SET fin_ms = ?2 WHERE id = ?1")?;
            for id in prolongees {
                prolonge.execute(params![id, quand_ms])?;
            }
        }
        transaction.commit()?;
        Ok(ids)
    }

    /// Les adresses vues depuis `depuis_ms` (hors moments privés) et leur nombre d'images.
    pub fn adresses_depuis(&self, depuis_ms: i64) -> Result<Vec<(String, i64)>> {
        let mut requete = self.connexion.prepare(
            "SELECT url, count(*) FROM images WHERE url IS NOT NULL AND privee = 0 AND quand_ms >= ?1 GROUP BY url",
        )?;
        let lignes = requete.query_map(params![depuis_ms], |r| Ok((r.get(0)?, r.get(1)?)))?;
        lignes.collect()
    }

    /// Repousse l'heure de fin d'apparitions (id, fin), en une transaction.
    pub fn prolonger_apparitions(&self, lot: &[(i64, i64)]) -> Result<()> {
        let transaction = self.connexion.unchecked_transaction()?;
        {
            let mut prolonge =
                transaction.prepare_cached("UPDATE apparitions SET fin_ms = max(fin_ms, ?2) WHERE id = ?1")?;
            for (id, fin) in lot {
                prolonge.execute(params![id, fin])?;
            }
        }
        transaction.commit()
    }

    /// Le texte visible à l'écran à un moment donné, dans l'ordre de lecture (haut en bas, gauche à droite).
    pub fn texte_visible(&self, ecran: u32, quand_ms: i64) -> Result<Vec<String>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT t.contenu FROM apparitions a JOIN textes t ON t.id = a.texte
             WHERE a.ecran = ?1 AND a.debut_ms <= ?2 AND a.fin_ms >= ?2 AND a.vu = 1
             ORDER BY round(a.y / 8), a.x",
        )?;
        let mut lignes: Vec<String> = Vec::new();
        for ligne in requete.query_map(params![ecran, quand_ms], |r| r.get::<_, String>(0))? {
            let ligne = ligne?;
            if !lignes.contains(&ligne) {
                lignes.push(ligne);
            }
        }
        Ok(lignes)
    }

    /// Les textes visibles à un moment (à `tolerance_ms` près : la lecture suit l'image de peu),
    /// avec leur place, dans l'ordre de lecture, une fois chacun : pour lire une conversation
    /// (qui a écrit quoi, d'après le côté de la bulle).
    pub fn textes_vus_a(&self, ecran: u32, quand_ms: i64, tolerance_ms: i64) -> Result<Vec<TexteVu>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT t.contenu, a.source, a.vu, a.focus, a.x, a.y, a.l, a.h
             FROM apparitions a JOIN textes t ON t.id = a.texte
             WHERE a.ecran = ?1 AND a.debut_ms <= ?2 + ?3 AND a.fin_ms >= ?2 - ?3 AND a.vu = 1
             ORDER BY round(a.y / 8), a.x",
        )?;
        let mut textes: Vec<TexteVu> = Vec::new();
        for t in requete.query_map(params![ecran, quand_ms, tolerance_ms], |r| {
            Ok(TexteVu {
                texte: r.get(0)?,
                source: r.get(1)?,
                vu: r.get(2)?,
                focus: r.get(3)?,
                cadre: [
                    r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                ],
            })
        })? {
            let t = t?;
            if !textes.iter().any(|deja| deja.texte == t.texte) {
                textes.push(t);
            }
        }
        Ok(textes)
    }

    /// Les titres de fenêtre des apps dont l'identifiant commence par l'un de `apps`, pendant une
    /// période, hors navigation privée : une fois chacun, (identifiant de l'app, titre).
    pub fn titres_de_fenetres(&self, apps: &[&str], debut_ms: i64, fin_ms: i64) -> Result<Vec<(String, String)>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT DISTINCT app_bundle, fenetre FROM images
             WHERE quand_ms BETWEEN ?1 AND ?2 AND privee = 0 AND fenetre IS NOT NULL AND app_bundle IS NOT NULL",
        )?;
        let lignes = requete.query_map(params![debut_ms, fin_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut titres = Vec::new();
        for ligne in lignes {
            let (app, titre) = ligne?;
            if apps.iter().any(|a| app.starts_with(a)) && !titre.trim().is_empty() {
                titres.push((app, titre));
            }
        }
        Ok(titres)
    }

    /// Les moments où une fenêtre (ce titre, dans l'une de `apps`) était devant, pendant une
    /// période, hors navigation privée : (heure, écran), dans l'ordre.
    pub fn images_de_fenetre(&self, apps: &[&str], titre: &str, debut_ms: i64, fin_ms: i64) -> Result<Vec<(i64, u32)>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT quand_ms, ecran, app_bundle FROM images
             WHERE quand_ms BETWEEN ?1 AND ?2 AND privee = 0 AND fenetre = ?3 ORDER BY quand_ms",
        )?;
        let lignes = requete.query_map(params![debut_ms, fin_ms, titre], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        let mut images = Vec::new();
        for ligne in lignes {
            let (quand, ecran, app) = ligne?;
            if app.is_some_and(|app| apps.iter().any(|a| app.starts_with(a))) {
                images.push((quand, ecran));
            }
        }
        Ok(images)
    }

    /// Fenêtre et URL d'une image.
    /// Les images d'un écran prises en navigation privée, de juste après `apres_ms` à `jusqu_ms` :
    /// verrouillées, et sans titre ni adresse gardés.
    pub fn noter_privee(&self, ecran: u32, apres_ms: i64, jusqu_ms: i64) -> Result<usize> {
        // Ce qu'on en avait vu (le vecteur de l'image) part aussi.
        self.connexion.execute(
            "DELETE FROM vues WHERE image IN
             (SELECT id FROM images WHERE ecran = ?1 AND quand_ms > ?2 AND quand_ms <= ?3)",
            params![ecran, apres_ms, jusqu_ms],
        )?;
        self.connexion.execute(
            "UPDATE images SET privee = 1, fenetre = NULL, url = NULL
             WHERE ecran = ?1 AND quand_ms > ?2 AND quand_ms <= ?3",
            params![ecran, apres_ms, jusqu_ms],
        )
    }

    /// Verrouille (comme la navigation privée) les images de ces apps prises pendant la période :
    /// ce qu'on n'a pas pu vérifier (la lecture du texte bloquée) est gardé hors de vue par
    /// précaution. Rend le nombre d'images verrouillées.
    pub fn noter_privee_apps(&self, apres_ms: i64, jusqu_ms: i64, apps: &[&str]) -> Result<usize> {
        let transaction = self.connexion.unchecked_transaction()?;
        let mut total = 0;
        for app in apps {
            transaction.execute(
                "DELETE FROM vues WHERE image IN
                 (SELECT id FROM images WHERE app_bundle = ?3 AND privee = 0 AND quand_ms > ?1 AND quand_ms <= ?2)",
                params![apres_ms, jusqu_ms, app],
            )?;
            total += transaction.execute(
                "UPDATE images SET privee = 1, fenetre = NULL, url = NULL
                 WHERE app_bundle = ?3 AND privee = 0 AND quand_ms > ?1 AND quand_ms <= ?2",
                params![apres_ms, jusqu_ms, app],
            )?;
        }
        transaction.commit()?;
        Ok(total)
    }

    /// Combien d'images de ces apps la période compte, pas encore verrouillées (essai à blanc).
    pub fn compter_images_apps(&self, apres_ms: i64, jusqu_ms: i64, apps: &[&str]) -> Result<usize> {
        let mut total = 0;
        for app in apps {
            total += self.connexion.query_row(
                "SELECT COUNT(*) FROM images WHERE app_bundle = ?3 AND privee = 0 AND quand_ms > ?1 AND quand_ms <= ?2",
                params![apres_ms, jusqu_ms, app],
                |r| r.get::<_, i64>(0),
            )? as usize;
        }
        Ok(total)
    }

    pub fn noter_fenetre(&self, image: i64, fenetre: Option<&str>, url: Option<&str>) -> Result<()> {
        self.connexion.execute(
            "UPDATE images SET fenetre = ?2, url = ?3 WHERE id = ?1",
            params![image, fenetre, url],
        )?;
        Ok(())
    }

    /// Un passage de voix vient d'être enregistré (pas encore transcrit), avec son empreinte
    /// de voix et la voix connue à laquelle il est rattaché, si on les a.
    pub fn ajouter_voix(
        &self,
        source: u8,
        debut_ms: i64,
        fin_ms: i64,
        fichier: &str,
        empreinte: Option<&[u8]>,
        locuteur_id: Option<i64>,
    ) -> Result<i64> {
        self.ajouter_voix_avec(source, debut_ms, fin_ms, fichier, empreinte, locuteur_id, false)
    }

    /// Comme [`Self::ajouter_voix`] ; `appel` : le son du Mac pendant un appel.
    #[allow(clippy::too_many_arguments)]
    pub fn ajouter_voix_avec(
        &self,
        source: u8,
        debut_ms: i64,
        fin_ms: i64,
        fichier: &str,
        empreinte: Option<&[u8]>,
        locuteur_id: Option<i64>,
        appel: bool,
    ) -> Result<i64> {
        let ajout = "INSERT INTO voix (source, debut_ms, fin_ms, fichier, empreinte, locuteur_id, appel)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
        match self
            .connexion
            .execute(ajout, params![source, debut_ms, fin_ms, fichier, empreinte, locuteur_id, appel])
        {
            // La voix vient d'être effacée (un tri des voix pendant ce passage) : le passage est
            // gardé, sans voix, plutôt que perdu.
            Err(Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY && locuteur_id.is_some() =>
            {
                self.connexion
                    .execute(ajout, params![source, debut_ms, fin_ms, fichier, empreinte, None::<i64>, appel])?;
            }
            autre => {
                autre?;
            }
        }
        Ok(self.connexion.last_insert_rowid())
    }

    /// Retient une correction de Julien (le même fait n'est gardé qu'une fois). Rend son identifiant.
    pub fn ajouter_correction(&self, cree_ms: i64, texte: &str, question: Option<&str>) -> Result<i64> {
        let deja: Option<i64> = self
            .connexion
            .query_row("SELECT id FROM corrections WHERE texte = ?1", params![texte], |r| r.get(0))
            .optional()?;
        if let Some(id) = deja {
            return Ok(id);
        }
        self.connexion.execute(
            "INSERT INTO corrections (cree_ms, texte, question) VALUES (?1, ?2, ?3)",
            params![cree_ms, texte, question],
        )?;
        Ok(self.connexion.last_insert_rowid())
    }

    /// Les corrections retenues, les plus récentes d'abord.
    pub fn corrections(&self, limite: usize) -> Result<Vec<Correction>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, cree_ms, texte, question FROM corrections ORDER BY cree_ms DESC, id DESC LIMIT ?1",
        )?;
        requete
            .query_map(params![limite as i64], |r| {
                Ok(Correction {
                    id: r.get(0)?,
                    cree_ms: r.get(1)?,
                    texte: r.get(2)?,
                    question: r.get(3)?,
                })
            })?
            .collect()
    }

    /// Oublie une correction ; rend faux si elle n'existait pas.
    pub fn oublier_correction(&self, id: i64) -> Result<bool> {
        Ok(self.connexion.execute("DELETE FROM corrections WHERE id = ?1", params![id])? > 0)
    }

    /// L'empreinte de voix d'un passage (en octets), s'il en a une.
    pub fn empreinte_du_passage(&self, passage: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .connexion
            .query_row("SELECT empreinte FROM voix WHERE id = ?1", params![passage], |r| r.get::<_, Option<Vec<u8>>>(0))
            .optional()?
            .flatten())
    }

    /// Range un seul passage dans une autre voix (ou aucune : « ? ») — une correction de Julien.
    /// L'ancienne voix perd ce passage ; anonyme et vide, elle disparaît. Rend l'ancienne voix.
    pub fn deplacer_passage(&self, passage: i64, vers: Option<i64>) -> Result<Option<i64>> {
        let transaction = self.connexion.unchecked_transaction()?;
        let ancienne: Option<i64> = transaction
            .query_row("SELECT locuteur_id FROM voix WHERE id = ?1", params![passage], |r| r.get(0))
            .optional()?
            .flatten();
        transaction.execute("UPDATE voix SET locuteur_id = ?2 WHERE id = ?1", params![passage, vers])?;
        if let Some(ancienne) = ancienne.filter(|a| Some(*a) != vers) {
            transaction.execute(
                "UPDATE locuteurs SET passages = MAX(passages - 1, 0) WHERE id = ?1",
                params![ancienne],
            )?;
            transaction.execute(
                "DELETE FROM locuteurs WHERE id = ?1 AND nom IS NULL AND moi = 0
                 AND NOT EXISTS (SELECT 1 FROM voix WHERE locuteur_id = ?1)",
                params![ancienne],
            )?;
        }
        transaction.commit()?;
        Ok(ancienne)
    }

    /// Tous les passages qui ont une empreinte de voix, dans l'ordre du temps (pour les retrier).
    pub fn empreintes_de_voix(&self) -> Result<Vec<EmpreinteDeVoix>> {
        let mut requete = self.connexion.prepare(
            "SELECT id, source, appel, empreinte, locuteur_id FROM voix WHERE empreinte IS NOT NULL ORDER BY debut_ms, id",
        )?;
        requete
            .query_map([], |r| {
                Ok(EmpreinteDeVoix {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    appel: r.get(2)?,
                    empreinte: r.get(3)?,
                    locuteur_id: r.get(4)?,
                })
            })?
            .collect()
    }

    /// Applique d'un coup un tri des voix (`bscreen retrier-voix`) :
    /// - `noms` : les voix à nommer ;
    /// - `places` : la voix de chaque passage (`None` : aucune, une vidéo ou « ? » ; un nombre
    ///   négatif −k : la k-ième voix de `nouvelles`) ;
    /// - `gardees` : les voix qui restent, avec leur nouveau centre et leur nombre de passages ;
    /// - `nouvelles` : les voix à créer (centre, passages) ;
    /// - `effacees` : les voix qui disparaissent.
    ///
    /// L'état d'avant est d'abord copié dans la base (`voix_avant_tri`, `locuteurs_avant_tri`),
    /// pour [`Self::annuler_tri_des_voix`].
    pub fn appliquer_tri_des_voix(
        &self,
        noms: &[(i64, String)],
        places: &[(i64, Option<i64>)],
        gardees: &[(i64, Vec<u8>, u32)],
        nouvelles: &[(Vec<u8>, u32)],
        effacees: &[i64],
    ) -> Result<()> {
        let transaction = self.connexion.unchecked_transaction()?;
        transaction.execute_batch(
            "DROP TABLE IF EXISTS voix_avant_tri;
             DROP TABLE IF EXISTS locuteurs_avant_tri;
             CREATE TABLE voix_avant_tri AS SELECT id, locuteur_id FROM voix;
             CREATE TABLE locuteurs_avant_tri AS SELECT * FROM locuteurs;",
        )?;
        let mut ids_nouvelles = Vec::with_capacity(nouvelles.len());
        for (centre, passages) in nouvelles {
            transaction.execute(
                "INSERT INTO locuteurs (centre, passages, moi) VALUES (?1, ?2, 0)",
                params![centre, passages],
            )?;
            ids_nouvelles.push(transaction.last_insert_rowid());
        }
        {
            let mut maj = transaction.prepare("UPDATE voix SET locuteur_id = ?2 WHERE id = ?1")?;
            for (passage, voix) in places {
                let voix = match voix {
                    Some(k) if *k < 0 => Some(ids_nouvelles[(-k - 1) as usize]),
                    autre => *autre,
                };
                maj.execute(params![passage, voix])?;
            }
            let mut centre = transaction.prepare("UPDATE locuteurs SET centre = ?2, passages = ?3 WHERE id = ?1")?;
            for (id, c, passages) in gardees {
                centre.execute(params![id, c, passages])?;
            }
            let mut nom = transaction.prepare("UPDATE locuteurs SET nom = ?2 WHERE id = ?1")?;
            for (id, n) in noms {
                nom.execute(params![id, n])?;
            }
            // Un passage sans empreinte, ou arrivé pendant le tri, peut encore viser une voix effacée.
            let mut detacher = transaction.prepare("UPDATE voix SET locuteur_id = NULL WHERE locuteur_id = ?1")?;
            let mut effacer = transaction.prepare("DELETE FROM locuteurs WHERE id = ?1")?;
            for id in effacees {
                detacher.execute(params![id])?;
                effacer.execute(params![id])?;
            }
        }
        transaction.commit()
    }

    /// Remet les voix comme avant le dernier tri. Les passages enregistrés depuis gardent leur
    /// voix si elle existe encore. Rend faux s'il n'y a pas de tri à annuler.
    pub fn annuler_tri_des_voix(&self) -> Result<bool> {
        let existe: bool = self.connexion.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'locuteurs_avant_tri')",
            [],
            |r| r.get(0),
        )?;
        if !existe {
            return Ok(false);
        }
        let transaction = self.connexion.unchecked_transaction()?;
        transaction.execute_batch(
            "CREATE TEMP TABLE voix_maintenant AS SELECT id, locuteur_id FROM voix;
             UPDATE voix SET locuteur_id = NULL;
             DELETE FROM locuteurs;
             INSERT INTO locuteurs SELECT * FROM locuteurs_avant_tri;
             UPDATE voix SET locuteur_id = (SELECT a.locuteur_id FROM voix_avant_tri a WHERE a.id = voix.id)
                 WHERE id IN (SELECT id FROM voix_avant_tri);
             UPDATE voix SET locuteur_id = (SELECT m.locuteur_id FROM voix_maintenant m WHERE m.id = voix.id)
                 WHERE id NOT IN (SELECT id FROM voix_avant_tri)
                   AND (SELECT m.locuteur_id FROM voix_maintenant m WHERE m.id = voix.id) IN (SELECT id FROM locuteurs);
             DROP TABLE voix_maintenant;
             DROP TABLE voix_avant_tri;
             DROP TABLE locuteurs_avant_tri;",
        )?;
        transaction.commit()?;
        Ok(true)
    }

    fn voix_depuis(r: &rusqlite::Row<'_>) -> Result<Voix> {
        Ok(Voix {
            id: r.get(0)?,
            source: r.get(1)?,
            debut_ms: r.get(2)?,
            fin_ms: r.get(3)?,
            fichier: r.get(4)?,
            texte: r.get(5)?,
            locuteur_id: r.get(6)?,
            locuteur: r.get(7)?,
            moi: r.get(8)?,
            appel: r.get(9)?,
        })
    }

    /// Toutes les voix connues.
    pub fn locuteurs(&self) -> Result<Vec<Locuteur>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT id, nom, centre, passages, moi FROM locuteurs ORDER BY id")?;
        requete
            .query_map([], |r| {
                Ok(Locuteur {
                    id: r.get(0)?,
                    nom: r.get(1)?,
                    centre: r.get(2)?,
                    passages: r.get(3)?,
                    moi: r.get(4)?,
                })
            })?
            .collect()
    }

    /// Un passage où l'on entend bien cette voix (pour la réécouter avant de la nommer) :
    /// le plus long des passages compris, jamais pendant une navigation privée.
    /// Rend (fichier, début, fin).
    pub fn passage_exemple(&self, locuteur_id: i64) -> Result<Option<(String, i64, i64)>> {
        let candidats: Vec<(String, i64, i64)> = {
            let mut requete = self.connexion.prepare_cached(
                "SELECT fichier, debut_ms, fin_ms FROM voix WHERE locuteur_id = ?1
                 ORDER BY (texte IS NOT NULL AND texte != '') DESC, fin_ms - debut_ms DESC LIMIT 8",
            )?;
            requete
                .query_map(params![locuteur_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_>>()?
        };
        for (fichier, debut, fin) in candidats {
            if !self.privee_pendant(debut, fin)? {
                return Ok(Some((fichier, debut, fin)));
            }
        }
        Ok(None)
    }

    /// Où en est la recherche par le sens : (textes compris, textes en attente après `apres_id`).
    pub fn compter_sens(&self, apres_id: i64) -> Result<(u64, u64)> {
        let compris: i64 = self
            .connexion
            .query_row("SELECT COUNT(DISTINCT texte) FROM sens", [], |r| r.get(0))?;
        let en_attente: i64 =
            self.connexion
                .query_row("SELECT COUNT(*) FROM textes WHERE id > ?1", params![apres_id], |r| r.get(0))?;
        Ok((compris as u64, en_attente as u64))
    }

    pub fn nouveau_locuteur(&self, centre: &[u8], moi: bool) -> Result<i64> {
        self.connexion.execute(
            "INSERT INTO locuteurs (centre, passages, moi) VALUES (?1, 1, ?2)",
            params![centre, moi],
        )?;
        Ok(self.connexion.last_insert_rowid())
    }

    pub fn maj_locuteur(&self, id: i64, centre: &[u8], passages: u32) -> Result<()> {
        self.connexion.execute(
            "UPDATE locuteurs SET centre = ?2, passages = ?3 WHERE id = ?1",
            params![id, centre, passages],
        )?;
        Ok(())
    }

    /// Donne un nom à une voix. Si une autre voix porte déjà ce nom, les deux n'en font plus qu'une
    /// (ses passages lui sont rattachés) ; rend l'identifiant de la voix qui reste.
    pub fn nommer_locuteur(&self, id: i64, nom: &str) -> Result<i64> {
        let nom = nom.trim();
        let meme: Option<i64> = self
            .connexion
            .query_row(
                "SELECT id FROM locuteurs WHERE nom = ?1 AND id != ?2 ORDER BY passages DESC LIMIT 1",
                params![nom, id],
                |r| r.get(0),
            )
            .optional()?;
        let transaction = self.connexion.unchecked_transaction()?;
        let garde = match meme {
            Some(garde) => {
                transaction.execute("UPDATE voix SET locuteur_id = ?1 WHERE locuteur_id = ?2", params![garde, id])?;
                transaction.execute(
                    "UPDATE locuteurs SET passages = passages + (SELECT passages FROM locuteurs WHERE id = ?2)
                     WHERE id = ?1",
                    params![garde, id],
                )?;
                transaction.execute("DELETE FROM locuteurs WHERE id = ?1", params![id])?;
                garde
            }
            None => {
                transaction.execute("UPDATE locuteurs SET nom = ?2 WHERE id = ?1", params![id, nom])?;
                id
            }
        };
        transaction.commit()?;
        Ok(garde)
    }

    /// Les passages pas encore transcrits, les plus anciens d'abord.
    pub fn voix_a_transcrire(&self, limite: usize) -> Result<Vec<Voix>> {
        let mut requete = self.connexion.prepare_cached(
            &format!(
                "SELECT v.id, v.source, v.debut_ms, v.fin_ms, v.fichier, v.texte, v.locuteur_id, {NOM_AFFICHE},
                        COALESCE(l.moi, 0), v.appel
                 FROM voix v LEFT JOIN locuteurs l ON l.id = v.locuteur_id
                 WHERE v.texte IS NULL ORDER BY v.id LIMIT ?1"
            ),
        )?;
        requete
            .query_map(params![limite as i64], Self::voix_depuis)?
            .collect()
    }

    /// Les fichiers à archiver, terminés avant `avant_ms` et pas encore archivés : les vidéos (morceaux
    /// fermés ; pas les fichiers privés, chiffrés à part) et la voix, dans l'ordre du temps.
    pub fn fichiers_a_archiver(&self, avant_ms: i64) -> Result<Vec<AArchiver>> {
        let jour = |fichier: &str| fichier.split('/').nth(1).unwrap_or_default().to_string();
        let mut sortie = Vec::new();
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, fichier FROM morceaux
             WHERE fin_ms IS NOT NULL AND fin_ms < ?1 AND fichier LIKE 'video/%'
               AND fichier NOT IN (SELECT fichier FROM archives)
             ORDER BY debut_ms",
        )?;
        for ligne in requete.query_map(params![avant_ms], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, fichier) = ligne?;
            sortie.push(AArchiver { jour: jour(&fichier), fichier, morceau: Some(id) });
        }
        let mut requete = self.connexion.prepare_cached(
            "SELECT DISTINCT fichier FROM voix
             WHERE fin_ms < ?1 AND fichier LIKE 'audio/%' AND fichier NOT IN (SELECT fichier FROM archives)
             ORDER BY debut_ms",
        )?;
        for ligne in requete.query_map(params![avant_ms], |r| r.get::<_, String>(0))? {
            let fichier = ligne?;
            sortie.push(AArchiver { jour: jour(&fichier), fichier, morceau: None });
        }
        Ok(sortie)
    }

    /// Ce fichier est parti sur l'archive (taille et empreinte du fichier chiffré).
    pub fn noter_archive(&self, fichier: &str, octets: u64, empreinte: &str, quand_ms: i64) -> Result<()> {
        self.connexion.execute(
            "INSERT OR REPLACE INTO archives (fichier, octets, empreinte, quand_ms) VALUES (?1, ?2, ?3, ?4)",
            params![fichier, octets as i64, empreinte, quand_ms],
        )?;
        Ok(())
    }

    /// L'empreinte du fichier archivé, s'il l'est.
    pub fn archive_de(&self, fichier: &str) -> Result<Option<(u64, String)>> {
        self.connexion
            .query_row("SELECT octets, empreinte FROM archives WHERE fichier = ?1", params![fichier], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
            })
            .optional()
    }

    /// Le fichier est revenu sur le Mac (pour en effacer des images) : plus archivé.
    pub fn oublier_archive(&self, fichier: &str) -> Result<()> {
        self.connexion.execute("DELETE FROM archives WHERE fichier = ?1", params![fichier])?;
        Ok(())
    }

    /// Combien de fichiers archivés, et leur place (chiffrés).
    pub fn bilan_archive(&self) -> Result<(u64, u64)> {
        self.connexion.query_row("SELECT COUNT(*), COALESCE(SUM(octets), 0) FROM archives", [], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
        })
    }

    /// Les images d'un morceau qui auront une vignette : celles regardées par la recherche dans les
    /// images (une par scène), et au moins une par minute. (image, position, heure)
    pub fn images_pour_vignettes(&self, morceau: i64) -> Result<Vec<(i64, i64, i64)>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT i.id, i.position_ms, i.quand_ms, EXISTS(SELECT 1 FROM vues v WHERE v.image = i.id)
             FROM images i WHERE i.morceau = ?1 AND i.privee = 0 ORDER BY i.position_ms",
        )?;
        let mut sortie = Vec::new();
        let mut minute = i64::MIN;
        for ligne in requete.query_map(params![morceau], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, bool>(3)?))
        })? {
            let (id, position, quand, scene) = ligne?;
            if scene || quand / 60_000 != minute {
                minute = quand / 60_000;
                sortie.push((id, position, quand));
            }
        }
        Ok(sortie)
    }

    pub fn ajouter_vignette(&self, image: i64, morceau: i64, position_ms: i64, fichier: &str) -> Result<()> {
        self.connexion.execute(
            "INSERT OR REPLACE INTO vignettes (image, morceau, position_ms, fichier)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM images WHERE id = ?1)",
            params![image, morceau, position_ms, fichier],
        )?;
        Ok(())
    }

    /// La vignette la plus proche d'un moment d'une vidéo archivée (son fichier, sa position).
    pub fn vignette_proche(&self, fichier_video: &str, position_ms: i64) -> Result<Option<String>> {
        self.connexion
            .query_row(
                "SELECT v.fichier FROM vignettes v JOIN morceaux m ON m.id = v.morceau
                 WHERE m.fichier = ?1
                 ORDER BY (v.position_ms > ?2), abs(v.position_ms - ?2) LIMIT 1",
                params![fichier_video, position_ms],
                |r| r.get(0),
            )
            .optional()
    }

    /// Oublie les vignettes d'un morceau (rend leurs fichiers, à effacer).
    pub fn oublier_vignettes_du_morceau(&self, morceau: i64) -> Result<Vec<String>> {
        let fichiers: Vec<String> = {
            let mut requete = self.connexion.prepare("SELECT fichier FROM vignettes WHERE morceau = ?1")?;
            requete.query_map(params![morceau], |r| r.get(0))?.collect::<Result<_>>()?
        };
        self.connexion.execute("DELETE FROM vignettes WHERE morceau = ?1", params![morceau])?;
        Ok(fichiers)
    }

    /// Les fichiers de vignettes de ces images (à effacer avec elles).
    pub fn vignettes_des_images(&self, ids: &[i64]) -> Result<Vec<String>> {
        let mut requete = self.connexion.prepare_cached("SELECT fichier FROM vignettes WHERE image = ?1")?;
        let mut sortie = Vec::new();
        for id in ids {
            if let Some(f) = requete.query_row(params![id], |r| r.get::<_, String>(0)).optional()? {
                sortie.push(f);
            }
        }
        Ok(sortie)
    }

    /// Les passages transcrits d'une plage pas encore relus pour les sous-entendus, dans l'ordre.
    pub fn passages_a_relire(&self, depuis_ms: i64, jusqu_ms: i64) -> Result<Vec<Voix>> {
        let mut requete = self.connexion.prepare_cached(&format!(
            "SELECT v.id, v.source, v.debut_ms, v.fin_ms, v.fichier, v.texte, v.locuteur_id, {NOM_AFFICHE},
                    COALESCE(l.moi, 0), v.appel
             FROM voix v LEFT JOIN locuteurs l ON l.id = v.locuteur_id
             WHERE v.relue = 0 AND v.texte IS NOT NULL AND v.debut_ms >= ?1 AND v.fin_ms <= ?2
             ORDER BY v.debut_ms"
        ))?;
        requete.query_map(params![depuis_ms, jusqu_ms], Self::voix_depuis)?.collect()
    }

    /// Ces passages sont relus (qu'il y ait eu quelque chose à déduire ou non).
    pub fn noter_relus(&self, ids: &[i64]) -> Result<()> {
        let transaction = self.connexion.unchecked_transaction()?;
        {
            let mut requete = transaction.prepare_cached("UPDATE voix SET relue = 1 WHERE id = ?1")?;
            for id in ids {
                requete.execute(params![id])?;
            }
        }
        transaction.commit()
    }

    /// Range la version explicite d'un passage ; elle entre aussi dans la recherche (« écran » 0,
    /// source 3 = déduit), pour la durée du passage. Rien si le passage a été effacé entre-temps.
    pub fn ajouter_deduit(&self, voix: &Voix, texte: &str, confiance: f32, modele: &str, cree_ms: i64) -> Result<()> {
        let transaction = self.connexion.unchecked_transaction()?;
        let existe: bool =
            transaction.query_row("SELECT EXISTS(SELECT 1 FROM voix WHERE id = ?1)", params![voix.id], |r| r.get(0))?;
        if existe {
            transaction.execute(
                "INSERT INTO deduits (voix, texte, confiance, modele, cree_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![voix.id, texte, f64::from(confiance), modele, cree_ms],
            )?;
            let cle = empreinte(texte);
            transaction.execute(
                "INSERT INTO textes (empreinte, contenu) VALUES (?1, ?2) ON CONFLICT(empreinte) DO NOTHING",
                params![cle, texte],
            )?;
            let texte_id: i64 =
                transaction.query_row("SELECT id FROM textes WHERE empreinte = ?1", params![cle], |r| r.get(0))?;
            transaction.execute(
                "INSERT INTO apparitions (texte, ecran, debut_ms, fin_ms, source, vu, focus)
                 VALUES (?1, 0, ?2, ?3, 3, 1, ?4)",
                params![texte_id, voix.debut_ms, voix.fin_ms, voix.source == 0],
            )?;
        }
        transaction.commit()
    }

    /// La version explicite de chaque passage qui en a une (la plus sûre) : passage → (texte, confiance).
    pub fn deduits_de(&self, voix: &[i64]) -> Result<HashMap<i64, (String, f32)>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT texte, confiance FROM deduits WHERE voix = ?1 ORDER BY confiance DESC LIMIT 1")?;
        let mut sortie = HashMap::new();
        for &id in voix {
            if let Some(d) = requete
                .query_row(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)? as f32)))
                .optional()?
            {
                sortie.insert(id, d);
            }
        }
        Ok(sortie)
    }

    /// Les passages qui chevauchent une plage de temps, dans l'ordre.
    pub fn voix_entre(&self, debut_ms: i64, fin_ms: i64) -> Result<Vec<Voix>> {
        let mut requete = self.connexion.prepare_cached(
            &format!(
                "SELECT v.id, v.source, v.debut_ms, v.fin_ms, v.fichier, v.texte, v.locuteur_id, {NOM_AFFICHE},
                        COALESCE(l.moi, 0), v.appel
                 FROM voix v LEFT JOIN locuteurs l ON l.id = v.locuteur_id
                 WHERE v.debut_ms < ?2 AND v.fin_ms > ?1 ORDER BY v.debut_ms"
            ),
        )?;
        requete
            .query_map(params![debut_ms, fin_ms], Self::voix_depuis)?
            .collect()
    }

    /// Range la transcription d'un passage ; le texte entre aussi dans la recherche
    /// (« écran » 0, source 2 = voix), pour la durée du passage.
    pub fn noter_transcription(&self, voix: &Voix, texte: &str, mots_json: &str, modele: &str) -> Result<()> {
        self.connexion.execute(
            "UPDATE voix SET texte = ?2, mots = ?3, modele = ?4 WHERE id = ?1",
            params![voix.id, texte, mots_json, modele],
        )?;
        if texte.trim().is_empty() {
            return Ok(());
        }
        let texte_id = self.texte_id(texte)?;
        self.connexion.execute(
            "INSERT INTO apparitions (texte, ecran, debut_ms, fin_ms, source, vu, focus)
             VALUES (?1, 0, ?2, ?3, 2, 1, ?4)",
            params![texte_id, voix.debut_ms, voix.fin_ms, voix.source == 0],
        )?;
        Ok(())
    }

    /// Oublie tout ce qui a été lu, entendu ou cherché pendant une période (effacement demandé) :
    /// textes affichés ou dits pendant ce temps, passages de voix, recherches tapées.
    /// Rend les fichiers audio à effacer. Les images, elles, passent par l'effacement des vidéos.
    pub fn oublier_periode(&self, debut_ms: i64, fin_ms: i64) -> Result<Vec<String>> {
        let transaction = self.connexion.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM apparitions WHERE debut_ms <= ?2 AND fin_ms >= ?1",
            params![debut_ms, fin_ms],
        )?;
        let fichiers: Vec<String> = {
            let mut requete = transaction.prepare("SELECT fichier FROM voix WHERE debut_ms <= ?2 AND fin_ms >= ?1")?;
            requete
                .query_map(params![debut_ms, fin_ms], |r| r.get(0))?
                .collect::<Result<_>>()?
        };
        transaction.execute(
            "DELETE FROM voix WHERE debut_ms <= ?2 AND fin_ms >= ?1",
            params![debut_ms, fin_ms],
        )?;
        transaction.execute(
            "DELETE FROM recherches WHERE quand_ms BETWEEN ?1 AND ?2",
            params![debut_ms, fin_ms],
        )?;
        // Les résumés qui parlaient de cette période partent aussi (refaits sans elle la nuit suivante).
        transaction.execute(
            "DELETE FROM episodes WHERE debut_ms <= ?2 AND fin_ms >= ?1",
            params![debut_ms, fin_ms],
        )?;
        transaction.execute(
            "DELETE FROM textes WHERE id NOT IN (SELECT DISTINCT texte FROM apparitions)",
            [],
        )?;
        transaction.commit()?;
        Ok(fichiers)
    }

    /// Note un épisode résumé ; rend son identifiant.
    pub fn ajouter_episode(&self, debut_ms: i64, fin_ms: i64, titre: &str, resume: &str, modele: &str, cree_ms: i64) -> Result<i64> {
        self.connexion.execute(
            "INSERT INTO episodes (debut_ms, fin_ms, titre, resume, modele, cree_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![debut_ms, fin_ms, titre, resume, modele, cree_ms],
        )?;
        Ok(self.connexion.last_insert_rowid())
    }

    /// Les épisodes résumés qui touchent une période, dans l'ordre.
    pub fn episodes_entre(&self, debut_ms: i64, fin_ms: i64) -> Result<Vec<Episode>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, debut_ms, fin_ms, titre, resume, modele FROM episodes
             WHERE debut_ms <= ?2 AND fin_ms >= ?1 ORDER BY debut_ms",
        )?;
        requete
            .query_map(params![debut_ms, fin_ms], |r| {
                Ok(Episode {
                    id: r.get(0)?,
                    debut_ms: r.get(1)?,
                    fin_ms: r.get(2)?,
                    titre: r.get(3)?,
                    resume: r.get(4)?,
                    modele: r.get(5)?,
                })
            })?
            .collect()
    }

    /// Oublie les résumés qui couvrent ces instants (images effacées) : refaits sans elles.
    pub fn oublier_episodes_a(&self, quands: &[i64]) -> Result<usize> {
        let mut requete = self
            .connexion
            .prepare_cached("DELETE FROM episodes WHERE debut_ms <= ?1 AND fin_ms >= ?1")?;
        let mut oublies = 0;
        for quand in quands {
            oublies += requete.execute(params![quand])?;
        }
        Ok(oublies)
    }

    /// Les heures des images d'une période (tous écrans), sans les moments privés.
    pub fn heures_non_privees(&self, debut_ms: i64, fin_ms: i64) -> Result<Vec<i64>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT quand_ms FROM images WHERE quand_ms BETWEEN ?1 AND ?2 AND privee = 0 ORDER BY quand_ms",
        )?;
        requete.query_map(params![debut_ms, fin_ms], |r| r.get(0))?.collect()
    }

    /// Tout le texte stocké (pour l'essai du masquage).
    pub fn tous_les_textes(&self) -> Result<Vec<String>> {
        let mut requete = self.connexion.prepare(
            "SELECT contenu FROM textes
             UNION ALL SELECT fenetre FROM images WHERE fenetre IS NOT NULL
             UNION ALL SELECT url FROM images WHERE url IS NOT NULL
             UNION ALL SELECT requete FROM recherches
             UNION ALL SELECT texte FROM voix WHERE texte IS NOT NULL
             UNION ALL SELECT titre || ' ' || resume FROM episodes",
        )?;
        requete.query_map([], |r| r.get(0))?.collect()
    }

    pub fn meta(&self, cle: &str) -> Result<Option<String>> {
        self.connexion
            .query_row("SELECT valeur FROM meta WHERE cle = ?1", params![cle], |r| r.get(0))
            .optional()
    }

    pub fn noter_meta(&self, cle: &str, valeur: &str) -> Result<()> {
        self.connexion.execute(
            "INSERT INTO meta (cle, valeur) VALUES (?1, ?2) ON CONFLICT(cle) DO UPDATE SET valeur = excluded.valeur",
            params![cle, valeur],
        )?;
        Ok(())
    }

    /// Repasse tout le texte déjà stocké par `masquer` (qui rend `Some(texte masqué)` s'il y avait
    /// un secret) : textes et leur index, titres de fenêtre, adresses, recherches, transcriptions.
    /// Rend le nombre de lignes changées.
    pub fn remasquer(&self, masquer: impl Fn(&str) -> Option<String>) -> Result<usize> {
        let transaction = self.connexion.unchecked_transaction()?;
        let mut changes = 0;
        let textes: Vec<(i64, String)> = {
            let mut requete = transaction.prepare("SELECT id, contenu FROM textes")?;
            requete.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_>>()?
        };
        for (id, contenu) in textes {
            let Some(masque) = masquer(&contenu) else { continue };
            let deja: Option<i64> = transaction
                .query_row("SELECT id FROM textes WHERE empreinte = ?1", params![empreinte(&masque)], |r| r.get(0))
                .optional()?;
            match deja {
                // Le texte masqué existe déjà : ses apparitions le rejoignent, et l'ancien texte
                // disparaît (le déclencheur le retire aussi de l'index).
                Some(autre) => {
                    transaction.execute("UPDATE apparitions SET texte = ?1 WHERE texte = ?2", params![autre, id])?;
                    transaction.execute("DELETE FROM textes WHERE id = ?1", params![id])?;
                }
                None => {
                    transaction.execute(
                        "INSERT INTO textes_fts(textes_fts, rowid, contenu) VALUES ('delete', ?1, ?2)",
                        params![id, contenu],
                    )?;
                    transaction.execute(
                        "UPDATE textes SET contenu = ?2, empreinte = ?3 WHERE id = ?1",
                        params![id, masque, empreinte(&masque)],
                    )?;
                    transaction.execute(
                        "INSERT INTO textes_fts(rowid, contenu) VALUES (?1, ?2)",
                        params![id, masque],
                    )?;
                }
            }
            changes += 1;
        }
        // Les autres colonnes de texte.
        for (table, colonne) in [
            ("images", "fenetre"),
            ("images", "url"),
            ("recherches", "requete"),
            ("recherches", "url"),
            ("voix", "texte"),
        ] {
            let lignes: Vec<(i64, String)> = {
                let mut requete = transaction.prepare(&format!(
                    "SELECT id, {colonne} FROM {table} WHERE {colonne} IS NOT NULL"
                ))?;
                requete.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_>>()?
            };
            for (id, valeur) in lignes {
                if let Some(masque) = masquer(&valeur) {
                    transaction.execute(
                        &format!("UPDATE {table} SET {colonne} = ?2 WHERE id = ?1"),
                        params![id, masque],
                    )?;
                    changes += 1;
                }
            }
        }
        transaction.commit()?;
        Ok(changes)
    }

    /// Recopie le journal (WAL) dans la base et le vide : après un effacement, plus rien n'y traîne.
    pub fn vider_journal(&self) -> Result<()> {
        self.connexion
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
    }

    /// Nombre de passages de voix et leur durée totale (ms).
    pub fn bilan_voix(&self) -> Result<(i64, i64)> {
        self.connexion.query_row(
            "SELECT COUNT(*), COALESCE(SUM(fin_ms - debut_ms), 0) FROM voix",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// Oublie les passages de voix finis avant `avant_ms` ; rend leurs fichiers, à effacer.
    pub fn oublier_voix_avant(&self, avant_ms: i64) -> Result<Vec<String>> {
        let fichiers: Vec<String> = {
            let mut requete = self.connexion.prepare("SELECT fichier FROM voix WHERE fin_ms < ?1")?;
            requete.query_map(params![avant_ms], |r| r.get(0))?.collect::<Result<_>>()?
        };
        self.connexion.execute("DELETE FROM voix WHERE fin_ms < ?1", params![avant_ms])?;
        Ok(fichiers)
    }

    /// Une recherche tapée (sauf si la même vient d'être notée).
    pub fn noter_recherche(&self, quand_ms: i64, moteur: &str, requete: &str, url: &str) -> Result<bool> {
        let deja: i64 = self.connexion.query_row(
            "SELECT COUNT(*) FROM recherches WHERE requete = ?1 AND quand_ms > ?2",
            params![requete, quand_ms - 600_000],
            |r| r.get(0),
        )?;
        if deja > 0 {
            return Ok(false);
        }
        self.connexion.execute(
            "INSERT INTO recherches (quand_ms, moteur, requete, url) VALUES (?1, ?2, ?3, ?4)",
            params![quand_ms, moteur, requete, url],
        )?;
        Ok(true)
    }

    /// Recherche plein texte (mots exacts ou débuts de mots, accents ignorés), les plus pertinents d'abord.
    /// Cherche des mots dans tout ce qui a été lu ou dit. Les mots entiers d'abord (« bar ») ; puis,
    /// pour compléter jusqu'à `limite`, les débuts de mots (« barre », « Barcelone »).
    /// Les mots d'une requête qui valent d'être cherchés : un mot trop courant (« le », « un ») est
    /// laissé de côté s'il y en a d'autres ; une requête faite seulement de mots trop courants n'en
    /// garde aucun.
    pub fn mots_a_chercher(&self, requete: &str) -> Result<Vec<String>> {
        let mots = mots_de(requete);
        if mots.is_empty() {
            return Ok(mots);
        }
        let total: f64 = self.connexion.query_row("SELECT count(*) FROM textes", [], |r| r.get::<_, i64>(0))?.max(1) as f64;
        let mut frequence = self.connexion.prepare_cached("SELECT doc FROM textes_fts_vocab WHERE term = ?1")?;
        let mut rares = Vec::new();
        for mot in &mots {
            let doc: i64 = frequence.query_row(params![comme_l_index(mot)], |r| r.get(0)).optional()?.unwrap_or(0);
            // Une petite base n'a pas de mot courant : il en faut au moins 200 textes.
            if doc < 200 || (doc as f64) / total <= MOT_COURANT {
                rares.push(mot.clone());
            }
        }
        Ok(rares)
    }

    pub fn chercher(&self, requete: &str, limite: usize) -> Result<Vec<Trouvaille>> {
        let mots = self.mots_a_chercher(requete)?;
        let (Some(entiers), Some(debuts)) = (requete_fts_mots(&mots, false), requete_fts_mots(&mots, true)) else {
            return Ok(Vec::new());
        };
        let mut trouvees = self.chercher_fts(&entiers, limite, true)?;
        if trouvees.len() < limite {
            let deja: std::collections::HashSet<i64> = trouvees.iter().map(|t| t.apparition).collect();
            for t in self.chercher_fts(&debuts, limite, false)? {
                if trouvees.len() >= limite {
                    break;
                }
                if !deja.contains(&t.apparition) {
                    trouvees.push(t);
                }
            }
        }
        Ok(trouvees)
    }

    fn chercher_fts(&self, fts: &str, limite: usize, entier: bool) -> Result<Vec<Trouvaille>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT t.contenu, a.ecran, a.debut_ms, a.fin_ms, a.vu, a.x, a.y, a.l, a.h, bm25(textes_fts), a.id
             FROM textes_fts
             JOIN textes t ON t.id = textes_fts.rowid
             JOIN apparitions a ON a.texte = t.id
             WHERE textes_fts MATCH ?1
             ORDER BY bm25(textes_fts), a.fin_ms DESC
             LIMIT ?2",
        )?;
        let lignes = requete.query_map(params![fts, limite as i64], |r| {
            Ok(Trouvaille {
                texte: r.get(0)?,
                ecran: r.get(1)?,
                debut_ms: r.get(2)?,
                fin_ms: r.get(3)?,
                vu: r.get(4)?,
                cadre: [
                    r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                ],
                score: r.get(9)?,
                entier,
                apparition: r.get(10)?,
            })
        })?;
        lignes.collect()
    }

    /// Du texte a-t-il été noté pour cet écran à cet instant ?
    pub fn a_du_texte(&self, ecran: u32, quand_ms: i64) -> Result<bool> {
        self.connexion.query_row(
            "SELECT EXISTS(SELECT 1 FROM apparitions WHERE ecran = ?1 AND fin_ms >= ?2 AND debut_ms <= ?2)",
            params![ecran, quand_ms],
            |r| r.get(0),
        )
    }

    /// Textes pas encore passés par le modèle de sens (après `apres_id`), dans l'ordre.
    pub fn textes_sans_sens(&self, apres_id: i64, limite: usize) -> Result<Vec<(i64, String)>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT id, contenu FROM textes WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        let lignes = requete.query_map(params![apres_id, limite as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        lignes.collect()
    }

    /// Range les vecteurs d'un texte (un par morceau) ; un texte effacé entre-temps est ignoré.
    pub fn ranger_sens(&self, texte: i64, vecteurs: &[(f32, Vec<i8>)]) -> Result<()> {
        let transaction = self.connexion.unchecked_transaction()?;
        {
            let existe: bool =
                transaction.query_row("SELECT EXISTS(SELECT 1 FROM textes WHERE id = ?1)", params![texte], |r| r.get(0))?;
            if existe {
                let mut requete = transaction.prepare_cached(
                    "INSERT OR REPLACE INTO sens (texte, morceau, echelle, vecteur) VALUES (?1, ?2, ?3, ?4)",
                )?;
                let mut code = transaction.prepare_cached("INSERT OR REPLACE INTO codes_sens (id, code) VALUES (?1, ?2)")?;
                for (morceau, (echelle, v)) in vecteurs.iter().enumerate() {
                    let octets: Vec<u8> = v.iter().map(|&x| x as u8).collect();
                    requete.execute(params![texte, morceau as i64, f64::from(*echelle), &octets])?;
                    code.execute(params![transaction.last_insert_rowid(), code_de(&octets)])?;
                }
            }
        }
        transaction.commit()
    }

    /// Un échantillon de `n` vecteurs de sens (premiers morceaux) répartis sur toute la table, avec
    /// leur texte : de quoi vérifier qu'un nouveau modèle redonne les mêmes vecteurs.
    pub fn echantillon_sens(&self, n: usize) -> Result<Vec<(Sens, String)>> {
        let total: i64 = self.connexion.query_row("SELECT COUNT(*) FROM sens WHERE morceau = 0", [], |r| r.get(0))?;
        let pas = (total / n.max(1) as i64).max(1);
        let mut requete = self.connexion.prepare(
            "SELECT s.id, s.texte, s.echelle, s.vecteur, t.contenu
             FROM (SELECT *, ROW_NUMBER() OVER (ORDER BY id) AS rang FROM sens WHERE morceau = 0) s
             JOIN textes t ON t.id = s.texte
             WHERE s.rang % ?1 = 0 LIMIT ?2",
        )?;
        let lignes = requete.query_map(params![pas, n as i64], |r| {
            let octets: Vec<u8> = r.get(3)?;
            Ok((
                Sens {
                    id: r.get(0)?,
                    texte: r.get(1)?,
                    echelle: r.get::<_, f64>(2)? as f32,
                    vecteur: octets.into_iter().map(|b| b as i8).collect(),
                },
                r.get(4)?,
            ))
        })?;
        lignes.collect()
    }

    /// Les vecteurs rangés après `apres_id` (pour tenir un index en mémoire à jour).
    pub fn sens_depuis(&self, apres_id: i64, limite: usize) -> Result<Vec<Sens>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, texte, echelle, vecteur FROM sens WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let lignes = requete.query_map(params![apres_id, limite as i64], |r| {
            let octets: Vec<u8> = r.get(3)?;
            Ok(Sens {
                id: r.get(0)?,
                texte: r.get(1)?,
                echelle: r.get::<_, f64>(2)? as f32,
                vecteur: octets.into_iter().map(|b| b as i8).collect(),
            })
        })?;
        lignes.collect()
    }

    /// Les endroits où des textes donnés sont apparus, dans l'ordre des textes (le plus proche
    /// d'abord) ; `score` = la similarité (plus grand = plus proche). Les plus récents d'abord
    /// pour un même texte, `par_texte` apparitions au plus.
    pub fn trouvailles_de_textes(&self, textes: &[(i64, f32)], par_texte: usize, limite: usize) -> Result<Vec<Trouvaille>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT t.contenu, a.ecran, a.debut_ms, a.fin_ms, a.vu, a.x, a.y, a.l, a.h, a.id
             FROM textes t JOIN apparitions a ON a.texte = t.id
             WHERE t.id = ?1 ORDER BY a.vu DESC, a.fin_ms DESC LIMIT ?2",
        )?;
        let mut sortie = Vec::new();
        for &(texte, similarite) in textes {
            let lignes = requete.query_map(params![texte, par_texte as i64], |r| {
                Ok(Trouvaille {
                    texte: r.get(0)?,
                    ecran: r.get(1)?,
                    debut_ms: r.get(2)?,
                    fin_ms: r.get(3)?,
                    vu: r.get(4)?,
                    cadre: [
                        r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                    ],
                    score: f64::from(similarite),
                    entier: false,
                    apparition: r.get(9)?,
                })
            })?;
            for l in lignes {
                sortie.push(l?);
                if sortie.len() >= limite {
                    return Ok(sortie);
                }
            }
        }
        Ok(sortie)
    }

    /// Supprime le texte affiché aux heures données (images supprimées) et les textes devenus orphelins.
    pub fn oublier_textes_a(&self, ecran: u32, heures_ms: &[i64]) -> Result<usize> {
        let transaction = self.connexion.unchecked_transaction()?;
        let mut retirees = 0;
        {
            let mut requete = transaction
                .prepare_cached("DELETE FROM apparitions WHERE ecran = ?1 AND debut_ms <= ?2 AND fin_ms >= ?2")?;
            for heure in heures_ms {
                retirees += requete.execute(params![ecran, heure])?;
            }
        }
        transaction.execute(
            "DELETE FROM textes WHERE id NOT IN (SELECT DISTINCT texte FROM apparitions)",
            [],
        )?;
        transaction.commit()?;
        Ok(retirees)
    }

    /// Purge : le texte affiché avant une heure donnée.
    pub fn oublier_textes_avant(&self, avant_ms: i64) -> Result<usize> {
        let retirees = self
            .connexion
            .execute("DELETE FROM apparitions WHERE fin_ms < ?1", params![avant_ms])?;
        self.connexion.execute(
            "DELETE FROM textes WHERE id NOT IN (SELECT DISTINCT texte FROM apparitions)",
            [],
        )?;
        self.connexion
            .execute("DELETE FROM recherches WHERE quand_ms < ?1", params![avant_ms])?;
        Ok(retirees)
    }

    /// Pour des images à supprimer : (id, morceau, fichier du morceau, position dans la vidéo).
    pub fn emplacements(&self, ids: &[i64]) -> Result<Vec<(i64, i64, String, i64)>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT i.id, i.morceau, m.fichier, i.position_ms FROM images i JOIN morceaux m ON m.id = i.morceau
             WHERE i.id = ?1 AND i.fichier_prive IS NULL",
        )?;
        let mut trouves = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(ligne) = requete
                .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .optional()?
            {
                trouves.push(ligne);
            }
        }
        Ok(trouves)
    }

    /// Les images privées encore dans une vidéo terminée (fermée), à chiffrer à part.
    pub fn privees_a_chiffrer(&self, limite: usize) -> Result<Vec<PriveeARanger>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT i.id, i.morceau, m.fichier, i.position_ms, i.quand_ms FROM images i JOIN morceaux m ON m.id = i.morceau
             WHERE i.privee = 1 AND i.fichier_prive IS NULL AND m.fin_ms IS NOT NULL
             ORDER BY i.morceau, i.position_ms LIMIT ?1",
        )?;
        let lignes = requete.query_map(params![limite as i64], |r| {
            Ok(PriveeARanger {
                id: r.get(0)?,
                morceau: r.get(1)?,
                fichier: r.get(2)?,
                position_ms: r.get(3)?,
                quand_ms: r.get(4)?,
            })
        })?;
        lignes.collect()
    }

    /// L'image est maintenant chiffrée à part, dans `fichier` (relatif au dossier des données).
    pub fn noter_fichier_prive(&self, id: i64, fichier: &str) -> Result<()> {
        self.connexion.execute(
            "UPDATE images SET fichier_prive = ?2 WHERE id = ?1",
            params![id, fichier],
        )?;
        Ok(())
    }

    /// L'image est rangée dans une suite chiffrée : `position_ms` devient son rang dans la suite.
    pub fn noter_dans_suite(&self, id: i64, fichier: &str, rang: i64) -> Result<()> {
        self.connexion.execute(
            "UPDATE images SET fichier_prive = ?2, position_ms = ?3 WHERE id = ?1",
            params![id, fichier, rang],
        )?;
        Ok(())
    }

    /// Retour en arrière : l'image est de nouveau lue dans sa vidéo, à sa position d'origine.
    pub fn remettre_dans_la_video(&self, id: i64, position_ms: i64) -> Result<()> {
        self.connexion.execute(
            "UPDATE images SET fichier_prive = NULL, position_ms = ?2 WHERE id = ?1",
            params![id, position_ms],
        )?;
        Ok(())
    }

    /// Les images d'une suite privée : (id, rang dans la suite), dans l'ordre.
    pub fn images_de_la_suite(&self, fichier: &str) -> Result<Vec<(i64, i64)>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT id, position_ms FROM images WHERE fichier_prive = ?1 ORDER BY position_ms")?;
        requete.query_map(params![fichier], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    /// Les images privées encore rangées une à une (fichiers `.bsp`, avant les suites), par
    /// morceau puis par heure : (id, morceau, fichier, heure).
    pub fn privees_une_a_une(&self, limite: usize) -> Result<Vec<(i64, i64, String, i64)>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, morceau, fichier_prive, quand_ms FROM images
             WHERE fichier_prive LIKE '%.bsp' ORDER BY morceau, quand_ms LIMIT ?1",
        )?;
        requete
            .query_map(params![limite as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect()
    }

    /// Tous les fichiers privés encore utilisés (pour retrouver les fichiers orphelins).
    pub fn fichiers_prives_utilises(&self) -> Result<std::collections::HashSet<String>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT DISTINCT fichier_prive FROM images WHERE fichier_prive IS NOT NULL")?;
        requete.query_map([], |r| r.get(0))?.collect()
    }

    /// L'image est de nouveau lue dans sa vidéo (le rangement à part a échoué).
    pub fn oublier_fichier_prive(&self, id: i64) -> Result<()> {
        self.connexion
            .execute("UPDATE images SET fichier_prive = NULL WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Les fichiers privés de ces images (pour les effacer avec elles).
    pub fn fichiers_prives(&self, ids: &[i64]) -> Result<Vec<String>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT fichier_prive FROM images WHERE id = ?1 AND fichier_prive IS NOT NULL")?;
        let mut fichiers = Vec::new();
        for id in ids {
            if let Some(f) = requete.query_row(params![id], |r| r.get(0)).optional()? {
                fichiers.push(f);
            }
        }
        Ok(fichiers)
    }

    /// Les fichiers privés des images d'un morceau (purge).
    pub fn fichiers_prives_du_morceau(&self, morceau: i64) -> Result<Vec<String>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT fichier_prive FROM images WHERE morceau = ?1 AND fichier_prive IS NOT NULL")?;
        requete.query_map(params![morceau], |r| r.get(0))?.collect()
    }

    /// Supprime des images (et, plus tard, tout ce qui en dépend). Rend le nombre supprimé.
    pub fn supprimer_images(&self, ids: &[i64]) -> Result<usize> {
        let transaction = self.connexion.unchecked_transaction()?;
        let mut supprimees = 0;
        {
            let mut requete = transaction.prepare_cached("DELETE FROM images WHERE id = ?1")?;
            for id in ids {
                supprimees += requete.execute(params![id])?;
            }
        }
        transaction.commit()?;
        Ok(supprimees)
    }

    /// Écran et heure de chaque image (pour effacer aussi le texte affiché à ce moment).
    pub fn heures_images(&self, ids: &[i64]) -> Result<Vec<(u32, i64)>> {
        let mut requete = self
            .connexion
            .prepare_cached("SELECT ecran, quand_ms FROM images WHERE id = ?1")?;
        let mut heures = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(h) = requete
                .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
            {
                heures.push(h);
            }
        }
        Ok(heures)
    }

    /// Nombre d'images restant dans un morceau.
    pub fn nb_images_morceau(&self, morceau: i64) -> Result<i64> {
        self.connexion.query_row(
            "SELECT COUNT(*) FROM images WHERE morceau = ?1",
            params![morceau],
            |r| r.get(0),
        )
    }

    /// Met à jour la taille d'un morceau (après réencodage).
    pub fn changer_taille_morceau(&self, morceau: i64, octets: u64) -> Result<()> {
        self.connexion.execute(
            "UPDATE morceaux SET octets = ?2 WHERE id = ?1",
            params![morceau, octets as i64],
        )?;
        Ok(())
    }

    /// Supprime un morceau et toutes ses images.
    pub fn supprimer_morceau(&self, id: i64) -> Result<()> {
        self.connexion
            .execute("DELETE FROM morceaux WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn bilan(&self) -> Result<Bilan> {
        let (images, premiere_ms, derniere_ms): (i64, Option<i64>, Option<i64>) =
            self.connexion
                .query_row("SELECT COUNT(*), MIN(quand_ms), MAX(quand_ms) FROM images", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
        let (morceaux, octets): (i64, Option<i64>) =
            self.connexion
                .query_row("SELECT COUNT(*), SUM(octets) FROM morceaux", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        Ok(Bilan {
            images: images as u64,
            morceaux: morceaux as u64,
            octets: octets.unwrap_or(0) as u64,
            premiere_ms,
            derniere_ms,
        })
    }

    /// Ce qui a été enregistré, jour par jour (heure locale), depuis `depuis_ms` : pour mesurer
    /// ce que B-Screen coûte et capte (phase 11.3).
    pub fn bilan_par_jour(&self, depuis_ms: i64) -> Result<Vec<BilanJour>> {
        let mut jours: std::collections::BTreeMap<String, BilanJour> = std::collections::BTreeMap::new();
        let jour = |r: &rusqlite::Row<'_>| -> Result<String> { r.get(0) };
        {
            let mut q = self.connexion.prepare(
                "SELECT date(quand_ms / 1000, 'unixepoch', 'localtime') AS j, COUNT(*),
                        COUNT(DISTINCT strftime('%H', quand_ms / 1000, 'unixepoch', 'localtime')),
                        SUM(privee)
                 FROM images WHERE quand_ms >= ?1 GROUP BY j",
            )?;
            for ligne in q.query_map(params![depuis_ms], |r| Ok((jour(r)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))? {
                let (j, images, heures, privees) = ligne?;
                let b = jours.entry(j.clone()).or_insert_with(|| BilanJour { jour: j, ..BilanJour::default() });
                b.images = images as u64;
                b.heures_actives = heures as u32;
                b.images_privees = privees as u64;
            }
        }
        {
            let mut q = self.connexion.prepare(
                "SELECT date(debut_ms / 1000, 'unixepoch', 'localtime') AS j, COALESCE(SUM(octets), 0)
                 FROM morceaux WHERE debut_ms >= ?1 GROUP BY j",
            )?;
            for ligne in q.query_map(params![depuis_ms], |r| Ok((jour(r)?, r.get::<_, i64>(1)?)))? {
                let (j, octets) = ligne?;
                jours.entry(j.clone()).or_insert_with(|| BilanJour { jour: j, ..BilanJour::default() }).octets_video =
                    octets as u64;
            }
        }
        {
            let mut q = self.connexion.prepare(
                "SELECT date(debut_ms / 1000, 'unixepoch', 'localtime') AS j, COUNT(*),
                        COALESCE(SUM(fin_ms - debut_ms), 0), SUM(texte IS NOT NULL AND texte != '')
                 FROM voix WHERE debut_ms >= ?1 GROUP BY j",
            )?;
            for ligne in q.query_map(params![depuis_ms], |r| {
                Ok((jour(r)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?))
            })? {
                let (j, n, ms, transcrits) = ligne?;
                let b = jours.entry(j.clone()).or_insert_with(|| BilanJour { jour: j, ..BilanJour::default() });
                b.passages_voix = n as u64;
                b.secondes_voix = (ms / 1000) as u64;
                b.passages_transcrits = transcrits.unwrap_or(0) as u64;
            }
        }
        {
            let mut q = self.connexion.prepare(
                "SELECT date(debut_ms / 1000, 'unixepoch', 'localtime') AS j, COUNT(*)
                 FROM apparitions WHERE debut_ms >= ?1 AND ecran != 0 GROUP BY j",
            )?;
            for ligne in q.query_map(params![depuis_ms], |r| Ok((jour(r)?, r.get::<_, i64>(1)?)))? {
                let (j, n) = ligne?;
                jours.entry(j.clone()).or_insert_with(|| BilanJour { jour: j, ..BilanJour::default() }).textes_vus =
                    n as u64;
            }
        }
        Ok(jours.into_values().collect())
    }

    /// Les textes vus (ou présents) sur un écran pendant une plage, une fois chacun, dans l'ordre
    /// de lecture : (texte, source 0 = accessibilité / 1 = OCR, vu, focus, cadre en points).
    pub fn textes_entre(&self, ecran: u32, debut_ms: i64, fin_ms: i64) -> Result<Vec<TexteVu>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT t.contenu, MIN(a.source), MAX(a.vu), MAX(a.focus), MIN(a.y), MIN(a.x), MAX(a.l), MAX(a.h)
             FROM apparitions a JOIN textes t ON t.id = a.texte
             WHERE a.ecran = ?1 AND a.debut_ms <= ?3 AND a.fin_ms >= ?2
             GROUP BY a.texte
             ORDER BY MIN(a.y), MIN(a.x)",
        )?;
        requete
            .query_map(params![ecran, debut_ms, fin_ms], |r| {
                Ok(TexteVu {
                    texte: r.get(0)?,
                    source: r.get(1)?,
                    vu: r.get(2)?,
                    focus: r.get(3)?,
                    cadre: [
                        r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                        r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                    ],
                })
            })?
            .collect()
    }

    /// Les recherches tapées pendant une plage.
    pub fn recherches_entre(&self, debut_ms: i64, fin_ms: i64) -> Result<Vec<Recherche>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT id, quand_ms, moteur, requete, url FROM recherches WHERE quand_ms BETWEEN ?1 AND ?2 ORDER BY quand_ms",
        )?;
        requete
            .query_map(params![debut_ms, fin_ms], |r| {
                Ok(Recherche {
                    id: r.get(0)?,
                    quand_ms: r.get(1)?,
                    moteur: r.get(2)?,
                    requete: r.get(3)?,
                    url: r.get(4)?,
                })
            })?
            .collect()
    }

    /// Les mots d'un passage de voix avec leur heure (JSON rangé à la transcription).
    pub fn mots_voix(&self, id: i64) -> Result<Option<String>> {
        self.connexion
            .query_row("SELECT mots FROM voix WHERE id = ?1", params![id], |r| r.get(0))
            .optional()
            .map(Option::flatten)
    }

    /// Les textes vus à l'écran depuis une heure donnée (une fois chacun, au plus `limite`).
    pub fn textes_vus_depuis(&self, depuis_ms: i64, limite: usize) -> Result<Vec<String>> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT t.contenu FROM textes t
             WHERE t.id IN (SELECT DISTINCT texte FROM apparitions WHERE fin_ms >= ?1 AND vu = 1 AND ecran != 0)
             LIMIT ?2",
        )?;
        requete
            .query_map(params![depuis_ms, limite as i64], |r| r.get(0))?
            .collect()
    }

    /// L'image à l'écran juste avant (ou à) une heure, tous écrans confondus.
    /// Une période touche-t-elle une navigation privée (une image privée pendant la période, ou
    /// l'écran privé juste avant son début) ? Pour verrouiller les voix entendues à ce moment-là.
    pub fn privee_pendant(&self, debut_ms: i64, fin_ms: i64) -> Result<bool> {
        let pendant: bool = self.connexion.query_row(
            "SELECT EXISTS(SELECT 1 FROM images WHERE privee = 1 AND quand_ms BETWEEN ?1 AND ?2)",
            params![debut_ms, fin_ms.max(debut_ms)],
            |r| r.get(0),
        )?;
        Ok(pendant || self.image_avant(debut_ms)?.is_some_and(|i| i.privee))
    }

    /// Le verrou privé de toute une période, lu une fois : pour vérifier beaucoup de passages de
    /// voix sans une requête chacun (voir [`VerrouPrive::pendant`]).
    pub fn verrou_prive(&self, debut_ms: i64, fin_ms: i64) -> Result<VerrouPrive> {
        let mut requete = self.connexion.prepare_cached(
            "SELECT quand_ms, privee FROM
                 (SELECT quand_ms, privee FROM images WHERE quand_ms < ?1 ORDER BY quand_ms DESC LIMIT 1)
             UNION ALL
             SELECT quand_ms, privee FROM images WHERE quand_ms BETWEEN ?1 AND ?2
             ORDER BY 1",
        )?;
        let images = requete
            .query_map(params![debut_ms, fin_ms], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<(i64, bool)>>>()?;
        Ok(VerrouPrive { images })
    }

    pub fn image_avant(&self, quand_ms: i64) -> Result<Option<Image>> {
        let id: Option<(i64, u32)> = self
            .connexion
            .query_row(
                "SELECT quand_ms, ecran FROM images WHERE quand_ms <= ?1 ORDER BY quand_ms DESC LIMIT 1",
                params![quand_ms],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match id {
            Some((quand, ecran)) => self.image_proche(quand, ecran),
            None => Ok(None),
        }
    }

    /// L'image la plus proche d'une heure donnée (pour la frise).
    pub fn image_proche(&self, quand_ms: i64, ecran: u32) -> Result<Option<Image>> {
        // Deux lectures par l'index des heures (juste avant, juste après), pas toute la table :
        // trier toutes les images par écart prenait ~2 ms par appel, et 14 s pour une semaine de voix.
        self.connexion
            .query_row(
                "SELECT i.id, i.morceau, COALESCE(i.fichier_prive, m.fichier), i.quand_ms, i.position_ms, i.ecran,
                        i.app_bundle, i.app_nom, i.aire, i.declenchee, m.largeur, m.hauteur, i.fenetre, i.url, i.privee
                 FROM images i JOIN morceaux m ON m.id = i.morceau
                 WHERE i.id IN (
                     (SELECT id FROM images WHERE ecran = ?2 AND quand_ms <= ?1 ORDER BY quand_ms DESC LIMIT 1),
                     (SELECT id FROM images WHERE ecran = ?2 AND quand_ms > ?1 ORDER BY quand_ms LIMIT 1))
                 ORDER BY ABS(i.quand_ms - ?1), i.quand_ms LIMIT 1",
                params![quand_ms, ecran],
                |r| {
                    Ok(Image {
                        id: r.get(0)?,
                        morceau: r.get(1)?,
                        fichier: r.get(2)?,
                        quand_ms: r.get(3)?,
                        position_ms: r.get(4)?,
                        ecran: r.get(5)?,
                        app_bundle: r.get(6)?,
                        app_nom: r.get(7)?,
                        aire: r.get(8)?,
                        declenchee: r.get(9)?,
                        largeur: r.get(10)?,
                        hauteur: r.get(11)?,
                        fenetre: r.get(12)?,
                        url: r.get(13)?,
                        privee: r.get(14)?,
                    })
                },
            )
            .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_avec_un_morceau() -> (Base, i64) {
        let base = Base::en_memoire().unwrap();
        let id = base
            .nouveau_morceau(&NouveauMorceau {
                ecran: 1,
                fichier: "video/2026-11-19/ecran1-215700.mp4",
                debut_ms: 1_000,
                largeur: 3456,
                hauteur: 2234,
            })
            .unwrap();
        (base, id)
    }

    fn image(morceau: i64, quand_ms: i64) -> NouvelleImage<'static> {
        NouvelleImage {
            morceau,
            quand_ms,
            position_ms: quand_ms - 1_000,
            ecran: 1,
            app_bundle: Some("com.apple.Safari"),
            app_nom: Some("Safari"),
            aire: 0.01,
            declenchee: false,
        }
    }

    #[test]
    fn images_dans_l_ordre_et_filtrees_par_heure() {
        let (base, m) = base_avec_un_morceau();
        for quand in [5_000, 1_000, 3_000, 9_000] {
            base.ajouter_image(&image(m, quand)).unwrap();
        }
        let trouvees = base.images_entre(1_000, 5_000).unwrap();
        let heures: Vec<i64> = trouvees.iter().map(|i| i.quand_ms).collect();
        assert_eq!(heures, vec![1_000, 3_000, 5_000]);
        assert_eq!(trouvees[0].fichier, "video/2026-11-19/ecran1-215700.mp4");
        assert_eq!(trouvees[0].app_nom.as_deref(), Some("Safari"));
    }

    #[test]
    fn image_la_plus_proche() {
        let (base, m) = base_avec_un_morceau();
        for quand in [1_000, 3_000, 9_000] {
            base.ajouter_image(&image(m, quand)).unwrap();
        }
        assert_eq!(base.image_proche(3_900, 1).unwrap().unwrap().quand_ms, 3_000);
        assert_eq!(base.image_proche(8_000, 1).unwrap().unwrap().quand_ms, 9_000);
        assert!(base.image_proche(8_000, 2).unwrap().is_none());
        // Pile dessus, avant la première, après la dernière, à égale distance (la plus ancienne).
        assert_eq!(base.image_proche(3_000, 1).unwrap().unwrap().quand_ms, 3_000);
        assert_eq!(base.image_proche(0, 1).unwrap().unwrap().quand_ms, 1_000);
        assert_eq!(base.image_proche(50_000, 1).unwrap().unwrap().quand_ms, 9_000);
        assert_eq!(base.image_proche(2_000, 1).unwrap().unwrap().quand_ms, 1_000);
    }

    #[test]
    fn verrou_prive_comme_privee_pendant() {
        let (base, m) = base_avec_un_morceau();
        for quand in (1_000..=20_000).step_by(1_000) {
            base.ajouter_image(&image(m, quand)).unwrap();
        }
        // Privées : 6 000 à 8 000, et 15 000.
        base.noter_privee(1, 5_500, 8_000).unwrap();
        base.noter_privee(1, 14_500, 15_000).unwrap();
        let verrou = base.verrou_prive(2_500, 20_000).unwrap();
        let periodes = [
            (2_500, 3_000),
            (4_100, 5_900),
            (5_900, 6_000),
            (6_500, 6_600),
            (8_100, 8_900),
            (9_100, 9_500),
            (14_200, 14_900),
            (15_000, 15_000),
            (15_500, 15_900),
            (16_000, 19_000),
            (9_000, 8_000),
        ];
        for (debut, fin) in periodes {
            assert_eq!(
                verrou.pendant(debut, fin),
                base.privee_pendant(debut, fin).unwrap(),
                "période {debut}-{fin}"
            );
        }
        assert!(verrou.pendant(6_500, 6_600) && verrou.pendant(8_100, 8_900) && !verrou.pendant(9_100, 9_500));
    }

    #[test]
    fn supprimer_un_morceau_supprime_ses_images() {
        let (base, m) = base_avec_un_morceau();
        base.ajouter_image(&image(m, 2_000)).unwrap();
        base.fermer_morceau(m, 2_000, 123_456).unwrap();
        let bilan = base.bilan().unwrap();
        assert_eq!((bilan.images, bilan.morceaux, bilan.octets), (1, 1, 123_456));
        assert_eq!(base.morceaux_avant(10_000).unwrap().len(), 1);
        base.supprimer_morceau(m).unwrap();
        let bilan = base.bilan().unwrap();
        assert_eq!((bilan.images, bilan.morceaux), (0, 0));
    }

    #[test]
    fn supprimer_des_images() {
        let (base, m) = base_avec_un_morceau();
        let ids: Vec<i64> = [1_000, 3_000, 5_000]
            .iter()
            .map(|&q| base.ajouter_image(&image(m, q)).unwrap())
            .collect();
        let emplacements = base.emplacements(&[ids[1], 999]).unwrap();
        assert_eq!(
            emplacements,
            vec![(ids[1], m, "video/2026-11-19/ecran1-215700.mp4".to_string(), 2_000)]
        );
        assert_eq!(base.supprimer_images(&[ids[0], ids[1]]).unwrap(), 2);
        assert_eq!(base.nb_images_morceau(m).unwrap(), 1);
        assert_eq!(base.images_entre(0, 10_000).unwrap()[0].quand_ms, 5_000);
    }

    #[test]
    fn textes_et_recherche() {
        let (base, _) = base_avec_un_morceau();
        let lac = base.texte_id("Le lac d'Annecy est un lac alpin").unwrap();
        assert_eq!(
            base.texte_id("Le lac d'Annecy est un lac alpin").unwrap(),
            lac,
            "pas de doublon"
        );
        let code = base.texte_id("fn main() { println!(\"bonjour\") }").unwrap();
        let apparition = |texte, quand_ms| NouvelleApparition {
            texte,
            ecran: 1,
            quand_ms,
            source: 0,
            vu: true,
            focus: false,
            cadre: [10.0, 20.0, 300.0, 18.0],
        };
        let ids = base
            .noter_apparitions(&[apparition(lac, 1_000), apparition(code, 1_000)], &[], 1_000)
            .unwrap();
        base.noter_apparitions(&[], &[ids[0]], 3_000).unwrap();

        // Accents et casse ignorés, début de mot accepté.
        let trouve = base.chercher("ANNEC alpin", 10).unwrap();
        assert_eq!(trouve.len(), 1);
        assert_eq!((trouve[0].debut_ms, trouve[0].fin_ms), (1_000, 3_000));
        assert!(base.chercher("ännecy", 10).unwrap().len() == 1);
        assert!(base.chercher("bonjour", 10).unwrap().len() == 1);
        assert!(base.chercher("dauphin", 10).unwrap().is_empty());
        assert!(base.chercher("  \"' ", 10).unwrap().is_empty());

        // Supprimer l'image de 2 000 ms retire le texte affiché à ce moment.
        assert_eq!(base.oublier_textes_a(1, &[2_000]).unwrap(), 1);
        assert!(base.chercher("annecy", 10).unwrap().is_empty());
        assert_eq!(base.chercher("bonjour", 10).unwrap().len(), 1);
    }

    #[test]
    fn mots_entiers_d_abord() {
        let base = Base::en_memoire().unwrap();
        let barre = base.texte_id("La barre d'outils").unwrap();
        let bar = base.texte_id("On se retrouve au bar du port").unwrap();
        let apparition = |texte, quand_ms| NouvelleApparition {
            texte,
            ecran: 1,
            quand_ms,
            source: 0,
            vu: true,
            focus: false,
            cadre: [0.0, 0.0, 10.0, 10.0],
        };
        base.noter_apparitions(&[apparition(barre, 1_000), apparition(bar, 2_000)], &[], 2_000)
            .unwrap();
        let trouve = base.chercher("bar", 10).unwrap();
        assert_eq!(trouve.len(), 2, "chaque apparition une seule fois");
        assert!(trouve[0].entier && trouve[0].texte.contains("bar du port"));
        assert!(!trouve[1].entier && trouve[1].texte.contains("barre"));
        // La limite garde les mots entiers.
        let un = base.chercher("bar", 1).unwrap();
        assert!(un.len() == 1 && un[0].entier);
    }

    #[test]
    fn texte_visible_dans_l_ordre_de_lecture() {
        let (base, _) = base_avec_un_morceau();
        let (a, b, c) = (
            base.texte_id("en bas").unwrap(),
            base.texte_id("en haut").unwrap(),
            base.texte_id("caché").unwrap(),
        );
        let apparition = |texte, y: f64, vu| NouvelleApparition {
            texte,
            ecran: 1,
            quand_ms: 1_000,
            source: 0,
            vu,
            focus: false,
            cadre: [10.0, y, 100.0, 16.0],
        };
        base.noter_apparitions(
            &[
                apparition(a, 500.0, true),
                apparition(b, 40.0, true),
                apparition(c, 90.0, false),
            ],
            &[],
            1_000,
        )
        .unwrap();
        assert_eq!(
            base.texte_visible(1, 1_000).unwrap(),
            vec!["en haut".to_string(), "en bas".to_string()]
        );
        assert!(base.texte_visible(1, 9_000).unwrap().is_empty());
    }

    #[test]
    fn mots_trop_courants() {
        let (base, _) = base_avec_un_morceau();
        let mut nouvelles = Vec::new();
        for i in 0..400 {
            let t = base.texte_id(&format!("le texte numéro {i}")).unwrap();
            nouvelles.push(NouvelleApparition { texte: t, ecran: 1, quand_ms: 1_000 + i, source: 0, vu: true, focus: false, cadre: [0.0; 4] });
        }
        let t = base.texte_id("la voiture rouge est garée").unwrap();
        nouvelles.push(NouvelleApparition { texte: t, ecran: 1, quand_ms: 5_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] });
        base.noter_apparitions(&nouvelles, &[], 5_000).unwrap();
        // « le » est dans presque tous les textes : seul, rien ; avec un mot rare, il est laissé de côté.
        assert!(base.chercher("le", 50).unwrap().is_empty());
        assert_eq!(base.mots_a_chercher("le voiture").unwrap(), vec!["voiture"]);
        assert_eq!(base.chercher("le voiture", 50).unwrap().len(), 1);
        // Accents comme dans l'index.
        assert_eq!(comme_l_index("Numéro"), "numero");
        assert!(base.chercher("numéro", 50).unwrap().is_empty());
    }

    #[test]
    fn recherches_tapees_sans_doublon() {
        let (base, _) = base_avec_un_morceau();
        assert!(
            base.noter_recherche(1_000, "google.com", "poisson clown", "https://…")
                .unwrap()
        );
        assert!(
            !base
                .noter_recherche(5_000, "google.com", "poisson clown", "https://…")
                .unwrap()
        );
        assert!(
            base.noter_recherche(5_000, "google.com", "mollusque", "https://…")
                .unwrap()
        );
    }

    #[test]
    fn corrections_retenues_une_fois() {
        let base = Base::en_memoire().unwrap();
        let a = base.ajouter_correction(1_000, "Le dîner était mardi.", Some("on a dîné quand ?")).unwrap();
        assert_eq!(base.ajouter_correction(2_000, "Le dîner était mardi.", None).unwrap(), a);
        let b = base.ajouter_correction(3_000, "Camille est mon associé.", None).unwrap();
        let toutes: Vec<i64> = base.corrections(10).unwrap().iter().map(|c| c.id).collect();
        assert_eq!(toutes, vec![b, a]);
        assert!(base.oublier_correction(a).unwrap());
        assert!(!base.oublier_correction(a).unwrap());
        assert_eq!(base.corrections(10).unwrap().len(), 1);
    }

    #[test]
    fn corriger_la_voix_d_un_passage() {
        let base = Base::en_memoire().unwrap();
        let moi = base.nouveau_locuteur(&[1u8; 8], true).unwrap();
        let anonyme = base.nouveau_locuteur(&[2u8; 8], false).unwrap();
        let camille = base.nouveau_locuteur(&[3u8; 8], false).unwrap();
        base.nommer_locuteur(camille, "Camille").unwrap();
        let a = base.ajouter_voix(0, 0, 1_000, "a.aac", Some(&[9u8; 8]), Some(anonyme)).unwrap();
        let b = base.ajouter_voix(0, 2_000, 3_000, "b.aac", None, Some(moi)).unwrap();
        assert_eq!(base.empreinte_du_passage(a).unwrap(), Some(vec![9u8; 8]));
        assert_eq!(base.empreinte_du_passage(b).unwrap(), None);
        // Le passage de la voix anonyme est d'Camille : elle, vide, disparaît.
        assert_eq!(base.deplacer_passage(a, Some(camille)).unwrap(), Some(anonyme));
        assert!(!base.locuteurs().unwrap().iter().any(|l| l.id == anonyme));
        // Celui de Julien devient « ? » : sa voix reste (c'est la sienne), avec un passage de moins.
        assert_eq!(base.deplacer_passage(b, None).unwrap(), Some(moi));
        let voix = base.voix_entre(0, 10_000).unwrap();
        assert_eq!((voix[0].locuteur_id, voix[1].locuteur_id), (Some(camille), None));
        assert!(base.locuteurs().unwrap().iter().any(|l| l.id == moi && l.passages == 0));
    }

    #[test]
    fn tri_des_voix_applique_puis_annule() {
        let base = Base::en_memoire().unwrap();
        let moi = base.nouveau_locuteur(&[1u8; 8], true).unwrap();
        let v2 = base.nouveau_locuteur(&[2u8; 8], false).unwrap();
        let a = base.ajouter_voix(0, 0, 1_000, "a.aac", Some(&[9u8; 8]), Some(moi)).unwrap();
        let b = base.ajouter_voix(0, 2_000, 3_000, "b.aac", Some(&[8u8; 8]), Some(moi)).unwrap();
        let c = base.ajouter_voix_avec(1, 4_000, 5_000, "c.aac", Some(&[7u8; 8]), Some(v2), false).unwrap();
        let empreintes = base.empreintes_de_voix().unwrap();
        assert_eq!(empreintes.iter().map(|e| e.id).collect::<Vec<_>>(), vec![a, b, c]);
        // b part dans une voix nouvelle, nommée Camille ; c (une vidéo) n'a plus de voix ; v2 disparaît.
        base.appliquer_tri_des_voix(
            &[],
            &[(a, Some(moi)), (b, Some(-1)), (c, None)],
            &[(moi, vec![3u8; 8], 1)],
            &[(vec![4u8; 8], 1)],
            &[v2],
        )
        .unwrap();
        let voix = base.voix_entre(0, 10_000).unwrap();
        let nouvelle = voix[1].locuteur_id.unwrap();
        assert_eq!((voix[0].locuteur_id, voix[2].locuteur_id), (Some(moi), None));
        assert!(nouvelle != moi && nouvelle != v2);
        base.nommer_locuteur(nouvelle, "Camille").unwrap();
        let apres: Vec<(i64, Option<String>)> = base.locuteurs().unwrap().into_iter().map(|l| (l.id, l.nom)).collect();
        assert_eq!(apres, vec![(moi, None), (nouvelle, Some("Camille".to_string()))]);
        assert_eq!(base.locuteurs().unwrap()[0].centre, vec![3u8; 8]);
        // Annulé : tout revient comme avant (voix et centres).
        assert!(base.annuler_tri_des_voix().unwrap());
        let voix = base.voix_entre(0, 10_000).unwrap();
        assert_eq!(
            voix.iter().map(|v| v.locuteur_id).collect::<Vec<_>>(),
            vec![Some(moi), Some(moi), Some(v2)]
        );
        let avant: Vec<(i64, Vec<u8>)> = base.locuteurs().unwrap().into_iter().map(|l| (l.id, l.centre)).collect();
        assert_eq!(avant, vec![(moi, vec![1u8; 8]), (v2, vec![2u8; 8])]);
        assert!(!base.annuler_tri_des_voix().unwrap());
    }

    #[test]
    fn rouvrir_ne_recree_pas_le_schema() {
        let dossier = std::env::temp_dir().join(format!("bscreen-base-{}", std::process::id()));
        let fichier = dossier.join("index.sqlite");
        {
            let base = Base::ouvrir(&fichier).unwrap();
            base.nouveau_morceau(&NouveauMorceau {
                ecran: 1,
                fichier: "a.mp4",
                debut_ms: 0,
                largeur: 1,
                hauteur: 1,
            })
            .unwrap();
        }
        let base = Base::ouvrir(&fichier).unwrap();
        assert_eq!(base.bilan().unwrap().morceaux, 1);
        drop(base);
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    /// L'indexation par le sens range ses vecteurs pendant que la capture écrit (deux connexions,
    /// comme dans l'app) : elle attend son tour au lieu d'échouer (« database is locked », 22/11).
    #[test]
    fn ranger_pendant_que_la_capture_ecrit() {
        let dossier = std::env::temp_dir().join(format!("bscreen-verrou-{}", std::process::id()));
        let fichier = dossier.join("index.sqlite");
        let sens = Base::ouvrir(&fichier).unwrap();
        let id = sens.texte_id("un texte à comprendre").unwrap();
        let (pris, verrou_pris) = std::sync::mpsc::channel();
        let capture = {
            let fichier = fichier.clone();
            std::thread::spawn(move || {
                let capture = Base::ouvrir(&fichier).unwrap();
                let transaction = capture.connexion.unchecked_transaction().unwrap();
                transaction.execute("INSERT INTO meta (cle, valeur) VALUES ('essai', '1')", []).unwrap();
                pris.send(()).unwrap();
                // La capture garde le verrou d'écriture un moment (une rafale de textes).
                std::thread::sleep(std::time::Duration::from_millis(300));
                transaction.commit().unwrap();
            })
        };
        verrou_pris.recv().unwrap();
        sens.ranger_sens(id, &[(0.01, vec![1, -2, 3])]).unwrap();
        capture.join().unwrap();
        assert_eq!(sens.sens_depuis(0, 10).unwrap().len(), 1);
        drop(sens);
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    #[test]
    fn voix_transcrite_et_retrouvee() {
        let base = Base::en_memoire().unwrap();
        let id = base
            .ajouter_voix(1, 10_000, 14_000, "audio/2026-11-20/systeme-101000-000.aac", None, None)
            .unwrap();
        base.ajouter_voix(0, 20_000, 21_000, "audio/2026-11-20/micro-102000-000.aac", None, None)
            .unwrap();
        let a_faire = base.voix_a_transcrire(10).unwrap();
        assert_eq!(a_faire.len(), 2);
        assert_eq!(a_faire[0].id, id);
        base.noter_transcription(&a_faire[0], "On regarde les dauphins demain ?", "[]", "apple-fr").unwrap();
        base.noter_transcription(&a_faire[1], "", "[]", "apple-fr").unwrap();
        assert!(base.voix_a_transcrire(10).unwrap().is_empty());
        let trouvees = base.chercher("dauphin", 10).unwrap();
        assert_eq!(trouvees.len(), 1);
        assert_eq!(trouvees[0].ecran, 0, "la voix est rangée sur l'« écran » 0");
        assert_eq!((trouvees[0].debut_ms, trouvees[0].fin_ms), (10_000, 14_000));
        assert_eq!(base.voix_entre(13_000, 30_000).unwrap().len(), 2);
        assert_eq!(base.bilan_voix().unwrap(), (2, 5_000));
        assert_eq!(base.oublier_voix_avant(15_000).unwrap().len(), 1);
        assert_eq!(base.bilan_voix().unwrap().0, 1);
    }

    #[test]
    fn voix_nommees_et_fusionnees() {
        let base = Base::en_memoire().unwrap();
        let moi = base.nouveau_locuteur(&[0u8; 8], true).unwrap();
        let v3 = base.nouveau_locuteur(&[1u8; 8], false).unwrap();
        let v4 = base.nouveau_locuteur(&[2u8; 8], false).unwrap();
        base.ajouter_voix(0, 0, 1_000, "a.aac", Some(&[9u8; 8]), Some(moi)).unwrap();
        base.ajouter_voix(1, 2_000, 3_000, "b.aac", None, Some(v3)).unwrap();
        base.ajouter_voix(1, 4_000, 5_000, "c.aac", None, Some(v4)).unwrap();
        let noms: Vec<Option<String>> = base.voix_entre(0, 10_000).unwrap().into_iter().map(|v| v.locuteur).collect();
        assert_eq!(noms, vec![Some("Toi".into()), Some(format!("Voix {v3}")), Some(format!("Voix {v4}"))]);
        // « c'est Camille » deux fois : les deux voix n'en font plus qu'une.
        assert_eq!(base.nommer_locuteur(v3, "Camille").unwrap(), v3);
        assert_eq!(base.nommer_locuteur(v4, " Camille ").unwrap(), v3);
        let voix = base.voix_entre(0, 10_000).unwrap();
        assert_eq!(voix[2].locuteur.as_deref(), Some("Camille"));
        assert_eq!(voix[2].locuteur_id, Some(v3));
        let locuteurs = base.locuteurs().unwrap();
        assert_eq!(locuteurs.len(), 2);
        assert_eq!(locuteurs.iter().find(|l| l.id == v3).unwrap().passages, 2);
    }

    #[test]
    fn oublier_une_periode() {
        let base = Base::en_memoire().unwrap();
        let avant = base.texte_id("vu avant").unwrap();
        let pendant = base.texte_id("mot de passe affiché pendant").unwrap();
        base.noter_apparitions(
            &[
                NouvelleApparition { texte: avant, ecran: 1, quand_ms: 1_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] },
                NouvelleApparition { texte: pendant, ecran: 1, quand_ms: 10_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] },
            ],
            &[],
            10_000,
        )
        .unwrap();
        base.ajouter_voix(0, 9_000, 11_000, "audio/x.aac", None, None).unwrap();
        base.ajouter_voix(1, 30_000, 31_000, "audio/y.aac", None, None).unwrap();
        base.noter_recherche(10_500, "google.com", "secret", "https://…").unwrap();
        let fichiers = base.oublier_periode(9_500, 20_000).unwrap();
        assert_eq!(fichiers, vec!["audio/x.aac".to_string()]);
        assert!(base.chercher("passe", 10).unwrap().is_empty(), "le texte de la période est oublié");
        assert_eq!(base.bilan_voix().unwrap().0, 1);
        assert!(base.noter_recherche(10_600, "google.com", "secret", "https://…").unwrap(), "la recherche a été oubliée");
    }

    #[test]
    fn masquage_retroactif() {
        let base = Base::en_memoire().unwrap();
        let secret = base.texte_id(&["export CLE=sk", "-ABCDEFGHIJKLMNOPQRSTU"].concat()).unwrap();
        let deja = base.texte_id("export CLE=•").unwrap();
        base.noter_apparitions(
            &[
                NouvelleApparition { texte: secret, ecran: 1, quand_ms: 1_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] },
                NouvelleApparition { texte: deja, ecran: 1, quand_ms: 2_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] },
            ],
            &[],
            2_000,
        )
        .unwrap();
        let masquer = |t: &str| t.contains("sk-").then(|| "export CLE=•".to_string());
        assert_eq!(base.remasquer(masquer).unwrap(), 1);
        assert!(base.chercher("ABCDEFGHIJKLMNOPQRSTU", 10).unwrap().is_empty(), "plus dans l'index");
        assert_eq!(base.chercher("export", 10).unwrap().len(), 2, "les deux apparitions vont au texte masqué");
        assert_eq!(base.remasquer(masquer).unwrap(), 0, "rien à refaire");
        assert_eq!(base.meta("masquage").unwrap(), None);
        base.noter_meta("masquage", "1").unwrap();
        assert_eq!(base.meta("masquage").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn sens_range_et_efface_avec_son_texte() {
        let (base, _) = base_avec_un_morceau();
        let t = base.texte_id("des escargots dans le jardin").unwrap();
        base.noter_apparitions(
            &[NouvelleApparition { texte: t, ecran: 1, quand_ms: 1_000, source: 0, vu: true, focus: false, cadre: [0.0; 4] }],
            &[],
            1_000,
        )
        .unwrap();
        let (id, contenu) = base.textes_sans_sens(0, 10).unwrap().remove(0);
        assert_eq!(contenu, "des escargots dans le jardin");
        base.ranger_sens(id, &[(0.01, vec![1, -2, 3]), (0.02, vec![4, 5, -6])]).unwrap();
        let sens = base.sens_depuis(0, 10).unwrap();
        assert_eq!(sens.len(), 2);
        assert_eq!(sens[1].vecteur, vec![4, 5, -6]);
        assert!(base.textes_sans_sens(id, 10).unwrap().is_empty());
        let trouvees = base.trouvailles_de_textes(&[(id, 0.8)], 3, 10).unwrap();
        assert_eq!(trouvees.len(), 1);
        assert!((trouvees[0].score - 0.8).abs() < 1e-6);
        // Chaque vecteur a son code (le signe de chaque nombre), relu dans l'ordre.
        let codes = base.codes_depuis(true, 0, 10).unwrap();
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[0].0, sens[0].id);
        assert_eq!(codes[0].1, vec![0b101]);
        assert_eq!(codes[1].1, vec![0b011]);
        assert_eq!(base.sens_par_ids(&[sens[1].id, 999]).unwrap().len(), 1);
        // Un vecteur rangé sans code (une ancienne version) le reçoit au rattrapage.
        base.connexion.execute("DELETE FROM codes_sens WHERE id = ?1", params![sens[0].id]).unwrap();
        assert_eq!(base.completer_codes().unwrap(), 1);
        assert_eq!(base.codes_depuis(true, 0, 10).unwrap().len(), 2);
        // Le texte oublié emporte ses vecteurs et leurs codes.
        base.oublier_textes_avant(10_000_000).unwrap();
        assert!(base.sens_depuis(0, 10).unwrap().is_empty());
        assert!(base.codes_depuis(true, 0, 10).unwrap().is_empty());
    }

    #[test]
    fn verrouiller_les_images_d_une_app() {
        let base = Base::en_memoire().unwrap();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "a.mp4", debut_ms: 0, largeur: 10, hauteur: 10 })
            .unwrap();
        for (i, (quand, app)) in [(1_000i64, "com.apple.Safari"), (2_000, "com.vscodium"), (3_000, "com.apple.Safari"), (9_000, "com.apple.Safari")]
            .iter()
            .enumerate()
        {
            base.ajouter_image(&NouvelleImage {
                morceau: m,
                quand_ms: *quand,
                position_ms: i as i64 * 1_000,
                ecran: 1,
                app_bundle: Some(app),
                app_nom: None,
                aire: 0.5,
                declenchee: false,
            })
            .unwrap();
        }
        assert_eq!(base.compter_images_apps(0, 5_000, &["com.apple.Safari"]).unwrap(), 2);
        assert_eq!(base.noter_privee_apps(0, 5_000, &["com.apple.Safari"]).unwrap(), 2);
        // Déjà verrouillées : rien de plus ; l'autre app et l'image d'après restent visibles.
        assert_eq!(base.compter_images_apps(0, 5_000, &["com.apple.Safari"]).unwrap(), 0);
        let privees: Vec<bool> = base.images_entre(0, 10_000).unwrap().iter().map(|i| i.privee).collect();
        assert_eq!(privees, vec![true, false, true, false]);
    }

    #[test]
    fn moments_prives() {
        let base = Base::en_memoire().unwrap();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "a.mp4", debut_ms: 0, largeur: 10, hauteur: 10 })
            .unwrap();
        for (i, quand) in [1_000i64, 3_000, 5_000, 7_000].iter().enumerate() {
            let id = base
                .ajouter_image(&NouvelleImage {
                    morceau: m,
                    quand_ms: *quand,
                    position_ms: i as i64 * 2_000,
                    ecran: 1,
                    app_bundle: Some("com.google.Chrome"),
                    app_nom: Some("Chrome"),
                    aire: 1.0,
                    declenchee: false,
                })
                .unwrap();
            base.noter_fenetre(id, Some("titre"), Some("https://exemple.fr")).unwrap();
        }
        // Indexée à 3 s (normale) puis à 7 s (privée) : les images de 3 s exclu à 7 s inclus.
        assert_eq!(base.noter_privee(1, 3_000, 7_000).unwrap(), 2);
        let images = base.images_entre(0, 10_000).unwrap();
        let privees: Vec<bool> = images.iter().map(|i| i.privee).collect();
        assert_eq!(privees, vec![false, false, true, true]);
        assert_eq!(images[3].url, None, "ni titre ni adresse gardés");
        // Voix : privée si elle touche une image privée, ou commence pendant un écran privé.
        assert!(!base.privee_pendant(1_500, 2_500).unwrap(), "avant la navigation privée");
        assert!(base.privee_pendant(4_000, 6_000).unwrap(), "chevauche l'image privée de 5 s");
        assert!(base.privee_pendant(7_500, 9_000).unwrap(), "commence après l'image privée de 7 s");

        // Rangées à part seulement quand la vidéo est terminée.
        assert!(base.privees_a_chiffrer(100).unwrap().is_empty(), "vidéo encore ouverte");
        base.fermer_morceau(m, 8_000, 1234).unwrap();
        let a_ranger = base.privees_a_chiffrer(100).unwrap();
        assert_eq!(a_ranger.iter().map(|p| p.quand_ms).collect::<Vec<_>>(), vec![5_000, 7_000]);
        assert_eq!(a_ranger[0].fichier, "a.mp4");
        let (id5, id7) = (a_ranger[0].id, a_ranger[1].id);
        base.noter_fichier_prive(id5, "prive/1970-01-01/5.bsp").unwrap();
        assert_eq!(base.privees_a_chiffrer(100).unwrap().len(), 1, "une seule reste dans la vidéo");
        // La frise lit l'image rangée dans son fichier chiffré ; l'effacement ne touche plus la vidéo pour elle.
        let images = base.images_entre(0, 10_000).unwrap();
        assert_eq!(images[2].fichier, "prive/1970-01-01/5.bsp");
        assert_eq!(images[3].fichier, "a.mp4");
        let emplacements = base.emplacements(&[id5, id7]).unwrap();
        assert_eq!(emplacements.iter().map(|e| e.0).collect::<Vec<_>>(), vec![id7]);
        assert_eq!(base.fichiers_prives(&[id5, id7]).unwrap(), vec!["prive/1970-01-01/5.bsp".to_string()]);
        assert_eq!(base.fichiers_prives_du_morceau(m).unwrap(), vec!["prive/1970-01-01/5.bsp".to_string()]);
    }

    #[test]
    fn lecture_seule_quand_l_app_ne_tourne_pas() {
        let dossier = std::env::temp_dir().join(format!("bscreen-lecture-{}", std::process::id()));
        let fichier = dossier.join("index.sqlite");
        {
            let base = Base::ouvrir(&fichier).unwrap();
            base.texte_id("bonjour les dauphins").unwrap();
        }
        // Base fermée proprement : plus de -wal ni de -shm (l'app ne tourne pas).
        let wal = dossier.join("index.sqlite-wal");
        assert!(!wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0);
        let base = Base::ouvrir_lecture(&fichier).expect("ouverture en lecture seule");
        assert_eq!(base.chercher("dauphins", 10).unwrap().len(), 0, "aucune apparition, mais la requête passe");
        assert!(base.texte_id("écriture").is_err(), "lecture seule : rien ne s'écrit");
        drop(base);
        std::fs::remove_dir_all(&dossier).unwrap();
    }

    #[test]
    fn bilan_jour_par_jour() {
        let base = Base::en_memoire().unwrap();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "a.mp4", debut_ms: 1_000, largeur: 1, hauteur: 1 })
            .unwrap();
        base.changer_taille_morceau(m, 2_048).unwrap();
        for k in 0..3 {
            base.ajouter_image(&NouvelleImage {
                morceau: m,
                quand_ms: 1_000 + k * 2_000,
                position_ms: k * 2_000,
                ecran: 1,
                app_bundle: None,
                app_nom: None,
                aire: 1.0,
                declenchee: false,
            })
            .unwrap();
        }
        base.ajouter_voix(0, 2_000, 5_000, "a.aac", None, None).unwrap();
        let jours = base.bilan_par_jour(0).unwrap();
        assert_eq!(jours.len(), 1);
        let j = &jours[0];
        assert_eq!((j.images, j.octets_video, j.passages_voix, j.secondes_voix), (3, 2_048, 1, 3));
    }

    #[test]
    fn vues_rangees_puis_effacees_avec_l_image() {
        let base = Base::en_memoire().unwrap();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "a.mp4", debut_ms: 0, largeur: 10, hauteur: 10 })
            .unwrap();
        let ids: Vec<i64> = (0..3)
            .map(|k| {
                base.ajouter_image(&NouvelleImage {
                    morceau: m,
                    quand_ms: 1_000 * (k + 1),
                    position_ms: 1_000 * k,
                    ecran: 1,
                    app_bundle: Some("com.apple.Safari"),
                    app_nom: Some("Safari"),
                    aire: 0.5,
                    declenchee: false,
                })
                .unwrap()
            })
            .collect();
        assert_eq!(base.images_apres(ids[0], 10).unwrap().iter().map(|i| i.id).collect::<Vec<_>>(), ids[1..].to_vec());
        for &id in &ids {
            base.ranger_vue(id, 0.01, &[1, -2, 3]).unwrap();
        }
        assert_eq!(base.compter_vues().unwrap(), 3);
        let vues = base.vues_depuis(0, 10).unwrap();
        assert_eq!(vues[1].image, ids[1]);
        assert_eq!(vues[1].vecteur, vec![1, -2, 3]);
        // Une image devenue privée perd sa vue, et n'en reprend pas.
        base.noter_privee(1, 2_500, 3_000).unwrap();
        assert_eq!(base.compter_vues().unwrap(), 2);
        base.ranger_vue(ids[2], 0.01, &[1]).unwrap();
        assert_eq!(base.compter_vues().unwrap(), 2);
        // Une image effacée emporte sa vue.
        base.supprimer_images(&[ids[0]]).unwrap();
        assert_eq!(base.compter_vues().unwrap(), 1);
        assert_eq!(base.images_par_id(&[ids[1], ids[0]]).unwrap().len(), 1);
    }

    #[test]
    fn sous_entendus_ranges_cherchables_et_effaces() {
        let base = Base::en_memoire().unwrap();
        let v = base.ajouter_voix(0, 10_000, 12_000, "a.aac", None, None).unwrap();
        let passage = base.voix_entre(10_000, 10_001).unwrap().remove(0);
        base.noter_transcription(&passage, "Il est derrière le truc.", "[]", "essai").unwrap();
        assert_eq!(base.passages_a_relire(0, 100_000).unwrap().len(), 1);
        base.ajouter_deduit(&passage, "Le chargeur est derrière le support d'écran.", 0.8, "essai", 1).unwrap();
        base.noter_relus(&[v]).unwrap();
        assert!(base.passages_a_relire(0, 100_000).unwrap().is_empty());
        // Cherchable par ses mots, au moment du passage.
        let t = base.chercher("support", 10).unwrap();
        assert_eq!((t.len(), t[0].ecran, t[0].debut_ms), (1, 0, 10_000));
        assert_eq!(base.deduits_de(&[v]).unwrap()[&v].0, "Le chargeur est derrière le support d'écran.");
        // Le passage effacé emporte sa version explicite et ce qu'on en cherchait.
        base.oublier_periode(9_000, 13_000).unwrap();
        assert!(base.deduits_de(&[v]).unwrap().is_empty());
        assert!(base.chercher("support", 10).unwrap().is_empty());
    }

    #[test]
    fn archive_et_vignettes() {
        let base = Base::en_memoire().unwrap();
        let m = base
            .nouveau_morceau(&NouveauMorceau { ecran: 1, fichier: "video/2026-11-19/ecran1-1000.mp4", debut_ms: 0, largeur: 10, hauteur: 10 })
            .unwrap();
        let ids: Vec<i64> = (0..4)
            .map(|k| {
                base.ajouter_image(&NouvelleImage {
                    morceau: m,
                    quand_ms: 30_000 * k,
                    position_ms: 30_000 * k,
                    ecran: 1,
                    app_bundle: None,
                    app_nom: None,
                    aire: 0.5,
                    declenchee: false,
                })
                .unwrap()
            })
            .collect();
        base.ajouter_voix(0, 1_000, 3_000, "audio/2026-11-19/v1.m4a", None, None).unwrap();
        // Pas encore fermé : rien de la vidéo.
        assert_eq!(base.fichiers_a_archiver(1_000_000).unwrap().len(), 1);
        base.fermer_morceau(m, 120_000, 1_000).unwrap();
        let a = base.fichiers_a_archiver(1_000_000).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].jour.as_str(), a[0].morceau), ("2026-11-19", Some(m)));
        base.noter_archive(&a[0].fichier, 900, "abc", 5).unwrap();
        assert_eq!(base.fichiers_a_archiver(1_000_000).unwrap().len(), 1);
        assert_eq!(base.archive_de(&a[0].fichier).unwrap(), Some((900, "abc".to_string())));
        assert_eq!(base.bilan_archive().unwrap(), (1, 900));
        // Vignettes : une par minute (pas de scène connue ici) ; la plus proche avant le moment.
        let pour = base.images_pour_vignettes(m).unwrap();
        assert_eq!(pour.iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[0], ids[2]]);
        for (image, position, _) in &pour {
            base.ajouter_vignette(*image, m, *position, &format!("vignettes/2026-11-19/{image}.heic")).unwrap();
        }
        let proche = base.vignette_proche(&a[0].fichier, 75_000).unwrap().unwrap();
        assert_eq!(proche, format!("vignettes/2026-11-19/{}.heic", ids[2]));
        assert_eq!(base.vignettes_des_images(&[ids[0], ids[1]]).unwrap().len(), 1);
        base.oublier_archive(&a[0].fichier).unwrap();
        assert!(base.archive_de(&a[0].fichier).unwrap().is_none());
    }
}
