//! Prévisualiseurs du Shell (`IPreviewHandler`) : PDF, documents Office, courriels…
//!
//! Le prévisualiseur s'affiche dans une fenêtre enfant de la fenêtre principale, créée et
//! pilotée par un thread STA dédié (pompe de messages) : un prévisualiseur lent ne bloque pas
//! le thread d'interface. [`NativePreview`] n'est pas `Send` et doit vivre sur ce thread.

use std::ffi::c_void;
use std::path::Path;

use windows::Win32::Foundation::{E_NOINTERFACE, HWND, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CLSIDFromString, CoCreateInstance, STGM_READ,
    STGM_SHARE_DENY_NONE,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_DEFAULTTOSTAR, ASSOCSTR_SHELLEXTENSION, AssocQueryStringW, IInitializeWithItem,
    IPreviewHandler, IShellItem, SHCreateItemFromParsingName, SHCreateStreamOnFileEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, MoveWindow, WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_VISIBLE,
};
use windows::core::{GUID, Interface, PCWSTR, PWSTR, w};

use crate::path::shell_path;

/// Interface « IPreviewHandler » telle qu'enregistrée sous `shellex` dans le registre.
const PREVIEW_HANDLER_IID: &str = "{8895b1c6-b41f-4c1c-a562-0d564250836f}";

/// CLSID du prévisualiseur enregistré pour l'extension `ext` (sans point), s'il y en a un.
pub fn handler_for(ext: &str) -> Option<GUID> {
    let ext: Vec<u16> = format!(".{ext}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let iid: Vec<u16> = PREVIEW_HANDLER_IID
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = [0u16; 64];
    let mut len = buf.len() as u32;
    // SAFETY: chaînes terminées par un nul ; tampon de sortie local de capacité annoncée.
    unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_DEFAULTTOSTAR,
            ASSOCSTR_SHELLEXTENSION,
            PCWSTR(ext.as_ptr()),
            PCWSTR(iid.as_ptr()),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
        .ok()
        .ok()?;
        CLSIDFromString(PCWSTR(buf.as_ptr())).ok()
    }
}

/// Rectangle en pixels physiques, relatif à la zone client du parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Prévisualiseur affiché. Libéré (`Unload`, fenêtre détruite) à la destruction.
pub struct NativePreview {
    handler: IPreviewHandler,
    child: HWND,
}

impl NativePreview {
    /// Crée la fenêtre enfant dans `parent` et y affiche l'aperçu de `path` par le
    /// prévisualiseur `clsid`. À appeler sur un thread STA qui pompe des messages.
    pub fn open(parent: isize, rect: Rect, path: &Path, clsid: GUID) -> windows::core::Result<Self> {
        let file = shell_path(path);
        // SAFETY: fenêtre enfant d'un HWND valide (fenêtre principale) ; objets COM possédés ;
        // la chaîne du chemin vit jusqu'à la fin des appels d'initialisation.
        unsafe {
            let child = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("Static"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                Some(HWND(parent as *mut c_void)),
                None,
                None,
                None,
            )?;
            let result = (|| {
                let handler: IPreviewHandler =
                    CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER)?;
                if let Ok(init) = handler.cast::<IInitializeWithFile>() {
                    init.Initialize(PCWSTR(file.as_ptr()), STGM_READ.0)?;
                } else if let Ok(init) = handler.cast::<IInitializeWithItem>() {
                    let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(file.as_ptr()), None)?;
                    init.Initialize(&item, STGM_READ.0)?;
                } else if let Ok(init) = handler.cast::<IInitializeWithStream>() {
                    let stream = SHCreateStreamOnFileEx(
                        PCWSTR(file.as_ptr()),
                        STGM_READ.0 | STGM_SHARE_DENY_NONE.0,
                        0,
                        false,
                        None,
                    )?;
                    init.Initialize(&stream, STGM_READ.0)?;
                } else {
                    return Err(windows::core::Error::from_hresult(E_NOINTERFACE));
                }
                let rc = RECT {
                    left: 0,
                    top: 0,
                    right: rect.width,
                    bottom: rect.height,
                };
                handler.SetWindow(child, &rc)?;
                handler.DoPreview()?;
                Ok(handler)
            })();
            match result {
                Ok(handler) => Ok(Self { handler, child }),
                Err(e) => {
                    let _ = DestroyWindow(child);
                    Err(e)
                }
            }
        }
    }

    /// Suit la mise en page de la fenêtre principale.
    pub fn resize(&self, rect: Rect) {
        // SAFETY: fenêtre enfant et prévisualiseur possédés par `self`, sur leur thread.
        unsafe {
            let _ = MoveWindow(
                self.child,
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                true,
            );
            let rc = RECT {
                left: 0,
                top: 0,
                right: rect.width,
                bottom: rect.height,
            };
            let _ = self.handler.SetRect(&rc);
        }
    }
}

impl Drop for NativePreview {
    fn drop(&mut self) {
        // SAFETY: libère le prévisualiseur puis détruit la fenêtre enfant créée par `open`,
        // sur le même thread.
        unsafe {
            let _ = self.handler.Unload();
            let _ = DestroyWindow(self.child);
        }
    }
}
