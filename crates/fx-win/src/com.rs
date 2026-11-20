//! Initialisation COM par thread.

use std::marker::PhantomData;

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};

/// Appartement COM STA du thread courant, libéré à la destruction.
///
/// N'est ni `Send` ni `Sync` : `CoUninitialize` doit être appelé sur le thread qui a initialisé.
pub struct ComApartment {
    _not_send: PhantomData<*const ()>,
}

impl ComApartment {
    /// Initialise le thread courant en STA (exigé par la plupart des API Shell).
    pub fn sta() -> windows::core::Result<Self> {
        // SAFETY: aucun pointeur passé ; l'appel est équilibré par CoUninitialize dans Drop,
        // y compris pour S_FALSE (déjà initialisé dans le même mode), comme l'exige la doc.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.ok()?;
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: équilibre le CoInitializeEx réussi de `sta()`, sur le même thread (type !Send).
        unsafe { CoUninitialize() };
    }
}

/// Initialisation OLE (STA + presse-papiers + glisser-déposer) du thread courant, libérée à la
/// destruction. Le thread d'interface l'initialise avant de créer sa fenêtre.
pub struct OleApartment {
    _not_send: PhantomData<*const ()>,
}

impl OleApartment {
    pub fn init() -> windows::core::Result<Self> {
        // SAFETY: équilibré par OleUninitialize dans Drop, sur le même thread (type !Send).
        unsafe { OleInitialize(None)? };
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for OleApartment {
    fn drop(&mut self) {
        // SAFETY: équilibre l'OleInitialize réussi de `init`.
        unsafe { OleUninitialize() };
    }
}
