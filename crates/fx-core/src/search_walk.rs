//! Construction de l'index de recherche par parcours des dossiers, sans droits
//! d'administration (repli quand la MFT n'est pas accessible).
//!
//! Le parcours est parallèle (`rayon`) ; un seul thread insère dans l'index les lots envoyés
//! par dossier. Les points d'analyse (jonctions, liens) sont indexés mais jamais suivis ; les
//! dossiers illisibles sont comptés. Aucun fichier n'est ouvert : rien n'est téléchargé depuis
//! OneDrive.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};

use fx_win::listing::{ListOptions, file_ref, list_dir};

use crate::search::{IndexMeta, ROOT_REC, VolumeIndex, record_of};

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;

/// Éléments d'un dossier : noms à la suite, et pour chacun (enregistrement, attributs, longueur).
struct Batch {
    parent: u32,
    names: Vec<u16>,
    items: Vec<(u32, u32, u16)>,
}

/// Avancement d'une construction.
#[derive(Debug, Default)]
pub struct WalkProgress {
    pub items: AtomicU64,
    pub unreadable: AtomicU64,
    pub cancelled: AtomicBool,
}

fn walk<'s>(dir: PathBuf, rec: u32, tx: SyncSender<Batch>, p: &'s WalkProgress, scope: &rayon::Scope<'s>) {
    if p.cancelled.load(Ordering::Relaxed) {
        return;
    }
    let mut batch = Batch {
        parent: rec,
        names: Vec::new(),
        items: Vec::new(),
    };
    let mut subdirs = Vec::new();
    let options = ListOptions::default();
    let cancelled = || p.cancelled.load(Ordering::Relaxed);
    let r = list_dir(&dir, &options, &cancelled, &mut |e| {
        let Some(child) = record_of(e.file_id).filter(|_| e.file_id != 0) else {
            return;
        };
        let Ok(len) = u16::try_from(e.name.len()) else {
            return;
        };
        batch.names.extend_from_slice(e.name);
        batch.items.push((child, e.attributes, len));
        if e.attributes & ATTR_DIRECTORY != 0 && e.attributes & ATTR_REPARSE_POINT == 0 {
            subdirs.push((dir.join(OsString::from_wide(e.name)), child));
        }
    });
    if r.is_err() {
        p.unreadable.fetch_add(1, Ordering::Relaxed);
    }
    p.items.fetch_add(batch.items.len() as u64, Ordering::Relaxed);
    if !batch.items.is_empty() && tx.send(batch).is_err() {
        return;
    }
    for (d, child) in subdirs {
        let tx = tx.clone();
        scope.spawn(move |s| walk(d, child, tx, p, s));
    }
}

/// Construit l'index du volume `letter` (NTFS : les références de fichier sont requises).
pub fn build(letter: char, p: &WalkProgress) -> Result<VolumeIndex, String> {
    let root = PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase()));
    let (_, serial) = file_ref(&root).ok_or_else(|| format!("volume {letter}: inaccessible"))?;
    let mut ix = VolumeIndex::new(letter);
    ix.meta = IndexMeta {
        serial,
        source: 0,
        ..IndexMeta::default()
    };
    let (tx, rx) = sync_channel::<Batch>(1024);
    std::thread::scope(|s| {
        s.spawn(|| {
            rayon::scope(|sc| walk(root.clone(), ROOT_REC, tx, p, sc));
        });
        for b in rx {
            let mut at = 0usize;
            for (rec, attrs, len) in b.items {
                let name = &b.names[at..at + len as usize];
                at += len as usize;
                ix.upsert(rec, b.parent, name, attrs);
            }
        }
    });
    if p.cancelled.load(Ordering::Relaxed) {
        return Err("construction annulée".into());
    }
    ix.shrink();
    Ok(ix)
}

/// Enregistrement MFT du dossier `path`, s'il est sur le volume `serial`.
pub fn record_of_path(path: &Path, serial: u32) -> Option<u32> {
    let (r, s) = file_ref(path)?;
    if s != serial {
        return None;
    }
    let rec = record_of(r)?;
    // Le dossier racine est toujours désigné par ROOT_REC dans l'index.
    Some(if path.parent().is_none() { ROOT_REC } else { rec })
}
