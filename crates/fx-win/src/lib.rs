//! Wrappers sûrs autour des API Win32, COM et Shell utilisées par l'explorateur.
//!
//! C'est le **seul** crate du workspace qui contient du code `unsafe`. Chaque bloc `unsafe`
//! porte un commentaire `// SAFETY:` justifiant son invariant (lint
//! `clippy::undocumented_unsafe_blocks` en deny).

#![cfg(windows)]

pub mod clipboard;
pub mod com;
pub mod context_menu;
pub mod dragdrop;
pub mod dwm;
pub mod fileop;
mod handle;
pub mod icons;
pub mod listing;
pub mod locale;
pub mod path;
mod pidl;
pub mod preview;
pub mod process;
pub mod shell;
pub mod shell_host;
pub mod watcher;
pub mod win_e;

pub use windows::core::Error as WinError;
