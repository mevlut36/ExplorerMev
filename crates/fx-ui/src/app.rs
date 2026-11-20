//! Contrôleur de la fenêtre : onglets, double volet, routage des événements vers le volet
//! concerné, services globaux (hôte Shell, icônes, glisser-déposer), commandes (palette,
//! favoris, terminal, propriétés, tailles de dossiers), session et instrumentation.
//!
//! L'état et la logique d'une vue de dossier sont dans [`crate::pane`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fx_core::loader::Loader;
use fx_core::source::DirSource;
use fx_core::sys::{WinCollator, WinSource};
use fx_win::context_menu::MenuOptions;
use fx_win::shell_host::ShellHost;
use slint::platform::Key;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::icons::IconLoader;
use crate::model::FolderModel;
use crate::nav::NavEntry;
use crate::ops::Job;
use crate::pane::{Pane, post_pane, same_path, with_pane};
use crate::{MainWindow, NavItem, Options, PaletteItem, SearchItem, TabItem, UiError};

/// Modes de mesure (`--bench-startup`, `--bench-open`…).
#[derive(Debug, Clone)]
pub enum BenchMode {
    /// Mesure le démarrage puis quitte.
    Startup { out: Option<PathBuf> },
    /// Ouvre `path` plusieurs fois (en alternance avec le dossier de départ) et mesure.
    Open {
        path: PathBuf,
        iterations: usize,
        out: Option<PathBuf>,
    },
    /// Scénario d'auto-test dans un dossier jetable (sans entrée synthétique), puis quitte.
    SelfTest { out: Option<PathBuf> },
    /// Défilement automatisé du dossier de départ (molette, rapide, sauts), puis quitte.
    Scroll { maximized: bool, out: Option<PathBuf> },
    /// Recherche globale : frappe simulée au rythme d'un utilisateur, mémoire avec l'index
    /// chargé puis libéré, relecture du cache ; puis quitte.
    Search { out: Option<PathBuf> },
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub(crate) fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with_borrow(|a| a.clone());
    if let Some(app) = app {
        f(&app);
    }
}

pub(crate) fn with_app_ret<R>(f: impl FnOnce(&Rc<App>) -> R) -> Option<R> {
    let app = APP.with_borrow(|a| a.clone())?;
    Some(f(&app))
}

/// Accès à la fenêtre Slint (thread d'interface).
pub(crate) fn with_ui<R>(f: impl FnOnce(&MainWindow) -> R) -> Option<R> {
    let app = APP.with_borrow(|a| a.clone())?;
    Some(f(&app.ui))
}

/// Exécute `f` sur le thread d'interface (appelable depuis n'importe quel thread).
pub(crate) fn post(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    if let Err(e) = slint::invoke_from_event_loop(move || with_app(f)) {
        tracing::warn!("événement perdu, boucle d'interface arrêtée : {e}");
    }
}

struct BenchState {
    mode: BenchMode,
    start_path: PathBuf,
    first_frame_ms: Option<f64>,
    first_frame_done_ms: Option<f64>,
    initial_painted_ms: Option<f64>,
    iteration: usize,
    on_target: bool,
    results: Vec<crate::pane::NavTiming>,
    entries: usize,
    report: String,
}

/// Un onglet : un ou deux volets.
pub(crate) struct Tab {
    pub panes: [Rc<Pane>; 2],
    pub dual: Cell<bool>,
    /// Volet actif (0 ou 1).
    pub focus: Cell<usize>,
}

/// Onglet fermé, pour Ctrl+Maj+T.
struct ClosedTab {
    paths: [Option<PathBuf>; 2],
    dual: bool,
}

/// Commandes de la palette (Ctrl+Maj+P).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmd {
    NewTab,
    CloseTab,
    ReopenTab,
    NextTab,
    ToggleDual,
    SwitchPane,
    CopyToOther,
    MoveToOther,
    TogglePreview,
    ToggleView,
    ToggleHidden,
    Refresh,
    NewFolder,
    SelectAll,
    Rename,
    Delete,
    Properties,
    CopyPath,
    OpenTerminal,
    FolderSize,
    AddFavorite,
    RemoveFavorite,
    WinEOn,
    WinEOff,
    Search,
    IndexMft,
}

const COMMANDS: &[(Cmd, &str, &str)] = &[
    (Cmd::NewTab, "Nouvel onglet", "Ctrl+T"),
    (Cmd::CloseTab, "Fermer l'onglet", "Ctrl+W"),
    (Cmd::ReopenTab, "Rouvrir l'onglet fermé", "Ctrl+Maj+T"),
    (Cmd::NextTab, "Onglet suivant", "Ctrl+Tab"),
    (Cmd::ToggleDual, "Double volet", "F6"),
    (Cmd::SwitchPane, "Passer à l'autre volet", "Tab"),
    (Cmd::CopyToOther, "Copier vers l'autre volet", "F5 (double volet)"),
    (
        Cmd::MoveToOther,
        "Déplacer vers l'autre volet",
        "Maj+F5 (double volet)",
    ),
    (Cmd::TogglePreview, "Volet d'aperçu", "Alt+P"),
    (Cmd::ToggleView, "Affichage : détails ou icônes", "Ctrl+Maj+V"),
    (
        Cmd::ToggleHidden,
        "Afficher ou masquer les éléments cachés",
        "Ctrl+H",
    ),
    (Cmd::Refresh, "Actualiser", "F5 / Ctrl+R"),
    (Cmd::NewFolder, "Nouveau dossier", "Ctrl+Maj+N"),
    (Cmd::SelectAll, "Tout sélectionner", "Ctrl+A"),
    (Cmd::Rename, "Renommer", "F2"),
    (Cmd::Delete, "Supprimer (corbeille)", "Suppr"),
    (Cmd::Properties, "Propriétés", "Alt+Entrée"),
    (Cmd::CopyPath, "Copier le chemin d'accès", "Ctrl+Maj+C"),
    (Cmd::OpenTerminal, "Ouvrir dans le terminal", "Ctrl+Maj+`"),
    (Cmd::FolderSize, "Calculer la taille des dossiers", "Ctrl+Maj+S"),
    (Cmd::AddFavorite, "Ajouter aux favoris", "Ctrl+D"),
    (Cmd::RemoveFavorite, "Retirer des favoris", ""),
    (Cmd::Search, "Rechercher sur l'ordinateur", "Ctrl+Maj+F"),
    (
        Cmd::IndexMft,
        "Indexation rapide par la MFT (administrateur)…",
        "",
    ),
    (Cmd::WinEOn, "Remplacer Win+E par WinDir…", ""),
    (Cmd::WinEOff, "Rendre Win+E à l'Explorateur", ""),
];

pub(crate) struct App {
    pub ui: MainWindow,
    pub hwnd: Cell<Option<isize>>,
    pub show_hidden: Cell<bool>,
    icons_view: bool,
    pub network_timeout: Duration,
    pub drive_labels: RefCell<HashMap<PathBuf, String>>,
    mica: bool,
    host: Option<ShellHost>,
    icons: Arc<IconLoader>,
    icon_px: Cell<u32>,
    source: Arc<WinSource>,
    nav_entries: RefCell<Vec<NavEntry>>,
    favorites: RefCell<Vec<PathBuf>>,
    data_dir: Option<PathBuf>,
    tabs: RefCell<Vec<Rc<Tab>>>,
    active_tab: Cell<usize>,
    closed_tabs: RefCell<Vec<ClosedTab>>,
    next_id: Cell<u64>,
    suggest_seq: Cell<u64>,
    session_seq: Cell<u64>,
    palette: RefCell<Vec<Cmd>>,
    index_dir: Option<PathBuf>,
    index_mft: bool,
    /// Moteur de recherche, démarré à la première ouverture de la recherche.
    search: RefCell<Option<fx_core::search_engine::Engine>>,
    search_gen: Cell<u64>,
    search_sent: Cell<Option<Instant>>,
    /// Une requête attend ses résultats.
    search_waiting: Cell<bool>,
    search_rows: RefCell<Vec<fx_core::search_engine::SearchRow>>,
    search_index_status: RefCell<String>,
    /// (durée de la recherche, délai frappe → résultats) en ms, pour les mesures.
    pub(crate) search_latencies: RefCell<Vec<(f64, f64)>>,
    /// Auto-test : début de l'attente de l'index.
    search_wait: RefCell<Option<Instant>>,
    /// Auto-test : début de l'attente de l'étape en cours.
    self_test_since: RefCell<Option<Instant>>,
    preview_visible: Cell<bool>,
    preview_seq: Cell<u64>,
    preview_host: RefCell<Option<ShellHost>>,
    preview_native: Cell<bool>,
    preview_last_rect: Cell<fx_win::preview::Rect>,
    drop_reg: RefCell<Option<fx_win::dragdrop::DropRegistration>>,
    drop_pane: Cell<Option<u64>>,
    frames: Cell<u64>,
    main_start: Instant,
    bench: RefCell<Option<BenchState>>,
    scroll: RefCell<Option<crate::bench::ScrollRun>>,
    latency: Option<Arc<crate::bench::LatencyProbe>>,
}

