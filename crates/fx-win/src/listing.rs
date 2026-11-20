//! Énumération rapide du contenu d'un dossier (voir ADR 0002).
//!
//! Trois méthodes, du plus riche au plus universel :
//! - [`ListMethod::IdExtdDirectoryInfo`] : `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`,
//!   fournit le tag de point d'analyse ;
//! - [`ListMethod::FullDirectoryInfo`] : `GetFileInformationByHandleEx(FileFullDirectoryInfo)` ;
//! - [`ListMethod::FindFirstFileEx`] : `FindFirstFileExW(FindExInfoBasic, LARGE_FETCH)`.
//!
//! [`list_dir`] essaie la méthode demandée puis se replie sur les suivantes si le système de
//! fichiers refuse la classe d'information. Aucune de ces méthodes ne lit les données des
//! fichiers : les fichiers OneDrive « en ligne seulement » ne sont pas téléchargés.

use std::mem::offset_of;
use std::path::Path;

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_NET_NAME, ERROR_BAD_NETPATH, ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND,
    ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL, ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER,
    ERROR_NETNAME_DELETED, ERROR_NETWORK_UNREACHABLE, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
    ERROR_OPERATION_ABORTED, ERROR_PATH_NOT_FOUND, ERROR_SEM_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO, FILE_ID_EXTD_DIR_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH,
    FileBasicInfo, FileFullDirectoryInfo, FileFullDirectoryRestartInfo, FileIdExtdDirectoryInfo,
    FileIdExtdDirectoryRestartInfo, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
    GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

use crate::handle::{FindHandle, OwnedHandle};
use crate::path::verbatim_wide;

/// Méthode d'énumération.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListMethod {
    IdExtdDirectoryInfo,
    FullDirectoryInfo,
    FindFirstFileEx,
}

impl ListMethod {
    /// Ordre de repli à partir de cette méthode.
    fn fallbacks(self) -> &'static [ListMethod] {
        use ListMethod::*;
        match self {
            IdExtdDirectoryInfo => &[IdExtdDirectoryInfo, FullDirectoryInfo, FindFirstFileEx],
            FullDirectoryInfo => &[FullDirectoryInfo, FindFirstFileEx],
            FindFirstFileEx => &[FindFirstFileEx],
        }
    }
}

/// Entrée brute d'un dossier. Le nom n'est valide que pendant l'appel du callback.
#[derive(Debug)]
pub struct RawEntry<'a> {
    /// Nom UTF-16 tel que stocké par le système de fichiers (peut contenir des substituts isolés).
    pub name: &'a [u16],
    pub attributes: u32,
    /// Tag de point d'analyse (`IO_REPARSE_TAG_*`) si `FILE_ATTRIBUTE_REPARSE_POINT`, sinon 0.
    pub reparse_tag: u32,
    pub size: u64,
    /// FILETIME (intervalles de 100 ns depuis 1601, UTC).
    pub created: i64,
    pub modified: i64,
    /// Référence de fichier (NTFS : numéro d'enregistrement MFT sur les 48 bits de poids
    /// faible) ; 0 si la méthode de listing ne la fournit pas.
    pub file_id: u64,
}

/// Options d'énumération.
#[derive(Clone, Copy, Debug)]
pub struct ListOptions {
    pub method: ListMethod,
    /// Taille du tampon pour les méthodes `GetFileInformationByHandleEx`, en octets.
    pub buffer_size: usize,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            method: ListMethod::IdExtdDirectoryInfo,
            buffer_size: 64 * 1024,
        }
    }
}

/// Erreur d'énumération, classée pour un message utilisateur compréhensible.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error("accès refusé")]
    AccessDenied,
    #[error("emplacement introuvable")]
    NotFound,
    #[error("ce chemin n'est pas un dossier")]
    NotADirectory,
    #[error("emplacement réseau injoignable")]
    NetworkUnreachable,
    #[error("opération annulée")]
    Cancelled,
    #[error("chemin invalide : {0}")]
    InvalidPath(std::io::Error),
    #[error("{0}")]
    Other(windows::core::Error),
}

