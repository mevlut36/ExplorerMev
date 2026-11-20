//! Prototype jetable de la phase 0.
//!
//! Vérifie que Slint (style fluent) peut :
//! - afficher un fond Mica Windows 11 ;
//! - faire défiler une vue détails de 100 000 lignes à 60 images/s au moins.
//!
//! Usage :
//!   slint-scroll [--renderer femtovg|software|skia] [--view custom|table]
//!                [--rows N] [--no-mica] [--bench] [--out fichier.txt]
//!
//! En mode `--bench`, la liste défile toute seule selon trois scénarios, chaque image est
//! horodatée via le rendering notifier de Slint, puis un rapport est affiché et le programme quitte.

slint::include_modules!();

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Model, ModelNotify, ModelRc, ModelTracker, SharedString, StandardListViewItem, VecModel};

mod win;

// ---------------------------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Args {
    renderer: String,
    view: String,
    rows: usize,
    mica: bool,
    maximized: bool,
    bench: bool,
    out: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        renderer: "femtovg".into(),
        view: "custom".into(),
        rows: 100_000,
        mica: true,
        maximized: false,
        bench: false,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--renderer" => a.renderer = it.next().unwrap_or(a.renderer),
            "--view" => a.view = it.next().unwrap_or(a.view),
            "--rows" => a.rows = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.rows),
            "--no-mica" => a.mica = false,
            "--maximized" => a.maximized = true,
            "--bench" => a.bench = true,
            "--out" => a.out = it.next(),
            other => eprintln!("argument ignoré : {other}"),
        }
    }
    a
}

// ---------------------------------------------------------------------------------------------
// Modèles paresseux : aucune ligne n'est matérialisée tant que Slint ne la demande pas.
// ---------------------------------------------------------------------------------------------

const EXTS: [(&str, &str); 8] = [
    ("txt", "Document texte"),
    ("rs", "Fichier source Rust"),
    ("png", "Fichier PNG"),
    ("pdf", "Document PDF"),
    ("docx", "Document Microsoft Word"),
    ("zip", "Dossier compressé"),
    ("exe", "Application"),
    ("json", "Fichier source JSON"),
];

fn fake_row(i: usize, n: usize) -> Row {
    let dirs = n / 50;
    // Date déterministe dérivée de l'index.
    let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let day = 1 + (h % 28);
    let month = 1 + ((h >> 8) % 12);
    let year = 2018 + ((h >> 16) % 8);
    let hour = (h >> 24) % 24;
    let min = (h >> 32) % 60;
    let modified = SharedString::from(format!("{day:02}/{month:02}/{year} {hour:02}:{min:02}"));
    if i < dirs {
        Row {
            name: format!("Dossier {i:05}").into(),
            modified,
            kind: "Dossier de fichiers".into(),
            size: SharedString::new(),
            is_dir: true,
        }
    } else {
        let (ext, kind) = EXTS[(h >> 40) as usize % EXTS.len()];
        let kib = 1 + (h >> 44) % 900_000;
        Row {
            name: format!("Fichier de test numéro {i:06}.{ext}").into(),
            modified,
            kind: kind.into(),
            size: format!("{} Ko", group_thousands(kib)).into(),
            is_dir: false,
        }
    }
}

fn group_thousands(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push('\u{202F}');
        }
        out.push(c);
    }
    out
}

struct FakeFolder {
    n: usize,
    calls: Rc<Cell<u64>>,
    notify: ModelNotify,
}

