//! Implémentations Windows des abstractions de `fx-core`, au-dessus de `fx-win`.

use std::any::Any;
use std::path::Path;

use fx_win::listing::{ListError, ListOptions, list_dir};

use crate::collate::{Collator, PortableCollator};
use crate::folder::RawEntry;
use crate::source::{DirSource, ErrorKind, SourceError};

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
        fx_win::shell::type_name(file_name, is_dir)
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
