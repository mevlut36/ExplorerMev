//! Appels Win32 du prototype. Dans le produit, tout ceci vivra dans `fx-win`.

use std::ffi::c_void;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use windows::Win32::Foundation::{FILETIME, HWND};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::Controls::MARGINS;
use windows::core::BOOL;

fn hwnd_of(window: &slint::Window) -> Option<HWND> {
    let handle = window.window_handle();
    let raw = handle.window_handle().ok()?.as_raw();
    match raw {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut c_void)),
        _ => None,
    }
}

/// Applique le fond Mica et le thème de la barre de titre.
pub fn apply_mica(window: &slint::Window, extend_frame: bool) -> windows::core::Result<()> {
    let Some(hwnd) = hwnd_of(window) else {
        return Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_HANDLE,
        ));
    };
    let dark = window
        .with_winit_window(|w| w.theme() == Some(slint::winit_030::winit::window::Theme::Dark))
        .unwrap_or(false);
    let dark = BOOL::from(dark);
    let backdrop = DWMSBT_MAINWINDOW;
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: `hwnd` provient de la fenêtre winit vivante (nous sommes dans son callback de rendu,
    // sur son thread). Les pointeurs passés désignent des variables locales vivantes pendant
    // l'appel, et la taille annoncée est exactement celle du type pointé.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        )?;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const c_void,
            size_of_val(&backdrop) as u32,
        )?;
        if extend_frame {
            DwmExtendFrameIntoClientArea(hwnd, &margins)?;
        }
    }
    Ok(())
}

fn filetime_100ns(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// Millisecondes écoulées depuis la création du processus (horloge système précise).
pub fn ms_since_process_start() -> Option<f64> {
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    // SAFETY: pseudo-handle du processus courant, toujours valide ; les quatre pointeurs
    // désignent des FILETIME locaux initialisés.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;
    // SAFETY: fonction sans précondition.
    let now = unsafe { GetSystemTimePreciseAsFileTime() };
    let delta = filetime_100ns(now).checked_sub(filetime_100ns(creation))?;
    Some(delta as f64 / 10_000.0)
}

#[derive(Clone, Copy, Debug)]
pub struct Mem {
    pub working_set_mb: f64,
    pub private_mb: f64,
}

pub fn memory() -> Option<Mem> {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` est une PROCESS_MEMORY_COUNTERS_EX dont `cb` annonce la taille réelle ;
    // l'API accepte la version étendue via un pointeur sur la structure de base.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            c.cb,
        )
    }
    .ok()?;
    Some(Mem {
        working_set_mb: c.WorkingSetSize as f64 / 1_048_576.0,
        private_mb: c.PrivateUsage as f64 / 1_048_576.0,
    })
}

pub fn refresh_rate_hz(window: &slint::Window) -> Option<f64> {
    window
        .with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz()))
        .flatten()
        .map(|mhz| mhz as f64 / 1000.0)
}

/// Temps CPU total (noyau + utilisateur) consommé par le processus, en millisecondes.
pub fn process_cpu_ms() -> Option<f64> {
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    // SAFETY: voir `ms_since_process_start`.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;
    Some((filetime_100ns(kernel) + filetime_100ns(user)) as f64 / 10_000.0)
}
