//! Services du Shell : nom de type, ouverture de fichier, dossiers connus.
//!
//! Ces fonctions exigent un thread initialisé en COM STA (voir [`crate::com::ComApartment`]) et
//! peuvent bloquer : elles ne doivent jamais être appelées depuis le thread d'interface.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Profile, FOLDERID_Videos, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHFILEINFOW,
    SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetKnownFolderPath, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use crate::path::wide_nul;

/// Libellé de type tel qu'affiché par l'Explorateur (« Document texte », « Dossier de fichiers »).
///
/// N'accède pas au fichier (`SHGFI_USEFILEATTRIBUTES`) : seul le nom (son extension) compte.
pub fn type_name(file_name: &[u16], is_dir: bool) -> Option<String> {
    let name: Vec<u16> = file_name.iter().copied().chain(std::iter::once(0)).collect();
    let attrs = if is_dir {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let mut info = SHFILEINFOW::default();
    // SAFETY: `name` est terminé par un nul ; `info` est une SHFILEINFOW locale dont la taille
    // exacte est transmise.
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            attrs,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = info
        .szTypeName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szTypeName.len());
    (len > 0).then(|| String::from_utf16_lossy(&info.szTypeName[..len]))
}

/// Ouvre `path` avec l'action par défaut (comme un double-clic dans l'Explorateur).
///
/// `owner` : HWND de la fenêtre parente des éventuelles boîtes de dialogue du Shell.
pub fn open(path: &Path, owner: Option<isize>) -> windows::core::Result<()> {
    let file = wide_nul(path.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        hwnd: HWND(owner.unwrap_or(0) as *mut c_void),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` est initialisée avec sa taille ; `file` (pointé par lpFile) vit jusqu'au
    // retour de l'appel, synchrone grâce à SEE_MASK_NOASYNC.
    unsafe { ShellExecuteExW(&mut info) }
}

/// Dossiers connus utilisés par le volet de navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownFolder {
    Profile,
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
}

/// Chemin d'un dossier connu (tient compte des redirections, OneDrive compris).
pub fn known_folder(folder: KnownFolder) -> Option<PathBuf> {
    let id = match folder {
        KnownFolder::Profile => &FOLDERID_Profile,
        KnownFolder::Desktop => &FOLDERID_Desktop,
        KnownFolder::Downloads => &FOLDERID_Downloads,
        KnownFolder::Documents => &FOLDERID_Documents,
        KnownFolder::Pictures => &FOLDERID_Pictures,
        KnownFolder::Music => &FOLDERID_Music,
        KnownFolder::Videos => &FOLDERID_Videos,
    };
    // SAFETY: `id` pointe sur un GUID statique ; le PWSTR renvoyé est libéré ci-dessous par
    // CoTaskMemFree, comme l'exige la documentation, après copie.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = PathBuf::from(OsString::from_wide(p.as_wide()));
        CoTaskMemFree(Some(p.0 as *const c_void));
        Some(path)
    }
}

/// Raccourci : `OsStr` vers UTF-16 sans nul.
pub fn to_wide(s: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().collect()
}
