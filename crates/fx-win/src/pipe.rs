//! Tube nommé entre l'interface et l'assistant d'indexation élevé.
//!
//! - Le serveur (l'interface, non élevée) crée le tube **en lecture seule** pour lui
//!   (`PIPE_ACCESS_INBOUND`) : l'assistant ne peut qu'y écrire, il ne reçoit aucune commande.
//! - `FILE_FLAG_FIRST_PIPE_INSTANCE` : échoue si un autre processus a déjà créé ce nom ;
//!   une seule instance ; `PIPE_REJECT_REMOTE_CLIENTS`.
//! - DACL réduite à l'utilisateur courant (`D:P(A;;GA;;;<SID>)`).
//! - Le serveur vérifie que le client connecté est le processus qu'il a lancé
//!   (`GetNamedPipeClientProcessId`).

use std::io;
use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessId, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::handle::OwnedHandle;

/// Préfixe des noms de tube de l'assistant ; suivi de 32 chiffres hexadécimaux.
pub const PIPE_PREFIX: &str = r"\\.\pipe\ExplorerMev-index-";

/// Nom de tube valide pour l'assistant (vérifié par l'assistant avant toute action).
pub fn is_valid_pipe_name(name: &str) -> bool {
    name.strip_prefix(PIPE_PREFIX).is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

/// Nom de tube imprévisible (128 bits du générateur aléatoire du système, via `RandomState`).
pub fn random_pipe_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{PIPE_PREFIX}{:016x}{:016x}", part(), part())
}

/// SID de l'utilisateur courant, sous forme texte (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: pseudo-handle du processus courant ; `token` reçoit un handle que l'on ferme.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
    let token = OwnedHandle(token);
    let mut buf = vec![0u64; 64];
    let mut len = 0u32;
    // SAFETY: tampon local aligné sur 8 octets, de taille transmise.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut len,
        )
    }?;
    // SAFETY: GetTokenInformation(TokenUser) a écrit une TOKEN_USER au début du tampon aligné.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut text = PWSTR::null();
    // SAFETY: SID valide (dans `buf`, vivant) ; la chaîne allouée est libérée par LocalFree.
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut text)?;
        let s = text
            .to_string()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        s
    }
}

/// Tube serveur (côté interface).
pub struct PipeServer {
    pipe: OwnedHandle,
    event: OwnedHandle,
}

// SAFETY: handles noyau utilisables depuis n'importe quel thread ; un seul thread lit.
unsafe impl Send for PipeServer {}

impl PipeServer {
    /// Crée le tube `name`, accessible au seul utilisateur courant.
    pub fn create(name: &str) -> io::Result<Self> {
        if !is_valid_pipe_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nom de tube invalide",
            ));
        }
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{})", current_user_sid()?));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: chaîne SDDL vivante ; le descripteur alloué est libéré par LocalFree.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None)
        }?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        // SAFETY: nom et attributs de sécurité vivants pendant l'appel.
        let pipe = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(name),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                1 << 20,
                0,
                Some(&sa),
            )
        };
        // SAFETY: descripteur alloué par ConvertStringSecurityDescriptor…, plus utilisé.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
        if pipe.is_invalid() {
            return Err(io::Error::last_os_error());
        }
        let pipe = OwnedHandle(pipe);
        // SAFETY: événement sans nom, possédé.
        let event = OwnedHandle(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?);
        Ok(Self { pipe, event })
    }

    /// Attend la connexion du client `expected_pid` (au plus `timeout`, et pas après la fin
    /// du processus `process`). Refuse tout autre client.
    pub fn accept(&self, expected_pid: u32, process: HANDLE, timeout: Duration) -> io::Result<()> {
        let mut ov = OVERLAPPED {
            hEvent: self.event.0,
            ..Default::default()
        };
        // SAFETY: `ov` vit jusqu'à la fin de l'attente (ou de l'annulation) ci-dessous.
        let r = unsafe { ConnectNamedPipe(self.pipe.0, Some(&mut ov)) };
        if let Err(e) = r {
            if e.code() == ERROR_PIPE_CONNECTED.to_hresult() {
                // Déjà connecté entre la création et l'appel.
            } else if e.code() == ERROR_IO_PENDING.to_hresult() {
                // SAFETY: handles valides.
                let w = unsafe {
                    WaitForMultipleObjects(&[self.event.0, process], false, timeout.as_millis() as u32)
                };
                if w != WAIT_OBJECT_0 {
                    // SAFETY: E/S en cours sur notre tube ; on attend sa fin avant de rendre `ov`.
                    unsafe {
                        let _ = CancelIoEx(self.pipe.0, Some(&ov));
                        let mut n = 0;
                        let _ = GetOverlappedResult(self.pipe.0, &ov, &mut n, true);
                    }
                    let msg = if w == WAIT_TIMEOUT {
                        "délai dépassé"
                    } else if w.0 == WAIT_OBJECT_0.0 + 1 {
                        "l'assistant s'est arrêté"
                    } else {
                        "attente impossible"
                    };
                    return Err(io::Error::new(io::ErrorKind::TimedOut, msg));
                }
            } else {
                return Err(e.into());
            }
        }
        let mut pid = 0u32;
        // SAFETY: tube connecté.
        unsafe { GetNamedPipeClientProcessId(self.pipe.0, &mut pid) }?;
        if pid != expected_pid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "client du tube inattendu",
            ));
        }
        Ok(())
    }

    /// Lit au plus `buf.len()` octets ; 0 à la fermeture par le client.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut ov = OVERLAPPED {
            hEvent: self.event.0,
            ..Default::default()
        };
        let mut n = 0u32;
        // SAFETY: `buf` et `ov` vivent jusqu'à la fin de l'E/S (attendue ci-dessous).
        let r = unsafe { ReadFile(self.pipe.0, Some(buf), None, Some(&mut ov)) };
        match r {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_BROKEN_PIPE.to_hresult() => return Ok(0),
            Err(e) => return Err(e.into()),
        }
        // SAFETY: E/S lancée sur notre tube avec `ov` ; attente de sa fin.
        match unsafe { GetOverlappedResult(self.pipe.0, &ov, &mut n, true) } {
            Ok(()) => Ok(n as usize),
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    pub fn read_exact(&self, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let n = self.read(buf)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            buf = &mut buf[n..];
        }
        Ok(())
    }
}

