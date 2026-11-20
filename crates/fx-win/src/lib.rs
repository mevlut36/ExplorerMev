//! Wrappers sûrs autour des API Win32, COM et Shell utilisées par l'explorateur.
//!
//! C'est le **seul** crate du workspace qui contient du code `unsafe`. Chaque bloc `unsafe`
//! porte un commentaire `// SAFETY:` justifiant son invariant (lint
//! `clippy::undocumented_unsafe_blocks` en deny).

#![cfg(windows)]

pub mod com;
pub mod dwm;
mod handle;
pub mod listing;
pub mod locale;
pub mod path;
pub mod process;
pub mod shell;

pub use windows::core::Error as WinError;
