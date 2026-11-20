//! Volet de dossier : tout l'état d'une vue (dossier, sélection, historique, chargeur,
//! surveillance, renommage) et sa logique. Un onglet contient un ou deux volets ; un volet
//! n'est affiché que s'il est rattaché à un emplacement de la fenêtre (`slot` 0 ou 1).
//!
//! Règle : aucun emprunt de `State` n'est gardé pendant un appel à l'interface Slint ou au
//! modèle, car ces appels peuvent déclencher des callbacks qui reviennent ici.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fx_core::Folder;
use fx_core::filter::Filter;
use fx_core::format::{group_thousands, human_size, item_count};
use fx_core::history::{History, parent_of};
use fx_core::loader::{ApplyResult, LoadEvent, LoadStats, Loader, Sink, ViewResult};
use fx_core::selection::{Selection, TypeAhead};
use fx_core::sort::{NameKeys, SortColumn, SortSpec};
use fx_core::source::FsEvent;
use fx_win::context_menu::{AppCommand, MenuOptions, MenuOutcome};
use fx_win::fileop::{FileOp, OpOutcome};
use slint::platform::{Key, PointerEventButton, WindowEvent as SlintEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, SharedString, VecModel};

use crate::Crumb;
use crate::app::{with_app, with_ui};
use crate::model::{FolderModel, GridModel};
use crate::ops::{self, Job};

/// Hauteur d'une ligne de tuiles (`Theme.tile-height` dans app.slint), en pixels logiques.
pub(crate) const TILE_HEIGHT: f32 = 140.0;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NavTiming {
    pub first_rows: Option<Duration>,
    pub first_paint: Option<Duration>,
    pub complete: Option<Duration>,
    pub complete_paint: Option<Duration>,
    pub stats: LoadStats,
}

const PAINT_FIRST: u8 = 1;
const PAINT_COMPLETE: u8 = 2;
/// Délai pendant lequel on attend l'apparition d'éléments créés pour les sélectionner.
const PENDING_SELECT_TIMEOUT: Duration = Duration::from_secs(5);
const INVALID_NAME_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

pub(crate) fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().collect()
}

fn column_index(c: SortColumn) -> i32 {
    match c {
        SortColumn::Name => 0,
        SortColumn::Modified => 1,
        SortColumn::Type => 2,
        SortColumn::Size => 3,
    }
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        fx_win::path::display_form(p)
            .to_string_lossy()
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Exécute `f` sur le volet `id` depuis n'importe quel thread (ignoré si le volet n'existe plus).
pub(crate) fn post_pane(id: u64, f: impl FnOnce(&Rc<Pane>) + Send + 'static) {
    if let Err(e) = slint::invoke_from_event_loop(move || with_pane(id, f)) {
        tracing::warn!("événement perdu, boucle d'interface arrêtée : {e}");
    }
}

/// Exécute `f` sur le volet `id` (thread d'interface).
pub(crate) fn with_pane(id: u64, f: impl FnOnce(&Rc<Pane>)) {
    let pane = crate::app::with_app_ret(|a| a.find_pane(id)).flatten();
    if let Some(p) = pane {
        f(&p);
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub folder: Option<Arc<Folder>>,
    pub keys: Option<Arc<NameKeys>>,
    pub order: Arc<Vec<u32>>,
    /// Tri demandé au chargeur pour le chargement en cours.
    pub load_spec: SortSpec,
    /// Tri voulu par l'utilisateur.
    pub spec: SortSpec,
    pub filter: Filter,
    /// Texte du filtre (restitué dans la barre quand le volet redevient actif).
    pub filter_text: String,
    /// Incrémenté à chaque changement de tri ou de filtre.
    pub view_version: u64,
    pub apply_version: u64,
    pub complete: bool,
    /// Noms à sélectionner dès qu'ils apparaissent (retour au parent, création, collage).
    pub pending_select: Vec<Vec<u16>>,
    pub pending_since: Option<Instant>,
    /// Passer en renommage dès que l'élément attendu apparaît (nouveau dossier).
    pub pending_rename: bool,
    pub watcher: Option<Box<dyn Send>>,
    pub applying: bool,
    pub queued: Vec<FsEvent>,
    pub rename_entry: Option<u32>,
    /// Appui sur un élément déjà sélectionné : la sélection ne change qu'au relâchement
    /// (pour pouvoir glisser plusieurs éléments).
    pub press_deferred: Option<usize>,
    /// Appui sur le nom de l'unique élément déjà sélectionné : un relâchement sur le nom, sans
    /// glissement ni double-clic, lance le renommage (clic lent, comme l'Explorateur).
    pub rename_candidate: bool,
    pub rename_armed: bool,
    /// Incrémenté pour annuler un renommage par clic lent en attente.
    pub rename_gen: u64,
    pub dragging: bool,
    pub marquee_base: Option<Selection>,
    /// Calculs de taille de dossiers en cours (annulés au changement de dossier).
    pub size_jobs: Vec<Arc<fx_core::size::SizeProgress>>,
}

/// Dernier état affiché du volet, réappliqué quand il est rattaché à un emplacement.
#[derive(Clone)]
pub(crate) struct UiState {
    pub status: SharedString,
    pub error: SharedString,
    pub loading: bool,
    pub filtering: bool,
    pub sort_column: i32,
    pub sort_descending: bool,
    pub rename_row: i32,
    pub rename_text: SharedString,
    pub rename_stem: i32,
    pub drop_row: i32,
    /// 0 détails, 1 icônes.
    pub view_mode: i32,
    /// Page d'accueil à la place de la liste.
    pub home: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            status: SharedString::new(),
            error: SharedString::new(),
            loading: false,
            filtering: false,
            sort_column: 0,
            sort_descending: false,
            rename_row: -1,
            rename_text: SharedString::new(),
            rename_stem: 0,
            drop_row: -1,
            view_mode: 0,
            home: false,
        }
    }
}

pub(crate) struct Pane {
    pub id: u64,
    /// Emplacement d'affichage (0 ou 1), `None` si le volet est caché (autre onglet).
    pub slot: Cell<Option<usize>>,
    pub loader: Arc<Loader>,
    pub model: Rc<FolderModel>,
    /// Lignes de la vue icônes, construites sur `model`.
    pub grid_model: Rc<GridModel>,
    pub history: RefCell<History>,
    pub generation: Cell<u64>,
    pub view_seq: Cell<u64>,
    pub st: RefCell<State>,
    pub type_ahead: RefCell<TypeAhead>,
    pub status_seq: Cell<u64>,
    pub nav_start: Cell<Instant>,
    pub timing: Cell<NavTiming>,
    pub paint_wait: Cell<u8>,
    pub saved_scroll: Cell<f32>,
    pub ui_state: RefCell<UiState>,
}

impl Pane {
    pub(crate) fn new(id: u64, loader: Arc<Loader>, model: Rc<FolderModel>) -> Self {
        Self {
            id,
            slot: Cell::new(None),
            loader,
            grid_model: Rc::new(GridModel(model.clone())),
            model,
            history: RefCell::default(),
            generation: Cell::new(0),
            view_seq: Cell::new(0),
            st: RefCell::default(),
            type_ahead: RefCell::default(),
            status_seq: Cell::new(0),
            nav_start: Cell::new(Instant::now()),
            timing: Cell::default(),
            paint_wait: Cell::new(0),
            saved_scroll: Cell::new(0.0),
            ui_state: RefCell::default(),
        }
    }

