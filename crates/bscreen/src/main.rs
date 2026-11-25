//! B-Screen : l'application (sans argument) et la commande `bscreen`.

mod application;
mod ask;
mod banc;
mod semaine;
mod tri_voix;
mod web;

use std::fs::{self, OpenOptions};
use std::sync::Mutex;

use anyhow::Result;
use bscreen_core::{NOM, VERSION, chemins};

const AIDE: &str = "\
B-Screen — mémoire de l'écran et de la voix

Utilisation :
  open B-Screen.app     lance l'app (icône dans la barre des menus)
  bscreen chemins       affiche où sont rangées les données
  bscreen chiffrement   état de l'image chiffrée
  bscreen migrer        copie les données dans l'image chiffrée (B-Screen quitté ; clé demandée)
  bscreen chercher \"texte\"  la recherche de la frise, sans l'ouvrir : combien, et en combien de temps
  bscreen retrier-voix --a-blanc [--nommer 3,743=Camille]  ce que changerait le tri des voix du passé
  bscreen retrier-voix [--nommer …]   le fait (l'état d'avant est gardé) ; --annuler le remet
  bscreen essai-appel   les apps qui se servent du micro, et s'il y a un appel
  bscreen corrections [--oublier N]  les corrections retenues par Ask (ou en oublier une)
  bscreen mesures [jours] le bilan de la semaine de mesures face aux objectifs du plan
  bscreen archive        où en est l'archive (serveur réglé dans les réglages)
  bscreen archiver [--a-blanc] [--age N] [--jours J]  envoie sur l'archive ce qui a plus de N jours
  bscreen archive-essai  aller-retour complet sur une seule vidéo, puis tout est remis comme avant
  bscreen indexer-images regarde tout de suite les images en attente (recherche dans les images)
  bscreen essai-images \"question\"  les moments dont l'image ressemble le plus
  bscreen banc-d-essai [--reponses] [--contexte N]  le banc d'essai de la recherche, sur une
                        semaine inventée (la vraie base n'est ni lue ni écrite)
  bscreen effacer-clair efface l'ancien dossier en clair, une fois la copie vérifiée
  bscreen version       affiche la version
  bscreen aide          affiche cette aide

Les autorisations (écran, micro, accessibilité) se donnent depuis le menu
de l'app : lancée depuis un terminal, c'est le terminal que macOS vérifierait.";

/// `bscreen archive-essai` : une vidéo part sur l'archive, en revient (vignette, grande image,
/// contenu identique), puis tout est remis comme avant.
fn essai_archive() -> Result<()> {
    use bscreen_macos::archive;
    let donnees = chemins::donnees();
    let base = bscreen_store::Base::ouvrir(&donnees.join("index.sqlite"))?;
    let choisi = base
        .fichiers_a_archiver(chrono_maintenant_ms() - 2 * 86_400_000)?
        .into_iter()
        .find(|f| f.morceau.is_some() && donnees.join(&f.fichier).is_file())
        .ok_or_else(|| anyhow::anyhow!("aucune vidéo de plus de 2 jours à essayer"))?;
    let morceau = choisi.morceau.unwrap_or_default();
    let chemin = donnees.join(&choisi.fichier);
    let original = fs::read(&chemin)?;
    let avant = archive::empreinte_de(&original);
    println!("Vidéo d'essai : {} ({} Ko)", choisi.fichier, original.len() / 1024);
    let position = base.images_pour_vignettes(morceau)?.first().map(|p| p.1).unwrap_or(0);
    let t = std::time::Instant::now();
    let b = archive::archiver_ces(&base, vec![choisi.clone()], 1, false)?;
    let parti = !chemin.exists() && base.archive_de(&choisi.fichier)?.is_some();
    println!(
        "  {} envoyée, vérifiée, effacée du Mac ({} vignette(s), {:.1} s : vignettes {} ms, chiffrement {} ms, envoi {} ms, vérification {} ms)",
        marque(parti && b.fichiers == 1),
        b.vignettes,
        t.elapsed().as_secs_f64(),
        b.ms[0],
        b.ms[1],
        b.ms[2],
        b.ms[3]
    );
    let petite = bscreen_macos::lecture::image_enregistree(&chemin, position, Some((320, 200)));
    println!("  {} vignette pour la frise, sans réseau", marque(petite.is_ok()));
    let t = std::time::Instant::now();
    let grande = bscreen_macos::lecture::image_enregistree(&chemin, position, None);
    println!(
        "  {} grande image ramenée de l'archive ({} en {:.1} s)",
        marque(grande.as_ref().is_ok_and(|i| i.largeur > 1000)),
        grande.as_ref().map(|i| format!("{}×{}", i.largeur, i.hauteur)).unwrap_or_else(|e| e.to_string()),
        t.elapsed().as_secs_f64()
    );
    let cache = donnees.join("archive-cache").join(&choisi.fichier);
    let identique = fs::read(&cache).map(|c| archive::empreinte_de(&c) == avant).unwrap_or(false);
    println!("  {} contenu ramené identique à l'original", marque(identique));
    // Tout remettre comme avant.
    archive::ramener_a_sa_place(&chemin)?;
    let remise = fs::read(&chemin).map(|c| archive::empreinte_de(&c) == avant).unwrap_or(false) && base.archive_de(&choisi.fichier)?.is_none();
    archive::effacer_distant(&choisi.fichier)?;
    for v in base.oublier_vignettes_du_morceau(morceau)? {
        let _ = fs::remove_file(donnees.join(v));
    }
    let _ = fs::remove_file(&cache);
    println!("  {} remise à sa place, effacée de l'archive, vignettes effacées", marque(remise));
    Ok(())
}

fn marque(juste: bool) -> &'static str {
    if juste { "✓" } else { "✗" }
}

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn")).collect();
    let commande = arguments.first().map(String::as_str);
    // Les commandes qui lisent les données ouvrent d'abord l'image chiffrée (clé du Trousseau).
    if !matches!(
        commande,
        None | Some(
            "chemins" | "chiffrement" | "migrer" | "effacer-clair" | "banc-d-essai" | "version" | "--version" | "-V" | "aide"
                | "--help" | "-h"
        )
    ) {
        bscreen_macos::coffre::ouvrir(false)?;
    }
    match commande {
        None => {
            journal_dans_fichier()?;
            application::lancer()
        }
        Some("chemins") => {
            println!("données   {}", chemins::donnees().display());
            println!("réglages  {}", chemins::reglages().display());
            println!("journaux  {}", chemins::journaux().display());
            Ok(())
        }
        // Essai du réencodage sur une COPIE : bscreen essai-reencodage <fichier.mp4> <position_ms>...
        Some("essai-reencodage") => {
            let fichier = std::path::PathBuf::from(arguments.get(1).map(String::as_str).unwrap_or_default());
            let positions: std::collections::HashSet<i64> =
                arguments.iter().skip(2).filter_map(|p| p.parse().ok()).collect();
            let octets = bscreen_macos::effacement::reencoder_sans(&fichier, &positions)?;
            println!("réencodé sans {} image(s) : {} Ko", positions.len(), octets / 1024);
            Ok(())
        }
        // Essai de l'OCR : bscreen essai-ocr <fichier.mp4> <position_ms> <mot>...
        Some("essai-ocr") => {
            let fichier = std::path::PathBuf::from(arguments.get(1).map(String::as_str).unwrap_or_default());
            let position: i64 = arguments.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
            let mots: Vec<String> = arguments.iter().skip(3).cloned().collect();
            let debut = std::time::Instant::now();
            let cadres = bscreen_macos::lecture::reperer_mots(&fichier, position, &mots)?;
            println!("{} cadre(s) en {} ms", cadres.len(), debut.elapsed().as_millis());
            for c in cadres.iter().take(5) {
                println!("  x={:.3} y={:.3} l={:.3} h={:.3}", c[0], c[1], c[2], c[3]);
            }
            Ok(())
        }
        // Essai de l'audio sur un fichier, sans toucher à la base : bscreen essai-voix <fichier audio> [dossier]
        // Découpe en passages de voix, écrit chaque passage en AAC, puis transcrit si l'autorisation est là.
        Some("essai-voix") => {
            use bscreen_macos::autorisations::{Autorisation, Etat};
            let fichier = std::path::PathBuf::from(arguments.get(1).map(String::as_str).unwrap_or_default());
            let dossier = arguments
                .get(2)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("bscreen-essai-voix"));
            let debut = std::time::Instant::now();
            let (taux, echantillons) = bscreen_macos::audio::lire_fichier(&fichier)?;
            let passages = bscreen_macos::audio::decouper(taux, &echantillons);
            println!(
                "{:.1} s d'audio à {taux} Hz → {} passage(s) de voix en {} ms",
                echantillons.len() as f64 / taux,
                passages.len(),
                debut.elapsed().as_millis()
            );
            let transcripteur = (Autorisation::Voix.etat() == Etat::Accordee)
                .then(bscreen_macos::transcription::Transcripteur::nouveau)
                .transpose();
            let transcripteur = match transcripteur {
                Ok(t) => t,
                Err(e) => {
                    println!("transcription impossible : {e}");
                    None
                }
            };
            if transcripteur.is_none() {
                println!("(transcription sautée : reconnaissance vocale pas autorisée pour ce processus)");
            }
            let (connues, fenetres) = bscreen_macos::classement::description();
            println!("classifieur : {fenetres}");
            let utiles: Vec<&String> = connues
                .iter()
                .filter(|e| ["speech", "music", "singing"].contains(&e.as_str()))
                .collect();
            println!("classifieur : {} étiquettes, dont {utiles:?}", connues.len());
            for (i, (debut_s, morceau)) in passages.iter().enumerate() {
                let avant = std::time::Instant::now();
                match bscreen_macos::classement::ecouter(morceau) {
                    Ok(e) => println!(
                        "  {i:02}  parole {:.2} · musique {:.2} · chant {:.2} ({}/{} s parlées, {} ms) → {}",
                        e.parole,
                        e.musique,
                        e.chant,
                        e.fenetres_parlees,
                        e.fenetres,
                        avant.elapsed().as_millis(),
                        if e.est_de_la_parole() { "gardé" } else { "écarté" }
                    ),
                    Err(e) => println!("  {i:02}  classement impossible : {e}"),
                }
                let chemin = dossier.join(format!("passage-{i:02}.aac"));
                bscreen_macos::audio::ecrire_aac(&chemin, morceau)?;
                let octets = std::fs::metadata(&chemin).map(|m| m.len()).unwrap_or(0);
                let duree = morceau.len() as f64 / f64::from(bscreen_macos::audio::TAUX);
                println!("  {i:02}  {debut_s:6.2} s → {:6.2} s  ({:.1} Ko)", debut_s + duree, octets as f64 / 1024.0);
                if let Some(t) = transcripteur.as_ref() {
                    let avant = std::time::Instant::now();
                    match t.transcrire(&chemin, &[]) {
                        Ok(tr) => {
                            println!("      « {} »  ({} ms)", tr.texte, avant.elapsed().as_millis());
                            let mots: Vec<String> = tr
                                .mots
                                .iter()
                                .map(|m| format!("{}@{:.2}", m.mot, debut_s + m.debut_s))
                                .collect();
                            println!("      {}", mots.join(" "));
                        }
                        Err(e) => println!("      transcription : {e}"),
                    }
                }
            }
            Ok(())
        }
        // Essai des empreintes de voix : bscreen essai-empreintes <fichier audio>...
        // Une empreinte par fichier, puis la similarité de chaque paire (1 = même voix).
        Some("essai-empreintes") => {
            let dossier = bscreen_macos::empreintes::dossier_modeles()
                .ok_or_else(|| anyhow::anyhow!("modèles introuvables (scripts/modeles.sh)"))?;
            let debut = std::time::Instant::now();
            let empreinteur = bscreen_macos::empreintes::Empreinteur::charger(&dossier)?;
            println!("modèles chargés en {} ms ({})", debut.elapsed().as_millis(), dossier.display());
            let mut noms = Vec::new();
            let mut empreintes = Vec::new();
            for fichier in arguments.iter().skip(1) {
                let (taux, son) = bscreen_macos::audio::lire_fichier(std::path::Path::new(fichier))?;
                let son = bscreen_macos::audio::en_16k(taux, &son);
                let avant = std::time::Instant::now();
                empreintes.push(empreinteur.empreinte(&son)?);
                let nom = std::path::Path::new(fichier)
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("{nom:12} {:.1} s  {} ms", son.len() as f64 / 16_000.0, avant.elapsed().as_millis());
                noms.push(nom);
            }
            print!("\n{:12}", "");
            for n in &noms {
                print!("{n:>11}");
            }
            println!();
            for (i, a) in empreintes.iter().enumerate() {
                print!("{:12}", noms[i]);
                for b in &empreintes {
                    print!("{:>11.2}", bscreen_core::voix_familieres::cosinus(a, b));
                }
                println!();
            }
            Ok(())
        }
        // Essai du masquage sur la vraie base, sans rien changer : bscreen essai-masquage
        // N'affiche que le voisinage des masques (jamais les secrets).
        Some("essai-masquage") => {
            let base = bscreen_store::Base::ouvrir(&chemins::donnees().join("index.sqlite"))?;
            let mut exemples = Vec::new();
            let mut total = 0usize;
            let compte = |t: &str| {
                let m = if t.starts_with("http") {
                    bscreen_core::masquage::masquer_url(t)
                } else {
                    bscreen_core::masquage::masquer(t)
                };
                match m {
                    std::borrow::Cow::Owned(m) => Some(m),
                    std::borrow::Cow::Borrowed(_) => None,
                }
            };
            for texte in base.tous_les_textes()? {
                if let Some(m) = compte(&texte) {
                    total += 1;
                    if exemples.len() < 40 {
                        exemples.push(voisinage_des_masques(&m));
                    }
                }
            }
            println!("{total} texte(s) seraient masqués");
            for e in exemples {
                println!("  {e}");
            }
            Ok(())
        }
        // Serveur MCP en lecture seule, sur l'entrée / la sortie standard (pour Claude Code) :
        //   claude mcp add b-screen -- ~/Applications/B-Screen.app/Contents/MacOS/bscreen mcp
        Some("mcp") => {
            // Recherche par le sens : le modèle se charge à la première question (l'app, elle, indexe).
            let sens: Option<bscreen_api::mcp::ChercheurSens> = bscreen_macos::sens::disponible().then(|| {
                bscreen_macos::sens::demarrer_avec(false);
                std::sync::Arc::new(|base: &bscreen_store::Base, question: &str, n: usize| {
                    bscreen_macos::sens::vecteur_question(question, std::time::Duration::from_secs(30))
                        .and_then(|v| bscreen_macos::sens::plus_proches(base, &v, n, 0.2).ok())
                        .unwrap_or_default()
                }) as bscreen_api::mcp::ChercheurSens
            });
            // Recherche dans les images : le modèle des textes se charge à la première question.
            let images: Option<bscreen_api::mcp::ChercheurImages> = bscreen_macos::images::disponible().then(|| {
                bscreen_macos::images::demarrer_avec(false);
                std::sync::Arc::new(|base: &bscreen_store::Base, question: &str, n: usize| {
                    bscreen_macos::images::vecteur_question(question, std::time::Duration::from_secs(30))
                        .and_then(|v| bscreen_macos::images::plus_proches(base, &v, n, 0.06).ok())
                        .unwrap_or_default()
                }) as bscreen_api::mcp::ChercheurImages
            });
            bscreen_api::mcp::lancer_avec(&chemins::donnees().join("index.sqlite"), sens, images)
        }
        // Export d'une journée en JSON Lines : bscreen export [aujourd'hui | hier | AAAA-MM-JJ] [fichier]
        // (par défaut : hier, dans le dossier des données, export/AAAA-MM-JJ.jsonl).
        Some("export") => {
            let quand = arguments.get(1).map(String::as_str).unwrap_or("hier");
            let (debut, _) = bscreen_api::heures::periode(quand, chrono_maintenant_ms())
                .ok_or_else(|| anyhow::anyhow!("jour incompris : « {quand} »"))?;
            let jour = bscreen_api::heures::jour_local(debut);
            let fichier = arguments
                .get(2)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| chemins::donnees().join("export").join(format!("{jour}.jsonl")));
            if let Some(dossier) = fichier.parent() {
                std::fs::create_dir_all(dossier)?;
            }
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let mut sortie = std::io::BufWriter::new(std::fs::File::create(&fichier)?);
            let n = bscreen_api::export::exporter_jour(&base, jour, &mut sortie)?;
            std::io::Write::flush(&mut sortie)?;
            println!("{n} moment(s) du {jour} → {}", fichier.display());
            Ok(())
        }
        // Essai de précision de la sélection : bscreen essai-selection <image>
        // Rend, pour chaque ligne lue, son texte et le bord gauche de chaque caractère (en pixels).
        Some("essai-selection") => {
            let fichier = std::path::PathBuf::from(arguments.get(1).map(String::as_str).unwrap_or_default());
            let largeur: f64 = arguments.get(2).and_then(|l| l.parse().ok()).unwrap_or(1.0);
            let memoire = || {
                std::process::Command::new("/bin/ps")
                    .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                    .output()
                    .ok()
                    .and_then(|s| String::from_utf8_lossy(&s.stdout).trim().parse::<u64>().ok())
                    .map(|ko| ko / 1024)
                    .unwrap_or(0)
            };
            eprintln!("mémoire avant : {} Mo", memoire());
            let debut = std::time::Instant::now();
            let lignes = bscreen_macos::ocr::lire_mots_fichier(&fichier)?;
            eprintln!("{} ligne(s) lue(s) en {} ms", lignes.len(), debut.elapsed().as_millis());
            eprintln!("mémoire après : {} Mo", memoire());
            if std::env::var_os("BSCREEN_ESSAI_DEUX_FOIS").is_some() {
                let debut = std::time::Instant::now();
                let _ = bscreen_macos::ocr::lire_mots_fichier(&fichier)?;
                eprintln!("deuxième lecture (modèle déjà chargé) : {} ms", debut.elapsed().as_millis());
            }
            for ligne in &lignes {
                let bords: Vec<String> = ligne.bords.iter().map(|b| format!("{:.1}", b * largeur)).collect();
                println!("{}\t{}", ligne.texte, bords.join(" "));
            }
            Ok(())
        }
        // Lancé par l'app une fois par démarrage du Mac : prépare le modèle d'OCR précis (le
        // système le garde ensuite en cache) sur la dernière image enregistrée, puis s'arrête.
        Some("prechauffer-ocr") => {
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let maintenant = chrono_maintenant_ms();
            if let Some(image) = base.image_avant(maintenant)? {
                let _ = bscreen_macos::lecture::lire_image_enregistree(
                    &chemins::donnees().join(&image.fichier),
                    image.position_ms,
                )?;
            }
            Ok(())
        }
        // Ce que le vocabulaire automatique de la transcription retiendrait (sans rien changer).
        Some("essai-vocabulaire") => {
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let depuis = chrono_maintenant_ms() - 7 * 86_400_000;
            let textes = base.textes_vus_depuis(depuis, 20_000)?;
            let noms = bscreen_core::vocabulaire::noms_propres(&textes, 5, 50);
            println!("{} textes lus · {} noms propres retenus :", textes.len(), noms.len());
            println!("{}", noms.join(", "));
            Ok(())
        }
        // Ce qui a été enregistré jour par jour : bscreen bilan [jours] (7 par défaut).
        Some("bilan") => {
            let jours: i64 = arguments.get(1).and_then(|j| j.parse().ok()).unwrap_or(7);
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let depuis = chrono_maintenant_ms() - jours * 86_400_000;
            let lignes = base.bilan_par_jour(depuis)?;
            println!(
                "{:<11} {:>7} {:>6} {:>9} {:>8} {:>9} {:>11} {:>10}",
                "jour", "images", "heures", "vidéo", "Mo/heure", "passages", "voix (min)", "textes"
            );
            for j in &lignes {
                let mo = j.octets_video as f64 / 1_048_576.0;
                let par_heure = if j.heures_actives > 0 { mo / f64::from(j.heures_actives) } else { 0.0 };
                println!(
                    "{:<11} {:>7} {:>6} {:>7.0} Mo {:>8.1} {:>9} {:>11.1} {:>10}",
                    j.jour,
                    j.images,
                    j.heures_actives,
                    mo,
                    par_heure,
                    format!("{}/{}", j.passages_transcrits, j.passages_voix),
                    j.secondes_voix as f64 / 60.0,
                    j.textes_vus
                );
            }
            let total_mo: f64 = lignes.iter().map(|j| j.octets_video as f64).sum::<f64>() / 1_048_576.0;
            let heures: u32 = lignes.iter().map(|j| j.heures_actives).sum();
            if heures > 0 {
                println!(
                    "\n{total_mo:.0} Mo de vidéo pour {heures} h d'activité : ≈ {:.1} Go par mois à 8 h par jour",
                    total_mo / f64::from(heures) * 8.0 * 30.0 / 1024.0
                );
            }
            Ok(())
        }
        Some("chiffrement") => {
            use bscreen_macos::coffre;
            println!("image       {} ({})", chemins::image_chiffree().display(),
                if chemins::image_chiffree().exists() { "créée" } else { "absente" });
            println!("en service  {}", if chemins::chiffrement_actif() { "oui" } else { "non (données en clair)" });
            println!("ouverte     {}", if coffre::est_ouverte() { "oui" } else { "non" });
            println!("en clair    {}", if chemins::dossier_clair().exists() { "l'ancien dossier existe encore" } else { "plus rien" });
            Ok(())
        }
        Some("migrer") => {
            if bscreen_macos::app::deja_lancee() {
                anyhow::bail!("quitte B-Screen avant la migration");
            }
            let tapee = lire_cle_sans_echo("Clé de secours : ")?;
            let cle = bscreen_macos::coffre::normaliser(&tapee).ok_or_else(|| anyhow::anyhow!("ce n'est pas une clé de secours"))?;
            let n = bscreen_macos::coffre::migrer(&cle)?;
            println!("{n} fichiers copiés et vérifiés ; clé rangée dans le Trousseau ; B-Screen utilisera l'image au prochain lancement.");
            println!("L'ancien dossier en clair est gardé : `bscreen effacer-clair` une fois la frise vérifiée.");
            Ok(())
        }
        Some("effacer-clair") => {
            bscreen_macos::coffre::ouvrir(false)?;
            let n = bscreen_macos::coffre::effacer_clair()?;
            println!("{n} fichiers en clair effacés.");
            Ok(())
        }
        // Images de navigation privée encore dans les vidéos : `--a-blanc` compte seulement ; sinon
        // elles sont chiffrées à part et retirées des vidéos (l'app le fait seule toutes les 5 min).
        Some("ranger-prive") => {
            bscreen_macos::coffre::ouvrir(false)?;
            let a_blanc = arguments.iter().any(|a| a == "--a-blanc");
            if !a_blanc && bscreen_macos::app::deja_lancee() {
                anyhow::bail!("B-Screen tourne : il range lui-même les moments privés (ou quitte-le d'abord)");
            }
            let n = bscreen_macos::prive::ranger_les_moments_prives(a_blanc)?;
            if a_blanc {
                println!("{n} images privées encore dans les vidéos (rien n'a été touché).");
            } else {
                println!("{n} images privées chiffrées à part et retirées des vidéos.");
            }
            Ok(())
        }
        // bscreen resumer [n] : résume tout de suite les n épisodes terminés les plus récents (modèle local).
        Some("resumer") => {
            bscreen_macos::coffre::ouvrir(false)?;
            let n = arguments.get(1).and_then(|n| n.parse().ok()).unwrap_or(1);
            let faits = bscreen_macos::resumes::resumer(n, true)?;
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            println!("{faits} épisode(s) résumé(s) ; {} encore sans résumé sur 7 jours.", bscreen_macos::resumes::a_resumer(&base)?.len());
            let fin = chrono::Local::now().timestamp_millis();
            for e in base.episodes_entre(fin - 7 * 86_400_000, fin)?.iter().rev().take(faits) {
                println!(
                    "\n{} → {} · {}\n{}",
                    bscreen_api::heures::lisible(e.debut_ms),
                    bscreen_api::heures::lisible(e.fin_ms),
                    e.titre,
                    e.resume
                );
            }
            Ok(())
        }
        // bscreen contexte "question" : le contexte qu'Ask préparerait pour cette question.
        Some("contexte") => {
            let question = arguments.get(1).cloned().unwrap_or_else(|| "Qu'ai-je fait ce soir ?".to_string());
            bscreen_macos::coffre::ouvrir(false)?;
            bscreen_macos::sens::demarrer_avec(false);
            bscreen_macos::sens::prechauffer();
            std::thread::sleep(std::time::Duration::from_secs(4));
            println!("{}", ask::preparer_contexte(&question));
            Ok(())
        }
        // bscreen chercher "texte" : la recherche de la frise sans l'ouvrir (deux fois : à froid, puis
        // base en cache), avec le détail des temps sur la sortie d'erreur.
        Some("chercher") => {
            let requete = arguments.get(1).cloned().unwrap_or_else(|| "bar".to_string());
            tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();
            bscreen_macos::sens::demarrer_avec(false);
            bscreen_macos::sens::prechauffer();
            bscreen_macos::images::demarrer_avec(false);
            bscreen_macos::images::prechauffer();
            std::thread::sleep(std::time::Duration::from_secs(4));
            for _ in 0..2 {
                let debut = std::time::Instant::now();
                let trouvees = application::chercher_pour_la_frise(&requete);
                let dits = trouvees.iter().filter(|t| t.ecran == 0 && !t.sens).count();
                let sens = trouvees.iter().filter(|t| t.sens).count();
                println!(
                    "« {requete} » : {} résultat(s) ({} à l'écran, {dits} dits, {sens} par le sens) en {} ms",
                    trouvees.len(),
                    trouvees.len() - dits - sens,
                    debut.elapsed().as_millis()
                );
            }
            Ok(())
        }
        // bscreen mesures [jours] : le bilan de la semaine de mesures (phase 11.3).
        Some("mesures") => semaine::bilan(arguments.get(1).and_then(|j| j.parse().ok()).unwrap_or(7)),
        // bscreen banc-d-essai [--reponses] [--contexte N] : le banc d'essai de la recherche (7.9),
        // sur une semaine inventée rangée en mémoire (la vraie base n'est ni lue ni écrite).
        Some("banc-d-essai") => banc::passer(&arguments[1..]),
        // bscreen retrier-voix [--a-blanc] [--nommer 3,743,744=Camille]… | --annuler : retrie les voix
        // du passé à partir des empreintes gardées (à blanc : rien n'est écrit).
        Some("retrier-voix") => {
            let fichier = chemins::donnees().join("index.sqlite");
            if arguments.iter().any(|a| a == "--annuler") {
                let base = bscreen_store::Base::ouvrir(&fichier)?;
                let fait = base.annuler_tri_des_voix()?;
                println!("{}", if fait { "Voix remises comme avant le dernier tri." } else { "Aucun tri à annuler." });
                return Ok(());
            }
            let a_blanc = arguments.iter().any(|a| a == "--a-blanc");
            let mut nommer = Vec::new();
            let mut suivants = arguments.iter().skip(1);
            while let Some(a) = suivants.next() {
                if a == "--nommer" {
                    let Some(texte) = suivants.next() else {
                        anyhow::bail!("--nommer : il manque identifiants=nom (par exemple 3,743=Camille)");
                    };
                    nommer.push(tri_voix::lire_nommage(texte)?);
                }
            }
            let base = if a_blanc {
                bscreen_store::Base::ouvrir_lecture(&fichier)?
            } else {
                bscreen_store::Base::ouvrir(&fichier)?
            };
            println!("{}", tri_voix::retrier(&base, a_blanc, &nommer)?);
            Ok(())
        }
        // bscreen corrections [--oublier N] : ce qu'Ask a retenu des corrections de Julien.
        Some("corrections") => {
            let base = bscreen_store::Base::ouvrir(&chemins::donnees().join("index.sqlite"))?;
            if let Some(i) = arguments.iter().position(|a| a == "--oublier") {
                let Some(id) = arguments.get(i + 1).and_then(|n| n.parse::<i64>().ok()) else {
                    anyhow::bail!("--oublier : il faut le numéro de la correction");
                };
                let fait = base.oublier_correction(id)?;
                println!("{}", if fait { "Correction oubliée." } else { "Pas de correction de ce numéro." });
                return Ok(());
            }
            let corrections = base.corrections(200)?;
            if corrections.is_empty() {
                println!("Aucune correction retenue.");
            }
            for c in corrections {
                println!("{:>4}  {}  {}", c.id, bscreen_api::heures::lisible(c.cree_ms), c.texte);
            }
            Ok(())
        }
        // bscreen essai-appel : les apps qui se servent du micro, et s'il y a un appel en cours.
        Some("essai-appel") => {
            println!("Au micro : {}", bscreen_macos::appels::apps_au_micro().join(", "));
            println!("Appel : {}", bscreen_macos::appels::app_en_appel().unwrap_or_else(|| "aucun".to_string()));
            Ok(())
        }
        // bscreen essai-ask "question" : pose la question comme le panneau Ask, et montre ce qui se passe.
        Some("essai-ask") => {
            let question = arguments.get(1).cloned().unwrap_or_else(|| "Qu'ai-je fait ce soir ?".to_string());
            // Comme le panneau : le modèle de sens se charge pendant qu'on tape (ici, 5 s).
            bscreen_macos::sens::demarrer_avec(false);
            bscreen_macos::sens::prechauffer();
            ask::preparer();
            std::thread::sleep(std::time::Duration::from_secs(5));
            let depart = std::time::Instant::now();
            let mut premier_mot = false;
            let (envoi, reception) = std::sync::mpsc::channel();
            let _arret = ask::demander(
                &question,
                None,
                Box::new(move |e| {
                    let _ = envoi.send(e);
                }),
            )?;
            for e in reception {
                match e {
                    bscreen_ui::ask::Evenement::Lecture { outil, quoi } => {
                        println!("[{:.1} s] lit : {outil} {quoi}", depart.elapsed().as_secs_f64())
                    }
                    bscreen_ui::ask::Evenement::Envoye { outil, contenu } => println!(
                        "[{:.1} s] parti du Mac : {outil}, {} caractères",
                        depart.elapsed().as_secs_f64(),
                        contenu.chars().count()
                    ),
                    bscreen_ui::ask::Evenement::Morceau(t) => {
                        use std::io::Write;
                        if !premier_mot {
                            premier_mot = true;
                            println!("[{:.1} s] premiers mots :", depart.elapsed().as_secs_f64());
                        }
                        print!("{t}");
                        let _ = std::io::stdout().flush();
                    }
                    bscreen_ui::ask::Evenement::Fini { erreur, secondes } => {
                        println!("fini en {secondes:.1} s{}", erreur.map(|e| format!(" — {e}")).unwrap_or_default());
                        break;
                    }
                }
            }
            // Le serveur Ollama lancé pour l'essai (modèle local) ne reste pas derrière.
            bscreen_macos::ollama::arreter();
            Ok(())
        }
        // Relit par l'OCR les images enregistrées sans texte (avant la phase 3).
        Some("rattraper-texte") => {
            let debut = std::time::Instant::now();
            let (images, notes) = bscreen_macos::rattrapage::rattraper(|faites, total| {
                if faites % 20 == 0 || faites == total {
                    eprint!("\r{faites}/{total} images");
                }
            })?;
            println!("\r{images} images relues, {notes} textes notés en {:.0} s", debut.elapsed().as_secs_f64());
            Ok(())
        }
        // Indexe tout de suite tout le texte pas encore passé par le modèle de sens.
        Some("indexer-sens") => {
            let encodeur = bscreen_macos::sens::Encodeur::charger()?;
            let debut = std::time::Instant::now();
            let mut total = 0;
            loop {
                let n = bscreen_macos::sens::indexer_un_lot(&encodeur, 64)?;
                if n == 0 {
                    break;
                }
                total += n;
                eprint!("\r{total} textes");
            }
            let s = debut.elapsed().as_secs_f64();
            println!("\r{total} textes indexés en {s:.0} s ({:.0} ms par texte)", s * 1000.0 / total.max(1) as f64);
            Ok(())
        }
        // bscreen archiver [--a-blanc] [--age N] [--jours J] : envoie sur l'archive du homelab ce qui a
        // plus de N jours (par défaut, le réglage : 30), au plus J journées.
        Some("archiver") => {
            let valeur = |nom: &str| {
                arguments.iter().position(|a| a == nom).and_then(|i| arguments.get(i + 1)).and_then(|v| v.parse::<u32>().ok())
            };
            let a_blanc = arguments.iter().any(|a| a == "--a-blanc");
            let age = valeur("--age").unwrap_or(bscreen_core::reglages::Reglages::charger().archive_jours.max(1));
            let jours = valeur("--jours").map_or(usize::MAX, |j| j as usize);
            let avant = chrono_maintenant_ms() - i64::from(age) * 86_400_000;
            let b = bscreen_macos::archive::archiver(avant, jours, a_blanc)?;
            println!(
                "{} : {} journée(s), {} fichier(s), {} Mo{}{}",
                if a_blanc { "À blanc (rien n'est envoyé ni effacé)" } else { "Archivé" },
                b.jours,
                b.fichiers,
                b.octets / 1_048_576,
                if a_blanc { String::new() } else { format!(", {} vignette(s)", b.vignettes) },
                if b.gardes > 0 { format!(", {} gardé(s) sur le Mac (vérification ratée)", b.gardes) } else { String::new() }
            );
            Ok(())
        }
        // bscreen archive : où en est l'archive.
        Some("archive") => {
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let (fichiers, octets) = base.bilan_archive()?;
            let derniere = base.meta("archive_derniere")?.and_then(|v| v.parse::<i64>().ok());
            let reglages = bscreen_core::reglages::Reglages::charger();
            println!(
                "Archive {} : {fichiers} fichier(s) sur le serveur d'archive ({} Mo chiffrés) ; dernière nuit : {} ; robot Teleport : {}",
                if reglages.archive_jours == 0 { "éteinte".to_string() } else { format!("après {} jours", reglages.archive_jours) },
                octets / 1_048_576,
                derniere.map_or("jamais".to_string(), bscreen_api::heures::lisible),
                if bscreen_macos::archive::disponible() { "installé" } else { "absent" }
            );
            Ok(())
        }
        // bscreen archive-essai : aller-retour complet sur une seule vidéo (la plus ancienne), puis tout
        // est remis comme avant (vidéo à sa place, rien sur l'archive, vignettes effacées).
        Some("archive-essai") => essai_archive(),
        // bscreen indexer-images : regarde tout de suite les images en attente (une par scène), comme
        // l'app le fait en fond.
        Some("indexer-images") => {
            let encodeur = bscreen_macos::images::EncodeurImage::charger()?;
            let mut etat = bscreen_macos::images::Indexation::default();
            let debut = std::time::Instant::now();
            let mut total = 0;
            loop {
                let n = bscreen_macos::images::indexer_un_lot(&encodeur, &mut etat, 256)?;
                if n == 0 {
                    break;
                }
                total += n;
                print!("\r{total} images parcourues…");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            let gardees = bscreen_macos::images::etat_index().unwrap_or(0);
            println!("\r{total} images parcourues en {:.0} s ; {gardees} images comprises en tout", debut.elapsed().as_secs_f64());
            Ok(())
        }
        // bscreen essai-images "question" : les moments dont l'image ressemble le plus (cosinus,
        // probabilité selon SigLIP 2), sans l'app ni la page (seulement l'heure et la ressemblance).
        Some("essai-images") => {
            let question = arguments.get(1).cloned().unwrap_or_else(|| "une voiture rouge".to_string());
            let texte = bscreen_macos::images::EncodeurTexte::charger()?;
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let debut = std::time::Instant::now();
            let v = texte.vecteur(&question)?;
            let proches = bscreen_macos::images::plus_proches(&base, &v, 20, -1.0)?;
            println!("« {question} » : {} images comprises, {} ms", base.compter_vues()?, debut.elapsed().as_millis());
            let images = base.images_par_id(&proches.iter().map(|p| p.0).collect::<Vec<_>>())?;
            for (id, cosinus) in &proches {
                let Some(i) = images.iter().find(|i| i.id == *id) else { continue };
                println!(
                    "{cosinus:.3}  p {:.4}  {}{}",
                    bscreen_macos::images::probabilite(*cosinus),
                    bscreen_api::heures::lisible(i.quand_ms),
                    if i.privee { "  (privé)" } else { "" }
                );
            }
            Ok(())
        }
        // bscreen essai-sens "question"  ou  bscreen essai-sens questions.json (banc : [[id, question], …])
        Some("essai-sens") => {
            let encodeur = bscreen_macos::sens::Encodeur::charger()?;
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let argument = arguments.get(1).cloned().unwrap_or_default();
            if argument.ends_with(".json") {
                let questions: Vec<(serde_json::Value, String)> = serde_json::from_slice(&fs::read(&argument)?)?;
                let mut rangs = Vec::new();
                let debut = std::time::Instant::now();
                for (cible, question) in &questions {
                    let Some(cible) = cible.as_i64() else { continue };
                    let v = encodeur.question(question)?;
                    let proches = bscreen_macos::sens::plus_proches(&base, &v, 10_000, -1.0)?;
                    let rang = proches.iter().position(|(t, _)| *t == cible).map(|r| r + 1);
                    let score = proches.iter().find(|(t, _)| *t == cible).map(|p| p.1).unwrap_or(0.0);
                    println!("{:>5}  {score:.3}  (1er {:.3})  {question}", rang.map(|r| r.to_string()).unwrap_or("-".into()), proches[0].1);
                    rangs.push(rang.unwrap_or(usize::MAX));
                }
                let n = rangs.len() as f64;
                let part = |k: usize| rangs.iter().filter(|&&r| r <= k).count() as f64 / n * 100.0;
                let mrr = rangs.iter().map(|&r| if r == usize::MAX { 0.0 } else { 1.0 / r as f64 }).sum::<f64>() / n;
                println!("\n1er {:.0} %  top5 {:.0} %  top20 {:.0} %  MRR {mrr:.2}  ({:.0} ms par question)",
                    part(1), part(5), part(20), debut.elapsed().as_secs_f64() * 1000.0 / n);
            } else {
                let debut = std::time::Instant::now();
                let v = encodeur.question(&argument)?;
                let proches = bscreen_macos::sens::plus_proches(&base, &v, 10, -1.0)?;
                println!("({} ms)", debut.elapsed().as_millis());
                for (t, score) in proches {
                    let texte = base.trouvailles_de_textes(&[(t, score)], 1, 1)?.pop().map(|t| t.texte).unwrap_or_default();
                    let court: String = texte.replace('\n', " ").chars().take(110).collect();
                    println!("{score:.3}  {court}");
                }
            }
            Ok(())
        }
        Some("essai-index") => {
            // L'index compact (bits de signe + départage) face à la recherche exacte, sur les
            // recherches tapées par Julien : rappel des 20 premiers. Rien n'est écrit ; seuls des
            // chiffres sont affichés.
            use bscreen_core::index_compact::{IndexCompact, code, code_octets};
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let mut questions: Vec<String> = base.recherches_entre(0, i64::MAX)?.into_iter().map(|r| r.requete).collect();
            questions.sort();
            questions.dedup();
            // (id, clé à regrouper, échelle, vecteur)
            type Ligne = (i64, i64, f32, Vec<i8>);
            fn evaluer(nom: &str, lignes: &[Ligne], vecteurs_questions: &[Vec<f32>], tronque: Option<usize>, centrer: bool) {
                evaluer_avec(nom, lignes, vecteurs_questions, tronque, centrer, false)
            }
            fn evaluer_avec(nom: &str, lignes: &[Ligne], vecteurs_questions: &[Vec<f32>], tronque: Option<usize>, centrer: bool, fins: bool) {
                let mut index = IndexCompact::default();
                let dim = lignes.first().map_or(0, |l| l.3.len());
                let mut centre = vec![0f32; dim];
                if centrer {
                    for (_, _, e, v) in lignes {
                        for (c, &x) in centre.iter_mut().zip(v) { *c += f32::from(x) * e; }
                    }
                    for c in &mut centre { *c /= lignes.len().max(1) as f32; }
                }
                for (id, _, e, v) in lignes {
                    if centrer {
                        let d: Vec<f32> = v.iter().zip(&centre).map(|(&x, c)| f32::from(x) * e - c).collect();
                        index.ajouter(*id, &code(&d));
                        continue;
                    }
                    match tronque {
                        Some(n) => index.ajouter(*id, &code_octets(&v[..n])),
                        None => index.ajouter(*id, &code_octets(v)),
                    }
                }
                let par_id: std::collections::HashMap<i64, usize> = lignes.iter().enumerate().map(|(i, l)| (l.0, i)).collect();
                let score = |q: &[f32], i: usize| lignes[i].3.iter().zip(q).map(|(&a, &b)| f32::from(a) * b).sum::<f32>() * lignes[i].2;
                let premiers = |q: &[f32], quels: &mut dyn Iterator<Item = usize>| -> Vec<i64> {
                    let mut meilleurs: std::collections::HashMap<i64, f32> = std::collections::HashMap::new();
                    for i in quels {
                        let s = score(q, i);
                        let m = meilleurs.entry(lignes[i].1).or_insert(f32::MIN);
                        *m = m.max(s);
                    }
                    let mut l: Vec<(i64, f32)> = meilleurs.into_iter().collect();
                    l.sort_by(|a, b| b.1.total_cmp(&a.1));
                    l.into_iter().take(20).map(|p| p.0).collect()
                };
                print!("{nom} ({} vecteurs, {} Mo en codes) :", lignes.len(), index.octets() / 1_048_576);
                for k in [200, 500, 1000, 2000, 4000] {
                    let (mut rappel, mut ms) = (0.0, 0.0);
                    for q in vecteurs_questions {
                        let exacts = premiers(q, &mut (0..lignes.len()));
                        let debut = std::time::Instant::now();
                        let qc = match tronque { Some(n) => code(&q[..n]), None => code(q) };
                        let candidats = if fins { index.candidats_fins(q, k) } else { index.candidats(&qc, k) };
                        let trouves = premiers(q, &mut candidats.iter().map(|id| par_id[id]));
                        ms += debut.elapsed().as_secs_f64() * 1000.0;
                        rappel += exacts.iter().filter(|e| trouves.contains(e)).count() as f64 / exacts.len().max(1) as f64;
                    }
                    let n = vecteurs_questions.len().max(1) as f64;
                    print!("  K={k} : {:.1} % ({:.1} ms)", rappel / n * 100.0, ms / n);
                }
                println!();
            }
            // Le sens.
            let mut lignes: Vec<Ligne> = Vec::new();
            loop {
                let lot = base.sens_depuis(lignes.last().map_or(0, |l| l.0), 50_000)?;
                if lot.is_empty() { break; }
                lignes.extend(lot.into_iter().map(|s| (s.id, s.texte, s.echelle, s.vecteur)));
            }
            let encodeur = bscreen_macos::sens::Encodeur::charger()?;
            let vq: Vec<Vec<f32>> = questions.iter().filter_map(|q| encodeur.question(q).ok()).collect();
            println!("{} questions", vq.len());
            evaluer("sens, 768 bits", &lignes, &vq, None, false);
            evaluer("sens, 768 bits centrés", &lignes, &vq, None, true);
            evaluer("sens, 256 bits", &lignes, &vq, Some(256), false);
            drop(encodeur);
            // Les images.
            let mut vues: Vec<Ligne> = Vec::new();
            loop {
                let lot = base.vues_depuis(vues.last().map_or(0, |l| l.0), 50_000)?;
                if lot.is_empty() { break; }
                vues.extend(lot.into_iter().map(|v| (v.id, v.image, v.echelle, v.vecteur)));
            }
            let texte = bscreen_macos::images::EncodeurTexte::charger()?;
            let vq: Vec<Vec<f32>> = questions.iter().filter_map(|q| texte.vecteur(q).ok()).collect();
            evaluer("images, 768 bits", &vues, &vq, None, false);
            evaluer("images, 768 bits centrés", &vues, &vq, None, true);
            evaluer_avec("images, centrés, question en nombres", &vues, &vq, None, true, true);
            evaluer_avec("images, question en nombres", &vues, &vq, None, false, true);
            Ok(())
        }
        Some("essai-lecture") => {
            // bscreen essai-lecture <vidéo> [pas_ms] : temps de relecture des grandes images de la frise.
            let fichier = std::path::PathBuf::from(arguments.get(1).ok_or_else(|| anyhow::anyhow!("vidéo"))?);
            let pas: i64 = arguments.get(2).and_then(|a| a.parse().ok()).unwrap_or(9_000);
            let duree = bscreen_macos::lecture::duree_lisible_ms(&fichier).unwrap_or(0);
            let positions: Vec<i64> = (0..).map(|k| k * pas).take_while(|&p| p < duree).take(30).collect();
            for ligne in bscreen_macos::lecture::chronometrer_lecture(&fichier, &positions)? {
                println!("{ligne}");
            }
            Ok(())
        }
        Some("verifier-sens") => {
            // Après un changement de modèle : redonne-t-il les vecteurs déjà rangés ? (sinon tout
            // l'index serait à refaire). Rien n'est écrit ; seuls des chiffres sont affichés.
            let n: usize = arguments.get(1).and_then(|a| a.parse().ok()).unwrap_or(300);
            let encodeur = bscreen_macos::sens::Encodeur::charger()?;
            let base = bscreen_store::Base::ouvrir_lecture(&chemins::donnees().join("index.sqlite"))?;
            let echantillon = base.echantillon_sens(n)?;
            let debut = std::time::Instant::now();
            let mut cosinus: Vec<f32> = Vec::new();
            let mut decoupes_autrement = 0;
            for (rangee, contenu) in &echantillon {
                let vecteurs = encodeur.texte(contenu)?;
                // Un texte en plusieurs morceaux aujourd'hui a pu être découpé autrement à l'époque
                // (256 jetons par morceau avant le 24/11/2026) : on ne compare que les textes d'un morceau.
                if vecteurs.len() != 1 {
                    decoupes_autrement += 1;
                    continue;
                }
                let nouveau = &vecteurs[0];
                let ancien: Vec<f32> = rangee.vecteur.iter().map(|&o| f32::from(o) * rangee.echelle).collect();
                let norme = ancien.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
                cosinus.push(ancien.iter().zip(nouveau).map(|(a, b)| a * b).sum::<f32>() / norme);
            }
            cosinus.sort_by(f32::total_cmp);
            if cosinus.is_empty() {
                println!("aucun vecteur rangé");
                return Ok(());
            }
            let moyenne = cosinus.iter().sum::<f32>() / cosinus.len() as f32;
            if decoupes_autrement > 0 {
                println!("{decoupes_autrement} textes en plusieurs morceaux laissés de côté");
            }
            println!(
                "{} textes : cosinus ancien / nouveau modèle — pire {:.5}, 1 % {:.5}, médiane {:.5}, moyenne {moyenne:.5} ({:.0} ms par texte)",
                cosinus.len(),
                cosinus[0],
                cosinus[cosinus.len() / 100],
                cosinus[cosinus.len() / 2],
                debut.elapsed().as_secs_f64() * 1000.0 / cosinus.len() as f64
            );
            Ok(())
        }
        Some("version" | "--version" | "-V") => {
            println!("{NOM} {VERSION}");
            Ok(())
        }
        Some("aide" | "--help" | "-h") => {
            println!("{AIDE}");
            Ok(())
        }
        Some(autre) => {
            eprintln!("commande inconnue : {autre}\n\n{AIDE}");
            std::process::exit(2);
        }
    }
}

