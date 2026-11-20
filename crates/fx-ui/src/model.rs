//! Adaptateur `slint::Model` : ne fabrique une ligne affichable que lorsque Slint la demande
//! (lignes visibles), et détient la vue courante, la sélection et le cache d'icônes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use fx_core::Folder;
use fx_core::format::{human_size, size_kb};
use fx_core::selection::Selection;
use slint::{
    Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

use crate::icons::{Delivered, IconKey, IconLoader, Request, grid_icon_key, icon_key};
use crate::{GridLine, Row};

/// Au-delà de ce nombre de lignes modifiées, on réinitialise le modèle plutôt que de notifier
/// ligne par ligne.
const RESET_THRESHOLD: usize = 4000;
/// Nombre maximal d'icônes d'éléments gardées en mémoire.
const MAX_ITEM_ICONS: usize = 2000;
/// Nombre maximal de vignettes gardées en mémoire (les plus anciennes sont libérées) :
/// ~80 Kio chacune à 144 px, soit au plus ~16 Mo.
const MAX_THUMBS: usize = 200;
/// Lignes de grille gardées (et tenues à jour). La moitié doit dépasser le nombre de lignes
/// visibles (≤ 16 lignes de 140 px sur un écran 4K).
const GRID_CACHE_LINES: usize = 64;
/// Taille des icônes de la vue icônes, en multiple de celle de la vue détails (16 → 96 px).
const GRID_ICON_FACTOR: u32 = 6;

#[derive(Default)]
struct View {
    folder: Option<Arc<Folder>>,
    /// Indices d'entrées dans l'ordre affiché (trié et filtré).
    visible: Vec<u32>,
    /// Ligne de chaque entrée (`u32::MAX` si masquée par le filtre).
    row_of: Vec<u32>,
    selection: Selection,
}

#[derive(Default)]
struct IconCache {
    images: HashMap<IconKey, Option<Image>>,
    pending: HashSet<IconKey>,
    /// Lignes affichées sans icône, par icône attendue : une livraison ne rafraîchit que les
    /// lignes qui attendaient cette icône.
    waiting: HashMap<IconKey, Vec<usize>>,
    item_count: usize,
    /// Vignettes en cache, de la plus ancienne à la plus récente.
    thumbs: VecDeque<IconKey>,
}

pub struct FolderModel {
    view: RefCell<View>,
    notify: ModelNotify,
    icons: RefCell<IconCache>,
    loader: std::sync::Arc<IconLoader>,
    /// Tailles de dossiers calculées à la demande, par nom : (octets, terminé).
    dir_sizes: RefCell<HashMap<Vec<u16>, (u64, bool)>>,
    icon_px: Cell<u32>,
    /// Vue icônes (grille) plutôt que détails.
    grid: Cell<bool>,
    /// Colonnes de la grille (largeur du volet / largeur d'une tuile).
    cols: Cell<usize>,
    /// Notifications du modèle de lignes de grille ([`GridModel`]).
    grid_notify: ModelNotify,
    /// Lignes de grille fabriquées (modèle des tuiles de chaque ligne). Un changement
    /// d'élément (icône, sélection) met à jour la tuile en place : la ligne n'est pas
    /// refabriquée, ses tuiles ne sont pas réinstanciées.
    grid_cache: RefCell<HashMap<usize, Rc<VecModel<Row>>>>,
    /// Lignes de grille visibles `[début, fin)`, relevées à chaque image.
    visible_lines: Cell<(usize, usize)>,
    /// Noms des éléments créés à l'instant (mis en évidence par `Row::fresh`).
    fresh: RefCell<HashSet<Vec<u16>>>,
    /// Éléments de lignes en cache mais hors de l'écran, mis à jour sans demander leur icône :
    /// elle sera demandée quand ils deviendront visibles.
    deferred: RefCell<HashSet<usize>>,
}

impl FolderModel {
    pub fn new(loader: std::sync::Arc<IconLoader>) -> Self {
        Self {
            view: RefCell::default(),
            notify: ModelNotify::default(),
            icons: RefCell::default(),
            loader,
            icon_px: Cell::new(16),
            dir_sizes: RefCell::default(),
            grid: Cell::new(false),
            cols: Cell::new(1),
            grid_notify: ModelNotify::default(),
            grid_cache: RefCell::default(),
            visible_lines: Cell::new((0, 0)),
            deferred: RefCell::default(),
            fresh: RefCell::default(),
        }
    }

    fn row_changed(&self, row: usize) {
        self.notify.row_changed(row);
        if self.grid.get() {
            let cols = self.cols.get().max(1);
            let line = row / cols;
            let items = self.grid_cache.borrow().get(&line).cloned();
            // Ligne non fabriquée : rien d'affiché à mettre à jour.
            let Some(items) = items else { return };
            let (first, end) = self.visible_lines.get();
            let visible = (first..end).contains(&line);
            if let Some(data) = self.make_row(row, visible) {
                items.set_row_data(row % cols, data);
            }
        }
    }

    /// Lignes de grille affichées (appelé à chaque image) : les éléments différés qui
    /// deviennent visibles reçoivent leur icône.
    pub fn set_visible_lines(&self, first: usize, end: usize) {
        if self.visible_lines.replace((first, end)) == (first, end) || !self.grid.get() {
            return;
        }
        let cols = self.columns();
        let now: Vec<usize> = {
            let mut d = self.deferred.borrow_mut();
            let rows: Vec<usize> = d
                .iter()
                .copied()
                .filter(|r| (first..end).contains(&(r / cols)))
                .collect();
            for r in &rows {
                d.remove(r);
            }
            rows
        };
        for r in now {
            self.row_changed(r);
        }
    }

    fn reset(&self) {
        self.deferred.borrow_mut().clear();
        self.grid_cache.borrow_mut().clear();
        self.notify.reset();
        self.grid_notify.reset();
    }

    /// Met en évidence les éléments `names` (ou retire la mise en évidence si `on` est faux).
    pub fn set_fresh(&self, names: &[Vec<u16>], on: bool) {
        {
            let mut set = self.fresh.borrow_mut();
            for n in names {
                if on {
                    set.insert(n.clone());
                } else {
                    set.remove(n);
                }
            }
        }
        let rows: Vec<usize> = {
            let v = self.view.borrow();
            let Some(folder) = v.folder.as_ref() else { return };
            names
                .iter()
                .filter_map(|n| folder.find(n))
                .filter_map(|e| v.row_of.get(e as usize).copied())
                .filter(|&r| r != u32::MAX)
                .map(|r| r as usize)
                .collect()
        };
        for r in rows {
            self.row_changed(r);
        }
    }

    pub fn is_grid(&self) -> bool {
        self.grid.get()
    }

    /// Passe en vue icônes ou détails (les icônes changent de taille).
    pub fn set_grid(&self, grid: bool) {
        if grid != self.grid.get() {
            self.grid.set(grid);
            *self.icons.borrow_mut() = IconCache::default();
            self.reset();
        }
    }

    pub fn columns(&self) -> usize {
        self.cols.get().max(1)
    }

    /// Nombre de colonnes de la grille (suivant la largeur du volet).
    pub fn set_columns(&self, cols: usize) {
        let cols = cols.max(1);
        if cols != self.cols.get() {
            self.cols.set(cols);
            self.icons.borrow_mut().waiting.clear();
            self.deferred.borrow_mut().clear();
            self.grid_cache.borrow_mut().clear();
            self.grid_notify.reset();
        }
    }

    /// Taille des icônes demandées, en pixels physiques.
    fn px(&self) -> u32 {
        if self.grid.get() {
            self.icon_px.get() * GRID_ICON_FACTOR
        } else {
            self.icon_px.get()
        }
    }

    /// Taille des icônes en pixels physiques (16 px logiques × échelle).
    pub fn set_icon_px(&self, px: u32) {
        if px != self.icon_px.get() {
            self.icon_px.set(px);
            *self.icons.borrow_mut() = IconCache::default();
        }
    }

    /// Nouvelle vue complète (nouveau dossier, nouveau tri ou filtre, mise à jour).
    pub fn set_view(&self, folder: Option<Arc<Folder>>, visible: Vec<u32>, selection: Selection) {
        {
            let mut v = self.view.borrow_mut();
            let len = folder.as_ref().map_or(0, |f| f.len());
            let mut row_of = vec![u32::MAX; len];
            for (r, &e) in visible.iter().enumerate() {
                if let Some(slot) = row_of.get_mut(e as usize) {
                    *slot = r as u32;
                }
            }
            *v = View {
                folder,
                visible,
                row_of,
                selection,
            };
        }
        self.icons.borrow_mut().waiting.clear();
        self.reset();
    }

    /// Change seulement l'ordre affiché (tri, filtre), en gardant dossier et sélection.
    pub fn set_visible(&self, visible: Vec<u32>) {
        let (folder, selection) = {
            let v = self.view.borrow();
            (v.folder.clone(), v.selection.clone())
        };
        self.set_view(folder, visible, selection);
    }

    pub fn clear(&self) {
        self.set_view(None, Vec::new(), Selection::default());
    }

    pub fn folder(&self) -> Option<Arc<Folder>> {
        self.view.borrow().folder.clone()
    }

    pub fn visible(&self) -> Vec<u32> {
        self.view.borrow().visible.clone()
    }

    pub fn entry_at(&self, row: usize) -> Option<u32> {
        self.view.borrow().visible.get(row).copied()
    }

    pub fn row_of(&self, entry: u32) -> Option<usize> {
        let r = *self.view.borrow().row_of.get(entry as usize)?;
        (r != u32::MAX).then_some(r as usize)
    }

    pub fn len(&self) -> usize {
        self.view.borrow().visible.len()
    }

    pub fn selection(&self) -> Selection {
        self.view.borrow().selection.clone()
    }

    /// Entrées sélectionnées, dans l'ordre d'affichage.
    pub fn selected(&self) -> Vec<u32> {
        let v = self.view.borrow();
        v.selection.selected_in(&v.visible)
    }

    /// Modifie la sélection et notifie seulement les lignes qui changent d'apparence.
    pub fn update_selection<R>(&self, f: impl FnOnce(&mut Selection, &[u32]) -> R) -> R {
        let (result, changed_rows) = {
            let mut v = self.view.borrow_mut();
            let before = v.selection.clone();
            let View {
                selection,
                visible,
                row_of,
                ..
            } = &mut *v;
            let result = f(selection, visible);
            let mut changed: Vec<u32> = before.entries();
            changed.extend(selection.entries());
            changed.extend(before.focus());
            changed.extend(selection.focus());
            changed.sort_unstable();
            changed.dedup();
            let rows: Vec<usize> = changed
                .into_iter()
                .filter(|&e| {
                    before.is_selected(e) != selection.is_selected(e)
                        || Some(e) == before.focus()
                        || Some(e) == selection.focus()
                })
                .filter_map(|e| row_of.get(e as usize).copied())
                .filter(|&r| r != u32::MAX)
                .map(|r| r as usize)
                .collect();
            (result, rows)
        };
        if changed_rows.len() > RESET_THRESHOLD {
            self.reset();
        } else {
            for r in changed_rows {
                self.row_changed(r);
            }
        }
        result
    }

    /// Taille calculée d'un dossier (affichée dans la colonne Taille).
    pub fn set_dir_size(&self, name: &[u16], bytes: u64, done: bool) {
        self.dir_sizes.borrow_mut().insert(name.to_vec(), (bytes, done));
        let row = {
            let v = self.view.borrow();
            v.folder
                .as_ref()
                .and_then(|f| f.find(name))
                .and_then(|e| v.row_of.get(e as usize).copied())
        };
        if let Some(r) = row.filter(|&r| r != u32::MAX) {
            self.row_changed(r as usize);
        }
    }

    pub fn clear_dir_sizes(&self) {
        self.dir_sizes.borrow_mut().clear();
    }

    /// Icônes chargées : mise en cache et rafraîchissement des lignes qui les attendaient.
    pub fn on_icons(&self, delivered: &Delivered) {
        let rows = {
            let mut c = self.icons.borrow_mut();
            let mut rows: Vec<usize> = Vec::new();
            for (key, result) in delivered {
                if !c.pending.remove(key) {
                    continue; // demandée par un autre volet
                }
                rows.extend(c.waiting.remove(key).unwrap_or_default());
                // Requête abandonnée : rien en cache, elle sera refaite si la ligne revient.
                let Some(img) = result else { continue };
                match key {
                    IconKey::Item(..) => c.item_count += 1,
                    IconKey::Thumb(..) if img.is_some() => c.thumbs.push_back(key.clone()),
                    _ => {}
                }
                let image = img.as_ref().map(|i| {
                    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&i.rgba, i.width, i.height);
                    Image::from_rgba8(buf)
                });
                c.images.insert(key.clone(), image);
            }
            if c.item_count > MAX_ITEM_ICONS {
                c.images.retain(|k, _| !matches!(k, IconKey::Item(..)));
                c.item_count = 0;
            }
            while c.thumbs.len() > MAX_THUMBS {
                if let Some(old) = c.thumbs.pop_front() {
                    c.images.remove(&old);
                }
            }
            rows
        };
        let len = self.len();
        for r in rows.into_iter().collect::<HashSet<_>>() {
            if r < len {
                self.row_changed(r);
            }
        }
    }

    /// Icône de l'élément `row` ; si elle n'est pas en cache, `request` : la demander, sinon
    /// noter l'élément comme différé.
    fn icon_for(&self, folder: &Folder, entry: u32, row: usize, request: bool) -> Option<Image> {
        let px = self.px();
        let (mut key, mut sample) = if self.grid.get() {
            grid_icon_key(folder, entry, px)?
        } else {
            icon_key(folder, entry, px)?
        };
        let mut c = self.icons.borrow_mut();
        if let Some(img) = c.images.get(&key) {
            if img.is_some() || !matches!(key, IconKey::Thumb(..)) {
                return img.clone();
            }
            // Pas de vignette pour ce fichier : icône du type.
            (key, sample) = icon_key(folder, entry, px)?;
            if let Some(img) = c.images.get(&key) {
                return img.clone();
            }
        }
        if !request {
            drop(c);
            self.deferred.borrow_mut().insert(row);
            return None;
        }
        if !c.pending.contains(&key) {
            c.pending.insert(key.clone());
            self.loader.request(Request {
                key: key.clone(),
                sample_name: sample,
                size: px,
                generation: self.loader.generation(),
            });
        }
        c.waiting.entry(key).or_default().push(row);
        None
    }
}