fn is(e: &windows::core::Error, code: WIN32_ERROR) -> bool {
    e.code() == code.to_hresult()
}

impl From<windows::core::Error> for ListError {
    fn from(e: windows::core::Error) -> Self {
        if is(&e, ERROR_ACCESS_DENIED) {
            Self::AccessDenied
        } else if is(&e, ERROR_OPERATION_ABORTED) {
            // E/S synchrone annulée (CancelSynchronousIo) : chargement abandonné.
            Self::Cancelled
        } else if is(&e, ERROR_FILE_NOT_FOUND) || is(&e, ERROR_PATH_NOT_FOUND) || is(&e, ERROR_INVALID_NAME) {
            Self::NotFound
        } else if is(&e, ERROR_DIRECTORY) {
            Self::NotADirectory
        } else if [
            ERROR_BAD_NETPATH,
            ERROR_BAD_NET_NAME,
            ERROR_NETNAME_DELETED,
            ERROR_NETWORK_UNREACHABLE,
            ERROR_SEM_TIMEOUT,
        ]
        .iter()
        .any(|c| is(&e, *c))
        {
            Self::NetworkUnreachable
        } else {
            Self::Other(e)
        }
    }
}

/// L'erreur signale-t-elle que le système de fichiers ne connaît pas cette classe d'information ?
fn is_unsupported(e: &windows::core::Error) -> bool {
    [
        ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
        ERROR_INVALID_LEVEL,
        ERROR_INVALID_FUNCTION,
    ]
    .iter()
    .any(|c| is(e, *c))
}

enum Attempt {
    Done,
    Unsupported,
}

