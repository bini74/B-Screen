//! Génère les icônes de B-Screen à partir des SVG du logo :
//! - `assets/AppIcon.icns` (icône de l'app, via `iconutil` de macOS) ;
//! - `assets/barre-36.rgba` + `assets/barre-36.png` (icône « modèle » de la barre des menus).
//!
//! Lancer depuis la racine du dépôt : `cargo run -p icones`.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use resvg::{tiny_skia, usvg};

fn rendre(svg: &Path, taille: u32) -> Result<tiny_skia::Pixmap> {
    let donnees = fs::read(svg).with_context(|| format!("lecture de {}", svg.display()))?;
    let arbre = usvg::Tree::from_data(&donnees, &usvg::Options::default())?;
    let mut image = tiny_skia::Pixmap::new(taille, taille).context("taille d'image invalide")?;
    let echelle = taille as f32 / arbre.size().width();
    resvg::render(
        &arbre,
        tiny_skia::Transform::from_scale(echelle, echelle),
        &mut image.as_mut(),
    );
    Ok(image)
}

fn main() -> Result<()> {
    let assets = Path::new("assets");
    if !assets.join("logo-strates.svg").exists() {
        bail!("à lancer depuis la racine du dépôt b-screen");
    }

    // 1. Icône de l'app : un .iconset puis iconutil.
    let jeu = Path::new("target/AppIcon.iconset");
    let _ = fs::remove_dir_all(jeu);
    fs::create_dir_all(jeu)?;
    // Jusqu'à 32 px, le dessin simplifié (une strate, flèches épaisses) : le complet y devient
    // une tache (les flèches disparaissaient à 16 px).
    for (nom, taille) in [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ] {
        let dessin = if taille <= 32 { "logo-strates-petit.svg" } else { "logo-strates.svg" };
        rendre(&assets.join(dessin), taille)?.save_png(jeu.join(nom))?;
    }
    let statut = Command::new("/usr/bin/iconutil")
        .args(["-c", "icns", "-o"])
        .arg(assets.join("AppIcon.icns"))
        .arg(jeu)
        .status()?;
    if !statut.success() {
        bail!("iconutil a échoué");
    }

    // 2. Icône de la barre des menus : 36 px = 18 points sur un écran Retina.
    let barre = rendre(&assets.join("barre-strates.svg"), 36)?;
    barre.save_png(assets.join("barre-36.png"))?;
    // tray-icon attend du RGBA « droit » ; tiny-skia stocke du RGBA prémultiplié.
    let mut rgba = Vec::with_capacity(36 * 36 * 4);
    for pixel in barre.pixels() {
        let c = pixel.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    fs::write(assets.join("barre-36.rgba"), rgba)?;

    println!("icônes générées dans assets/");
    Ok(())
}
