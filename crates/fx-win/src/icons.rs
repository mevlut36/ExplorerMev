//! Icônes du Shell converties en RGBA (alpha non prémultiplié) pour l'interface.
//!
//! - [`type_icon`] : icône d'un type (extension), via la liste d'images système. N'accède jamais
//!   au fichier : sûr pour les fichiers OneDrive en ligne seulement.
//! - [`item_icon`] : icône propre à un élément (`.exe`, `.lnk`, `.ico`, dossier personnalisé),
//!   via `IShellItemImageFactory`. Lit le fichier : à éviter pour les fichiers cloud non
//!   présents localement.
//!
//! Les deux exigent un thread COM et ne doivent pas tourner sur le thread d'interface.

use std::path::Path;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHFILEINFOW, SHGFI_SYSICONINDEX,
    SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList, SHIL_EXTRALARGE, SHIL_LARGE, SHIL_SMALL,
    SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use crate::path::shell_path;

/// Image RGBA 8 bits, alpha non prémultiplié, lignes de haut en bas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Lit un HBITMAP en BGRA 32 bits de haut en bas.
fn bitmap_bgra(hbm: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut bm = BITMAP::default();
    // SAFETY: `bm` est une structure BITMAP locale de taille transmise ; le DC mémoire est
    // détruit en fin de bloc ; le tampon fait exactement w × h × 4 octets, ce qu'annonce bmi.
    unsafe {
        if GetObjectW(
            HGDIOBJ(hbm.0),
            size_of::<BITMAP>() as i32,
            Some((&mut bm as *mut BITMAP).cast()),
        ) == 0
        {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 || w > 1024 || h > 1024 {
            return None;
        }
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            hbm,
            0,
            h as u32,
            Some(buf.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        (lines == h).then_some((w as u32, h as u32, buf))
    }
}

fn bgra_to_rgba(w: u32, h: u32, mut px: Vec<u8>, premultiplied: bool) -> IconImage {
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        let a = p[3] as u32;
        if premultiplied && a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    IconImage {
        width: w,
        height: h,
        rgba: px,
    }
}

/// Convertit puis détruit un HICON.
fn hicon_to_image(icon: HICON) -> Option<IconImage> {
    let mut info = ICONINFO::default();
    // SAFETY: HICON valide (possédé par l'appelant, détruit ici) ; les bitmaps renvoyés par
    // GetIconInfo nous appartiennent et sont supprimés après lecture.
    unsafe {
        let ok = GetIconInfo(icon, &mut info).is_ok();
        let _ = DestroyIcon(icon);
        if !ok {
            return None;
        }
        let color = bitmap_bgra(info.hbmColor);
        let mask = bitmap_bgra(info.hbmMask);
        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        let (w, h, mut px) = color?;
        // Icône sans canal alpha : la transparence vient du masque (blanc = transparent).
        if px.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            let (mw, mh, m) = mask?;
            if mw != w || mh < h {
                return None;
            }
            for (p, mpx) in px.as_chunks_mut::<4>().0.iter_mut().zip(m.as_chunks::<4>().0) {
                p[3] = if mpx[0] == 0 { 255 } else { 0 };
            }
        }
        Some(bgra_to_rgba(w, h, px, false))
    }
}

/// Icône du type de `file_name` (seule l'extension compte) d'environ `size` pixels.
pub fn type_icon(file_name: &[u16], is_dir: bool, size: u32) -> Option<IconImage> {
    let name: Vec<u16> = file_name.iter().copied().chain(std::iter::once(0)).collect();
    let attrs = if is_dir {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let mut info = SHFILEINFOW::default();
    let list_id = if size <= 20 {
        SHIL_SMALL
    } else if size <= 36 {
        SHIL_LARGE
    } else {
        SHIL_EXTRALARGE
    };
    // SAFETY: `name` terminé par un nul ; `info` locale de taille transmise ; l'image list
    // système est un objet COM possédé ; l'HICON renvoyé est détruit par `hicon_to_image`.
    unsafe {
        let r = SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            attrs,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES,
        );
        if r == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(list_id as i32).ok()?;
        let icon = list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()?;
        hicon_to_image(icon)
    }
}

/// Icône propre à l'élément `path` (lit le fichier pour `.exe`, `.ico`…).
pub fn item_icon(path: &Path, size: u32) -> Option<IconImage> {
    let wide = shell_path(path);
    // SAFETY: chaîne terminée par un nul ; objets COM possédés ; le HBITMAP renvoyé nous
    // appartient et est supprimé après lecture.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
        let hbm = factory
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                SIIGBF_ICONONLY,
            )
            .ok()?;
        let px = bitmap_bgra(hbm);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        let (w, h, px) = px?;
        // Les bitmaps de IShellItemImageFactory sont en alpha prémultiplié.
        Some(bgra_to_rgba(w, h, px, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiply_restores_color() {
        // Pixel BGRA prémultiplié : rouge pur à 50 % d'opacité.
        let img = bgra_to_rgba(1, 1, vec![0, 0, 128, 128], true);
        assert_eq!(img.rgba, vec![255, 0, 0, 128]);
        let straight = bgra_to_rgba(1, 1, vec![10, 20, 30, 40], false);
        assert_eq!(straight.rgba, vec![30, 20, 10, 40]);
    }
}
