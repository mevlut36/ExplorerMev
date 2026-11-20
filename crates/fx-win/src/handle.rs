//! Propriété RAII des handles Win32.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::FindClose;

/// Handle de noyau fermé par `CloseHandle` à la destruction.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

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
