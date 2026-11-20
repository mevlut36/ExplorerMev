//! Apparence de l'application : mode (système, clair, sombre), couleur d'accent, fond teinté.
//!
//! Mémorisée dans `%APPDATA%\ExplorerMev\apparence.txt` (lignes `clé=valeur`). La lecture est
//! tolérante : une valeur inconnue ou un fichier illisible redonne les valeurs par défaut.

use std::path::Path;

/// Noms d'accent dans le fichier, dans l'ordre de `Theme.accents` (app.slint).
const ACCENTS: [&str; 8] = [
    "bleu",
    "rose",
    "vert",
    "violet",
    "orange",
    "rouge",
    "turquoise",
    "gris",
];
const MODES: [&str; 3] = ["systeme", "clair", "sombre"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appearance {
    /// 0 système, 1 clair, 2 sombre.
    pub mode: i32,
    /// Indice dans `Theme.accents`.
    pub accent: i32,
    pub tinted: bool,
}

impl Appearance {
    /// Clair ou sombre effectif, sachant le thème de Windows.
    pub fn effective_dark(&self, system_dark: bool) -> bool {
        match self.mode {
            1 => false,
            2 => true,
            _ => system_dark,
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut a = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_lowercase();
            match key.trim().to_lowercase().as_str() {
                "mode" => {
                    if let Some(i) = MODES.iter().position(|m| *m == value) {
                        a.mode = i as i32;
                    }
                }
                "accent" => {
                    if let Some(i) = ACCENTS.iter().position(|m| *m == value) {
                        a.accent = i as i32;
                    }
                }
                "teinte" => a.tinted = matches!(value.as_str(), "1" | "oui" | "true"),
                _ => {}
            }
        }
        a
    }

    pub fn to_text(self) -> String {
        format!(
            "mode={}\naccent={}\nteinte={}\n",
            MODES[self.mode.clamp(0, 2) as usize],
            ACCENTS[self.accent.clamp(0, ACCENTS.len() as i32 - 1) as usize],
            u8::from(self.tinted)
        )
    }
}

pub fn load(dir: Option<&Path>) -> Appearance {
    dir.and_then(|d| std::fs::read_to_string(d.join("apparence.txt")).ok())
        .map(|t| Appearance::parse(&t))
        .unwrap_or_default()
}

pub fn save(dir: Option<&Path>, a: &Appearance) {
    let Some(dir) = dir else { return };
    let r =
        std::fs::create_dir_all(dir).and_then(|()| std::fs::write(dir.join("apparence.txt"), a.to_text()));
    if let Err(e) = r {
        tracing::warn!("apparence non enregistrée : {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tolerance() {
        let a = Appearance {
            mode: 1,
            accent: 1,
            tinted: true,
        };
        assert_eq!(Appearance::parse(&a.to_text()), a);
        assert_eq!(Appearance::parse(""), Appearance::default());
        assert_eq!(
            Appearance::parse("mode=violet\naccent=\x00\nn'importe quoi\nteinte=peut-être"),
            Appearance::default()
        );
        assert_eq!(Appearance::parse("MODE = Sombre\nAccent=Vert").mode, 2);
        // Valeurs hors limites à l'écriture : ramenées dans la table.
        let wild = Appearance {
            mode: 9,
            accent: -3,
            tinted: false,
        };
        assert_eq!(Appearance::parse(&wild.to_text()).accent, 0);
    }

    #[test]
    fn effective_mode() {
        let a = |mode| Appearance {
            mode,
            ..Default::default()
        };
        assert!(a(0).effective_dark(true) && !a(0).effective_dark(false));
        assert!(!a(1).effective_dark(true) && a(2).effective_dark(false));
    }

    #[test]
    fn save_and_load_use_the_data_dir() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let a = Appearance {
            mode: 2,
            accent: 4,
            tinted: true,
        };
        save(Some(dir.path()), &a);
        assert_eq!(load(Some(dir.path())), a);
        assert_eq!(load(None), Appearance::default());
        Ok(())
    }
}
