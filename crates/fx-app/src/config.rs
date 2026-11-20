//! Configuration TOML dans `%APPDATA%\WinDir\config.toml`.
//!
//! Tolérante : un fichier absent est créé avec un modèle commenté ; un fichier illisible donne
//! les valeurs par défaut (sans être écrasé) ; une valeur invalide ne remplace que son propre
//! champ par la valeur par défaut. Chaque problème produit un avertissement lisible.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Réglages de l'application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// `software` (défaut) ou `gpu`.
    pub renderer: String,
    pub mica: bool,
    /// Dossier ouvert au démarrage (profil utilisateur si absent).
    pub start_folder: Option<PathBuf>,
    /// Afficher les éléments cachés (Ctrl+H bascule pendant la session).
    pub show_hidden: bool,
    /// Délai d'abandon d'un dossier qui ne répond pas (réseau), en secondes.
    pub network_timeout: Duration,
    /// Niveau du journal : `error`, `warn`, `info`, `debug`, `trace`, `off`.
    pub log_level: String,
    /// Affichage des nouveaux volets : vue icônes plutôt que détails.
    pub icons_view: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            renderer: "software".into(),
            mica: true,
            start_folder: None,
            show_hidden: false,
            network_timeout: Duration::from_secs(10),
            log_level: "info".into(),
            icons_view: false,
        }
    }
}

/// Modèle écrit au premier lancement.
pub const TEMPLATE: &str = r#"# Configuration de WinDir. Une valeur invalide est ignorée (valeur par défaut) avec un
# avertissement ; supprimer ce fichier le recrée avec les valeurs par défaut.

# Moteur de rendu : "software" (léger, défaut) ou "gpu" (OpenGL, plus de mémoire).
renderer = "software"

# Fond Mica de Windows 11.
mica = true

# Dossier ouvert au démarrage (profil utilisateur si absent).
# start_folder = 'C:\Users\Moi\Documents'

# Afficher les éléments cachés (Ctrl+H bascule pendant la session).
show_hidden = false

# Abandon d'un dossier qui ne répond pas (partage réseau injoignable), en secondes.
network_timeout_secs = 10

# Niveau du journal (%LOCALAPPDATA%\WinDir\logs) : "error", "warn", "info", "debug", "trace", "off".
log_level = "info"

# Affichage des nouveaux volets : "details" (défaut) ou "icons" (Ctrl+Maj+V bascule).
view = "details"
"#;

const KNOWN: [&str; 7] = [
    "view",
    "renderer",
    "mica",
    "start_folder",
    "show_hidden",
    "network_timeout_secs",
    "log_level",
];

pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("WinDir").join("config.toml"))
}

/// Lit la configuration depuis `path`. Renvoie toujours une configuration utilisable et la
/// liste des avertissements.
pub fn load(path: &Path) -> (Config, Vec<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, TEMPLATE);
            return (Config::default(), Vec::new());
        }
        Err(e) => {
            return (
                Config::default(),
                vec![format!(
                    "Configuration illisible ({e}) : valeurs par défaut utilisées."
                )],
            );
        }
    };
    parse(&text)
}

/// Analyse le texte d'une configuration (champ par champ, avec repli).
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut warnings = Vec::new();
    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            let first_line = e.to_string().lines().next().unwrap_or_default().to_string();
            warnings.push(format!(
                "config.toml invalide ({first_line}) : valeurs par défaut utilisées."
            ));
            return (cfg, warnings);
        }
    };
    let mut unknown: Vec<String> = Vec::new();
    let mut bad = |key: &str, expected: &str| {
        warnings.push(format!(
            "config.toml : « {key} » doit être {expected} ; valeur par défaut utilisée."
        ));
    };
    for (key, value) in &table {
        match key.as_str() {
            "renderer" => match value.as_str() {
                Some(r @ ("software" | "gpu")) => cfg.renderer = r.into(),
                _ => bad(key, "\"software\" ou \"gpu\""),
            },
            "mica" => match value.as_bool() {
                Some(b) => cfg.mica = b,
                None => bad(key, "true ou false"),
            },
            "show_hidden" => match value.as_bool() {
                Some(b) => cfg.show_hidden = b,
                None => bad(key, "true ou false"),
            },
            "start_folder" => match value.as_str() {
                Some(s) if !s.trim().is_empty() => cfg.start_folder = Some(PathBuf::from(s)),
                _ => bad(key, "un chemin de dossier"),
            },
            "network_timeout_secs" => match value.as_integer() {
                Some(n) if (1..=600).contains(&n) => cfg.network_timeout = Duration::from_secs(n as u64),
                _ => bad(key, "un nombre de secondes entre 1 et 600"),
            },
            "log_level" => match value.as_str() {
                Some(l @ ("error" | "warn" | "info" | "debug" | "trace" | "off")) => cfg.log_level = l.into(),
                _ => bad(key, "error, warn, info, debug, trace ou off"),
            },
            "view" => match value.as_str() {
                Some("details") => cfg.icons_view = false,
                Some("icons") => cfg.icons_view = true,
                _ => bad(key, "\"details\" ou \"icons\""),
            },
            other => {
                debug_assert!(!KNOWN.contains(&other));
                unknown.push(other.to_string());
            }
        }
    }
    for k in unknown {
        warnings.push(format!("config.toml : réglage inconnu « {k} » ignoré."));
    }
    (cfg, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults_without_warning() {
        let (cfg, w) = parse(TEMPLATE);
        assert_eq!(cfg, Config::default());
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn valid_values_are_applied() {
        let (cfg, w) = parse(
            "renderer = \"gpu\"\nmica = false\nshow_hidden = true\nnetwork_timeout_secs = 3\n\
             start_folder = 'D:\\Projets'\nlog_level = \"debug\"\nview = \"icons\"\n",
        );
        assert!(cfg.icons_view);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(cfg.renderer, "gpu");
        assert!(!cfg.mica && cfg.show_hidden);
        assert_eq!(cfg.network_timeout, Duration::from_secs(3));
        assert_eq!(cfg.start_folder, Some(PathBuf::from(r"D:\Projets")));
        assert_eq!(cfg.log_level, "debug");
    }

    #[test]
    fn invalid_field_falls_back_alone() {
        let (cfg, w) = parse(
            "renderer = \"vulkan\"\nmica = \"oui\"\nshow_hidden = true\nnetwork_timeout_secs = 0\nzoom = 2\n",
        );
        assert_eq!(cfg.renderer, "software");
        assert!(cfg.mica);
        assert!(cfg.show_hidden, "un champ valide est conservé");
        assert_eq!(cfg.network_timeout, Duration::from_secs(10));
        assert_eq!(w.len(), 4, "{w:?}");
    }

    #[test]
    fn corrupted_file_gives_defaults_and_is_not_overwritten() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "renderer = \"gpu\n[[[")?;
        let (cfg, w) = load(&path);
        assert_eq!(cfg, Config::default());
        assert_eq!(w.len(), 1);
        assert_eq!(std::fs::read_to_string(&path)?, "renderer = \"gpu\n[[[");
        Ok(())
    }

    #[test]
    fn missing_file_creates_template() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("WinDir").join("config.toml");
        let (cfg, w) = load(&path);
        assert_eq!(cfg, Config::default());
        assert!(w.is_empty());
        assert_eq!(std::fs::read_to_string(&path)?, TEMPLATE);
        Ok(())
    }
}
