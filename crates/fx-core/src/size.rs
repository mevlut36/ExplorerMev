//! Taille d'un dossier, calculée en arrière-plan à la demande : parcours parallèle, annulable,
//! qui ne suit jamais les points d'analyse (jonctions, liens symboliques) pour éviter les boucles
//! et les doubles comptes, comme l'Explorateur.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::folder::{ATTR_DIRECTORY, ATTR_REPARSE_POINT};
use crate::source::DirSource;

/// Compteurs partagés, lus pendant le calcul pour afficher la progression.
#[derive(Debug, Default)]
pub struct SizeProgress {
    pub bytes: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    /// Dossiers illisibles (accès refusé…) : la taille est alors un minimum.
    pub errors: AtomicU64,
    pub done: AtomicBool,
    pub cancelled: AtomicBool,
}

impl SizeProgress {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// Nom UTF-16 brut vers `OsString` sans perte (substituts isolés compris).
fn name_of(name: &[u16]) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(name)
    }
    #[cfg(not(windows))]
    {
        std::ffi::OsString::from(String::from_utf16_lossy(name))
    }
}

fn walk<'s>(source: &'s dyn DirSource, dir: PathBuf, p: &'s SizeProgress, scope: &rayon::Scope<'s>) {
    if p.cancelled.load(Ordering::Relaxed) {
        return;
    }
    let mut subdirs = Vec::new();
    let cancelled = || p.cancelled.load(Ordering::Relaxed);
    let r = source.list(&dir, &cancelled, &mut |e| {
        if e.attributes & ATTR_DIRECTORY != 0 {
            // Jonctions et liens : comptés comme entrées, jamais suivis.
            if e.attributes & ATTR_REPARSE_POINT == 0 {
                subdirs.push(dir.join(name_of(e.name)));
            }
            p.dirs.fetch_add(1, Ordering::Relaxed);
        } else {
            p.files.fetch_add(1, Ordering::Relaxed);
            p.bytes.fetch_add(e.size, Ordering::Relaxed);
        }
    });
    if r.is_err() {
        p.errors.fetch_add(1, Ordering::Relaxed);
    }
    for d in subdirs {
        scope.spawn(move |s| walk(source, d, p, s));
    }
}

/// Lance le calcul de la taille de `root` sur un thread dédié ; renvoie les compteurs.
pub fn start(source: Arc<dyn DirSource>, root: PathBuf) -> Arc<SizeProgress> {
    let progress = Arc::new(SizeProgress::default());
    let p = progress.clone();
    let spawned = std::thread::Builder::new().name("fx-size".into()).spawn(move || {
        rayon::scope(|s| walk(&*source, root, &p, s));
        p.done.store(true, Ordering::Release);
    });
    if spawned.is_err() {
        progress.done.store(true, Ordering::Release);
        progress.errors.fetch_add(1, Ordering::Relaxed);
    }
    progress
}

/// Version synchrone (tests).
pub fn compute(source: &dyn DirSource, root: &Path) -> SizeProgress {
    let p = SizeProgress::default();
    rayon::scope(|s| walk(source, root.to_path_buf(), &p, s));
    p.done.store(true, Ordering::Release);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    #[test]
    fn sums_files_recursively() {
        let src = MemorySource::default()
            .with("R", &[("a.bin", false, 10), ("sous", true, 0)])
            .with(r"R\sous", &[("b.bin", false, 5), ("plus", true, 0)])
            .with(r"R\sous\plus", &[("c.bin", false, 1)]);
        let p = compute(&src, Path::new("R"));
        assert_eq!(p.bytes(), 16);
        assert_eq!(p.files.load(Ordering::Relaxed), 3);
        assert_eq!(p.dirs.load(Ordering::Relaxed), 2);
        assert_eq!(p.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn unreadable_subfolder_is_counted_as_error() {
        let src = MemorySource::default().with("R", &[("a.bin", false, 10), ("fantome", true, 0)]);
        let p = compute(&src, Path::new("R"));
        assert_eq!(p.bytes(), 10);
        assert_eq!(p.errors.load(Ordering::Relaxed), 1);
    }
}