    // ------------------------------------------------------------------ services globaux

    fn hwnd(&self) -> Option<isize> {
        crate::app::with_app_ret(|a| a.hwnd.get()).flatten()
    }

    fn show_hidden(&self) -> bool {
        crate::app::with_app_ret(|a| a.show_hidden.get()).unwrap_or(false)
    }

    fn network_timeout(&self) -> Duration {
        crate::app::with_app_ret(|a| a.network_timeout).unwrap_or(Duration::from_secs(10))
    }

    fn host_post(&self, job: impl FnOnce(&fx_win::shell_host::HostCtx) + Send + 'static) {
        let id = self.id;
        let ok = crate::app::with_app_ret(|a| a.host_post_raw(job)).unwrap_or(false);
        if !ok {
            with_pane(id, |p| {
                p.flash_status("Le service Shell est indisponible.".into())
            });
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        crate::app::with_app_ret(|a| a.active_pane().id == self.id).unwrap_or(false)
    }

    /// Mise à jour des éléments globaux de la fenêtre (adresse, filtre…), seulement si ce volet
    /// est le volet actif.
    fn ui_global(&self, f: impl FnOnce(&crate::MainWindow)) {
        if self.is_active() {
            with_ui(f);
        }
    }

    // ------------------------------------------------------------------ état affiché

    fn push_ui(&self) {
        let Some(slot) = self.slot.get() else { return };
        let s = self.ui_state.borrow().clone();
        with_ui(|ui| {
            if slot == 1 {
                ui.set_status_1(s.status);
                ui.set_error_text_1(s.error);
                ui.set_loading_1(s.loading);
                ui.set_filtering_1(s.filtering);
                ui.set_sort_column_1(s.sort_column);
                ui.set_sort_descending_1(s.sort_descending);
                ui.set_rename_text_1(s.rename_text);
                ui.set_rename_stem_1(s.rename_stem);
                ui.set_rename_row_1(s.rename_row);
                ui.set_drop_row_1(s.drop_row);
                ui.set_view_mode_1(s.view_mode);
                ui.set_home_1(s.home);
            } else {
                ui.set_status_0(s.status);
                ui.set_error_text_0(s.error);
                ui.set_loading_0(s.loading);
                ui.set_filtering_0(s.filtering);
                ui.set_sort_column_0(s.sort_column);
                ui.set_sort_descending_0(s.sort_descending);
                ui.set_rename_text_0(s.rename_text);
                ui.set_rename_stem_0(s.rename_stem);
                ui.set_rename_row_0(s.rename_row);
                ui.set_drop_row_0(s.drop_row);
                ui.set_view_mode_0(s.view_mode);
                ui.set_home_0(s.home);
            }
        });
    }

    fn ui_with(&self, f: impl FnOnce(&mut UiState)) {
        f(&mut self.ui_state.borrow_mut());
        self.push_ui();
    }

    pub(crate) fn ui_status(&self, s: SharedString) {
        self.ui_with(|u| u.status = s);
    }
    pub(crate) fn ui_error(&self, s: SharedString) {
        self.ui_with(|u| u.error = s);
    }
    pub(crate) fn ui_loading(&self, b: bool) {
        self.ui_with(|u| u.loading = b);
    }
    fn ui_sort_column(&self, c: i32) {
        self.ui_with(|u| u.sort_column = c);
    }
    fn ui_sort_descending(&self, d: bool) {
        self.ui_with(|u| u.sort_descending = d);
    }
    fn ui_rename_row(&self, r: i32) {
        self.ui_with(|u| u.rename_row = r);
    }
    fn ui_rename_text(&self, s: SharedString) {
        self.ui_with(|u| u.rename_text = s);
    }
    fn ui_rename_stem(&self, n: i32) {
        self.ui_with(|u| u.rename_stem = n);
    }
    pub(crate) fn ui_drop_row(&self, r: i32) {
        self.ui_with(|u| u.drop_row = r);
    }
    fn ui_filtering(&self, b: bool) {
        self.ui_with(|u| u.filtering = b);
    }

    fn ui_scroll_to_row(&self, row: i32) {
        if let Some(slot) = self.slot.get() {
            with_ui(|ui| ui.invoke_scroll_to_row(slot as i32, row));
        }
    }
    pub(crate) fn ui_focus_list(&self) {
        if let Some(slot) = self.slot.get() {
            with_ui(|ui| ui.invoke_focus_list(slot as i32));
        }
    }
    fn ui_page_rows(&self) -> i32 {
        let slot = self.slot.get().unwrap_or(0);
        with_ui(|ui| {
            if slot == 1 {
                ui.get_page_rows_1()
            } else {
                ui.get_page_rows_0()
            }
        })
        .unwrap_or(10)
    }
    fn ui_row_anchor_x(&self) -> f32 {
        let slot = self.slot.get().unwrap_or(0) as i32;
        with_ui(|ui| ui.invoke_row_anchor_x(slot)).unwrap_or(0.0)
    }
    fn ui_row_anchor_y(&self, row: i32) -> f32 {
        let slot = self.slot.get().unwrap_or(0) as i32;
        with_ui(|ui| ui.invoke_row_anchor_y(slot, row)).unwrap_or(0.0)
    }
    pub(crate) fn max_scroll(&self) -> f32 {
        let slot = self.slot.get().unwrap_or(0);
        with_ui(|ui| {
            if slot == 1 {
                ui.get_max_scroll_1()
            } else {
                ui.get_max_scroll_0()
            }
        })
        .unwrap_or(0.0)
    }
    pub(crate) fn set_scroll(&self, y: f32) {
        if let Some(slot) = self.slot.get() {
            with_ui(|ui| ui.invoke_set_scroll(slot as i32, y));
        }
    }

    /// Rattache le volet à l'emplacement `slot` : modèle, état, défilement.
    pub(crate) fn bind(self: &Rc<Self>, slot: usize) {
        self.slot.set(Some(slot));
        let model = slint::ModelRc::from(self.model.clone());
        let grid = slint::ModelRc::from(self.grid_model.clone());
        let cols = with_ui(|ui| {
            if slot == 1 {
                ui.set_rows_1(model);
                ui.set_grid_lines_1(grid);
                ui.get_grid_columns_1()
            } else {
                ui.set_rows_0(model);
                ui.set_grid_lines_0(grid);
                ui.get_grid_columns_0()
            }
        });
        if let Some(cols) = cols {
            self.model.set_columns(cols.max(1) as usize);
        }
        self.push_ui();
        // Le défilement est restauré après la mise en page des lignes.
        let id = self.id;
        let y = self.saved_scroll.get();
        slint::Timer::single_shot(Duration::ZERO, move || with_pane(id, |p| p.set_scroll(y)));
    }

    /// Détache le volet de son emplacement (onglet masqué), en mémorisant le défilement.
    pub(crate) fn unbind(&self) {
        if let Some(slot) = self.slot.take() {
            let y = with_ui(|ui| {
                if slot == 1 {
                    ui.get_scroll_y_1()
                } else {
                    ui.get_scroll_y_0()
                }
            })
            .unwrap_or(0.0);
            self.saved_scroll.set(y);
        }
    }

    /// Met à jour les éléments globaux de la fenêtre pour ce volet (s'il est actif) :
    /// adresse, fil d'Ariane, titre, boutons, filtre, volet de navigation.
    pub(crate) fn sync_global(&self) {
        let Some(path) = self.location() else { return };
        let filter_text = self.st.borrow().filter_text.clone();
        let (can_back, can_forward) = {
            let h = self.history.borrow();
            (h.can_go_back(), h.can_go_forward())
        };
        let crumbs: Vec<Crumb> =
            crate::app::with_app_ret(|a| crate::nav::crumbs(&path, &a.drive_labels.borrow()))
                .unwrap_or_default()
                .into_iter()
                .map(|(label, p)| Crumb {
                    label: label.into(),
                    path: p.display().to_string().into(),
                })
                .collect();
        let shown = if crate::home::is_home(&path) {
            PathBuf::from("Accueil")
        } else {
            fx_win::path::display_form(&path)
        };
        let title = crate::nav::title_of(&path);
        self.ui_global(|ui| {
            ui.set_address(shown.display().to_string().into());
            ui.set_folder_title(title.into());
            ui.set_can_back(can_back);
            ui.set_can_forward(can_forward);
            ui.set_can_up(parent_of(&path).is_some());
            ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
            ui.set_filter_text(filter_text.into());
        });
        if self.is_active() {
            with_app(|a| a.refresh_nav_items(&path));
        }
    }

    fn update_nav_ui(&self, _path: &Path) {
        self.sync_global();
        with_app(|a| a.on_pane_location_changed());
    }

    /// Applique le réglage global « éléments masqués ».
    pub(crate) fn set_show_hidden(self: &Rc<Self>, show: bool) {
        {
            let mut st = self.st.borrow_mut();
            st.filter = st.filter.clone().with_hidden(show);
            st.view_version += 1;
        }
        self.recompute_view(false);
    }

    /// Valeurs de l'état affiché (auto-test).
    pub(crate) fn ui_snapshot(&self) -> UiState {
        self.ui_state.borrow().clone()
    }
}

impl Pane {
    /// Dossier affiché, `None` sur la page d'accueil (qui n'est pas un dossier : toutes les actions
    /// sur le disque sont alors sans effet).
    pub(crate) fn current_path(&self) -> Option<PathBuf> {
        self.location().filter(|p| !crate::home::is_home(p))
    }

