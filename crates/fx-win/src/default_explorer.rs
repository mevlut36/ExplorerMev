//! « Explorateur par défaut » : WinDir s'ouvre à la place de l'Explorateur de fichiers quand
//! l'utilisateur ou un autre programme ouvre un dossier.
//!
//! **Ce que cela change** (pour le compte courant seulement, sous `HKCU\Software\Classes`) :
//! - `Win+E` : voir [`crate::win_e`] ;
//! - le verbe `open` des classes `Directory` et `Drive` : ouvrir un dossier ou un lecteur
//!   depuis un autre programme, `Win+R`, le menu Démarrer… lance `windir.exe "<dossier>"`.
//!
//! **Ce que cela ne change pas** : `explorer.exe` reste le shell de Windows (barre des tâches,
//! bureau, menu Démarrer). Remplacer le shell n'est pas pris en charge : WinDir n'en est pas un.
//! La navigation à l'intérieur d'une fenêtre de l'Explorateur déjà ouverte n'est pas
//! interceptée non plus.
//!
//! **Réversible** : chaque clé posée porte un marqueur (la valeur `WinDir`, chemin du
//! programme) ; désactiver ne supprime que ce qui porte le marqueur, et la valeur par défaut de
//! `Directory\shell` seulement si c'est celle que WinDir a écrite. Si un autre programme a déjà
//! pris l'un de ces emplacements, rien n'est modifié.

use std::path::{Path, PathBuf};

use crate::win_e::{
    Key, WinEError, WinEKey, WinEState, create_key, delete_tree, delete_value, set_string, unquote,
};

/// Classes dont le verbe `open` est redirigé.
const CLASSES: [&str; 2] = ["Directory", "Drive"];
/// Marqueur posé sur la clé `open` créée par WinDir (valeur = chemin du programme).
const MARKER: &str = "WinDir";
/// Verbe par défaut que WinDir écrit sur `<classe>\shell`.
const DEFAULT_VERB: &str = "open";

/// État global de l'option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultState {
    /// Comportement de Windows.
    Windows,
    /// Win+E et dossiers ouvrent WinDir (programme indiqué).
    WinDir(PathBuf),
    /// Une partie seulement est active (réparable en réactivant ou désactivant).
    Partial,
    /// Un autre programme occupe un des emplacements : WinDir n'y touche pas.
    Other(String),
}

/// Emplacement des clés (`Software\Classes` en usage réel ; une clé de test sinon).
#[derive(Clone, Debug)]
pub struct DefaultExplorer {
    base: String,
}

impl Default for DefaultExplorer {
    fn default() -> Self {
        Self {
            base: r"Software\Classes".into(),
        }
    }
}

impl DefaultExplorer {
    /// Clé de test sous `HKCU\<base>` (à supprimer avec [`DefaultExplorer::delete_base`]).
    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    fn win_e(&self) -> WinEKey {
        WinEKey::with_base(self.base.clone())
    }

    fn shell_key(&self, class: &str) -> String {
        format!(r"{}\{class}\shell", self.base)
    }

    fn open_key(&self, class: &str) -> String {
        format!(r"{}\open", self.shell_key(class))
    }

    /// État du verbe `open` d'une classe.
    fn class_state(&self, class: &str) -> windows::core::Result<WinEState> {
        // Verbe par défaut déjà défini par autre chose que nous : on n'y touche pas.
        if let Some(shell) = WinEKey::open(&self.shell_key(class))? {
            let default = WinEKey::read(&shell, "")?.unwrap_or_default();
            if !default.trim().is_empty() && !default.eq_ignore_ascii_case(DEFAULT_VERB) {
                return Ok(WinEState::Other(format!(
                    "verbe par défaut de {class} : {default}"
                )));
            }
        }
        let Some(open) = WinEKey::open(&self.open_key(class))? else {
            return Ok(WinEState::Default);
        };
        let command = match WinEKey::open(&format!(r"{}\command", self.open_key(class)))? {
            Some(c) => WinEKey::read(&c, "")?.unwrap_or_default(),
            None => String::new(),
        };
        Ok(match WinEKey::read(&open, MARKER)? {
            Some(path) => WinEState::WinDir(PathBuf::from(path)),
            None => WinEState::Other(if command.is_empty() {
                format!("verbe open de {class}")
            } else {
                command
            }),
        })
    }

