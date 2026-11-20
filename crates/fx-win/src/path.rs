//! Conversion des chemins vers la forme `\\?\` (sans limite MAX_PATH).

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const VERBATIM: &str = r"\\?\";
const VERBATIM_UNC: &str = r"\\?\UNC\";
const DEVICE: &str = r"\\.\";

fn starts_with(w: &[u16], prefix: &str) -> bool {
    let mut it = w.iter();
    prefix.encode_utf16().all(|c| it.next() == Some(&c))
}

/// Chemin absolu en UTF-16 **terminé par un nul**, sous forme `\\?\C:\…` ou `\\?\UNC\serveur\…`.
///
/// Le chemin est d'abord normalisé (`std::path::absolute`, qui résout `.`, `..` et `/`), car la
/// forme `\\?\` désactive toute normalisation côté Windows. Les chemins déjà en `\\?\` ou en
/// `\\.\` sont conservés tels quels.
pub fn verbatim_wide(path: &Path) -> std::io::Result<Vec<u16>> {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    if starts_with(&raw, VERBATIM) || starts_with(&raw, DEVICE) {
        let mut out = raw;
        out.push(0);
        return Ok(out);
    }
    let abs = std::path::absolute(path)?;
    let w: Vec<u16> = abs.as_os_str().encode_wide().collect();
    let mut out = Vec::with_capacity(w.len() + 9);
    if starts_with(&w, r"\\") {
        out.extend(VERBATIM_UNC.encode_utf16());
        out.extend_from_slice(&w[2..]);
    } else {
        out.extend(VERBATIM.encode_utf16());
        out.extend_from_slice(&w);
    }
    out.push(0);
    Ok(out)
}

/// Retire le préfixe `\\?\` (ou `\\?\UNC\`) pour l'affichage.
pub fn display_form(path: &Path) -> PathBuf {
    let w: Vec<u16> = path.as_os_str().encode_wide().collect();
    if starts_with(&w, VERBATIM_UNC) {
        let mut out: Vec<u16> = r"\\".encode_utf16().collect();
        out.extend_from_slice(&w[VERBATIM_UNC.len()..]);
        PathBuf::from(OsString::from_wide(&out))
    } else if starts_with(&w, VERBATIM) {
        PathBuf::from(OsString::from_wide(&w[VERBATIM.len()..]))
    } else {
        path.to_path_buf()
    }
}

/// Chemin pour les API du Shell (`SHParseDisplayName`, `SHCreateItemFromParsingName`,
/// `ShellExecuteExW`…), UTF-16 terminé par un nul : absolu et normalisé (`/` devient `\`, `.` et
/// `..` résolus), sans préfixe `\\?\` que le Shell n'accepte pas partout. Le Shell refuse les
/// barres obliques avec `E_INVALIDARG` (voir NOTES-WIN32.md).
pub fn shell_path(path: &Path) -> Vec<u16> {
    let shown = display_form(path);
    let abs = std::path::absolute(&shown).unwrap_or(shown);
    wide_nul(abs.as_os_str())
}

/// UTF-16 terminé par un nul, sans transformation.
pub fn wide_nul(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(w: &[u16]) -> String {
        String::from_utf16_lossy(&w[..w.len() - 1])
    }

    #[test]
    fn drive_path_gets_prefix_and_is_normalized() -> std::io::Result<()> {
        assert_eq!(
            s(&verbatim_wide(Path::new(r"C:\Windows\..\Users/x"))?),
            r"\\?\C:\Users\x"
        );
        Ok(())
    }

    #[test]
    fn unc_path_uses_unc_prefix() -> std::io::Result<()> {
        assert_eq!(
            s(&verbatim_wide(Path::new(r"\\srv\share\a"))?),
            r"\\?\UNC\srv\share\a"
        );
        Ok(())
    }

    #[test]
    fn verbatim_is_kept() -> std::io::Result<()> {
        assert_eq!(s(&verbatim_wide(Path::new(r"\\?\C:\a\..\b"))?), r"\\?\C:\a\..\b");
        Ok(())
    }

    #[test]
    fn shell_path_normalizes_slashes() {
        assert_eq!(s(&shell_path(Path::new("C:/Users/./x/../y"))), r"C:\Users\y");
        assert_eq!(s(&shell_path(Path::new(r"\\?\C:\a"))), r"C:\a");
    }

    #[test]
    fn display_strips_prefix() {
        assert_eq!(display_form(Path::new(r"\\?\C:\a")), PathBuf::from(r"C:\a"));
        assert_eq!(
            display_form(Path::new(r"\\?\UNC\srv\s")),
            PathBuf::from(r"\\srv\s")
        );
        assert_eq!(display_form(Path::new(r"D:\x")), PathBuf::from(r"D:\x"));
    }
}
