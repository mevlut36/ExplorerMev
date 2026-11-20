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

use crate::mft_proto::{Frame, Rec, read_frame};
use crate::search::{IndexMeta, Query, ROOT_REC, VolumeIndex, record_of, search};
use crate::search_walk::{WalkProgress, build, record_of_path};

/// Un cache plus ancien est rafraîchi par une reconstruction en arrière-plan (les
/// changements faits pendant que l'application était fermée n'y sont pas).
const CACHE_MAX_AGE: Duration = Duration::from_secs(600);
/// Sans recherche pendant ce délai, les index sont libérés (relus depuis le cache à la
/// recherche suivante) : la mémoire revient à son niveau de repos.
pub const UNLOAD_AFTER: Duration = Duration::from_secs(300);
/// Au-delà, les changements notés pendant que l'index est libéré sont abandonnés et l'index
/// sera reconstruit.
const MAX_PENDING: usize = 200_000;
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
    /// Libérer les index tout de suite (mesures).
    Release,
    /// La recherche vient d'être ouverte : relire les index libérés avant la première frappe.
    Warm,
    /// Trame de l'assistant d'indexation élevé (MFT, journal USN).
    Mft(Frame),
    /// L'assistant s'est arrêté (ou n'a pas pu démarrer).
    MftEnded(String),
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
    letters: Vec<char>,
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
        let letters = volumes.clone();
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
            letters,
            _watchers: watchers,
        }
    }

    /// Lance l'assistant d'indexation **élevé** (`exe --index-helper`, invite UAC) : la MFT
    /// remplace le parcours des dossiers et le journal USN, la surveillance. À n'appeler
    /// qu'à la demande de l'utilisateur.
    pub fn start_mft(&self, exe: PathBuf) {
        let tx = self.tx.clone();
        let letters: String = self.letters.iter().collect();
        let spawned = std::thread::Builder::new().name("fx-mft".into()).spawn(move || {
            let reason = match mft_session(&exe, &letters, &tx) {
                Ok(()) => "assistant d'indexation arrêté".to_string(),
                Err(e) => e,
            };
            let _ = tx.send(Cmd::MftEnded(reason));
        });
        if let Err(e) = spawned {
            tracing::error!("assistant d'indexation non démarré : {e}");
        }
    }

    /// Prépare les index (ouverture de la recherche) : relit ceux qui ont été libérés.
    pub fn warm(&self) {
        let _ = self.tx.send(Cmd::Warm);
    }

    /// Libère les index immédiatement (comme après `UNLOAD_AFTER` sans recherche).
    pub fn release(&self) {
        let _ = self.tx.send(Cmd::Release);
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
    /// Changements reçus pendant une construction ou pendant que l'index est libéré, rejoués
    /// ensuite.
    pending: Vec<DirChange>,
    /// Trop de changements notés : reconstruire au lieu de rejouer.
    stale: bool,
    /// Modifié depuis la dernière écriture du cache.
    dirty: bool,
    /// Libéré de la mémoire (relu depuis le cache à la prochaine recherche).
    unloaded: bool,
    /// Tenu à jour par le journal USN (assistant élevé) plutôt que par la surveillance.
    mft: bool,
    /// Index en cours de construction depuis la MFT.
    mft_build: Option<VolumeIndex>,
    /// Enregistrements du journal reçus pendant que l'index est libéré.
    mft_pending: Vec<Rec>,
}

/// Crée le tube, lance l'assistant élevé, vérifie que c'est bien lui qui se connecte, puis
/// relaie ses trames au moteur.
fn mft_session(exe: &Path, letters: &str, tx: &Sender<Cmd>) -> Result<(), String> {
    use fx_win::pipe::{PipeServer, launch_elevated, random_pipe_name};
    let name = random_pipe_name();
    let server = PipeServer::create(&name).map_err(|e| format!("tube : {e}"))?;
    let args = format!("--index-helper {name} {} {letters}", std::process::id());
    let child = launch_elevated(exe, &args).map_err(|e| format!("élévation refusée ou impossible : {e}"))?;
    server
        .accept(child.pid, child.handle(), Duration::from_secs(120))
        .map_err(|e| format!("assistant : {e}"))?;
    tracing::info!(pid = child.pid, "assistant d'indexation connecté");
    loop {
        match read_frame(&mut &server) {
            Ok(Some(f)) => {
                if tx.send(Cmd::Mft(f)).is_err() {
                    return Ok(());
                }
            }
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("assistant : {e}")),
        }
    }
}

