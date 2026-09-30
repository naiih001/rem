# rem — TODO: parity with Claude Code / Codex CLI / Pi

Source: gap analysis of `rem` (dev @ d819188) vs Claude Code, Codex CLI, Pi agent.
Last updated: 2026-09-30

Legend: `[ ]` todo, `[/]` in-progress branch, `[x]` done.

## P0 — not competitive without these

### 1. Fix broken tree + ship baseline
- [ ] Resolve merge conflicts in `src/agent.rs`, `src/main.rs`, `src/tui.rs` (`feat/skills` markers)
- [ ] `cargo build`, `cargo test`, `cargo fmt`, `cargo clippy -D warnings` green
- [ ] Merge `feat/skills` cleanly into `dev`
- [ ] Add license (MIT/Apache-2.0), flesh out README + `--help`

### 2. Headless / automation interfaces (TUI-only today)
- [ ] `rem -p "<prompt>"` print mode (pipe final response to stdout)
- [ ] `rem exec "<prompt>"` non-interactive run, non-zero exit on error
- [ ] `--output json|jsonl` event stream (turns, tool calls/results, errors)
- [ ] Stdin pipe support (`echo ... | rem -p`)
- [ ] `--yolo` / `--plan` / `--mode <name>` flags for headless runs
- [ ] `--resume <id>` works headless; fail-open policy flag (`--approve` / `--deny` default)

### 3. MCP client (`feat/mcp` exists, not landed)
- [ ] Land `feat/mcp`: MCP client (stdio + SSE/http)
- [ ] MCP servers from config (`~/.config/rem/mcp.toml` or `[mcp]` section)
- [ ] Discovered MCP tools flow through `PermissionHook::classify` (fail-closed `Confirm`)
- [ ] `/mcp` command: list servers/tools, reconnect, auth status
- [ ] MCP tools appear in preamble + `last_tool_events` tree

### 4. `@file` / ref-files (`feat/ref-files` exists, not landed)
- [ ] Land `feat/ref-files`: `@path` expansion before `chat()`
- [ ] Support `@dir/`, `@file:line-range`, multiple refs per prompt
- [ ] Missing-file error surfaces in TUI, doesn't send turn
- [ ] Docs + `/help` entry

### 5. Providers + models (single OpenAI `/chat/completions` today)
- [ ] Multi-profile config (`[providers.*]`, env-var keys, no hardcoded single `[api]`)
- [ ] `/model` switch mid-session + `/effort` (`low/medium/high`)
- [ ] Anthropic native, Bedrock/Vertex passthrough or OAuth + `/login`
- [ ] Retry + rate-limit backoff + timeout errors surfaced per-turn
- [ ] Token meter + `/cost` + `/context` (used/total, compaction point)

### 6. Subagents + task decomposition (single linear loop today)
- [ ] `TodoWrite` tool (list, status, transcript UI)
- [ ] `Task` tool: spawn subagent with scoped prompt/tools (even serial queue v1)
- [ ] Parallel subagent fan-out (bounded) + result merge into parent turn
- [ ] Background tasks with polling (`/tasks`)

### 7. Checkpoints / undo / rewind (no revert today)
- [ ] Snapshot `git diff` + `messages_json` before each mutate turn
- [ ] `/rewind [n]` restores files + history; `/undo` last mutate
- [ ] Diff-accept flow: show patch, `[a]ccept / [r]eject` before `edit/write` lands
- [ ] `apply_patch`-style fuzzy edit tool (replace exact-`oldText`-only `edit`)

## P1 — expected by users

- [ ] Live token streaming (today: tool completions only, no token stream)
- [ ] Plan artifact: `plan` mode writes plan file, approval-to-implement handoff
- [ ] Memory hierarchy: `AGENTS.md` walk-up + `~/.config/rem/AGENTS.md` + `@import` + `/memory`; hot-reload (no restart)
- [ ] Custom commands: `~/.config/rem/commands/*.md` + project `.agents/commands/*.md` (prompt templates)
- [ ] User hooks: pre/post tool-call hooks from config (command hooks a la Codex)
- [ ] Extensions API or WASM/plugin tools (or defer to MCP-only — decide)
- [ ] Images / multimodal: paste image, `read` PDFs/screenshots as content blocks
- [ ] Trust model: `project_trust` prompt before loading project `AGENTS.md`/skills/MCP
- [ ] Session branches: continue-from-earlier = new branch; `/branch`, history tree UI
- [ ] Session export/share (`/export`, JSONL dump); `list_all` picker across projects
- [ ] GitHub workflow: `/pr`, `gh` integration, worktree support
- [ ] Background `bash` (async run + `poll`, no 30s kill)
- [ ] Diagnostics loop: LSP / `cargo check` feedback after edits
- [ ] Wire `generate_title()` (today every session is `untitled` + 40-char fallback)
- [ ] `/skills` extras: `disable-model-invocation`, `user-invocable`, `allowed-tools`, `when_to_use`

## P2 — product maturity

- [ ] Distribution: `cargo install rem`, brew/npm, auto-update check, shell completions
- [ ] `/doctor`: config, provider ping, MCP servers, theme, db health
- [ ] Config hierarchy: global + project-local `.rem/config.toml` + env overrides + wizard
- [ ] Keybinding config (today hardcoded Esc/Ctrl-C/Ctrl-D/Tab)
- [ ] Observability: `--verbose` log file, persisted audit trail of approvals, cost/latency trailer (`Done: Xs · N tools · $c`)
- [ ] Hardening: seatbelt/Docker/container mode; secrets redaction on `read`; symlink-safe `inside_project`
- [ ] Docs: `docs/cli.md`, `docs/permissions.md`, `docs/sessions.md`, `docs/skills.md` parity with Pi docs set

## Tech debt / correctness

- [ ] `edit` uniqueness check is `O(n·m)` string scan — fine, but error shows only 120 chars; include line numbers
- [ ] `split_segments` shell parser: document as heuristic, add fuzz tests for `$(...)`, quotes, `#` comments
- [ ] `Context::TOOL_BUDGET=2000` chars vs bytes mismatch (`truncate` on bytes, note on `len()`); unify on chars
- [ ] SQLite: WAL mode, index on `project_root, updated_at`; prune old sessions (`/prune`)
- [ ] TUI (5251 lines): split `tui.rs` into `tui/{app,events,render,commands}.rs`
- [ ] Glossary promises `permission ...` transcript blocks — implement or drop the claim
- [ ] CI: runagate `feat/mcp` + `feat/ref-files` merges through `dev` test + clippy gate
