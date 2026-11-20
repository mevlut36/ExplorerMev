//! Option « Remplacer Win+E » : la touche Windows+E ouvre WinDir au lieu de l'Explorateur.
//!
//! Windows lance la commande `opennewwindow` de l'objet « Ce PC »
//! (`{52205fd8-5dfb-447d-801a-d0b52f2e83e1}`). Une commande par utilisateur sous
//! `HKCU\Software\Classes\CLSID\{…}\shell\opennewwindow\command` (valeur par défaut = programme,
//! `DelegateExecute` vide) prend le pas sur celle du système. Rien n'est écrit hors de HKCU ;
//! désactiver supprime la clé `CLSID\{…}` que WinDir a créée, et seulement si elle désigne
//! WinDir : l'état d'origine est rétabli.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

/// Objet « Ce PC » dont Windows ouvre une nouvelle fenêtre sur Win+E.
const THIS_PC_CLSID: &str = "{52205fd8-5dfb-447d-801a-d0b52f2e83e1}";
/// Marqueur posé sur la clé créée par WinDir.
const OWNER_VALUE: &str = "WinDir";

/// État de Win+E pour l'utilisateur courant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WinEState {
    /// Comportement de Windows (Explorateur).
    Default,
    /// Remplacé par WinDir (programme indiqué).
    WinDir(PathBuf),
    /// Remplacé par un autre programme : WinDir n'y touche pas.
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum WinEError {
    #[error("Win+E est déjà associé à un autre programme ({0}) ; WinDir ne le remplace pas.")]
    TakenByOther(String),
    #[error("Registre : {0}")]
    Registry(#[from] windows::core::Error),
}

/// Emplacement des clés : `HKCU\<base>\CLSID\{…}`. `base` vaut `Software\Classes` en usage
/// réel ; les tests utilisent une clé à eux.
#[derive(Clone, Debug)]
pub struct WinEKey {
    base: String,
}

impl Default for WinEKey {
    fn default() -> Self {
        Self {
            base: r"Software\Classes".into(),
        }
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: clé ouverte par RegOpenKeyExW / RegCreateKeyExW, fermée une seule fois.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn check(e: WIN32_ERROR) -> windows::core::Result<()> {
    e.ok()
}

impl WinEKey {
    /// Clé de test sous `HKCU\<base>` (à supprimer avec [`WinEKey::delete_base`]).
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    fn clsid_key(&self) -> String {
        format!(r"{}\CLSID\{THIS_PC_CLSID}", self.base)
    }

    fn command_key(&self) -> String {
        format!(r"{}\shell\opennewwindow\command", self.clsid_key())
    }

    fn open(path: &str) -> windows::core::Result<Option<Key>> {
        let mut h = HKEY::default();
        // SAFETY: chaîne HSTRING vivante pendant l'appel ; `h` reçoit une clé que `Key` ferme.
        let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), Some(0), KEY_READ, &mut h) };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(r)?;
        Ok(Some(Key(h)))
    }

    fn read(key: &Key, name: &str) -> windows::core::Result<Option<String>> {
        let name = HSTRING::from(name);
        let mut buf = vec![0u16; 1024];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: clé ouverte ; tampon local dont la taille en octets est transmise.
        let r = unsafe {
            RegGetValueW(
                key.0,
                PCWSTR::null(),
                &name,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(r)?;
        let len = (bytes as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        Ok(Some(s.trim_end_matches('\0').to_string()))
    }

    /// État actuel de Win+E.
    pub fn state(&self) -> windows::core::Result<WinEState> {
        let Some(cmd) = Self::open(&self.command_key())? else {
            return Ok(WinEState::Default);
        };
        let command = Self::read(&cmd, "")?.unwrap_or_default();
        let ours = Self::open(&self.clsid_key())?
            .map(|k| Self::read(&k, OWNER_VALUE))
            .transpose()?
            .flatten()
            .is_some();
        Ok(if command.trim().is_empty() {
            WinEState::Default
        } else if ours {
            WinEState::WinDir(PathBuf::from(unquote(&command)))
        } else {
            WinEState::Other(command)
        })
    }

    /// Associe Win+E à `exe`. Refuse si un autre programme l'a déjà pris.
    pub fn enable(&self, exe: &Path) -> Result<(), WinEError> {
        if let WinEState::Other(cmd) = self.state()? {
            return Err(WinEError::TakenByOther(cmd));
        }
        let command = format!("\"{}\"", exe.display());
        let marker = self.clsid_key();
        let mut h = HKEY::default();
        // SAFETY: chaînes HSTRING vivantes pendant les appels ; `h` est fermée par `Key`.
        unsafe {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(marker.as_str()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut h,
                None,
            ))?;
        }
        let clsid = Key(h);
        set_string(&clsid, OWNER_VALUE, &exe.display().to_string())?;
        let mut h = HKEY::default();
        // SAFETY: idem.
        unsafe {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(self.command_key()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut h,
                None,
            ))?;
        }
        let cmd = Key(h);
        set_string(&cmd, "", &command)?;
        set_string(&cmd, "DelegateExecute", "")?;
        Ok(())
    }

    /// Rend Win+E à l'Explorateur, si (et seulement si) WinDir l'avait pris.
    pub fn disable(&self) -> windows::core::Result<bool> {
        if !matches!(self.state()?, WinEState::WinDir(_)) {
            return Ok(false);
        }
        self.delete_tree(&self.clsid_key())?;
        Ok(true)
    }

    /// Supprime la clé de base (tests uniquement : jamais `Software\Classes`).
    pub fn delete_base(&self) -> windows::core::Result<()> {
        if self.base.eq_ignore_ascii_case(r"Software\Classes") {
            return Ok(());
        }
        self.delete_tree(&self.base)
    }

    fn delete_tree(&self, path: &str) -> windows::core::Result<()> {
        // SAFETY: chaîne HSTRING vivante pendant l'appel.
        let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path)) };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(r)
    }
}

fn set_string(key: &Key, name: &str, value: &str) -> windows::core::Result<()> {
    let data: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: clé ouverte en écriture ; données REG_SZ terminées par un nul.
    unsafe {
        check(RegSetValueExW(
            key.0,
            &HSTRING::from(name),
            None,
            REG_SZ,
            Some(&data),
        ))
    }
}

fn unquote(command: &str) -> &str {
    let c = command.trim();
    if let Some(rest) = c.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else {
        c.split(' ').next().unwrap_or(c)
    }
}
