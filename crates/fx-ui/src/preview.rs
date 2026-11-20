//! Volet d'aperçu : choix du type d'aperçu et préparation hors du thread d'interface.
//!
//! Ordre : message (dossier, fichier en ligne seulement, sélection multiple) ; texte (lecture
//! des 64 premiers Kio) ; image (vignette du Shell) ; prévisualiseur natif du Shell
//! (`IPreviewHandler`, PDF, Office…) ; à défaut, vignette du Shell ou message.

use std::cell::RefCell;
use std::io::Read;
use std::path::Path;

use fx_win::preview::{NativePreview, Rect};

pub const TEXT_EXTS: &[&str] = &[
    "txt",
    "md",
    "log",
    "csv",
    "tsv",
    "json",
    "xml",
    "ini",
    "toml",
    "yaml",
    "yml",
    "rs",
    "py",
    "js",
    "ts",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "java",
    "go",
    "html",
    "htm",
    "css",
    "bat",
    "cmd",
    "ps1",
    "sh",
    "sql",
    "cfg",
    "conf",
    "gitignore",
    "lock",
    "slint",
    "properties",
    "editorconfig",
    "svg",
];
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "ico", "avif", "jxr",
];

/// Taille lue pour l'aperçu texte.
const TEXT_LIMIT: usize = 64 * 1024;

/// Lit le début d'un fichier texte (UTF-8, ou UTF-16 avec BOM). `None` pour un fichier binaire.
pub fn read_text(path: &Path) -> Option<String> {
    let mut buf = Vec::with_capacity(TEXT_LIMIT);
    std::fs::File::open(path)
        .ok()?
        .take(TEXT_LIMIT as u64)
        .read_to_end(&mut buf)
        .ok()?;
    decode_text(&buf)
}

pub fn decode_text(buf: &[u8]) -> Option<String> {
    if let Some(rest) = buf.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if let Some(rest) = buf.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let body = buf.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(buf);
    // Octet nul dans les premiers Kio : fichier binaire.
    if body.iter().take(4096).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(body).into_owned())
}

thread_local! {
    /// Prévisualiseur natif affiché ; ne vit que sur le thread de l'hôte d'aperçu.
    static NATIVE: RefCell<Option<NativePreview>> = const { RefCell::new(None) };
}

/// Ferme le prévisualiseur natif (thread de l'hôte d'aperçu).
pub fn close_native() {
    NATIVE.with_borrow_mut(|n| n.take());
}

/// Ouvre un prévisualiseur natif (thread de l'hôte d'aperçu). `Ok(false)` : aucun n'est
/// enregistré pour ce type.
pub fn open_native(parent: isize, rect: Rect, path: &Path, ext: &str) -> Result<bool, String> {
    close_native();
    let Some(clsid) = fx_win::preview::handler_for(ext) else {
        return Ok(false);
    };
    let native = NativePreview::open(parent, rect, path, clsid).map_err(|e| e.message())?;
    NATIVE.with_borrow_mut(|n| *n = Some(native));
    Ok(true)
}

/// Suit la mise en page (thread de l'hôte d'aperçu).
pub fn resize_native(rect: Rect) {
    NATIVE.with_borrow(|n| {
        if let Some(n) = n {
            n.resize(rect);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_utf16_and_rejects_binary() {
        assert_eq!(decode_text("été".as_bytes()).as_deref(), Some("été"));
        assert_eq!(decode_text(&[0xEF, 0xBB, 0xBF, b'a']).as_deref(), Some("a"));
        assert_eq!(
            decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]).as_deref(),
            Some("hi")
        );
        assert_eq!(
            decode_text(&[0xFE, 0xFF, 0, b'o', 0, b'k']).as_deref(),
            Some("ok")
        );
        assert_eq!(decode_text(&[b'M', b'Z', 0, 0, 3]), None);
    }
}
