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

/// Clé d'icône. La taille (pixels physiques) en fait partie : les deux volets peuvent
/// afficher la même icône en vue détails (16 px logiques) et en vue icônes (96 px).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconKey {
    /// Icône du type (extension en minuscules) : partagée par tous les fichiers de ce type.
    Type { ext: Vec<u16>, dir: bool, px: u32 },
    /// Icône propre à un élément.
    Item(PathBuf, u32),
    /// Vignette d'un fichier (vue icônes). `None` à la livraison : pas de vignette, l'icône
    /// du type la remplace.
    Thumb(PathBuf, u32),
}

impl IconKey {
    /// Requête coûteuse (lecture du fichier) : attend que le défilement se calme.
    fn per_item(&self) -> bool {
        matches!(self, IconKey::Item(..) | IconKey::Thumb(..))
    }
}

/// Clé d'icône d'une entrée, et nom d'exemple pour la requête.
pub fn icon_key(folder: &Folder, entry: u32, px: u32) -> Option<(IconKey, Vec<u16>)> {
    let e = folder.entry(entry)?;
    let name = folder.name(entry);
    if e.is_dir() {
        // Dossier personnalisé (desktop.ini) : marqué lecture seule ou système.
        if e.attributes & (ATTR_READONLY | ATTR_SYSTEM) != 0 && !e.is_cloud_online_only() {
            return Some((IconKey::Item(folder.child_path(entry), px), name.to_vec()));
        }
        return Some((
            IconKey::Type {
                ext: Vec::new(),
                dir: true,
                px,
            },
            name.to_vec(),
        ));
    }
    let ext = fx_core::folder::extension_key(name);
    let ext_s = String::from_utf16_lossy(&ext);
    // Un fichier OneDrive en ligne seulement n'est jamais lu : icône de type.
    if PER_ITEM.contains(&ext_s.as_str()) && !e.is_cloud_online_only() {
        return Some((IconKey::Item(folder.child_path(entry), px), name.to_vec()));
    }
    Some((IconKey::Type { ext, dir: false, px }, name.to_vec()))
}

/// Clé de la vue icônes : vignette pour les fichiers présents localement, sinon comme
/// [`icon_key`]. Jamais de vignette pour un fichier en ligne seulement (elle le
/// téléchargerait).
pub fn grid_icon_key(folder: &Folder, entry: u32, px: u32) -> Option<(IconKey, Vec<u16>)> {
    let e = folder.entry(entry)?;
    let (key, sample) = icon_key(folder, entry, px)?;
    if e.is_dir() || e.is_cloud_online_only() || matches!(key, IconKey::Item(..)) {
        return Some((key, sample));
    }
    Some((IconKey::Thumb(folder.child_path(entry), px), sample))
}

pub struct Request {
    pub key: IconKey,
    pub sample_name: Vec<u16>,
    pub size: u32,
    pub generation: u64,
}

/// Résultats : `Some(image)` chargée (ou `Some(None)` : pas d'icône) ; `None` : requête
/// abandonnée (périmée), à redemander si la ligne est de nouveau affichée.
pub type Delivered = Vec<(IconKey, Option<Option<IconImage>>)>;

/// Au-delà, les requêtes d'icônes d'éléments les plus anciennes sont abandonnées : pendant un
/// défilement rapide, elles concernent des lignes qui ne sont plus affichées.
/// Assez pour une grille d'icônes plein écran en 4K (~34 × 15 tuiles).
const MAX_PENDING_ITEMS: usize = 512;

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

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

/// Les icônes d'éléments (lecture du fichier, plus coûteuse) attendent que les demandes se
/// calment : pendant un défilement rapide, les lignes demandées disparaissent aussitôt.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(60);

fn load(req: &Request) -> Option<IconImage> {
    load_raw(req).map(|img| fit(img, req.size))
}

