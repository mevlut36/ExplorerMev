//! Chargement d'un dossier en arrière-plan : lots incrémentaux, tri, annulation, générations.
//!
//! Chaque chargement porte une **génération** choisie par l'appelant. Lancer un nouveau
//! chargement annule le précédent ; l'appelant ignore de son côté tout événement dont la
//! génération n'est plus la sienne.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::collate::Collator;
use crate::filter::Filter;
use crate::folder::{Folder, RawEntry};
use crate::sort::{NameKeys, SortSpec, sorted_order_with_keys};
use crate::source::{DirSource, ErrorKind, SourceError};

/// Nombre d'entrées au-delà duquel un premier lot est publié.
pub const FIRST_BATCH: usize = 1000;
/// Délai au-delà duquel un premier lot est publié même s'il est incomplet.
pub const FIRST_BATCH_DELAY: Duration = Duration::from_millis(40);
/// Intervalle entre deux lots intermédiaires pour les dossiers lents (réseau).
pub const PARTIAL_INTERVAL: Duration = Duration::from_millis(300);

/// Statistiques d'un chargement terminé.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoadStats {
    pub entries: usize,
    pub list: Duration,
    pub sort: Duration,
}

/// Événements émis par le chargeur, depuis un thread de travail.
pub enum LoadEvent {
    /// Contenu partiel (trié) pour un affichage immédiat.
    Partial {
        generation: u64,
        folder: Arc<Folder>,
        order: Vec<u32>,
        elapsed: Duration,
    },
    /// Contenu complet trié.
    Complete {
        generation: u64,
        folder: Arc<Folder>,
        keys: Arc<NameKeys>,
        order: Vec<u32>,
        stats: LoadStats,
    },
    Failed {
        generation: u64,
        error: SourceError,
    },
}

pub type Sink = Arc<dyn Fn(LoadEvent) + Send + Sync>;

/// Résultat d'un recalcul de vue (tri et/ou filtre).
pub struct ViewResult {
    pub generation: u64,
    pub seq: u64,
    pub spec: SortSpec,
    /// Nouvel ordre complet ; `None` s'il n'a pas changé (filtre seul).
    pub order: Option<Vec<u32>>,
    pub visible: Vec<u32>,
}

type TypeCache = Arc<Mutex<HashMap<(Vec<u16>, bool), Arc<str>>>>;

pub struct Loader {
    source: Arc<dyn DirSource>,
    collator: Arc<dyn Collator>,
    types: TypeCache,
    current: Mutex<Option<Arc<AtomicBool>>>,
}

impl Loader {
    pub fn new(source: Arc<dyn DirSource>, collator: Arc<dyn Collator>) -> Self {
        Self {
            source,
            collator,
            types: Arc::default(),
            current: Mutex::new(None),
        }
    }

    pub fn collator(&self) -> Arc<dyn Collator> {
        self.collator.clone()
    }

    /// Annule le chargement en cours, s'il y en a un.
    pub fn cancel(&self) {
        if let Ok(mut cur) = self.current.lock()
            && let Some(token) = cur.take()
        {
            token.store(true, Ordering::Relaxed);
        }
    }

    /// Lance le chargement de `path` sur un thread dédié. Annule le chargement précédent.
    pub fn load(&self, path: PathBuf, generation: u64, sort: SortSpec, sink: Sink) {
        let token = Arc::new(AtomicBool::new(false));
        if let Ok(mut cur) = self.current.lock()
            && let Some(old) = cur.replace(token.clone())
        {
            old.store(true, Ordering::Relaxed);
        }
        let job = Job {
            source: self.source.clone(),
            collator: self.collator.clone(),
            types: self.types.clone(),
            token,
            path,
            generation,
            sort,
        };
        let sink2 = sink.clone();
        let spawned = std::thread::Builder::new().name("fx-list".into()).spawn(move || {
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| job.run(&sink2)));
            if r.is_err() {
                tracing::error!("panic dans le thread de listing");
                sink2(LoadEvent::Failed {
                    generation,
                    error: SourceError::new(ErrorKind::Other, "erreur interne pendant le listing"),
                });
            }
        });
        if let Err(e) = spawned {
            sink(LoadEvent::Failed {
                generation,
                error: SourceError::new(ErrorKind::Other, e.to_string()),
            });
        }
    }

    /// Recalcule l'ordre (tri) et la vue filtrée sur le pool `rayon`, sans bloquer l'appelant.
    #[allow(clippy::too_many_arguments)]
    pub fn compute_view(
        &self,
        folder: Arc<Folder>,
        keys: Arc<NameKeys>,
        spec: SortSpec,
        filter: Filter,
        generation: u64,
        seq: u64,
        done: impl FnOnce(ViewResult) + Send + 'static,
    ) {
        let collator = self.collator.clone();
        rayon::spawn(move || {
            let order = sorted_order_with_keys(&folder, spec, &keys, &*collator);
            let visible = filter.apply(&folder, &order);
            done(ViewResult {
                generation,
                seq,
                spec,
                order: Some(order),
                visible,
            });
        });
    }

    /// Recalcule seulement la vue filtrée (l'ordre est inchangé).
    #[allow(clippy::too_many_arguments)]
    pub fn filter_view(
        &self,
        folder: Arc<Folder>,
        order: Arc<Vec<u32>>,
        spec: SortSpec,
        filter: Filter,
        generation: u64,
        seq: u64,
        done: impl FnOnce(ViewResult) + Send + 'static,
    ) {
        rayon::spawn(move || {
            let visible = filter.apply(&folder, &order);
            done(ViewResult {
                generation,
                seq,
                spec,
                order: None,
                visible,
            });
        });
    }
}

