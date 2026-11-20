//! Adaptateur `slint::Model` : ne fabrique une ligne affichable que lorsque Slint la demande
//! (lignes visibles), et détient la vue courante, la sélection et le cache d'icônes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fx_core::Folder;
use fx_core::format::size_kb;
use fx_core::selection::Selection;
use slint::{Image, Model, ModelNotify, ModelTracker, Rgba8Pixel, SharedPixelBuffer, SharedString};

use crate::Row;
use crate::icons::{Delivered, IconKey, IconLoader, Request, icon_key};

/// Au-delà de ce nombre de lignes modifiées, on réinitialise le modèle plutôt que de notifier
/// ligne par ligne.
const RESET_THRESHOLD: usize = 4000;
/// Nombre maximal d'icônes d'éléments gardées en mémoire.
const MAX_ITEM_ICONS: usize = 2000;

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
    /// Lignes affichées sans icône en attendant le chargement.
    waiting_rows: Vec<usize>,
    item_count: usize,
}

pub struct FolderModel {
    view: RefCell<View>,
    notify: ModelNotify,
    icons: RefCell<IconCache>,
    loader: IconLoader,
    icon_px: Cell<u32>,
}

impl FolderModel {
    pub fn new(loader: IconLoader) -> Self {
        Self {
            view: RefCell::default(),
            notify: ModelNotify::default(),
            icons: RefCell::default(),
            loader,
            icon_px: Cell::new(16),
        }
    }

    /// Taille des icônes en pixels physiques (16 px logiques × échelle).
    pub fn set_icon_px(&self, px: u32) {
        if px != self.icon_px.get() {
            self.icon_px.set(px);
            *self.icons.borrow_mut() = IconCache::default();
        }
    }

    pub fn icon_loader(&self) -> &IconLoader {
        &self.loader
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
        self.icons.borrow_mut().waiting_rows.clear();
        self.notify.reset();
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
            self.notify.reset();
        } else {
            for r in changed_rows {
                self.notify.row_changed(r);
            }
        }
        result
    }

    /// Icônes chargées : mise en cache et rafraîchissement des lignes qui les attendaient.
    pub fn on_icons(&self, delivered: Delivered) {
        let rows = {
            let mut c = self.icons.borrow_mut();
            for (key, img) in delivered {
                c.pending.remove(&key);
                if matches!(key, IconKey::Item(_)) {
                    c.item_count += 1;
                }
                let image = img.map(|i| {
                    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&i.rgba, i.width, i.height);
                    Image::from_rgba8(buf)
                });
                c.images.insert(key, image);
            }
            if c.item_count > MAX_ITEM_ICONS {
                c.images.retain(|k, _| matches!(k, IconKey::Type { .. }));
                c.item_count = 0;
            }
            std::mem::take(&mut c.waiting_rows)
        };
        let len = self.len();
        for r in rows.into_iter().collect::<HashSet<_>>() {
            if r < len {
                self.notify.row_changed(r);
            }
        }
    }

    fn icon_for(&self, folder: &Folder, entry: u32, row: usize) -> Option<Image> {
        let (key, sample) = icon_key(folder, entry)?;
        let mut c = self.icons.borrow_mut();
        if let Some(img) = c.images.get(&key) {
            return img.clone();
        }
        if !c.pending.contains(&key) {
            c.pending.insert(key.clone());
            self.loader.request(Request {
                key,
                sample_name: sample,
                size: self.icon_px.get(),
                generation: self.loader.generation(),
            });
        }
        c.waiting_rows.push(row);
        None
    }
}

impl Model for FolderModel {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.view.borrow().visible.len()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
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
        let icon = self.icon_for(&folder, index, row);
        Some(Row {
            name: folder.display_name(index).into(),
            modified: fx_win::locale::format_filetime(e.modified)
                .map(SharedString::from)
                .unwrap_or_default(),
            kind: folder.type_label(&e).into(),
            size: if e.is_dir() {
                SharedString::new()
            } else {
                size_kb(e.size).into()
            },
            is_dir: e.is_dir(),
            dimmed: e.is_hidden(),
            selected,
            focused,
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
