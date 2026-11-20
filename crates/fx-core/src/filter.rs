//! Filtre instantané du dossier courant : sous-chaîne insensible à la casse.

use rayon::prelude::*;

use crate::folder::Folder;
use crate::sort::PARALLEL_THRESHOLD;

/// Vue filtrée : requête texte (sous-chaîne, minuscules) et masquage des fichiers cachés.
#[derive(Clone, Debug)]
pub struct Filter {
    needle: Vec<char>,
    show_hidden: bool,
}

impl Default for Filter {
    /// Filtre neutre : tout est visible.
    fn default() -> Self {
        Self {
            needle: Vec::new(),
            show_hidden: true,
        }
    }
}

impl Filter {
    /// Filtre texte ; les fichiers cachés restent visibles (voir [`Filter::with_hidden`]).
    pub fn new(query: &str) -> Self {
        Self {
            needle: query.trim().chars().flat_map(char::to_lowercase).collect(),
            show_hidden: true,
        }
    }

    /// Affiche (`true`) ou masque (`false`) les éléments portant l'attribut caché, comme le
    /// réglage « Éléments masqués » de l'Explorateur.
    pub fn with_hidden(mut self, show_hidden: bool) -> Self {
        self.show_hidden = show_hidden;
        self
    }

    pub fn shows_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Une requête texte est-elle saisie ?
    pub fn has_query(&self) -> bool {
        !self.needle.is_empty()
    }

    /// Le filtre laisse-t-il tout passer ?
    pub fn is_identity(&self) -> bool {
        self.needle.is_empty() && self.show_hidden
    }

    pub fn matches(&self, name: &[u16]) -> bool {
        if self.needle.is_empty() {
            return true;
        }
        let hay: Vec<char> = char::decode_utf16(name.iter().copied())
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .flat_map(char::to_lowercase)
            .collect();
        hay.windows(self.needle.len())
            .any(|w| w == self.needle.as_slice())
    }

    /// Sous-ensemble de `order` (ordre préservé) qui passe le filtre.
    pub fn apply(&self, folder: &Folder, order: &[u32]) -> Vec<u32> {
        if self.is_identity() {
            return order.to_vec();
        }
        let keep = |i: &&u32| {
            let i = **i;
            (self.show_hidden || folder.entry(i).is_some_and(|e| !e.is_hidden()))
                && self.matches(folder.name(i))
        };
        if order.len() >= PARALLEL_THRESHOLD {
            order.par_iter().filter(keep).copied().collect()
        } else {
            order.iter().filter(keep).copied().collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::folder::RawEntry;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn case_and_accent_aware_substring() {
        let f = Filter::new("  ÉTÉ ");
        assert!(f.matches(&w("Photos d'été 2024.jpg")));
        assert!(!f.matches(&w("Photos d'ete.jpg")));
        assert!(Filter::new("").matches(&w("x")));
        assert!(!Filter::new("abc").matches(&w("ab")));
    }

    #[test]
    fn apply_keeps_order() {
        let mut folder = Folder::new(PathBuf::new());
        for n in ["rapport.pdf", "photo.jpg", "Rapport final.docx"] {
            let n = w(n);
            folder.push(
                &RawEntry {
                    name: &n,
                    attributes: 0,
                    reparse_tag: 0,
                    size: 0,
                    created: 0,
                    modified: 0,
                },
                0,
            );
        }
        assert_eq!(Filter::new("RAPP").apply(&folder, &[2, 1, 0]), vec![2, 0]);
    }

    #[test]
    fn hidden_entries_can_be_masked() {
        use crate::folder::ATTR_HIDDEN;
        let mut folder = Folder::new(PathBuf::new());
        for (n, attrs) in [("visible.txt", 0), ("cache.ini", ATTR_HIDDEN), ("autre.txt", 0)] {
            let n = w(n);
            folder.push(
                &RawEntry {
                    name: &n,
                    attributes: attrs,
                    reparse_tag: 0,
                    size: 0,
                    created: 0,
                    modified: 0,
                },
                0,
            );
        }
        assert_eq!(Filter::default().apply(&folder, &[0, 1, 2]), vec![0, 1, 2]);
        assert_eq!(
            Filter::default().with_hidden(false).apply(&folder, &[0, 1, 2]),
            vec![0, 2]
        );
        assert_eq!(
            Filter::new("a").with_hidden(false).apply(&folder, &[0, 1, 2]),
            vec![2]
        );
        assert!(!Filter::default().with_hidden(false).is_identity());
        assert!(!Filter::default().with_hidden(false).has_query());
    }
}