struct Job {
    source: Arc<dyn DirSource>,
    collator: Arc<dyn Collator>,
    types: TypeCache,
    token: Arc<AtomicBool>,
    path: PathBuf,
    generation: u64,
    sort: SortSpec,
}

impl Job {
    fn run(self, sink: &Sink) {
        let _thread_state = self.source.init_worker_thread();
        let start = Instant::now();
        let generation = self.generation;
        let mut folder = Folder::with_capacity(self.path.clone(), 256);
        let mut dir_type: Option<u32> = None;
        let mut file_types: HashMap<Vec<u16>, u32> = HashMap::new();
        let mut ext = Vec::<u16>::with_capacity(16);
        let mut published = false;
        let mut last_publish = start;
        let token = self.token.clone();
        let cancelled = move || token.load(Ordering::Relaxed);

        let result = self
            .source
            .list(&self.path, &cancelled, &mut |raw: &RawEntry<'_>| {
                let is_dir = raw.attributes & crate::folder::ATTR_DIRECTORY != 0;
                let type_id = if is_dir {
                    match dir_type {
                        Some(id) => id,
                        None => {
                            let id = folder.intern_type(self.type_label(raw.name, true));
                            dir_type = Some(id);
                            id
                        }
                    }
                } else {
                    // Pas d'allocation par entrée : `ext` est réutilisé et la table est interrogée
                    // par tranche empruntée.
                    ext.clear();
                    if let Some(dot) = raw.name.iter().rposition(|&c| c == b'.' as u16) {
                        lower_into(&raw.name[dot + 1..], &mut ext);
                    }
                    match file_types.get(ext.as_slice()) {
                        Some(&id) => id,
                        None => {
                            let id = folder.intern_type(self.type_label(raw.name, false));
                            file_types.insert(ext.clone(), id);
                            id
                        }
                    }
                };
                folder.push(raw, type_id);

                let now = Instant::now();
                let due = if published {
                    now - last_publish >= PARTIAL_INTERVAL
                } else {
                    folder.len() >= FIRST_BATCH || now - start >= FIRST_BATCH_DELAY
                };
                if due {
                    published = true;
                    last_publish = now;
                    let snapshot = Arc::new(folder.clone());
                    let keys = NameKeys::build(&snapshot, &*self.collator);
                    let order = sorted_order_with_keys(&snapshot, self.sort, &keys, &*self.collator);
                    sink(LoadEvent::Partial {
                        generation,
                        folder: snapshot,
                        order,
                        elapsed: start.elapsed(),
                    });
                }
            });

