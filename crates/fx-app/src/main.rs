//! WinDir — explorateur de fichiers pour Windows 11.

#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, bail};
use fx_ui::{BenchMode, Options};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;

const USAGE: &str = "\
WinDir — explorateur de fichiers

Usage : windir [dossier] [options]

Options :
  --renderer <software|gpu>   moteur de rendu (défaut : software)
  --no-mica                   fond opaque au lieu de Mica
  --bench-startup             mesure le démarrage, affiche les durées et quitte
  --bench-open <dossier>      mesure l'ouverture de <dossier> (cache chaud) et quitte
  --iterations <n>            nombre d'ouvertures pour --bench-open (défaut : 3)
  --out <fichier>             écrit aussi le rapport de mesure dans <fichier>
  -h, --help                  affiche cette aide

Journal : %LOCALAPPDATA%\\WinDir\\logs (niveau : variable WINDIR_LOG = error|warn|info|debug|trace)";

struct Args {
    path: Option<PathBuf>,
    renderer: String,
    mica: bool,
    bench_startup: bool,
    bench_open: Option<PathBuf>,
    iterations: usize,
    out: Option<PathBuf>,
    help: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut a = Args {
        path: None,
        renderer: "software".into(),
        mica: true,
        bench_startup: false,
        bench_open: None,
        iterations: 3,
        out: None,
        help: false,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--renderer") => {
                let r = it.next().context("--renderer attend une valeur")?;
                a.renderer = match r.to_str() {
                    Some("software" | "sw") => "software".into(),
                    Some("gpu" | "femtovg") => "femtovg".into(),
                    _ => bail!("moteur de rendu inconnu : {}", r.to_string_lossy()),
                };
            }
            Some("--no-mica") => a.mica = false,
            Some("--bench-startup") => a.bench_startup = true,
            Some("--bench-open") => {
                a.bench_open = Some(it.next().context("--bench-open attend un dossier")?.into())
            }
            Some("--iterations") => {
                a.iterations = it
                    .next()
                    .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
                    .filter(|&n| n > 0)
                    .context("--iterations attend un entier positif")?;
            }
            Some("--out") => a.out = Some(it.next().context("--out attend un fichier")?.into()),
            Some("-h" | "--help" | "/?") => a.help = true,
            Some(s) if s.starts_with("--") => bail!("option inconnue : {s}"),
            _ => a.path = Some(PathBuf::from(arg)),
        }
    }
    Ok(a)
}

fn log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("WinDir").join("logs"))
}

fn init_logging() -> Option<WorkerGuard> {
    let level = match std::env::var("WINDIR_LOG").as_deref() {
        Ok("error") => LevelFilter::ERROR,
        Ok("warn") => LevelFilter::WARN,
        Ok("debug") => LevelFilter::DEBUG,
        Ok("trace") => LevelFilter::TRACE,
        Ok("off") => return None,
        _ => LevelFilter::INFO,
    };
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("windir")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(level)
        .with_target(false)
        .init();
    Some(guard)
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(thread = thread.name().unwrap_or("?"), "panic : {info}");
        default(info);
    }));
}

fn main() -> ExitCode {
    let main_start = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            fx_win::process::attach_parent_console();
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let benching = args.bench_startup || args.bench_open.is_some();
    if args.help || benching {
        // Sous-système GUI : on se rattache à la console du parent pour y écrire.
        fx_win::process::attach_parent_console();
    }
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let _log_guard = init_logging();
    install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "démarrage");

    let start = args
        .path
        .or_else(|| fx_win::shell::known_folder(fx_win::shell::KnownFolder::Profile))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(r"C:\"));
    let bench = if let Some(path) = args.bench_open {
        Some(BenchMode::Open {
            path,
            iterations: args.iterations,
            out: args.out,
        })
    } else if args.bench_startup {
        Some(BenchMode::Startup { out: args.out })
    } else {
        None
    };

    let options = Options {
        start,
        renderer: args.renderer,
        mica: args.mica,
        bench,
        main_start,
    };
    match fx_ui::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
