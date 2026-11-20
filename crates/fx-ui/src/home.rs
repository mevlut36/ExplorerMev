//! Page d'accueil : accès rapide, éléments épinglés, fichiers récents.
//!
//! L'accueil est un emplacement virtuel (`HOME_PATH`) qui entre dans l'historique et les onglets
//! comme un dossier. Ce module rassemble ses données (hors du thread d'interface : icônes et
//! raccourcis récents) et les découpe en lignes affichables (logique pure, testée).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fx_win::icons::IconImage;

/// Emplacement virtuel de la page d'accueil.
pub const HOME_PATH: &str = "explorermev:accueil";

pub fn home_path() -> PathBuf {
    PathBuf::from(HOME_PATH)
}

pub fn is_home(path: &Path) -> bool {
    path.to_string_lossy().eq_ignore_ascii_case(HOME_PATH)
}

/// Saisie dans la barre d'adresse qui désigne l'accueil.
pub fn is_home_text(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    t == HOME_PATH || t == "accueil" || t == "home"
}

/// Un élément affiché (carte ou ligne récente).
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// Dossier contenant (ou description) : second texte de la carte.
    pub sub: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub date: String,
    pub icon: Option<IconImage>,
}

#[derive(Clone, Debug, Default)]
pub struct HomeData {
    pub quick: Vec<Entry>,
    pub pinned: Vec<Entry>,
    pub recent: Vec<Entry>,
}

/// Une ligne affichable. `kind` : 0 titre de section, 1 rangée de cartes, 2 fichier récent,
/// 3 remarque (section vide).
#[derive(Clone, Debug)]
pub struct Line {
    pub kind: i32,
    pub title: String,
    pub items: Vec<Entry>,
}

/// Découpe les données en lignes pour `columns` cartes par rangée.
pub fn lines(data: &HomeData, columns: usize) -> Vec<Line> {
    let columns = columns.max(1);
    let mut out = Vec::new();
    let header = |t: &str| Line {
        kind: 0,
        title: t.into(),
        items: Vec::new(),
    };
    let note = |t: &str| Line {
        kind: 3,
        title: t.into(),
        items: Vec::new(),
    };
    let cards = |out: &mut Vec<Line>, list: &[Entry]| {
        for chunk in list.chunks(columns) {
            out.push(Line {
                kind: 1,
                title: String::new(),
                items: chunk.to_vec(),
            });
        }
    };
    out.push(header("Accès rapide"));
    if data.quick.is_empty() {
        out.push(note("Chargement…"));
    } else {
        cards(&mut out, &data.quick);
    }
    out.push(header("Épinglés"));
    if data.pinned.is_empty() {
        out.push(note(
            "Aucun élément épinglé. Cliquez sur l'étoile d'un élément pour l'ajouter ici et au volet de gauche.",
        ));
    } else {
        cards(&mut out, &data.pinned);
    }
    out.push(header("Récents"));
    if data.recent.is_empty() {
        out.push(note("Aucun fichier récent."));
    } else {
        for e in &data.recent {
            out.push(Line {
                kind: 2,
                title: String::new(),
                items: vec![e.clone()],
            });
        }
    }
    out
}

/// Entrées épinglées dans l'ordre des favoris : on réutilise l'entrée déjà connue (avec son icône)
/// quand l'élément figure ailleurs dans l'accueil, sinon une entrée sans icône.
pub fn pinned_entries(data: &HomeData, favorites: &[PathBuf]) -> Vec<Entry> {
    let same = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
    favorites
        .iter()
        .map(|f| {
            data.pinned
                .iter()
                .chain(&data.quick)
                .chain(&data.recent)
                .find(|e| same(&e.path, f))
                .cloned()
                .unwrap_or_else(|| {
                    let (name, sub) = name_and_parent(f);
                    Entry {
                        name,
                        sub,
                        path: f.clone(),
                        is_dir: f.extension().is_none(),
                        date: String::new(),
                        icon: None,
                    }
                })
        })
        .collect()
}

/// Nom court et dossier parent d'un chemin.
pub fn name_and_parent(path: &Path) -> (String, String) {
    let shown = fx_win::path::display_form(path);
    let name = shown
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| shown.display().to_string());
    let parent = shown
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (name, parent)
}