        if let Err(error) = result {
            if error.kind != ErrorKind::Cancelled {
                sink(LoadEvent::Failed { generation, error });
            }
            return;
        }
        if self.token.load(Ordering::Relaxed) {
            return;
        }
        let list = start.elapsed();
        let folder = Arc::new(folder);
        let keys = Arc::new(NameKeys::build(&folder, &*self.collator));
        let order = sorted_order_with_keys(&folder, self.sort, &keys, &*self.collator);
        let stats = LoadStats {
            entries: folder.len(),
            list,
            sort: start.elapsed() - list,
        };
        tracing::debug!(path = %self.path.display(), entries = stats.entries, ?stats.list, ?stats.sort, "dossier chargé");
        if !self.token.load(Ordering::Relaxed) {
            sink(LoadEvent::Complete {
                generation,
                folder,
                keys,
                order,
                stats,
            });
        }
    }

    /// Libellé de type, via le cache global puis la source.
    fn type_label(&self, name: &[u16], is_dir: bool) -> Arc<str> {
        let key = (lower_owned(name, is_dir), is_dir);
        if let Ok(cache) = self.types.lock()
            && let Some(l) = cache.get(&key)
        {
            return l.clone();
        }
        let label: Arc<str> = Arc::from(self.source.type_name(name, is_dir).unwrap_or_else(|| {
            if is_dir {
                "Dossier de fichiers".into()
            } else {
                "Fichier".into()
            }
        }));
        if let Ok(mut cache) = self.types.lock() {
            cache.insert(key, label.clone());
        }
        label
    }
}

/// Extension en minuscules (vide pour un dossier ou un nom sans point).
fn lower_owned(name: &[u16], is_dir: bool) -> Vec<u16> {
    let mut out = Vec::new();
    if !is_dir && let Some(dot) = name.iter().rposition(|&c| c == b'.' as u16) {
        lower_into(&name[dot + 1..], &mut out);
    }
    out
}

