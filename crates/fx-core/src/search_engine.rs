//! Moteur de recherche de l'application : un thread `fx-search` possède les index (un par
//! volume NTFS fixe), répond aux requêtes et applique les changements.
//!
//! - Au démarrage, chaque index est relu depuis son cache (`<cache>\X.idx`) si le volume a le
//!   même numéro de série : la recherche est disponible en quelques dizaines de ms.
//! - Sans cache, ou s'il a plus d'une heure, l'index est reconstruit en arrière-plan (thread
//!   `fx-search-build`) puis remplace l'ancien ; les changements reçus pendant la construction
//!   sont rejoués ensuite.
//! - Une surveillance récursive de chaque volume (`ReadDirectoryChangesW`) garde l'index à
//!   jour ; un débordement de sa file déclenche une reconstruction.
//! - Seule la requête la plus récente est exécutée : les frappes intermédiaires sont abandonnées.

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime};

use fx_win::listing::{file_ref, stat_entry};
use fx_win::watcher::{DirChange, Watcher, watch_tree};

use crate::search::{Query, ROOT_REC, VolumeIndex, record_of, search};
use crate::search_walk::{WalkProgress, build, record_of_path};

/// Un cache plus ancien est rafraîchi par une reconstruction en arrière-plan.
const CACHE_MAX_AGE: Duration = Duration::from_secs(3600);
/// Résultats renvoyés à l'interface.
pub const RESULT_LIMIT: usize = 1000;

/// Ligne de résultat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRow {
    pub name: String,
    /// Chemin complet de l'élément.
    pub path: String,
    pub is_dir: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// État de l'index (affiché sous le champ de recherche).
    Status(String),
    Results {
        generation: u64,
        total: usize,
        rows: Vec<SearchRow>,
        millis: f64,
    },
}

enum Cmd {
    Query {
        text: String,
        generation: u64,
    },
    Changes {
        vol: usize,
        changes: Vec<DirChange>,
    },
    Built {
        vol: usize,
        result: Result<VolumeIndex, String>,
        secs: f64,
    },
}

/// Poignée du moteur : l'arrêter = la détruire (surveillances arrêtées, thread terminé).
pub struct Engine {
    tx: Sender<Cmd>,
    latest: Arc<AtomicU64>,
    _watchers: Vec<Watcher>,
}

impl Engine {
    /// Démarre le moteur pour `volumes`. `on_event` est appelé depuis le thread du moteur.
    pub fn start(
        cache_dir: PathBuf,
        volumes: Vec<char>,
        on_event: impl Fn(EngineEvent) + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel::<Cmd>();
        let latest = Arc::new(AtomicU64::new(0));
        let mut watchers = Vec::new();
        for (vol, letter) in volumes.iter().enumerate() {
            let tx = tx.clone();
            let root = PathBuf::from(format!("{letter}:\\"));
            match watch_tree(&root, move |changes| {
                let _ = tx.send(Cmd::Changes { vol, changes });
            }) {
                Ok(w) => watchers.push(w),
                Err(e) => tracing::warn!("surveillance de {letter}: impossible : {e}"),
            }
        }
        let builder_tx = tx.clone();
        let l = latest.clone();
        let spawned = std::thread::Builder::new()
            .name("fx-search".into())
            .spawn(move || {
                run(cache_dir, volumes, rx, builder_tx, &l, &on_event);
            });
        if let Err(e) = spawned {
            tracing::error!("moteur de recherche non démarré : {e}");
        }
        Self {
            tx,
            latest,
            _watchers: watchers,
        }
    }

    /// Lance une recherche ; renvoie sa génération (les résultats la portent).
    pub fn query(&self, text: &str) -> u64 {
        let generation = self.latest.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.tx.send(Cmd::Query {
            text: text.to_string(),
            generation,
        });
        generation
    }
}

struct Vol {
    letter: char,
    root: PathBuf,
    serial: u32,
    index: Option<VolumeIndex>,
    building: bool,
    /// Changements reçus pendant une construction, rejoués ensuite.
    pending: Vec<DirChange>,
    dirty: bool,
}

fn cache_path(dir: &Path, letter: char) -> PathBuf {
    dir.join(format!("{letter}.idx"))
}

