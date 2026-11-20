//! Surveillance d'un dossier par `ReadDirectoryChangesW` (E/S asynchrone), avec regroupement
//! des événements. Un thread par dossier surveillé ; arrêt propre par `CancelIoEx`.

use std::mem::offset_of;
use std::path::Path;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};
use windows::core::PCWSTR;

use crate::handle::OwnedHandle;
use crate::listing::ListError;
use crate::path::verbatim_wide;

/// Changement observé dans le dossier (noms relatifs, UTF-16 brut).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirChange {
    Added(Vec<u16>),
    Removed(Vec<u16>),
    Modified(Vec<u16>),
    RenamedFrom(Vec<u16>),
    RenamedTo(Vec<u16>),
    /// Trop de changements d'un coup (tampon débordé) : il faut relister.
    Overflow,
    /// Le dossier n'est plus accessible (supprimé, réseau perdu) ; la surveillance s'arrête.
    Gone,
}

/// Surveillance active ; s'arrête à la destruction.
pub struct Watcher {
    stop: OwnedHandle,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: événement possédé par `self`, vivant jusqu'à la fin de cette fonction.
        let _ = unsafe { SetEvent(self.stop.0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Après le premier événement d'un lot, on attend ce silence avant de livrer…
pub const QUIET: Duration = Duration::from_millis(60);
/// …sans dépasser ce délai total.
pub const MAX_DELAY: Duration = Duration::from_millis(250);

/// Commence à surveiller `path`. `on_batch` est appelé depuis le thread de surveillance.
pub fn watch(
    path: &Path,
    on_batch: impl FnMut(Vec<DirChange>) + Send + 'static,
) -> Result<Watcher, ListError> {
    start(path, Mode::Dir, on_batch)
}

/// Surveille toute l'arborescence de `path` (un volume entier pour l'index de recherche) :
/// noms et attributs seulement, chemins relatifs à `path`.
pub fn watch_tree(
    path: &Path,
    on_batch: impl FnMut(Vec<DirChange>) + Send + 'static,
) -> Result<Watcher, ListError> {
    start(path, Mode::Tree, on_batch)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dir,
    Tree,
}

fn start(
    path: &Path,
    mode: Mode,
    mut on_batch: impl FnMut(Vec<DirChange>) + Send + 'static,
) -> Result<Watcher, ListError> {
    let wide = verbatim_wide(path).map_err(ListError::InvalidPath)?;
    // SAFETY: `wide` terminé par un nul ; handle possédé par OwnedHandle.
    let dir = OwnedHandle(unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }?);
    // SAFETY: événements sans nom, possédés.
    let io_event = OwnedHandle(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?);
    // SAFETY: idem.
    let stop = OwnedHandle(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?);
    let stop_raw = stop.0.0 as isize;
    let thread = std::thread::Builder::new()
        .name(
            if mode == Mode::Tree {
                "fx-watch-tree"
            } else {
                "fx-watch"
            }
            .into(),
        )
        .spawn(move || {
            let stop = HANDLE(stop_raw as *mut _);
            run(&dir, &io_event, stop, mode, &mut on_batch);
        })
        .map_err(|e| {
            ListError::Other(windows::core::Error::new(
                windows::core::HRESULT(-1),
                e.to_string(),
            ))
        })?;
    Ok(Watcher {
        stop,
        thread: Some(thread),
    })
}

const FILTER: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0
        | FILE_NOTIFY_CHANGE_DIR_NAME.0
        | FILE_NOTIFY_CHANGE_ATTRIBUTES.0
        | FILE_NOTIFY_CHANGE_SIZE.0
        | FILE_NOTIFY_CHANGE_LAST_WRITE.0
        | FILE_NOTIFY_CHANGE_CREATION.0,
);

/// Arborescence : noms et attributs (caché, système) seulement.
const TREE_FILTER: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0 | FILE_NOTIFY_CHANGE_DIR_NAME.0 | FILE_NOTIFY_CHANGE_ATTRIBUTES.0,
);

