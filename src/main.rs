mod git;
mod server;
mod ui;
mod shared_config;
pub mod actions;

use std::path::{Path, PathBuf};

use clap::Parser;

/// Fast, native, single-binary Git client.
#[derive(Debug, Parser)]
#[command(name = "grit", version, about)]
struct Cli {
    /// Run the headless web daemon without the desktop GUI.
    #[arg(long)]
    headless: bool,

    /// Port for the embedded web daemon.
    #[arg(long, default_value_t = 5000)]
    port: u16,

    /// Repository path to open.
    #[arg(long)]
    path: Option<PathBuf>,
}

fn main() -> iced::Result {
    let cli = Cli::parse();
    tracing_subscriber::fmt::init();

    let open_explicit = cli.path.is_some();
    let repo_path = resolve_path(cli.path.as_deref().unwrap_or(Path::new(".")));

    if cli.headless {
        // An explicit --path pins one tab; otherwise start empty so persisted
        // tabs are restored by boot() instead of being shadowed by a CWD tab.
        let registry = match cli.path {
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
        };
        let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
        runtime.block_on(server::run(registry, cli.port));
        Ok(())
    } else {
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
        ui::state::run(mode, repo_path, open_explicit)
    }
}

/// Picks the GUI's data source: attach to an already-running daemon when one
/// answers on `port`, otherwise own an embedded registry + server.
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

    #[test]
    fn parses_defaults() {
        let cli = Cli::parse_from(["grit"]);
        assert!(!cli.headless);
        assert_eq!(cli.port, 5000);
        assert!(cli.path.is_none());
    }

    #[test]
    fn parses_headless_port_and_path() {
        let cli = Cli::parse_from(["grit", "--headless", "--port", "9090", "--path", "/repo"]);
        assert!(cli.headless);
        assert_eq!(cli.port, 9090);
        assert_eq!(cli.path, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn resolve_path_expands_relative_to_cwd() {
        let absolute = resolve_path(Path::new("/some/abs/path"));
        assert_eq!(absolute, PathBuf::from("/some/abs/path"));
    }

    #[test]
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