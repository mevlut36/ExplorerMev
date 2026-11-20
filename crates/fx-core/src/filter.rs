//! Filtre instantané du dossier courant : sous-chaîne insensible à la casse.

use rayon::prelude::*;

use crate::folder::Folder;
use crate::sort::PARALLEL_THRESHOLD;

/// Requête de filtre prétraitée (minuscules).
#[derive(Clone, Debug, Default)]
pub struct Filter {
    needle: Vec<char>,
}

impl Filter {
    pub fn new(query: &str) -> Self {
        Self {
            needle: query.trim().chars().flat_map(char::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
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

    /// Sous-ensemble de `order` (ordre préservé) dont le nom correspond.
    pub fn apply(&self, folder: &Folder, order: &[u32]) -> Vec<u32> {
        if self.is_empty() {
            return order.to_vec();
        }
        let keep = |i: &&u32| self.matches(folder.name(**i));
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
}