/// Énumère `path` en appelant `on_entry` pour chaque entrée (hors `.` et `..`).
///
/// `cancelled` est consulté entre deux tampons ; s'il renvoie `true`, l'énumération s'arrête avec
/// [`ListError::Cancelled`]. Renvoie la méthode effectivement utilisée.
pub fn list_dir(
    path: &Path,
    options: &ListOptions,
    cancelled: &dyn Fn() -> bool,
    on_entry: &mut dyn FnMut(&RawEntry<'_>),
) -> Result<ListMethod, ListError> {
    let wide = verbatim_wide(path).map_err(ListError::InvalidPath)?;
    for &method in options.method.fallbacks() {
        match list_with(method, &wide, options.buffer_size, cancelled, on_entry)? {
            Attempt::Done => return Ok(method),
            Attempt::Unsupported => continue,
        }
    }
    // FindFirstFileEx ne renvoie jamais `Unsupported`.
    Err(ListError::Other(windows::core::Error::from_hresult(
        ERROR_NOT_SUPPORTED.to_hresult(),
    )))
}

/// Énumère avec une méthode précise, **sans repli** (pour les benchmarks et les tests).
pub fn list_dir_exact(
    path: &Path,
    method: ListMethod,
    buffer_size: usize,
    on_entry: &mut dyn FnMut(&RawEntry<'_>),
) -> Result<(), ListError> {
    let wide = verbatim_wide(path).map_err(ListError::InvalidPath)?;
    match list_with(method, &wide, buffer_size, &|| false, on_entry)? {
        Attempt::Done => Ok(()),
        Attempt::Unsupported => Err(ListError::Other(windows::core::Error::from_hresult(
            ERROR_NOT_SUPPORTED.to_hresult(),
        ))),
    }
}

fn list_with(
    method: ListMethod,
    wide: &[u16],
    buffer_size: usize,
    cancelled: &dyn Fn() -> bool,
    on_entry: &mut dyn FnMut(&RawEntry<'_>),
) -> Result<Attempt, ListError> {
    match method {
        ListMethod::IdExtdDirectoryInfo => by_handle::<IdExtd>(wide, buffer_size, cancelled, on_entry),
        ListMethod::FullDirectoryInfo => by_handle::<Full>(wide, buffer_size, cancelled, on_entry),
        ListMethod::FindFirstFileEx => find_first(wide, cancelled, on_entry).map(|()| Attempt::Done),
    }
}

fn is_dot(name: &[u16]) -> bool {
    matches!(name, [0x2E] | [0x2E, 0x2E])
}

// ---------------------------------------------------------------------------------------------
// GetFileInformationByHandleEx
// ---------------------------------------------------------------------------------------------

/// Décrit une structure de la famille `FILE_*_DIR_INFO`.
trait DirInfo: Copy {
    const FIRST: FILE_INFO_BY_HANDLE_CLASS;
    const NEXT: FILE_INFO_BY_HANDLE_CLASS;
    const NAME_OFFSET: usize;
    fn next(&self) -> u32;
    fn name_bytes(&self) -> u32;
    fn entry(&self, name: &[u16]) -> RawEntryOwnedFields;
}

/// Champs d'une entrée hors nom (le nom est emprunté au tampon).
struct RawEntryOwnedFields {
    attributes: u32,
    reparse_tag: u32,
    size: u64,
    created: i64,
    modified: i64,
    file_id: u64,
}

#[derive(Clone, Copy)]
struct IdExtd(FILE_ID_EXTD_DIR_INFO);
#[derive(Clone, Copy)]
struct Full(FILE_FULL_DIR_INFO);

impl DirInfo for IdExtd {
    const FIRST: FILE_INFO_BY_HANDLE_CLASS = FileIdExtdDirectoryRestartInfo;
    const NEXT: FILE_INFO_BY_HANDLE_CLASS = FileIdExtdDirectoryInfo;
    const NAME_OFFSET: usize = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    fn next(&self) -> u32 {
        self.0.NextEntryOffset
    }
    fn name_bytes(&self) -> u32 {
        self.0.FileNameLength
    }
    fn entry(&self, _name: &[u16]) -> RawEntryOwnedFields {
        let i = &self.0;
        RawEntryOwnedFields {
            attributes: i.FileAttributes,
            reparse_tag: if i.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                i.ReparsePointTag
            } else {
                0
            },
            size: i.EndOfFile.max(0) as u64,
            created: i.CreationTime,
            modified: i.LastWriteTime,
            file_id: u64::from_le_bytes(i.FileId.Identifier[..8].try_into().unwrap_or([0; 8])),
        }
    }
}

impl DirInfo for Full {
    const FIRST: FILE_INFO_BY_HANDLE_CLASS = FileFullDirectoryRestartInfo;
    const NEXT: FILE_INFO_BY_HANDLE_CLASS = FileFullDirectoryInfo;
    const NAME_OFFSET: usize = offset_of!(FILE_FULL_DIR_INFO, FileName);
    fn next(&self) -> u32 {
        self.0.NextEntryOffset
    }
    fn name_bytes(&self) -> u32 {
        self.0.FileNameLength
    }
    fn entry(&self, _name: &[u16]) -> RawEntryOwnedFields {
        let i = &self.0;
        RawEntryOwnedFields {
            attributes: i.FileAttributes,
            // Pour un point d'analyse, EaSize contient le tag (voir NOTES-WIN32.md, vérifié par test).
            reparse_tag: if i.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                i.EaSize
            } else {
                0
            },
            size: i.EndOfFile.max(0) as u64,
            created: i.CreationTime,
            modified: i.LastWriteTime,
            file_id: 0,
        }
    }
}

fn open_dir(wide: &[u16]) -> Result<OwnedHandle, ListError> {
    // SAFETY: `wide` est terminé par un nul (garanti par `verbatim_wide`) et vit pendant l'appel.
    let h = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0 | SYNCHRONIZE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }?;
    let handle = OwnedHandle(h);
    // FILE_FLAG_BACKUP_SEMANTICS ouvre aussi les fichiers : on vérifie qu'il s'agit d'un dossier.
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: `basic` est une FILE_BASIC_INFO locale, la taille passée est exactement la sienne.
    unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileBasicInfo,
            &mut basic as *mut FILE_BASIC_INFO as *mut _,
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    }?;
    if basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(ListError::NotADirectory);
    }
    Ok(handle)
}

