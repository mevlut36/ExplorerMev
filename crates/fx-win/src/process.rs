//! Mesures du processus courant (démarrage, CPU, mémoire) et console.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn times() -> Option<(u64, u64, u64)> {
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    // SAFETY: pseudo-handle du processus courant, toujours valide ; quatre FILETIME locaux.
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
    Some((ft(creation), ft(kernel), ft(user)))
}

/// Millisecondes écoulées depuis la création du processus.
pub fn ms_since_process_start() -> Option<f64> {
    let (creation, _, _) = times()?;
    // SAFETY: fonction sans précondition.
    let now = ft(unsafe { GetSystemTimePreciseAsFileTime() });
    Some(now.checked_sub(creation)? as f64 / 10_000.0)
}

/// Temps CPU total (noyau + utilisateur) consommé, en millisecondes.
pub fn cpu_ms() -> Option<f64> {
    let (_, k, u) = times()?;
    Some((k + u) as f64 / 10_000.0)
}

/// Mémoire du processus, en octets.
#[derive(Clone, Copy, Debug, Default)]
pub struct Memory {
    pub working_set: u64,
    pub private: u64,
}

pub fn memory() -> Option<Memory> {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` est une PROCESS_MEMORY_COUNTERS_EX dont `cb` annonce la taille réelle ;
    // l'API accepte la version étendue via un pointeur sur la structure de base.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    }
    .ok()?;
    Some(Memory {
        working_set: c.WorkingSetSize as u64,
        private: c.PrivateUsage as u64,
    })
}

/// Annule l'E/S synchrone en cours sur le thread `thread` (HANDLE brut). Renvoie `false` s'il
/// n'y en avait pas. Utilisé pour débloquer un listing sur un partage réseau injoignable.
pub fn cancel_synchronous_io(thread: isize) -> bool {
    // SAFETY: le HANDLE vient d'un JoinHandle vivant détenu par l'appelant ; l'API se contente
    // d'annuler les E/S en attente de ce thread.
    unsafe {
        windows::Win32::System::IO::CancelSynchronousIo(windows::Win32::Foundation::HANDLE(thread as *mut _))
    }
    .is_ok()
}

/// Rattache le processus (sous-système GUI) à la console du parent, si elle existe, pour que
/// les sorties de `--bench-startup` y apparaissent.
pub fn attach_parent_console() -> bool {
    // SAFETY: aucun pointeur ; échoue proprement s'il n'y a pas de console parente.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}