/// Rassemble les données de l'accueil. À appeler hors du thread d'interface (COM, icônes) :
/// `nav` : dossiers connus et lecteurs (libellé, chemin) ; `favorites` : épinglés ; `px` : taille
/// des icônes en pixels physiques.
pub fn collect(nav: Vec<(String, PathBuf)>, favorites: Vec<PathBuf>, px: u32) -> HomeData {
    let _com = fx_win::com::ComApartment::sta().ok();
    let mut cache: HashMap<(Vec<u16>, bool), Option<IconImage>> = HashMap::new();
    let mut icon_of = |path: &Path, is_dir: bool| -> Option<IconImage> {
        let ext = fx_core::folder::extension_key(
            &path
                .file_name()
                .map(|n| std::os::windows::ffi::OsStrExt::encode_wide(n).collect::<Vec<u16>>())
                .unwrap_or_default(),
        );
        cache
            .entry((if is_dir { Vec::new() } else { ext }, is_dir))
            .or_insert_with(|| {
                // Un dossier sans nom donnerait une icône générique : on interroge un nom type.
                let sample: Vec<u16> = if is_dir {
                    "dossier".encode_utf16().collect()
                } else {
                    path.file_name()
                        .map(|n| std::os::windows::ffi::OsStrExt::encode_wide(n).collect())
                        .unwrap_or_default()
                };
                fx_win::icons::type_icon(&sample, is_dir, px)
            })
            .clone()
    };

    let quick = nav
        .into_iter()
        .map(|(label, path)| Entry {
            // Dossiers connus et lecteurs : leur propre icône (Documents, disque…). Pas pour
            // WSL : demander l'icône d'une distribution démarrerait sa machine virtuelle.
            icon: if fx_core::wsl::is_root(path.parent().unwrap_or(&path))
                || path.to_string_lossy().to_lowercase().starts_with(r"\wsl")
            {
                icon_of(&path, true)
            } else {
                fx_win::icons::item_icon(&path, px).or_else(|| icon_of(&path, true))
            },
            sub: name_and_parent(&path).1,
            name: label,
            path,
            is_dir: true,
            date: String::new(),
        })
        .collect();

    let pinned = favorites
        .into_iter()
        .map(|path| {
            // Le type est lu sans ouvrir l'élément (métadonnées du dossier parent).
            let is_dir = match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) => {
                    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(name).collect();
                    !fx_core::wsl::is_root(parent)
                        && fx_win::listing::stat_entry(parent, &wide).is_none_or(|e| e.attributes & 0x10 != 0)
                }
                _ => true,
            };
            let (name, parent) = name_and_parent(&path);
            Entry {
                icon: icon_of(&path, is_dir),
                name,
                sub: parent,
                path,
                is_dir,
                date: String::new(),
            }
        })
        .collect();

    let recent = fx_win::recent::recent_items(40)
        .into_iter()
        .map(|r| {
            let (name, parent) = name_and_parent(&r.target);
            Entry {
                icon: icon_of(&r.target, r.is_dir),
                name,
                sub: parent,
                date: fx_win::locale::format_filetime(r.accessed).unwrap_or_default(),
                path: r.target,
                is_dir: r.is_dir,
            }
        })
        .collect();

    HomeData {
        quick,
        pinned,
        recent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: &str) -> Entry {
        Entry {
            name: n.into(),
            sub: String::new(),
            path: PathBuf::from(n),
            is_dir: false,
            date: String::new(),
            icon: None,
        }
    }

    #[test]
    fn home_path_detection() {
        assert!(is_home(&home_path()));
        assert!(is_home(Path::new("EXPLORERMEV:ACCUEIL")));
        assert!(!is_home(Path::new(r"C:\Users")));
        for t in ["accueil", " Home ", "explorermev:accueil"] {
            assert!(is_home_text(t), "{t}");
        }
        assert!(!is_home_text(r"C:\accueil"));
    }

    #[test]
    fn pinned_entries_follow_favorites_and_reuse_known_entries() {
        let data = HomeData {
            quick: vec![entry("C:\\Users")],
            pinned: Vec::new(),
            recent: vec![entry("D:\\a.txt")],
        };
        let favs = vec![PathBuf::from("d:\\A.TXT"), PathBuf::from("E:\\inconnu")];
        let p = pinned_entries(&data, &favs);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "D:\\a.txt", "entrée connue réutilisée (casse ignorée)");
        assert_eq!(p[1].path, PathBuf::from("E:\\inconnu"));
        assert!(p[1].icon.is_none());
    }

    #[test]
    fn lines_chunk_cards_and_describe_empty_sections() {
        let data = HomeData {
            quick: (0..7).map(|i| entry(&format!("q{i}"))).collect(),
            pinned: Vec::new(),
            recent: vec![entry("a"), entry("b")],
        };
        let l = lines(&data, 3);
        let kinds: Vec<i32> = l.iter().map(|x| x.kind).collect();
        // Accès rapide : titre + 3 rangées (3, 3, 1) ; Épinglés : titre + remarque ; Récents : titre + 2.
        assert_eq!(kinds, [0, 1, 1, 1, 0, 3, 0, 2, 2]);
        assert_eq!(l[1].items.len(), 3);
        assert_eq!(l[3].items.len(), 1);
        assert!(l[5].title.contains("épingl"));
        // Colonnes nulles : au moins une carte par rangée, sans boucle infinie.
        assert_eq!(lines(&data, 0).iter().filter(|x| x.kind == 1).count(), 7);
        // Rien du tout : remarques seulement.
        let empty = lines(&HomeData::default(), 4);
        assert_eq!(
            empty.iter().map(|x| x.kind).collect::<Vec<_>>(),
            [0, 3, 0, 3, 0, 3]
        );
    }
}
