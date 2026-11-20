//! Contrôle d'accès « l'utilisateur peut-il lister ce dossier ? » pour l'assistant
//! d'indexation élevé.
//!
//! L'assistant voit toute la MFT, y compris les noms de dossiers que le compte de l'utilisateur
//! ne peut pas ouvrir (profils d'autres comptes, dossiers protégés). Il ne transmet un nom que
//! si le dossier qui le contient est listable avec le jeton **non élevé** de l'utilisateur :
//! l'index ne contient ainsi rien de plus que ce qu'un parcours des dossiers sans droits
//! verrait. Le contrôle est fait par `AccessCheck` (droit `FILE_LIST_DIRECTORY`), une fois par
//! descripteur de sécurité distinct.

use std::io;
use std::path::Path;

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, GetSecurityInfo, SE_FILE_OBJECT};
use windows::Win32::Security::{
    AccessCheck, DACL_SECURITY_INFORMATION, DuplicateToken, GENERIC_MAPPING, GROUP_SECURITY_INFORMATION,
    GetTokenInformation, MapGenericMask, OWNER_SECURITY_INFORMATION, PRIVILEGE_SET, PSECURITY_DESCRIPTOR,
    SecurityImpersonation, TOKEN_DUPLICATE, TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN, TOKEN_QUERY,
    TokenElevationType, TokenElevationTypeFull, TokenLinkedToken,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::HSTRING;

use crate::handle::OwnedHandle;

/// Descripteur de sécurité alloué par le système (libéré par `LocalFree`).
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: descripteur alloué par GetSecurityInfo / GetNamedSecurityInfoW, libéré une fois.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

const PARTS: windows::Win32::Security::OBJECT_SECURITY_INFORMATION =
    windows::Win32::Security::OBJECT_SECURITY_INFORMATION(
        OWNER_SECURITY_INFORMATION.0 | GROUP_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0,
    );

impl SecurityDescriptor {
    /// Descripteur de l'objet ouvert `h` (droit `READ_CONTROL` requis).
    pub fn of_handle(h: HANDLE) -> io::Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: handle valide ; `sd` reçoit un descripteur alloué que `Self` libère.
        let r = unsafe { GetSecurityInfo(h, SE_FILE_OBJECT, PARTS, None, None, None, None, Some(&mut sd)) };
        r.ok()?;
        Ok(Self(sd))
    }

    /// Descripteur du fichier ou dossier `path`.
    pub fn of_path(path: &Path) -> io::Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let name = HSTRING::from(path);
        // SAFETY: chaîne vivante pendant l'appel ; descripteur alloué libéré par `Self`.
        let r =
            unsafe { GetNamedSecurityInfoW(&name, SE_FILE_OBJECT, PARTS, None, None, None, None, &mut sd) };
        r.ok()?;
        Ok(Self(sd))
    }
}

/// Jeton d'identité (emprunt) servant aux contrôles.
pub struct ListChecker {
    token: OwnedHandle,
}

// SAFETY: un jeton noyau peut être utilisé depuis n'importe quel thread.
unsafe impl Send for ListChecker {}

fn process_token() -> io::Result<OwnedHandle> {
    let mut t = HANDLE::default();
    // SAFETY: pseudo-handle du processus courant ; handle reçu possédé.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE, &mut t) }?;
    Ok(OwnedHandle(t))
}

fn impersonation(token: HANDLE) -> io::Result<OwnedHandle> {
    let mut dup = HANDLE::default();
    // SAFETY: jeton valide ; copie possédée.
    unsafe { DuplicateToken(token, SecurityImpersonation, &mut dup) }?;
    Ok(OwnedHandle(dup))
}

impl ListChecker {
    /// Jeton du processus courant (tests ; assistant lancé sans élévation).
    pub fn current() -> io::Result<Self> {
        let t = process_token()?;
        Ok(Self {
            token: impersonation(t.0)?,
        })
    }

    /// Jeton **non élevé** de l'utilisateur : pour un processus élevé (jeton complet d'un
    /// administrateur sous UAC), le jeton lié restreint ; sinon le jeton du processus.
    pub fn limited_user() -> io::Result<Self> {
        let t = process_token()?;
        let mut kind = TOKEN_ELEVATION_TYPE::default();
        let mut len = 0u32;
        // SAFETY: tampon local de taille transmise.
        unsafe {
            GetTokenInformation(
                t.0,
                TokenElevationType,
                Some((&mut kind as *mut TOKEN_ELEVATION_TYPE).cast()),
                size_of::<TOKEN_ELEVATION_TYPE>() as u32,
                &mut len,
            )
        }?;
        if kind != TokenElevationTypeFull {
            return Self::current();
        }
        let mut linked = TOKEN_LINKED_TOKEN::default();
        // SAFETY: tampon local de taille transmise ; le jeton lié reçu est possédé.
        unsafe {
            GetTokenInformation(
                t.0,
                TokenLinkedToken,
                Some((&mut linked as *mut TOKEN_LINKED_TOKEN).cast()),
                size_of::<TOKEN_LINKED_TOKEN>() as u32,
                &mut len,
            )
        }?;
        let linked = OwnedHandle(linked.LinkedToken);
        Ok(Self {
            token: impersonation(linked.0)?,
        })
    }

    /// Le dossier décrit par `sd` est-il listable ? Toute erreur vaut refus.
    pub fn can_list(&self, sd: &SecurityDescriptor) -> bool {
        let mapping = GENERIC_MAPPING {
            GenericRead: FILE_GENERIC_READ.0,
            GenericWrite: FILE_GENERIC_WRITE.0,
            GenericExecute: FILE_GENERIC_EXECUTE.0,
            GenericAll: FILE_ALL_ACCESS.0,
        };
        let mut desired = FILE_LIST_DIRECTORY.0;
        // SAFETY: masque local.
        unsafe { MapGenericMask(&mut desired, &mapping) };
        let mut privileges = [0u8; 256];
        let mut plen = privileges.len() as u32;
        let mut granted = 0u32;
        let mut status = windows::core::BOOL(0);
        // SAFETY: descripteur valide (possédé par `sd`), jeton d'emprunt valide, tampons
        // locaux de tailles transmises.
        let r = unsafe {
            AccessCheck(
                sd.0,
                self.token.0,
                desired,
                &mapping,
                Some(privileges.as_mut_ptr() as *mut PRIVILEGE_SET),
                &mut plen,
                &mut granted,
                &mut status,
            )
        };
        r.is_ok() && status.as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listable_and_denied_directories() -> Result<(), Box<dyn std::error::Error>> {
        let checker = ListChecker::current()?;
        let ok = tempfile::tempdir()?;
        assert!(checker.can_list(&SecurityDescriptor::of_path(ok.path())?));
        // Dossier dont la DACL refuse la liste à l'utilisateur courant.
        let denied = tempfile::tempdir()?;
        let sid = crate::pipe::current_user_sid()?;
        let path = denied.path().display().to_string();
        let st = std::process::Command::new("icacls")
            .args([&path, "/deny", &format!("*{sid}:(RD)")])
            .output()?
            .status;
        assert!(st.success());
        let sd = SecurityDescriptor::of_path(denied.path())?;
        let can = checker.can_list(&sd);
        // Restaure les droits pour que le dossier temporaire puisse être supprimé.
        let _ = std::process::Command::new("icacls")
            .args([&path, "/remove:d", &format!("*{sid}")])
            .output();
        assert!(!can);
        // Hors élévation, le jeton « utilisateur restreint » est celui du processus.
        assert!(ListChecker::limited_user()?.can_list(&SecurityDescriptor::of_path(ok.path())?));
        Ok(())
    }
}
