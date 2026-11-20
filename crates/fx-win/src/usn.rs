//! Lecture de la MFT et du journal USN d'un volume NTFS (`FSCTL_ENUM_USN_DATA`,
//! `FSCTL_READ_USN_JOURNAL`) : l'index de recherche se construit sans parcourir les dossiers.
//!
//! Ouvrir le volume (`\\.\C:`) exige les droits d'administration : ces fonctions sont
//! appelées par l'assistant d'indexation élevé (`explorermev --index-helper`), jamais par
//! l'interface. Les enregistrements sont décodés depuis des octets (code sûr), avec contrôle de
//! chaque longueur.

use std::ffi::c_void;
use std::io;

use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V1,
    READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0,
};
use windows::core::HSTRING;

/// Taille du tampon d'échange avec le pilote NTFS.
const BUFFER: usize = 1 << 20;

/// Volume ouvert en lecture.
pub struct Volume {
    handle: HANDLE,
}

// SAFETY: un HANDLE de volume peut être utilisé depuis n'importe quel thread.
unsafe impl Send for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: handle ouvert par `open`, fermé une seule fois.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// État du journal USN du volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalInfo {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
}

/// Enregistrement USN décodé (version 2, références de fichier sur 64 bits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsnRecord {
    pub frn: u64,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    /// Identifiant du descripteur de sécurité (partagé dans `$Secure`).
    pub security_id: u32,
    pub name: Vec<u16>,
}

/// Raisons du journal utiles à l'index.
pub mod reason {
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    pub const CLOSE: u32 = 0x8000_0000;
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Décode les enregistrements `USN_RECORD_V2` d'un tampon (sans l'en-tête de 8 octets).
/// Un enregistrement mal formé arrête le décodage (jamais de lecture hors du tampon).
pub fn parse_records(mut b: &[u8], mut f: impl FnMut(UsnRecord)) {
    while b.len() >= 60 {
        let Some(len) = u32_at(b, 0).map(|l| l as usize) else {
            return;
        };
        if len < 60 || len > b.len() || len % 8 != 0 {
            return;
        }
        let rec = &b[..len];
        b = &b[len..];
        if u16_at(rec, 4) != Some(2) {
            continue; // version 3/4 (ReFS, plages) : ignorée
        }
        let (Some(frn), Some(parent), Some(usn), Some(reason), Some(attributes)) = (
            u64_at(rec, 8),
            u64_at(rec, 16),
            u64_at(rec, 24),
            u32_at(rec, 40),
            u32_at(rec, 52),
        ) else {
            return;
        };
        let (Some(name_len), Some(name_off), Some(security_id)) =
            (u16_at(rec, 56), u16_at(rec, 58), u32_at(rec, 48))
        else {
            return;
        };
        let (name_len, name_off) = (name_len as usize, name_off as usize);
        let Some(raw) = rec.get(name_off..name_off + name_len) else {
            return;
        };
        let name = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        f(UsnRecord {
            frn,
            parent,
            usn: usn as i64,
            reason,
            attributes,
            security_id,
            name,
        });
    }
}