fn run(
    dir: &OwnedHandle,
    io_event: &OwnedHandle,
    stop: HANDLE,
    mode: Mode,
    on_batch: &mut dyn FnMut(Vec<DirChange>),
) {
    let (subtree, filter, size) = match mode {
        Mode::Dir => (false, FILTER, 64 * 1024),
        Mode::Tree => (true, TREE_FILTER, 512 * 1024),
    };
    let mut buf: Vec<u64> = vec![0; size / 8];
    let bytes = (buf.len() * 8) as u32;
    let mut pending: Vec<DirChange> = Vec::new();
    let mut first_at: Option<Instant> = None;
    let mut last_at = Instant::now();
    loop {
        let mut ov = OVERLAPPED {
            hEvent: io_event.0,
            ..Default::default()
        };
        // SAFETY: `buf` (aligné sur 8 octets, exigence DWORD) et `ov` restent vivants et
        // immobiles jusqu'à la fin de l'E/S : on attend toujours sa complétion (ou son
        // annulation) avant de les réutiliser ou de quitter.
        let issued = unsafe {
            let _ = ResetEvent(io_event.0);
            ReadDirectoryChangesW(
                dir.0,
                buf.as_mut_ptr().cast(),
                bytes,
                subtree,
                filter,
                None,
                Some(&mut ov),
                None,
            )
        };
        if issued.is_err() {
            pending.push(DirChange::Gone);
            on_batch(pending);
            return;
        }
        // Attente de la complétion, en livrant le lot en cours quand le délai est atteint.
        loop {
            let timeout = match first_at {
                None => INFINITE,
                Some(first) => {
                    let quiet_left = QUIET.saturating_sub(last_at.elapsed());
                    let max_left = MAX_DELAY.saturating_sub(first.elapsed());
                    quiet_left.min(max_left).as_millis() as u32
                }
            };
            // SAFETY: deux handles d'événement valides.
            let w = unsafe { WaitForMultipleObjects(&[io_event.0, stop], false, timeout) };
            if w == WAIT_TIMEOUT {
                on_batch(std::mem::take(&mut pending));
                first_at = None;
                continue;
            }
            if w != WAIT_OBJECT_0 {
                // Arrêt demandé (ou attente en échec) : on annule l'E/S et on attend sa fin
                // avant de libérer le tampon.
                // SAFETY: `ov` désigne l'E/S en cours sur `dir`.
                unsafe {
                    let _ = CancelIoEx(dir.0, Some(&ov));
                    let mut n = 0u32;
                    let _ = GetOverlappedResult(dir.0, &ov, &mut n, true);
                }
                return;
            }
            let mut n = 0u32;
            // SAFETY: l'E/S est terminée (événement signalé).
            let r = unsafe { GetOverlappedResult(dir.0, &ov, &mut n, false) };
            match r {
                Err(e) if e.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => pending.push(DirChange::Overflow),
                Err(_) => {
                    pending.push(DirChange::Gone);
                    on_batch(pending);
                    return;
                }
                Ok(()) if n == 0 => pending.push(DirChange::Overflow),
                Ok(()) => {
                    // SAFETY: l'API a écrit `n` octets valides au début du tampon.
                    let raw = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), n as usize) };
                    parse(raw, &mut pending);
                }
            }
            last_at = Instant::now();
            first_at.get_or_insert(last_at);
            break; // relancer une lecture
        }
    }
}

fn parse(raw: &[u8], out: &mut Vec<DirChange>) {
    let name_off = offset_of!(FILE_NOTIFY_INFORMATION, FileName);
    let mut off = 0usize;
    loop {
        if off + name_off > raw.len() {
            return;
        }
        // SAFETY: en-tête entièrement dans le tampon (vérifié) ; lecture non alignée d'une
        // structure C plain-old-data.
        let info: FILE_NOTIFY_INFORMATION =
            unsafe { std::ptr::read_unaligned(raw.as_ptr().add(off).cast::<FILE_NOTIFY_INFORMATION>()) };
        let len = info.FileNameLength as usize;
        let start = off + name_off;
        if start + len > raw.len() {
            return;
        }
        let name: Vec<u16> = raw[start..start + len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        out.push(match info.Action.0 {
            1 => DirChange::Added(name),
            2 => DirChange::Removed(name),
            4 => DirChange::RenamedFrom(name),
            5 => DirChange::RenamedTo(name),
            _ => DirChange::Modified(name),
        });
        if info.NextEntryOffset == 0 {
            return;
        }
        off += info.NextEntryOffset as usize;
    }
}
