//! Contrôleur de la fenêtre : navigation, surveillance, sélection, clavier, menu contextuel,
//! renommage, opérations sur les fichiers, presse-papiers, glisser-déposer et instrumentation.
//!
//! Règle : aucun emprunt de `State` n'est gardé pendant un appel à l'interface Slint ou au
//! modèle, car ces appels peuvent déclencher des callbacks qui reviennent ici.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
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
use fx_core::sys::{WinCollator, WinSource};
use fx_win::context_menu::{AppCommand, MenuOptions, MenuOutcome};
use fx_win::fileop::{FileOp, OpOutcome};
use fx_win::shell_host::ShellHost;
use slint::platform::{Key, PointerEventButton, WindowEvent as SlintEvent};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, LogicalPosition, ModelRc, SharedString, VecModel};

use crate::icons::IconLoader;
use crate::model::FolderModel;
use crate::nav::NavEntry;
use crate::ops::{self, Job};
use crate::{Crumb, MainWindow, NavItem, Options, UiError};

/// Modes de mesure (`--bench-startup`, `--bench-open`).
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
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with_borrow(|a| a.clone());
    if let Some(app) = app {
        f(&app);
    }
}

fn with_app_ret<R>(f: impl FnOnce(&Rc<App>) -> R) -> Option<R> {
    let app = APP.with_borrow(|a| a.clone())?;
    Some(f(&app))
}