    /// Emplacement affiché : un dossier ou la page d'accueil (adresse, onglets, session).
    pub(crate) fn location(&self) -> Option<PathBuf> {
        self.history.borrow().current().map(Path::to_path_buf)
    }

    pub(crate) fn is_home(&self) -> bool {
        self.location().is_some_and(|p| crate::home::is_home(&p))
    }

    /// Affiche la page d'accueil dans ce volet.
    fn show_home(self: &Rc<Self>, push: bool) {
        self.generation.set(self.generation.get() + 1);
        self.loader.cancel();
        if push {
            self.history.borrow_mut().navigate(crate::home::home_path());
        }
        {
            let mut st = self.st.borrow_mut();
            st.complete = true;
            st.folder = None;
            st.watcher = None;
            st.queued.clear();
            st.pending_select.clear();
            st.pending_rename = false;
            st.rename_entry = None;
            st.rename_gen += 1;
            st.press_deferred = None;
            st.marquee_base = None;
            for j in st.size_jobs.drain(..) {
                j.cancel();
            }
        }
        self.model.clear();
        self.ui_rename_row(-1);
        self.ui_error(SharedString::new());
        self.ui_filtering(false);
        self.ui_loading(false);
        self.ui_status(SharedString::new());
        self.ui_with(|u| u.home = true);
        self.ui_global(|ui| {
            ui.set_address_editing(false);
            ui.set_filter_text(SharedString::new());
        });
        self.sync_global();
        with_app(|a| {
            a.on_pane_location_changed();
            a.on_home_shown();
        });
    }

    pub(crate) fn navigate(self: &Rc<Self>, path: PathBuf, push: bool, select: Vec<Vec<u16>>) {
        if crate::home::is_home(&path) {
            self.show_home(push);
            return;
        }
        let pid = self.id;
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if push {
            self.history.borrow_mut().navigate(path.clone());
        }
        self.ui_rename_row(-1);
        self.ui_with(|u| u.home = false);
        let spec = {
            let mut st = self.st.borrow_mut();
            st.complete = false;
            st.pending_since = (!select.is_empty()).then(Instant::now);
            st.pending_select = select;
            st.pending_rename = false;
            st.filter = Filter::default().with_hidden(self.show_hidden());
            st.filter_text.clear();
            for j in st.size_jobs.drain(..) {
                j.cancel();
            }
            st.load_spec = st.spec;
            st.watcher = None;
            st.applying = false;
            st.queued.clear();
            st.rename_entry = None;
            st.rename_gen += 1;
            st.press_deferred = None;
            st.marquee_base = None;
            st.spec
        };
        *self.type_ahead.borrow_mut() = TypeAhead::default();
        self.ui_global(|ui| ui.set_filter_text(SharedString::new()));
        self.model.clear_dir_sizes();
        self.ui_error(SharedString::new());
        self.ui_filtering(false);
        self.ui_loading(true);
        self.ui_global(|ui| ui.set_address_editing(false));
        self.update_nav_ui(&path);
        self.nav_start.set(Instant::now());
        self.timing.set(NavTiming::default());
        tracing::debug!(path = %path.display(), generation, "navigation");
        let sink: Sink = Arc::new(move |ev| post_pane(pid, move |a| a.on_load_event(ev)));
        // La surveillance démarre sur le thread de listing, avant le listing : aucun
        // changement n'est perdu, et l'ouverture d'un dossier réseau ne bloque jamais l'interface.
        let watch: fx_core::source::WatchSink =
            Box::new(move |events| post_pane(pid, move |a| a.on_fs_events(generation, events)));
        self.loader.load(path, generation, spec, sink, Some(watch));
        // Dossier qui ne répond pas (partage réseau injoignable) : abandon après le délai.
        let timeout = self.network_timeout();
        slint::Timer::single_shot(timeout, move || {
            with_pane(pid, |a| {
                let waiting = a.generation.get() == generation
                    && a.timing.get().first_rows.is_none()
                    && !a.st.borrow().complete;
                if waiting {
                    a.cancel_loading(format!(
                        "Cet emplacement ne répond pas (aucune réponse après {} s). Appuyez sur F5 pour réessayer.",
                        timeout.as_secs()
                    ));
                }
            })
        });
    }

    /// Abandonne le chargement en cours (Échap, délai dépassé), y compris une E/S bloquée.
    pub(crate) fn cancel_loading(self: &Rc<Self>, message: String) {
        if self.st.borrow().complete {
            return;
        }
        self.loader.cancel();
        // Les résultats éventuels de ce chargement seront ignorés.
        self.generation.set(self.generation.get() + 1);
        {
            let mut st = self.st.borrow_mut();
            st.complete = true;
            st.folder = None;
            st.watcher = None;
            st.queued.clear();
        }
        tracing::info!("chargement abandonné : {message}");
        self.model.clear();
        self.ui_loading(false);
        self.ui_error(message.into());
        self.ui_status(SharedString::new());
    }

    pub(crate) fn navigate_to(self: &Rc<Self>, text: &str) {
        let t = text.trim().trim_matches('"');
        self.ui_global(|ui| ui.set_suggestions(ModelRc::default()));
        if t.is_empty() {
            if let Some(cur) = self.current_path() {
                self.update_nav_ui(&cur);
            }
            return;
        }
        if crate::home::is_home_text(t) {
            self.navigate(crate::home::home_path(), true, Vec::new());
            return;
        }
        let path = std::path::absolute(t).unwrap_or_else(|_| PathBuf::from(t));
        if self
            .current_path()
            .as_deref()
            .is_some_and(|c| same_path(c, &path))
        {
            self.refresh();
        } else {
            self.navigate(path, true, Vec::new());
        }
    }

