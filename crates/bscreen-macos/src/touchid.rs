//! Touch ID (ou le mot de passe du Mac) pour déverrouiller les moments privés (phase 6.2).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Mutex;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};

thread_local! {
    /// Le contexte en cours : il doit vivre jusqu'à la réponse.
    static CONTEXTE: RefCell<Option<Retained<LAContext>>> = const { RefCell::new(None) };
}

/// Demande Touch ID (le mot de passe du Mac en secours). `vue` : la vue native de la frise, baissée
/// le temps de la demande pour que la fenêtre du système passe devant. Fil principal ; `reponse`
/// est appelée sur le fil principal.
pub fn demander(vue: *mut c_void, raison: &str, reponse: Box<dyn FnOnce(bool) + Send>) {
    let contexte = unsafe { LAContext::new() };
    let politique = LAPolicy::DeviceOwnerAuthentication;
    if let Err(e) = unsafe { contexte.canEvaluatePolicy_error(politique) } {
        tracing::warn!(erreur = %e.localizedDescription(), "Touch ID indisponible");
        reponse(false);
        return;
    }
    crate::fenetres::abaisser(vue, true);
    let adresse_vue = vue as usize;
    let reponse = Mutex::new(Some(reponse));
    let bloc = RcBlock::new(move |accorde: Bool, erreur: *mut NSError| {
        let accorde = accorde.as_bool();
        if let Some(e) = unsafe { erreur.as_ref() } {
            tracing::info!(code = e.code(), "Touch ID : pas de déverrouillage");
        }
        let Some(reponse) = reponse.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        DispatchQueue::main().exec_async(move || {
            crate::fenetres::abaisser(adresse_vue as *mut c_void, false);
            CONTEXTE.with(|c| c.borrow_mut().take());
            reponse(accorde);
        });
    });
    unsafe {
        contexte.evaluatePolicy_localizedReason_reply(politique, &NSString::from_str(raison), &bloc);
    }
    CONTEXTE.with(|c| *c.borrow_mut() = Some(contexte));
}