/// Exécute `f` sur le thread d'interface (appelable depuis n'importe quel thread).
fn post(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    if let Err(e) = slint::invoke_from_event_loop(move || with_app(f)) {
        tracing::warn!("événement perdu, boucle d'interface arrêtée : {e}");
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct NavTiming {
    first_rows: Option<Duration>,
    first_paint: Option<Duration>,
    complete: Option<Duration>,
    complete_paint: Option<Duration>,
    stats: LoadStats,
}

const PAINT_FIRST: u8 = 1;
const PAINT_COMPLETE: u8 = 2;
/// Délai pendant lequel on attend l'apparition d'éléments créés pour les sélectionner.
const PENDING_SELECT_TIMEOUT: Duration = Duration::from_secs(5);
const INVALID_NAME_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

#[derive(Default)]
struct State {
    folder: Option<Arc<Folder>>,
    keys: Option<Arc<NameKeys>>,
    order: Arc<Vec<u32>>,
    /// Tri demandé au chargeur pour le chargement en cours.
    load_spec: SortSpec,
    /// Tri voulu par l'utilisateur.
    spec: SortSpec,
    filter: Filter,
    /// Incrémenté à chaque changement de tri ou de filtre.
    view_version: u64,
    apply_version: u64,
    complete: bool,
    /// Noms à sélectionner dès qu'ils apparaissent (retour au parent, création, collage).
    pending_select: Vec<Vec<u16>>,
    pending_since: Option<Instant>,
    /// Passer en renommage dès que l'élément attendu apparaît (nouveau dossier).
    pending_rename: bool,
    watcher: Option<Box<dyn Send>>,
    applying: bool,
    queued: Vec<FsEvent>,
    rename_entry: Option<u32>,
    /// Appui sur un élément déjà sélectionné : la sélection ne change qu'au relâchement
    /// (pour pouvoir glisser plusieurs éléments).
    press_deferred: Option<usize>,
    dragging: bool,
    marquee_base: Option<Selection>,
}

struct BenchState {
    mode: BenchMode,
    start_path: PathBuf,
    first_frame_ms: Option<f64>,
    first_frame_done_ms: Option<f64>,
    initial_painted_ms: Option<f64>,
    iteration: usize,
    on_target: bool,
    results: Vec<NavTiming>,
    entries: usize,
    report: String,
}

pub(crate) struct App {
    ui: MainWindow,
    loader: Arc<Loader>,
    model: Rc<FolderModel>,
    host: Option<ShellHost>,
    history: RefCell<History>,
    generation: Cell<u64>,
    view_seq: Cell<u64>,
    st: RefCell<State>,
    hwnd: Cell<Option<isize>>,
    mica: bool,
    nav_entries: RefCell<Vec<NavEntry>>,
    drive_labels: RefCell<HashMap<PathBuf, String>>,
    suggest_seq: Cell<u64>,
    status_seq: Cell<u64>,
    type_ahead: RefCell<TypeAhead>,
    drop_reg: RefCell<Option<fx_win::dragdrop::DropRegistration>>,
    nav_start: Cell<Instant>,
    timing: Cell<NavTiming>,
    paint_wait: Cell<u8>,
    frames: Cell<u64>,
    main_start: Instant,
    bench: RefCell<Option<BenchState>>,
}

pub(crate) fn run(options: Options) -> Result<(), UiError> {
    let ui = MainWindow::new()?;
    ui.set_mica(options.mica);
    let icons = IconLoader::start(2, |batch| post(move |a| a.model.on_icons(batch)));
    let model = Rc::new(FolderModel::new(icons));
    ui.set_rows(ModelRc::from(model.clone()));
    let loader = Arc::new(Loader::new(Arc::new(WinSource::default()), Arc::new(WinCollator)));
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
        loader,
        model,
        host,
        history: RefCell::default(),
        generation: Cell::new(0),
        view_seq: Cell::new(0),
        st: RefCell::default(),
        hwnd: Cell::new(None),
        mica: options.mica,
        nav_entries: RefCell::default(),
        drive_labels: RefCell::default(),
        suggest_seq: Cell::new(0),
        status_seq: Cell::new(0),
        type_ahead: RefCell::default(),
        drop_reg: RefCell::new(None),
        nav_start: Cell::new(Instant::now()),
        timing: Cell::default(),
        paint_wait: Cell::new(0),
        frames: Cell::new(0),
        main_start: options.main_start,
        bench: RefCell::new(bench),
    });
    APP.with_borrow_mut(|a| *a = Some(app.clone()));

    wire_callbacks(&ui);
    {
        let mut ready = false;
        ui.window().on_winit_window_event(move |w, ev| {
            if !ready {
                // Premier événement : la fenêtre native existe.
                ready = true;
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
                    with_app(|a| a.model.set_icon_px(px));
                }
                WindowEvent::RedrawRequested => with_app(|a| a.on_redraw()),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    crate::nav::build(|entries| post(move |a| a.set_nav(entries)));
    // Le listing du dossier de départ démarre avant la création de la fenêtre.
    app.navigate(options.start.clone(), true, Vec::new());
    ui.show()?;
    ui.invoke_focus_list();
    slint::run_event_loop()?;
    app.drop_reg.borrow_mut().take();
    APP.with_borrow_mut(|a| *a = None);
    Ok(())
}

fn wire_callbacks(ui: &MainWindow) {
    ui.on_navigate_to(|text| with_app(|a| a.navigate_to(&text)));
    ui.on_address_edited(|text| with_app(|a| a.address_edited(text.to_string())));
    ui.on_go_back(|| with_app(|a| a.go_back()));
    ui.on_go_forward(|| with_app(|a| a.go_forward()));
    ui.on_go_up(|| with_app(|a| a.go_up()));
    ui.on_refresh(|| with_app(|a| a.refresh()));
    ui.on_nav_activated(|p| with_app(|a| a.navigate(PathBuf::from(p.as_str()), true, Vec::new())));
    ui.on_crumb_activated(|p| with_app(|a| a.navigate_to(&p)));
    ui.on_sort_by(|col| with_app(|a| a.sort_by(col)));
    ui.on_filter_edited(|text| with_app(|a| a.filter_edited(&text)));
    ui.on_row_pointer(|row, kind, ctrl, shift, on_name| {
        with_app(|a| a.row_pointer(row, kind, ctrl, shift, on_name))
    });
    ui.on_background_pointer(|kind, ctrl, shift| with_app(|a| a.background_pointer(kind, ctrl, shift)));
    ui.on_drag_out(|row| with_app(|a| a.drag_out(row)));
    ui.on_marquee_changed(|ra, rb, ctrl| with_app(|a| a.marquee_changed(ra, rb, ctrl)));
    ui.on_marquee_ended(|| with_app(|a| a.st.borrow_mut().marquee_base = None));
    ui.on_rename_commit(|t| with_app(|a| a.rename_commit(&t)));
    ui.on_rename_cancel(|| with_app(|a| a.rename_cancel()));
    ui.on_key(|text, ctrl, shift, alt| with_app_ret(|a| a.on_key(&text, ctrl, shift, alt)).unwrap_or(false));
}

fn wide(s: &OsStr) -> Vec<u16> {
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

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        fx_win::path::display_form(p)
            .to_string_lossy()
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Cible de dépôt de la fenêtre : délègue au contrôleur (thread d'interface).
struct UiDrop;

impl fx_win::dragdrop::DropHandler for UiDrop {
    fn hover(&self, x: i32, y: i32) -> Option<PathBuf> {
        with_app_ret(|a| a.drop_hover(x, y)).flatten()
    }
    fn leave(&self) {
        with_app(|a| a.ui.set_drop_row(-1));
    }
    fn drop_files(&self, dest: PathBuf, paths: Vec<PathBuf>, effect: u32) {
        with_app(|a| a.drop_files(dest, paths, effect));
    }
}

impl App {
    // ------------------------------------------------------------------ fenêtre native

    fn on_native_window(
        self: &Rc<Self>,
        w: &slint::Window,
        theme: Option<slint::winit_030::winit::window::Theme>,
    ) {
        self.hwnd.set(crate::window::hwnd(w));
        self.model.set_icon_px((16.0 * w.scale_factor()).round() as u32);
        let Some(h) = self.hwnd.get() else { return };
        crate::window::apply_style(h, theme, self.mica);
        match fx_win::dragdrop::register(h, Box::new(UiDrop)) {
            Ok(reg) => *self.drop_reg.borrow_mut() = Some(reg),
            Err(e) => tracing::warn!("cible de glisser-déposer non enregistrée : {e}"),
        }
    }

    // ------------------------------------------------------------------ navigation

    fn current_path(&self) -> Option<PathBuf> {
        self.history.borrow().current().map(Path::to_path_buf)
    }

    fn navigate(self: &Rc<Self>, path: PathBuf, push: bool, select: Vec<Vec<u16>>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if push {
            self.history.borrow_mut().navigate(path.clone());
        }
        self.ui.set_rename_row(-1);
        // La surveillance démarre avant le listing : aucun changement n'est perdu (les
        // événements arrivés pendant le listing sont appliqués ensuite).
        let watcher = self.loader.watch(
            &path,
            Box::new(move |events| post(move |a| a.on_fs_events(generation, events))),
        );
        let spec = {
            let mut st = self.st.borrow_mut();
            st.complete = false;
            st.pending_since = (!select.is_empty()).then(Instant::now);
            st.pending_select = select;
            st.pending_rename = false;
            st.filter = Filter::default();
            st.load_spec = st.spec;
            st.watcher = watcher;
            st.applying = false;
            st.queued.clear();
            st.rename_entry = None;
            st.press_deferred = None;
            st.marquee_base = None;
            st.spec
        };
        self.model.icon_loader().next_generation();
        *self.type_ahead.borrow_mut() = TypeAhead::default();
        self.ui.set_filter_text(SharedString::new());
        self.ui.set_error_text(SharedString::new());
        self.ui.set_loading(true);
        self.ui.set_address_editing(false);
        self.update_nav_ui(&path);
        self.nav_start.set(Instant::now());
        self.timing.set(NavTiming::default());
        tracing::debug!(path = %path.display(), generation, "navigation");
        let sink: Sink = Arc::new(|ev| post(move |a| a.on_load_event(ev)));
        self.loader.load(path, generation, spec, sink);
    }

    fn update_nav_ui(&self, path: &Path) {
        let shown = fx_win::path::display_form(path);
        self.ui.set_address(shown.display().to_string().into());
        let title = shown
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| shown.display().to_string());
        self.ui.set_folder_title(title.into());
        {
            let h = self.history.borrow();
            self.ui.set_can_back(h.can_go_back());
            self.ui.set_can_forward(h.can_go_forward());
        }
        self.ui.set_can_up(parent_of(path).is_some());
        let crumbs: Vec<Crumb> = crate::nav::crumbs(path, &self.drive_labels.borrow())
            .into_iter()
            .map(|(label, p)| Crumb {
                label: label.into(),
                path: p.display().to_string().into(),
            })
            .collect();
        self.ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
        self.refresh_nav_items(path);
    }

    fn set_nav(self: &Rc<Self>, entries: Vec<NavEntry>) {
        {
            let mut labels = self.drive_labels.borrow_mut();
            for e in entries.iter().filter(|e| !e.header && e.path.parent().is_none()) {
                labels.insert(e.path.clone(), e.label.clone());
            }
        }
        *self.nav_entries.borrow_mut() = entries;
        if let Some(cur) = self.current_path() {
            self.update_nav_ui(&cur);
        }
    }

    fn refresh_nav_items(&self, current: &Path) {
        let items: Vec<NavItem> = self
            .nav_entries
            .borrow()
            .iter()
            .map(|e| NavItem {
                label: e.label.clone().into(),
                glyph: e.glyph.into(),
                path: e.path.display().to_string().into(),
                header: e.header,
                current: !e.header && same_path(&e.path, current),
            })
            .collect();
        self.ui.set_nav_items(ModelRc::new(VecModel::from(items)));
    }

    fn navigate_to(self: &Rc<Self>, text: &str) {
        let t = text.trim().trim_matches('"');
        self.ui.set_suggestions(ModelRc::default());
        if t.is_empty() {
            if let Some(cur) = self.current_path() {
                self.update_nav_ui(&cur);
            }
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

    fn go_back(self: &Rc<Self>) {
        let p = self.history.borrow_mut().go_back().map(Path::to_path_buf);
        if let Some(p) = p {
            self.navigate(p, false, Vec::new());
        }
    }

    fn go_forward(self: &Rc<Self>) {
        let p = self.history.borrow_mut().go_forward().map(Path::to_path_buf);
        if let Some(p) = p {
            self.navigate(p, false, Vec::new());
        }
    }

    fn go_up(self: &Rc<Self>) {
        let Some(cur) = self.current_path() else { return };
        if let Some(parent) = parent_of(&cur) {
            // Comme l'Explorateur : on resélectionne le dossier d'où l'on vient.
            let select = cur.file_name().map(wide).into_iter().collect();
            self.navigate(parent, true, select);
        }
    }

    fn refresh(self: &Rc<Self>) {
        let Some(cur) = self.current_path() else { return };
        let select = self.selected_names();
        self.navigate(cur, false, select);
    }

    // ------------------------------------------------------------------ résultats du chargeur

    fn on_load_event(self: &Rc<Self>, ev: LoadEvent) {
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
                self.st.borrow_mut().folder = Some(folder.clone());
                let len = folder.len();
                self.model.set_view(Some(folder), order, Selection::new(len));
                self.mark_rows(false);
                self.update_status();
            }
            LoadEvent::Complete {
                generation,
                folder,
                keys,
                order,
                stats,
            } => {
                if generation != self.generation.get() {
                    return;
                }
                let (filter, needs_resort) = {
                    let mut st = self.st.borrow_mut();
                    st.folder = Some(folder.clone());
                    st.keys = Some(keys);
                    st.complete = true;
                    st.order = Arc::new(order.clone());
                    (st.filter.clone(), st.spec != st.load_spec)
                };
                let visible = if filter.is_empty() {
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
                self.ui.set_loading(false);
                let mut t = self.timing.get();
                t.stats = stats;
                self.timing.set(t);
                self.mark_rows(true);
                self.after_pending(pending);
                if needs_resort || !filter.is_empty() {
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
                self.ui.set_loading(false);
                self.ui.set_error_text(error.user_message().into());
                self.ui.set_status(SharedString::new());
                self.mark_rows(true);
            }
        }
    }

    /// Entrées attendues (par nom) présentes dans `folder` : retirées de l'attente.
    fn take_pending(&self, folder: &Folder) -> Option<(Vec<u32>, bool)> {
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

    fn after_pending(self: &Rc<Self>, pending: Option<(Vec<u32>, bool)>) {
        let Some((found, rename)) = pending else { return };
        if let Some(row) = found.first().and_then(|&e| self.model.row_of(e)) {
            self.ui.invoke_scroll_to_row(row as i32);
        }
        if rename {
            self.begin_rename();
        }
    }

    // ------------------------------------------------------------------ surveillance

    fn on_fs_events(self: &Rc<Self>, generation: u64, events: Vec<FsEvent>) {
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

    fn flush_queued(self: &Rc<Self>) {
        let events = {
            let mut st = self.st.borrow_mut();
            if !st.complete || st.applying || st.rename_entry.is_some() || st.queued.is_empty() {
                return;
            }
            std::mem::take(&mut st.queued)
        };
        self.start_apply(events);
    }

    fn start_apply(self: &Rc<Self>, events: Vec<FsEvent>) {
        let (folder, spec, filter) = {
            let mut st = self.st.borrow_mut();
            let Some(folder) = st.folder.clone() else { return };
            st.applying = true;
            st.apply_version = st.view_version;
            (folder, st.spec, st.filter.clone())
        };
        let generation = self.generation.get();
        self.loader
            .apply_events(folder, events, spec, filter, generation, |r| {
                post(move |a| a.on_applied(r))
            });
    }

    fn on_applied(self: &Rc<Self>, r: ApplyResult) {
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
            self.ui.set_rename_row(row as i32);
        } else if rename_entry.is_some() {
            self.ui.set_rename_row(-1);
        }
        self.after_pending(pending);
        if needs_view {
            self.recompute_view(true);
        }
        self.update_status();
        self.flush_queued();
    }

    // ------------------------------------------------------------------ tri, filtre

    fn recompute_view(self: &Rc<Self>, resort: bool) {
        let st = self.st.borrow();
        let (Some(folder), true) = (st.folder.clone(), st.complete) else {
            return;
        };
        let seq = self.view_seq.get() + 1;
        self.view_seq.set(seq);
        let generation = self.generation.get();
        let done = |r: ViewResult| post(move |a| a.on_view(r));
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

    fn on_view(self: &Rc<Self>, r: ViewResult) {
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
            self.ui.invoke_scroll_to_row(row as i32);
        }
        self.update_status();
    }

    fn sort_by(self: &Rc<Self>, column: i32) {
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
        self.ui.set_sort_column(column_index(spec.column));
        self.ui.set_sort_descending(spec.descending);
        self.recompute_view(true);
    }

    fn filter_edited(self: &Rc<Self>, text: &str) {
        {
            let mut st = self.st.borrow_mut();
            st.filter = Filter::new(text);
            st.view_version += 1;
        }
        self.recompute_view(false);
    }

    // ------------------------------------------------------------------ sélection, souris

    fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(folder) = self.model.folder() else {
            return Vec::new();
        };
        self.model
            .selected()
            .into_iter()
            .map(|e| folder.child_path(e))
            .collect()
    }

    fn selected_names(&self) -> Vec<Vec<u16>> {
        let Some(folder) = self.model.folder() else {
            return Vec::new();
        };
        self.model
            .selected()
            .into_iter()
            .map(|e| folder.name(e).to_vec())
            .collect()
    }

    fn row_pointer(self: &Rc<Self>, row: i32, kind: i32, ctrl: bool, shift: bool, on_name: bool) {
        let Ok(row) = usize::try_from(row) else { return };
        match kind {
            0 => {
                let already = self
                    .model
                    .entry_at(row)
                    .is_some_and(|e| self.model.selection().is_selected(e));
                let base = (!on_name && ctrl).then(|| self.model.selection());
                {
                    let mut st = self.st.borrow_mut();
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
                    st.press_deferred = None;
                    st.dragging = false;
                    d
                };
                if let Some(r) = deferred {
                    self.model.update_selection(|s, v| s.click(v, r, false, false));
                    self.update_status();
                }
            }
            2 => self.open_selection(),
            3 => {
                self.model.update_selection(|s, v| s.right_click(v, row));
                self.update_status();
                let (x, y) = fx_win::shell::cursor_pos();
                self.show_menu(false, x, y);
            }
            _ => {}
        }
    }

    fn background_pointer(self: &Rc<Self>, kind: i32, ctrl: bool, _shift: bool) {
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

    fn marquee_changed(self: &Rc<Self>, a: i32, b: i32, _ctrl: bool) {
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

    fn drag_out(self: &Rc<Self>, row: i32) {
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
        let Some(hwnd) = self.hwnd.get() else { return };
        if paths.is_empty() {
            return;
        }
        // Boucle modale d'OLE sur le thread d'interface (exigence d'OLE pour la source).
        if let Err(e) = fx_win::dragdrop::drag_files(hwnd, &paths) {
            tracing::warn!("glisser-déposer : {e}");
        }
        // OLE a capturé la souris : Slint n'a pas vu le relâchement du bouton.
        let _ = self
            .ui
            .window()
            .dispatch_event_with_result(SlintEvent::PointerReleased {
                position: LogicalPosition::new(0.0, 0.0),
                button: PointerEventButton::Left,
            });
        self.ui.set_drop_row(-1);
    }

    fn drop_hover(self: &Rc<Self>, x: i32, y: i32) -> Option<PathBuf> {
        let scale = self.ui.window().scale_factor();
        let row = self.ui.invoke_row_at(x as f32 / scale, y as f32 / scale);
        let folder = self.model.folder();
        if let (Ok(r), Some(folder)) = (usize::try_from(row), &folder)
            && let Some(e) = self.model.entry_at(r)
            && folder.entry(e).is_some_and(|en| en.is_dir())
        {
            self.ui.set_drop_row(row);
            return Some(folder.child_path(e));
        }
        self.ui.set_drop_row(-1);
        let st = self.st.borrow();
        if st.complete && st.folder.is_some() {
            self.current_path()
        } else {
            None
        }
    }

    fn drop_files(self: &Rc<Self>, dest: PathBuf, paths: Vec<PathBuf>, effect: u32) {
        self.ui.set_drop_row(-1);
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

    fn on_key(self: &Rc<Self>, text: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
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
            } else {
                return false;
            }
            return true;
        }
        if is(Key::F5) {
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
                ' ' => {
                    self.model.update_selection(|s, _| s.toggle_focus());
                    self.update_status();
                }
                _ => return false,
            }
        } else if let Some(target) = self.nav_target(c) {
            if let Some(row) = target {
                self.model.update_selection(|s, v| s.move_to(v, row, ctrl, shift));
                self.ui.invoke_scroll_to_row(row as i32);
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
                self.ui.invoke_scroll_to_row(row as i32);
                self.update_status();
            }
        } else {
            return false;
        }
        true
    }

    /// Ligne cible d'une touche de déplacement (`Some(None)` : touche reconnue, liste vide).
    fn nav_target(&self, c: char) -> Option<Option<usize>> {
        let is = |k: Key| c == char::from(k);
        let len = self.model.len();
        let cur = self.model.selection().focus_row(&self.model.visible());
        let page = self.ui.get_page_rows().max(1) as usize;
        let last = len.saturating_sub(1);
        let target = if is(Key::DownArrow) {
            cur.map_or(0, |r| (r + 1).min(last))
        } else if is(Key::UpArrow) {
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

    fn host_post(&self, job: impl FnOnce(&fx_win::shell_host::HostCtx) + Send + 'static) {
        match &self.host {
            Some(h) if h.post(job) => {}
            _ => self.flash_status("Le service Shell est indisponible.".into()),
        }
    }

    fn open_selection(self: &Rc<Self>) {
        let Some(folder) = self.model.folder() else { return };
        let sel = self.model.selected();
        if let [single] = sel[..]
            && folder.entry(single).is_some_and(|e| e.is_dir())
        {
            self.navigate(folder.child_path(single), true, Vec::new());
            return;
        }
        let owner = self.hwnd.get();
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
                    post(move |a| a.flash_status(msg));
                }
            });
        }
    }

    fn show_menu(self: &Rc<Self>, background: bool, x: i32, y: i32) {
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
        let owner = self.hwnd.get().unwrap_or(0);
        self.host_post(move |ctx| {
            let opts = MenuOptions {
                x,
                y,
                owner,
                auto_dismiss_ms: None,
            };
            let r = fx_win::context_menu::show(ctx, &folder_path, &items, opts).map_err(|e| e.message());
            post(move |a| a.on_menu_done(r));
        });
    }

    fn keyboard_menu(self: &Rc<Self>) {
        let visible = self.model.visible();
        let sel = self.model.selection();
        let row = sel.focus_row(&visible).filter(|&r| sel.is_selected(visible[r]));
        let scale = self.ui.window().scale_factor();
        let lx = self.ui.invoke_row_anchor_x();
        let ly = match row {
            Some(r) => self.ui.invoke_row_anchor_y(r as i32),
            None => self.ui.invoke_row_anchor_y(-1),
        };
        let (x, y) = fx_win::shell::client_to_screen(
            self.hwnd.get().unwrap_or(0),
            (lx * scale) as i32,
            (ly * scale) as i32,
        );
        self.show_menu(row.is_none(), x, y);
    }

    fn on_menu_done(self: &Rc<Self>, r: Result<MenuOutcome, String>) {
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

    fn begin_rename(self: &Rc<Self>) {
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
        self.ui.invoke_scroll_to_row(row as i32);
        self.ui.set_rename_text(name.into());
        self.ui.set_rename_stem(stem as i32);
        self.ui.set_rename_row(row as i32);
    }

    fn rename_commit(self: &Rc<Self>, text: &str) {
        let Some(entry) = self.st.borrow_mut().rename_entry.take() else {
            return;
        };
        self.ui.set_rename_row(-1);
        self.ui.invoke_focus_list();
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

    fn rename_cancel(self: &Rc<Self>) {
        self.st.borrow_mut().rename_entry = None;
        self.ui.set_rename_row(-1);
        self.ui.invoke_focus_list();
        self.flush_queued();
    }

    fn delete_selection(self: &Rc<Self>, permanent: bool) {
        let items = self.selected_paths();
        if items.is_empty() {
            return;
        }
        // Suppression définitive : Windows demande confirmation (IFileOperation).
        self.run_job(Job::Op(FileOp::Delete { items, permanent }), "Suppression");
    }

    fn new_folder(self: &Rc<Self>) {
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

    fn clipboard_set(self: &Rc<Self>, cut: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.host_post(move |_| {
            let r = fx_win::clipboard::set_files(&paths, cut);
            post(move |a| match r {
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

    fn paste(self: &Rc<Self>) {
        let Some(dest) = self.current_path() else { return };
        if !fx_win::clipboard::has_files() {
            self.flash_status("Le presse-papiers ne contient pas de fichiers.".into());
            return;
        }
        self.run_job(Job::Paste { dest }, "Collage");
    }

    fn run_job(self: &Rc<Self>, job: Job, what: &'static str) {
        let owner = self.hwnd.get().unwrap_or(0);
        let generation = self.generation.get();
        ops::spawn(job, owner, move |r| {
            post(move |a| a.on_job_done(generation, what, r))
        });
    }

    fn on_job_done(self: &Rc<Self>, generation: u64, what: &str, r: Result<OpOutcome, String>) {
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

    fn update_status(&self) {
        let (complete, filtered, total) = {
            let st = self.st.borrow();
            let Some(folder) = &st.folder else { return };
            (st.complete, !st.filter.is_empty(), folder.len())
        };
        let shown = self.model.len();
        let mut s = if filtered && complete {
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
        self.ui.set_status(s.into());
    }

    /// Message temporaire dans la barre d'état (erreurs, confirmations).
    fn flash_status(&self, msg: String) {
        let seq = self.status_seq.get() + 1;
        self.status_seq.set(seq);
        self.ui.set_status(msg.into());
        slint::Timer::single_shot(Duration::from_secs(5), move || {
            with_app(|a| {
                if a.status_seq.get() == seq {
                    a.update_status();
                }
            })
        });
    }

    // ------------------------------------------------------------------ instrumentation

    fn mark_rows(&self, complete: bool) {
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

    fn on_redraw(self: &Rc<Self>) {
        self.frames.set(self.frames.get() + 1);
        if self.frames.get() == 1 {
            let at = fx_win::process::ms_since_process_start();
            slint::Timer::single_shot(Duration::ZERO, move || {
                with_app(|a| a.bench_first_frame(at, fx_win::process::ms_since_process_start()))
            });
        }
        let wait = self.paint_wait.replace(0);
        if wait != 0 {
            // Le timer à délai nul s'exécute après le rendu de cette image.
            slint::Timer::single_shot(Duration::ZERO, move || with_app(|a| a.painted(wait)));
        }
    }

    fn painted(self: &Rc<Self>, wait: u8) {
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
            self.bench_navigation_painted();
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

    fn bench_navigation_painted(self: &Rc<Self>) {
        let mut next: Option<PathBuf> = None;
        let mut finish = false;
        {
            let mut guard = self.bench.borrow_mut();
            let Some(b) = guard.as_mut() else { return };
            if b.initial_painted_ms.is_none() {
                b.initial_painted_ms = fx_win::process::ms_since_process_start();
                match &b.mode {
                    BenchMode::Startup { .. } => finish = true,
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
                    let t = self.timing.get();
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
                with_app(|a| a.navigate(path, true, Vec::new()));
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
        let Some(folder) = self.model.folder() else {
            return Vec::new();
        };
        self.model
            .selected()
            .into_iter()
            .map(|e| folder.display_name(e))
            .collect()
    }

    /// Scénario d'auto-test : chaque étape agit comme l'utilisateur (mêmes fonctions que les
    /// callbacks de l'interface), vérifie le résultat, puis planifie la suivante.
    fn self_test_step(self: &Rc<Self>, step: usize, dir: PathBuf) {
        let next = |delay_ms: u64, d: PathBuf| {
            slint::Timer::single_shot(Duration::from_millis(delay_ms), move || {
                with_app(|a| a.self_test_step(step + 1, d));
            });
        };
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
                    self.model.len() == 3,
                    &format!(
                        "surveillance : 3 fichiers externes affichés ({})",
                        self.model.len()
                    ),
                );
                self.new_folder();
                next(1200, dir);
            }
            2 => {
                let renaming = self.st.borrow().rename_entry.is_some();
                self.check(
                    renaming && self.ui.get_rename_row() >= 0,
                    "nouveau dossier créé et passé en renommage",
                );
                self.check(
                    self.ui.get_rename_text() == "Nouveau dossier",
                    &format!("nom proposé « {} »", self.ui.get_rename_text()),
                );
                self.rename_commit("Dossier renommé");
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
                self.on_key("a", true, false, false);
                self.check(
                    self.model.selected().len() == 4,
                    &format!("Ctrl+A : {} éléments sélectionnés", self.model.selected().len()),
                );
                self.sort_by(3);
                next(400, dir);
            }
            4 => {
                let names: Vec<String> = self
                    .model
                    .visible()
                    .iter()
                    .filter_map(|&e| self.model.folder().map(|f| f.display_name(e)))
                    .collect();
                self.check(
                    names == ["Dossier renommé", "bravo.txt", "charlie.log", "alpha.txt"],
                    &format!("tri par taille, dossiers en premier ({names:?})"),
                );
                self.sort_by(0);
                self.on_key("c", false, false, false);
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
                let items = self.selected_paths();
                let owner = self.hwnd.get().unwrap_or(0);
                self.host_post(move |ctx| {
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
                    self.model.len() == 3,
                    &format!("suppression externe appliquée ({} éléments)", self.model.len()),
                );
                let sel = self.selected_display_names();
                self.check(
                    sel.len() == 1,
                    &format!("sélection reportée après suppression ({sel:?})"),
                );
                slint::Timer::single_shot(Duration::from_millis(300), || with_app(|a| a.bench_report()));
            }
            _ => {}
        }
    }

    fn bench_report(&self) {
        let Some(b) = self.bench.borrow_mut().take() else {
            return;
        };
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
            out += &format!(
                "mémoire au repos : privée {:.1} Mo, working set {:.1} Mo\n",
                m.private as f64 / 1_048_576.0,
                m.working_set as f64 / 1_048_576.0
            );
        }
        out += &b.report;
        print!("{out}");
        let target = match &b.mode {
            BenchMode::Startup { out } | BenchMode::Open { out, .. } | BenchMode::SelfTest { out } => {
                out.clone()
            }
        };
        if let Some(p) = target
            && let Err(e) = std::fs::write(&p, &out)
        {
            eprintln!("écriture de {} impossible : {e}", p.display());
        }
        let _ = slint::quit_event_loop();
    }
}
