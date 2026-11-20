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

/// Passe le thread courant en mode arrière-plan (priorités processeur, disque et mémoire
/// abaissées) : pour les travaux longs qui ne doivent pas gêner l'utilisateur.
pub fn background_thread() -> bool {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    // SAFETY: pseudo-handle du thread courant, toujours valide.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }.is_ok()
}

/// Le processus tourne-t-il dans un paquet MSIX ? Dans ce cas les écritures sous `HKCU` et dans
/// `%APPDATA%` / `%LOCALAPPDATA%` sont, par défaut, redirigées vers un emplacement privé du
/// paquet (voir [`registry_is_virtualized`]).
pub fn is_packaged() -> bool {
    use windows::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER};
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    // SAFETY: longueur locale ; aucun tampon (la fonction renvoie alors la taille nécessaire,
    // ERROR_INSUFFICIENT_BUFFER, si le processus a une identité de paquet).
    let r = unsafe { GetCurrentPackageFullName(&mut len, None) };
    r != APPMODEL_ERROR_NO_PACKAGE && (r == ERROR_INSUFFICIENT_BUFFER || r.0 == 0)
}

/// Dossier d'installation du paquet (`C:\Program Files\WindowsApps\…`), s'il y en a un.
pub fn package_path() -> Option<std::path::PathBuf> {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackagePath;
    use windows::core::PWSTR;
    let mut len = 0u32;
    // SAFETY: longueur locale, aucun tampon : la fonction renvoie la taille nécessaire.
    let r = unsafe { GetCurrentPackagePath(&mut len, None) };
    if r != ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: tampon local de `len` unités UTF-16, taille transmise.
    let r = unsafe { GetCurrentPackagePath(&mut len, Some(PWSTR(buf.as_mut_ptr()))) };
    if r.0 != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..end])))
}

/// Le manifeste désactive-t-il la virtualisation d'écriture du registre
/// (`<desktop6:RegistryWriteVirtualization>disabled</…>`) ?
pub fn manifest_disables_registry_virtualization(manifest: &str) -> bool {
    manifest
        .to_ascii_lowercase()
        .contains("registrywritevirtualization>disabled<")
}

/// Les écritures dans le registre sont-elles redirigées vers un emplacement privé du paquet ?
/// `false` hors paquet, et dans un paquet dont le manifeste désactive la virtualisation. Dans un
/// paquet qui la garde, une clé écrite (par exemple celle de l'option « explorateur par
/// défaut ») ne serait jamais vue par l'Explorateur.
pub fn registry_is_virtualized() -> bool {
    if !is_packaged() {
        return false;
    }
    // Manifeste illisible : on suppose la virtualisation (refus plutôt qu'une option sans effet).
    package_path()
        .and_then(|p| std::fs::read_to_string(p.join("AppxManifest.xml")).ok())
        .is_none_or(|m| !manifest_disables_registry_virtualization(&m))
}

/// Chemin du programme à enregistrer dans le registre pour qu'un autre processus le lance.
/// Hors paquet : l'exécutable courant. Dans un paquet : l'**alias d'exécution**
/// (`%LOCALAPPDATA%\Microsoft\WindowsApps\<exe>`), seul chemin lançable de l'extérieur et qui
/// survit aux mises à jour (le dossier du paquet porte sa version).
pub fn launch_path() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    if !is_packaged() {
        return Ok(exe);
    }
    let name = exe
        .file_name()
        .ok_or_else(|| std::io::Error::other("nom de l'exécutable introuvable"))?;
    let local =
        std::env::var_os("LOCALAPPDATA").ok_or_else(|| std::io::Error::other("LOCALAPPDATA absent"))?;
    let alias = std::path::PathBuf::from(local)
        .join("Microsoft")
        .join("WindowsApps")
        .join(name);
    if alias.exists() {
        Ok(alias)
    } else {
        Err(std::io::Error::other(format!(
            "alias d'exécution introuvable : {}",
            alias.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_is_not_packaged() {
        assert!(!is_packaged());
        assert!(package_path().is_none());
        assert!(!registry_is_virtualized());
        assert_eq!(launch_path().ok(), std::env::current_exe().ok());
    }

    #[test]
    fn manifest_virtualization_flag_is_read() {
        let off = r#"<Properties><DisplayName>x</DisplayName>
            <desktop6:RegistryWriteVirtualization>disabled</desktop6:RegistryWriteVirtualization></Properties>"#;
        assert!(manifest_disables_registry_virtualization(off));
        let on = off.replace("disabled", "enabled");
        assert!(!manifest_disables_registry_virtualization(&on));
        assert!(!manifest_disables_registry_virtualization("<Package/>"));
    }
}
