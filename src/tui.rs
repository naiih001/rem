use std::sync::{Arc, Mutex};

use cursive::{
    traits::*,
    views::{EditView, LinearLayout, TextView},
    CbSink, Cursive,
};

use crate::agent::AgentLoop;

/// Swappable UI abstraction. `CursiveBackend` is the cursive 0.21 implementation;
/// a Ratatui backend would implement this same trait and be swapped in `main.rs`.
pub trait TuiBackend {
    fn run(self, agent: impl AgentLoop + Send + Sync + 'static) -> anyhow::Result<()>;
}

pub struct CursiveBackend;

impl CursiveBackend {
    pub fn new() -> Self {
        Self
    }
}

impl TuiBackend for CursiveBackend {
    fn run(self, agent: impl AgentLoop + Send + Sync + 'static) -> anyhow::Result<()> {
        let agent = Arc::new(agent);
        // Called inside the tokio runtime (from `#[tokio::main]`), so a Handle exists.
        let handle = tokio::runtime::Handle::current();
        let buf = Arc::new(Mutex::new(
            "rem — chat below. /quit exits, /clear clears.".to_string(),
        ));

        let mut siv = cursive::default();

        let log = TextView::new(buf.lock().unwrap().clone())
            .with_name("log")
            .scrollable();

        // Clone Arcs into the (UI-thread, 'static) submit callback.
        let submit_agent = agent.clone();
        let submit_buf = buf.clone();
        let submit_handle = handle.clone();
        let on_submit = move |siv: &mut Cursive, text: &str| {
            let input = text.trim().to_string();
            siv.call_on_name("in", |v: &mut EditView| v.set_content(""));
            if input.is_empty() {
                return;
            }
            if input == "/quit" {
                siv.quit();
                return;
            }
            if input == "/clear" {
                if let Ok(mut b) = submit_buf.lock() {
                    b.clear();
                }
                siv.call_on_name("log", |v: &mut TextView| v.set_content(""));
                return;
            }

            append_log(siv, &submit_buf, &format!("> {input}"));
            siv.call_on_name("status", |v: &mut TextView| v.set_content("thinking…"));

            // NEVER block_on on the UI thread: work happens on a spawned thread
            // that blocks on the Rig future, then posts UI updates via CbSink.
            let sink: CbSink = siv.cb_sink().clone();
            let a = submit_agent.clone();
            let b = submit_buf.clone();
            let h = submit_handle.clone();
            std::thread::spawn(move || {
                let result = h.block_on(a.chat(&input));
                let _ = sink.send(Box::new(move |s: &mut Cursive| {
                    match result {
                        Ok(text) => {
                            append_log(s, &b, &text);
                            s.call_on_name("status", |v: &mut TextView| {
                                v.set_content("ready")
                            });
                        }
                        Err(e) => {
                            append_log(s, &b, &format!("[error] {e}"));
                            s.call_on_name("status", |v: &mut TextView| {
                                v.set_content("error — see log")
                            });
                        }
                    }
                }));
            });
        };

        let layout = LinearLayout::vertical()
            .child(log)
            .child(
                LinearLayout::horizontal()
                    .child(TextView::new("> ").fixed_width(2))
                    .child(
                        EditView::new()
                            .on_submit(on_submit)
                            .with_name("in")
                            .full_width(),
                    ),
            )
            .child(TextView::new("ready").with_name("status").fixed_height(1));

        siv.add_fullscreen_layer(layout);
        siv.run();
        Ok(())
    }
}

fn append_log(siv: &mut Cursive, buf: &Arc<Mutex<String>>, line: &str) {
    let snapshot = {
        let mut b = buf.lock().unwrap();
        if !b.is_empty() {
            b.push('\n');
        }
        b.push_str(line);
        b.clone()
    };
    siv.call_on_name("log", |v: &mut TextView| v.set_content(snapshot));
}
