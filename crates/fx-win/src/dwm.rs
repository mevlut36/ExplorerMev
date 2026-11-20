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

/// Applique à la fenêtre l'icône de l'exécutable (ressource numéro 1, voir `app.rc`), en
/// petite taille (barre de titre) et en grande taille (Alt+Tab). winit ne le fait pas : sans
/// cela la barre de titre garde l'icône générique.
pub fn set_window_icon(hwnd: isize) -> windows::core::Result<()> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON,
        SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
    };
    use windows::core::PCWSTR;
    // SAFETY: l'identifiant de ressource 1 est passé comme un pointeur (MAKEINTRESOURCE). `hwnd`
    // est la fenêtre principale (valide) ; l'instance est celle de
    // l'exécutable ; les icônes chargées sont possédées par le système tant que la fenêtre
    // existe (LoadImageW sans LR_SHARED : elles sont libérées à la fin du processus, une seule
    // fois par fenêtre ici).
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let load = |w: i32, h: i32| {
            LoadImageW(
                Some(instance.into()),
                PCWSTR(std::ptr::without_provenance::<u16>(1)),
                IMAGE_ICON,
                w,
                h,
                LR_DEFAULTCOLOR,
            )
        };
        let window = HWND(hwnd as *mut c_void);
        let small = load(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON))?;
        let big = load(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON))?;
        SendMessageW(
            window,
            WM_SETICON,
            Some(windows::Win32::Foundation::WPARAM(ICON_SMALL as usize)),
            Some(windows::Win32::Foundation::LPARAM(small.0 as isize)),
        );
        SendMessageW(
            window,
            WM_SETICON,
            Some(windows::Win32::Foundation::WPARAM(ICON_BIG as usize)),
            Some(windows::Win32::Foundation::LPARAM(big.0 as isize)),
        );
    }
    Ok(())
}
