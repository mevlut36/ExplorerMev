//! ExplorerMev — explorateur de fichiers pour Windows 11.

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
ExplorerMev — explorateur de fichiers

Usage : explorermev [dossier] [options]

Options :
  --renderer <software|gpu>   moteur de rendu (défaut : software)
  --no-mica                   fond opaque au lieu de Mica
  --show-hidden               affiche les éléments cachés
  --network-timeout <s>       délai d'abandon d'un dossier qui ne répond pas
  --bench-startup             mesure le démarrage, affiche les durées et quitte
  --bench-open <dossier>      mesure l'ouverture de <dossier> (cache chaud) et quitte
  --bench-scroll <dossier>    mesure le défilement dans <dossier> et quitte (--maximized : plein écran)
  --bench-search              mesure la recherche globale (frappe simulée, mémoire) et quitte
  --theme <réglages>          apparence pour cette session seulement (ex. mode=clair,accent=rose,teinte=1)
  --foreground                prend le premier plan au démarrage (utilisé par le raccourci Win+E)
  --icons                     ouvre les volets en vue icônes (réglage « view » de config.toml)
  --self-test <dossier vide>  scénario d'auto-test dans ce dossier (créé et modifié !), puis quitte
  --iterations <n>            nombre d'ouvertures pour --bench-open (défaut : 3)
  --out <fichier>             écrit aussi le rapport de mesure dans <fichier>
  --default-explorer <on|off|status>
                              ExplorerMev explorateur par défaut (Win+E et ouverture des dossiers), puis quitte
  --win-e <on|off|status>     seulement Win+E ouvre ExplorerMev (on) ou l'Explorateur (off), puis quitte
  -V, --version               affiche la version
  -h, --help                  affiche cette aide

Configuration : %APPDATA%\\ExplorerMev\\config.toml (les options ci-dessus sont prioritaires)
Journal : %LOCALAPPDATA%\\ExplorerMev\\logs (niveau : log_level, ou variable EXPLORERMEV_LOG)";

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
    foreground: bool,
    theme: Option<String>,
    iterations: usize,
    out: Option<PathBuf>,
    help: bool,
    version: bool,
    win_e: Option<String>,
    default_explorer: Option<String>,
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
        foreground: false,
        theme: None,
        iterations: 3,
        out: None,
        help: false,
        version: false,
        win_e: None,
        default_explorer: None,
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
            Some("--foreground") => a.foreground = true,
            Some("--theme") => {
                a.theme = Some(
                    it.next()
                        .context("--theme attend des réglages")?
                        .to_string_lossy()
                        .into_owned(),
                )
            }
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
            Some("-V" | "--version") => a.version = true,
            Some("--default-explorer") => {
                let v = it.next().context("--default-explorer attend on, off ou status")?;
                match v.to_str() {
                    Some(s @ ("on" | "off" | "status")) => a.default_explorer = Some(s.into()),
                    _ => bail!("--default-explorer attend on, off ou status"),
                }
            }
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

/// Agent résident : Win+E ouvre ExplorerMev (une seule instance, arrêt par `--default-explorer off`).
fn hotkey_agent() -> ExitCode {
    fn open_window() -> bool {
        // L'agent vient de recevoir la frappe : il cède le droit de passer au premier plan.
        fx_win::hotkey::allow_foreground();
        match fx_win::process::launch_path()
            .and_then(|exe| std::process::Command::new(exe).arg("--foreground").spawn())
        {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("Win+E : lancement impossible : {e}");
                false
            }
        }
    }
    match fx_win::hotkey::run_agent(open_window) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("agent de raccourci : {e}");
            ExitCode::FAILURE
        }
    }
}

/// Lance l'agent de raccourci (il tourne déjà : sans effet, l'instance unique est gardée).
fn start_hotkey_agent(exe: &std::path::Path) {
    if fx_win::hotkey::is_agent_running() {
        return;
    }
    if let Err(e) = std::process::Command::new(exe).arg("--hotkey-agent").spawn() {
        eprintln!("agent de raccourci non lancé : {e}");
    }
}

/// Dans un paquet MSIX qui isole le registre, les clés écrites ne seraient jamais vues par
/// l'Explorateur : on refuse plutôt que de réussir sans effet.
fn refuse_if_registry_virtualized() -> Option<ExitCode> {
    if !fx_win::process::registry_is_virtualized() {
        return None;
    }
    eprintln!(
        "Ce paquet isole le registre : installez la dernière version du paquet MSIX (script \
         packaging/package.ps1) ou utilisez la version portable."
    );
    Some(ExitCode::FAILURE)
}