/// Applique un enregistrement de la MFT ou du journal USN.
fn apply_mft(ix: &mut VolumeIndex, r: &Rec) {
    const FILE_DELETE: u32 = 0x0000_0200;
    const RENAME_OLD_NAME: u32 = 0x0000_1000;
    const RENAME_NEW_NAME: u32 = 0x0000_2000;
    let Some(rec) = record_of(r.frn) else { return };
    // Enregistrements réservés de NTFS ($MFT, $LogFile…) et racine : jamais indexés.
    if rec < 24 {
        return;
    }
    let Some(parent) = record_of(r.parent) else { return };
    if r.reason & FILE_DELETE != 0 {
        ix.remove(rec);
    } else if r.reason & RENAME_OLD_NAME != 0 && r.reason & RENAME_NEW_NAME == 0 {
        // Ancien nom : le nouveau suit dans un autre enregistrement.
    } else {
        ix.upsert(
            rec,
            if parent == ROOT_REC { ROOT_REC } else { parent },
            &r.name,
            r.attributes,
        );
    }
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
        .map(|v| match (&v.index, v.building || v.mft_build.is_some()) {
            (Some(ix), false) if v.mft => format!(
                "{}: {} éléments (MFT)",
                v.letter,
                crate::format::group_thousands(ix.len() as u64)
            ),
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
            (None, false) if v.unloaded => format!("{}: en cache", v.letter),
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
            stale: false,
            dirty: false,
            unloaded: false,
            mft: false,
            mft_build: None,
            mft_pending: Vec::new(),
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
    let mut last_query = Instant::now();
    loop {
        let cmd = match next.take() {
            Some(c) => c,
            None => match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(c) => c,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if last_query.elapsed() >= UNLOAD_AFTER {
                        unload(&mut vols, &cache_dir);
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
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
                last_query = Instant::now();
                if vols.iter().any(|v| v.unloaded) {
                    reload(&mut vols, &cache_dir, &tx);
                    on_event(EngineEvent::Status(status(&vols)));
                }
                run_query(&vols, &text, generation, latest, on_event);
            }
            Cmd::Release => {
                unload(&mut vols, &cache_dir);
                on_event(EngineEvent::Status(status(&vols)));
            }
            Cmd::Warm => {
                last_query = Instant::now();
                if vols.iter().any(|v| v.unloaded) {
                    reload(&mut vols, &cache_dir, &tx);
                    on_event(EngineEvent::Status(status(&vols)));
                }
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

/// Libère les index inactifs (cache écrit d'abord s'ils ont changé).
fn unload(vols: &mut [Vol], cache_dir: &Path) {
    for v in vols.iter_mut() {
        if v.building || v.unloaded {
            continue;
        }
        let mem = || {
            fx_win::process::memory()
                .map(|m| m.private / 1_048_576)
                .unwrap_or(0)
        };
        let m0 = mem();
        let Some(mut ix) = v.index.take() else { continue };
        if v.dirty {
            save_cache(cache_dir, &mut ix);
            v.dirty = false;
        }
        let m1 = mem();
        drop(ix);
        v.unloaded = true;
        tracing::info!(volume = %v.letter, avant = m0, apres_cache = m1, apres = mem(), "index libéré (inactif)");
    }
}

/// Relit les index libérés et rejoue les changements notés entre-temps.
fn reload(vols: &mut [Vol], cache_dir: &Path, tx: &Sender<Cmd>) {
    for (i, v) in vols.iter_mut().enumerate() {
        if !v.unloaded {
            continue;
        }
        v.unloaded = false;
        match load_cache(cache_dir, v.letter, v.serial) {
            Some((mut ix, _)) if !v.stale => {
                for r in std::mem::take(&mut v.mft_pending) {
                    apply_mft(&mut ix, &r);
                }
                let pending = std::mem::take(&mut v.pending);
                v.dirty = !pending.is_empty();
                let overflow = apply(&mut ix, &v.root, v.serial, pending);
                v.index = Some(ix);
                if overflow {
                    v.building = true;
                    spawn_builds(vec![(i, v.letter)], tx.clone());
                }
            }
            _ => {
                v.pending.clear();
                v.stale = false;
                v.building = true;
                spawn_builds(vec![(i, v.letter)], tx.clone());
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
        Cmd::Release => unload(vols, cache_dir),
        Cmd::Warm => {}
        Cmd::Changes { vol, changes } => {
            let Some(v) = vols.get_mut(vol) else { return };
            // Volume suivi par le journal USN : la surveillance est redondante.
            if v.mft {
                return;
            }
            if v.building || v.index.is_none() {
                if v.pending.len() + changes.len() > MAX_PENDING {
                    v.pending.clear();
                    v.stale = true;
                } else if !v.stale {
                    v.pending.extend(changes);
                }
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
            if v.mft || v.mft_build.is_some() {
                return; // l'index de la MFT est plus complet et déjà à jour
            }
            match result {
                Ok(mut ix) => {
                    tracing::info!(volume = %v.letter, elements = ix.len(), secs, "index construit");
                    let pending = std::mem::take(&mut v.pending);
                    v.stale = false;
                    apply(&mut ix, &v.root, v.serial, pending);
                    save_cache(cache_dir, &mut ix);
                    v.index = Some(ix);
                    v.dirty = false;
                }
                Err(e) => tracing::warn!("index {}: {e}", v.letter),
            }
            on_event(EngineEvent::Status(status(vols)));
        }
        Cmd::Mft(frame) => handle_mft(frame, vols, cache_dir, on_event),
        Cmd::MftEnded(reason) => {
            tracing::info!("{reason}");
            for v in vols.iter_mut() {
                v.mft = false;
                v.mft_build = None;
            }
            on_event(EngineEvent::Status(format!("{} · {reason}", status(vols))));
        }
    }
}

fn handle_mft(frame: Frame, vols: &mut [Vol], cache_dir: &Path, on_event: &dyn Fn(EngineEvent)) {
    let letter = match &frame {
        Frame::Begin { letter, .. }
        | Frame::Records { letter, .. }
        | Frame::EndEnum { letter }
        | Frame::Changes { letter, .. }
        | Frame::Error { letter, .. } => *letter,
    };
    let Some(v) = vols.iter_mut().find(|v| v.letter == letter) else {
        return;
    };
    match frame {
        Frame::Begin {
            serial,
            journal_id,
            next_usn,
            ..
        } => {
            if serial != 0 && serial != v.serial {
                tracing::warn!(volume = %letter, "MFT d'un autre volume : ignorée");
                return;
            }
            let mut ix = VolumeIndex::new(letter);
            ix.meta = IndexMeta {
                serial: v.serial,
                source: 1,
                journal_id,
                next_usn,
            };
            v.mft_build = Some(ix);
            on_event(EngineEvent::Status(status(vols)));
        }
        Frame::Records { records, .. } => {
            if let Some(ix) = v.mft_build.as_mut() {
                for r in &records {
                    apply_mft(ix, r);
                }
            }
        }
        Frame::EndEnum { .. } => {
            let Some(mut ix) = v.mft_build.take() else { return };
            ix.shrink();
            tracing::info!(volume = %letter, elements = ix.len(), "index construit depuis la MFT");
            save_cache(cache_dir, &mut ix);
            v.index = Some(ix);
            v.unloaded = false;
            v.mft = true;
            v.dirty = false;
            v.pending.clear();
            v.stale = false;
            on_event(EngineEvent::Status(status(vols)));
        }
        Frame::Changes {
            next_usn, records, ..
        } => {
            if !v.mft {
                return;
            }
            match v.index.as_mut() {
                Some(ix) => {
                    for r in &records {
                        apply_mft(ix, r);
                    }
                    ix.meta.next_usn = next_usn;
                    ix.maybe_compact();
                    v.dirty = true;
                }
                None if v.mft_pending.len() + records.len() <= MAX_PENDING => v.mft_pending.extend(records),
                None => {
                    // Trop de changements pendant que l'index était libéré : la surveillance
                    // reprend et l'index sera reconstruit à la relecture.
                    v.mft_pending.clear();
                    v.mft = false;
                    v.stale = true;
                }
            }
        }
        Frame::Error { message, .. } => {
            tracing::warn!(volume = %letter, "MFT illisible : {message}");
            v.mft_build = None;
            on_event(EngineEvent::Status(format!(
                "{} · {letter}: MFT illisible ({message})",
                status(vols)
            )));
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
        let Some((r, s)) = file_ref(&full) else {
            continue;
        };
        let Some(rec) = record_of(r).filter(|_| s == serial && r != 0) else {
            continue;
        };
        let Some(parent_rec) = ensure_dir(ix, parent, serial) else {
            continue;
        };
        ix.upsert(rec, parent_rec, &entry.name, entry.attributes);
    }
    ix.maybe_compact();
    false
}

/// Enregistrement du dossier `dir`, en l'indexant avec ses ancêtres manquants (dossiers créés
/// pendant que l'application était fermée, ou avant le démarrage de la surveillance).
fn ensure_dir(ix: &mut VolumeIndex, dir: &Path, serial: u32) -> Option<u32> {
    let Some(up) = dir.parent() else {
        return Some(ROOT_REC);
    };
    let rec = record_of_path(dir, serial)?;
    if ix.contains(rec) {
        return Some(rec);
    }
    let parent = ensure_dir(ix, up, serial)?;
    let name: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(dir.file_name()?).collect();
    let entry = stat_entry(up, &name)?;
    ix.upsert(rec, parent, &entry.name, entry.attributes);
    Some(rec)
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
    fn mft_and_journal_records_build_and_update_the_index() {
        let rec = |frn: u64, parent: u64, reason: u32, name: &str, attrs: u32| Rec {
            frn: frn | (3 << 48), // numéro de séquence ignoré
            parent: parent | (1 << 48),
            attributes: attrs,
            reason,
            name: name.encode_utf16().collect(),
        };
        let mut ix = VolumeIndex::new('C');
        for r in [
            rec(5, 5, 0, ".", 0x10),    // racine : ignorée
            rec(9, 5, 0, "$Secure", 0), // réservé : ignoré
            rec(100, 5, 0, "Projets", 0x10),
            rec(101, 100, 0, "plan.txt", 0x20),
        ] {
            apply_mft(&mut ix, &r);
        }
        assert_eq!(ix.len(), 2);
        assert_eq!(found(&ix, "plan"), [r"C:\Projets\plan.txt"]);
        // Journal : renommage (ancien nom ignoré, nouveau appliqué), création, suppression.
        apply_mft(&mut ix, &rec(100, 5, 0x1000, "Projets", 0x10));
        apply_mft(&mut ix, &rec(100, 5, 0x2000, "Archives", 0x10));
        assert_eq!(found(&ix, "plan"), [r"C:\Archives\plan.txt"]);
        apply_mft(&mut ix, &rec(102, 100, 0x100, "neuf.md", 0x20));
        assert_eq!(found(&ix, "neuf"), [r"C:\Archives\neuf.md"]);
        apply_mft(&mut ix, &rec(101, 100, 0x200, "plan.txt", 0x20));
        assert!(found(&ix, "plan").is_empty());
    }

    #[test]
    fn live_changes_update_the_index() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let dir = std::fs::canonicalize(tmp.path())?;
        let dir = PathBuf::from(dir.to_string_lossy().trim_start_matches(r"\\?\"));
        // Le dossier temporaire lui-même n'est pas indexé (comme un dossier créé pendant que
        // l'application était fermée) : il est ajouté avec son premier contenu.
        let (_, root, serial) = chain_index(&dir)?;
        let (mut ix, _, _) = chain_index(dir.parent().ok_or("parent")?)?;
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