    pub(crate) fn go_back(self: &Rc<Self>) {
        let p = self.history.borrow_mut().go_back().map(Path::to_path_buf);
        if let Some(p) = p {
            self.navigate(p, false, Vec::new());
        }
    }

    pub(crate) fn go_forward(self: &Rc<Self>) {
        let p = self.history.borrow_mut().go_forward().map(Path::to_path_buf);
        if let Some(p) = p {
            self.navigate(p, false, Vec::new());
        }
    }

    pub(crate) fn go_up(self: &Rc<Self>) {
        let Some(cur) = self.current_path() else { return };
        if let Some(parent) = parent_of(&cur) {
            // Comme l'Explorateur : on resélectionne le dossier d'où l'on vient.
            let select = cur.file_name().map(wide).into_iter().collect();
            self.navigate(parent, true, select);
        }
    }

    pub(crate) fn refresh(self: &Rc<Self>) {
        if self.is_home() {
            with_app(|a| a.refresh_home());
            return;
        }
        let Some(cur) = self.current_path() else { return };
        let select = self.selected_names();
        self.navigate(cur, false, select);
    }

    // ------------------------------------------------------------------ résultats du chargeur

    pub(crate) fn on_load_event(self: &Rc<Self>, ev: LoadEvent) {
        match ev {
            LoadEvent::Partial {
                generation,
                folder,
                order,
                ..
            } => {
                if generation != self.generation.get() || self.st.borrow().complete {
                    return;
                }
                let filter = {
                    let mut st = self.st.borrow_mut();
                    st.folder = Some(folder.clone());
                    st.filter.clone()
                };
                let len = folder.len();
                let visible = if filter.is_identity() {
                    order
                } else {
                    filter.apply(&folder, &order)
                };
                self.model.set_view(Some(folder), visible, Selection::new(len));
                self.mark_rows(false);
                self.update_status();
            }
            LoadEvent::Complete {
                generation,
                folder,
                keys,
                order,
                stats,
                watcher,
            } => {
                if generation != self.generation.get() {
                    return;
                }
                let (filter, needs_resort) = {
                    let mut st = self.st.borrow_mut();
                    st.folder = Some(folder.clone());
                    st.keys = Some(keys);
                    st.complete = true;
                    st.watcher = watcher;
                    st.order = Arc::new(order.clone());
                    (st.filter.clone(), st.spec != st.load_spec)
                };
                let visible = if filter.is_identity() {
                    order
                } else {
                    filter.apply(&folder, &order)
                };
                let mut sel = Selection::new(folder.len());
                let pending = self.take_pending(&folder);
                if let Some((found, _)) = &pending {
                    sel.select_only(found);
                }
                self.model.set_view(Some(folder), visible, sel);
                self.ui_loading(false);
                let mut t = self.timing.get();
                t.stats = stats;
                self.timing.set(t);
                self.mark_rows(true);
                self.after_pending(pending);
                if needs_resort {
                    self.recompute_view(needs_resort);
                }
                self.update_status();
                self.flush_queued();
            }
            LoadEvent::Failed { generation, error } => {
                if generation != self.generation.get() {
                    return;
                }
                tracing::info!(kind = ?error.kind, "{error}");
                {
                    let mut st = self.st.borrow_mut();
                    st.folder = None;
                    st.keys = None;
                    st.order = Arc::default();
                    st.complete = true;
                    st.watcher = None;
                    st.queued.clear();
                }
                self.model.clear();
                self.ui_loading(false);
                self.ui_error(error.user_message().into());
                self.ui_status(SharedString::new());
                self.mark_rows(true);
            }
        }
    }

    /// Entrées attendues (par nom) présentes dans `folder` : retirées de l'attente.
    pub(crate) fn take_pending(&self, folder: &Folder) -> Option<(Vec<u32>, bool)> {
        let mut st = self.st.borrow_mut();
        if st.pending_select.is_empty() {
            return None;
        }
        let wanted: HashSet<&[u16]> = st.pending_select.iter().map(Vec::as_slice).collect();
        let found: Vec<u32> = (0..folder.len() as u32)
            .filter(|&i| wanted.contains(folder.name(i)))
            .collect();
        if found.is_empty() {
            if st
                .pending_since
                .is_none_or(|t| t.elapsed() > PENDING_SELECT_TIMEOUT)
            {
                st.pending_select.clear();
                st.pending_rename = false;
            }
            return None;
        }
        st.pending_select.clear();
        st.pending_since = None;
        let rename = std::mem::take(&mut st.pending_rename);
        Some((found, rename))
    }

    pub(crate) fn after_pending(self: &Rc<Self>, pending: Option<(Vec<u32>, bool)>) {
        let Some((found, rename)) = pending else { return };
        if let Some(row) = found.first().and_then(|&e| self.model.row_of(e)) {
            self.ui_scroll_to_row(row as i32);
        }
        if rename {
            self.begin_rename();
        }
    }

    // ------------------------------------------------------------------ surveillance

    pub(crate) fn on_fs_events(self: &Rc<Self>, generation: u64, events: Vec<FsEvent>) {
        if generation != self.generation.get() {
            return;
        }
        let defer = {
            let mut st = self.st.borrow_mut();
            let defer = !st.complete || st.applying || st.rename_entry.is_some();
            if defer {
                st.queued.extend(events.iter().cloned());
            }
            defer
        };
        if !defer {
            self.start_apply(events);
        }
    }

    pub(crate) fn flush_queued(self: &Rc<Self>) {
        let events = {
            let mut st = self.st.borrow_mut();
            if !st.complete || st.applying || st.rename_entry.is_some() || st.queued.is_empty() {
                return;
            }
            std::mem::take(&mut st.queued)
        };
        self.start_apply(events);
    }

    pub(crate) fn start_apply(self: &Rc<Self>, events: Vec<FsEvent>) {
        let pid = self.id;
        let (folder, spec, filter) = {
            let mut st = self.st.borrow_mut();
            let Some(folder) = st.folder.clone() else { return };
            st.applying = true;
            st.apply_version = st.view_version;
            (folder, st.spec, st.filter.clone())
        };
        let generation = self.generation.get();
        self.loader
            .apply_events(folder, events, spec, filter, generation, move |r| {
                post_pane(pid, move |a| a.on_applied(r))
            });
    }

