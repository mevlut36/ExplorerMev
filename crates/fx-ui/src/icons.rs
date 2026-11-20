//! Chargement asynchrone des icônes : seulement pour les lignes affichées, jamais sur le thread
//! d'interface, avec cache par type d'extension et par élément.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};
use fx_core::Folder;
use fx_core::folder::{ATTR_READONLY, ATTR_SYSTEM};
use fx_win::icons::IconImage;

/// Extensions dont l'icône dépend du fichier lui-même.
const PER_ITEM: [&str; 10] = [
    "exe",
    "lnk",
    "ico",
    "url",
    "cur",
    "ani",
    "scr",
    "msc",
    "cpl",
    "appref-ms",
];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconKey {
    /// Icône du type (extension en minuscules) : partagée par tous les fichiers de ce type.
    Type { ext: Vec<u16>, dir: bool },
    /// Icône propre à un élément.
    Item(PathBuf),
}

/// Clé d'icône d'une entrée, et nom d'exemple pour la requête.
pub fn icon_key(folder: &Folder, entry: u32) -> Option<(IconKey, Vec<u16>)> {
    let e = folder.entry(entry)?;
    let name = folder.name(entry);
    if e.is_dir() {
        // Dossier personnalisé (desktop.ini) : marqué lecture seule ou système.
        if e.attributes & (ATTR_READONLY | ATTR_SYSTEM) != 0 && !e.is_cloud_online_only() {
            return Some((IconKey::Item(folder.child_path(entry)), name.to_vec()));
        }
        return Some((
            IconKey::Type {
                ext: Vec::new(),
                dir: true,
            },
            name.to_vec(),
        ));
    }
    let ext = fx_core::folder::extension_key(name);
    let ext_s = String::from_utf16_lossy(&ext);
    // Un fichier OneDrive en ligne seulement n'est jamais lu : icône de type.
    if PER_ITEM.contains(&ext_s.as_str()) && !e.is_cloud_online_only() {
        return Some((IconKey::Item(folder.child_path(entry)), name.to_vec()));
    }
    Some((IconKey::Type { ext, dir: false }, name.to_vec()))
}

pub struct Request {
    pub key: IconKey,
    pub sample_name: Vec<u16>,
    pub size: u32,
    pub generation: u64,
}

pub type Delivered = Vec<(IconKey, Option<IconImage>)>;

pub struct IconLoader {
    tx: Sender<Request>,
    generation: Arc<AtomicU64>,
}

impl IconLoader {
    /// Démarre `threads` threads STA. `deliver` reçoit des lots de résultats (thread de travail).
    pub fn start(threads: usize, deliver: impl Fn(Delivered) + Send + Sync + 'static) -> Self {
        let (tx, rx) = unbounded::<Request>();
        let generation = Arc::new(AtomicU64::new(0));
        let deliver = Arc::new(deliver);
        for i in 0..threads {
            let rx = rx.clone();
            let deliver = deliver.clone();
            let generation = generation.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("fx-icon-{i}"))
                .spawn(move || worker(rx, &*deliver, &generation));
            if let Err(e) = spawned {
                tracing::error!("thread d'icônes non démarré : {e}");
            }
        }
        Self { tx, generation }
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    /// Change de dossier : les requêtes d'icônes d'éléments en attente deviennent inutiles.
    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

fn worker(rx: Receiver<Request>, deliver: &dyn Fn(Delivered), generation: &AtomicU64) {
    let _com = fx_win::com::ComApartment::sta().ok();
    while let Ok(first) = rx.recv() {
        let mut batch: Delivered = Vec::new();
        let mut next = Some(first);
        while let Some(req) = next {
            let stale =
                matches!(req.key, IconKey::Item(_)) && req.generation < generation.load(Ordering::Relaxed);
            if !stale {
                let img = match &req.key {
                    IconKey::Type { dir, .. } => fx_win::icons::type_icon(&req.sample_name, *dir, req.size),
                    IconKey::Item(path) => fx_win::icons::item_icon(path, req.size)
                        .or_else(|| fx_win::icons::type_icon(&req.sample_name, false, req.size)),
                };
                batch.push((req.key, img));
            }
            // Regroupe ce qui est déjà en attente (au plus 32) pour limiter les rafraîchissements.
            next = if batch.len() < 32 {
                rx.try_recv().ok()
            } else {
                None
            };
        }
        if !batch.is_empty() {
            deliver(batch);
        }
    }
}
