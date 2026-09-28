mod git;
mod server;
#[cfg(feature = "desktop")]
mod ui;
mod krust;
mod folio;
mod shared_config;
pub mod actions;
#[cfg(test)]
mod test_support;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const ABOUT: &str = env!("CARGO_PKG_DESCRIPTION");

/// Command-line options.
#[derive(Debug, PartialEq)]
struct Cli {
    /// Run the headless web daemon without the desktop GUI.
    headless: bool,
    /// Port for the embedded web daemon.
    port: u16,
    /// Repository path to open.
    path: Option<PathBuf>,
}

/// What a parse produced: run with these options, or print text and exit 0.
enum Parsed {
    Run(Cli),
    Print(String),
}

fn help_text() -> String {
    format!(
        "grit {}\n{}\n\n\
         Usage: grit [OPTIONS]\n\n\
         Options:\n\
         \x20     --headless        Run the headless web daemon without the desktop GUI\n\
         \x20     --port <PORT>     Port for the embedded web daemon [default: 5000]\n\
         \x20     --path <PATH>     Repository path to open\n\
         \x20 -h, --help            Print help\n\
         \x20 -V, --version         Print version\n",
        env!("CARGO_PKG_VERSION"),
        ABOUT,
    )
}

impl Cli {
    /// Parses `argv`-style arguments, where index 0 is the program name.
    /// `Err` carries a message to print after an `error: ` prefix.
    fn parse_from<I, S>(args: I) -> Result<Parsed, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let argv: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();
        let mut cli = Cli {
            headless: false,
            port: 5000,
            path: None,
        };
        let mut i = 1; // skip the program name
        while i < argv.len() {
            let arg = argv[i].clone();
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            };
            match name.as_str() {
                "-h" | "--help" => return Ok(Parsed::Print(help_text())),
                "-V" | "--version" => {
                    return Ok(Parsed::Print(format!(
                        "grit {}\n",
                        env!("CARGO_PKG_VERSION")
                    )))
                }
                "--headless" => {
                    if let Some(value) = inline {
                        return Err(format!("unexpected value '{value}' for '--headless'"));
                    }
                    cli.headless = true;
                }
                "--port" | "--path" => {
                    let value = match inline {
                        Some(v) => v,
                        None => {
                            i += 1;
                            argv.get(i).cloned().ok_or_else(|| {
                                format!("a value is required for '{name}' but none was supplied")
                            })?
                        }
                    };
                    if name == "--port" {
                        cli.port = value
                            .parse()
                            .map_err(|e| format!("invalid value '{value}' for '--port': {e}"))?;
                    } else {
                        cli.path = Some(PathBuf::from(value));
                    }
                }
                _ => return Err(format!("unexpected argument '{arg}' found")),
            }
            i += 1;
        }
        Ok(Parsed::Run(cli))
    }
}

