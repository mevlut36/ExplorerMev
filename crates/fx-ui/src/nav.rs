//! Volet de navigation (dossiers connus, lecteurs) et fil d'Ariane.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

use fx_win::shell::{DriveKind, KnownFolder, display_name, known_folder, logical_drives};

#[derive(Clone, Debug)]
pub struct NavEntry {
    pub label: String,
    pub glyph: &'static str,
    pub path: PathBuf,
    pub header: bool,
}

/// Délai maximal pour obtenir le nom d'un lecteur (réseau injoignable).
const DRIVE_LABEL_TIMEOUT: Duration = Duration::from_millis(1500);

fn drive_letter(p: &Path) -> String {
    p.to_string_lossy().trim_end_matches('\\').to_string()
}

/// Construit les entrées du volet hors du thread d'interface, puis appelle `ready`.
pub fn build(ready: impl FnOnce(Vec<NavEntry>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("fx-nav".into()).spawn(move || {
        let _com = fx_win::com::ComApartment::sta().ok();
        let mut out = Vec::new();
        out.push(NavEntry {
            label: "Accès rapide".into(),
            glyph: "",
            path: PathBuf::new(),
            header: true,
        });
        let known = [
            (KnownFolder::Desktop, "\u{E8FC}", "Bureau"),
            (KnownFolder::Downloads, "\u{E896}", "Téléchargements"),
            (KnownFolder::Documents, "\u{E8A5}", "Documents"),
            (KnownFolder::Pictures, "\u{E91B}", "Images"),
            (KnownFolder::Music, "\u{EC4F}", "Musique"),
            (KnownFolder::Videos, "\u{E714}", "Vidéos"),
        ];
        for (kf, glyph, fallback) in known {
            if let Some(path) = known_folder(kf) {
                let label = display_name(&path).unwrap_or_else(|| fallback.into());
                out.push(NavEntry {
                    label,
                    glyph,
                    path,
                    header: false,
                });
            }
        }
        out.push(NavEntry {
            label: "Ce PC".into(),
            glyph: "",
            path: PathBuf::new(),
            header: true,
        });
        for (path, kind) in logical_drives() {
            let glyph = match kind {
                DriveKind::Network => "\u{E8CE}",
                DriveKind::Removable => "\u{E88E}",
                DriveKind::Optical => "\u{E958}",
                _ => "\u{EDA2}",
            };
            // Le nom d'un lecteur réseau peut bloquer : thread dédié avec délai.
            let (tx, rx) = channel();
            let p = path.clone();
            let _ = std::thread::Builder::new()
                .name("fx-drive-label".into())
                .spawn(move || {
                    let _com = fx_win::com::ComApartment::sta().ok();
                    let _ = tx.send(display_name(&p));
                });
            let label = rx
                .recv_timeout(DRIVE_LABEL_TIMEOUT)
                .ok()
                .flatten()
                .unwrap_or_else(|| format!("Lecteur ({})", drive_letter(&path)));
            out.push(NavEntry {
                label,
                glyph,
                path,
                header: false,
            });
        }
        // Linux (WSL) : une entrée par distribution installée, comme le nœud « Linux » de
        // l'Explorateur.
        let distros = fx_win::wsl::distributions();
        if !distros.is_empty() {
            out.push(NavEntry {
                label: "Linux".into(),
                glyph: "",
                path: PathBuf::new(),
                header: true,
            });
            for d in distros {
                out.push(NavEntry {
                    path: PathBuf::from(format!(r"{}\{d}", fx_core::wsl::ROOT)),
                    label: d,
                    glyph: "\u{E756}",
                    header: false,
                });
            }
        }
        ready(out);
    });
    if let Err(e) = spawned {
        tracing::error!("volet de navigation non construit : {e}");
    }
}

/// Segments du fil d'Ariane : (libellé, chemin). La racine prend le nom du lecteur s'il est
/// connu (« Disque local (C:) »).
pub fn crumbs(path: &Path, drive_labels: &HashMap<PathBuf, String>) -> Vec<(String, PathBuf)> {
    let shown = fx_win::path::display_form(path);
    // Racine « Linux » : un segment lui est consacré.
    if fx_core::wsl::is_root(&shown) {
        return vec![("Linux".into(), PathBuf::from(fx_core::wsl::ROOT))];
    }
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut acc = PathBuf::new();
    for comp in shown.components() {
        match comp {
            Component::Prefix(p) => {
                // `\\wsl.localhost\Ubuntu` : segments « Linux » puis « Ubuntu ».
                if let std::path::Prefix::UNC(server, share) = p.kind()
                    && (server.eq_ignore_ascii_case("wsl.localhost") || server.eq_ignore_ascii_case("wsl$"))
                {
                    out.push(("Linux".into(), PathBuf::from(fx_core::wsl::ROOT)));
                    out.push((share.to_string_lossy().into_owned(), p.as_os_str().into()));
                }
                acc.push(p.as_os_str());
            }
            Component::RootDir
                if matches!(
                    out.last(),
                    Some((l, _)) if l != "Linux" && fx_core::wsl::parent_of_distro_root(&acc).is_some()
                ) =>
            {
                // Racine d'une distribution : déjà représentée par le segment précédent.
                acc.push("\\");
            }
            Component::RootDir => {
                acc.push("\\");
                let label = drive_labels
                    .get(&acc)
                    .cloned()
                    .unwrap_or_else(|| acc.to_string_lossy().trim_end_matches('\\').to_string());
                out.push((label, acc.clone()));
            }
            Component::Normal(n) => {
                acc.push(n);
                out.push((n.to_string_lossy().into_owned(), acc.clone()));
            }
            Component::CurDir | Component::ParentDir => {}
        }
    }
    out
}

/// Suggestions d'autocomplétion : sous-dossiers du dossier parent de `text` commençant par le
/// dernier segment saisi (insensible à la casse), au plus `max`.
pub fn suggestions(text: &str, max: usize) -> Vec<String> {
    let Some(sep) = text.rfind('\\') else {
        return Vec::new();
    };
    let (parent, prefix) = text.split_at(sep + 1);
    let prefix = prefix.to_lowercase();
    let mut found = Vec::new();
    let r = fx_win::listing::list_dir(
        Path::new(parent),
        &fx_win::listing::ListOptions::default(),
        &|| false,
        &mut |e| {
            if e.attributes & fx_core::folder::ATTR_DIRECTORY != 0 {
                let name = String::from_utf16_lossy(e.name);
                if name.to_lowercase().starts_with(&prefix) {
                    found.push(name);
                }
            }
        },
    );
    if r.is_err() {
        return Vec::new();
    }
    found.sort_by_key(|n| n.to_lowercase());
    found
        .into_iter()
        .take(max)
        .map(|n| format!("{parent}{n}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crumbs_of_drive_path_use_drive_label() {
        let mut labels = HashMap::new();
        labels.insert(PathBuf::from(r"C:\"), "Disque local (C:)".to_string());
        let c = crumbs(Path::new(r"C:\Users\moi"), &labels);
        let names: Vec<&str> = c.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(names, ["Disque local (C:)", "Users", "moi"]);
        assert_eq!(c[1].1, PathBuf::from(r"C:\Users"));
    }

    #[test]
    fn crumbs_of_wsl_paths() {
        let names = |p: &str| -> Vec<String> {
            crumbs(Path::new(p), &HashMap::new())
                .into_iter()
                .map(|(l, _)| l)
                .collect()
        };
        assert_eq!(names(r"\\wsl.localhost"), ["Linux"]);
        assert_eq!(names(r"\\wsl.localhost\Ubuntu"), ["Linux", "Ubuntu"]);
        assert_eq!(
            names(r"\\wsl.localhost\Ubuntu\home\moi"),
            ["Linux", "Ubuntu", "home", "moi"]
        );
        let c = crumbs(Path::new(r"\\wsl.localhost\Ubuntu\home"), &HashMap::new());
        assert_eq!(c[0].1, PathBuf::from(r"\\wsl.localhost"));
        assert_eq!(c[1].1, PathBuf::from(r"\\wsl.localhost\Ubuntu"));
        assert_eq!(c[2].1, PathBuf::from(r"\\wsl.localhost\Ubuntu\home"));
    }

    #[test]
    fn crumbs_of_unc_path() {
        let c = crumbs(Path::new(r"\\srv\partage\dossier"), &HashMap::new());
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].0, r"\\srv\partage");
        assert_eq!(c[1].1, PathBuf::from(r"\\srv\partage\dossier"));
    }

    #[test]
    fn suggestions_list_matching_subfolders() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        for n in ["Alpha", "alpine", "Beta"] {
            std::fs::create_dir(dir.path().join(n))?;
        }
        std::fs::write(dir.path().join("alfichier.txt"), b"")?;
        let base = format!("{}\\", dir.path().display());
        let s = suggestions(&format!("{base}al"), 10);
        assert_eq!(s, vec![format!("{base}Alpha"), format!("{base}alpine")]);
        Ok(())
    }
}
