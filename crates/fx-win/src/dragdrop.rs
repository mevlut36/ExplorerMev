//! Glisser-déposer OLE, interopérable avec l'Explorateur et les autres applications.
//!
//! - Cible : [`register`] enregistre un `IDropTarget` sur la fenêtre (thread d'interface, qui
//!   doit être initialisé par [`crate::com::OleApartment`]). Les images de glissement du Shell
//!   sont affichées via `IDropTargetHelper`.
//! - Source : [`drag_files`] lance `SHDoDragDrop` avec l'objet de données du Shell. La boucle
//!   modale d'OLE exige le thread qui possède la souris, c'est-à-dire le thread d'interface.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropTarget,
    IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT, MODIFIERKEYS_FLAGS};

/// `MK_ALT` (oleidl.h) : touche Alt pendant un glisser-déposer, absente du crate `windows`.
const MK_ALT: MODIFIERKEYS_FLAGS = MODIFIERKEYS_FLAGS(0x20);
use windows::Win32::UI::Shell::{
    CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper, SHDoDragDrop,
};
use windows::core::{Ref, implement};

use crate::pidl::ui_object_of;

/// Effets possibles, en bits `DROPEFFECT_*`.
pub const EFFECT_NONE: u32 = DROPEFFECT_NONE.0;
pub const EFFECT_COPY: u32 = DROPEFFECT_COPY.0;
pub const EFFECT_MOVE: u32 = DROPEFFECT_MOVE.0;
pub const EFFECT_LINK: u32 = DROPEFFECT_LINK.0;

/// Ce que la fenêtre décide pendant un glisser-déposer entrant. Coordonnées en pixels
/// physiques, relatives à la zone client.
pub trait DropHandler {
    /// Survol : renvoie la destination proposée (dossier survolé ou courant), ou `None`.
    fn hover(&self, x: i32, y: i32) -> Option<PathBuf>;
    fn leave(&self);
    /// Dépôt de `paths` dans `dest` avec l'effet choisi.
    fn drop_files(&self, dest: PathBuf, paths: Vec<PathBuf>, effect: u32);
}

/// Effet par défaut, comme l'Explorateur : Ctrl copie, Maj déplace, Ctrl+Maj ou Alt crée un
/// lien ; sinon déplacement sur le même volume, copie entre volumes.
pub fn default_effect(keys: u32, allowed: u32, same_volume: bool) -> u32 {
    let ctrl = keys & MK_CONTROL.0 != 0;
    let shift = keys & MK_SHIFT.0 != 0;
    let alt = keys & MK_ALT.0 != 0;
    let wanted = if (ctrl && shift) || alt {
        EFFECT_LINK
    } else if ctrl {
        EFFECT_COPY
    } else if shift || same_volume {
        EFFECT_MOVE
    } else {
        EFFECT_COPY
    };
    if allowed & wanted != 0 {
        wanted
    } else if allowed & EFFECT_COPY != 0 {
        EFFECT_COPY
    } else if allowed & EFFECT_MOVE != 0 {
        EFFECT_MOVE
    } else {
        EFFECT_NONE
    }
}

fn volume_of(p: &Path) -> Option<std::ffi::OsString> {
    p.components().next().map(|c| c.as_os_str().to_ascii_uppercase())
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// Chemins `CF_HDROP` d'un objet de données (vide s'il n'en contient pas).
pub fn files_of(data: &IDataObject) -> Vec<PathBuf> {
    let fmt = hdrop_format();
    // SAFETY: support obtenu de GetData et libéré par ReleaseStgMedium ; DragQueryFileW lit le
    // HGLOBAL de type DROPFILES avec des tampons locaux de longueur transmise.
    unsafe {
        let Ok(mut medium) = data.GetData(&fmt) else {
            return Vec::new();
        };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(OsString::from_wide(&buf[..got.min(len)])));
        }
        ReleaseStgMedium(&mut medium);
        out
    }
}

#[implement(IDropTarget)]
struct DropTarget {
    hwnd: HWND,
    handler: Box<dyn DropHandler>,
    helper: Option<IDropTargetHelper>,
    paths: RefCell<Vec<PathBuf>>,
    dest: RefCell<Option<PathBuf>>,
    effect: Cell<u32>,
}

impl DropTarget {
    fn client(&self, pt: &POINTL) -> (i32, i32) {
        let mut p = POINT { x: pt.x, y: pt.y };
        // SAFETY: HWND de notre fenêtre, point local.
        let _ = unsafe { ScreenToClient(self.hwnd, &mut p) };
        (p.x, p.y)
    }