impl Model for FolderModel {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.view.borrow().visible.len()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        self.make_row(row, true)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

impl FolderModel {
    /// Données affichables de la ligne `row` ; `request` : demander l'icône si elle manque.
    fn make_row(&self, row: usize, request: bool) -> Option<Row> {
        let (folder, index, selected, focused) = {
            let v = self.view.borrow();
            let folder = v.folder.clone()?;
            let index = *v.visible.get(row)?;
            (
                folder,
                index,
                v.selection.is_selected(index),
                v.selection.focus() == Some(index),
            )
        };
        let e = *folder.entry(index)?;
        let icon = self.icon_for(&folder, index, row, request);
        Some(Row {
            name: folder.display_name(index).into(),
            modified: fx_win::locale::format_filetime(e.modified)
                .map(SharedString::from)
                .unwrap_or_default(),
            kind: folder.type_label(&e).into(),
            size: if e.is_dir() {
                match self.dir_sizes.borrow().get(folder.name(index)) {
                    Some(&(bytes, true)) => human_size(bytes).into(),
                    Some(&(bytes, false)) => format!("… {}", human_size(bytes)).into(),
                    None => SharedString::new(),
                }
            } else {
                size_kb(e.size).into()
            },
            is_dir: e.is_dir(),
            dimmed: e.is_hidden(),
            cloud: match e.cloud_state() {
                fx_core::folder::CloudState::None => 0,
                fx_core::folder::CloudState::OnlineOnly => 1,
                fx_core::folder::CloudState::Local => 2,
                fx_core::folder::CloudState::Pinned => 3,
            },
            selected,
            focused,
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
            fresh: self.fresh.borrow().contains(folder.name(index)),
        })
    }
}

/// Lignes de la vue icônes : chaque ligne porte `columns()` éléments du [`FolderModel`].
/// Seules les lignes affichées sont fabriquées (virtualisation de `ListView`).
pub struct GridModel(pub Rc<FolderModel>);

impl Model for GridModel {
    type Data = GridLine;