fn by_handle<T: DirInfo>(
    wide: &[u16],
    buffer_size: usize,
    cancelled: &dyn Fn() -> bool,
    on_entry: &mut dyn FnMut(&RawEntry<'_>),
) -> Result<Attempt, ListError> {
    let handle = open_dir(wide)?;
    // Tampon aligné sur 8 octets, comme l'exigent les structures FILE_*_DIR_INFO.
    let words = buffer_size.max(4096) / 8;
    let mut buf: Vec<u64> = vec![0; words];
    let bytes = words * 8;
    let mut class = T::FIRST;
    let mut first = true;
    let mut name_tmp: Vec<u16> = Vec::new();
    loop {
        if cancelled() {
            return Err(ListError::Cancelled);
        }
        // SAFETY: `buf` est un tampon possédé de `bytes` octets, aligné sur 8, vivant pendant l'appel.
        let r =
            unsafe { GetFileInformationByHandleEx(handle.0, class, buf.as_mut_ptr().cast(), bytes as u32) };
        if let Err(e) = r {
            if is(&e, ERROR_NO_MORE_FILES) {
                return Ok(Attempt::Done);
            }
            if first && is_unsupported(&e) {
                return Ok(Attempt::Unsupported);
            }
            return Err(e.into());
        }
        first = false;
        class = T::NEXT;

        // SAFETY: on réinterprète le tampon `u64` comme des octets : même mémoire, alignement
        // plus faible, longueur exacte en octets.
        let raw: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), bytes) };
        let mut off = 0usize;
        loop {
            if off + T::NAME_OFFSET > raw.len() {
                break;
            }
            // SAFETY: l'intervalle [off, off + NAME_OFFSET) est dans le tampon (vérifié ci-dessus) ;
            // `read_unaligned` n'exige aucun alignement et T est une structure C plain-old-data.
            // On ne lit que les champs d'en-tête, qui précèdent `FileName`.
            let info: T = unsafe { std::ptr::read_unaligned(raw.as_ptr().add(off).cast::<T>()) };
            let name_len = info.name_bytes() as usize;
            let name_start = off + T::NAME_OFFSET;
            if name_start + name_len > raw.len() || !name_len.is_multiple_of(2) {
                break; // entrée tronquée ou incohérente : on s'arrête plutôt que de lire hors tampon
            }
            let name_ptr = raw[name_start..].as_ptr();
            let name: &[u16] = if (name_ptr as usize).is_multiple_of(align_of::<u16>()) {
                // SAFETY: [name_start, name_start + name_len) est dans le tampon (vérifié), le
                // pointeur est aligné pour u16 (vérifié), la mémoire est initialisée par l'API et
                // n'est pas modifiée tant que `name` est vivant (avant le prochain appel).
                unsafe { std::slice::from_raw_parts(name_ptr.cast::<u16>(), name_len / 2) }
            } else {
                name_tmp.clear();
                name_tmp.extend(
                    raw[name_start..name_start + name_len]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes([c[0], c[1]])),
                );
                &name_tmp
            };
            if !is_dot(name) {
                let f = info.entry(name);
                on_entry(&RawEntry {
                    name,
                    attributes: f.attributes,
                    reparse_tag: f.reparse_tag,
                    size: f.size,
                    created: f.created,
                    modified: f.modified,
                    file_id: f.file_id,
                });
            }
            let next = info.next() as usize;
            if next == 0 {
                break;
            }
            off += next;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// FindFirstFileExW
// ---------------------------------------------------------------------------------------------

fn filetime(ft: windows::Win32::Foundation::FILETIME) -> i64 {
    (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) as i64
}

