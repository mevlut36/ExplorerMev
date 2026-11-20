//! Tri des entrées d'un dossier : dossiers en premier, tri stable par colonne, tri naturel des
//! noms, parallélisé au-delà d'un seuil.

use std::cmp::Ordering;

use rayon::prelude::*;

use crate::collate::Collator;
use crate::folder::Folder;

/// En dessous de ce nombre d'entrées, le tri séquentiel est plus rapide que `rayon`.
pub const PARALLEL_THRESHOLD: usize = 20_000;
const KEY_CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Modified,
    Type,
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct SortSpec {
    pub column: SortColumn,
    pub descending: bool,
}

impl SortSpec {
    /// Nouveau tri après un clic sur l'en-tête `column`, comme dans l'Explorateur : même
    /// colonne → inversion ; autre colonne → ordre par défaut de cette colonne (décroissant pour
    /// la date, croissant sinon).
    pub fn clicked(self, column: SortColumn) -> Self {
        if self.column == column {
            Self {
                column,
                descending: !self.descending,
            }
        } else {
            Self {
                column,
                descending: column == SortColumn::Modified,
            }
        }
    }
}

/// Clés de tri des noms, rangées par blocs dans des arènes (pas d'allocation par entrée).
pub struct NameKeys {
    chunks: Vec<(Vec<u8>, Vec<u32>)>,
}

impl NameKeys {
    pub fn build(folder: &Folder, collator: &dyn Collator) -> Self {
        let build_chunk = |chunk: &[crate::folder::Entry]| {
            let mut bytes = Vec::with_capacity(chunk.len() * 48);
            let mut ends = Vec::with_capacity(chunk.len());
            for e in chunk {
                collator.key(folder.name_of(e), &mut bytes);
                ends.push(bytes.len() as u32);
            }
            (bytes, ends)
        };
        let entries = folder.entries();
        let chunks = if entries.len() >= PARALLEL_THRESHOLD {
            entries.par_chunks(KEY_CHUNK).map(build_chunk).collect()
        } else {
            entries.chunks(KEY_CHUNK).map(build_chunk).collect()
        };
        Self { chunks }
    }

    pub fn get(&self, i: u32) -> &[u8] {
        let i = i as usize;
        let Some((bytes, ends)) = self.chunks.get(i / KEY_CHUNK) else {
            return &[];
        };
        let j = i % KEY_CHUNK;
        let start = if j == 0 { 0 } else { ends[j - 1] as usize };
        let end = ends.get(j).copied().unwrap_or(0) as usize;
        bytes.get(start..end).unwrap_or(&[])
    }
}

/// Renvoie l'ordre d'affichage (indices dans `folder`) pour `spec`.
pub fn sorted_order(folder: &Folder, spec: SortSpec, collator: &dyn Collator) -> Vec<u32> {
    let keys = NameKeys::build(folder, collator);
    sorted_order_with_keys(folder, spec, &keys, collator)
}