    pub(crate) fn on_applied(self: &Rc<Self>, r: ApplyResult) {
        if r.generation != self.generation.get() {
            return;
        }
        self.st.borrow_mut().applying = false;
        if r.reload {
            self.refresh();
            return;
        }
        // Les recalculs de vue en cours portaient sur l'ancien dossier : on les invalide.
        self.view_seq.set(self.view_seq.get() + 1);
        let old_sel = self.model.selection();
        let old_focus_row = old_sel.focus_row(&self.model.visible());
        let mut sel = old_sel.remap(r.folder.len(), |e| r.map.get(e as usize).copied().flatten());
        // Les éléments sélectionnés ont disparu (suppression) : on sélectionne celui qui a pris
        // leur place, comme l'Explorateur.
        if sel.is_empty()
            && !old_sel.is_empty()
            && let Some(row) = old_focus_row
            && let Some(&e) = r.visible.get(row.min(r.visible.len().saturating_sub(1)))
        {
            sel.select_only(&[e]);
        }
        let rename_entry = self.st.borrow().rename_entry;
        let new_rename = rename_entry.and_then(|e| r.map.get(e as usize).copied().flatten());
        let needs_view = {
            let mut st = self.st.borrow_mut();
            st.folder = Some(r.folder.clone());
            st.keys = Some(r.keys.clone());
            st.order = Arc::new(r.order);
            st.rename_entry = new_rename;
            st.view_version != st.apply_version
        };
        let pending = self.take_pending(&r.folder);
        if let Some((found, _)) = &pending {
            sel.select_only(found);
        }
        self.model.set_view(Some(r.folder), r.visible, sel);
        if let Some(e) = new_rename
            && let Some(row) = self.model.row_of(e)
        {
            self.ui_rename_row(row as i32);
        } else if rename_entry.is_some() {
            self.ui_rename_row(-1);
        }
        self.after_pending(pending);
        if needs_view {
            self.recompute_view(true);
        }
        self.update_status();
        self.flush_queued();
    }

    // ------------------------------------------------------------------ tri, filtre

    pub(crate) fn recompute_view(self: &Rc<Self>, resort: bool) {
        let pid = self.id;
        let st = self.st.borrow();
        let (Some(folder), true) = (st.folder.clone(), st.complete) else {
            return;
        };
        let seq = self.view_seq.get() + 1;
        self.view_seq.set(seq);
        let generation = self.generation.get();
        let done = move |r: ViewResult| post_pane(pid, move |a| a.on_view(r));
        match (resort, st.keys.clone()) {
            (true, Some(keys)) => {
                self.loader
                    .compute_view(folder, keys, st.spec, st.filter.clone(), generation, seq, done)
            }
            _ => self.loader.filter_view(
                folder,
                st.order.clone(),
                st.spec,
                st.filter.clone(),
                generation,
                seq,
                done,
            ),
        }
    }

    pub(crate) fn on_view(self: &Rc<Self>, r: ViewResult) {
        if r.generation != self.generation.get() || r.seq != self.view_seq.get() {
            return;
        }
        if let Some(order) = r.order {
            let mut st = self.st.borrow_mut();
            st.order = Arc::new(order);
            st.load_spec = r.spec;
        }
        self.model.set_visible(r.visible);
        if let Some(row) = self.model.selection().focus_row(&self.model.visible()) {
            self.ui_scroll_to_row(row as i32);
        }
        self.update_status();
    }

    pub(crate) fn sort_by(self: &Rc<Self>, column: i32) {
        let column = match column {
            1 => SortColumn::Modified,
            2 => SortColumn::Type,
            3 => SortColumn::Size,
            _ => SortColumn::Name,
        };
        let spec = {
            let mut st = self.st.borrow_mut();
            st.spec = st.spec.clicked(column);
            st.view_version += 1;
            st.spec
        };
        self.ui_sort_column(column_index(spec.column));
        self.ui_sort_descending(spec.descending);
        self.recompute_view(true);
    }

    pub(crate) fn filter_edited(self: &Rc<Self>, text: &str) {
        {
            let mut st = self.st.borrow_mut();
            st.filter = Filter::new(text).with_hidden(self.show_hidden());
            st.filter_text = text.to_string();
            st.view_version += 1;
        }
        self.ui_filtering(!text.trim().is_empty());
        self.recompute_view(false);
    }

    // ------------------------------------------------------------------ sélection, souris

    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(folder) = self.model.folder() else {
            return Vec::new();
        };
        self.model
            .selected()
            .into_iter()
            .map(|e| folder.child_path(e))
            .collect()
    }

    pub(crate) fn selected_names(&self) -> Vec<Vec<u16>> {
        let Some(folder) = self.model.folder() else {
            return Vec::new();
        };
        self.model
            .selected()
            .into_iter()
            .map(|e| folder.name(e).to_vec())
            .collect()
    }

    pub(crate) fn row_pointer(self: &Rc<Self>, row: i32, kind: i32, ctrl: bool, shift: bool, on_name: bool) {
        let Ok(row) = usize::try_from(row) else { return };
        match kind {
            0 => {
                let already = self
                    .model
                    .entry_at(row)
                    .is_some_and(|e| self.model.selection().is_selected(e));
                let base = (!on_name && ctrl).then(|| self.model.selection());
                let sole = already && self.model.selected().len() == 1;
                {
                    let mut st = self.st.borrow_mut();
                    st.rename_gen += 1;
                    st.rename_armed = false;
                    st.rename_candidate = sole && !ctrl && !shift;
                    st.dragging = false;
                    st.marquee_base = base;
                    st.press_deferred = (already && !ctrl && !shift).then_some(row);
                }
                if !(already && !ctrl && !shift) {
                    self.model.update_selection(|s, v| s.click(v, row, ctrl, shift));
                    self.update_status();
                }
            }
            1 => {
                let deferred = {
                    let mut st = self.st.borrow_mut();
                    let d = if st.dragging { None } else { st.press_deferred };
                    st.rename_armed = std::mem::take(&mut st.rename_candidate) && !st.dragging;
                    st.press_deferred = None;
                    st.dragging = false;
                    d
                };
                if let Some(r) = deferred {
                    self.model.update_selection(|s, v| s.click(v, r, false, false));
                    self.update_status();
                }
            }
            // Relâchement sur le nom (après un appui sur le nom) : renommage au bout du délai de
            // double-clic, sauf double-clic, nouvel appui ou changement de sélection entre-temps.
            4 => self.arm_slow_rename(row),
            2 => {
                self.cancel_slow_rename();
                self.open_selection();
            }
            3 => {
                self.cancel_slow_rename();
                self.model.update_selection(|s, v| s.right_click(v, row));
                self.update_status();
                let (x, y) = fx_win::shell::cursor_pos();
                self.show_menu(false, x, y);
            }
            _ => {}
        }
    }

    fn cancel_slow_rename(&self) {
        let mut st = self.st.borrow_mut();
        st.rename_gen += 1;
        st.rename_armed = false;
        st.rename_candidate = false;
    }

    fn arm_slow_rename(self: &Rc<Self>, row: usize) {
        let Some(entry) = self.model.entry_at(row) else {
            return;
        };
        let generation = {
            let mut st = self.st.borrow_mut();
            if !std::mem::take(&mut st.rename_armed) || st.rename_entry.is_some() {
                return;
            }
            st.rename_gen += 1;
            st.rename_gen
        };
        let id = self.id;
        let delay = Duration::from_millis(u64::from(fx_win::shell::double_click_ms()) + 60);
        slint::Timer::single_shot(delay, move || {
            with_pane(id, |p| {
                let still = p.st.borrow().rename_gen == generation
                    && p.st.borrow().rename_entry.is_none()
                    && p.model.selected() == [entry];
                if still {
                    p.begin_rename();
                }
            })
        });
    }

    pub(crate) fn background_pointer(self: &Rc<Self>, kind: i32, ctrl: bool, _shift: bool) {
        self.cancel_slow_rename();
        match kind {
            0 => {
                let base = ctrl.then(|| self.model.selection());
                self.st.borrow_mut().marquee_base = base;
                if !ctrl {
                    self.model.update_selection(|s, _| s.clear());
                    self.update_status();
                }
            }
            3 => {
                self.model.update_selection(|s, _| s.clear());
                self.update_status();
                let (x, y) = fx_win::shell::cursor_pos();
                self.show_menu(true, x, y);
            }
            _ => {}
        }
    }

