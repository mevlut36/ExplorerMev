//! Modèle métier de l'explorateur, indépendant de l'interface.
//!
//! Le système réel est abstrait par des traits ([`source::DirSource`], [`collate::Collator`]) :
//! tout le crate est testable sans fenêtre, et l'essentiel sans Windows. Les implémentations
//! Windows sont dans [`sys`] (feature `windows`, active par défaut).

pub mod collate;
pub mod filter;
pub mod folder;
pub mod format;
pub mod history;
pub mod loader;
pub mod selection;
pub mod size;
pub mod sort;
pub mod source;
#[cfg(all(windows, feature = "windows"))]
pub mod sys;

pub use folder::{Entry, Folder, RawEntry};
pub use sort::{SortColumn, SortSpec};
