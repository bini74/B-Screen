//! Gestes du trackpad que l'interface ne reçoit pas toute seule.
//!
//! gpui transmet le défilement à deux doigts, mais pas le pincement :
//! on l'écoute ici, directement dans les événements d'AppKit.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};

/// Écoute le pincement dans les fenêtres de B-Screen, tant que la valeur est gardée.
pub struct Pincement {
    moniteur: Retained<AnyObject>,
}

impl Pincement {
    /// `quand_pince` reçoit l'agrandissement relatif de chaque pas du geste
    /// (par exemple 0,05 = 5 % plus grand, -0,05 = 5 % plus petit).
    pub fn ecouter(quand_pince: impl Fn(f64) + 'static) -> Option<Self> {
        let bloc = RcBlock::new(move |evenement: NonNull<NSEvent>| -> *mut NSEvent {
            let evenement_ref = unsafe { evenement.as_ref() };
            quand_pince(evenement_ref.magnification());
            // On laisse passer l'événement : la fenêtre le reçoit aussi.
            evenement.as_ptr()
        });
        let moniteur = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &bloc) }?;
        Some(Self { moniteur })
    }
}

impl Drop for Pincement {
    fn drop(&mut self) {
        unsafe { NSEvent::removeMonitor(&self.moniteur) };
    }
}