fn main() -> ExitCode {
    let cli = match Cli::parse_from(std::env::args()) {
        Ok(Parsed::Run(cli)) => cli,
        Ok(Parsed::Print(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("error: {message}\n\nUsage: grit [OPTIONS]\n\nFor more information, try '--help'.");
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt::init();

    let repo_path = resolve_path(cli.path.as_deref().unwrap_or(Path::new(".")));

    #[cfg(not(feature = "desktop"))]
    {
        // Web-only build: everything runs headless.
        serve_headless(&cli, repo_path);
        ExitCode::SUCCESS
    }

    #[cfg(feature = "desktop")]
    {
        let open_explicit = cli.path.is_some();

        let mut headless = cli.headless;
        if !headless && !display_available() {
            headless = true;
            eprintln!(
                "No display server detected (DISPLAY/WAYLAND_DISPLAY unset); \
                 serving the web UI at http://127.0.0.1:{} instead",
                cli.port
            );
        }

        if headless {
            serve_headless(&cli, repo_path);
            return ExitCode::SUCCESS;
        }

        let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
        let daemon_found =
            runtime.block_on(server::is_daemon_running(cli.port));
        let mode = match choose_gui(daemon_found, cli.port) {
            ui::state::GuiMode::Embedded(registry) => {
                runtime.spawn(server::run(registry.clone(), cli.port));
                ui::state::GuiMode::Embedded(registry)
            }
            remote => remote,
        };
        ui::state::run(mode, repo_path, open_explicit).expect("iced application error");
        ExitCode::SUCCESS
    }
}

/// Builds the headless registry: an explicit `--path` pins one tab; otherwise
/// start empty so persisted tabs are restored by boot() instead of being
/// shadowed by a CWD tab.
fn headless_registry(cli: &Cli, repo_path: PathBuf) -> server::registry::TabRegistry {
    match cli.path {
        Some(ref path) => {
            if !repo_path.is_dir() || !repo_path.join(".git").exists() {
                eprintln!("error: --path {} is not a git repository", path.display());
                std::process::exit(2);
            }
            server::registry::TabRegistry::with_single_tab(
                0,
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                repo_path,
            )
        }
        None => server::registry::TabRegistry::new(),
    }
}

fn serve_headless(cli: &Cli, repo_path: PathBuf) {
    let registry = headless_registry(cli, repo_path);
    let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
    runtime.block_on(server::run(registry, cli.port));
}

#[cfg(feature = "desktop")]
fn display_available() -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"]
        .into_iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Picks the GUI's data source: attach to an already-running daemon when one
/// answers on `port`, otherwise own an embedded registry + server.
#[cfg(feature = "desktop")]
fn choose_gui(daemon_found: bool, port: u16) -> ui::state::GuiMode {
    if daemon_found {
        tracing::info!("Grit daemon detected on port {port}; attaching as client");
        ui::state::GuiMode::Remote { port }
    } else {
        ui::state::GuiMode::Embedded(server::registry::TabRegistry::new())
    }
}

fn resolve_path(path: &Path) -> PathBuf {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(path)
    };
    candidate.canonicalize().unwrap_or(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        match Cli::parse_from(args).expect("parse should succeed") {
            Parsed::Run(cli) => cli,
            Parsed::Print(_) => panic!("expected options, got help/version output"),
        }
    }

    #[test]
    fn parses_defaults() {
        let cli = parse(&["grit"]);
        assert!(!cli.headless);
        assert_eq!(cli.port, 5000);
        assert!(cli.path.is_none());
    }

    #[test]
    fn parses_headless_port_and_path() {
        let cli = parse(&["grit", "--headless", "--port", "9090", "--path", "/repo"]);
        assert!(cli.headless);
        assert_eq!(cli.port, 9090);
        assert_eq!(cli.path, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn parses_inline_equals_values() {
        let cli = parse(&["grit", "--port=9090", "--path=/repo"]);
        assert_eq!(cli.port, 9090);
        assert_eq!(cli.path, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn rejects_unknown_and_incomplete_flags() {
        assert!(Cli::parse_from(["grit", "--nope"]).is_err());
        assert!(Cli::parse_from(["grit", "--port"]).is_err());
        assert!(Cli::parse_from(["grit", "--port", "not-a-number"]).is_err());
        assert!(Cli::parse_from(["grit", "--headless=yes"]).is_err());
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(
            Cli::parse_from(["grit", "--help"]),
            Ok(Parsed::Print(_))
        ));
        assert!(matches!(
            Cli::parse_from(["grit", "-V"]),
            Ok(Parsed::Print(_))
        ));
    }

    #[test]
    fn resolve_path_expands_relative_to_cwd() {
        let absolute = resolve_path(Path::new("/some/abs/path"));
        assert_eq!(absolute, PathBuf::from("/some/abs/path"));
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn gui_mode_prefers_running_daemon() {
        match choose_gui(true, 5000) {
            ui::state::GuiMode::Remote { port } => assert_eq!(port, 5000),
            _ => panic!("a running daemon must select Remote mode"),
        }
        assert!(matches!(
            choose_gui(false, 5000),
            ui::state::GuiMode::Embedded(_)
        ));
    }
}