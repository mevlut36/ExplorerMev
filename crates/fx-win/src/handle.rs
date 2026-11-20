//! Propriété RAII des handles Win32.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::FindClose;

/// Handle de noyau fermé par `CloseHandle` à la destruction.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

// SAFETY: un handle de noyau n'est pas lié au thread qui l'a créé ; OwnedHandle en a la
// propriété exclusive, donc le transférer à un autre thread ne crée pas d'accès concurrent.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: le handle a été obtenu par CreateFileW, n'est possédé que par cette
            // structure et n'est fermé qu'ici, une seule fois.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Handle de recherche fermé par `FindClose` à la destruction.
pub(crate) struct FindHandle(pub(crate) HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: handle renvoyé par FindFirstFileExW, possédé exclusivement, fermé une fois.
            let _ = unsafe { FindClose(self.0) };
        }
    }
}
