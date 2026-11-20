//! WinDir — explorateur de fichiers pour Windows 11.

#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, bail};
use fx_ui::{BenchMode, Options};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;

mod config;
mod helper;

const USAGE: &str = "\
WinDir — explorateur de fichiers

Usage : windir [dossier] [options]

Options :
  --renderer <software|gpu>   moteur de rendu (défaut : software)
  --no-mica                   fond opaque au lieu de Mica
  --show-hidden               affiche les éléments cachés
  --network-timeout <s>       délai d'abandon d'un dossier qui ne répond pas
  --bench-startup             mesure le démarrage, affiche les durées et quitte
  --bench-open <dossier>      mesure l'ouverture de <dossier> (cache chaud) et quitte
  --bench-scroll <dossier>    mesure le défilement dans <dossier> et quitte (--maximized : plein écran)
  --bench-search              mesure la recherche globale (frappe simulée, mémoire) et quitte
  --icons                     ouvre les volets en vue icônes (réglage « view » de config.toml)
  --self-test <dossier vide>  scénario d'auto-test dans ce dossier (créé et modifié !), puis quitte
  --iterations <n>            nombre d'ouvertures pour --bench-open (défaut : 3)
  --out <fichier>             écrit aussi le rapport de mesure dans <fichier>
  --win-e <on|off|status>     Win+E ouvre WinDir (on) ou l'Explorateur (off), puis quitte
  -h, --help                  affiche cette aide

Configuration : %APPDATA%\\WinDir\\config.toml (les options ci-dessus sont prioritaires)
Journal : %LOCALAPPDATA%\\WinDir\\logs (niveau : log_level, ou variable WINDIR_LOG)";

struct Args {
    path: Option<PathBuf>,
    renderer: Option<String>,
    mica: Option<bool>,
    show_hidden: Option<bool>,
    network_timeout: Option<u64>,
    bench_startup: bool,
    bench_open: Option<PathBuf>,
    self_test: Option<PathBuf>,
    bench_scroll: Option<PathBuf>,
    bench_search: bool,
    maximized: bool,
    icons: bool,
    iterations: usize,
    out: Option<PathBuf>,
    help: bool,
    win_e: Option<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut a = Args {
        path: None,
        renderer: None,
        mica: None,
        show_hidden: None,
        network_timeout: None,
        bench_startup: false,
        bench_open: None,
        self_test: None,
        bench_scroll: None,
        bench_search: false,
        maximized: false,
        icons: false,
        iterations: 3,
        out: None,
        help: false,
        win_e: None,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--renderer") => {
                let r = it.next().context("--renderer attend une valeur")?;
                a.renderer = Some(match r.to_str() {
                    Some("software" | "sw") => "software".into(),
                    Some("gpu" | "femtovg") => "gpu".into(),
                    _ => bail!("moteur de rendu inconnu : {}", r.to_string_lossy()),
                });
            }
            Some("--no-mica") => a.mica = Some(false),
            Some("--show-hidden") => a.show_hidden = Some(true),
            Some("--network-timeout") => {
                a.network_timeout = Some(
                    it.next()
                        .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
                        .filter(|&n: &u64| (1..=600).contains(&n))
                        .context("--network-timeout attend un nombre de secondes (1 à 600)")?,
                );
            }
            Some("--bench-startup") => a.bench_startup = true,
            Some("--bench-open") => {
                a.bench_open = Some(it.next().context("--bench-open attend un dossier")?.into())
            }
            Some("--bench-search") => a.bench_search = true,
            Some("--bench-scroll") => {
                a.bench_scroll = Some(it.next().context("--bench-scroll attend un dossier")?.into())
            }
            Some("--maximized") => a.maximized = true,
            Some("--icons") => a.icons = true,
            Some("--self-test") => {
                a.self_test = Some(it.next().context("--self-test attend un dossier")?.into())
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
            Some("--win-e") => {
                let v = it.next().context("--win-e attend on, off ou status")?;
                match v.to_str() {
                    Some(s @ ("on" | "off" | "status")) => a.win_e = Some(s.into()),
                    _ => bail!("--win-e attend on, off ou status"),
                }
            }
            Some(s) if s.starts_with("--") => bail!("option inconnue : {s}"),
            _ => a.path = Some(PathBuf::from(arg)),
        }
    }
    Ok(a)
}

