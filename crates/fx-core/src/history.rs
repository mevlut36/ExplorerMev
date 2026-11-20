//! Navigation : historique précédent/suivant et dossier parent.

use std::path::{Path, PathBuf};

const MAX_HISTORY: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    current: Option<PathBuf>,
}

impl History {
    pub fn current(&self) -> Option<&Path> {
        self.current.as_deref()
    }

    /// Va vers `path`. Revenir au dossier courant ne crée pas d'entrée d'historique.
    pub fn navigate(&mut self, path: PathBuf) {
        if self.current.as_ref() == Some(&path) {
            return;
        }
        if let Some(cur) = self.current.replace(path) {
            self.back.push(cur);
            if self.back.len() > MAX_HISTORY {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn go_back(&mut self) -> Option<&Path> {
        let prev = self.back.pop()?;
        if let Some(cur) = self.current.replace(prev) {
            self.forward.push(cur);
        }
        self.current()
    }

    pub fn go_forward(&mut self) -> Option<&Path> {
        let next = self.forward.pop()?;
        if let Some(cur) = self.current.replace(next) {
            self.back.push(cur);
        }
        self.current()
    }
}

/// Dossier parent ; `None` à la racine d'un volume ou d'un partage (`C:\`, `\\serveur\partage`).
pub fn parent_of(path: &Path) -> Option<PathBuf> {
    // Racine d'une distribution WSL : le parent est le dossier virtuel « Linux ».
    if let Some(root) = crate::wsl::parent_of_distro_root(path) {
        return Some(root);
    }
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn back_and_forward() {
        let mut h = History::default();
        assert!(!h.can_go_back());
        h.navigate(p("A"));
        h.navigate(p("B"));
        h.navigate(p("C"));
        assert_eq!(h.go_back(), Some(Path::new("B")));
        assert_eq!(h.go_back(), Some(Path::new("A")));
        assert_eq!(h.go_back(), None);
        assert_eq!(h.current(), Some(Path::new("A")));
        assert_eq!(h.go_forward(), Some(Path::new("B")));
        assert!(h.can_go_forward());
    }

    #[test]
    fn navigating_clears_forward() {
        let mut h = History::default();
        h.navigate(p("A"));
        h.navigate(p("B"));
        h.go_back();
        h.navigate(p("D"));
        assert!(!h.can_go_forward());
        assert_eq!(h.go_back(), Some(Path::new("A")));
    }

    #[test]
    fn same_folder_is_not_duplicated() {
        let mut h = History::default();
        h.navigate(p("A"));
        h.navigate(p("A"));
        assert!(!h.can_go_back());
    }

    #[test]
    fn history_is_bounded() {
        let mut h = History::default();
        for i in 0..1000 {
            h.navigate(p(&i.to_string()));
        }
        let mut n = 0;
        while h.go_back().is_some() {
            n += 1;
        }
        assert_eq!(n, MAX_HISTORY);
    }

    #[cfg(windows)]
    #[test]
    fn parents() {
        assert_eq!(parent_of(Path::new(r"C:\a\b")), Some(p(r"C:\a")));
        assert_eq!(parent_of(Path::new(r"C:\a")), Some(p(r"C:\")));
        assert_eq!(parent_of(Path::new(r"C:\")), None);
        assert_eq!(parent_of(Path::new(r"\\srv\share\x")), Some(p(r"\\srv\share\")));
        assert_eq!(parent_of(Path::new(r"\\srv\share\")), None);
        assert_eq!(parent_of(Path::new(r"\\?\C:\a")), Some(p(r"\\?\C:\")));
    }
}