impl Model for FakeFolder {
    type Data = Row;
    fn row_count(&self) -> usize {
        self.n
    }
    fn row_data(&self, row: usize) -> Option<Row> {
        self.calls.set(self.calls.get() + 1);
        (row < self.n).then(|| fake_row(row, self.n))
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

struct FakeTable {
    n: usize,
    calls: Rc<Cell<u64>>,
    notify: ModelNotify,
}

impl Model for FakeTable {
    type Data = ModelRc<StandardListViewItem>;
    fn row_count(&self) -> usize {
        self.n
    }
    fn row_data(&self, row: usize) -> Option<Self::Data> {
        self.calls.set(self.calls.get() + 1);
        (row < self.n).then(|| {
            let r = fake_row(row, self.n);
            let cells: Vec<StandardListViewItem> = [r.name, r.modified, r.kind, r.size]
                .into_iter()
                .map(StandardListViewItem::from)
                .collect();
            ModelRc::new(VecModel::from(cells))
        })
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

// ---------------------------------------------------------------------------------------------
// Abstraction minimale sur les deux fenêtres comparées.
// ---------------------------------------------------------------------------------------------

trait BenchUi {
    fn window(&self) -> &slint::Window;
    fn set_scroll(&self, y: f32);
    fn max_scroll(&self) -> f32;
    fn set_stats(&self, s: SharedString);
    fn show(&self) -> Result<(), slint::PlatformError>;
}

macro_rules! impl_bench_ui {
    ($t:ty) => {
        impl BenchUi for $t {
            fn window(&self) -> &slint::Window {
                slint::ComponentHandle::window(self)
            }
            fn set_scroll(&self, y: f32) {
                self.set_scroll_y(-y);
            }
            fn max_scroll(&self) -> f32 {
                self.get_max_scroll()
            }
            fn set_stats(&self, s: SharedString) {
                <$t>::set_stats(self, s);
            }
            fn show(&self) -> Result<(), slint::PlatformError> {
                slint::ComponentHandle::show(self)
            }
        }
    };
}
impl_bench_ui!(CustomViewWindow);
impl_bench_ui!(TableViewWindow);

// ---------------------------------------------------------------------------------------------
// Mesure des images
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct FrameLog {
    before: Option<Instant>,
    /// (fin du rendu, durée CPU du rendu en ms)
    frames: Vec<(Instant, f64)>,
    first_frame_ms_since_process_start: Option<f64>,
    first_frame_ms_since_main: Option<f64>,
    /// Borne haute : horodatée par un timer à délai nul, exécuté après le retour du rendu.
    first_frame_done_ms_since_process_start: Option<f64>,
}

struct Phase {
    name: &'static str,
    start: Instant,
    end: Instant,
    cpu_start_ms: f64,
    cpu_end_ms: f64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn phase_report(log: &FrameLog, ph: &Phase) -> String {
    let frames: Vec<&(Instant, f64)> = log
        .frames
        .iter()
        .filter(|(t, _)| *t >= ph.start && *t <= ph.end)
        .collect();
    let mut intervals: Vec<f64> = frames
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).as_secs_f64() * 1000.0)
        .collect();
    let mut render: Vec<f64> = frames.iter().map(|(_, r)| *r).collect();
    intervals.sort_by(f64::total_cmp);
    render.sort_by(f64::total_cmp);
    let dur = (ph.end - ph.start).as_secs_f64();
    let over16 = intervals.iter().filter(|&&v| v > 16.7).count();
    let over33 = intervals.iter().filter(|&&v| v > 33.4).count();
    let cpu = (ph.cpu_end_ms - ph.cpu_start_ms) / (dur * 1000.0) * 100.0;
    format!(
        "{:<8} CPU={:>5.1}% d'un cœur | images={:>5} i/s={:>6.1} | intervalle ms p50={:>5.2} p99={:>5.2} max={:>6.2} >16,7ms={} >33ms={} | rendu CPU ms p50={:>5.2} p99={:>5.2} max={:>6.2}",
        ph.name,
        cpu,
        frames.len(),
        frames.len() as f64 / dur,
        percentile(&intervals, 0.5),
        percentile(&intervals, 0.99),
        intervals.last().copied().unwrap_or(f64::NAN),
        over16,
        over33,
        percentile(&render, 0.5),
        percentile(&render, 0.99),
        render.last().copied().unwrap_or(f64::NAN),
    )
}

// ---------------------------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let main_start = Instant::now();
    let args = parse_args();

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(args.renderer.clone())
        .select()?;

    let calls = Rc::new(Cell::new(0u64));
    let title: SharedString = format!("Dossier factice — {} éléments", args.rows).into();

    let ui: Rc<dyn BenchUi> = match args.view.as_str() {
        "table" => {
            let w = TableViewWindow::new()?;
            w.set_mica(args.mica);
            w.set_folder_title(title);
            w.set_rows(ModelRc::new(FakeTable {
                n: args.rows,
                calls: calls.clone(),
                notify: ModelNotify::default(),
            }));
            Rc::new(w)
        }
        _ => {
            let w = CustomViewWindow::new()?;
            w.set_mica(args.mica);
            w.set_folder_title(title);
            w.set_rows(ModelRc::new(FakeFolder {
                n: args.rows,
                calls: calls.clone(),
                notify: ModelNotify::default(),
            }));
            Rc::new(w)
        }
    };

    let log = Rc::new(RefCell::new(FrameLog::default()));
    let mica_status = Rc::new(RefCell::new(String::from("non appliqué")));
    let record_frame = {
        let log = log.clone();
        move |render_ms: f64| {
            let now = Instant::now();
            let mut l = log.borrow_mut();
            if l.first_frame_ms_since_main.is_none() {
                l.first_frame_ms_since_main = Some((now - main_start).as_secs_f64() * 1000.0);
                l.first_frame_ms_since_process_start = win::ms_since_process_start();
                let log = log.clone();
                slint::Timer::single_shot(Duration::ZERO, move || {
                    log.borrow_mut().first_frame_done_ms_since_process_start = win::ms_since_process_start();
                });
            }
            l.frames.push((now, render_ms));
        }
    };
    // Le rendering notifier n'existe pas pour tous les moteurs (le moteur logiciel renvoie
    // `Unsupported`). Dans ce cas on horodate l'événement winit RedrawRequested, émis juste
    // avant le rendu : on obtient la cadence, pas la durée de rendu.
    let notifier_ok = {
        let log = log.clone();
        let record_frame = record_frame.clone();
        ui.window()
            .set_rendering_notifier(move |state, _api| match state {
                slint::RenderingState::BeforeRendering => {
                    log.borrow_mut().before = Some(Instant::now());
                }
                slint::RenderingState::AfterRendering => {
                    let before = log.borrow().before;
                    record_frame(
                        before
                            .map(|b| b.elapsed().as_secs_f64() * 1000.0)
                            .unwrap_or(f64::NAN),
                    );
                }
                _ => {}
            })
            .is_ok()
    };
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        let mica = args.mica;
        let mica_status = mica_status.clone();
        let mut mica_done = false;
        let mut maximize = args.maximized;
        let extend_frame = std::env::var_os("PROTO_NO_EXTEND_FRAME").is_none();
        ui.window().on_winit_window_event(move |w, ev| {
            // `slint::Window::set_maximized` avant `show()` est ignoré : on passe par winit.
            if maximize {
                maximize = false;
                w.with_winit_window(|ww| ww.set_maximized(true));
            }
            // Premier événement reçu : la fenêtre native existe, on applique Mica avant le
            // premier rendu.
            if mica && !mica_done {
                mica_done = true;
                *mica_status.borrow_mut() = match win::apply_mica(w, extend_frame) {
                    Ok(()) => "appliqué (DWMSBT_MAINWINDOW)".into(),
                    Err(e) => format!("échec : {e}"),
                };
            }
            if !notifier_ok && matches!(ev, WindowEvent::RedrawRequested) {
                record_frame(f64::NAN);
            }
            EventResult::Propagate
        });
    }

    // Statistiques en direct dans la barre du haut (mode interactif).
    let stats_timer = slint::Timer::default();
    if !args.bench {
        let log = log.clone();
        let ui2 = ui.clone();
        let calls = calls.clone();
        stats_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
            let l = log.borrow();
            let now = Instant::now();
            let n = l
                .frames
                .iter()
                .rev()
                .take_while(|(t, _)| now - *t < Duration::from_secs(1))
                .count();
            let mem = win::memory()
                .map(|m| format!("{:.1} Mo privés", m.private_mb))
                .unwrap_or_default();
            ui2.set_stats(format!("{n} images/s · {} lignes construites · {mem}", calls.get()).into());
        });
    }

    let bench_timer = slint::Timer::default();
    if args.bench {
        run_bench(&ui, &log, &calls, &mica_status, &args, &bench_timer);
    }

    ui.show()?;
    slint::run_event_loop()?;
    Ok(())
}