pub(crate) fn run(options: Options) -> Result<(), UiError> {
    let ui = MainWindow::new()?;
    ui.set_mica(options.mica);
    let icons = Arc::new(IconLoader::start(2, |batch| {
        post(move |a| a.deliver_icons(&batch));
    }));
    let host = match ShellHost::start("fx-shell") {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::error!("hôte Shell indisponible : {e}");
            None
        }
    };
    let bench = options.bench.clone().map(|mode| BenchState {
        mode,
        start_path: options.start.clone(),
        first_frame_ms: None,
        first_frame_done_ms: None,
        initial_painted_ms: None,
        iteration: 0,
        on_target: false,
        results: Vec::new(),
        entries: 0,
        report: String::new(),
    });
    let app = Rc::new(App {
        ui: ui.clone_strong(),
        hwnd: Cell::new(None),
        show_hidden: Cell::new(options.show_hidden),
        icons_view: options.icons_view,
        network_timeout: options.network_timeout,
        drive_labels: RefCell::default(),
        mica: options.mica,
        host,
        icons,
        icon_px: Cell::new(16),
        source: Arc::new(WinSource::default()),
        nav_entries: RefCell::default(),
        favorites: RefCell::new(load_favorites(options.data_dir.as_deref())),
        data_dir: options.data_dir.clone(),
        tabs: RefCell::default(),
        active_tab: Cell::new(0),
        closed_tabs: RefCell::default(),
        next_id: Cell::new(1),
        suggest_seq: Cell::new(0),
        session_seq: Cell::new(0),
        palette: RefCell::default(),
        index_dir: options.index_dir.clone(),
        index_mft: options.index_mft,
        search: RefCell::new(None),
        search_gen: Cell::new(0),
        search_sent: Cell::new(None),
        search_waiting: Cell::new(false),
        search_rows: RefCell::default(),
        search_index_status: RefCell::default(),
        search_latencies: RefCell::default(),
        search_wait: RefCell::default(),
        self_test_since: RefCell::default(),
        preview_visible: Cell::new(false),
        preview_seq: Cell::new(0),
        preview_host: RefCell::new(None),
        preview_native: Cell::new(false),
        preview_last_rect: Cell::new(fx_win::preview::Rect::default()),
        drop_reg: RefCell::new(None),
        drop_pane: Cell::new(None),
        frames: Cell::new(0),
        main_start: options.main_start,
        bench: RefCell::new(bench),
        scroll: RefCell::new(None),
        latency: options.bench.is_some().then(crate::bench::LatencyProbe::start),
    });
    APP.with_borrow_mut(|a| *a = Some(app.clone()));

    wire_callbacks(&ui);
    {
        let mut ready = false;
        let maximize = matches!(options.bench, Some(BenchMode::Scroll { maximized: true, .. }));
        ui.window().on_winit_window_event(move |w, ev| {
            if !ready {
                // Premier événement : la fenêtre native existe.
                ready = true;
                if maximize {
                    w.with_winit_window(|ww| ww.set_maximized(true));
                }
                let theme = w.with_winit_window(|ww| ww.theme()).flatten();
                with_app(|a| a.on_native_window(w, theme));
            }
            match ev {
                WindowEvent::ThemeChanged(theme) => with_app(|a| {
                    if let Some(h) = a.hwnd.get() {
                        crate::window::apply_style(h, Some(*theme), a.mica);
                    }
                }),
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    let px = (16.0 * scale_factor).round() as u32;
                    with_app(|a| a.set_icon_px(px));
                }
                WindowEvent::RedrawRequested => with_app(|a| a.on_redraw()),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    crate::nav::build(|entries| post(move |a| a.set_nav(entries)));
    // Session précédente, sinon un onglet sur le dossier de départ. Le listing démarre avant
    // la création de la fenêtre.
    let restored = options.restore_session && app.restore_session();
    if !restored {
        app.open_tab(options.start.clone(), None, false, true);
    }
    ui.show()?;
    app.active_pane().ui_focus_list();
    if let Some(w) = options.startup_warning.clone() {
        app.active_pane().flash_status(w);
    }
    slint::run_event_loop()?;
    if options.restore_session {
        app.save_session();
    }
    app.drop_reg.borrow_mut().take();
    APP.with_borrow_mut(|a| *a = None);
    Ok(())
}

fn wire_callbacks(ui: &MainWindow) {
    fn pane_at(slot: i32) -> Option<Rc<Pane>> {
        with_app_ret(|a| a.pane_in_slot(slot)).flatten()
    }
    ui.on_navigate_to(|text| with_app(|a| a.active_pane().navigate_to(&text)));
    ui.on_address_edited(|text| with_app(|a| a.address_edited(text.to_string())));
    ui.on_go_back(|| with_app(|a| a.active_pane().go_back()));
    ui.on_go_forward(|| with_app(|a| a.active_pane().go_forward()));
    ui.on_go_up(|| with_app(|a| a.active_pane().go_up()));
    ui.on_refresh(|| with_app(|a| a.active_pane().refresh()));
    ui.on_nav_activated(|p| {
        with_app(|a| {
            a.active_pane()
                .navigate(PathBuf::from(p.as_str()), true, Vec::new())
        })
    });
    ui.on_crumb_activated(|p| with_app(|a| a.active_pane().navigate_to(&p)));
    ui.on_filter_edited(|text| with_app(|a| a.active_pane().filter_edited(&text)));
    ui.on_tab_activated(|i| with_app(|a| a.activate_tab(i as usize)));
    ui.on_tab_closed(|i| with_app(|a| a.close_tab(i as usize)));
    ui.on_new_tab(|| with_app(|a| a.new_tab_here()));
    ui.on_toggle_dual(|| with_app(|a| a.toggle_dual()));
    ui.on_toggle_preview(|| with_app(|a| a.toggle_preview()));
    ui.on_pane_activated(|slot| with_app(|a| a.set_active_pane(slot as usize)));
    ui.on_sort_by(|slot, col| {
        if let Some(p) = pane_at(slot) {
            p.sort_by(col);
        }
    });
    ui.on_row_pointer(|slot, row, kind, ctrl, shift, on_name| {
        if let Some(p) = pane_at(slot) {
            p.row_pointer(row, kind, ctrl, shift, on_name);
        }
    });
    ui.on_background_pointer(|slot, kind, ctrl, shift| {
        if let Some(p) = pane_at(slot) {
            p.background_pointer(kind, ctrl, shift);
        }
    });
    ui.on_drag_out(|slot, row| {
        if let Some(p) = pane_at(slot) {
            p.drag_out(row);
        }
    });
    ui.on_marquee_changed(|slot, ra, rb, ctrl| {
        if let Some(p) = pane_at(slot) {
            p.marquee_changed(ra, rb, ctrl);
        }
    });
    ui.on_marquee_grid(|slot, la, lb, ca, cb| {
        if let Some(p) = pane_at(slot) {
            p.marquee_grid(la, lb, ca, cb);
        }
    });
    ui.on_columns_changed(|slot, cols| {
        if let Some(p) = pane_at(slot) {
            p.model.set_columns(cols.max(1) as usize);
        }
    });
    ui.on_toggle_view(|| with_app(|a| a.run_cmd(Cmd::ToggleView)));
    ui.on_marquee_ended(|slot| {
        if let Some(p) = pane_at(slot) {
            p.st.borrow_mut().marquee_base = None;
        }
    });
    ui.on_rename_commit(|slot, t| {
        if let Some(p) = pane_at(slot) {
            p.rename_commit(&t);
        }
    });
    ui.on_rename_cancel(|slot| {
        if let Some(p) = pane_at(slot) {
            p.rename_cancel();
        }
    });
    ui.on_key(|slot, text, ctrl, shift, alt| {
        with_app_ret(|a| a.on_key(slot, &text, ctrl, shift, alt)).unwrap_or(false)
    });
    ui.on_palette_edited(|q| with_app(|a| a.palette_filter(&q)));
    ui.on_palette_run(|i| with_app(|a| a.palette_run(i)));
    ui.on_search_opened(|| with_app(|a| a.search_opened()));
    ui.on_search_edited(|q| with_app(|a| a.search_edited(&q)));
    ui.on_search_run(|i, open| with_app(|a| a.search_run(i, open)));
}

/// Cible de dépôt de la fenêtre : délègue au volet sous le pointeur (thread d'interface).
struct UiDrop;

impl fx_win::dragdrop::DropHandler for UiDrop {
    fn hover(&self, x: i32, y: i32) -> Option<PathBuf> {
        with_app_ret(|a| a.drop_hover(x, y)).flatten()
    }
    fn leave(&self) {
        with_app(|a| a.drop_leave());
    }
    fn drop_files(&self, dest: PathBuf, paths: Vec<PathBuf>, effect: u32) {
        with_app(|a| {
            let pane = a.drop_pane.get().and_then(|id| a.find_pane(id));
            a.drop_leave();
            if let Some(p) = pane {
                p.drop_files(dest, paths, effect);
            }
        });
    }
}

fn load_favorites(dir: Option<&Path>) -> Vec<PathBuf> {
    let Some(dir) = dir else { return Vec::new() };
    std::fs::read_to_string(dir.join("favoris.txt"))
        .map(|t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

impl App {
    // ------------------------------------------------------------------ fenêtre native

    fn on_native_window(
        self: &Rc<Self>,
        w: &slint::Window,
        theme: Option<slint::winit_030::winit::window::Theme>,
    ) {
        self.hwnd.set(crate::window::hwnd(w));
        self.set_icon_px((16.0 * w.scale_factor()).round() as u32);
        let Some(h) = self.hwnd.get() else { return };
        crate::window::apply_style(h, theme, self.mica);
        match fx_win::dragdrop::register(h, Box::new(UiDrop)) {
            Ok(reg) => *self.drop_reg.borrow_mut() = Some(reg),
            Err(e) => tracing::warn!("cible de glisser-déposer non enregistrée : {e}"),
        }
    }

    fn set_icon_px(&self, px: u32) {
        self.icon_px.set(px);
        for p in self.all_panes() {
            p.model.set_icon_px(px);
        }
    }

    fn deliver_icons(&self, batch: &crate::icons::Delivered) {
        for p in self.all_panes() {
            p.model.on_icons(batch);
        }
    }

    pub(crate) fn host_post_raw(
        &self,
        job: impl FnOnce(&fx_win::shell_host::HostCtx) + Send + 'static,
    ) -> bool {
        self.host.as_ref().is_some_and(|h| h.post(job))
    }

    // ------------------------------------------------------------------ onglets et volets

    fn new_pane(&self) -> Rc<Pane> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let source: Arc<dyn DirSource> = self.source.clone();
        let loader = Arc::new(Loader::new(source, Arc::new(WinCollator)));
        let model = Rc::new(FolderModel::new(self.icons.clone()));
        model.set_icon_px(self.icon_px.get());
        let pane = Rc::new(Pane::new(id, loader, model));
        if self.icons_view {
            pane.set_grid(true);
        }
        pane
    }

    fn all_panes(&self) -> Vec<Rc<Pane>> {
        self.tabs
            .borrow()
            .iter()
            .flat_map(|t| t.panes.iter().cloned())
            .collect()
    }

    pub(crate) fn find_pane(&self, id: u64) -> Option<Rc<Pane>> {
        self.tabs
            .borrow()
            .iter()
            .flat_map(|t| t.panes.iter())
            .find(|p| p.id == id)
            .cloned()
    }

    pub(crate) fn active_tab(&self) -> Rc<Tab> {
        let tabs = self.tabs.borrow();
        tabs.get(self.active_tab.get())
            .or_else(|| tabs.first())
            .cloned()
            .unwrap_or_else(|| {
                // Invariant : il y a toujours au moins un onglet après le démarrage. Par sécurité, un
                // onglet vide (non affiché) plutôt qu'un panic.
                Rc::new(Tab {
                    panes: [self.new_pane(), self.new_pane()],
                    dual: Cell::new(false),
                    focus: Cell::new(0),
                })
            })
    }

    pub(crate) fn active_pane(&self) -> Rc<Pane> {
        let t = self.active_tab();
        t.panes[if t.dual.get() { t.focus.get() } else { 0 }].clone()
    }

    fn other_pane(&self) -> Option<Rc<Pane>> {
        let t = self.active_tab();
        t.dual.get().then(|| t.panes[1 - t.focus.get()].clone())
    }

    /// Volet affiché à l'emplacement `slot` (0 ou 1) de l'onglet actif.
    pub(crate) fn pane_in_slot(&self, slot: i32) -> Option<Rc<Pane>> {
        let t = self.active_tab();
        match slot {
            0 => Some(t.panes[0].clone()),
            1 if t.dual.get() => Some(t.panes[1].clone()),
            _ => None,
        }
    }

    /// Ouvre un onglet sur `path` (et `second` pour le volet droit, si double volet).
    fn open_tab(self: &Rc<Self>, path: PathBuf, second: Option<PathBuf>, dual: bool, activate: bool) {
        let tab = Rc::new(Tab {
            panes: [self.new_pane(), self.new_pane()],
            dual: Cell::new(dual),
            focus: Cell::new(0),
        });
        tab.panes[0].navigate(path, true, Vec::new());
        if let Some(p) = second {
            tab.panes[1].navigate(p, true, Vec::new());
        }
        let index = {
            let mut tabs = self.tabs.borrow_mut();
            let at = (self.active_tab.get() + 1).min(tabs.len());
            tabs.insert(at, tab);
            at
        };
        if activate || self.tabs.borrow().len() == 1 {
            self.activate_tab(index);
        } else {
            self.refresh_tabs_ui();
        }
    }

    fn new_tab_here(self: &Rc<Self>) {
        let path = self
            .active_pane()
            .current_path()
            .unwrap_or_else(|| PathBuf::from(r"C:\"));
        self.open_tab(path, None, false, true);
    }

    fn activate_tab(self: &Rc<Self>, index: usize) {
        if index >= self.tabs.borrow().len() {
            return;
        }
        let old = self.active_tab();
        self.close_native_preview();
        if self.active_tab.get() != index || old.panes[0].slot.get().is_none() {
            for p in &old.panes {
                p.unbind();
            }
            self.active_tab.set(index);
        }
        self.show_tab();
    }

    /// Rattache les volets de l'onglet actif aux emplacements de la fenêtre.
    fn show_tab(self: &Rc<Self>) {
        let t = self.active_tab();
        let dual = t.dual.get();
        if dual && t.panes[1].current_path().is_none() {
            // Premier passage en double volet : le volet droit s'ouvre sur le même dossier.
            if let Some(p) = t.panes[0].current_path() {
                t.panes[1].navigate(p, true, Vec::new());
            }
        }
        if !dual {
            t.focus.set(0);
            t.panes[1].unbind();
        }
        t.panes[0].bind(0);
        if dual {
            t.panes[1].bind(1);
        }
        self.ui.set_dual(dual);
        self.ui.set_active_pane(t.focus.get() as i32);
        let active = self.active_pane();
        active.sync_global();
        self.refresh_tabs_ui();
        self.update_preview();
        let id = active.id;
        slint::Timer::single_shot(Duration::ZERO, move || with_pane(id, |p| p.ui_focus_list()));
    }

    fn set_active_pane(self: &Rc<Self>, slot: usize) {
        let t = self.active_tab();
        if !t.dual.get() || t.focus.get() == slot {
            return;
        }
        t.focus.set(slot);
        self.ui.set_active_pane(slot as i32);
        self.active_pane().sync_global();
        self.update_preview();
    }

    fn toggle_dual(self: &Rc<Self>) {
        let t = self.active_tab();
        t.dual.set(!t.dual.get());
        self.show_tab();
        self.schedule_session_save();
    }

    fn switch_pane(self: &Rc<Self>) {
        let t = self.active_tab();
        if t.dual.get() {
            let slot = 1 - t.focus.get();
            self.set_active_pane(slot);
            t.panes[slot].ui_focus_list();
        }
    }

    fn close_tab(self: &Rc<Self>, index: usize) {
        let len = self.tabs.borrow().len();
        if index >= len {
            return;
        }
        if len == 1 {
            // Dernier onglet : on ferme la fenêtre, comme l'Explorateur.
            self.save_session();
            let _ = slint::quit_event_loop();
            return;
        }
        let tab = self.tabs.borrow_mut().remove(index);
        for p in &tab.panes {
            p.unbind();
            p.loader.cancel();
        }
        self.closed_tabs.borrow_mut().push(ClosedTab {
            paths: [tab.panes[0].current_path(), tab.panes[1].current_path()],
            dual: tab.dual.get(),
        });
        let active = self.active_tab.get();
        let new_active = if index < active || active >= len - 1 {
            active.saturating_sub(1)
        } else {
            active
        };
        self.active_tab.set(new_active.min(len - 2));
        // L'onglet affiché a changé : on force le rattachement.
        for p in self.active_tab().panes.iter() {
            p.unbind();
        }
        self.show_tab();
        self.schedule_session_save();
    }

    fn reopen_closed_tab(self: &Rc<Self>) {
        let Some(c) = self.closed_tabs.borrow_mut().pop() else {
            return;
        };
        let [first, second] = c.paths;
        if let Some(p) = first {
            self.open_tab(p, second, c.dual, true);
        }
    }

    fn cycle_tab(self: &Rc<Self>, delta: isize) {
        let len = self.tabs.borrow().len() as isize;
        if len > 1 {
            let i = (self.active_tab.get() as isize + delta).rem_euclid(len) as usize;
            self.activate_tab(i);
        }
    }

    fn refresh_tabs_ui(&self) {
        let active = self.active_tab.get();
        let items: Vec<TabItem> = self
            .tabs
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let p = &t.panes[if t.dual.get() { t.focus.get() } else { 0 }];
                let title = p
                    .current_path()
                    .map(|path| {
                        let shown = fx_win::path::display_form(&path);
                        shown
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| shown.display().to_string())
                    })
                    .unwrap_or_default();
                TabItem {
                    title: title.into(),
                    active: i == active,
                }
            })
            .collect();
        self.ui.set_tabs(ModelRc::new(VecModel::from(items)));
    }

    /// Un volet a changé de dossier : titres d'onglets et session.
    pub(crate) fn on_pane_location_changed(self: &Rc<Self>) {
        self.refresh_tabs_ui();
        self.schedule_session_save();
    }

    // ------------------------------------------------------------------ session

    fn session_file(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("session.txt"))
    }

    fn schedule_session_save(self: &Rc<Self>) {
        let seq = self.session_seq.get() + 1;
        self.session_seq.set(seq);
        slint::Timer::single_shot(Duration::from_secs(1), move || {
            with_app(|a| {
                if a.session_seq.get() == seq {
                    a.save_session();
                }
            })
        });
    }

    /// Format texte, une ligne par onglet : `onglet\t<double>\t<volet actif>\t<chemin>\t<chemin>`
    /// (la tabulation est interdite dans les chemins Windows).
    fn save_session(&self) {
        if self.bench.borrow().is_some() {
            return;
        }
        let Some(file) = self.session_file() else { return };
        let mut out = format!("windir-session 1\nactif\t{}\n", self.active_tab.get());
        for t in self.tabs.borrow().iter() {
            let path = |p: &Pane| {
                p.current_path()
                    .map(|x| x.display().to_string())
                    .unwrap_or_default()
            };
            out += &format!(
                "onglet\t{}\t{}\t{}\t{}\n",
                t.dual.get() as u8,
                t.focus.get(),
                path(&t.panes[0]),
                path(&t.panes[1])
            );
        }
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&file, out) {
            tracing::warn!("session non enregistrée : {e}");
        }
    }

    fn restore_session(self: &Rc<Self>) -> bool {
        let Some(text) = self.session_file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return false;
        };
        let mut active = 0usize;
        let mut count = 0;
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            match parts.as_slice() {
                ["actif", i] => active = i.parse().unwrap_or(0),
                ["onglet", dual, focus, p0, p1] if !p0.is_empty() => {
                    let second = (!p1.is_empty()).then(|| PathBuf::from(p1));
                    self.open_tab(PathBuf::from(p0), second, *dual == "1", false);
                    if let Some(t) = self.tabs.borrow().last() {
                        t.focus.set(focus.parse::<usize>().unwrap_or(0).min(1));
                    }
                    count += 1;
                }
                _ => {}
            }
        }
        if count == 0 {
            return false;
        }
        // `open_tab` insère après l'onglet actif : on remet l'ordre d'origine.
        self.active_tab.set(0);
        self.activate_tab(active.min(count - 1));
        true
    }

    // ------------------------------------------------------------------ volet de navigation

    fn set_nav(self: &Rc<Self>, entries: Vec<NavEntry>) {
        {
            let mut labels = self.drive_labels.borrow_mut();
            for e in entries.iter().filter(|e| !e.header && e.path.parent().is_none()) {
                labels.insert(e.path.clone(), e.label.clone());
            }
        }
        *self.nav_entries.borrow_mut() = entries;
        self.active_pane().sync_global();
    }

    pub(crate) fn refresh_nav_items(&self, current: &Path) {
        let mut items: Vec<NavItem> = Vec::new();
        let favorites = self.favorites.borrow();
        if !favorites.is_empty() {
            items.push(NavItem {
                label: "Favoris".into(),
                glyph: "".into(),
                path: "".into(),
                header: true,
                current: false,
            });
            for f in favorites.iter() {
                let shown = fx_win::path::display_form(f);
                let label = shown
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| shown.display().to_string());
                items.push(NavItem {
                    label: label.into(),
                    glyph: "\u{E734}".into(),
                    path: f.display().to_string().into(),
                    header: false,
                    current: same_path(f, current),
                });
            }
        }
        items.extend(self.nav_entries.borrow().iter().map(|e| NavItem {
            label: e.label.clone().into(),
            glyph: e.glyph.into(),
            path: e.path.display().to_string().into(),
            header: e.header,
            current: !e.header && same_path(&e.path, current),
        }));
        self.ui.set_nav_items(ModelRc::new(VecModel::from(items)));
    }

    fn address_edited(self: &Rc<Self>, text: String) {
        let seq = self.suggest_seq.get() + 1;
        self.suggest_seq.set(seq);
        let _ = std::thread::Builder::new()
            .name("fx-suggest".into())
            .spawn(move || {
                let list = crate::nav::suggestions(&text, 10);
                post(move |a| {
                    if a.suggest_seq.get() == seq {
                        let items: Vec<SharedString> = list.into_iter().map(SharedString::from).collect();
                        a.ui.set_suggestions(ModelRc::new(VecModel::from(items)));
                    }
                });
            });
    }

    pub(crate) fn toggle_hidden(self: &Rc<Self>) {
        let show = !self.show_hidden.get();
        self.show_hidden.set(show);
        for p in self.all_panes() {
            p.set_show_hidden(show);
        }
        self.active_pane().flash_status(
            if show {
                "Éléments masqués affichés"
            } else {
                "Éléments masqués cachés"
            }
            .into(),
        );
    }

    // ------------------------------------------------------------------ glisser-déposer

    fn drop_hover(self: &Rc<Self>, x: i32, y: i32) -> Option<PathBuf> {
        let scale = self.ui.window().scale_factor();
        let (lx, ly) = (x as f32 / scale, y as f32 / scale);
        let slot = self.ui.invoke_pane_at(lx, ly);
        let target = self.pane_in_slot(slot);
        // Efface la surbrillance de l'autre volet.
        for p in self.active_tab().panes.iter() {
            if target.as_ref().is_none_or(|t| t.id != p.id) {
                p.ui_drop_row(-1);
            }
        }
        let pane = target?;
        self.drop_pane.set(Some(pane.id));
        let row = self.ui.invoke_row_at(slot, lx, ly);
        pane.drop_hover(row)
    }

    fn drop_leave(&self) {
        self.drop_pane.set(None);
        for p in self.active_tab().panes.iter() {
            p.ui_drop_row(-1);
        }
    }

    // ------------------------------------------------------------------ clavier

    fn on_key(self: &Rc<Self>, slot: i32, text: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        let pane = self.pane_in_slot(slot).unwrap_or_else(|| self.active_pane());
        if pane.st.borrow().rename_entry.is_some() {
            return false;
        }
        let Some(mut c) = text.chars().next() else {
            return false;
        };
        if ctrl && ('\u{1}'..='\u{1a}').contains(&c) {
            c = (b'a' + (c as u8 - 1)) as char;
        }
        let is = |k: Key| c == char::from(k);
        let dual = self.active_tab().dual.get();
        let lower = c.to_ascii_lowercase();
        let cmd = if ctrl && !alt {
            match lower {
                't' if shift => Some(Cmd::ReopenTab),
                't' => Some(Cmd::NewTab),
                'w' => Some(Cmd::CloseTab),
                '\t' => {
                    self.cycle_tab(if shift { -1 } else { 1 });
                    return true;
                }
                'r' => Some(Cmd::Refresh),
                'd' => Some(Cmd::AddFavorite),
                'c' if shift => Some(Cmd::CopyPath),
                'v' if shift => Some(Cmd::ToggleView),
                's' if shift => Some(Cmd::FolderSize),
                '`' if shift => Some(Cmd::OpenTerminal),
                _ => None,
            }
        } else if alt && !ctrl {
            if lower == 'p' {
                Some(Cmd::TogglePreview)
            } else if is(Key::Return) {
                Some(Cmd::Properties)
            } else {
                None
            }
        } else if is(Key::F6) && !shift {
            Some(Cmd::ToggleDual)
        } else if dual && is(Key::Tab) && !shift {
            Some(Cmd::SwitchPane)
        } else if dual && is(Key::F5) {
            Some(if shift { Cmd::MoveToOther } else { Cmd::CopyToOther })
        } else {
            None
        };
        if let Some(cmd) = cmd {
            self.run_cmd(cmd);
            return true;
        }
        pane.on_key(text, ctrl, shift, alt)
    }

    // ------------------------------------------------------------------ commandes

    // ------------------------------------------------------------------ recherche globale

    /// Premier affichage de la recherche : démarre le moteur (cache de l'index, puis mise à
    /// jour en arrière-plan). L'index n'occupe la mémoire qu'à partir de là.
    /// Démarre le moteur de recherche s'il ne tourne pas ; `false` s'il ne peut pas démarrer.
    fn ensure_search_engine(&self) -> bool {
        if self.search.borrow().is_some() {
            return true;
        }
        let Some(dir) = self.index_dir.clone() else {
            self.ui
                .set_search_status("Dossier du cache d'index introuvable.".into());
            return false;
        };
        let volumes = fx_win::shell::fixed_ntfs_volumes();
        self.ui.set_search_status("Chargement de l'index…".into());
        let engine = fx_core::search_engine::Engine::start(dir, volumes, |ev| {
            post(move |a| a.on_search_event(ev));
        });
        // Réglage `index_mft = true` : l'utilisateur a choisi l'indexation par la MFT.
        if self.index_mft
            && let Ok(exe) = std::env::current_exe()
        {
            engine.start_mft(exe);
        }
        *self.search.borrow_mut() = Some(engine);
        true
    }

    /// Indexation rapide par la MFT : confirmation, puis assistant élevé (invite UAC).
    fn index_with_mft(self: &Rc<Self>) {
        let owner = self.hwnd.get().unwrap_or(0);
        let posted = self.host_post_raw(move |_| {
            let ok = fx_win::shell::confirm(
                owner,
                "Indexation rapide (MFT)",
                "WinDir peut lire directement la table des fichiers des volumes NTFS (MFT) et \
                 leur journal de modifications : l'index est construit en quelques secondes et \
                 reste à jour sans surveiller les dossiers.\n\nCela demande les droits \
                 d'administration : Windows va afficher une demande de confirmation. Un petit \
                 assistant tourne alors avec ces droits jusqu'à la fermeture de WinDir ; il ne \
                 fait que lire et transmettre les noms des fichiers, et seulement ceux des \
                 dossiers que votre compte peut ouvrir.\n\nContinuer ?",
            );
            if ok {
                post(|a| {
                    if a.ensure_search_engine()
                        && let (Some(e), Ok(exe)) = (a.search.borrow().as_ref(), std::env::current_exe())
                    {
                        e.start_mft(exe);
                    }
                });
            }
        });
        if !posted {
            self.active_pane()
                .flash_status("Le service Shell est indisponible.".into());
        }
    }

    fn search_opened(self: &Rc<Self>) {
        if self.search.borrow().is_some() {
            if let Some(e) = self.search.borrow().as_ref() {
                e.warm();
            }
        } else if !self.ensure_search_engine() {
            return;
        }
        let q = self.ui.get_search_query();
        if !q.trim().is_empty() {
            self.search_edited(&q);
        } else {
            self.ui
                .set_search_status(self.search_index_status.borrow().clone().into());
        }
    }

    fn search_edited(&self, text: &str) {
        if text.trim().is_empty() {
            self.search_gen.set(0);
            self.search_rows.borrow_mut().clear();
            self.ui.set_search_items(ModelRc::default());
            self.ui
                .set_search_status(self.search_index_status.borrow().clone().into());
            return;
        }
        if let Some(e) = self.search.borrow().as_ref() {
            self.search_gen.set(e.query(text));
            self.search_sent.set(Some(Instant::now()));
            self.search_waiting.set(true);
        }
    }

    fn on_search_event(&self, ev: fx_core::search_engine::EngineEvent) {
        use fx_core::search_engine::EngineEvent;
        match ev {
            EngineEvent::Status(s) => {
                *self.search_index_status.borrow_mut() = s.clone();
                if self.search_rows.borrow().is_empty() {
                    self.ui.set_search_status(s.into());
                }
                // Index prêt ou mis à jour : la requête affichée est refaite (sauf si une
                // requête est déjà en cours : elle verra l'index à jour).
                let q = self.ui.get_search_query();
                if self.ui.get_search_open() && !q.trim().is_empty() && !self.search_waiting.get() {
                    self.search_edited(&q);
                }
            }
            EngineEvent::Results {
                generation,
                total,
                rows,
                millis,
            } => {
                if generation != self.search_gen.get() {
                    return;
                }
                self.search_waiting.set(false);
                let latency = self.search_sent.get().map(|t| t.elapsed().as_secs_f64() * 1000.0);
                if let Some(l) = latency {
                    self.search_latencies.borrow_mut().push((millis, l));
                }
                let items: Vec<SearchItem> = rows
                    .iter()
                    .map(|r| SearchItem {
                        name: r.name.clone().into(),
                        folder: std::path::Path::new(&r.path)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                            .into(),
                        is_dir: r.is_dir,
                        dimmed: r.hidden,
                    })
                    .collect();
                let shown = items.len();
                *self.search_rows.borrow_mut() = rows;
                self.ui.set_search_items(ModelRc::new(VecModel::from(items)));
                let count = fx_core::format::group_thousands(total as u64);
                let more = if total > shown {
                    format!(" (les {shown} premiers affichés)")
                } else {
                    String::new()
                };
                self.ui
                    .set_search_status(format!("{count} résultat(s){more} · {millis:.0} ms").into());
            }
        }
    }

    fn search_run(self: &Rc<Self>, index: i32, open: bool) {
        let Some(row) = usize::try_from(index)
            .ok()
            .and_then(|i| self.search_rows.borrow().get(i).cloned())
        else {
            return;
        };
        let path = PathBuf::from(&row.path);
        let pane = self.active_pane();
        pane.ui_focus_list();
        if open && row.is_dir {
            pane.navigate(path, true, Vec::new());
        } else if open {
            let owner = self.hwnd.get();
            let id = pane.id;
            self.host_post_raw(move |_| {
                if let Err(e) = fx_win::shell::open(&path, owner) {
                    let msg = format!("Impossible d'ouvrir « {} » : {}", path.display(), e.message());
                    post_pane(id, move |p| p.flash_status(msg));
                }
            });
        } else if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            let name: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(name).collect();
            pane.navigate(parent.to_path_buf(), true, vec![name]);
        }
    }

    fn palette_filter(&self, query: &str) {
        let q = query.trim().to_lowercase();
        let matches: Vec<Cmd> = COMMANDS
            .iter()
            .filter(|(_, label, _)| q.split_whitespace().all(|w| label.to_lowercase().contains(w)))
            .map(|(c, _, _)| *c)
            .collect();
        let items: Vec<PaletteItem> = matches
            .iter()
            .filter_map(|c| COMMANDS.iter().find(|(x, _, _)| x == c))
            .map(|(_, label, hint)| PaletteItem {
                label: (*label).into(),
                hint: (*hint).into(),
            })
            .collect();
        *self.palette.borrow_mut() = matches;
        self.ui.set_palette_items(ModelRc::new(VecModel::from(items)));
    }

    fn palette_run(self: &Rc<Self>, index: i32) {
        let cmd = usize::try_from(index)
            .ok()
            .and_then(|i| self.palette.borrow().get(i).copied());
        let pane = self.active_pane();
        pane.ui_focus_list();
        if let Some(cmd) = cmd {
            self.run_cmd(cmd);
        }
    }

    fn run_cmd(self: &Rc<Self>, cmd: Cmd) {
        let pane = self.active_pane();
        match cmd {
            Cmd::NewTab => self.new_tab_here(),
            Cmd::CloseTab => self.close_tab(self.active_tab.get()),
            Cmd::ReopenTab => self.reopen_closed_tab(),
            Cmd::NextTab => self.cycle_tab(1),
            Cmd::ToggleDual => self.toggle_dual(),
            Cmd::SwitchPane => self.switch_pane(),
            Cmd::CopyToOther | Cmd::MoveToOther => self.transfer_to_other(cmd == Cmd::MoveToOther),
            Cmd::TogglePreview => self.toggle_preview(),
            Cmd::ToggleView => pane.toggle_view(),
            Cmd::ToggleHidden => self.toggle_hidden(),
            Cmd::Refresh => pane.refresh(),
            Cmd::NewFolder => pane.new_folder(),
            Cmd::SelectAll => {
                pane.model.update_selection(|s, v| s.select_all(v));
                pane.update_status();
            }
            Cmd::Rename => pane.begin_rename(),
            Cmd::Delete => pane.delete_selection(false),
            Cmd::Properties => self.properties(),
            Cmd::CopyPath => self.copy_path(),
            Cmd::OpenTerminal => self.open_terminal(),
            Cmd::FolderSize => self.folder_sizes(),
            Cmd::AddFavorite => self.add_favorite(),
            Cmd::RemoveFavorite => self.remove_favorite(),
            Cmd::Search => self.ui.invoke_open_search(),
            Cmd::IndexMft => self.index_with_mft(),
            Cmd::WinEOn => self.set_win_e(true),
            Cmd::WinEOff => self.set_win_e(false),
        }
    }

    /// F5 / Maj+F5 en double volet : copie ou déplace la sélection vers le dossier de l'autre volet.
    fn transfer_to_other(self: &Rc<Self>, is_move: bool) {
        let src = self.active_pane();
        let Some(dest) = self.other_pane().and_then(|p| p.current_path()) else {
            src.flash_status("Double volet désactivé (F6 pour l'activer).".into());
            return;
        };
        let items = src.selected_paths();
        if items.is_empty() {
            src.flash_status("Aucun élément sélectionné.".into());
            return;
        }
        let what = if is_move { "Déplacement" } else { "Copie" };
        src.run_job(Job::Drop { items, dest, is_move }, what);
    }

    /// Éléments visés par une commande : la sélection, sinon le dossier courant.
    fn targets(&self) -> Vec<PathBuf> {
        let p = self.active_pane();
        let sel = p.selected_paths();
        if sel.is_empty() {
            p.current_path().into_iter().collect()
        } else {
            sel
        }
    }

    /// Option « Remplacer Win+E » (désactivée par défaut). L'activation est confirmée ; la
    /// désactivation ne retire que la clé posée par WinDir.
    fn set_win_e(self: &Rc<Self>, on: bool) {
        if fx_win::process::is_packaged() {
            self.active_pane().flash_status(
                "Win+E ne peut pas être remplacé depuis le paquet MSIX (registre isolé) : utilisez la version portable."
                    .into(),
            );
            return;
        }
        let owner = self.hwnd.get().unwrap_or(0);
        let id = self.active_pane().id;
        let exe = std::env::current_exe();
        let posted = self.host_post_raw(move |_| {
            let key = fx_win::win_e::WinEKey::default();
            let msg = if on {
                match exe {
                    Err(e) => format!("Chemin de WinDir introuvable : {e}"),
                    Ok(exe) => {
                        let ok = fx_win::shell::confirm(
                            owner,
                            "Remplacer Win+E",
                            "La touche Windows+E ouvrira WinDir au lieu de l'Explorateur, pour \
                             votre compte uniquement.\n\nLa commande « Rendre Win+E à \
                             l'Explorateur » (ou « windir --win-e off ») rétablit le \
                             comportement d'origine.\n\nContinuer ?",
                        );
                        if !ok {
                            return;
                        }
                        match key.enable(&exe) {
                            Ok(()) => "Win+E ouvre désormais WinDir.".to_string(),
                            Err(e) => e.to_string(),
                        }
                    }
                }
            } else {
                match key.disable() {
                    Ok(true) => "Win+E ouvre de nouveau l'Explorateur.".to_string(),
                    Ok(false) => "Win+E n'était pas associé à WinDir : rien à changer.".to_string(),
                    Err(e) => format!("Registre : {}", e.message()),
                }
            };
            post_pane(id, move |p| p.flash_status(msg));
        });
        if !posted {
            self.active_pane()
                .flash_status("Le service Shell est indisponible.".into());
        }
    }

    fn properties(self: &Rc<Self>) {
        let paths = self.targets();
        let owner = self.hwnd.get().unwrap_or(0);
        let id = self.active_pane().id;
        if !self.host_post_raw(move |_| {
            if let Err(e) = fx_win::shell::show_properties(&paths, owner) {
                let msg = format!("Propriétés : {}", e.message());
                post_pane(id, move |p| p.flash_status(msg));
            }
        }) {
            self.active_pane()
                .flash_status("Le service Shell est indisponible.".into());
        }
    }

    fn copy_path(self: &Rc<Self>) {
        let paths = self.targets();
        let text = paths
            .iter()
            .map(|p| format!("\"{}\"", fx_win::path::display_form(p).display()))
            .collect::<Vec<_>>()
            .join("\r\n");
        let owner = self.hwnd.get().unwrap_or(0);
        let id = self.active_pane().id;
        let n = paths.len();
        self.host_post_raw(move |_| {
            let r = fx_win::clipboard::set_text(owner, &text);
            post_pane(id, move |p| match r {
                Ok(()) => p.flash_status(if n == 1 {
                    "Chemin copié".into()
                } else {
                    format!("{n} chemins copiés")
                }),
                Err(e) => p.flash_status(format!("Presse-papiers : {}", e.message())),
            });
        });
    }

    fn open_terminal(self: &Rc<Self>) {
        let pane = self.active_pane();
        let Some(dir) = pane.current_path() else { return };
        let dir = fx_win::path::display_form(&dir);
        let id = pane.id;
        let _ = std::thread::Builder::new()
            .name("fx-terminal".into())
            .spawn(move || {
                // Windows Terminal, sinon l'invite de commandes.
                let wt = std::process::Command::new("wt.exe").arg("-d").arg(&dir).spawn();
                if wt.is_err() {
                    let cmd = std::process::Command::new("cmd.exe")
                        .arg("/K")
                        .current_dir(&dir)
                        .spawn();
                    if let Err(e) = cmd {
                        let msg = format!("Terminal : {e}");
                        post_pane(id, move |p| p.flash_status(msg));
                    }
                }
            });
    }

    /// Calcule en arrière-plan la taille des dossiers sélectionnés (ou de tous ceux du dossier).
    fn folder_sizes(self: &Rc<Self>) {
        let pane = self.active_pane();
        let Some(folder) = pane.model.folder() else { return };
        let mut dirs: Vec<u32> = pane
            .model
            .selected()
            .into_iter()
            .filter(|&e| folder.entry(e).is_some_and(|x| x.is_dir()))
            .collect();
        if dirs.is_empty() {
            dirs = pane
                .model
                .visible()
                .into_iter()
                .filter(|&e| folder.entry(e).is_some_and(|x| x.is_dir()))
                .take(500)
                .collect();
        }
        if dirs.is_empty() {
            pane.flash_status("Aucun dossier à mesurer.".into());
            return;
        }
        let source: Arc<dyn DirSource> = self.source.clone();
        let jobs: Vec<(Vec<u16>, Arc<fx_core::size::SizeProgress>)> = dirs
            .iter()
            .map(|&e| {
                (
                    folder.name(e).to_vec(),
                    fx_core::size::start(source.clone(), folder.child_path(e)),
                )
            })
            .collect();
        pane.st
            .borrow_mut()
            .size_jobs
            .extend(jobs.iter().map(|(_, j)| j.clone()));
        let generation = pane.generation.get();
        poll_sizes(pane.id, generation, jobs);
    }

    fn add_favorite(self: &Rc<Self>) {
        let pane = self.active_pane();
        let folder = pane.model.folder();
        let sel = pane.model.selected();
        let target = match (&folder, sel.as_slice()) {
            (Some(f), [e]) if f.entry(*e).is_some_and(|x| x.is_dir()) => Some(f.child_path(*e)),
            _ => pane.current_path(),
        };
        let Some(target) = target else { return };
        if self.favorites.borrow().iter().any(|f| same_path(f, &target)) {
            pane.flash_status("Déjà dans les favoris.".into());
            return;
        }
        self.favorites.borrow_mut().push(target);
        self.save_favorites();
        pane.sync_global();
        pane.flash_status("Ajouté aux favoris".into());
    }

    fn remove_favorite(self: &Rc<Self>) {
        let pane = self.active_pane();
        let Some(cur) = pane.current_path() else { return };
        let before = self.favorites.borrow().len();
        self.favorites.borrow_mut().retain(|f| !same_path(f, &cur));
        if self.favorites.borrow().len() == before {
            pane.flash_status("Ce dossier n'est pas dans les favoris.".into());
            return;
        }
        self.save_favorites();
        pane.sync_global();
        pane.flash_status("Retiré des favoris".into());
    }

    fn save_favorites(&self) {
        let Some(dir) = &self.data_dir else { return };
        let text: String = self
            .favorites
            .borrow()
            .iter()
            .map(|p| format!("{}\n", p.display()))
            .collect();
        let _ = std::fs::create_dir_all(dir);
        if let Err(e) = std::fs::write(dir.join("favoris.txt"), text) {
            tracing::warn!("favoris non enregistrés : {e}");
        }
    }

    // ------------------------------------------------------------------ aperçu

    fn toggle_preview(self: &Rc<Self>) {
        let v = !self.preview_visible.get();
        self.preview_visible.set(v);
        self.ui.set_preview_visible(v);
        self.update_preview();
    }

    /// Met à jour le volet d'aperçu pour la sélection du volet actif, après un court délai
    /// (le défilement au clavier ne déclenche pas un aperçu par ligne).
    pub(crate) fn update_preview(self: &Rc<Self>) {
        let seq = self.preview_seq.get() + 1;
        self.preview_seq.set(seq);
        if !self.preview_visible.get() {
            self.close_native_preview();
            return;
        }
        slint::Timer::single_shot(Duration::from_millis(120), move || {
            with_app(|a| {
                if a.preview_seq.get() == seq {
                    a.show_preview(seq);
                }
            })
        });
    }

    fn preview_message(&self, title: String, text: &str) {
        self.ui.set_preview_kind(0);
        self.ui.set_preview_title(title.into());
        self.ui.set_preview_text(text.into());
    }

    fn preview_host(&self) -> bool {
        if self.preview_host.borrow().is_none() {
            match ShellHost::start("fx-preview") {
                Ok(h) => *self.preview_host.borrow_mut() = Some(h),
                Err(e) => {
                    tracing::warn!("hôte d'aperçu indisponible : {e}");
                    return false;
                }
            }
        }
        true
    }

    fn preview_post(&self, job: impl FnOnce(&fx_win::shell_host::HostCtx) + Send + 'static) -> bool {
        self.preview_host() && self.preview_host.borrow().as_ref().is_some_and(|h| h.post(job))
    }

    fn close_native_preview(&self) {
        if self.preview_native.replace(false) {
            self.preview_post(|_| crate::preview::close_native());
        }
    }

    /// Rectangle de la zone d'aperçu, en pixels physiques de la zone client.
    fn preview_rect(&self) -> fx_win::preview::Rect {
        let s = self.ui.window().scale_factor();
        fx_win::preview::Rect {
            x: (self.ui.get_preview_x() * s).round() as i32,
            y: (self.ui.get_preview_y() * s).round() as i32,
            width: (self.ui.get_preview_width() * s).round() as i32,
            height: (self.ui.get_preview_height() * s).round() as i32,
        }
    }

    fn show_preview(self: &Rc<Self>, seq: u64) {
        self.close_native_preview();
        let pane = self.active_pane();
        let Some(folder) = pane.model.folder() else {
            self.preview_message(String::new(), "");
            return;
        };
        let sel = pane.model.selected();
        let [entry] = sel[..] else {
            let text = if sel.is_empty() {
                "Sélectionnez un fichier pour en afficher l'aperçu.".to_string()
            } else {
                format!("{} éléments sélectionnés", sel.len())
            };
            self.preview_message(String::new(), &text);
            return;
        };
        let Some(e) = folder.entry(entry).copied() else {
            return;
        };
        let name = folder.display_name(entry);
        let path = folder.child_path(entry);
        if e.is_dir() {
            self.preview_message(name, "Dossier");
            return;
        }
        // Jamais de lecture d'un fichier OneDrive « en ligne seulement » : elle le téléchargerait.
        if e.is_cloud_online_only() {
            self.preview_message(
                name,
                "Fichier disponible en ligne seulement : aperçu non téléchargé.",
            );
            return;
        }
        let ext = String::from_utf16_lossy(&fx_core::folder::extension_key(folder.name(entry)));
        self.preview_message(name.clone(), "Chargement de l'aperçu…");
        if crate::preview::TEXT_EXTS.contains(&ext.as_str()) {
            let _ = std::thread::Builder::new()
                .name("fx-preview-text".into())
                .spawn(move || {
                    let text = crate::preview::read_text(&path);
                    post(move |a| {
                        if a.preview_seq.get() != seq {
                            return;
                        }
                        match text {
                            Some(t) => {
                                a.ui.set_preview_kind(1);
                                a.ui.set_preview_text(t.into());
                            }
                            None => a.preview_message(name, "Fichier binaire : pas d'aperçu texte."),
                        }
                    });
                });
            return;
        }
        let image = crate::preview::IMAGE_EXTS.contains(&ext.as_str());
        let parent = self.hwnd.get().unwrap_or(0);
        let rect = self.preview_rect();
        let px = (rect.width.max(rect.height)).clamp(64, 1024) as u32;
        let ok = self.preview_post(move |_| {
            // Prévisualiseur natif pour les documents ; vignette du Shell pour les images et à
            // défaut.
            let native = if image {
                Ok(false)
            } else {
                crate::preview::open_native(parent, rect, &path, &ext)
            };
            let outcome = match native {
                Ok(true) => Ok(None),
                Ok(false) | Err(_) => match fx_win::icons::thumbnail(&path, px, false) {
                    Some(img) => Ok(Some(img)),
                    None => Err(native
                        .err()
                        .unwrap_or_else(|| "Aucun aperçu disponible pour ce type de fichier.".into())),
                },
            };
            post(move |a| {
                if a.preview_seq.get() != seq {
                    // Sélection changée entre-temps : on ferme ce qui vient d'être ouvert.
                    a.preview_native.set(true);
                    a.close_native_preview();
                    return;
                }
                match outcome {
                    Ok(None) => {
                        a.preview_native.set(true);
                        a.preview_last_rect.set(rect);
                        a.ui.set_preview_kind(3);
                    }
                    Ok(Some(img)) => {
                        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                            &img.rgba, img.width, img.height,
                        );
                        a.ui.set_preview_image(slint::Image::from_rgba8(buf));
                        a.ui.set_preview_kind(2);
                    }
                    Err(msg) => a.preview_message(name, &msg),
                }
            });
        });
        if !ok {
            self.preview_message(String::new(), "Aperçu indisponible.");
        }
    }

    /// Fenêtre d'aperçu natif : suit les changements de mise en page (appelé à chaque image).
    fn follow_native_preview(&self) {
        if !self.preview_native.get() {
            return;
        }
        let rect = self.preview_rect();
        if rect != self.preview_last_rect.get() {
            self.preview_last_rect.set(rect);
            self.preview_post(move |_| crate::preview::resize_native(rect));
        }
    }

    // ------------------------------------------------------------------ instrumentation

    fn on_redraw(self: &Rc<Self>) {
        self.frames.set(self.frames.get() + 1);
        if let Some(run) = self.scroll.borrow_mut().as_mut() {
            run.frames.push(Instant::now());
        }
        self.follow_native_preview();
        self.track_visible_lines();
        if self.frames.get() == 1 {
            let at = fx_win::process::ms_since_process_start();
            slint::Timer::single_shot(Duration::ZERO, move || {
                with_app(|a| a.bench_first_frame(at, fx_win::process::ms_since_process_start()))
            });
        }
        let pane = self.active_pane();
        let wait = pane.paint_wait.replace(0);
        if wait != 0 {
            // Le timer à délai nul s'exécute après le rendu de cette image.
            let id = pane.id;
            slint::Timer::single_shot(Duration::ZERO, move || with_pane(id, |p| p.painted(wait)));
        }
    }

    /// Relève les lignes de grille visibles de chaque volet affiché en vue icônes.
    fn track_visible_lines(&self) {
        for slot in 0..2 {
            let Some(p) = self.pane_in_slot(slot) else {
                continue;
            };
            if !p.model.is_grid() {
                continue;
            }
            let (y, h) = if slot == 1 {
                (self.ui.get_scroll_y_1(), self.ui.get_view_height_1())
            } else {
                (self.ui.get_scroll_y_0(), self.ui.get_view_height_0())
            };
            let line_h = crate::pane::TILE_HEIGHT;
            let first = (y.max(0.0) / line_h).floor() as usize;
            let end = ((y.max(0.0) + h.max(0.0)) / line_h).ceil() as usize + 1;
            p.model.set_visible_lines(first, end);
        }
    }

    fn bench_first_frame(&self, at: Option<f64>, done: Option<f64>) {
        tracing::info!(
            ms_since_process_start = at,
            ms_since_main = self.main_start.elapsed().as_secs_f64() * 1000.0,
            "première image"
        );
        if let Some(b) = self.bench.borrow_mut().as_mut() {
            b.first_frame_ms = at;
            b.first_frame_done_ms = done;
        }
    }

    pub(crate) fn bench_navigation_painted(self: &Rc<Self>) {
        let mut next: Option<PathBuf> = None;
        let mut finish = false;
        {
            let mut guard = self.bench.borrow_mut();
            let Some(b) = guard.as_mut() else { return };
            if b.initial_painted_ms.is_none() {
                b.initial_painted_ms = fx_win::process::ms_since_process_start();
                match &b.mode {
                    BenchMode::Startup { .. } => finish = true,
                    BenchMode::Scroll { .. } => {
                        slint::Timer::single_shot(Duration::from_millis(700), || {
                            with_app(|a| a.start_scroll_bench())
                        });
                    }
                    BenchMode::Search { .. } => {
                        slint::Timer::single_shot(Duration::from_millis(300), || {
                            with_app(|a| a.search_bench_step(0))
                        });
                    }
                    BenchMode::SelfTest { .. } => {
                        let start = b.start_path.clone();
                        slint::Timer::single_shot(Duration::from_millis(500), move || {
                            with_app(|a| a.self_test_step(0, start));
                        });
                    }
                    BenchMode::Open { path, .. } => {
                        b.on_target = true;
                        next = Some(path.clone());
                    }
                }
            } else if let BenchMode::Open { path, iterations, .. } = b.mode.clone() {
                if b.on_target {
                    let t = self.active_pane().timing.get();
                    b.entries = t.stats.entries;
                    b.results.push(t);
                    if b.iteration + 1 == iterations {
                        if let Some(m) = fx_win::process::memory() {
                            b.report += &format!(
                                "mémoire avec ce dossier affiché : privée {:.1} Mo, working set {:.1} Mo\n",
                                m.private as f64 / 1_048_576.0,
                                m.working_set as f64 / 1_048_576.0
                            );
                        }
                        finish = true;
                    } else {
                        b.on_target = false;
                        next = Some(b.start_path.clone());
                    }
                } else {
                    b.iteration += 1;
                    b.on_target = true;
                    next = Some(path);
                }
            }
        }
        if let Some(path) = next {
            slint::Timer::single_shot(Duration::from_millis(300), move || {
                with_app(|a| a.active_pane().navigate(path, true, Vec::new()));
            });
        }
        if finish {
            slint::Timer::single_shot(Duration::from_millis(1000), || with_app(|a| a.bench_report()));
        }
    }

    fn check(&self, ok: bool, what: &str) {
        let line = format!("{} {what}\n", if ok { "OK   " } else { "ÉCHEC" });
        tracing::info!("auto-test : {}", line.trim_end());
        if let Some(b) = self.bench.borrow_mut().as_mut() {
            b.report += &line;
        }
    }

    fn selected_display_names(&self) -> Vec<String> {
        let p = self.active_pane();
        let Some(folder) = p.model.folder() else {
            return Vec::new();
        };
        p.model
            .selected()
            .into_iter()
            .map(|e| folder.display_name(e))
            .collect()
    }

    /// Scénario d'auto-test : chaque étape agit comme l'utilisateur (mêmes fonctions que les
    /// callbacks de l'interface), vérifie le résultat, puis planifie la suivante.
    /// Auto-test : si `ok` est faux, refait l'étape `step` dans 200 ms, au plus 5 s en tout
    /// (les délais fixes échouent sur une machine chargée). Renvoie `true` si l'étape est
    /// reportée.
    fn self_test_retry(self: &Rc<Self>, step: usize, dir: &Path, ok: bool) -> bool {
        let started = *self.self_test_since.borrow_mut().get_or_insert(Instant::now());
        if !ok && started.elapsed() < Duration::from_secs(5) {
            let d = dir.to_path_buf();
            slint::Timer::single_shot(Duration::from_millis(200), move || {
                with_app(|a| a.self_test_step(step, d))
            });
            return true;
        }
        if started.elapsed() > Duration::from_millis(250) {
            tracing::info!(
                step,
                attente_ms = started.elapsed().as_millis() as u64,
                "auto-test : condition attendue"
            );
        }
        *self.self_test_since.borrow_mut() = None;
        false
    }

    fn self_test_step(self: &Rc<Self>, step: usize, dir: PathBuf) {
        let next = |delay_ms: u64, d: PathBuf| {
            slint::Timer::single_shot(Duration::from_millis(delay_ms), move || {
                with_app(|a| a.self_test_step(step + 1, d));
            });
        };
        let p = self.active_pane();
        match step {
            0 => {
                // Fichiers créés par un autre programme : la surveillance doit les faire apparaître.
                for (n, size) in [("bravo.txt", 10usize), ("alpha.txt", 3000), ("charlie.log", 500)] {
                    let _ = std::fs::write(dir.join(n), vec![b'x'; size]);
                }
                next(900, dir);
            }
            1 => {
                self.check(
                    p.model.len() == 3,
                    &format!("surveillance : 3 fichiers externes affichés ({})", p.model.len()),
                );
                p.new_folder();
                next(1200, dir);
            }
            2 => {
                let ui = p.ui_snapshot();
                let renaming = p.st.borrow().rename_entry.is_some();
                if self.self_test_retry(step, &dir, renaming && ui.rename_row >= 0) {
                    return;
                }
                self.check(
                    renaming && ui.rename_row >= 0,
                    "nouveau dossier créé et passé en renommage",
                );
                self.check(
                    ui.rename_text == "Nouveau dossier",
                    &format!("nom proposé « {} »", ui.rename_text),
                );
                p.rename_commit("Dossier renommé");
                next(1200, dir);
            }
            3 => {
                self.check(
                    dir.join("Dossier renommé").is_dir(),
                    "renommage appliqué sur le disque",
                );
                let sel = self.selected_display_names();
                self.check(
                    sel == ["Dossier renommé"],
                    &format!("élément renommé sélectionné ({sel:?})"),
                );
                p.on_key("a", true, false, false);
                self.check(
                    p.model.selected().len() == 4,
                    &format!("Ctrl+A : {} éléments sélectionnés", p.model.selected().len()),
                );
                p.sort_by(3);
                next(400, dir);
            }
            4 => {
                let names: Vec<String> = p
                    .model
                    .visible()
                    .iter()
                    .filter_map(|&e| p.model.folder().map(|f| f.display_name(e)))
                    .collect();
                self.check(
                    names == ["Dossier renommé", "bravo.txt", "charlie.log", "alpha.txt"],
                    &format!("tri par taille, dossiers en premier ({names:?})"),
                );
                p.sort_by(0);
                p.on_key("c", false, false, false);
                let sel = self.selected_display_names();
                self.check(
                    sel == ["charlie.log"],
                    &format!("saisie au clavier « c » ({sel:?})"),
                );
                // Menu contextuel réel, fermé automatiquement.
                let (x, y) = {
                    let hwnd = self.hwnd.get().unwrap_or(0);
                    let scale = self.ui.window().scale_factor();
                    fx_win::shell::client_to_screen(hwnd, (400.0 * scale) as i32, (300.0 * scale) as i32)
                };
                let folder_path = dir.clone();
                let items = p.selected_paths();
                let owner = self.hwnd.get().unwrap_or(0);
                self.host_post_raw(move |ctx| {
                    let opts = MenuOptions {
                        x,
                        y,
                        owner,
                        auto_dismiss_ms: Some(2500),
                    };
                    let r =
                        fx_win::context_menu::show(ctx, &folder_path, &items, opts).map_err(|e| e.message());
                    post(move |a| {
                        let n = r.as_ref().map(|o| o.items.len()).unwrap_or(0);
                        let err = r.as_ref().err().map(|e| format!(" — {e}")).unwrap_or_default();
                        a.check(
                            r.is_ok() && n > 5,
                            &format!("menu contextuel affiché ({n} entrées) puis fermé{err}"),
                        );
                    });
                });
                next(3500, dir);
            }
            5 => {
                // Suppression externe : la sélection passe à l'élément suivant.
                let _ = std::fs::remove_file(dir.join("charlie.log"));
                next(900, dir);
            }
            6 => {
                self.check(
                    p.model.len() == 3,
                    &format!("suppression externe appliquée ({} éléments)", p.model.len()),
                );
                let sel = self.selected_display_names();
                self.check(
                    sel.len() == 1,
                    &format!("sélection reportée après suppression ({sel:?})"),
                );
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .attributes(0x2) // FILE_ATTRIBUTE_HIDDEN
                        .open(dir.join("caché.ini"));
                }
                next(900, dir);
            }
            7 => {
                self.check(
                    p.model.len() == 3,
                    &format!("fichier caché masqué ({} éléments affichés)", p.model.len()),
                );
                p.on_key("h", true, false, false);
                next(400, dir);
            }
            8 => {
                self.check(
                    p.model.len() == 4,
                    &format!("Ctrl+H affiche le fichier caché ({} éléments)", p.model.len()),
                );
                // Partage réseau injoignable : abandon après le délai, sans bloquer l'interface.
                p.navigate(PathBuf::from(r"\\10.255.255.6\partage"), true, Vec::new());
                let wait = self.network_timeout + Duration::from_millis(800);
                slint::Timer::single_shot(wait, move || with_app(|a| a.self_test_step(9, dir)));
            }
            9 => {
                let ui = p.ui_snapshot();
                let err = ui.error.to_string();
                self.check(
                    err.contains("ne répond pas") || err.contains("injoignable"),
                    &format!("partage injoignable signalé ({err:?})"),
                );
                self.check(!ui.loading, "chargement terminé (plus de « Chargement… »)");
                p.go_back();
                next(900, dir);
            }
            10 => {
                self.check(
                    p.model.len() == 4 && p.current_path().as_deref().is_some_and(|c| same_path(c, &dir)),
                    &format!(
                        "retour arrière vers le dossier de test ({} éléments)",
                        p.model.len()
                    ),
                );
                // Onglets : Ctrl+T ouvre un onglet sur le même dossier.
                self.on_key(-1, "t", true, false, false);
                next(700, dir);
            }
            11 => {
                let n = self.tabs.borrow().len();
                self.check(
                    n == 2
                        && self.active_tab.get() == 1
                        && p.current_path().is_some_and(|c| same_path(&c, &dir)),
                    &format!("Ctrl+T : nouvel onglet actif ({n} onglets)"),
                );
                p.navigate(dir.join("Dossier renommé"), true, Vec::new());
                next(700, dir);
            }
            12 => {
                let tabs = self.tabs.borrow().len();
                // Double volet : F6, puis F5 copie vers l'autre volet.
                self.on_key(-1, &char::from(Key::F6).to_string(), false, false, false);
                self.check(
                    self.active_tab().dual.get(),
                    &format!("F6 : double volet activé ({tabs} onglets)"),
                );
                let _ = std::fs::write(dir.join("Dossier renommé").join("à copier.txt"), b"x");
                next(900, dir);
            }
            13 => {
                // Volet actif : gauche (sous-dossier) ; on sélectionne « à copier.txt » et F5.
                p.on_key("à", false, false, false);
                let sel = self.selected_display_names();
                self.check(
                    sel == ["à copier.txt"],
                    &format!("sélection dans le volet gauche ({sel:?})"),
                );
                self.on_key(-1, &char::from(Key::F5).to_string(), false, false, false);
                next(1500, dir);
            }
            14 => {
                // L'autre volet s'est ouvert sur le même dossier (sous-dossier) : la copie y crée
                // « à copier - Copie.txt ».
                let copied = std::fs::read_dir(dir.join("Dossier renommé"))
                    .map(|d| d.flatten().count())
                    .unwrap_or(0);
                self.check(
                    copied == 2,
                    &format!("F5 : copie vers l'autre volet ({copied} fichiers)"),
                );
                self.on_key(-1, "w", true, false, false);
                next(500, dir);
            }
            15 => {
                let n = self.tabs.borrow().len();
                self.check(n == 1, &format!("Ctrl+W : onglet fermé ({n} onglet)"));
                self.on_key(-1, "t", true, true, false);
                next(700, dir);
            }
            16 => {
                let n = self.tabs.borrow().len();
                let dual = self.active_tab().dual.get();
                self.check(
                    n == 2 && dual,
                    &format!("Ctrl+Maj+T : onglet rouvert, double volet conservé ({n} onglets)"),
                );
                // Volet d'aperçu : texte, image, document (prévisualiseur du Shell).
                let _ = std::fs::write(dir.join("image.bmp"), test_bmp(96, 64));
                let _ = std::fs::write(dir.join("rapport.pdf"), test_pdf());
                self.toggle_preview();
                p.navigate(dir.clone(), true, Vec::new());
                next(1000, dir);
            }
            17 => {
                p.on_key("a", false, false, false);
                next(600, dir);
            }
            18 => {
                let kind = self.ui.get_preview_kind();
                let text = self.ui.get_preview_text();
                self.check(
                    kind == 1 && text.starts_with("xxx"),
                    &format!("aperçu texte (type {kind}, {} caractères)", text.chars().count()),
                );
                // Nouvelle saisie (pas la suite de la précédente).
                *p.type_ahead.borrow_mut() = fx_core::selection::TypeAhead::default();
                p.on_key("i", false, false, false);
                next(1500, dir);
            }
            19 => {
                let kind = self.ui.get_preview_kind();
                let size = self.ui.get_preview_image().size();
                self.check(
                    kind == 2 && size.width > 0,
                    &format!(
                        "aperçu image : vignette du Shell (type {kind}, {}×{})",
                        size.width, size.height
                    ),
                );
                // Nouvelle saisie (pas la suite de la précédente).
                *p.type_ahead.borrow_mut() = fx_core::selection::TypeAhead::default();
                p.on_key("r", false, false, false);
                next(3000, dir);
            }
            20 => {
                let kind = self.ui.get_preview_kind();
                match fx_win::preview::handler_for("pdf") {
                    Some(clsid) => self.check(
                        kind == 3 && self.preview_native.get(),
                        &format!("aperçu PDF par le prévisualiseur du Shell {clsid:?} (type {kind})"),
                    ),
                    None => self.check(
                        kind != 1,
                        &format!("pas de prévisualiseur PDF enregistré : repli (type {kind})"),
                    ),
                }
                // Signal pour la capture d'écran du script de test.
                let _ = std::fs::write(dir.join("capture-apercu.flag"), b"");
                next(1500, dir);
            }
            21 => {
                // Masquer le volet ferme le prévisualiseur natif.
                self.toggle_preview();
                self.check(!self.preview_native.get(), "volet masqué : prévisualiseur fermé");
                // Vue icônes (Ctrl+Maj+V) : vignettes du Shell.
                self.on_key(-1, "v", true, true, false);
                next(2000, dir);
            }
            22 => {
                let cols = p.model.columns();
                let grid = p.model.is_grid() && p.ui_snapshot().view_mode == 1;
                self.check(
                    grid && cols > 1,
                    &format!("Ctrl+Maj+V : vue icônes ({cols} colonnes)"),
                );
                let row_of = |name: &str| {
                    let f = p.model.folder()?;
                    p.model.visible().iter().position(|&e| f.display_name(e) == name)
                };
                let thumb = row_of("image.bmp")
                    .and_then(|r| slint::Model::row_data(&*p.model, r))
                    .map(|row| (row.has_icon, row.icon.size()));
                self.check(
                    thumb.is_some_and(|(has, size)| has && size.width > 32),
                    &format!("vignette de l'image en vue icônes ({thumb:?})"),
                );
                // Flèches : droite = élément suivant, bas = ligne de tuiles suivante.
                p.model.update_selection(|s, v| s.move_to(v, 0, false, false));
                p.on_key(&char::from(Key::RightArrow).to_string(), false, false, false);
                let after_right = p.model.selection().focus_row(&p.model.visible());
                p.on_key(&char::from(Key::DownArrow).to_string(), false, false, false);
                let after_down = p.model.selection().focus_row(&p.model.visible());
                let expected_down = if 1 + cols < p.model.len() { 1 + cols } else { 1 };
                self.check(
                    after_right == Some(1) && after_down == Some(expected_down),
                    &format!("flèches en vue icônes (droite → {after_right:?}, bas → {after_down:?})"),
                );
                p.update_status();
                let _ = std::fs::write(dir.join("capture-grille.flag"), b"");
                next(1200, dir);
            }
            23 => {
                self.on_key(-1, "v", true, true, false);
                self.check(!p.model.is_grid(), "retour à la vue détails");
                // Recherche globale : ouverture (moteur démarré, index chargé ou construit).
                self.ui.invoke_open_search();
                *self.search_wait.borrow_mut() = Some(Instant::now());
                next(500, dir);
            }
            24 => {
                // Attente de l'index (construction complète au premier lancement).
                let status = self.search_index_status.borrow().clone();
                let ready = status.contains("éléments") && !status.contains('…');
                let waited = self
                    .search_wait
                    .borrow()
                    .map_or(0.0, |t| t.elapsed().as_secs_f64());
                if !ready && waited < 120.0 {
                    slint::Timer::single_shot(Duration::from_millis(500), move || {
                        with_app(|a| a.self_test_step(24, dir))
                    });
                    return;
                }
                self.check(ready, &format!("index de recherche prêt en {waited:.1} s"));
                // Créé après le démarrage de la surveillance : indexé en direct.
                let name = format!("fx-recherche-{}-zq.txt", std::process::id());
                let _ = std::fs::write(dir.join(&name), b"x");
                next(1500, dir);
            }
            25 => {
                let name = format!("fx-recherche-{}-zq", std::process::id());
                self.search_latencies.borrow_mut().clear();
                // Frappe lettre par lettre : chaque frappe lance une recherche.
                let mut typed = String::new();
                for c in name.chars() {
                    typed.push(c);
                    self.ui.set_search_query(typed.as_str().into());
                    self.search_edited(&typed);
                }
                next(1500, dir);
            }
            26 => {
                let rows = self.search_rows.borrow().clone();
                let expected = dir.join(format!("fx-recherche-{}-zq.txt", std::process::id()));
                let ok = rows.len() == 1 && same_path(Path::new(&rows[0].path), &expected);
                if self.self_test_retry(step, &dir, ok) {
                    // La surveillance n'a peut-être pas encore livré la création : on relance
                    // la requête.
                    self.search_edited(&format!("fx-recherche-{}-zq", std::process::id()));
                    return;
                }
                self.check(
                    ok,
                    &format!(
                        "fichier créé trouvé par la recherche ({} résultat(s){})",
                        rows.len(),
                        rows.first()
                            .filter(|_| !ok)
                            .map(|r| format!(" : {} au lieu de {}", r.path, expected.display()))
                            .unwrap_or_default()
                    ),
                );
                self.search_run(0, false);
                self.ui.set_search_open(false);
                next(1500, dir);
            }
            27 => {
                let sel = self.selected_display_names();
                let here = p.current_path().is_some_and(|c| same_path(&c, &dir));
                self.check(
                    here && sel.len() == 1 && sel[0].starts_with("fx-recherche-"),
                    &format!(
                        "Entrée : dossier du résultat affiché, élément sélectionné ({} sélectionné(s))",
                        sel.len()
                    ),
                );
                let _ = std::fs::remove_file(dir.join(format!("fx-recherche-{}-zq.txt", std::process::id())));
                next(1500, dir);
            }
            28 => {
                let q = format!("fx-recherche-{}-zq", std::process::id());
                self.search_edited(&q);
                next(1000, dir);
            }
            29 => {
                let n = self.search_rows.borrow().len();
                self.check(
                    n == 0,
                    &format!("fichier supprimé retiré de l'index ({n} résultat(s))"),
                );
                let lat = self.search_latencies.borrow().clone();
                if !lat.is_empty() {
                    let mut s: Vec<f64> = lat.iter().map(|l| l.0).collect();
                    let mut e: Vec<f64> = lat.iter().map(|l| l.1).collect();
                    s.sort_by(f64::total_cmp);
                    e.sort_by(f64::total_cmp);
                    let line = format!(
                        "recherche : {} requêtes, durée médiane {:.1} ms (max {:.1}), frappe → résultats médiane {:.1} ms (max {:.1})\n",
                        lat.len(),
                        s[s.len() / 2],
                        s[s.len() - 1],
                        e[e.len() / 2],
                        e[e.len() - 1]
                    );
                    if let Some(b) = self.bench.borrow_mut().as_mut() {
                        b.report += &line;
                    }
                }
                slint::Timer::single_shot(Duration::from_millis(300), || with_app(|a| a.bench_report()));
            }
            _ => {}
        }
    }

    fn bench_line(&self, line: String) {
        tracing::info!("mesure : {line}");
        if let Some(b) = self.bench.borrow_mut().as_mut() {
            b.report += &line;
            b.report += "\n";
        }
    }

    fn memory_line(&self, what: &str) {
        if let Some(m) = fx_win::process::memory() {
            self.bench_line(format!(
                "mémoire {what} : privée {:.1} Mo, working set {:.1} Mo",
                m.private as f64 / 1_048_576.0,
                m.working_set as f64 / 1_048_576.0
            ));
        }
    }

    /// Mesure de la recherche. Frappe : une touche toutes les 120 ms (frappe rapide) ; on
    /// mesure le délai entre chaque frappe et l'affichage de ses résultats.
    fn search_bench_step(self: &Rc<Self>, step: usize) {
        const WORDS: [&str; 4] = ["windows", "system32", "rapport 2024", "zzqx"];
        const PACE_MS: u64 = 120;
        let again = move |delay: u64, s: usize| {
            slint::Timer::single_shot(Duration::from_millis(delay), move || {
                with_app(|a| a.search_bench_step(s))
            });
        };
        match step {
            0 => {
                self.memory_line("au repos, avant la recherche");
                *self.search_wait.borrow_mut() = Some(Instant::now());
                self.ui.invoke_open_search();
                again(200, 1);
            }
            1 => {
                let status = self.search_index_status.borrow().clone();
                let waited = self
                    .search_wait
                    .borrow()
                    .map_or(0.0, |t| t.elapsed().as_secs_f64());
                if !(status.contains("éléments") && !status.contains('…')) && waited < 300.0 {
                    again(200, 1);
                    return;
                }
                self.bench_line(format!("index prêt en {waited:.2} s ({status})"));
                self.memory_line("index chargé");
                self.search_latencies.borrow_mut().clear();
                again(500, 100);
            }
            // 100 + n : n-ième frappe de la séquence complète.
            s if (100..1000).contains(&s) => {
                let n = s - 100;
                let seq: Vec<String> = WORDS
                    .iter()
                    .flat_map(|w| (1..=w.chars().count()).map(move |k| w.chars().take(k).collect::<String>()))
                    .collect();
                if let Some(text) = seq.get(n) {
                    self.ui.set_search_query(text.as_str().into());
                    self.search_edited(text);
                    again(PACE_MS, s + 1);
                    return;
                }
                let lat = self.search_latencies.borrow().clone();
                let mut d: Vec<f64> = lat.iter().map(|l| l.0).collect();
                let mut e: Vec<f64> = lat.iter().map(|l| l.1).collect();
                d.sort_by(f64::total_cmp);
                e.sort_by(f64::total_cmp);
                if let (Some(dm), Some(em)) = (d.last(), e.last()) {
                    self.bench_line(format!(
                        "frappes : {} sur {} affichées · recherche p50 {:.1} ms, max {:.1} ms · frappe → résultats p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
                        lat.len(),
                        seq.len(),
                        d[d.len() / 2],
                        dm,
                        e[e.len() / 2],
                        e[(e.len() * 95 / 100).min(e.len() - 1)],
                        em
                    ));
                }
                // Recherche fermée d'abord : sinon l'état « index libéré » relance la requête
                // affichée, qui relit l'index.
                self.ui.set_search_open(false);
                if let Some(eng) = self.search.borrow().as_ref() {
                    eng.release();
                }
                again(3000, 2);
            }
            2 => {
                self.memory_line("index libéré");
                self.search_latencies.borrow_mut().clear();
                // Réouverture comme l'utilisateur (relecture anticipée), première frappe 150 ms
                // plus tard.
                self.ui.set_search_query("".into());
                self.ui.invoke_open_search();
                again(150, 4);
            }
            4 => {
                self.ui.set_search_query("w".into());
                self.search_edited("w");
                again(3000, 3);
            }
            _ => {
                if let Some((d, e)) = self.search_latencies.borrow().first() {
                    self.bench_line(format!(
                        "première frappe 150 ms après la réouverture (index libéré, relu à l'ouverture) : {e:.0} ms (recherche {d:.1} ms)"
                    ));
                }
                self.memory_line("index relu");
                self.ui.set_search_open(false);
                slint::Timer::single_shot(Duration::from_millis(300), || with_app(|a| a.bench_report()));
            }
        }
    }

    fn start_scroll_bench(self: &Rc<Self>) {
        let driver = slint::Timer::default();
        driver.start(slint::TimerMode::Repeated, Duration::from_millis(2), || {
            with_app(|a| {
                let p = a.active_pane();
                // Hauteur lue une seule fois : la lire à chaque tick forcerait Slint à mettre
                // la liste à jour entre deux images (plusieurs sauts instanciés par image).
                let cached = a.scroll.borrow().as_ref().and_then(|r| r.max);
                let max = match cached {
                    Some(m) => m,
                    None => {
                        let m = p.max_scroll() as f64;
                        if let Some(r) = a.scroll.borrow_mut().as_mut() {
                            r.max = Some(m);
                        }
                        m
                    }
                };
                let pos = a.scroll.borrow_mut().as_mut().and_then(|r| r.position(max));
                match pos {
                    Some(y) => p.set_scroll(y as f32),
                    None => {
                        let report = a
                            .scroll
                            .borrow_mut()
                            .take()
                            .map(|r| r.report())
                            .unwrap_or_default();
                        let size = a.ui.window().size();
                        if let Some(b) = a.bench.borrow_mut().as_mut() {
                            b.report += &format!(
                                "défilement de {} éléments, fenêtre {}x{} px, échelle {} :\n{report}",
                                p.model.len(),
                                size.width,
                                size.height,
                                a.ui.window().scale_factor()
                            );
                        }
                        a.bench_report();
                    }
                }
            })
        });
        *self.scroll.borrow_mut() = Some(crate::bench::ScrollRun {
            started: Instant::now(),
            phases: Vec::new(),
            frames: Vec::new(),
            rng: 0x2545_F491_4F6C_DD1D,
            max: None,
            _driver: driver,
        });
    }

    fn bench_report(&self) {
        let Some(mut b) = self.bench.borrow_mut().take() else {
            return;
        };
        if let Some(l) = &self.latency {
            l.stop();
            b.report += &l.report();
        }
        let ms = |d: Option<Duration>| {
            d.map(|d| format!("{:.1}", d.as_secs_f64() * 1000.0))
                .unwrap_or("?".into())
        };
        let mut out = String::new();
        out += &format!(
            "démarrage : première image {:.1} ms (rendu terminé ≤ {:.1} ms), dossier initial affiché {:.1} ms\n",
            b.first_frame_ms.unwrap_or(f64::NAN),
            b.first_frame_done_ms.unwrap_or(f64::NAN),
            b.initial_painted_ms.unwrap_or(f64::NAN),
        );
        if let BenchMode::Open { path, .. } = &b.mode {
            out += &format!(
                "ouverture de {} ({} entrées), cache chaud :\n",
                path.display(),
                b.entries
            );
            for (i, t) in b.results.iter().enumerate() {
                out += &format!(
                    "  #{} premières lignes {} ms (peintes {} ms) · liste complète triée {} ms (peinte {} ms) · listing {:.1} ms, tri {:.1} ms\n",
                    i + 1,
                    ms(t.first_rows),
                    ms(t.first_paint),
                    ms(t.complete),
                    ms(t.complete_paint),
                    t.stats.list.as_secs_f64() * 1000.0,
                    t.stats.sort.as_secs_f64() * 1000.0,
                );
            }
        } else if let Some(m) = fx_win::process::memory() {
            let when = if matches!(b.mode, BenchMode::Startup { .. }) {
                "au repos"
            } else {
                "en fin de mesure"
            };
            out += &format!(
                "mémoire {when} : privée {:.1} Mo, working set {:.1} Mo\n",
                m.private as f64 / 1_048_576.0,
                m.working_set as f64 / 1_048_576.0
            );
        }
        out += &b.report;
        print!("{out}");
        let target = match &b.mode {
            BenchMode::Startup { out }
            | BenchMode::Open { out, .. }
            | BenchMode::SelfTest { out }
            | BenchMode::Search { out }
            | BenchMode::Scroll { out, .. } => out.clone(),
        };
        if let Some(p) = target
            && let Err(e) = std::fs::write(&p, &out)
        {
            eprintln!("écriture de {} impossible : {e}", p.display());
        }
        let _ = slint::quit_event_loop();
    }
}

