mod agent;
mod config;
mod context;
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

    let agent = match RigAgent::new(&cfg) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rem: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = RatatuiBackend::new().run(agent) {
        eprintln!("rem: TUI error: {e:#}");
        std::process::exit(1);
    }
}