/// Réduit l'image pour qu'elle tienne dans `max` × `max` pixels (moyenne par zone, alpha
/// pondéré). Le moteur de rendu logiciel redimensionnerait sinon l'image à chaque affichage.
pub fn fit(img: IconImage, max: u32) -> IconImage {
    let (w, h) = (img.width, img.height);
    if max == 0 || (w <= max && h <= max) || w == 0 || h == 0 {
        return img;
    }
    let scale = f64::from(max) / f64::from(w.max(h));
    let nw = ((f64::from(w) * scale).round() as u32).clamp(1, max);
    let nh = ((f64::from(h) * scale).round() as u32).clamp(1, max);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h).div_ceil(nh).min(h));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w).div_ceil(nw).min(w));
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * w + sx) * 4) as usize;
                    let a = u64::from(img.rgba[i + 3]);
                    acc[0] += u64::from(img.rgba[i]) * a;
                    acc[1] += u64::from(img.rgba[i + 1]) * a;
                    acc[2] += u64::from(img.rgba[i + 2]) * a;
                    acc[3] += a;
                    n += 1;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..3 {
                out[o + c] = acc[c].checked_div(acc[3]).unwrap_or(0).min(255) as u8;
            }
            out[o + 3] = (acc[3] / n.max(1)).min(255) as u8;
        }
    }
    IconImage {
        width: nw,
        height: nh,
        rgba: out,
    }
}

fn load_raw(req: &Request) -> Option<IconImage> {
    match &req.key {
        IconKey::Type { dir, .. } => fx_win::icons::type_icon(&req.sample_name, *dir, req.size),
        IconKey::Item(path, _) => fx_win::icons::item_icon(path, req.size)
            .or_else(|| fx_win::icons::type_icon(&req.sample_name, false, req.size)),
        // Cache de miniatures de Windows d'abord (aucune lecture du fichier), puis génération.
        IconKey::Thumb(path, _) => fx_win::icons::thumbnail(path, req.size, true)
            .or_else(|| fx_win::icons::thumbnail(path, req.size, false)),
    }
}

fn worker(rx: Receiver<Request>, deliver: &dyn Fn(Delivered), generation: &AtomicU64) {
    use crossbeam_channel::RecvTimeoutError;
    let _com = fx_win::com::ComApartment::sta().ok();
    // Icônes d'éléments en attente de stabilisation, de la plus ancienne à la plus récente.
    let mut held: Vec<Request> = Vec::new();
    let mut last_item_at = std::time::Instant::now();
    loop {
        let first = if held.is_empty() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(SETTLE.saturating_sub(last_item_at.elapsed()))
        };
        let incoming: Vec<Request> = match first {
            Ok(r) => std::iter::once(r).chain(rx.try_iter()).collect(),
            Err(RecvTimeoutError::Timeout) => Vec::new(),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut out: Delivered = Vec::new();
        // Icônes de type : tout de suite (peu coûteuses, partagées par extension).
        for req in incoming {
            if req.key.per_item() {
                last_item_at = std::time::Instant::now();
                held.push(req);
            } else {
                let img = load(&req);
                out.push((req.key, Some(img)));
            }
        }
        // On ne garde que les demandes les plus récentes (lignes affichées maintenant).
        let current = generation.load(Ordering::Relaxed);
        if held.len() > MAX_PENDING_ITEMS {
            let excess = held.len() - MAX_PENDING_ITEMS;
            out.extend(held.drain(..excess).map(|r| (r.key, None)));
        }
        let (stale, fresh): (Vec<Request>, Vec<Request>) =
            held.drain(..).partition(|r| r.generation < current);
        out.extend(stale.into_iter().map(|r| (r.key, None)));
        held = fresh;
        if !out.is_empty() {
            deliver(std::mem::take(&mut out));
        }
        if !held.is_empty() && last_item_at.elapsed() >= SETTLE {
            for req in std::mem::take(&mut held).into_iter().rev() {
                let img = load(&req);
                out.push((req.key, Some(img)));
                if out.len() >= 16 {
                    deliver(std::mem::take(&mut out));
                }
            }
            if !out.is_empty() {
                deliver(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_downscales_keeping_ratio_and_color() {
        // 4 × 2, rouge opaque à gauche, transparent à droite.
        let mut rgba = Vec::new();
        for _ in 0..2 {
            rgba.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        let img = fit(
            IconImage {
                width: 4,
                height: 2,
                rgba,
            },
            2,
        );
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(img.rgba[7], 0);
        let small = IconImage {
            width: 2,
            height: 2,
            rgba: vec![9; 16],
        };
        assert_eq!(fit(small.clone(), 16), small);
    }
}