    /// État de l'option : Win+E et chaque classe de dossiers.
    pub fn state(&self) -> windows::core::Result<DefaultState> {
        let mut parts = vec![self.win_e().state()?];
        for class in CLASSES {
            parts.push(self.class_state(class)?);
        }
        if let Some(WinEState::Other(what)) = parts.iter().find(|p| matches!(p, WinEState::Other(_))) {
            return Ok(DefaultState::Other(what.clone()));
        }
        let ours: Vec<&PathBuf> = parts
            .iter()
            .filter_map(|p| {
                if let WinEState::WinDir(path) = p {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        Ok(match ours.as_slice() {
            [] => DefaultState::Windows,
            all if all.len() == parts.len() => DefaultState::WinDir(all[0].clone()),
            _ => DefaultState::Partial,
        })
    }

    /// Active l'option pour `exe`. Refuse, sans rien modifier, si un autre programme occupe un
    /// des emplacements. En cas d'erreur d'écriture, ce qui a été posé est retiré.
    pub fn enable(&self, exe: &Path) -> Result<(), WinEError> {
        if let DefaultState::Other(what) = self.state()? {
            return Err(WinEError::TakenByOther(what));
        }
        let result = self.write_all(exe);
        if result.is_err() {
            let _ = self.disable();
        }
        result
    }

    fn write_all(&self, exe: &Path) -> Result<(), WinEError> {
        let exe_text = exe.display().to_string();
        for class in CLASSES {
            let shell = create_key(&self.shell_key(class))?;
            set_string(&shell, "", DEFAULT_VERB)?;
            let open = create_key(&self.open_key(class))?;
            set_string(&open, MARKER, &exe_text)?;
            set_string(&open, "MUIVerb", "Ouvrir avec WinDir")?;
            let command: Key = create_key(&format!(r"{}\command", self.open_key(class)))?;
            // `\.` : un lecteur arrive comme « D:\ », et `"D:\"` serait lu comme un guillemet
            // échappé ; `D:\\.` est normalisé en `D:\` par Windows.
            set_string(&command, "", &format!("\"{exe_text}\" \"%1\\.\""))?;
            // Sans cela, un DelegateExecute hérité de la classe parente l'emporterait.
            set_string(&command, "DelegateExecute", "")?;
        }
        self.win_e().enable(exe)
    }

    /// Rend l'Explorateur aux deux emplacements. `true` si quelque chose a été retiré.
    pub fn disable(&self) -> windows::core::Result<bool> {
        let mut changed = self.win_e().disable()?;
        for class in CLASSES {
            if !matches!(self.class_state(class)?, WinEState::WinDir(_)) {
                continue;
            }
            delete_tree(&self.open_key(class))?;
            // La valeur par défaut de `shell` n'est retirée que si c'est celle que WinDir a
            // écrite (jamais une valeur qui existait avant).
            if let Some(shell) = writable(&self.shell_key(class))? {
                let current = crate::win_e::WinEKey::read(&shell, "")?.unwrap_or_default();
                if current.eq_ignore_ascii_case(DEFAULT_VERB) {
                    delete_value(&shell, "")?;
                }
            }
            changed = true;
        }
        Ok(changed)
    }

    /// Supprime la clé de base (tests uniquement : jamais `Software\Classes`).
    pub fn delete_base(&self) -> windows::core::Result<()> {
        if self.base.eq_ignore_ascii_case(r"Software\Classes") {
            return Ok(());
        }
        delete_tree(&self.base)
    }
}

/// Ouvre `HKCU\<path>` en lecture et écriture (valeur par défaut à lire puis supprimer).
fn writable(path: &str) -> windows::core::Result<Option<Key>> {
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, RegOpenKeyExW};
    use windows::core::HSTRING;
    let mut h = HKEY::default();
    // SAFETY: chaîne HSTRING vivante pendant l'appel ; `h` reçoit une clé que `Key` ferme.
    let r = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            Some(0),
            KEY_READ | KEY_WRITE,
            &mut h,
        )
    };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    crate::win_e::check(r)?;
    Ok(Some(Key(h)))
}