/// Rafraîchit les tailles de dossiers en cours de calcul, jusqu'à la fin (ou au changement de
/// dossier du volet).
fn poll_sizes(pane_id: u64, generation: u64, jobs: Vec<(Vec<u16>, Arc<fx_core::size::SizeProgress>)>) {
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        with_pane(pane_id, |p| {
            if p.generation.get() != generation {
                return;
            }
            let mut pending = Vec::new();
            let mut total = 0u64;
            for (name, job) in jobs {
                let done = job.is_done();
                p.model.set_dir_size(&name, job.bytes(), done);
                total += job.bytes();
                if !done {
                    pending.push((name, job));
                }
            }
            if pending.is_empty() {
                p.flash_status(format!(
                    "Taille calculée : {}",
                    fx_core::format::human_size(total)
                ));
            } else {
                poll_sizes(pane_id, generation, pending);
            }
        });
    });
}

/// Image BMP 24 bits (dégradé) pour l'auto-test de l'aperçu.
fn test_bmp(w: u32, h: u32) -> Vec<u8> {
    let row = (w * 3).div_ceil(4) * 4;
    let data = row * h;
    let mut b = Vec::with_capacity((54 + data) as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(54 + data).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(h as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for y in 0..h {
        for x in 0..w {
            b.extend_from_slice(&[(255 * x / w) as u8, (255 * y / h) as u8, 200]);
        }
        b.resize(b.len() + (row - w * 3) as usize, 0);
    }
    b
}

/// Document PDF minimal (une page, un texte) pour l'auto-test de l'aperçu.
fn test_pdf() -> Vec<u8> {
    let content = "BT /F1 28 Tf 40 120 Td (WinDir - apercu PDF) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 297] /Contents 4 0 R          /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!("<< /Length {} >>
stream
{content}
endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from(
        "%PDF-1.4
",
    );
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out += &format!(
            "{} 0 obj
{o}
endobj
",
            i + 1
        );
    }
    let xref = out.len();
    out += &format!(
        "xref
0 {}
0000000000 65535 f 
",
        objects.len() + 1
    );
    for off in offsets {
        out += &format!(
            "{off:010} 00000 n 
"
        );
    }
    out += &format!(
        "trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
",
        objects.len() + 1
    );
    out.into_bytes()
}
