mod git;
mod server;
mod ui;

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
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Repository path to open.
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

fn main() -> iced::Result {
    let cli = Cli::parse();
    tracing_subscriber::fmt::init();

    let repo_path = resolve_path(&cli.path);

    if cli.headless {
        let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
        runtime.block_on(server::run(repo_path, cli.port));
        Ok(())
    } else {
        let runtime = tokio::runtime::Runtime::new().expect("failed to start Tokio runtime");
        runtime.spawn(server::run(repo_path.clone(), cli.port));
        ui::state::run(repo_path)
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
        assert_eq!(cli.port, 8080);
        assert_eq!(cli.path, PathBuf::from("."));
    }

    #[test]
    fn parses_headless_port_and_path() {
        let cli = Cli::parse_from(["grit", "--headless", "--port", "9090", "--path", "/repo"]);
        assert!(cli.headless);
        assert_eq!(cli.port, 9090);
        assert_eq!(cli.path, PathBuf::from("/repo"));
    }

    #[test]
    fn resolve_path_expands_relative_to_cwd() {
        let absolute = resolve_path(Path::new("/some/abs/path"));
        assert_eq!(absolute, PathBuf::from("/some/abs/path"));
    }
}