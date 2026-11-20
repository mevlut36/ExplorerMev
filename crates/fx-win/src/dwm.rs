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

/// Les applications de l'utilisateur sont-elles en thème sombre (réglage Windows) ? Sert à
/// choisir les couleurs dès la première image, avant que winit ne rapporte le thème.
pub fn system_prefers_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::{PCWSTR, w};
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: chaînes littérales terminées par un nul ; sortie : un DWORD local de taille transmise.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    let _ = PCWSTR::null();
    // Valeur absente (ancien Windows) : sombre, comme avant.
    r.0 != 0 || value == 0
}

/// Amène la fenêtre au premier plan (et la restaure si elle est réduite). Si Windows refuse le
/// focus, elle passe au moins devant les autres fenêtres. `true` si elle a pris le focus.
pub fn bring_to_front(hwnd: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, SW_RESTORE, SWP_NOMOVE, SWP_NOSIZE,
        SetForegroundWindow, SetWindowPos, ShowWindow,
    };
    let window = HWND(hwnd as *mut c_void);
    // SAFETY: `hwnd` est la fenêtre principale de ce processus ; appels sans pointeur conservé.
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let _ = BringWindowToTop(window);
        if SetForegroundWindow(window).as_bool() {
            return true;
        }
        // Focus refusé : on la place au-dessus des autres sans la laisser « toujours au premier plan ».
        let _ = SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        let _ = SetWindowPos(window, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    }
    false
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
