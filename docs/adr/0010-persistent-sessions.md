# ADR-0010: Persistent sessions

Status: Proposed
Date: 2026-09-26
Context: rem sessions are ephemeral today: history lives in `Context` in memory (`src/agent.rs`), and restarting the process loses everything. Goal is SQLite-backed persistent sessions scoped per project, with CLI + slash-command resume, rename, and fork. Storage layer already landed in `src/sessions.rs` (`open`, `create_session`, `get_session`, `list_for_project`, `save_messages`, `update_title`); startup wiring landed in `src/main.rs` (manual `--resume <id>` parse, canonicalized cwd as project root, `REM_SESSION_*` env stash, `src/tui.rs` signature untouched). TUI picker/commands + per-turn autosave wiring remain.

## Decision

- Storage: SQLite at `dirs::data_dir()/rem/sessions.db` (`sessions::db_path` creates parent dirs; `sessions::open` runs `CREATE TABLE IF NOT EXISTS`).
- Schema: `sessions(id TEXT PRIMARY KEY, project_root TEXT NOT NULL, title TEXT NOT NULL, created_at INTEGER, updated_at INTEGER, model TEXT, messages_json TEXT NOT NULL)` (`src/sessions.rs`). IDs are `gen_id` hashes (time + pid + thread + nanos).
- Entry points:
  - `--resume <id>` CLI flag (manual argv parse, no new deps); unknown id is fatal (`rem: no session with id ...`).
  - `/resume` opens a centered popup picker; `/resume <id>` resumes directly; `/rename <title>` retitles current session; `/fork` clones current history into a new session id.
- Autosave: each completed turn persists via `blocking_lock` `export_sync` -> `save_messages` (sync TUI submit path; async `export_messages_json` delegates to it).
- Titles: fallback is first-user-message truncated to 40 chars; LLM title (`RigAgent::generate_title`, 2-5 word prompt) is TODO and unwired — new rows start as `"untitled"`.
- Project scope: per-project filter is literal canonicalized cwd string (`list_for_project WHERE project_root = ?1 ORDER BY updated_at DESC`); no prefix/glob matching.
- Delete deferred: no `DELETE` path, no dedicated tool; removal only via external sqlite manipulation.

## Rationale

- SQLite in the platform data dir needs no server, survives restarts, and keeps one file per user across all projects.
- Literal-cwd filter is predictable and unit-testable; fuzzy project matching is deferred complexity.
- `blocking_lock export_sync` unblocks autosave without making the TUI submit path async (explicit TODO to do that properly).
- Picker + direct-id + rename + fork covers the resume UX with four small commands sharing `get_session`/`create_session`, instead of a larger session-management surface.
- Title fallback keeps rows readable without a blocking LLM call on every new session.

## Consequences

- `main.rs` owns lifecycle: create-or-resume before the agent starts, `import_messages_json` preload, `REM_SESSION_ID/DB/TITLE` env handoff to phase-3 TUI code.
- Every turn must call `export_sync` + `save_messages` and bump `updated_at`; failed saves must surface, never silently drop history.
- `list_for_project` ordering (`updated_at DESC`) defines picker order; `list_all` exists but the picker stays project-scoped.
- `update_title`/`touch` bump `updated_at`, so renames reorder the picker.
- Tests: round-trip create/save/get, unknown-id fatal, literal-cwd isolation, fallback title truncation, picker order by `updated_at`.

## Open Questions (plan-time)

- D1 autosave failure UX: transient status vs blocking error when `save_messages` fails mid-turn?
- D2 `/resume` picker shape: centered popup dimensions, preview of messages, keyboard map vs slash-menu (ADR-0007) ownership?
- D3 `/fork` semantics: copy `messages_json` verbatim with new id, or re-summarize; what title (`copy of X` vs prompt)?
- D4 LLM titles: when to call `generate_title` (after turn 1, lazily on picker open) without blocking submit?
- D5 delete story: keep deferred forever, or add `/delete` with confirm given no dedicated delete tool (glossary: Delete)?
- D6 cross-project listing: does the picker ever need `list_all`, or stay strictly per-project?
