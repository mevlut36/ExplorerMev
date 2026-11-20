//! Abstraction du système de fichiers pour le listing.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::folder::{ATTR_DIRECTORY, RawEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    AccessDenied,
    NotFound,
    NotADirectory,
    Network,
    Cancelled,
    Other,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct SourceError {
    pub kind: ErrorKind,
    pub message: String,
}

impl SourceError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Message destiné à l'utilisateur.
    pub fn user_message(&self) -> String {
        match self.kind {
            ErrorKind::AccessDenied => "Vous n'avez pas l'autorisation d'accéder à ce dossier.".into(),
            ErrorKind::NotFound => {
                "Ce dossier est introuvable. Il a peut-être été déplacé ou supprimé.".into()
            }
            ErrorKind::NotADirectory => "Ce chemin désigne un fichier, pas un dossier.".into(),
            ErrorKind::Network => "L'emplacement réseau est injoignable.".into(),
            ErrorKind::Cancelled => "Chargement annulé.".into(),
            ErrorKind::Other => format!("Impossible d'afficher ce dossier : {}", self.message),
        }
    }
}

/// Source d'entrées de dossier (le système réel, ou un faux pour les tests).
pub trait DirSource: Send + Sync + 'static {
    /// Prépare le thread de travail qui appellera la source (initialisation COM, par exemple).
    /// La valeur renvoyée est conservée jusqu'à la fin du thread.
    fn init_worker_thread(&self) -> Option<Box<dyn Any>> {
        None
    }

    /// Énumère `path`. `cancelled` doit être consulté régulièrement.
    fn list(
        &self,
        path: &Path,
        cancelled: &dyn Fn() -> bool,
        on_entry: &mut dyn FnMut(&RawEntry<'_>),
    ) -> Result<(), SourceError>;

    /// Libellé de type pour ce nom de fichier (seule l'extension compte).
    fn type_name(&self, file_name: &[u16], is_dir: bool) -> Option<String>;
}

/// Source en mémoire pour les tests : dossiers déclarés à l'avance, délai optionnel par entrée.
#[derive(Default)]
pub struct MemorySource {
    pub folders: HashMap<PathBuf, Vec<(String, bool, u64)>>,
    pub delay_per_entry: Option<Duration>,
}

impl MemorySource {
    pub fn with(mut self, path: &str, entries: &[(&str, bool, u64)]) -> Self {
        self.folders.insert(
            PathBuf::from(path),
            entries.iter().map(|(n, d, s)| (n.to_string(), *d, *s)).collect(),
        );
        self
    }
}

impl DirSource for MemorySource {
    fn list(
        &self,
        path: &Path,
        cancelled: &dyn Fn() -> bool,
        on_entry: &mut dyn FnMut(&RawEntry<'_>),
    ) -> Result<(), SourceError> {
        let entries = self
            .folders
            .get(path)
            .ok_or_else(|| SourceError::new(ErrorKind::NotFound, "absent"))?;
        for (i, (name, dir, size)) in entries.iter().enumerate() {
            if i % 64 == 0 && cancelled() {
                return Err(SourceError::new(ErrorKind::Cancelled, "annulé"));
            }
            if let Some(d) = self.delay_per_entry {
                std::thread::sleep(d);
            }
            let n: Vec<u16> = name.encode_utf16().collect();
            on_entry(&RawEntry {
                name: &n,
                attributes: if *dir { ATTR_DIRECTORY } else { 0 },
                reparse_tag: 0,
                size: *size,
                created: 0,
                modified: i as i64,
            });
        }
        Ok(())
    }

    fn type_name(&self, file_name: &[u16], is_dir: bool) -> Option<String> {
        if is_dir {
            return Some("Dossier de fichiers".into());
        }
        let ext = crate::folder::extension_key(file_name);
        Some(if ext.is_empty() {
            "Fichier".into()
        } else {
            format!("Fichier {}", String::from_utf16_lossy(&ext))
        })
    }
}
