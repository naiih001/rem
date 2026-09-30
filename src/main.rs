mod agent;
mod config;
mod context;
mod history;
mod markdown;
mod modes;
mod theme;
mod permissions;
mod sessions;
mod skills;
mod tools;
mod tui;

use agent::{AgentLoop, RigAgent};
use config::Config;
use tui::{RatatuiBackend, TuiBackend};

#[tokio::main]
async fn main() {
    let cfg = match Config::from_file() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rem: {e}");
            eprintln!("hint: create ~/.config/rem/config.toml with [api] and [model] sections");
            std::process::exit(1);
        }
    };

    // Manual --resume <id> parse (no new deps).
    let args: Vec<String> = std::env::args().collect();
    let mut resume_id: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--resume" {
            if i + 1 >= args.len() {
                eprintln!("rem: --resume requires a session id");
                std::process::exit(1);
            }
            resume_id = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let project_root = cwd.canonicalize().unwrap_or(cwd);
    let project_root_str = project_root.to_string_lossy().to_string();

    let conn = match sessions::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rem: {e}");
            std::process::exit(1);
        }
    };
    let db_path_str = sessions::db_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // New session or resume; resume of an unknown id is fatal.
    let is_resume = resume_id.is_some();
    let mut session = match resume_id {
        Some(id) => match sessions::get_session(&conn, &id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                eprintln!("rem: no session with id {id}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("rem: {e}");
                std::process::exit(1);
            }
        },
        None => match sessions::create_session(&conn, &project_root_str, &cfg.model.default) {
            Ok(s) => {
                if let Err(e) = sessions::update_title(&conn, &s.id, "untitled") {
                    eprintln!("rem: {e}");
                    std::process::exit(1);
                }
                sessions::get_session(&conn, &s.id)
                    .ok()
                    .flatten()
                    .unwrap_or(s)
            }
            Err(e) => {
                eprintln!("rem: {e}");
                std::process::exit(1);
            }
        },
    };
    // Keep the in-memory copy consistent even if the refetch above failed.
    if session.title != "untitled" && !is_resume {
        session.title = "untitled".to_string();
    }
    drop(conn);

    let (approval_tx, approval_rx) = permissions::approval_channel();

    let agent = match RigAgent::new(&cfg, approval_tx, project_root) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rem: {e}");
            std::process::exit(1);
        }
    };

    // Preload resumed history before the TUI owns the agent.
    if let Err(e) = agent.import_messages_json(&session.messages_json).await {
        eprintln!("rem: failed to restore session history: {e}");
        std::process::exit(1);
    }

    // TUI signature unchanged (src/tui.rs untouched): stash the session
    // coordinates where phase 3 TUI code can read them.
    unsafe {
        std::env::set_var("REM_SESSION_ID", &session.id);
        std::env::set_var("REM_SESSIONS_DB", &db_path_str);
        std::env::set_var("REM_SESSION_TITLE", &session.title);
    }

    let run_result = RatatuiBackend::new().run(agent, approval_rx);

    // Post-exit cleanup (all quit paths: /quit, Ctrl+D on empty input):
    // run() already tore down raw mode + the inline viewport, so wipe
    // visible screen + scrollback (mirrors launch Clear All+Purge) leaving
    // only the resume hint. Env var wins: /resume//fork inside the TUI
    // sync the final id there before returning.
    {
        use crossterm::{cursor::MoveTo, execute, terminal::{Clear, ClearType}};
        let _ = execute!(
            std::io::stdout(),
            Clear(ClearType::All),
            Clear(ClearType::Purge),
            MoveTo(0, 0),
        );
    }
    let sid = std::env::var("REM_SESSION_ID").unwrap_or_default();
    let sid = if sid.is_empty() { session.id.clone() } else { sid };
    if !sid.is_empty() {
        println!("Session saved. Resume with: rem --resume {sid}");
    }

    if let Err(e) = run_result {
        eprintln!("rem: TUI error: {e:#}");
        std::process::exit(1);
    }
}