    fn update(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, allowed: u32) -> u32 {
        let paths = self.paths.borrow();
        if paths.is_empty() {
            return EFFECT_NONE;
        }
        // Création de raccourcis (lien) non prise en charge pour l'instant : copie ou déplacement.
        let allowed = allowed & (EFFECT_COPY | EFFECT_MOVE);
        let (x, y) = self.client(pt);
        let dest = self.handler.hover(x, y);
        let effect = match &dest {
            // Interdit de déposer un dossier dans lui-même.
            Some(d) if paths.iter().any(|p| d.starts_with(p)) => EFFECT_NONE,
            Some(d) => {
                let same = volume_of(d).is_some() && paths.first().and_then(|p| volume_of(p)) == volume_of(d);
                default_effect(keys.0, allowed, same)
            }
            None => EFFECT_NONE,
        };
        *self.dest.borrow_mut() = dest;
        self.effect.set(effect);
        effect
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        *self.paths.borrow_mut() = data.as_ref().map(files_of).unwrap_or_default();
        // SAFETY: `effect` est un pointeur d'entrée/sortie fourni par OLE, valide pendant l'appel.
        unsafe {
            let allowed = (*effect).0;
            let e = self.update(keys, pt, allowed);
            *effect = DROPEFFECT(e);
            if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
                let _ = h.DragEnter(self.hwnd, d, &POINT { x: pt.x, y: pt.y }, DROPEFFECT(e));
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // SAFETY: voir DragEnter.
        unsafe {
            let e = self.update(keys, pt, (*effect).0);
            *effect = DROPEFFECT(e);
            if let Some(h) = &self.helper {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, DROPEFFECT(e));
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if let Some(h) = &self.helper {
            // SAFETY: interface COM valide.
            let _ = unsafe { h.DragLeave() };
        }
        self.paths.borrow_mut().clear();
        self.handler.leave();
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<'_, IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // SAFETY: voir DragEnter.
        unsafe {
            let e = self.update(keys, pt, (*effect).0);
            *effect = DROPEFFECT(e);
            if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, DROPEFFECT(e));
            }
        }
        let paths = std::mem::take(&mut *self.paths.borrow_mut());
        let dest = self.dest.borrow_mut().take();
        self.handler.leave();
        if let Some(dest) = dest
            && self.effect.get() != EFFECT_NONE
        {
            self.handler.drop_files(dest, paths, self.effect.get());
        }
        Ok(())
    }
}

/// Enregistrement d'une cible de dépôt, révoqué à la destruction.
pub struct DropRegistration {
    hwnd: HWND,
}

impl Drop for DropRegistration {
    fn drop(&mut self) {
        // SAFETY: révoque l'enregistrement fait par `register` sur la même fenêtre.
        let _ = unsafe { RevokeDragDrop(self.hwnd) };
    }
}

/// Fait de la fenêtre `hwnd` une cible de dépôt de fichiers. À appeler sur son thread, initialisé
/// par OLE.
pub fn register(hwnd: isize, handler: Box<dyn DropHandler>) -> windows::core::Result<DropRegistration> {
    let hwnd = HWND(hwnd as *mut _);
    // SAFETY: création d'un objet COM in-process ; un échec n'empêche pas le dépôt (pas
    // d'image de glissement).
    let helper: Option<IDropTargetHelper> =
        unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.ok();
    let target: IDropTarget = DropTarget {
        hwnd,
        handler,
        helper,
        paths: RefCell::default(),
        dest: RefCell::default(),
        effect: Cell::new(EFFECT_NONE),
    }
    .into();
    // SAFETY: HWND de notre fenêtre, thread OLE (exigence documentée) ; OLE garde une référence
    // à la cible jusqu'à RevokeDragDrop.
    unsafe { RegisterDragDrop(hwnd, &target)? };
    Ok(DropRegistration { hwnd })
}

/// Lance le glissement de fichiers d'un même dossier. Bloque (boucle modale d'OLE) jusqu'au
/// dépôt ; renvoie l'effet réalisé.
pub fn drag_files(hwnd: isize, paths: &[PathBuf]) -> windows::core::Result<u32> {
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let hwnd = HWND(hwnd as *mut _);
    let data: IDataObject = ui_object_of(hwnd, &refs)?;
    // SAFETY: thread OLE propriétaire de la souris ; le Shell fournit la source et l'image.
    let effect = unsafe {
        SHDoDragDrop(
            Some(hwnd),
            &data,
            None,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
        )?
    };
    Ok(effect.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_follow_explorer_rules() {
        let all = EFFECT_COPY | EFFECT_MOVE | EFFECT_LINK;
        assert_eq!(default_effect(0, all, true), EFFECT_MOVE);
        assert_eq!(default_effect(0, all, false), EFFECT_COPY);
        assert_eq!(default_effect(MK_CONTROL.0, all, true), EFFECT_COPY);
        assert_eq!(default_effect(MK_SHIFT.0, all, false), EFFECT_MOVE);
        assert_eq!(default_effect(MK_CONTROL.0 | MK_SHIFT.0, all, true), EFFECT_LINK);
        assert_eq!(default_effect(0, EFFECT_COPY, true), EFFECT_COPY);
        assert_eq!(
            default_effect(MK_ALT.0, EFFECT_COPY | EFFECT_MOVE, true),
            EFFECT_COPY
        );
    }
}