/// Texte du programme enregistré dans une commande (pour l'affichage).
pub fn program_of(command: &str) -> PathBuf {
    PathBuf::from(unquote(command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win_e::WinEKey;

    fn read_default(path: &str) -> Option<String> {
        let k = WinEKey::open(path).ok()??;
        WinEKey::read(&k, "").ok()?
    }

    fn fixture(tag: &str) -> DefaultExplorer {
        DefaultExplorer::with_base(format!(r"Software\WinDir-test-{tag}-{}", std::process::id()))
    }

    #[test]
    fn enable_then_disable_restores_everything() -> Result<(), Box<dyn std::error::Error>> {
        let d = fixture("def");
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            assert_eq!(d.state()?, DefaultState::Windows);
            assert!(!d.disable()?, "rien à retirer");
            // Une clé voisine préexistante (comme celles des autres outils) doit survivre.
            let neighbour = create_key(&format!(r"{}\Directory\shell\autre\command", d.base))?;
            set_string(&neighbour, "", r"C:\autre.exe")?;

            let exe = Path::new(r"C:\Outils\WinDir\windir.exe");
            d.enable(exe)?;
            assert_eq!(d.state()?, DefaultState::WinDir(exe.to_path_buf()));
            for class in CLASSES {
                assert_eq!(read_default(&d.shell_key(class)).as_deref(), Some("open"));
                let cmd = read_default(&format!(r"{}\command", d.open_key(class))).ok_or("commande")?;
                assert_eq!(cmd, r#""C:\Outils\WinDir\windir.exe" "%1\.""#);
            }
            // Réactiver (mise à jour du chemin) est permis.
            let moved = Path::new(r"D:\WinDir\windir.exe");
            d.enable(moved)?;
            assert_eq!(d.state()?, DefaultState::WinDir(moved.to_path_buf()));

            assert!(d.disable()?);
            assert_eq!(d.state()?, DefaultState::Windows);
            for class in CLASSES {
                assert!(WinEKey::open(&d.open_key(class))?.is_none(), "verbe open retiré");
                assert_eq!(
                    read_default(&d.shell_key(class)).unwrap_or_default(),
                    "",
                    "défaut retiré"
                );
            }
            let still = read_default(&format!(r"{}\Directory\shell\autre\command", d.base));
            assert_eq!(still.as_deref(), Some(r"C:\autre.exe"), "clé voisine intacte");
            Ok(())
        })();
        d.delete_base()?;
        result
    }

    #[test]
    fn other_programs_are_never_overwritten() -> Result<(), Box<dyn std::error::Error>> {
        // 1. Un autre programme a déjà un verbe open sur Directory.
        let d = fixture("open");
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let c = create_key(&format!(r"{}\command", d.open_key("Directory")))?;
            set_string(&c, "", r"C:\Autre\gestionnaire.exe %1")?;
            assert!(matches!(d.state()?, DefaultState::Other(_)));
            assert!(matches!(
                d.enable(Path::new(r"C:\w.exe")),
                Err(WinEError::TakenByOther(_))
            ));
            assert!(!d.disable()?);
            assert_eq!(
                read_default(&format!(r"{}\command", d.open_key("Directory"))).as_deref(),
                Some(r"C:\Autre\gestionnaire.exe %1"),
                "commande de l'autre programme intacte"
            );
            assert!(
                WinEKey::open(&d.open_key("Drive"))?.is_none(),
                "rien posé sur Drive"
            );
            Ok(())
        })();
        d.delete_base()?;
        result?;
        // 2. Un autre programme a déjà choisi un verbe par défaut sur Drive.
        let d = fixture("verb");
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let shell = create_key(&d.shell_key("Drive"))?;
            set_string(&shell, "", "autreverbe")?;
            assert!(matches!(
                d.enable(Path::new(r"C:\w.exe")),
                Err(WinEError::TakenByOther(_))
            ));
            assert_eq!(read_default(&d.shell_key("Drive")).as_deref(), Some("autreverbe"));
            assert!(
                WinEKey::open(&d.open_key("Directory"))?.is_none(),
                "rien posé du tout"
            );
            Ok(())
        })();
        d.delete_base()?;
        result
    }

    #[test]
    fn partial_state_is_reported_and_repaired() -> Result<(), Box<dyn std::error::Error>> {
        let d = fixture("part");
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let exe = Path::new(r"C:\w\windir.exe");
            d.enable(exe)?;
            // Quelqu'un supprime à la main le verbe de Drive : l'état devient « partiel ».
            delete_tree(&d.open_key("Drive"))?;
            assert_eq!(d.state()?, DefaultState::Partial);
            d.enable(exe)?;
            assert_eq!(d.state()?, DefaultState::WinDir(exe.to_path_buf()));
            assert!(d.disable()?);
            assert_eq!(d.state()?, DefaultState::Windows);
            Ok(())
        })();
        d.delete_base()?;
        result
    }

    #[test]
    fn real_classes_are_never_touched_by_tests() {
        // La clé de base par défaut est protégée contre la suppression.
        let d = DefaultExplorer::default();
        assert!(d.delete_base().is_ok());
    }
}
