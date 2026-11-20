//! Interface Slint de l'explorateur.
//!
//! Le thread d'interface ne fait que de l'affichage : le listing et le tri tournent dans
//! `fx_core::loader`, les appels Shell sur un thread STA dédié. Les résultats reviennent par
//! `slint::invoke_from_event_loop`.

/// Code généré par Slint depuis `ui/app.slint`. Les lints du projet (unwrap, unsafe documenté)
/// visent notre code, pas le code généré.
#[allow(
    clippy::all,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::undocumented_unsafe_blocks
)]
mod generated {
    slint::include_modules!();
}
pub use generated::*;

mod app;
mod model;
mod shell_worker;
mod window;

use std::path::PathBuf;
use std::time::Instant;

pub use app::BenchMode;

/// Options de lancement de l'interface.
#[derive(Debug, Clone)]
pub struct Options {
    /// Dossier affiché au démarrage.
    pub start: PathBuf,
    /// Moteur de rendu Slint : `software` (défaut) ou `femtovg`.
    pub renderer: String,
    pub mica: bool,
    pub bench: Option<BenchMode>,
    /// Instant de début de `main`, pour l'instrumentation du démarrage.
    pub main_start: Instant,
}

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("interface : {0}")]
    Platform(#[from] slint::PlatformError),
    #[error("sélection du moteur de rendu : {0}")]
    Backend(String),
}

/// Crée la fenêtre et exécute la boucle d'événements jusqu'à sa fermeture.
pub fn run(options: Options) -> Result<(), UiError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(options.renderer.clone())
        .select()
        .map_err(|e| UiError::Backend(e.to_string()))?;
    app::run(options)
}
