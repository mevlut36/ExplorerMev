//! Identifiants d'éléments du Shell (PIDL) et objets d'interface d'un groupe d'éléments.

use std::path::Path;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFindLastID, ILFree, IShellFolder, SHBindToParent, SHParseDisplayName};
use windows::core::{Interface, PCWSTR};

use crate::path::shell_path;

/// PIDL absolu possédé, libéré par `ILFree`.
pub(crate) struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    /// Analyse un chemin du système de fichiers (forme affichable, sans `\\?\` que le Shell
    /// n'accepte pas partout).
    pub(crate) fn parse(path: &Path) -> windows::core::Result<Self> {
        let wide = shell_path(path);
        let mut p: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `wide` est terminé par un nul ; `p` reçoit un PIDL alloué par le Shell, dont
        // nous devenons propriétaires (libéré dans Drop).
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut p, 0, None)? };
        Ok(Self(p))
    }

    pub(crate) fn as_ptr(&self) -> *const ITEMIDLIST {
        self.0
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: PIDL alloué par SHParseDisplayName, possédé exclusivement, libéré une fois.
            unsafe { ILFree(Some(self.0)) };
        }
    }
}

/// Obtient l'objet d'interface `T` (IContextMenu, IDataObject…) pour des éléments qui
/// partagent le même dossier parent, comme le fait l'Explorateur.
pub(crate) fn ui_object_of<T: Interface>(owner: HWND, paths: &[&Path]) -> windows::core::Result<T> {
    let pidls = paths
        .iter()
        .map(|p| Pidl::parse(p))
        .collect::<windows::core::Result<Vec<_>>>()?;
    let Some(first) = pidls.first() else {
        return Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_INVALIDARG,
        ));
    };
    // SAFETY: `first` est un PIDL absolu valide pendant l'appel ; le parent renvoyé est un
    // objet COM possédé.
    let parent: IShellFolder = unsafe { SHBindToParent(first.as_ptr(), None)? };
    // SAFETY: ILFindLastID renvoie un pointeur à l'intérieur de chaque PIDL possédé par
    // `pidls`, qui reste vivant jusqu'à la fin de la fonction (après GetUIObjectOf).
    let children: Vec<*const ITEMIDLIST> = pidls
        .iter()
        .map(|p| unsafe { ILFindLastID(p.as_ptr()) } as *const ITEMIDLIST)
        .collect();
    // SAFETY: tableau de PIDL relatifs valides, tous enfants de `parent` (même dossier).
    unsafe { parent.GetUIObjectOf::<T>(owner, &children, None) }
}
