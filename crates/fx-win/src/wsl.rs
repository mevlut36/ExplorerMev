//! Distributions WSL installées (clé `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`).
//!
//! Lecture seule du registre : instantanée, sans lancer `wsl.exe` ni démarrer la machine
//! virtuelle. Les distributions internes de Docker Desktop ne sont pas listées.

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::Registry::RegEnumKeyExW;
use windows::core::PWSTR;

use crate::win_e::{Key, WinEKey};

const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// Noms des sous-clés de `key`.
fn subkeys(key: &Key) -> Vec<String> {
    let mut out = Vec::new();
    for index in 0u32.. {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: clé ouverte en lecture ; tampon local dont la taille (en unités UTF-16) est
        // transmise ; les autres sorties sont inutilisées.
        let r = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if r == ERROR_NO_MORE_ITEMS {
            break;
        }
        if r == ERROR_MORE_DATA {
            continue; // nom démesuré : ignoré
        }
        if r.0 != 0 {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

/// Distributions installées, triées sans tenir compte de la casse.
pub fn distributions() -> Vec<String> {
    let Ok(Some(root)) = WinEKey::open(LXSS) else {
        return Vec::new();
    };
    let mut names: Vec<String> = subkeys(&root)
        .into_iter()
        .filter_map(|sub| {
            let key = WinEKey::open(&format!(r"{LXSS}\{sub}")).ok()??;
            WinEKey::read(&key, "DistributionName").ok()?
        })
        .filter(|n| !n.is_empty() && !n.to_lowercase().starts_with("docker-desktop"))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    #[test]
    fn distributions_are_readable_without_side_effects() {
        // Selon la machine : aucune, ou plusieurs. Jamais de nom vide ni de docker-desktop.
        for d in super::distributions() {
            assert!(!d.is_empty());
            assert!(!d.to_lowercase().starts_with("docker-desktop"));
        }
    }
}