/// Variante réutilisant des clés de noms déjà calculées.
pub fn sorted_order_with_keys(
    folder: &Folder,
    spec: SortSpec,
    keys: &NameKeys,
    collator: &dyn Collator,
) -> Vec<u32> {
    let entries = folder.entries();
    // Clés des libellés de type (peu nombreux) pour le tri par type.
    let type_keys: Vec<Vec<u8>> = folder
        .types()
        .iter()
        .map(|t| {
            let mut k = Vec::new();
            collator.key(&t.encode_utf16().collect::<Vec<u16>>(), &mut k);
            k
        })
        .collect();
    let empty: &[u8] = &[];

    let cmp = |&a: &u32, &b: &u32| -> Ordering {
        let (ea, eb) = (&entries[a as usize], &entries[b as usize]);
        // Dossiers toujours en premier, quel que soit le sens.
        let dirs = eb.is_dir().cmp(&ea.is_dir());
        if dirs != Ordering::Equal {
            return dirs;
        }
        let by_name = || keys.get(a).cmp(keys.get(b));
        let primary = match spec.column {
            SortColumn::Name => by_name(),
            SortColumn::Modified => ea.modified.cmp(&eb.modified),
            SortColumn::Size => ea.size.cmp(&eb.size),
            SortColumn::Type => {
                let ta = type_keys.get(ea.type_id as usize).map_or(empty, |k| k);
                let tb = type_keys.get(eb.type_id as usize).map_or(empty, |k| k);
                ta.cmp(tb)
            }
        };
        let primary = if spec.descending {
            primary.reverse()
        } else {
            primary
        };
        // Départage par nom croissant (sauf pour le tri par nom, déjà traité).
        primary.then_with(|| {
            if spec.column == SortColumn::Name {
                Ordering::Equal
            } else {
                by_name()
            }
        })
    };

    let mut order: Vec<u32> = (0..entries.len() as u32).collect();
    if order.len() >= PARALLEL_THRESHOLD {
        order.par_sort_by(cmp); // tri stable
    } else {
        order.sort_by(cmp);
    }
    order
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::collate::PortableCollator;
    use crate::folder::{ATTR_DIRECTORY, RawEntry};

    fn folder(items: &[(&str, bool, u64, i64, &str)]) -> Folder {
        let mut f = Folder::new(PathBuf::from("x"));
        for (name, dir, size, modified, kind) in items {
            let n: Vec<u16> = name.encode_utf16().collect();
            let t = f.intern_type(Arc::from(*kind));
            f.push(
                &RawEntry {
                    name: &n,
                    attributes: if *dir { ATTR_DIRECTORY } else { 0 },
                    reparse_tag: 0,
                    size: *size,
                    created: 0,
                    modified: *modified,
                },
                t,
            );
        }
        f
    }

    fn names(f: &Folder, spec: SortSpec) -> Vec<String> {
        sorted_order(f, spec, &PortableCollator)
            .into_iter()
            .map(|i| f.display_name(i))
            .collect()
    }

    fn sample() -> Folder {
        folder(&[
            ("b10.txt", false, 300, 5, "Texte"),
            ("Dossier B", true, 0, 1, "Dossier"),
            ("a2.txt", false, 100, 9, "Texte"),
            ("b2.png", false, 200, 9, "Image"),
            ("dossier a", true, 0, 7, "Dossier"),
        ])
    }

    #[test]
    fn name_ascending_dirs_first_natural() {
        let s = SortSpec {
            column: SortColumn::Name,
            descending: false,
        };
        assert_eq!(
            names(&sample(), s),
            ["dossier a", "Dossier B", "a2.txt", "b2.png", "b10.txt"]
        );
    }

    #[test]
    fn name_descending_keeps_dirs_first() {
        let s = SortSpec {
            column: SortColumn::Name,
            descending: true,
        };
        assert_eq!(
            names(&sample(), s),
            ["Dossier B", "dossier a", "b10.txt", "b2.png", "a2.txt"]
        );
    }

    #[test]
    fn size_with_name_tiebreak() {
        let s = SortSpec {
            column: SortColumn::Size,
            descending: false,
        };
        assert_eq!(
            names(&sample(), s),
            ["dossier a", "Dossier B", "a2.txt", "b2.png", "b10.txt"]
        );
    }

    #[test]
    fn modified_descending_ties_by_name_ascending() {
        let s = SortSpec {
            column: SortColumn::Modified,
            descending: true,
        };
        assert_eq!(
            names(&sample(), s),
            ["dossier a", "Dossier B", "a2.txt", "b2.png", "b10.txt"]
        );
    }

    #[test]
    fn by_type_label() {
        let s = SortSpec {
            column: SortColumn::Type,
            descending: false,
        };
        assert_eq!(
            names(&sample(), s),
            ["dossier a", "Dossier B", "b2.png", "a2.txt", "b10.txt"]
        );
    }

    #[test]
    fn header_click_semantics() {
        let s = SortSpec::default();
        assert_eq!(
            s.clicked(SortColumn::Name),
            SortSpec {
                column: SortColumn::Name,
                descending: true
            }
        );
        assert_eq!(
            s.clicked(SortColumn::Modified),
            SortSpec {
                column: SortColumn::Modified,
                descending: true
            }
        );
        assert_eq!(
            s.clicked(SortColumn::Size),
            SortSpec {
                column: SortColumn::Size,
                descending: false
            }
        );
    }

    #[test]
    fn parallel_path_matches_sequential() {
        let items: Vec<(String, bool, u64, i64, &str)> = (0..PARALLEL_THRESHOLD + 5000)
            .map(|i| {
                (
                    format!("f{}", (i * 7919) % 100_003),
                    i % 97 == 0,
                    (i % 13) as u64,
                    (i % 5) as i64,
                    "T",
                )
            })
            .collect();
        let refs: Vec<(&str, bool, u64, i64, &str)> = items
            .iter()
            .map(|(n, d, s, m, k)| (n.as_str(), *d, *s, *m, *k))
            .collect();
        let f = folder(&refs);
        let keys = NameKeys::build(&f, &PortableCollator);
        for column in [SortColumn::Name, SortColumn::Size, SortColumn::Modified] {
            let spec = SortSpec {
                column,
                descending: false,
            };
            let par = sorted_order_with_keys(&f, spec, &keys, &PortableCollator);
            // Référence séquentielle : tri stable de la bibliothèque standard.
            let mut seq: Vec<u32> = (0..f.len() as u32).collect();
            seq.sort_by(|&a, &b| {
                let (ea, eb) = (f.entry(a).copied(), f.entry(b).copied());
                let (Some(ea), Some(eb)) = (ea, eb) else {
                    return Ordering::Equal;
                };
                eb.is_dir()
                    .cmp(&ea.is_dir())
                    .then_with(|| match column {
                        SortColumn::Name => keys.get(a).cmp(keys.get(b)),
                        SortColumn::Size => ea.size.cmp(&eb.size),
                        _ => ea.modified.cmp(&eb.modified),
                    })
                    .then_with(|| {
                        if column == SortColumn::Name {
                            Ordering::Equal
                        } else {
                            keys.get(a).cmp(keys.get(b))
                        }
                    })
            });
            assert_eq!(par, seq, "colonne {column:?}");
        }
    }
}
