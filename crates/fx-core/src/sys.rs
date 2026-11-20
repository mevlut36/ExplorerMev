//! Implémentations Windows des abstractions de `fx-core`, au-dessus de `fx-win`.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use fx_win::listing::{ListError, ListOptions, list_dir, stat_entry};
use fx_win::watcher::DirChange;

use crate::collate::{Collator, PortableCollator};
use crate::folder::RawEntry;
use crate::source::{DirSource, ErrorKind, FsEvent, OwnedRaw, SourceError, WatchSink};

/// Système de fichiers réel.
#[derive(Clone, Copy, Debug, Default)]
pub struct WinSource {
    pub options: ListOptions,
}

impl From<ListError> for SourceError {
    fn from(e: ListError) -> Self {
        let kind = match e {
            ListError::AccessDenied => ErrorKind::AccessDenied,
            ListError::NotFound | ListError::InvalidPath(_) => ErrorKind::NotFound,
            ListError::NotADirectory => ErrorKind::NotADirectory,
            ListError::NetworkUnreachable => ErrorKind::Network,
            ListError::Cancelled => ErrorKind::Cancelled,
            ListError::Other(_) => ErrorKind::Other,
        };
        SourceError::new(kind, e.to_string())
    }
}

thread_local! {
    /// COM initialisé à la demande sur les threads de calcul (pool `rayon`) qui résolvent des
    /// noms de type ; libéré à la fin du thread.
    static LAZY_COM: RefCell<Option<Option<fx_win::com::ComApartment>>> = const { RefCell::new(None) };
}

fn ensure_com() {
    LAZY_COM.with_borrow_mut(|c| {
        if c.is_none() {
            *c = Some(fx_win::com::ComApartment::sta().ok());
        }
    });
}

fn owned(e: fx_win::listing::OwnedEntry) -> OwnedRaw {
    OwnedRaw {
        name: e.name,
        attributes: e.attributes,
        reparse_tag: e.reparse_tag,
        size: e.size,
        created: e.created,
        modified: e.modified,
    }
}

/// Convertit un lot de changements bruts en événements, en lisant les métadonnées des entrées
/// créées ou modifiées (une seule fois par nom).
fn to_events(dir: &Path, changes: Vec<DirChange>) -> Vec<FsEvent> {
    let mut order: Vec<Vec<u16>> = Vec::new();
    let mut last: HashMap<Vec<u16>, bool> = HashMap::new(); // nom -> existe encore ?
    for c in changes {
        let (name, alive) = match c {
            DirChange::Added(n) | DirChange::Modified(n) | DirChange::RenamedTo(n) => (n, true),
            DirChange::Removed(n) | DirChange::RenamedFrom(n) => (n, false),
            DirChange::Overflow | DirChange::Gone => return vec![FsEvent::Reload],
        };
        if last.insert(name.clone(), alive).is_none() {
            order.push(name);
        }
    }
    order
        .into_iter()
        .map(|name| {
            let alive = last.get(&name).copied().unwrap_or(false);
            match alive.then(|| stat_entry(dir, &name)).flatten() {
                Some(e) => FsEvent::Upsert(owned(e)),
                None => FsEvent::Remove(name),
            }
        })
        .collect()
}

impl DirSource for WinSource {
    fn init_worker_thread(&self) -> Option<Box<dyn Any>> {
        // SHGetFileInfoW exige COM ; un échec n'empêche pas le listing (libellés génériques).
        match fx_win::com::ComApartment::sta() {
            Ok(apt) => Some(Box::new(apt)),
            Err(e) => {
                tracing::warn!("initialisation COM du thread de listing impossible : {e}");
                None
            }
        }
    }

    fn list(
        &self,
        path: &Path,
        cancelled: &dyn Fn() -> bool,
        on_entry: &mut dyn FnMut(&RawEntry<'_>),
    ) -> Result<(), SourceError> {
        let method = list_dir(path, &self.options, cancelled, &mut |e| {
            on_entry(&RawEntry {
                name: e.name,
                attributes: e.attributes,
                reparse_tag: e.reparse_tag,
                size: e.size,
                created: e.created,
                modified: e.modified,
            })
        })?;
        if method != self.options.method {
            tracing::info!(path = %path.display(), ?method, "méthode de listing de repli utilisée");
        }
        Ok(())
    }

    fn type_name(&self, file_name: &[u16], is_dir: bool) -> Option<String> {
        ensure_com();
        fx_win::shell::type_name(file_name, is_dir)
    }

    fn watch(&self, path: &Path, mut sink: WatchSink) -> Option<Box<dyn Send>> {
        let dir = path.to_path_buf();
        match fx_win::watcher::watch(path, move |changes| {
            let events = to_events(&dir, changes);
            if !events.is_empty() {
                sink(events);
            }
        }) {
            Ok(w) => Some(Box::new(w)),
            Err(e) => {
                tracing::warn!(path = %path.display(), "surveillance impossible : {e}");
                None
            }
        }
    }
}

/// Collation de l'utilisateur, identique à `StrCmpLogicalW` (voir les tests de `fx-win`).
#[derive(Clone, Copy, Debug, Default)]
pub struct WinCollator;

impl Collator for WinCollator {
    fn key(&self, name: &[u16], out: &mut Vec<u8>) {
        if !fx_win::locale::natural_sort_key(name, out) {
            PortableCollator.key(name, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn events_are_deduplicated_and_statted() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("reste.txt"), b"abc")?;
        let changes = vec![
            DirChange::Added(w("reste.txt")),
            DirChange::Modified(w("reste.txt")),
            DirChange::Added(w("fantome.txt")), // déjà supprimé quand on le lit
            DirChange::RenamedFrom(w("ancien")),
        ];
        let ev = to_events(dir.path(), changes);
        assert_eq!(ev.len(), 3);
        assert!(matches!(&ev[0], FsEvent::Upsert(r) if r.name == w("reste.txt") && r.size == 3));
        assert_eq!(ev[1], FsEvent::Remove(w("fantome.txt")));
        assert_eq!(ev[2], FsEvent::Remove(w("ancien")));
        assert_eq!(
            to_events(dir.path(), vec![DirChange::Overflow]),
            vec![FsEvent::Reload]
        );
        Ok(())
    }
}
