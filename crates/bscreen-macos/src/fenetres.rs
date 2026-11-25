//! Réglages de fenêtres que l'interface ne sait pas faire seule.

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSColor, NSScreen, NSView, NSWindowStyleMask};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
}

/// L'écran principal (celui de la barre des menus), avec le même identifiant que la capture.
pub fn ecran_principal() -> u32 {
    unsafe { CGMainDisplayID() }
}

/// Fait de la fenêtre qui contient `vue` (un NSView) un calque plein écran :
/// sans bordure ni ombre, transparent, par-dessus la barre des menus, sur son écran.
pub fn plein_ecran(vue: *mut c_void) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if vue.is_null() {
        return;
    }
    // SAFETY : `vue` vient de la poignée native d'une fenêtre ouverte, sur le fil principal.
    let vue: &NSView = unsafe { &*vue.cast::<NSView>() };
    let Some(fenetre) = vue.window() else { return };
    let ecran = fenetre.screen().or_else(|| NSScreen::mainScreen(mtm));
    fenetre.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    fenetre.setHasShadow(false);
    fenetre.setOpaque(false);
    fenetre.setBackgroundColor(Some(&NSColor::clearColor()));
    if let Some(ecran) = ecran {
        fenetre.setFrame_display(ecran.frame(), true);
    }
    fenetre.makeKeyAndOrderFront(None);
}

/// Baisse la frise au niveau des fenêtres ordinaires (`true`) le temps d'une fenêtre du système
/// (Touch ID), puis la remet par-dessus tout (`false`).
pub fn abaisser(vue: *mut c_void, bas: bool) {
    if MainThreadMarker::new().is_none() || vue.is_null() {
        return;
    }
    // SAFETY : `vue` vient de la poignée native d'une fenêtre ouverte, sur le fil principal.
    let vue: &NSView = unsafe { &*vue.cast::<NSView>() };
    if let Some(fenetre) = vue.window() {
        // 0 = fenêtres ordinaires, 101 = menus surgissants (le niveau de la frise).
        fenetre.setLevel(if bas { 0 } else { 101 });
    }
}

/// Le numéro de la fenêtre qui contient `vue` (celui de `screencapture -l`).
pub fn numero_fenetre(vue: *mut c_void) -> Option<isize> {
    MainThreadMarker::new()?;
    if vue.is_null() {
        return None;
    }
    // SAFETY : `vue` vient de la poignée native d'une fenêtre ouverte, sur le fil principal.
    let vue: &NSView = unsafe { &*vue.cast::<NSView>() };
    vue.window().map(|f| f.windowNumber())
}

/// Photographie la fenêtre qui contient `vue` dans `fichier` (PNG, sans son ni ombre). C'est
/// B-Screen qui lance `screencapture` : il a l'autorisation d'enregistrer l'écran (pour s'essayer).
pub fn photographier(vue: *mut c_void, fichier: &std::path::Path) -> bool {
    let Some(numero) = numero_fenetre(vue) else { return false };
    std::process::Command::new("/usr/sbin/screencapture")
        .arg("-x")
        .arg("-o")
        .arg(format!("-l{numero}"))
        .arg(fichier)
        .status()
        .is_ok_and(|s| s.success())
}

/// Hauteur, en haut de l'écran principal, cachée par l'encoche de la caméra (0 sans encoche).
pub fn marge_haute_ecran() -> f64 {
    let Some(mtm) = MainThreadMarker::new() else { return 0.0 };
    NSScreen::mainScreen(mtm).map(|e| e.safeAreaInsets().top).unwrap_or(0.0)
}
