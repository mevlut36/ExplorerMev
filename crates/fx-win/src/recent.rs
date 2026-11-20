//! Éléments récents de Windows (dossier `%APPDATA%\Microsoft\Windows\Recent`).
//!
//! Chaque élément récent est un raccourci `.lnk`. On lit seulement la **cible** enregistrée dans
//! le raccourci (`IShellLinkW::GetPath`, sans résolution) : ni le fichier cible ni son contenu ne
//! sont ouverts, donc rien n'est téléchargé depuis OneDrive et un partage réseau injoignable ne
//! bloque pas. L'existence n'est vérifiée que pour les chemins locaux, par leurs métadonnées.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
use windows::core::{Interface, PCWSTR};

use crate::listing::{ListOptions, list_dir};

/// Élément récent : cible du raccourci et date de dernier accès (FILETIME du raccourci).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentItem {
    pub target: PathBuf,
    pub is_dir: bool,
    pub accessed: i64,
}

/// Dossier des éléments récents de l'utilisateur.
pub fn recent_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Recent"))
}

/// Cible d'un raccourci, sans le résoudre ni toucher à la cible.
fn link_target(lnk: &Path) -> Option<PathBuf> {
    let wide = crate::path::shell_path(lnk);
    // SAFETY: objets COM possédés ; chaîne terminée par un nul vivante pendant l'appel ; tampon
    // local de MAX_PATH unités UTF-16 dont la taille est transmise. Thread COM initialisé par
    // l'appelant.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()?;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        (end > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..end])))
    }
}

fn is_network(p: &Path) -> bool {
    p.to_string_lossy().starts_with(r"\\")
}

/// Les `limit` éléments récents les plus récents encore présents, sans doublon. À appeler sur un
/// thread COM (STA).
pub fn recent_items(limit: usize) -> Vec<RecentItem> {
    let Some(dir) = recent_dir() else {
        return Vec::new();
    };
    let mut links: Vec<(Vec<u16>, i64)> = Vec::new();
    let r = list_dir(&dir, &ListOptions::default(), &|| false, &mut |e| {
        // Les raccourcis seulement (le dossier contient aussi des dossiers de listes de sauts).
        let n = e.name;
        let is_lnk = n.len() > 4 && String::from_utf16_lossy(&n[n.len() - 4..]).eq_ignore_ascii_case(".lnk");
        if is_lnk && e.attributes & 0x10 == 0 {
            links.push((n.to_vec(), e.modified));
        }
    });
    if r.is_err() {
        return Vec::new();
    }
    links.sort_by_key(|l| std::cmp::Reverse(l.1));
    let mut out: Vec<RecentItem> = Vec::new();
    // On regarde plus de raccourcis que demandé : certaines cibles n'existent plus.
    for (name, accessed) in links.into_iter().take(limit * 4) {
        if out.len() >= limit {
            break;
        }
        let lnk = dir.join(String::from_utf16_lossy(&name));
        let Some(target) = link_target(&lnk) else { continue };
        if out
            .iter()
            .any(|o| o.target.as_os_str().eq_ignore_ascii_case(target.as_os_str()))
        {
            continue;
        }
        let is_dir = if is_network(&target) {
            // Pas d'entrée-sortie réseau : on suppose un fichier sauf extension absente.
            target.extension().is_none()
        } else {
            let (Some(parent), Some(file)) = (target.parent(), target.file_name()) else {
                continue;
            };
            let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(file).collect();
            match crate::listing::stat_entry(parent, &wide) {
                Some(e) => e.attributes & 0x10 != 0,
                None => continue, // supprimé depuis
            }
        };
        out.push(RecentItem {
            target,
            is_dir,
            accessed,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_items_are_readable_and_bounded() {
        let _com = crate::com::ComApartment::sta().ok();
        let items = recent_items(10);
        assert!(items.len() <= 10);
        let mut seen = std::collections::HashSet::new();
        for i in &items {
            assert!(!i.target.as_os_str().is_empty());
            assert!(seen.insert(i.target.to_string_lossy().to_lowercase()), "doublon");
        }
    }

    #[test]
    fn network_paths_are_recognized() {
        assert!(is_network(Path::new(r"\\srv\partage\a.txt")));
        assert!(!is_network(Path::new(r"C:\a.txt")));
    }
}
