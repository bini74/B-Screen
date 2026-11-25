//! Captures au bon moment (phase 1.2) : juste après un clic, et quand on s'arrête de taper
//! (1 s sans touche), comme Rewind. On ne regarde que le *moment* des événements : jamais
//! quelle touche, ni où le clic a eu lieu.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};

/// Numéro de la dernière touche : une pause de frappe n'est reconnue que si aucune touche n'a suivi.
static DERNIERE_TOUCHE: AtomicU64 = AtomicU64::new(0);
pub static CLICS: AtomicU64 = AtomicU64::new(0);
pub static PAUSES_DE_FRAPPE: AtomicU64 = AtomicU64::new(0);

const PAUSE_DE_FRAPPE: Duration = Duration::from_millis(1_000);

thread_local! {
    /// Le moniteur global doit rester en vie (fil principal).
    static MONITEUR: std::cell::RefCell<Option<Retained<AnyObject>>> = const { std::cell::RefCell::new(None) };
}

/// Écoute les clics et la frappe dans toutes les apps (autorisation d'accessibilité nécessaire).
/// À appeler une fois, sur le fil principal.
pub fn surveiller() {
    let masque = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::KeyDown;
    let reaction = RcBlock::new(|evenement: NonNull<NSEvent>| {
        // SAFETY : macOS passe un événement valide pendant l'appel.
        let genre = unsafe { evenement.as_ref() }.r#type();
        if genre == NSEventType::KeyDown {
            let numero = DERNIERE_TOUCHE.fetch_add(1, Ordering::Relaxed) + 1;
            let quand = DispatchTime::try_from(PAUSE_DE_FRAPPE).unwrap_or(DispatchTime::NOW);
            let _ = DispatchQueue::main().after(quand, move || {
                if DERNIERE_TOUCHE.load(Ordering::Relaxed) == numero {
                    PAUSES_DE_FRAPPE.fetch_add(1, Ordering::Relaxed);
                    crate::capture::declencher();
                }
            });
        } else {
            CLICS.fetch_add(1, Ordering::Relaxed);
            crate::capture::declencher();
        }
    });
    let moniteur = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(masque, &reaction);
    if moniteur.is_none() {
        tracing::warn!("clics et frappe non suivis (accessibilité ?)");
    }
    MONITEUR.with(|m| *m.borrow_mut() = moniteur);
}
