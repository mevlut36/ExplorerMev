//! Contrôleur de la fenêtre : navigation, application des résultats du chargeur, tri, filtre,
//! sélection et instrumentation.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fx_core::Folder;
use fx_core::filter::Filter;
use fx_core::format::item_count;
use fx_core::history::{History, parent_of};
use fx_core::loader::{LoadEvent, LoadStats, Loader, Sink, ViewResult};
use fx_core::sort::{NameKeys, SortColumn, SortSpec};
use fx_core::sys::{WinCollator, WinSource};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, ModelRc, SharedString};

use crate::model::FolderModel;
use crate::shell_worker::ShellWorker;
use crate::{MainWindow, Options, UiError};

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
    complete: bool,
    pending_select: Option<Vec<u16>>,
    selected_entry: Option<u32>,
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
    shell: ShellWorker,
    history: RefCell<History>,
    generation: Cell<u64>,
    view_seq: Cell<u64>,
    st: RefCell<State>,
    hwnd: Cell<Option<isize>>,
    mica: bool,
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
    let model = Rc::new(FolderModel::default());
    ui.set_rows(ModelRc::from(model.clone()));
    let loader = Arc::new(Loader::new(Arc::new(WinSource::default()), Arc::new(WinCollator)));
    let shell = ShellWorker::start(|msg| post(move |a| a.ui.set_status(msg.into())));

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
        shell,
        history: RefCell::default(),
        generation: Cell::new(0),
        view_seq: Cell::new(0),
        st: RefCell::default(),
        hwnd: Cell::new(None),
        mica: options.mica,
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
        let mut styled = false;
        ui.window().on_winit_window_event(move |w, ev| {
            if !styled {
                // Premier événement : la fenêtre native existe, on pose Mica avant le premier rendu.
                styled = true;
                let theme = w.with_winit_window(|ww| ww.theme()).flatten();
                with_app(|a| {
                    a.hwnd.set(crate::window::hwnd(w));
                    if let Some(h) = a.hwnd.get() {
                        crate::window::apply_style(h, theme, a.mica);
                    }
                });
            }
            match ev {
                WindowEvent::ThemeChanged(theme) => with_app(|a| {
                    if let Some(h) = a.hwnd.get() {
                        crate::window::apply_style(h, Some(*theme), a.mica);
                    }
                }),
                WindowEvent::RedrawRequested => with_app(|a| a.on_redraw()),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    // Le listing du dossier de départ démarre avant la création de la fenêtre : les deux se
    // recouvrent.
    app.navigate(options.start.clone(), true, None);
    tracing::info!(
        ms = options.main_start.elapsed().as_secs_f64() * 1000.0,
        "fenêtre en cours de création"
    );
    ui.show()?;
    ui.invoke_focus_list();
    slint::run_event_loop()?;
    APP.with_borrow_mut(|a| *a = None);
    Ok(())
}

fn wire_callbacks(ui: &MainWindow) {
    ui.on_navigate_to(|text| with_app(|a| a.navigate_to(&text)));
    ui.on_go_back(|| with_app(|a| a.go_back()));
    ui.on_go_forward(|| with_app(|a| a.go_forward()));
    ui.on_go_up(|| with_app(|a| a.go_up()));
    ui.on_refresh(|| with_app(|a| a.refresh()));
    ui.on_open_row(|row| with_app(|a| a.open_row(row)));
    ui.on_sort_by(|col| with_app(|a| a.sort_by(col)));
    ui.on_filter_edited(|text| with_app(|a| a.filter_edited(&text)));
    ui.on_selection_changed(|row| with_app(|a| a.selection_changed(row)));
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

impl App {
    // ------------------------------------------------------------------ navigation

    fn navigate(self: &Rc<Self>, path: PathBuf, push: bool, select: Option<Vec<u16>>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if push {
            self.history.borrow_mut().navigate(path.clone());
        }
        let spec = {
            let mut st = self.st.borrow_mut();
            st.complete = false;
            st.pending_select = select;
            st.filter = Filter::default();
            st.selected_entry = None;
            st.load_spec = st.spec;
            st.spec
        };
        self.ui.set_filter_text(SharedString::new());
        self.ui.set_error_text(SharedString::new());
        self.ui.set_loading(true);
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
        let h = self.history.borrow();
        self.ui.set_can_back(h.can_go_back());
        self.ui.set_can_forward(h.can_go_forward());
        self.ui.set_can_up(parent_of(path).is_some());
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.history.borrow().current().map(Path::to_path_buf)
    }

    fn navigate_to(self: &Rc<Self>, text: &str) {
        let t = text.trim().trim_matches('"');
        if t.is_empty() {
            if let Some(cur) = self.current_path() {
                self.update_nav_ui(&cur);
            }
            return;
        }
        let path = std::path::absolute(t).unwrap_or_else(|_| PathBuf::from(t));
        if Some(&path) == self.current_path().as_ref() {
            self.refresh();
        } else {
            self.navigate(path, true, None);
        }
    }

    fn go_back(self: &Rc<Self>) {
        let p = self.history.borrow_mut().go_back().map(Path::to_path_buf);
        if let Some(p) = p {
            self.navigate(p, false, None);
        }
    }

    fn go_forward(self: &Rc<Self>) {
        let p = self.history.borrow_mut().go_forward().map(Path::to_path_buf);
        if let Some(p) = p {
            self.navigate(p, false, None);
        }
    }

    fn go_up(self: &Rc<Self>) {
        let Some(cur) = self.current_path() else { return };
        if let Some(parent) = parent_of(&cur) {
            // Comme l'Explorateur : on resélectionne le dossier d'où l'on vient.
            let select = cur.file_name().map(wide);
            self.navigate(parent, true, select);
        }
    }

    fn refresh(self: &Rc<Self>) {
        let Some(cur) = self.current_path() else { return };
        let select = {
            let st = self.st.borrow();
            match (&st.folder, st.selected_entry) {
                (Some(f), Some(e)) => Some(f.name(e).to_vec()),
                _ => None,
            }
        };
        self.navigate(cur, false, select);
    }

    fn open_row(self: &Rc<Self>, row: i32) {
        let Ok(row) = usize::try_from(row) else { return };
        let Some(entry) = self.model.entry_at(row) else {
            return;
        };
        let Some(folder) = self.st.borrow().folder.clone() else {
            return;
        };
        let Some(e) = folder.entry(entry).copied() else {
            return;
        };
        let path = folder.child_path(entry);
        if e.is_dir() {
            self.navigate(path, true, None);
        } else {
            self.shell.open(path, self.hwnd.get());
        }
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
                self.model.set(Some(folder), order);
                self.ui.set_selected(-1);
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
                    (st.filter.clone(), st.spec != st.load_spec)
                };
                let visible = if filter.is_empty() {
                    order.clone()
                } else {
                    filter.apply(&folder, &order)
                };
                self.st.borrow_mut().order = Arc::new(order);
                self.model.set(Some(folder.clone()), visible);
                self.ui.set_loading(false);
                let mut t = self.timing.get();
                t.stats = stats;
                self.timing.set(t);
                self.mark_rows(true);

                let pending = self.st.borrow_mut().pending_select.take();
                let selected_row = pending.and_then(|name| folder.find(&name)).and_then(|e| {
                    self.st.borrow_mut().selected_entry = Some(e);
                    self.model.row_of(e)
                });
                match selected_row {
                    Some(r) => self.ui.invoke_select_row(r as i32),
                    None => self.ui.set_selected(-1),
                }
                if needs_resort || !filter.is_empty() {
                    self.recompute_view(needs_resort);
                }
                self.update_status();
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
                }
                self.model.clear();
                self.ui.set_loading(false);
                self.ui.set_error_text(error.user_message().into());
                self.ui.set_status(SharedString::new());
                self.mark_rows(true);
            }
        }
    }

    /// Relance le tri (si `resort`) et le filtre sur le pool de calcul.
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
        let (folder, selected) = {
            let mut st = self.st.borrow_mut();
            if let Some(order) = r.order {
                st.order = Arc::new(order);
                st.load_spec = r.spec;
            }
            (st.folder.clone(), st.selected_entry)
        };
        self.model.set(folder, r.visible);
        match selected.and_then(|e| self.model.row_of(e)) {
            Some(row) => self.ui.invoke_select_row(row as i32),
            None => self.ui.set_selected(-1),
        }
        self.update_status();
    }

    // ------------------------------------------------------------------ tri, filtre, sélection

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
            st.spec
        };
        self.ui.set_sort_column(column_index(spec.column));
        self.ui.set_sort_descending(spec.descending);
        // Si le chargement est en cours, son événement Complete relancera le tri.
        self.recompute_view(true);
    }

    fn filter_edited(self: &Rc<Self>, text: &str) {
        self.st.borrow_mut().filter = Filter::new(text);
        self.recompute_view(false);
    }

    fn selection_changed(&self, row: i32) {
        let entry = usize::try_from(row).ok().and_then(|r| self.model.entry_at(r));
        self.st.borrow_mut().selected_entry = entry;
        self.update_status();
    }

    fn update_status(&self) {
        let st = self.st.borrow();
        let Some(folder) = &st.folder else { return };
        let total = folder.len();
        let shown = self.model.len();
        let mut s = if st.filter.is_empty() || !st.complete {
            item_count(total)
        } else {
            format!(
                "{} sur {}",
                fx_core::format::group_thousands(shown as u64),
                item_count(total)
            )
        };
        if st.selected_entry.is_some() {
            s.push_str(" · 1 élément sélectionné");
        }
        if st.complete
            && let Some(ms) = self.timing.get().complete
        {
            match ms.as_millis() {
                0 => s.push_str(" · chargé en moins d'1 ms"),
                n => s.push_str(&format!(" · chargé en {n} ms")),
            }
        }
        self.ui.set_status(s.into());
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
        let mut next: Option<(PathBuf, bool)> = None;
        let mut finish = false;
        {
            let mut guard = self.bench.borrow_mut();
            let Some(b) = guard.as_mut() else { return };
            if b.initial_painted_ms.is_none() {
                b.initial_painted_ms = fx_win::process::ms_since_process_start();
                match &b.mode {
                    BenchMode::Startup { .. } => finish = true,
                    BenchMode::Open { path, .. } => {
                        b.on_target = true;
                        next = Some((path.clone(), true));
                    }
                }
            } else if let BenchMode::Open { path, iterations, .. } = b.mode.clone() {
                if b.on_target {
                    let t = self.timing.get();
                    b.entries = t.stats.entries;
                    b.results.push(t);
                    if b.iteration + 1 == iterations {
                        // Mémoire avec le gros dossier affiché, puis fin.
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
                        next = Some((b.start_path.clone(), false));
                    }
                } else {
                    b.iteration += 1;
                    b.on_target = true;
                    next = Some((path, false));
                }
            }
        }
        if let Some((path, _first)) = next {
            // Laisse l'interface au repos un instant entre deux mesures.
            slint::Timer::single_shot(Duration::from_millis(300), move || {
                with_app(|a| a.navigate(path, true, None));
            });
        }
        if finish {
            slint::Timer::single_shot(Duration::from_millis(1000), || with_app(|a| a.bench_report()));
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
            BenchMode::Startup { out } | BenchMode::Open { out, .. } => out.clone(),
        };
        if let Some(p) = target
            && let Err(e) = std::fs::write(&p, &out)
        {
            eprintln!("écriture de {} impossible : {e}", p.display());
        }
        let _ = slint::quit_event_loop();
    }
}
