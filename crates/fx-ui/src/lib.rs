//! Interface Slint de l'explorateur.
//!
//! Le thread d'interface ne fait que de l'affichage : le listing et le tri tournent dans
//! `fx_core::loader`, les appels Shell sur l'hôte Shell (thread STA) ou des threads
//! d'opération. Les résultats reviennent par
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
mod bench;
mod icons;
mod model;
mod nav;
mod ops;
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
    /// Afficher les éléments cachés (réglage « Éléments masqués » de l'Explorateur).
    pub show_hidden: bool,
    /// Délai au-delà duquel un dossier qui ne répond pas (réseau) est abandonné.
    pub network_timeout: std::time::Duration,
    /// Avertissement à afficher au démarrage (configuration illisible…).
    pub startup_warning: Option<String>,
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
    // OLE sur le thread d'interface (glisser-déposer) avant la création de la fenêtre.
    let _ole = fx_win::com::OleApartment::init().map_err(|e| UiError::Backend(format!("OLE : {e}")))?;
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(options.renderer.clone())
        // Le glisser-déposer intégré de winit enregistrerait sa propre cible : on le désactive
        // pour enregistrer la nôtre (IDropTarget complet, voir fx_win::dragdrop).
        .with_winit_window_attributes_hook(|attrs| {
            use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_drag_and_drop(false)
        })
        .select()
        .map_err(|e| UiError::Backend(e.to_string()))?;
    app::run(options)
}