fn run_bench(
    ui: &Rc<dyn BenchUi>,
    log: &Rc<RefCell<FrameLog>>,
    calls: &Rc<Cell<u64>>,
    mica_status: &Rc<RefCell<String>>,
    args: &Args,
    driver: &slint::Timer,
) {
    // Scénarios : (nom, durée, vitesse en px/s ; 0 = sauts aléatoires à chaque tick)
    const SCENARIOS: [(&str, f64, f64); 3] = [
        ("molette", 4.0, 1_500.0),
        ("rapide", 4.0, 15_000.0),
        ("sauts", 4.0, 0.0),
    ];
    const WARMUP: f64 = 1.0;

    let started = Instant::now();
    let phases: Rc<RefCell<Vec<Phase>>> = Rc::default();
    let idle_mem = Rc::new(Cell::new(None::<win::Mem>));
    let rng = Cell::new(0x2545_F491_4F6C_DD1Du64);
    let ui = ui.clone();
    let log = log.clone();
    let calls = calls.clone();
    let mica_status = mica_status.clone();
    let args = args.clone();

    driver.start(slint::TimerMode::Repeated, Duration::from_millis(2), move || {
        let t = started.elapsed().as_secs_f64();
        if t < WARMUP {
            return;
        }
        if idle_mem.get().is_none() {
            idle_mem.set(win::memory());
        }
        let mut acc = WARMUP;
        for (name, dur, speed) in SCENARIOS {
            if t < acc + dur {
                let local = t - acc;
                {
                    let mut p = phases.borrow_mut();
                    if p.last().is_none_or(|ph| ph.name != name) {
                        let now = Instant::now();
                        let cpu = win::process_cpu_ms().unwrap_or(f64::NAN);
                        p.push(Phase { name, start: now, end: now, cpu_start_ms: cpu, cpu_end_ms: cpu });
                    }
                    if let Some(ph) = p.last_mut() {
                        ph.end = Instant::now();
                        ph.cpu_end_ms = win::process_cpu_ms().unwrap_or(f64::NAN);
                    }
                }
                let max = ui.max_scroll() as f64;
                let y = if speed > 0.0 {
                    (local * speed) % max.max(1.0)
                } else {
                    // xorshift64
                    let mut x = rng.get();
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    rng.set(x);
                    (x % 1_000_000) as f64 / 1_000_000.0 * max
                };
                ui.set_scroll(y as f32);
                return;
            }
            acc += dur;
        }
        // Fin : rapport.
        let l = log.borrow();
        let mut out = String::new();
        let refresh = win::refresh_rate_hz(ui.window());
        out += &format!(
            "renderer={} vue={} lignes={} mica={} [{}] taille={}x{} px échelle={} rafraîchissement écran={}\n",
            args.renderer,
            args.view,
            args.rows,
            args.mica,
            mica_status.borrow(),
            ui.window().size().width,
            ui.window().size().height,
            ui.window().scale_factor(),
            refresh.map(|r| format!("{r:.0} Hz")).unwrap_or_else(|| "?".into()),
        );
        out += &format!(
            "première image : {:.1} ms depuis la création du processus, {:.1} ms depuis main(), rendu terminé ≤ {:.1} ms\n",
            l.first_frame_ms_since_process_start.unwrap_or(f64::NAN),
            l.first_frame_ms_since_main.unwrap_or(f64::NAN),
            l.first_frame_done_ms_since_process_start.unwrap_or(f64::NAN),
        );
        if let Some(m) = idle_mem.get() {
            out += &format!("mémoire au repos : working set {:.1} Mo, privée {:.1} Mo\n", m.working_set_mb, m.private_mb);
        }
        if let Some(m) = win::memory() {
            out += &format!(
                "mémoire après défilement : working set {:.1} Mo, privée {:.1} Mo\n",
                m.working_set_mb, m.private_mb
            );
        }
        out += &format!("appels row_data : {}\n", calls.get());
        for ph in phases.borrow().iter() {
            out += &phase_report(&l, ph);
            out.push('\n');
        }
        print!("{out}");
        if let Some(path) = &args.out
            && let Err(e) = std::fs::write(path, &out)
        {
            eprintln!("écriture de {path} impossible : {e}");
        }
        let _ = slint::quit_event_loop();
    });
}
