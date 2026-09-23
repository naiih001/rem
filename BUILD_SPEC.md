# REM Build Spec — MVP Agent (Rig + Cursive)

## 0. Non-goals / Constraints
- Greenfield crate at `/home/naet/Documents/rem` (name `rem`, v0.1.0, edition 2024). Do NOT rename.
- No persistence, no sessions file, no memory. In-memory chat only.
- Bash is UNRESTRICTED for MVP (no allowlist). Just `sh -c` + 30s timeout + 8KB output cap.
- Keep orchestrator lean: implement fully, `cargo build` must pass clean.

## 1. Stack (verified, do NOT re-decide)
- `rig` (facade, ~0.42) with `rig::providers::openai::CompletionsClient::builder(api_key).base_url(url).build()` — explicit `.base_url()`, no auto `OPENAI_BASE_URL`. Use `completion_model(model_name)` → `/chat/completions` path for OpenAI-compat proxies. `tokio` required (`rt-multi-thread`, `macros`).
- `cursive 0.21` for TUI. NEVER `block_on` on UI thread. Pattern: clone `CbSink`, spawn worker thread, use `tokio::runtime::Handle` to `block_on` Rig call, `sink.send(Box::new(...))` to update UI. Show spinner/status before spawn.
- `dotenvy` for `.env`. `serde`/`serde_json` as needed. `schemars` for Rig `Tool` derives if needed (check docs).

## 2. Config — `src/config.rs`
- Load via `dotenvy::dotenv().ok()`.
- Required: `REM_API_KEY`, `REM_BASE_URL`, `REM_MODEL`. Missing → `anyhow`/`String` error with clear message naming the var. No defaults.
- Provide `Config::from_env() -> Result<Self, String>`.
- Create `.env.example` in repo root with the 3 vars + placeholder values. Do NOT overwrite existing `.env` (4 bytes currently). Do NOT commit secrets.

## 3. Tools — `src/tools/` (4 Rig tools)
Each must implement `rig::tool::Tool` (check exact trait for installed rig version via `cargo doc` / docs.rs — verify, don't guess).
- `read`: params `{path: String, offset: Option<usize>, limit: Option<usize>}`. Read file to string. Default full file. Errors as String.
- `write`: params `{path: String, content: String}`. `create_dir_all(parent)`, overwrite. Return confirmation with byte count.
- `edit`: params `{path: String, edits: Vec<{oldText: String, newText: String}>}`. Each `oldText` must match exactly once (error otherwise). Apply against original content (not incrementally if overlapping — simplest: apply sequentially, fail on missing/ambiguous). Write back. Return count.
- `bash`: params `{command: String}`. Run `sh -c <command>` in project cwd, 30s timeout (use `tokio::process` or `std::process` + `wait_timeout` crate if needed — prefer tokio timeout). Capture combined stdout+stderr, truncate to 8000 bytes + note. Return output + exit code.

## 4. Agent loop — `src/agent.rs`
- Define `#[async_trait] pub trait AgentLoop { async fn chat(&self, prompt: &str) -> Result<String, String>; }` (use `async-trait` crate).
- Implement `RigAgent` struct holding Rig agent (built with preamble + the 4 tools, `max_steps` ~8-10). Use Rig's BUILT-IN multi-step agent builder — do NOT hand-roll ReAct. Check installed rig version's agent API (`AgentBuilder`).
- `agent.rs` must isolate Rig specifics so loop can be swapped later.

## 5. TUI — `src/tui.rs`
- Define `pub trait TuiBackend { fn run(self, agent: impl AgentLoop + Send + Sync + 'static) -> anyhow::Result<()>; }` (or similar swappable abstraction).
- Implement `CursiveBackend` with layout: top scrollable chat/tool log (`TextView` named "out" or `LinearLayout`), bottom input (`EditView` named "in"), status bar (`TextView` named "status").
- Behavior: Enter submits (callback on EditView), `/quit` exits, `/clear` clears log. Normal text → set status "thinking…", spawn worker thread per §1, on result append `> prompt` + response + tool snippets inline (`[read path] → …` if available from Rig trace, else just final text). Errors show in log + status.
- Must compile without Ratatui. Comment where Ratatui impl would plug in.

## 6. Wiring — `src/main.rs` + `Cargo.toml`
- `main.rs`: load config (fail-fast: eprint + exit 1 with clear msg), build tokio runtime (`#[tokio::main]`), construct `RigAgent`, then `CursiveBackend::new().run(agent)`.
- `Cargo.toml`: add `rig`, `cursive`, `tokio` (rt-multi-thread, macros, process, time), `dotenvy`, `anyhow`, `async-trait`, `serde`, `serde_json`, (+ `schemars` if Rig tools need it). Keep versions compatible; run `cargo update` scope minimal.
- Module tree: `src/{main.rs, config.rs, agent.rs, tui.rs, tools/{mod.rs, read.rs, write.rs, edit.rs, bash.rs}}`.

## 7. Definition of Done
1. `cargo build` clean (warnings ok if minor, no errors).
2. TUI launches, can chat against `REM_*` endpoint, all 4 tools callable by model.
3. No persistence required.
4. Output final: `cargo build` tail + file list + how to run (`cp .env.example .env`, fill, `cargo run`).

If Rig API differs from spec, adapt to installed version but preserve traits + behavior. Verify, don't guess.