    /// Rectangle de sélection en vue icônes : lignes `la..=lb`, colonnes `ca..=cb`.
    pub(crate) fn marquee_grid(self: &Rc<Self>, la: i32, lb: i32, ca: i32, cb: i32) {
        let base = {
            let mut st = self.st.borrow_mut();
            st.press_deferred = None;
            st.marquee_base.clone()
        };
        let cols = self.model.columns();
        let len = self.model.len();
        let (la, lb) = (la.max(0) as usize, lb.max(0) as usize);
        let (ca, cb) = (ca.max(0) as usize, (cb.max(0) as usize).min(cols - 1));
        let rows: Vec<usize> = (la..=lb)
            .flat_map(|l| (ca..=cb).map(move |c| l * cols + c))
            .take_while(|&r| r < len)
            .collect();
        self.model
            .update_selection(|s, v| s.select_rows(v, &rows, base.as_ref()));
        self.update_status();
    }

    pub(crate) fn set_grid(&self, grid: bool) {
        self.model.set_grid(grid);
        self.ui_with(|u| u.view_mode = i32::from(grid));
    }

    /// Bascule entre la vue détails et la vue icônes, en gardant l'élément courant visible.
    pub(crate) fn toggle_view(self: &Rc<Self>) {
        let grid = !self.model.is_grid();
        self.set_grid(grid);
        let focus = self.model.selection().focus_row(&self.model.visible());
        let id = self.id;
        slint::Timer::single_shot(Duration::ZERO, move || {
            with_pane(id, |p| {
                p.set_scroll(0.0);
                if let Some(r) = focus {
                    p.ui_scroll_to_row(r as i32);
                }
            })
        });
    }

    pub(crate) fn marquee_changed(self: &Rc<Self>, a: i32, b: i32, _ctrl: bool) {
        let base = {
            let mut st = self.st.borrow_mut();
            st.press_deferred = None;
            st.marquee_base.clone()
        };
        let (a, b) = (a.max(0) as usize, b.max(0) as usize);
        self.model
            .update_selection(|s, v| s.rectangle(v, a, b, base.as_ref()));
        self.update_status();
    }

    pub(crate) fn drag_out(self: &Rc<Self>, row: i32) {
        let Ok(row) = usize::try_from(row) else { return };
        if self.st.borrow().rename_entry.is_some() {
            return;
        }
        {
            let mut st = self.st.borrow_mut();
            st.press_deferred = None;
            st.dragging = true;
        }
        if self
            .model
            .entry_at(row)
            .is_some_and(|e| !self.model.selection().is_selected(e))
        {
            self.model.update_selection(|s, v| s.click(v, row, false, false));
        }
        let paths = self.selected_paths();
        let Some(hwnd) = self.hwnd() else { return };
        if paths.is_empty() {
            return;
        }
        // Boucle modale d'OLE sur le thread d'interface (exigence d'OLE pour la source).
        if let Err(e) = fx_win::dragdrop::drag_files(hwnd, &paths) {
            tracing::warn!("glisser-déposer : {e}");
        }
        // OLE a capturé la souris : Slint n'a pas vu le relâchement du bouton.
        crate::app::with_ui(|ui| {
            let _ = ui
                .window()
                .dispatch_event_with_result(SlintEvent::PointerReleased {
                    position: LogicalPosition::new(0.0, 0.0),
                    button: PointerEventButton::Left,
                });
        });
        self.ui_drop_row(-1);
    }

    /// Survol d'un glisser-déposer : `row` est la ligne sous le pointeur (-1 si aucune).
    pub(crate) fn drop_hover(self: &Rc<Self>, row: i32) -> Option<PathBuf> {
        let folder = self.model.folder();
        if let (Ok(r), Some(folder)) = (usize::try_from(row), &folder)
            && let Some(e) = self.model.entry_at(r)
            && folder.entry(e).is_some_and(|en| en.is_dir())
        {
            self.ui_drop_row(row);
            return Some(folder.child_path(e));
        }
        self.ui_drop_row(-1);
        let st = self.st.borrow();
        if st.complete && st.folder.is_some() {
            self.current_path()
        } else {
            None
        }
    }

    pub(crate) fn drop_files(self: &Rc<Self>, dest: PathBuf, paths: Vec<PathBuf>, effect: u32) {
        self.ui_drop_row(-1);
        if paths.is_empty() {
            return;
        }
        let is_move = effect == fx_win::dragdrop::EFFECT_MOVE;
        let what = if is_move { "Déplacement" } else { "Copie" };
        self.run_job(
            Job::Drop {
                items: paths,
                dest,
                is_move,
            },
            what,
        );
    }

    // ------------------------------------------------------------------ clavier

    pub(crate) fn on_key(self: &Rc<Self>, text: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        if self.st.borrow().rename_entry.is_some() {
            return false;
        }
        let Some(mut c) = text.chars().next() else {
            return false;
        };
        // Ctrl+lettre peut arriver comme caractère de contrôle (0x01 = A).
        if ctrl && ('\u{1}'..='\u{1a}').contains(&c) {
            c = (b'a' + (c as u8 - 1)) as char;
        }
        let is = |k: Key| c == char::from(k);
        if alt {
            if is(Key::LeftArrow) {
                self.go_back();
            } else if is(Key::RightArrow) {
                self.go_forward();
            } else if is(Key::UpArrow) {
                self.go_up();
            } else if is(Key::Home) {
                self.navigate(crate::home::home_path(), true, Vec::new());
            } else {
                return false;
            }
            return true;
        }
        if is(Key::Escape) && !self.st.borrow().complete {
            self.cancel_loading("Chargement annulé. Appuyez sur F5 pour réessayer.".into());
        } else if is(Key::F5) {
            self.refresh();
        } else if is(Key::F2) {
            self.begin_rename();
        } else if is(Key::Delete) {
            self.delete_selection(shift);
        } else if is(Key::Menu) || (shift && is(Key::F10)) {
            self.keyboard_menu();
        } else if is(Key::Return) {
            self.open_selection();
        } else if is(Key::Backspace) {
            self.go_up();
        } else if ctrl
            && !is(Key::UpArrow)
            && !is(Key::DownArrow)
            && !is(Key::PageUp)
            && !is(Key::PageDown)
            && !is(Key::Home)
            && !is(Key::End)
        {
            match c.to_ascii_lowercase() {
                'a' => {
                    self.model.update_selection(|s, v| s.select_all(v));
                    self.update_status();
                }
                'c' => self.clipboard_set(false),
                'x' => self.clipboard_set(true),
                'v' => self.paste(),
                'n' if shift => self.new_folder(),
                'h' => with_app(|a| a.toggle_hidden()),
                ' ' => {
                    self.model.update_selection(|s, _| s.toggle_focus());
                    self.update_status();
                }
                _ => return false,
            }
        } else if let Some(target) = self.nav_target(c) {
            if let Some(row) = target {
                self.model.update_selection(|s, v| s.move_to(v, row, ctrl, shift));
                self.ui_scroll_to_row(row as i32);
                self.update_status();
            }
        } else if !c.is_control() && (c as u32) < 0xF700 && !ctrl {
            // Saisie au clavier : saute à l'élément dont le nom commence par le texte tapé.
            let Some(folder) = self.model.folder() else {
                return true;
            };
            let visible = self.model.visible();
            let current = self.model.selection().focus_row(&visible);
            let row = self
                .type_ahead
                .borrow_mut()
                .push(c, Instant::now(), visible.len(), current, |r| {
                    folder.display_name(visible[r])
                });
            if let Some(row) = row {
                self.model
                    .update_selection(|s, v| s.move_to(v, row, false, false));
                self.ui_scroll_to_row(row as i32);
                self.update_status();
            }
        } else {
            return false;
        }
        true
    }