/// `--default-explorer on|off|status` : Win+E et ouverture des dossiers (installation,
/// désinstallation, scripts).
fn default_explorer(action: &str) -> ExitCode {
    use fx_win::default_explorer::{DefaultExplorer, DefaultState};
    if let Some(code) = refuse_if_registry_virtualized() {
        return code;
    }
    let key = DefaultExplorer::default();
    let result: anyhow::Result<String> = (|| {
        Ok(match action {
            "on" => {
                let exe = fx_win::process::launch_path()?;
                key.enable(&exe)?;
                start_hotkey_agent(&exe);
                format!("ExplorerMev est l'explorateur par défaut : {}", exe.display())
            }
            "off" => {
                let agent = fx_win::hotkey::stop_agent();
                if key.disable()? || agent {
                    "L'Explorateur Windows est de nouveau l'explorateur par défaut.".into()
                } else {
                    "ExplorerMev n'était pas l'explorateur par défaut : rien à changer.".into()
                }
            }
            _ => match key.state()? {
                DefaultState::Windows => "Explorateur par défaut : Windows.".into(),
                DefaultState::ExplorerMev(p) => format!("Explorateur par défaut : ExplorerMev ({}).", p.display()),
                DefaultState::Partial => {
                    "Explorateur par défaut : partiellement ExplorerMev (relancer --default-explorer on ou off)."
                        .into()
                }
                DefaultState::Other(what) => format!("Explorateur par défaut : autre programme ({what})."),
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

/// `--win-e on|off|status` : option « Remplacer Win+E » sans interface (installation,
/// désinstallation, scripts).
fn win_e(action: &str) -> ExitCode {
    use fx_win::win_e::{WinEKey, WinEState};
    if let Some(code) = refuse_if_registry_virtualized() {
        return code;
    }
    let key = WinEKey::default();
    let result: anyhow::Result<String> = (|| {
        Ok(match action {
            "on" => {
                let exe = fx_win::process::launch_path()?;
                key.enable(&exe)?;
                format!("Win+E ouvre désormais {}.", exe.display())
            }
            "off" => {
                if key.disable()? {
                    "Win+E ouvre de nouveau l'Explorateur.".into()
                } else {
                    "Win+E n'était pas associé à ExplorerMev : rien à changer.".into()
                }
            }
            _ => match key.state()? {
                WinEState::Default => "Win+E : Explorateur (comportement de Windows).".into(),
                WinEState::ExplorerMev(p) => format!("Win+E : ExplorerMev ({}).", p.display()),
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
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ExplorerMev").join("logs"))
}

fn init_logging(configured: &str) -> Option<WorkerGuard> {
    // La variable EXPLORERMEV_LOG l'emporte sur la configuration.
    let env = std::env::var("EXPLORERMEV_LOG").ok();
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
        .filename_prefix("explorermev")
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
                "ExplorerMev",
                &format!(
                    "ExplorerMev a rencontré une erreur interne et doit se fermer.\n\nDétails dans le journal :\n{logs}"
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
    // Agent de raccourci Win+E : aucun autre traitement (ni configuration, ni interface).
    if raw.first().map(String::as_str) == Some("--hotkey-agent") {
        return hotkey_agent();
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
    if args.help || args.version || benching || args.win_e.is_some() || args.default_explorer.is_some() {
        // Sous-système GUI : on se rattache à la console du parent pour y écrire.
        fx_win::process::attach_parent_console();
    }
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("ExplorerMev {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if let Some(action) = &args.win_e {
        return win_e(action);
    }
    if let Some(action) = &args.default_explorer {
        return default_explorer(action);
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
        // Sans dossier demandé ni configuré, la page d'accueil (les mesures gardent le profil).
        .or_else(|| (!benching).then(fx_ui::home_path))
        .or_else(|| fx_win::shell::known_folder(fx_win::shell::KnownFolder::Profile))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(r"C:\"));
    // Chemin absolu et normalisé (barres obliques, `..`) : le Shell n'accepte que cette forme.
    let start = if start == fx_ui::home_path() {
        start
    } else {
        std::path::absolute(&start).unwrap_or(start)
    };
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
        foreground: args.foreground,
        theme_override: args.theme.clone(),
        network_timeout: args
            .network_timeout
            .map(std::time::Duration::from_secs)
            .unwrap_or(cfg.network_timeout),
        startup_warning: warnings.first().cloned(),
        // Sans dossier explicite, on reprend les onglets de la session précédente.
        // Reprise de session seulement si le réglage `startup = "session"` la demande.
        restore_session: !benching && explicit_path.is_none() && cfg.restore_session,
        data_dir: std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("ExplorerMev")),
        // Les modes de mesure ont leur propre cache (reproductibilité, pas de mélange).
        index_dir: std::env::var_os("LOCALAPPDATA").map(|d| {
            PathBuf::from(d)
                .join("ExplorerMev")
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
