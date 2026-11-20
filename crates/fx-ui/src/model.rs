//! Adaptateur `slint::Model` : ne fabrique une ligne affichable que lorsque Slint la demande,
//! c'est-à-dire pour les lignes visibles.

use std::cell::RefCell;
use std::sync::Arc;

use fx_core::Folder;
use fx_core::format::size_kb;
use slint::{Model, ModelNotify, ModelTracker, SharedString};

use crate::Row;

#[derive(Default)]
struct View {
    folder: Option<Arc<Folder>>,
    /// Indices d'entrées dans l'ordre affiché (trié et filtré).
    visible: Vec<u32>,
}

#[derive(Default)]
pub struct FolderModel {
    view: RefCell<View>,
    notify: ModelNotify,
}

impl FolderModel {
    pub fn set(&self, folder: Option<Arc<Folder>>, visible: Vec<u32>) {
        *self.view.borrow_mut() = View { folder, visible };
        self.notify.reset();
    }

    pub fn clear(&self) {
        self.set(None, Vec::new());
    }

    /// Index d'entrée affiché à la ligne `row`.
    pub fn entry_at(&self, row: usize) -> Option<u32> {
        self.view.borrow().visible.get(row).copied()
    }

    /// Ligne où est affichée l'entrée `entry`.
    pub fn row_of(&self, entry: u32) -> Option<usize> {
        self.view.borrow().visible.iter().position(|&e| e == entry)
    }

    pub fn len(&self) -> usize {
        self.view.borrow().visible.len()
    }
}

impl Model for FolderModel {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.view.borrow().visible.len()
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        let view = self.view.borrow();
        let folder = view.folder.as_ref()?;
        let index = *view.visible.get(row)?;
        let e = folder.entry(index)?;
        Some(Row {
            name: folder.display_name(index).into(),
            modified: fx_win::locale::format_filetime(e.modified)
                .map(SharedString::from)
                .unwrap_or_default(),
            kind: folder.type_label(e).into(),
            size: if e.is_dir() {
                SharedString::new()
            } else {
                size_kb(e.size).into()
            },
            is_dir: e.is_dir(),
            dimmed: e.is_hidden(),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