    fn row_count(&self) -> usize {
        if !self.0.is_grid() {
            return 0;
        }
        self.0.len().div_ceil(self.0.columns())
    }

    fn row_data(&self, line: usize) -> Option<GridLine> {
        let m = &self.0;
        if !m.is_grid() {
            return None;
        }
        let cols = m.columns();
        let first = line * cols;
        if let Some(items) = m.grid_cache.borrow().get(&line) {
            return Some(GridLine {
                first: first as i32,
                items: ModelRc::from(items.clone()),
            });
        }
        let len = m.len();
        if first >= len {
            return None;
        }
        let items: Vec<Row> = (first..(first + cols).min(len))
            .filter_map(|r| m.make_row(r, true))
            .collect();
        let items = Rc::new(VecModel::from(items));
        // Cache plein : on retire la ligne la plus éloignée de celle demandée. Parmi
        // GRID_CACHE_LINES lignes distinctes, elle est à au moins GRID_CACHE_LINES / 2 lignes,
        // donc hors de l'écran : aucune ligne affichée ne cesse d'être tenue à jour. (Pas de
        // notification ici : Slint est en train de lire le modèle.)
        let mut cache = m.grid_cache.borrow_mut();
        if cache.len() >= GRID_CACHE_LINES
            && let Some(far) = cache.keys().copied().max_by_key(|l| l.abs_diff(line))
        {
            cache.remove(&far);
            let cols = m.columns();
            m.deferred.borrow_mut().retain(|r| r / cols != far);
        }
        cache.insert(line, items.clone());
        Some(GridLine {
            first: first as i32,
            items: ModelRc::from(items),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.0.grid_notify
    }
}
