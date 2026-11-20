//! Intégration avec la fenêtre native : HWND, Mica, thème de la barre de titre.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::Theme;

pub fn hwnd(window: &slint::Window) -> Option<isize> {
    let handle = window.window_handle();
    match handle.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Applique Mica et le thème clair/sombre de la barre de titre.
pub fn apply_style(hwnd: isize, theme: Option<Theme>, mica: bool) {
    let dark = theme == Some(Theme::Dark);
    let backdrop = if mica {
        fx_win::dwm::Backdrop::Mica
    } else {
        fx_win::dwm::Backdrop::None
    };
    if let Err(e) = fx_win::dwm::set_window_style(hwnd, dark, backdrop) {
        tracing::warn!("style de fenêtre DWM non appliqué : {e}");
    }
    if let Err(e) = fx_win::dwm::set_window_icon(hwnd) {
        tracing::warn!("icône de fenêtre non appliquée : {e}");
    }
}