fn load_cache(dir: &Path, letter: char, serial: u32) -> Option<(VolumeIndex, SystemTime)> {
    let path = cache_path(dir, letter);
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let file = std::fs::File::open(&path).ok()?;
    match VolumeIndex::read_from(&mut std::io::BufReader::new(file)) {
        Ok(ix) if ix.letter() == letter && ix.meta.serial == serial => Some((ix, modified)),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("cache de l'index {letter}: ignoré : {e}");
            None
        }
    }
}

fn save_cache(dir: &Path, ix: &mut VolumeIndex) {
    let path = cache_path(dir, ix.letter());
    let tmp = path.with_extension("tmp");
    let r = std::fs::create_dir_all(dir).and_then(|()| {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        ix.write_to(&mut f)?;
        std::io::Write::flush(&mut f)?;
        drop(f);
        std::fs::rename(&tmp, &path)
    });
    if let Err(e) = r {
        tracing::warn!("cache de l'index {}: non écrit : {e}", ix.letter());
        let _ = std::fs::remove_file(&tmp);
    }
}

fn status(vols: &[Vol]) -> String {
    let parts: Vec<String> = vols
        .iter()
        .map(|v| match (&v.index, v.building) {
            (Some(ix), true) => format!(
                "{}: {} éléments (mise à jour…)",
                v.letter,
                crate::format::group_thousands(ix.len() as u64)
            ),
            (Some(ix), false) => format!(
                "{}: {} éléments",
                v.letter,
                crate::format::group_thousands(ix.len() as u64)
            ),
            (None, true) => format!("{}: indexation…", v.letter),
            (None, false) => format!("{}: indisponible", v.letter),
        })
        .collect();
    format!("Index : {}", parts.join(" · "))
}