fn emit_find(data: &WIN32_FIND_DATAW, on_entry: &mut dyn FnMut(&RawEntry<'_>)) {
    let len = data
        .cFileName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(data.cFileName.len());
    let name = &data.cFileName[..len];
    if is_dot(name) {
        return;
    }
    let attributes = data.dwFileAttributes;
    on_entry(&RawEntry {
        name,
        attributes,
        reparse_tag: if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            data.dwReserved0
        } else {
            0
        },
        size: ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64,
        created: filetime(data.ftCreationTime),
        modified: filetime(data.ftLastWriteTime),
        file_id: 0,
    });
}

fn find_first(
    wide: &[u16],
    cancelled: &dyn Fn() -> bool,
    on_entry: &mut dyn FnMut(&RawEntry<'_>),
) -> Result<(), ListError> {
    // Motif : <dossier>\*
    let mut pattern: Vec<u16> = wide[..wide.len().saturating_sub(1)].to_vec();
    if pattern.last() != Some(&(b'\\' as u16)) {
        pattern.push(b'\\' as u16);
    }
    pattern.extend([b'*' as u16, 0]);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` est terminé par un nul ; `data` est une WIN32_FIND_DATAW locale, type
    // attendu pour FindExInfoBasic.
    let first = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let h = match first {
        Ok(h) => FindHandle(h),
        // Dossier existant mais vide (racine de volume sans « . ») : aucune entrée.
        Err(e) if is(&e, ERROR_FILE_NOT_FOUND) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut n = 0u32;
    loop {
        emit_find(&data, on_entry);
        n = n.wrapping_add(1);
        if n.is_multiple_of(512) && cancelled() {
            return Err(ListError::Cancelled);
        }
        // SAFETY: handle de recherche valide possédé par `h`, `data` local du bon type.
        match unsafe { FindNextFileW(h.0, &mut data) } {
            Ok(()) => {}
            Err(e) if is(&e, ERROR_NO_MORE_FILES) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}

/// Entrée possédée (copie d'une [`RawEntry`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedEntry {
    pub name: Vec<u16>,
    pub attributes: u32,
    pub reparse_tag: u32,
    pub size: u64,
    pub created: i64,
    pub modified: i64,
}

/// Métadonnées de l'entrée `name` du dossier `dir`, sans ouvrir le fichier (donc sans
/// déclencher de téléchargement OneDrive). `None` si elle n'existe pas (ou plus).
pub fn stat_entry(dir: &Path, name: &[u16]) -> Option<OwnedEntry> {
    let mut wide = verbatim_wide(dir).ok()?;
    wide.pop(); // nul final
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.extend_from_slice(name);
    wide.push(0);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: chemin terminé par un nul ; `data` locale du type attendu par FindExInfoBasic.
    let h = unsafe {
        FindFirstFileExW(
            PCWSTR(wide.as_ptr()),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            None,
            Default::default(),
        )
    }
    .ok()?;
    let _h = FindHandle(h);
    let mut out = None;
    emit_find(&data, &mut |e| {
        out = Some(OwnedEntry {
            name: e.name.to_vec(),
            attributes: e.attributes,
            reparse_tag: e.reparse_tag,
            size: e.size,
            created: e.created,
            modified: e.modified,
        })
    });
    out
}

/// Référence de fichier (NTFS : enregistrement MFT) et numéro de série du volume de `path`.
/// Ouvre l'élément sans droit de lecture du contenu (aucun téléchargement OneDrive), sans
/// suivre un point d'analyse.
pub fn file_ref(path: &Path) -> Option<(u64, u32)> {
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        GetFileInformationByHandle,
    };
    let wide = verbatim_wide(path).ok()?;
    // SAFETY: `wide` est terminé par un nul et vit pendant l'appel ; le handle est possédé par
    // `OwnedHandle`.
    let h = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .ok()?;
    let handle = OwnedHandle(h);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: structure locale passée par référence ; handle valide.
    unsafe { GetFileInformationByHandle(handle.0, &mut info) }.ok()?;
    let r = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    Some((r, info.dwVolumeSerialNumber))
}
