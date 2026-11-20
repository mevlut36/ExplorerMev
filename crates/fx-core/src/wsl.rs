//! Chemins WSL (`\\wsl.localhost\<distribution>\…`, ancien alias `\\wsl$`).
//!
//! La racine `\\wsl.localhost` n'est pas un dossier ouvrable : Windows ne l'énumère que par
//! l'espace de noms du Shell. ExplorerMev la présente comme un dossier virtuel dont les entrées
//! sont les distributions installées (voir `fx_win::wsl::distributions`).

use std::path::{Path, PathBuf};

/// Racine « Linux » (dossier virtuel).
pub const ROOT: &str = r"\\wsl.localhost";

/// Forme normalisée : sans préfixe `\\?\UNC\`, sans barre finale, en minuscules.
fn normalized(path: &Path) -> String {
    let s = path.to_string_lossy().replace('/', "\\");
    let s = match s.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => s,
    };
    s.trim_end_matches('\\').to_lowercase()
}

/// `\\wsl.localhost` ou `\\wsl$` (éventuellement terminé par une barre).
pub fn is_root(path: &Path) -> bool {
    let n = normalized(path);
    n == r"\\wsl.localhost" || n == r"\\wsl$"
}

/// Racine d'une distribution (`\\wsl.localhost\Ubuntu`) : son parent est la racine « Linux ».
pub fn parent_of_distro_root(path: &Path) -> Option<PathBuf> {
    let n = normalized(path);
    let rest = n
        .strip_prefix(r"\\wsl.localhost\")
        .or_else(|| n.strip_prefix(r"\\wsl$\"))?;
    (!rest.is_empty() && !rest.contains('\\')).then(|| PathBuf::from(ROOT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_detection() {
        for p in [
            r"\\wsl.localhost",
            r"\\wsl.localhost\",
            r"\\WSL.LOCALHOST\",
            r"\\wsl$",
            r"\\?\UNC\wsl.localhost\",
        ] {
            assert!(is_root(Path::new(p)), "{p}");
        }
        for p in [
            r"\\wsl.localhost\Ubuntu",
            r"\\srv\partage",
            r"C:\",
            r"\\wsl.localhostx",
        ] {
            assert!(!is_root(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn distro_root_parent_is_the_linux_root() {
        assert_eq!(
            parent_of_distro_root(Path::new(r"\\wsl.localhost\Ubuntu")),
            Some(PathBuf::from(ROOT))
        );
        assert_eq!(
            parent_of_distro_root(Path::new(r"\\wsl$\Ubuntu\")),
            Some(PathBuf::from(ROOT))
        );
        assert_eq!(
            parent_of_distro_root(Path::new(r"\\wsl.localhost\Ubuntu\home")),
            None
        );
        assert_eq!(parent_of_distro_root(Path::new(r"\\wsl.localhost")), None);
        assert_eq!(parent_of_distro_root(Path::new(r"\\srv\partage")), None);
    }
}