    /// Ligne cible d'une touche de déplacement (`Some(None)` : touche reconnue, liste vide).
    pub(crate) fn nav_target(&self, c: char) -> Option<Option<usize>> {
        let is = |k: Key| c == char::from(k);
        let len = self.model.len();
        let cur = self.model.selection().focus_row(&self.model.visible());
        // En vue icônes : haut/bas changent de ligne de tuiles, gauche/droite d'élément.
        let step = if self.model.is_grid() {
            self.model.columns()
        } else {
            1
        };
        let page = self.ui_page_rows().max(1) as usize * step;
        let last = len.saturating_sub(1);
        let target = if is(Key::DownArrow) {
            cur.map_or(0, |r| if r + step <= last { r + step } else { r })
        } else if is(Key::UpArrow) {
            cur.map_or(0, |r| r.checked_sub(step).unwrap_or(r))
        } else if step > 1 && is(Key::RightArrow) {
            cur.map_or(0, |r| (r + 1).min(last))
        } else if step > 1 && is(Key::LeftArrow) {
            cur.map_or(0, |r| r.saturating_sub(1))
        } else if is(Key::PageDown) {
            cur.map_or(0, |r| (r + page).min(last))
        } else if is(Key::PageUp) {
            cur.map_or(0, |r| r.saturating_sub(page))
        } else if is(Key::Home) {
            0
        } else if is(Key::End) {
            last
        } else {
            return None;
        };
        Some((len > 0).then_some(target))
    }

    // ------------------------------------------------------------------ commandes

    pub(crate) fn open_selection(self: &Rc<Self>) {
        let pid = self.id;
        let Some(folder) = self.model.folder() else { return };
        let sel = self.model.selected();
        if let [single] = sel[..]
            && folder.entry(single).is_some_and(|e| e.is_dir())
        {
            self.navigate(folder.child_path(single), true, Vec::new());
            return;
        }
        let owner = self.hwnd();
        let files: Vec<PathBuf> = sel
            .iter()
            .filter(|&&e| folder.entry(e).is_some_and(|en| !en.is_dir()))
            .take(16)
            .map(|&e| folder.child_path(e))
            .collect();
        for path in files {
            self.host_post(move |_| {
                if let Err(e) = fx_win::shell::open(&path, owner) {
                    let msg = format!("Impossible d'ouvrir « {} » : {}", path.display(), e.message());
                    post_pane(pid, move |a| a.flash_status(msg));
                }
            });
        }
    }

    pub(crate) fn show_menu(self: &Rc<Self>, background: bool, x: i32, y: i32) {
        let pid = self.id;
        let Some(folder_path) = self.current_path() else {
            return;
        };
        let items = if background {
            Vec::new()
        } else {
            self.selected_paths()
        };
        if !background && items.is_empty() {
            return;
        }
        let owner = self.hwnd().unwrap_or(0);
        self.host_post(move |ctx| {
            let opts = MenuOptions {
                x,
                y,
                owner,
                auto_dismiss_ms: None,
            };
            let r = fx_win::context_menu::show(ctx, &folder_path, &items, opts).map_err(|e| e.message());
            post_pane(pid, move |a| a.on_menu_done(r));
        });
    }

    pub(crate) fn keyboard_menu(self: &Rc<Self>) {
        let visible = self.model.visible();
        let sel = self.model.selection();
        let row = sel.focus_row(&visible).filter(|&r| sel.is_selected(visible[r]));
        let scale = crate::app::with_ui(|ui| ui.window().scale_factor()).unwrap_or(1.0);
        let lx = self.ui_row_anchor_x();
        let ly = match row {
            Some(r) => self.ui_row_anchor_y(r as i32),
            None => self.ui_row_anchor_y(-1),
        };
        let (x, y) = fx_win::shell::client_to_screen(
            self.hwnd().unwrap_or(0),
            (lx * scale) as i32,
            (ly * scale) as i32,
        );
        self.show_menu(row.is_none(), x, y);
    }

    pub(crate) fn on_menu_done(self: &Rc<Self>, r: Result<MenuOutcome, String>) {
        match r {
            Ok(out) => {
                tracing::debug!(invoked = ?out.invoked, app = ?out.app_command, "menu contextuel");
                if out.app_command == Some(AppCommand::Rename) {
                    self.begin_rename();
                }
            }
            Err(e) => self.flash_status(format!("Menu contextuel : {e}")),
        }
    }

    pub(crate) fn begin_rename(self: &Rc<Self>) {
        let Some(folder) = self.model.folder() else { return };
        let sel = self.model.selection();
        let entry = match sel.focus() {
            Some(f) if sel.is_selected(f) => Some(f),
            _ => self.model.selected().first().copied(),
        };
        let Some(entry) = entry else { return };
        let Some(row) = self.model.row_of(entry) else {
            return;
        };
        let name = folder.display_name(entry);
        let is_dir = folder.entry(entry).is_some_and(|e| e.is_dir());
        // Comme l'Explorateur : l'extension n'est pas présélectionnée.
        let stem = match name.rfind('.') {
            Some(dot) if dot > 0 && !is_dir => dot,
            _ => name.len(),
        };
        self.st.borrow_mut().rename_entry = Some(entry);
        self.ui_scroll_to_row(row as i32);
        self.ui_rename_text(name.into());
        self.ui_rename_stem(stem as i32);
        self.ui_rename_row(row as i32);
    }

    pub(crate) fn rename_commit(self: &Rc<Self>, text: &str) {
        let Some(entry) = self.st.borrow_mut().rename_entry.take() else {
            return;
        };
        self.ui_rename_row(-1);
        self.ui_focus_list();
        let folder = self.model.folder();
        let new_name = text.trim();
        if let Some(folder) = folder {
            let old = folder.display_name(entry);
            if new_name.is_empty() || new_name == old {
                // rien à faire
            } else if new_name.contains(INVALID_NAME_CHARS) {
                self.flash_status("Un nom ne peut pas contenir les caractères \\ / : * ? \" < > |".into());
            } else {
                {
                    let mut st = self.st.borrow_mut();
                    st.pending_select = vec![new_name.encode_utf16().collect()];
                    st.pending_since = Some(Instant::now());
                }
                let op = FileOp::Rename {
                    item: folder.child_path(entry),
                    new_name: OsString::from(new_name),
                };
                self.run_job(Job::Op(op), "Renommage");
            }
        }
        self.flush_queued();
    }

    pub(crate) fn rename_cancel(self: &Rc<Self>) {
        self.st.borrow_mut().rename_entry = None;
        self.ui_rename_row(-1);
        self.ui_focus_list();
        self.flush_queued();
    }

    pub(crate) fn delete_selection(self: &Rc<Self>, permanent: bool) {
        let items = self.selected_paths();
        if items.is_empty() {
            return;
        }
        // Suppression définitive : Windows demande confirmation (IFileOperation).
        self.run_job(Job::Op(FileOp::Delete { items, permanent }), "Suppression");
    }