/// `--win-e on|off|status` : option « Remplacer Win+E » sans interface (installation,
/// désinstallation, scripts).
fn win_e(action: &str) -> ExitCode {
    use fx_win::win_e::{WinEKey, WinEState};
    let key = WinEKey::default();
    let result: anyhow::Result<String> = (|| {
        Ok(match action {
            "on" => {
                let exe = std::env::current_exe()?;
                key.enable(&exe)?;
                format!("Win+E ouvre désormais {}.", exe.display())
            }
            "off" => {
                if key.disable()? {
                    "Win+E ouvre de nouveau l'Explorateur.".into()
                } else {
                    "Win+E n'était pas associé à WinDir : rien à changer.".into()
                }
            }
            _ => match key.state()? {
                WinEState::Default => "Win+E : Explorateur (comportement de Windows).".into(),
                WinEState::WinDir(p) => format!("Win+E : WinDir ({}).", p.display()),
                WinEState::Other(c) => format!("Win+E : autre programme ({c})."),
            },
        })
    })();
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("WinDir").join("logs"))
}

fn init_logging(configured: &str) -> Option<WorkerGuard> {
    // La variable WINDIR_LOG l'emporte sur la configuration.
    let env = std::env::var("WINDIR_LOG").ok();
    let level = match env.as_deref().unwrap_or(configured) {
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        "off" => return None,
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
        // Un panic des threads de travail est isolé (requête en échec) ; celui du thread
        // d'interface est fatal : on prévient l'utilisateur au lieu de disparaître sans un mot.
        if thread.name() == Some("main") {
            let logs = log_dir().map(|d| d.display().to_string()).unwrap_or_default();
            fx_win::shell::error_box(
                "WinDir",
                &format!(
                    "WinDir a rencontré une erreur interne et doit se fermer.\n\nDétails dans le journal :\n{logs}"
                ),
            );
        }
    }));
}

fn main() -> ExitCode {
    // Assistant d'indexation élevé : aucun autre traitement (ni configuration, ni interface).
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("--index-helper") {
        return helper::run(&raw[1..]);
    }
    let main_start = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            fx_win::process::attach_parent_console();
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let benching = args.bench_startup
        || args.bench_open.is_some()
        || args.self_test.is_some()
        || args.bench_scroll.is_some()
        || args.bench_search;
    if args.help || benching || args.win_e.is_some() {
        // Sous-système GUI : on se rattache à la console du parent pour y écrire.
        fx_win::process::attach_parent_console();
    }
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if let Some(action) = &args.win_e {
        return win_e(action);
    }
    // Les mesures et l'auto-test ignorent la configuration de l'utilisateur (reproductibilité).
    let (cfg, warnings) = match (benching, config::default_path()) {
        (false, Some(p)) => config::load(&p),
        _ => (config::Config::default(), Vec::new()),
    };
    let _log_guard = init_logging(&cfg.log_level);
    install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "démarrage");
    for w in &warnings {
        tracing::warn!("{w}");
    }

    if let Some(dir) = &args.self_test {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("dossier d'auto-test {} : {e}", dir.display());
            return ExitCode::FAILURE;
        }
        if std::fs::read_dir(dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
        {
            eprintln!("le dossier d'auto-test doit être vide : {}", dir.display());
            return ExitCode::FAILURE;
        }
    }
    let explicit_path = args.path.clone();
    let start = args
        .self_test
        .clone()
        .or(args.bench_scroll.clone())
        .or(args.path)
        .or(cfg.start_folder.clone())
        .or_else(|| fx_win::shell::known_folder(fx_win::shell::KnownFolder::Profile))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(r"C:\"));
    // Chemin absolu et normalisé (barres obliques, `..`) : le Shell n'accepte que cette forme.
    let start = std::path::absolute(&start).unwrap_or(start);
    let bench = if args.bench_scroll.is_some() {
        Some(BenchMode::Scroll {
            maximized: args.maximized,
            out: args.out.clone(),
        })
    } else if args.self_test.is_some() {
        Some(BenchMode::SelfTest { out: args.out })
    } else if args.bench_search {
        Some(BenchMode::Search { out: args.out })
    } else if let Some(path) = args.bench_open {
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

    let renderer = match args.renderer.unwrap_or(cfg.renderer).as_str() {
        "gpu" => "femtovg".to_string(),
        _ => "software".to_string(),
    };
    let options = Options {
        start,
        renderer,
        mica: args.mica.unwrap_or(cfg.mica),
        show_hidden: args.show_hidden.unwrap_or(cfg.show_hidden),
        icons_view: args.icons || cfg.icons_view,
        index_mft: cfg.index_mft,
        network_timeout: args
            .network_timeout
            .map(std::time::Duration::from_secs)
            .unwrap_or(cfg.network_timeout),
        startup_warning: warnings.first().cloned(),
        // Sans dossier explicite, on reprend les onglets de la session précédente.
        restore_session: !benching && explicit_path.is_none(),
        data_dir: std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("WinDir")),
        // Les modes de mesure ont leur propre cache (reproductibilité, pas de mélange).
        index_dir: std::env::var_os("LOCALAPPDATA").map(|d| {
            PathBuf::from(d)
                .join("WinDir")
                .join(if benching { "index-mesure" } else { "index" })
        }),
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