/// En mode app, les messages vont dans `~/Library/Logs/B-Screen/bscreen.log`.
fn journal_dans_fichier() -> Result<()> {
    let dossier = chemins::journaux();
    fs::create_dir_all(&dossier)?;
    let fichier = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dossier.join("bscreen.log"))?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(false)
        .with_writer(Mutex::new(fichier))
        .init();
    Ok(())
}

/// Le texte réduit à 25 caractères de part et d'autre de chaque masque.
fn voisinage_des_masques(texte: &str) -> String {
    let caracteres: Vec<char> = texte.chars().collect();
    let masque: Vec<char> = bscreen_core::masquage::MASQUE.chars().collect();
    let mut morceaux = Vec::new();
    let mut i = 0;
    while i + masque.len() <= caracteres.len() {
        if caracteres[i..i + masque.len()] == masque[..] {
            let a = i.saturating_sub(25);
            let b = (i + masque.len() + 25).min(caracteres.len());
            morceaux.push(caracteres[a..b].iter().collect::<String>().replace('\n', " "));
            i += masque.len();
        } else if caracteres[i] == '•' && caracteres.get(i + 4) == Some(&' ') {
            // Carte : « •••• •••• •••• 1234 »
            let a = i.saturating_sub(25);
            let b = (i + 19 + 25).min(caracteres.len());
            morceaux.push(caracteres[a..b].iter().collect::<String>().replace('\n', " "));
            i += 19;
        } else {
            i += 1;
        }
    }
    morceaux.join(" … ")
}

fn chrono_maintenant_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lit une ligne au terminal sans l'afficher (pour la clé de secours).
fn lire_cle_sans_echo(invite: &str) -> Result<String> {
    use std::io::Write;
    eprint!("{invite}");
    std::io::stderr().flush()?;
    let fd = libc::STDIN_FILENO;
    let mut avant: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY : tcgetattr / tcsetattr n'écrivent que dans les structures données.
    let terminal = unsafe { libc::tcgetattr(fd, &mut avant) } == 0;
    if terminal {
        let mut sans_echo = avant;
        sans_echo.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &sans_echo) };
    }
    let mut ligne = String::new();
    let lu = std::io::stdin().read_line(&mut ligne);
    if terminal {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &avant) };
        eprintln!();
    }
    lu?;
    Ok(ligne.trim().to_string())
}