impl Volume {
    /// Ouvre `\\.\X:` en lecture (droits d'administration requis).
    pub fn open(letter: char) -> io::Result<Self> {
        if !letter.is_ascii_alphabetic() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lettre de lecteur invalide",
            ));
        }
        let path = HSTRING::from(format!(r"\\.\{}:", letter.to_ascii_uppercase()));
        // SAFETY: chaîne HSTRING vivante pendant l'appel ; le handle est possédé par `Volume`.
        let handle = unsafe {
            CreateFileW(
                &path,
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(Self { handle })
    }

    fn ioctl(&self, code: u32, input: &[u8], out: &mut [u8]) -> io::Result<usize> {
        let mut returned = 0u32;
        // SAFETY: tampons d'entrée et de sortie locaux dont les tailles sont transmises ;
        // appel synchrone sur un handle valide.
        unsafe {
            DeviceIoControl(
                self.handle,
                code,
                Some(input.as_ptr() as *const c_void),
                input.len() as u32,
                Some(out.as_mut_ptr() as *mut c_void),
                out.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(returned as usize)
    }

    /// Identifiant et bornes du journal USN.
    pub fn journal(&self) -> io::Result<JournalInfo> {
        let mut out = [0u8; size_of::<USN_JOURNAL_DATA_V0>()];
        let n = self.ioctl(FSCTL_QUERY_USN_JOURNAL, &[], &mut out)?;
        let b = &out[..n];
        match (u64_at(b, 0), u64_at(b, 8), u64_at(b, 16)) {
            (Some(id), Some(first), Some(next)) => Ok(JournalInfo {
                id,
                first_usn: first as i64,
                next_usn: next as i64,
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "réponse du journal tronquée",
            )),
        }
    }

    /// Énumère tous les fichiers et dossiers du volume (entrées de la MFT), par lots.
    pub fn enum_mft(&self, high_usn: i64, mut f: impl FnMut(UsnRecord)) -> io::Result<()> {
        let mut out = vec![0u8; BUFFER];
        let mut start: u64 = 0;
        loop {
            let input = MFT_ENUM_DATA_V1 {
                StartFileReferenceNumber: start,
                LowUsn: 0,
                HighUsn: high_usn,
                MinMajorVersion: 2,
                MaxMajorVersion: 2,
            };
            let n = match self.ioctl(FSCTL_ENUM_USN_DATA, as_bytes(&input), &mut out) {
                Ok(n) => n,
                Err(e) if e.raw_os_error() == Some(ERROR_HANDLE_EOF.0 as i32) => return Ok(()),
                Err(e) => return Err(e),
            };
            let Some(next) = u64_at(&out[..n], 0) else {
                return Ok(());
            };
            parse_records(&out[8..n], &mut f);
            if next == start {
                return Ok(());
            }
            start = next;
        }
    }

    /// Lit le journal à partir de `start_usn` ; renvoie l'USN où reprendre.
    pub fn read_journal(
        &self,
        journal_id: u64,
        start_usn: i64,
        mut f: impl FnMut(UsnRecord),
    ) -> io::Result<i64> {
        let mut out = vec![0u8; BUFFER];
        let mut usn = start_usn;
        loop {
            let input = READ_USN_JOURNAL_DATA_V1 {
                StartUsn: usn,
                ReasonMask: 0xFFFF_FFFF,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                BytesToWaitFor: 0,
                UsnJournalID: journal_id,
                MinMajorVersion: 2,
                MaxMajorVersion: 2,
            };
            let n = self.ioctl(FSCTL_READ_USN_JOURNAL, as_bytes(&input), &mut out)?;
            let Some(next) = u64_at(&out[..n], 0) else {
                return Ok(usn);
            };
            let next = next as i64;
            if n <= 8 || next == usn {
                return Ok(next);
            }
            parse_records(&out[8..n], &mut f);
            usn = next;
        }
    }
}

impl Volume {
    /// Descripteur de sécurité (propriétaire, groupe, DACL) de l'élément `frn` du volume,
    /// ouvert par identifiant (`OpenFileById`, droit `READ_CONTROL` seulement).
    pub fn security_of(&self, frn: u64) -> io::Result<crate::access::SecurityDescriptor> {
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_SHARE_DELETE,
            FileIdType, OpenFileById, READ_CONTROL,
        };
        let id = FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
        };
        // SAFETY: handle de volume valide (indice de volume) ; descripteur local ; le handle
        // renvoyé est possédé puis fermé.
        let h = unsafe {
            OpenFileById(
                self.handle,
                &id,
                READ_CONTROL.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        }
        .map_err(io::Error::from)?;
        let h = crate::handle::OwnedHandle(h);
        crate::access::SecurityDescriptor::of_handle(h.0)
    }
}

/// Octets d'une structure d'entrée `repr(C)` sans pointeurs.
fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: `T` est une structure `repr(C)` de types entiers (entrées des FSCTL), lue en
    // octets pendant la durée de l'emprunt.
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(frn: u64, parent: u64, attrs: u32, name: &str) -> Vec<u8> {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (60 + name.len()).div_ceil(8) * 8;
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&frn.to_le_bytes());
        r[16..24].copy_from_slice(&parent.to_le_bytes());
        r[52..56].copy_from_slice(&attrs.to_le_bytes());
        r[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        r[58..60].copy_from_slice(&60u16.to_le_bytes());
        r[60..60 + name.len()].copy_from_slice(&name);
        r
    }

    #[test]
    fn parses_records_and_stops_on_garbage() {
        let mut b = record(7, 5, 0x10, "Documents");
        b.extend(record(9, 7, 0x20, "été.txt"));
        b.extend([0xFF; 12]); // déchet final
        let mut got = Vec::new();
        parse_records(&b, |r| {
            got.push((r.frn, r.parent, r.attributes, String::from_utf16_lossy(&r.name)))
        });
        assert_eq!(
            got,
            vec![
                (7, 5, 0x10, "Documents".to_string()),
                (9, 7, 0x20, "été.txt".to_string())
            ]
        );
        // Longueur de nom qui dépasse l'enregistrement : rien n'est lu hors du tampon.
        let mut bad = record(1, 5, 0, "x");
        bad[56..58].copy_from_slice(&500u16.to_le_bytes());
        let mut n = 0;
        parse_records(&bad, |_| n += 1);
        assert_eq!(n, 0);
    }
}