impl io::Read for &PipeServer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        PipeServer::read(self, buf)
    }
}

/// Côté assistant : connexion au tube en écriture seule.
pub struct PipeClient(OwnedHandle);

impl PipeClient {
    pub fn connect(name: &str) -> io::Result<Self> {
        if !is_valid_pipe_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nom de tube invalide",
            ));
        }
        // SAFETY: nom vivant pendant l'appel ; handle possédé.
        let h = unsafe {
            CreateFileW(
                &HSTRING::from(name),
                GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }?;
        Ok(Self(OwnedHandle(h)))
    }
}

impl io::Write for PipeClient {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = 0u32;
        // SAFETY: tampon vivant pendant l'appel synchrone.
        unsafe { WriteFile(self.0.0, Some(buf), Some(&mut n), None) }?;
        Ok(n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Processus lancé (handle possédé).
pub struct Child {
    handle: OwnedHandle,
    pub pid: u32,
}

impl Child {
    pub fn handle(&self) -> HANDLE {
        self.handle.0
    }

    /// Le processus est-il terminé ?
    pub fn exited(&self) -> bool {
        // SAFETY: handle de processus possédé.
        let w = unsafe { WaitForSingleObject(self.handle.0, 0) };
        w == WAIT_OBJECT_0
    }
}

/// Lance `exe args` **avec élévation** (invite du contrôle de compte d'utilisateur).
/// Erreur si l'utilisateur refuse.
pub fn launch_elevated(exe: &Path, args: &str) -> io::Result<Child> {
    launch(exe, args, true)
}

/// Lance `exe args` sans élévation (tests du protocole).
pub fn launch_plain(exe: &Path, args: &str) -> io::Result<Child> {
    launch(exe, args, false)
}

fn launch(exe: &Path, args: &str, elevated: bool) -> io::Result<Child> {
    let file = HSTRING::from(exe);
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: if elevated { w!("runas") } else { w!("open") },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: structure locale complète ; chaînes vivantes pendant l'appel ; le handle de
    // processus renvoyé est possédé par `Child`.
    unsafe { ShellExecuteExW(&mut info) }?;
    if info.hProcess.is_invalid() {
        return Err(io::Error::other("processus non créé"));
    }
    let handle = OwnedHandle(info.hProcess);
    // SAFETY: handle de processus valide.
    let pid = unsafe { GetProcessId(handle.0) };
    Ok(Child { handle, pid })
}

/// Le processus `pid` existe-t-il encore ? (assistant : s'arrêter avec l'interface)
pub fn process_alive(pid: u32) -> bool {
    // SAFETY: handle possédé et fermé ; droit de synchronisation seulement.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        let _ = CloseHandle(h);
        alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_are_strictly_validated() {
        let n = random_pipe_name();
        assert!(is_valid_pipe_name(&n), "{n}");
        assert_ne!(n, random_pipe_name());
        assert!(!is_valid_pipe_name(r"\\.\pipe\ExplorerMev-index-123"));
        assert!(!is_valid_pipe_name(
            r"\\.\pipe\autre-0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_valid_pipe_name(&format!("{n}x")));
        assert!(!is_valid_pipe_name(&n.to_uppercase()));
    }

    #[test]
    fn server_reads_what_the_client_writes_and_checks_the_sid_acl() -> io::Result<()> {
        use std::io::Write;
        let name = random_pipe_name();
        let server = PipeServer::create(&name)?;
        // Deuxième création du même nom : refusée (première instance).
        assert!(PipeServer::create(&name).is_err());
        let n2 = name.clone();
        let writer = std::thread::spawn(move || -> io::Result<()> {
            let mut c = PipeClient::connect(&n2)?;
            c.write_all(b"bonjour")
        });
        // Client = ce processus : on attend notre propre pid.
        let me = std::process::id();
        // Tient lieu de handle de l'assistant : un événement jamais signalé.
        // SAFETY: événement sans nom, possédé par `never`.
        let never = OwnedHandle(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?);
        server.accept(me, never.0, Duration::from_secs(5))?;
        let mut buf = [0u8; 7];
        server.read_exact(&mut buf)?;
        assert_eq!(&buf, b"bonjour");
        writer.join().map_err(|_| io::Error::other("thread"))??;
        let mut more = [0u8; 4];
        assert_eq!(server.read(&mut more)?, 0, "fermeture vue comme fin");
        Ok(())
    }
}