fn run(
    cache_dir: PathBuf,
    letters: Vec<char>,
    rx: Receiver<Cmd>,
    tx: Sender<Cmd>,
    latest: &AtomicU64,
    on_event: &dyn Fn(EngineEvent),
) {
    let mut vols: Vec<Vol> = Vec::new();
    for &letter in &letters {
        let root = PathBuf::from(format!("{letter}:\\"));
        let Some((_, serial)) = file_ref(&root) else {
            continue;
        };
        let t = Instant::now();
        let cached = load_cache(&cache_dir, letter, serial);
        let fresh = cached
            .as_ref()
            .is_some_and(|(_, m)| m.elapsed().map(|a| a < CACHE_MAX_AGE).unwrap_or(false));
        if let Some((ix, _)) = &cached {
            tracing::info!(volume = %letter, elements = ix.len(), ms = t.elapsed().as_secs_f64() * 1000.0, "index relu depuis le cache");
        }
        vols.push(Vol {
            letter,
            root,
            serial,
            index: cached.map(|(ix, _)| ix),
            building: !fresh,
            pending: Vec::new(),
            dirty: false,
        });
    }
    on_event(EngineEvent::Status(status(&vols)));
    // Reconstructions, une à la fois (le disque est la ressource limitante).
    let to_build: Vec<(usize, char)> = vols
        .iter()
        .enumerate()
        .filter(|(_, v)| v.building)
        .map(|(i, v)| (i, v.letter))
        .collect();
    spawn_builds(to_build, tx.clone());

    let mut next: Option<Cmd> = None;
    loop {
        let cmd = match next.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        match cmd {
            Cmd::Query { text, generation } => {
                // Changements et constructions en attente d'abord ; seule la dernière requête
                // compte.
                let (mut text, mut generation) = (text, generation);
                loop {
                    match rx.try_recv() {
                        Ok(Cmd::Query {
                            text: t,
                            generation: g,
                        }) => (text, generation) = (t, g),
                        Ok(other) => handle(other, &mut vols, &cache_dir, &tx, on_event),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                if generation != latest.load(Ordering::SeqCst) {
                    continue;
                }
                run_query(&vols, &text, generation, latest, on_event);
            }
            other => {
                handle(other, &mut vols, &cache_dir, &tx, on_event);
                // Les changements arrivent par rafales : on les traite d'un bloc.
                if let Ok(c) = rx.try_recv() {
                    next = Some(c);
                }
            }
        }
    }
}

fn run_query(vols: &[Vol], text: &str, generation: u64, latest: &AtomicU64, on_event: &dyn Fn(EngineEvent)) {
    let t = Instant::now();
    let Some(q) = Query::parse(text) else {
        on_event(EngineEvent::Results {
            generation,
            total: 0,
            rows: Vec::new(),
            millis: 0.0,
        });
        return;
    };
    let indexes: Vec<&VolumeIndex> = vols.iter().filter_map(|v| v.index.as_ref()).collect();
    let cancelled = || latest.load(Ordering::Relaxed) != generation;
    let Some(r) = search(&indexes, &q, RESULT_LIMIT, &cancelled) else {
        return;
    };
    let rows: Vec<SearchRow> = r
        .hits
        .iter()
        .filter_map(|h| {
            let ix = indexes.get(h.vol as usize)?;
            let i = h.item as usize;
            Some(SearchRow {
                name: ix.name(i).to_string(),
                path: ix.path(i)?,
                is_dir: ix.is_dir(i),
                hidden: ix.flags(i) & (crate::search::flag::HIDDEN | crate::search::flag::SYSTEM) != 0,
            })
        })
        .collect();
    on_event(EngineEvent::Results {
        generation,
        total: r.total,
        rows,
        millis: t.elapsed().as_secs_f64() * 1000.0,
    });
}

fn spawn_builds(list: Vec<(usize, char)>, tx: Sender<Cmd>) {
    if list.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("fx-search-build".into())
        .spawn(move || {
            for (vol, letter) in list {
                let t = Instant::now();
                let p = WalkProgress::default();
                let result = build(letter, &p);
                if tx
                    .send(Cmd::Built {
                        vol,
                        result,
                        secs: t.elapsed().as_secs_f64(),
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!("construction de l'index non démarrée : {e}");
    }
}

fn handle(cmd: Cmd, vols: &mut [Vol], cache_dir: &Path, tx: &Sender<Cmd>, on_event: &dyn Fn(EngineEvent)) {
    match cmd {
        Cmd::Query { .. } => {}
        Cmd::Changes { vol, changes } => {
            let Some(v) = vols.get_mut(vol) else { return };
            if v.building || v.index.is_none() {
                v.pending.extend(changes);
                return;
            }
            let overflow = match v.index.as_mut() {
                Some(ix) => apply(ix, &v.root, v.serial, changes),
                None => false,
            };
            v.dirty = true;
            if overflow {
                tracing::info!(volume = %v.letter, "trop de changements : reconstruction de l'index");
                v.building = true;
                spawn_builds(vec![(vol, v.letter)], tx.clone());
                on_event(EngineEvent::Status(status(vols)));
            }
        }
        Cmd::Built { vol, result, secs } => {
            let Some(v) = vols.get_mut(vol) else { return };
            v.building = false;
            match result {
                Ok(mut ix) => {
                    tracing::info!(volume = %v.letter, elements = ix.len(), secs, "index construit");
                    let pending = std::mem::take(&mut v.pending);
                    apply(&mut ix, &v.root, v.serial, pending);
                    save_cache(cache_dir, &mut ix);
                    v.index = Some(ix);
                    v.dirty = false;
                }
                Err(e) => tracing::warn!("index {}: {e}", v.letter),
            }
            on_event(EngineEvent::Status(status(vols)));
        }
    }
}

/// Applique des changements signalés par la surveillance. Renvoie `true` s'il faut tout
/// reconstruire (file débordée).
fn apply(ix: &mut VolumeIndex, root: &Path, serial: u32, changes: Vec<DirChange>) -> bool {
    // Suppressions : l'élément et, pour un dossier, ses descendants.
    let mut removed: HashSet<(u32, String)> = HashSet::new();
    // Ancien nom d'un renommage : l'élément seul. Le nouveau nom le réinsère avec le même
    // enregistrement ; ses descendants, qui le désignent par ce numéro, le suivent.
    let mut renamed: HashSet<(u32, String)> = HashSet::new();
    let mut touched: Vec<Vec<u16>> = Vec::new();
    for ch in changes {
        let (rel, set) = match ch {
            DirChange::Overflow => return true,
            DirChange::Gone => continue,
            DirChange::Removed(rel) => (rel, &mut removed),
            DirChange::RenamedFrom(rel) => (rel, &mut renamed),
            DirChange::Added(rel) | DirChange::RenamedTo(rel) | DirChange::Modified(rel) => {
                touched.push(rel);
                continue;
            }
        };
        let full = root.join(OsString::from_wide(&rel));
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            continue;
        };
        // Dossier parent supprimé lui aussi : son propre retrait suffit.
        if let Some(p) = record_of_path(parent, serial) {
            set.insert((p, name.to_string_lossy().to_lowercase()));
        }
    }
    let dirs = ix.remove_named(&removed);
    ix.remove_under(&dirs.into_iter().collect());
    ix.remove_named(&renamed);
    for rel in touched {
        let full = root.join(OsString::from_wide(&rel));
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            continue;
        };
        let name: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(name).collect();
        let Some(entry) = stat_entry(parent, &name) else {
            continue;
        };
        let (Some((r, s)), Some(p)) = (file_ref(&full), record_of_path(parent, serial)) else {
            continue;
        };
        let Some(rec) = record_of(r).filter(|_| s == serial && r != 0) else {
            continue;
        };
        let parent_rec = if parent.parent().is_none() { ROOT_REC } else { p };
        ix.upsert(rec, parent_rec, &entry.name, entry.attributes);
    }
    ix.maybe_compact();
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Index ne contenant que la chaîne des dossiers de `dir` (depuis la racine du volume).
    fn chain_index(dir: &Path) -> Result<(VolumeIndex, PathBuf, u32), String> {
        let letter = dir
            .to_str()
            .and_then(|s| s.chars().next())
            .ok_or("lettre")?
            .to_ascii_uppercase();
        let root = PathBuf::from(format!("{letter}:\\"));
        let (_, serial) = file_ref(&root).ok_or_else(|| format!("racine de {}", dir.display()))?;
        let mut ix = VolumeIndex::new(letter);
        let mut parent = ROOT_REC;
        let mut cur = root.clone();
        let tail = dir
            .strip_prefix(&root)
            .map_err(|e| format!("{e} : {}", dir.display()))?;
        for comp in tail.components() {
            cur.push(comp);
            let rec =
                record_of_path(&cur, serial).ok_or_else(|| format!("référence de {}", cur.display()))?;
            let name: Vec<u16> = comp.as_os_str().encode_wide().collect();
            ix.upsert(rec, parent, &name, 0x10);
            parent = rec;
        }
        Ok((ix, root, serial))
    }

    fn found(ix: &VolumeIndex, text: &str) -> Vec<String> {
        let q = Query::parse(text).expect("requête");
        let r = search(&[ix], &q, 50, &|| false).expect("non annulée");
        r.hits.iter().filter_map(|h| ix.path(h.item as usize)).collect()
    }

    fn rel(root: &Path, p: &Path) -> Vec<u16> {
        p.strip_prefix(root)
            .expect("sous la racine")
            .as_os_str()
            .encode_wide()
            .collect()
    }

    #[test]
    fn live_changes_update_the_index() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let dir = std::fs::canonicalize(tmp.path())?;
        let dir = PathBuf::from(dir.to_string_lossy().trim_start_matches(r"\\?\"));
        let (mut ix, root, serial) = chain_index(&dir)?;

        let sub = dir.join("Projet Zéphyr");
        std::fs::create_dir(&sub)?;
        let file = sub.join("notes-zephyrine.txt");
        std::fs::write(&file, b"x")?;
        apply(
            &mut ix,
            &root,
            serial,
            vec![
                DirChange::Added(rel(&root, &sub)),
                DirChange::Added(rel(&root, &file)),
            ],
        );
        assert_eq!(found(&ix, "zephyrine"), [file.display().to_string()]);

        // Renommage du dossier : le chemin du fichier suit sans autre événement.
        let renamed = dir.join("Archive Zéphyr");
        std::fs::rename(&sub, &renamed)?;
        apply(
            &mut ix,
            &root,
            serial,
            vec![
                DirChange::RenamedFrom(rel(&root, &sub)),
                DirChange::RenamedTo(rel(&root, &renamed)),
            ],
        );
        assert_eq!(
            found(&ix, "zephyrine"),
            [renamed.join("notes-zephyrine.txt").display().to_string()]
        );
        assert_eq!(found(&ix, "projet zéphyr"), Vec::<String>::new());

        // Suppression du dossier : ses descendants disparaissent aussi.
        std::fs::remove_dir_all(&renamed)?;
        apply(
            &mut ix,
            &root,
            serial,
            vec![DirChange::Removed(rel(&root, &renamed))],
        );
        assert!(found(&ix, "zéphyr").is_empty());
        assert!(found(&ix, "zephyrine").is_empty());
        let _ = wide("");
        Ok(())
    }
}
