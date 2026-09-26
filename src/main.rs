mod agent;
mod config;
mod context;
mod history;
mod markdown;
mod theme;
mod permissions;
mod tools;
mod tui;

use agent::RigAgent;
use config::Config;
use tui::{RatatuiBackend, TuiBackend};

#[tokio::main]
async fn main() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rem: {e}");
            eprintln!("hint: cp .env.example .env, fill in REM_* values, then `cargo run`");
            std::process::exit(1);
        }
    };

    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let (approval_tx, approval_rx) = permissions::approval_channel();

    let agent = match RigAgent::new(&cfg, approval_tx, project_root) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rem: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = RatatuiBackend::new().run(agent, approval_rx) {
        eprintln!("rem: TUI error: {e:#}");
        std::process::exit(1);
    }
}