    pub(crate) fn new_folder(self: &Rc<Self>) {
        let Some(parent) = self.current_path() else { return };
        {
            let mut st = self.st.borrow_mut();
            st.pending_rename = true;
            st.pending_select.clear();
        }
        let op = FileOp::NewFolder {
            parent,
            name: OsString::from("Nouveau dossier"),
        };
        self.run_job(Job::Op(op), "Nouveau dossier");
    }

    pub(crate) fn new_file(self: &Rc<Self>) {
        let Some(parent) = self.current_path() else { return };
        {
            let mut st = self.st.borrow_mut();
            st.pending_rename = true;
            st.pending_select.clear();
        }
        let op = FileOp::NewFile {
            parent,
            name: OsString::from("Nouveau fichier.txt"),
        };
        self.run_job(Job::Op(op), "Nouveau fichier");
    }

    /// Met en évidence `names` pendant ~1,6 s (animation de fondu dans l'interface).
    pub(crate) fn flash_created(self: &Rc<Self>, names: &[Vec<u16>]) {
        self.model.set_fresh(names, true);
        let names = names.to_vec();
        let id = self.id;
        slint::Timer::single_shot(Duration::from_millis(1600), move || {
            with_pane(id, |p| p.model.set_fresh(&names, false));
        });
    }

    pub(crate) fn clipboard_set(self: &Rc<Self>, cut: bool) {
        let pid = self.id;
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.host_post(move |_| {
            let r = fx_win::clipboard::set_files(&paths, cut);
            post_pane(pid, move |a| match r {
                Ok(()) => {
                    let verb = if cut { "coupé" } else { "copié" };
                    let msg = if n == 1 {
                        format!("1 élément {verb}")
                    } else {
                        format!("{n} éléments {verb}s")
                    };
                    a.flash_status(msg);
                }
                Err(e) => a.flash_status(format!("Presse-papiers : {}", e.message())),
            });
        });
    }

    pub(crate) fn paste(self: &Rc<Self>) {
        let Some(dest) = self.current_path() else { return };
        if !fx_win::clipboard::has_files() {
            self.flash_status("Le presse-papiers ne contient pas de fichiers.".into());
            return;
        }
        self.run_job(Job::Paste { dest }, "Collage");
    }

    pub(crate) fn run_job(self: &Rc<Self>, job: Job, what: &'static str) {
        let pid = self.id;
        let owner = self.hwnd().unwrap_or(0);
        let generation = self.generation.get();
        ops::spawn(job, owner, move |r| {
            post_pane(pid, move |a| a.on_job_done(generation, what, r))
        });
    }

    pub(crate) fn on_job_done(self: &Rc<Self>, generation: u64, what: &str, r: Result<OpOutcome, String>) {
        match r {
            Err(e) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.pending_select.clear();
                    st.pending_rename = false;
                }
                tracing::warn!("{what} : {e}");
                self.flash_status(format!("{what} : {e}"));
            }
            Ok(out) => {
                if out.aborted {
                    self.flash_status(format!("{what} : opération annulée"));
                }
                if generation != self.generation.get() || out.created.is_empty() {
                    return;
                }
                let Some(cur) = self.current_path() else { return };
                let names: Vec<Vec<u16>> = out
                    .created
                    .iter()
                    .filter(|p| p.parent().is_some_and(|d| same_path(d, &cur)))
                    .filter_map(|p| p.file_name().map(wide))
                    .collect();
                if names.is_empty() {
                    return;
                }
                self.flash_created(&names);
                {
                    let mut st = self.st.borrow_mut();
                    st.pending_select = names;
                    st.pending_since = Some(Instant::now());
                }
                // Les éléments sont peut-être déjà affichés (surveillance plus rapide que nous).
                if let Some(folder) = self.model.folder()
                    && let Some((found, rename)) = self.take_pending(&folder)
                {
                    self.model.update_selection(|s, _| s.select_only(&found));
                    self.after_pending(Some((found, rename)));
                    self.update_status();
                }
            }
        }
    }

    // ------------------------------------------------------------------ barre d'état

    pub(crate) fn update_status(&self) {
        let (complete, filtered, total) = {
            let st = self.st.borrow();
            let Some(folder) = &st.folder else { return };
            (st.complete, st.filter.has_query(), folder.len())
        };
        let shown = self.model.len();
        let total = if complete { total.max(shown) } else { total };
        let mut s = if !filtered && complete {
            item_count(shown)
        } else if filtered && complete {
            format!("{} sur {}", group_thousands(shown as u64), item_count(total))
        } else {
            item_count(total)
        };
        let sel = self.model.selected();
        if let Some(folder) = self.model.folder()
            && !sel.is_empty()
        {
            let bytes: u64 = sel
                .iter()
                .filter_map(|&e| folder.entry(e))
                .filter(|e| !e.is_dir())
                .map(|e| e.size)
                .sum();
            if sel.len() == 1 {
                s.push_str(" · 1 élément sélectionné");
            } else {
                s.push_str(&format!(
                    " · {} éléments sélectionnés",
                    group_thousands(sel.len() as u64)
                ));
            }
            if bytes > 0 {
                s.push_str(&format!(" ({})", human_size(bytes)));
            }
        }
        if complete && let Some(ms) = self.timing.get().complete {
            match ms.as_millis() {
                0 => s.push_str(" · chargé en moins d'1 ms"),
                n => s.push_str(&format!(" · chargé en {n} ms")),
            }
        }
        self.ui_status(s.into());
        if self.is_active() {
            with_app(|a| a.update_preview());
        }
    }

    /// Message temporaire dans la barre d'état (erreurs, confirmations).
    pub(crate) fn flash_status(&self, msg: String) {
        let pid = self.id;
        let seq = self.status_seq.get() + 1;
        self.status_seq.set(seq);
        self.ui_status(msg.into());
        slint::Timer::single_shot(Duration::from_secs(5), move || {
            with_pane(pid, |a| {
                if a.status_seq.get() == seq {
                    a.update_status();
                }
            })
        });
    }

    // ------------------------------------------------------------------ instrumentation

    pub(crate) fn mark_rows(&self, complete: bool) {
        let elapsed = self.nav_start.get().elapsed();
        let mut t = self.timing.get();
        if t.first_rows.is_none() {
            t.first_rows = Some(elapsed);
        }
        let mut wait = self.paint_wait.get() | PAINT_FIRST;
        if complete {
            t.complete = Some(elapsed);
            wait |= PAINT_COMPLETE;
        }
        self.timing.set(t);
        self.paint_wait.set(wait);
    }

    pub(crate) fn painted(self: &Rc<Self>, wait: u8) {
        let elapsed = self.nav_start.get().elapsed();
        let mut t = self.timing.get();
        if wait & PAINT_FIRST != 0 && t.first_paint.is_none() {
            t.first_paint = Some(elapsed);
        }
        if wait & PAINT_COMPLETE != 0 {
            t.complete_paint = Some(elapsed);
        }
        self.timing.set(t);
        if wait & PAINT_COMPLETE != 0 {
            tracing::info!(
                first_rows_ms = t.first_rows.map(|d| d.as_secs_f64() * 1000.0),
                complete_ms = t.complete.map(|d| d.as_secs_f64() * 1000.0),
                entries = t.stats.entries,
                "dossier affiché"
            );
            with_app(|a| a.bench_navigation_painted());
        }
    }
}
