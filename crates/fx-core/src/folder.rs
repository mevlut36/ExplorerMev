//! Représentation compacte du contenu d'un dossier.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_REPARSE_POINT: u32 = 0x400;
pub const ATTR_OFFLINE: u32 = 0x1000;
pub const ATTR_PINNED: u32 = 0x8_0000;
pub const ATTR_UNPINNED: u32 = 0x10_0000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// `IO_REPARSE_TAG_CLOUD*` : 0x9000001A, avec un sous-type dans les bits 12 à 15.
const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
const IO_REPARSE_TAG_CLOUD_MASK: u32 = 0x0000_F000;

/// État d'un élément de fournisseur cloud (OneDrive…), comme la colonne « État » de
/// l'Explorateur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudState {
    /// Élément ordinaire, hors fournisseur cloud.
    None,
    /// Disponible en ligne seulement : les données ne sont pas sur le disque.
    OnlineOnly,
    /// Disponible localement (peut être libéré par Windows).
    Local,
    /// « Toujours conserver sur cet appareil ».
    Pinned,
}

/// Entrée brute produite par une source de listing. Le nom est emprunté.
#[derive(Clone, Copy, Debug)]
pub struct RawEntry<'a> {
    pub name: &'a [u16],
    pub attributes: u32,
    pub reparse_tag: u32,
    pub size: u64,
    /// FILETIME UTC.
    pub created: i64,
    pub modified: i64,
}

/// Une entrée de dossier : 48 octets, le nom vit dans l'arène du [`Folder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub size: u64,
    pub modified: i64,
    pub created: i64,
    name_start: u32,
    pub attributes: u32,
    pub reparse_tag: u32,
    /// Index dans [`Folder::types`].
    pub type_id: u32,
    name_len: u16,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
    pub fn is_hidden(&self) -> bool {
        self.attributes & ATTR_HIDDEN != 0
    }
    /// État cloud, déduit des attributs et du tag de point d'analyse, sans accès au fichier.
    pub fn cloud_state(&self) -> CloudState {
        let tagged = self.reparse_tag & !IO_REPARSE_TAG_CLOUD_MASK == IO_REPARSE_TAG_CLOUD;
        let cloud_attrs = ATTR_PINNED | ATTR_UNPINNED | ATTR_RECALL_ON_DATA_ACCESS | ATTR_RECALL_ON_OPEN;
        if !tagged && self.attributes & cloud_attrs == 0 {
            CloudState::None
        } else if self.attributes & ATTR_PINNED != 0 {
            CloudState::Pinned
        } else if self.is_cloud_online_only() {
            CloudState::OnlineOnly
        } else {
            CloudState::Local
        }
    }

    /// Fichier cloud dont les données ne sont pas présentes localement : il ne faut ni lire son
    /// contenu, ni extraire son icône ou sa miniature.
    pub fn is_cloud_online_only(&self) -> bool {
        self.attributes & (ATTR_RECALL_ON_DATA_ACCESS | ATTR_RECALL_ON_OPEN | ATTR_OFFLINE) != 0
    }
}

/// Contenu d'un dossier : un seul chemin, une arène de noms UTF-16, des entrées compactes.
#[derive(Clone, Debug)]
pub struct Folder {
    path: PathBuf,
    names: Vec<u16>,
    entries: Vec<Entry>,
    /// Libellés de type, indexés par `Entry::type_id`.
    types: Vec<Arc<str>>,
}