fn lower_into(src: &[u16], out: &mut Vec<u16>) {
    if src.iter().all(|&c| c < 0x80) {
        out.extend(src.iter().map(|&c| {
            if (b'A' as u16..=b'Z' as u16).contains(&c) {
                c + 32
            } else {
                c
            }
        }));
    } else {
        out.extend(crate::folder::extension_key(&[&[b'.' as u16], src].concat()));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::collate::PortableCollator;
    use crate::sort::SortColumn;
    use crate::source::MemorySource;

    fn loader(src: MemorySource) -> Loader {
        Loader::new(Arc::new(src), Arc::new(PortableCollator))
    }

    fn channel_sink() -> (Sink, mpsc::Receiver<LoadEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        (
            Arc::new(move |e| {
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(e);
                }
            }),
            rx,
        )
    }

    fn wait_complete(rx: &mpsc::Receiver<LoadEvent>) -> Option<(u64, Arc<Folder>, Vec<u32>)> {
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).ok()? {
                LoadEvent::Complete {
                    generation,
                    folder,
                    order,
                    ..
                } => return Some((generation, folder, order)),
                LoadEvent::Failed { error, .. } => panic!("échec : {error}"),
                LoadEvent::Partial { .. } => {}
            }
        }
    }

    #[test]
    fn loads_sorts_and_types() {
        let src = MemorySource::default().with(
            "D",
            &[
                ("b.TXT", false, 1),
                ("A", true, 0),
                ("a.txt", false, 2),
                ("c", false, 3),
            ],
        );
        let l = loader(src);
        let (sink, rx) = channel_sink();
        l.load(PathBuf::from("D"), 7, SortSpec::default(), sink);
        let (generation, folder, order) = wait_complete(&rx).expect("événement Complete");
        assert_eq!(generation, 7);
        let names: Vec<String> = order.iter().map(|&i| folder.display_name(i)).collect();
        assert_eq!(names, ["A", "a.txt", "b.TXT", "c"]);
        let label = |name: &str| {
            let i = folder
                .find(&name.encode_utf16().collect::<Vec<_>>())
                .expect("présent");
            folder
                .entry(i)
                .map(|e| folder.type_label(e).to_string())
                .unwrap_or_default()
        };
        assert_eq!(label("a.txt"), "Fichier txt");
        assert_eq!(label("b.TXT"), "Fichier txt");
        assert_eq!(label("A"), "Dossier de fichiers");
        assert_eq!(label("c"), "Fichier");
        assert_eq!(folder.types().len(), 3);
    }

    #[test]
    fn missing_folder_fails_with_kind() {
        let l = loader(MemorySource::default());
        let (sink, rx) = channel_sink();
        l.load(PathBuf::from("nope"), 1, SortSpec::default(), sink);
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(LoadEvent::Failed { error, generation }) => {
                assert_eq!(generation, 1);
                assert_eq!(error.kind, ErrorKind::NotFound);
            }
            _ => panic!("attendu : Failed"),
        }
    }

    #[test]
    fn big_folder_publishes_a_partial_first() {
        let entries: Vec<(String, bool, u64)> = (0..5000).map(|i| (format!("f{i}"), false, 0)).collect();
        let refs: Vec<(&str, bool, u64)> = entries.iter().map(|(n, d, s)| (n.as_str(), *d, *s)).collect();
        let l = loader(MemorySource::default().with("big", &refs));
        let (sink, rx) = channel_sink();
        l.load(PathBuf::from("big"), 1, SortSpec::default(), sink);
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(LoadEvent::Partial { folder, order, .. }) => {
                assert!(folder.len() >= FIRST_BATCH.min(5000) || !folder.is_empty());
                assert_eq!(order.len(), folder.len());
            }
            _ => panic!("attendu : Partial"),
        }
        let (_, folder, _) = wait_complete(&rx).expect("Complete");
        assert_eq!(folder.len(), 5000);
    }

    #[test]
    fn new_load_cancels_previous() {
        let entries: Vec<(String, bool, u64)> = (0..2000).map(|i| (format!("f{i}"), false, 0)).collect();
        let refs: Vec<(&str, bool, u64)> = entries.iter().map(|(n, d, s)| (n.as_str(), *d, *s)).collect();
        let src = MemorySource {
            delay_per_entry: Some(Duration::from_millis(1)),
            ..Default::default()
        }
        .with("slow", &refs)
        .with("fast", &[("x", false, 0)]);
        let l = loader(src);
        let (sink, rx) = channel_sink();
        l.load(PathBuf::from("slow"), 1, SortSpec::default(), sink.clone());
        std::thread::sleep(Duration::from_millis(20));
        l.load(PathBuf::from("fast"), 2, SortSpec::default(), sink);
        let (generation, folder, _) = wait_complete(&rx).expect("Complete");
        assert_eq!(generation, 2);
        assert_eq!(folder.len(), 1);
        // Le chargement lent, annulé, ne doit jamais produire de Complete.
        let late = rx.recv_timeout(Duration::from_millis(300));
        assert!(!matches!(late, Ok(LoadEvent::Complete { generation: 1, .. })));
    }

    struct PanickingSource;
    impl DirSource for PanickingSource {
        fn list(
            &self,
            _: &std::path::Path,
            _: &dyn Fn() -> bool,
            _: &mut dyn FnMut(&RawEntry<'_>),
        ) -> Result<(), SourceError> {
            panic!("panne simulée dans la source");
        }
        fn type_name(&self, _: &[u16], _: bool) -> Option<String> {
            None
        }
    }

    #[test]
    fn worker_panic_becomes_a_clean_failure() {
        let l = Loader::new(Arc::new(PanickingSource), Arc::new(PortableCollator));
        let (sink, rx) = channel_sink();
        l.load(PathBuf::from("x"), 9, SortSpec::default(), sink);
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(LoadEvent::Failed { generation: 9, error }) => assert_eq!(error.kind, ErrorKind::Other),
            _ => panic!("attendu : Failed"),
        }
        // Le chargeur reste utilisable après le panic.
        let l2 = loader(MemorySource::default().with("ok", &[("a", false, 0)]));
        let (sink, rx) = channel_sink();
        l2.load(PathBuf::from("ok"), 1, SortSpec::default(), sink);
        assert!(wait_complete(&rx).is_some());
    }

    #[test]
    fn compute_view_sorts_and_filters() {
        let l =
            loader(MemorySource::default().with("D", &[("b", false, 5), ("a", false, 9), ("ab", false, 1)]));
        let (sink, rx) = channel_sink();
        l.load(PathBuf::from("D"), 1, SortSpec::default(), sink);
        let (_, folder, _) = wait_complete(&rx).expect("Complete");
        let keys = Arc::new(NameKeys::build(&folder, &PortableCollator));
        let (tx, rx2) = mpsc::channel();
        let spec = SortSpec {
            column: SortColumn::Size,
            descending: true,
        };
        l.compute_view(folder.clone(), keys, spec, Filter::new("a"), 1, 3, move |r| {
            let _ = tx.send(r);
        });
        let r = rx2.recv_timeout(Duration::from_secs(10)).expect("ViewResult");
        assert_eq!(r.seq, 3);
        let names = |v: &[u32]| v.iter().map(|&i| folder.display_name(i)).collect::<Vec<_>>();
        assert_eq!(names(r.order.as_deref().unwrap_or_default()), ["a", "b", "ab"]);
        assert_eq!(names(&r.visible), ["a", "ab"]);
    }
}
