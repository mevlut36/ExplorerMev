//! Presse-papiers de fichiers compatible avec l'Explorateur (objet de données du Shell,
//! format « Preferred DropEffect » pour couper/copier).

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::{IsClipboardFormatAvailable, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, OleGetClipboard, OleSetClipboard,
    ReleaseStgMedium,
};
use windows::core::w;

use crate::fileop::{OpOptions, OpOutcome, Source, copy_or_move};
use crate::pidl::ui_object_of;

fn preferred_effect_format() -> FORMATETC {
    // SAFETY: nom de format constant terminé par un nul.
    let cf = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    FORMATETC {
        cfFormat: cf as u16,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn set_preferred_effect(data: &IDataObject, effect: DROPEFFECT) -> windows::core::Result<()> {
    let fmt = preferred_effect_format();
    // SAFETY: allocation d'un HGLOBAL de 4 octets, écrit sous verrou ; avec fRelease = TRUE,
    // l'objet de données devient propriétaire du support (pas de double libération).
    unsafe {
        let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>())?;
        let p = GlobalLock(mem).cast::<u32>();
        if p.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        p.write_unaligned(effect.0);
        let _ = GlobalUnlock(mem);
        let medium = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: mem },
            pUnkForRelease: ManuallyDrop::new(None),
        };
        data.SetData(&fmt, &medium, true)
    }
}

/// Effet demandé par la source (couper = déplacer), copie par défaut.
fn preferred_effect(data: &IDataObject) -> DROPEFFECT {
    let fmt = preferred_effect_format();
    // SAFETY: support obtenu de GetData, lu sous verrou puis libéré par ReleaseStgMedium.
    unsafe {
        let Ok(mut medium) = data.GetData(&fmt) else {
            return DROPEFFECT_COPY;
        };
        let mut effect = DROPEFFECT_COPY;
        if medium.tymed == TYMED_HGLOBAL.0 as u32 {
            let p = GlobalLock(medium.u.hGlobal).cast::<u32>();
            if !p.is_null() {
                effect = DROPEFFECT(p.read_unaligned());
                let _ = GlobalUnlock(medium.u.hGlobal);
            }
        }
        ReleaseStgMedium(&mut medium);
        effect
    }
}

/// Place des fichiers (d'un même dossier) dans le presse-papiers. `cut` : collage = déplacement.
///
/// À appeler sur un thread OLE qui reste vivant et pompe des messages (l'hôte Shell) : il
/// devient propriétaire du presse-papiers.
pub fn set_files(paths: &[PathBuf], cut: bool) -> windows::core::Result<()> {
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let data: IDataObject = ui_object_of(HWND::default(), &refs)?;
    set_preferred_effect(&data, if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY })?;
    // SAFETY: objet COM valide ; OLE en garde une référence.
    unsafe { OleSetClipboard(&data) }
}

/// Le presse-papiers contient-il des fichiers ? (sans OLE, utilisable partout)
pub fn has_files() -> bool {
    // SAFETY: appel sans pointeur.
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_ok()
}

/// Colle le contenu du presse-papiers dans `dest` via `IFileOperation` (éléments virtuels
/// compris). Un collage après « Couper » déplace puis vide le presse-papiers. Renvoie `None` si
/// le presse-papiers est vide. À appeler sur un thread OLE (STA).
pub fn paste(dest: &Path, opts: OpOptions) -> windows::core::Result<Option<OpOutcome>> {
    // SAFETY: thread OLE (exigence documentée) ; objet COM possédé.
    let data = match unsafe { OleGetClipboard() } {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    let is_move = preferred_effect(&data) == DROPEFFECT_MOVE;
    let out = copy_or_move(Source::Data(&data), dest, is_move, opts)?;
    if is_move && !out.aborted {
        // SAFETY: vide le presse-papiers, comme l'Explorateur après un couper-coller.
        let _ = unsafe { OleSetClipboard(None) };
    }
    Ok(Some(out))
}

/// Place du texte (chemins copiés) dans le presse-papiers. `owner` : fenêtre propriétaire.
pub fn set_text(owner: isize, text: &str) -> windows::core::Result<()> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: le presse-papiers est ouvert puis refermé dans ce bloc ; le HGLOBAL est rempli
    // sous verrou avec exactement `wide.len()` unités, puis confié au système par
    // SetClipboardData (qui en devient propriétaire en cas de succès).
    unsafe {
        OpenClipboard(Some(HWND(owner as *mut _)))?;
        let result = (|| {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let p = GlobalLock(mem).cast::<u16>();
            if p.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0)))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