impl Folder {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            names: Vec::new(),
            entries: Vec::new(),
            types: Vec::new(),
        }
    }

    pub fn with_capacity(path: PathBuf, entries: usize) -> Self {
        Self {
            path,
            names: Vec::with_capacity(entries * 24),
            entries: Vec::with_capacity(entries),
            types: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, i: u32) -> Option<&Entry> {
        self.entries.get(i as usize)
    }

    pub fn name_of(&self, e: &Entry) -> &[u16] {
        let start = e.name_start as usize;
        self.names.get(start..start + e.name_len as usize).unwrap_or(&[])
    }

    pub fn name(&self, i: u32) -> &[u16] {
        self.entry(i).map(|e| self.name_of(e)).unwrap_or(&[])
    }

    /// Nom affichable (les substituts isolés sont remplacés par U+FFFD).
    pub fn display_name(&self, i: u32) -> String {
        String::from_utf16_lossy(self.name(i))
    }

    /// Chemin complet de l'entrée, construit à la demande (noms invalides préservés).
    pub fn child_path(&self, i: u32) -> PathBuf {
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(self.name(i))
        };
        #[cfg(not(windows))]
        let name = OsString::from(self.display_name(i));
        self.path.join(name)
    }

    pub fn types(&self) -> &[Arc<str>] {
        &self.types
    }

    pub fn type_label(&self, e: &Entry) -> &str {
        self.types.get(e.type_id as usize).map(|s| &**s).unwrap_or("")
    }

    /// Ajoute (ou retrouve) un libellé de type et renvoie son identifiant.
    pub fn intern_type(&mut self, label: Arc<str>) -> u32 {
        if let Some(i) = self
            .types
            .iter()
            .position(|t| Arc::ptr_eq(t, &label) || **t == *label)
        {
            return i as u32;
        }
        self.types.push(label);
        (self.types.len() - 1) as u32
    }

    /// Ajoute une entrée. Les noms de plus de 65 535 unités (impossible sur NTFS) sont tronqués.
    pub fn push(&mut self, raw: &RawEntry<'_>, type_id: u32) {
        let len = raw.name.len().min(u16::MAX as usize);
        let name_start = self.names.len() as u32;
        self.names.extend_from_slice(&raw.name[..len]);
        self.entries.push(Entry {
            size: raw.size,
            modified: raw.modified,
            created: raw.created,
            name_start,
            attributes: raw.attributes,
            reparse_tag: raw.reparse_tag,
            type_id,
            name_len: len as u16,
        });
    }

    /// Index de l'entrée portant exactement ce nom.
    pub fn find(&self, name: &[u16]) -> Option<u32> {
        self.entries
            .iter()
            .position(|e| self.name_of(e) == name)
            .map(|i| i as u32)
    }

    /// Nouveau dossier : `self` moins les entrées dont le nom est dans `removed`, plus `added`
    /// (nom, entrée, libellé de type). Renvoie aussi, pour chaque ancien index, le nouveau.
    pub fn with_changes(
        &self,
        removed: &std::collections::HashSet<Vec<u16>>,
        added: &[(RawEntry<'_>, Arc<str>)],
    ) -> (Folder, Vec<Option<u32>>) {
        let mut out = Folder::with_capacity(self.path.clone(), self.len() + added.len());
        out.types = self.types.clone();
        let mut map = Vec::with_capacity(self.len());
        for e in &self.entries {
            let name = self.name_of(e);
            if removed.contains(name) {
                map.push(None);
            } else {
                map.push(Some(out.entries.len() as u32));
                let raw = RawEntry {
                    name,
                    attributes: e.attributes,
                    reparse_tag: e.reparse_tag,
                    size: e.size,
                    created: e.created,
                    modified: e.modified,
                };
                out.push(&raw, e.type_id);
            }
        }
        for (raw, label) in added {
            let t = out.intern_type(label.clone());
            out.push(raw, t);
        }
        (out, map)
    }

    /// Octets occupés par les données (hors en-têtes de Vec), pour le suivi mémoire.
    pub fn heap_bytes(&self) -> usize {
        self.names.capacity() * 2 + self.entries.capacity() * size_of::<Entry>()
    }
}

/// Clé d'extension pour la table des types : extension en minuscules, sans le point ; vide
/// pour un fichier sans extension.
pub fn extension_key(name: &[u16]) -> Vec<u16> {
    match name.iter().rposition(|&c| c == b'.' as u16) {
        Some(dot) => {
            let ext = &name[dot + 1..];
            char::decode_utf16(ext.iter().copied())
                .flat_map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER).to_lowercase())
                .flat_map(|c| {
                    let mut b = [0u16; 2];
                    c.encode_utf16(&mut b).to_vec()
                })
                .collect()
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn raw(name: &[u16], attributes: u32) -> RawEntry<'_> {
        RawEntry {
            name,
            attributes,
            reparse_tag: 0,
            size: 7,
            created: 1,
            modified: 2,
        }
    }

    #[test]
    fn entry_is_compact() {
        assert_eq!(size_of::<Entry>(), 48);
    }

    #[test]
    fn push_and_read_back_names() {
        let mut f = Folder::new(PathBuf::from(r"C:\x"));
        let a = w("alpha.txt");
        let b = vec![0x61, 0xD800, 0x62];
        f.push(&raw(&a, 0), 0);
        f.push(&raw(&b, ATTR_DIRECTORY), 0);
        assert_eq!(f.name(0), &a[..]);
        assert_eq!(f.name(1), &b[..]);
        assert!(f.entry(1).is_some_and(|e| e.is_dir()));
        assert_eq!(f.find(&b), Some(1));
        assert_eq!(f.display_name(1), "a\u{FFFD}b");
        assert_eq!(f.name(5), &[] as &[u16]);
    }

    #[test]
    fn types_are_interned() {
        let mut f = Folder::new(PathBuf::new());
        let t1 = f.intern_type(Arc::from("Document texte"));
        let t2 = f.intern_type(Arc::from("Document texte"));
        let t3 = f.intern_type(Arc::from("Application"));
        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
    }

    #[test]
    fn extension_keys() {
        assert_eq!(extension_key(&w("A.TXT")), w("txt"));
        assert_eq!(extension_key(&w("archive.tar.GZ")), w("gz"));
        assert_eq!(extension_key(&w("Makefile")), w(""));
        assert_eq!(extension_key(&w(".gitignore")), w("gitignore"));
        assert_eq!(extension_key(&w("x.ÉTÉ")), w("été"));
    }

    #[test]
    fn with_changes_removes_replaces_and_maps() {
        let mut f = Folder::new(PathBuf::from("d"));
        let (a, b, c) = (w("a"), w("b"), w("c"));
        f.push(&raw(&a, 0), 0);
        f.push(&raw(&b, 0), 0);
        f.push(&raw(&c, 0), 0);
        let removed: std::collections::HashSet<Vec<u16>> = [a.clone(), c.clone()].into_iter().collect();
        let mut c2 = raw(&c, 0);
        c2.size = 99;
        let (g, map) = f.with_changes(&removed, &[(c2, Arc::from("Texte"))]);
        assert_eq!(map, vec![None, Some(0), None]);
        assert_eq!(g.len(), 2);
        assert_eq!(g.name(1), &c[..]);
        assert_eq!(g.entry(1).map(|e| e.size), Some(99));
        assert_eq!(
            g.entry(1).map(|e| g.type_label(e).to_string()).as_deref(),
            Some("Texte")
        );
    }

    #[test]
    fn cloud_state_from_attributes_and_tag() {
        let mut f = Folder::new(PathBuf::new());
        let n = w("x");
        let mut r = raw(&n, 0);
        f.push(&r, 0); // ordinaire
        r.attributes = ATTR_RECALL_ON_DATA_ACCESS | ATTR_OFFLINE;
        r.reparse_tag = 0x9000_701A;
        f.push(&r, 0);
        r.attributes = 0;
        f.push(&r, 0); // tag cloud, données présentes
        r.attributes = ATTR_PINNED;
        f.push(&r, 0);
        let states: Vec<CloudState> = f.entries().iter().map(Entry::cloud_state).collect();
        assert_eq!(
            states,
            [
                CloudState::None,
                CloudState::OnlineOnly,
                CloudState::Local,
                CloudState::Pinned
            ]
        );
    }

    #[test]
    fn cloud_states() {
        let mut f = Folder::new(PathBuf::new());
        let n = w("x");
        f.push(&raw(&n, ATTR_RECALL_ON_DATA_ACCESS | ATTR_UNPINNED), 0);
        f.push(&raw(&n, ATTR_PINNED), 0);
        assert!(f.entry(0).is_some_and(|e| e.is_cloud_online_only()));
        assert!(f.entry(1).is_some_and(|e| !e.is_cloud_online_only()));
    }
}
