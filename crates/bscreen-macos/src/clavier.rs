//! Le clavier de Julien (AZERTY, « French - numerical ») : quelle touche tape une lettre. Les
//! raccourcis globaux se règlent par position de touche, celle d'un clavier QWERTY : « A » y est
//! la touche qui tape « q » sur un AZERTY (c'est pourquoi ⌥⇧A n'ouvrait jamais Ask).

use std::ffi::c_void;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, cle: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        disposition: *const c_void,
        touche: u16,
        action: u16,
        modificateurs: u32,
        type_clavier: u32,
        options: u32,
        etat_touche_morte: *mut u32,
        longueur_max: usize,
        longueur: *mut usize,
        texte: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(donnees: *const c_void) -> *const u8;
    fn CFRelease(objet: *const c_void);
}

/// kUCKeyActionDisplay : ce qu'affiche la touche.
const AFFICHAGE: u16 = 3;
/// kUCKeyTranslateNoDeadKeysMask
const SANS_TOUCHE_MORTE: u32 = 1;

/// Le code macOS (kVK_…) de la touche qui tape `lettre` dans la disposition actuelle du clavier.
/// À appeler sur le fil principal (les sources de saisie l'exigent).
pub fn touche_de(lettre: char) -> Option<u16> {
    let cherchee: String = lettre.to_lowercase().collect();
    // SAFETY : fonctions documentées de Carbon ; la source (règle « copy ») est rendue à la fin,
    // les données de disposition lui appartiennent et ne servent que pendant ce temps.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let donnees = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let trouvee = if donnees.is_null() {
            None
        } else {
            let disposition = CFDataGetBytePtr(donnees).cast::<c_void>();
            let type_clavier = u32::from(LMGetKbdType());
            (0u16..128).find(|&touche| {
                let (mut etat, mut longueur, mut texte) = (0u32, 0usize, [0u16; 4]);
                let lu = UCKeyTranslate(
                    disposition,
                    touche,
                    AFFICHAGE,
                    0,
                    type_clavier,
                    SANS_TOUCHE_MORTE,
                    &mut etat,
                    texte.len(),
                    &mut longueur,
                    texte.as_mut_ptr(),
                );
                lu == 0 && longueur == 1 && String::from_utf16_lossy(&texte[..1]).to_lowercase() == cherchee
            })
        };
        CFRelease(source.cast_const());
        trouvee
    }
}
