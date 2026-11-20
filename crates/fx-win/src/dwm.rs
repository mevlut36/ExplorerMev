//! Apparence Windows 11 de la fenêtre : fond Mica et barre de titre claire/sombre.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::core::BOOL;

/// Fond de fenêtre système.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    None,
    Mica,
}

/// Applique le fond et le thème de la barre de titre à la fenêtre `hwnd`.
///
/// Le fond Mica n'est visible que sous les zones transparentes de la fenêtre. Un `hwnd` invalide
/// fait simplement échouer l'appel (pas de comportement indéfini).
pub fn set_window_style(hwnd: isize, dark: bool, backdrop: Backdrop) -> windows::core::Result<()> {
    let hwnd = HWND(hwnd as *mut c_void);
    let dark = BOOL::from(dark);
    let kind: DWM_SYSTEMBACKDROP_TYPE = match backdrop {
        Backdrop::None => DWMSBT_NONE,
        Backdrop::Mica => DWMSBT_MAINWINDOW,
    };
    // SAFETY: les pointeurs désignent des variables locales vivantes pendant l'appel et la
    // taille annoncée est exactement celle du type pointé. DWM valide lui-même le HWND.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const BOOL).cast::<c_void>(),
            size_of::<BOOL>() as u32,
        )?;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&kind as *const DWM_SYSTEMBACKDROP_TYPE).cast::<c_void>(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )?;
    }
    Ok(())
}
