# ADR-0013: Claude-compatible skills (`SKILL.md` bundles)

Status: Accepted
Date: 2026-09-30

## Context

`rem` has no reusable knowledge primitive beyond `AGENTS.md`: a single verbatim project-root file appended to the frozen startup preamble (ADR-0009), rewritten only by `/init`. Users coming from Claude Code / Pi / Codex expect **skills** — directory bundles (`SKILL.md` + supporting files) that the model discovers at startup (name + description index) and loads on demand. Goal: Claude-compatible `SKILL.md` support with project + global discovery, explicit `/skill:<name>` invoke (colon form), stacked multi-skill mentions, and a skills-only reload that makes new skills auto-work without restart.

## Decision

- **Format** (`src/skills.rs`): Claude-compatible `SKILL.md` — YAML frontmatter (`name`, `description`) + Markdown body. Only `description` is read; all other frontmatter is ignored for v1 with `TODO(skills-extras)` markers.
- **Discovery**: three roots, scanned in precedence order (first wins): `<project>/.agents/skills/` → `~/.config/rem/skills/` → `~/.agents/skills/`. Each holds `<name>/SKILL.md` bundles. Missing roots skipped; dot-folders skipped; folders without `SKILL.md` skipped.
- **Identity**: folder name is canonical. Frontmatter `name` is ignored (stricter than Claude, which lets it override for display).
- **Lenient load** (mirrors Claude): missing `description` → first non-empty body line (`MAX_DESC_CHARS = 1536` cap); bad YAML / missing frontmatter → still loads name-only with a `note`, direct invoke works, auto-match degrades. Parse notes surface in `/skills` (rem has no `--debug` flag yet).
- **Loading**: startup preamble appends a skills index section after `AGENTS.md` — one row per skill (`name — description — SKILL.md path (+ bundle siblings)`). Full bodies load on demand via the existing `read` tool (always `Allow`, no permission change). Sibling bundle files are listed (sorted, `MAX_SIBLINGS = 20` + `+N more`); the model reads them with `read`/`glob` and runs scripts via normal `bash` approvals. No new tool, no privileged runner.
- **Auto-use**: the model may self-load a listed skill whenever the task matches, plus explicit user invoke.
- **Commands** (`src/tui.rs` registry + `submit()`):
  - `/skills` lists discovered skills (`/skill:<name> — <desc> — <path>`); `/skills reload` re-scans + rebuilds (below). Refused while `app.busy`.
  - `/skill:<name>` (colon form): leading mentions (up to `MAX_STACKED = 6`, first + 5 more per Claude) expand to full bodies prepended in order, shared trailing text substituted as `$ARGUMENTS` (`$ARGUMENTS[N]` / `$N`, shell-quote-aware split; `ARGUMENTS: <text>` appended when no placeholder consumed it). Transcript + history show the short form (mirrors `/init` display handling). Unknown skill → notice, no turn. Bare `/skill:` → usage notice.
  - Inline stacked mentions (`... /skill:a /skill:b task`) work when leading; mid-sentence mentions are not leading and pass through untouched.
- **Menu**: `/skill:<partial>` suggests cached skill names (`▸ <name>` rows, Tab/Enter complete to invokable `/skill:<name>` text), mirroring the `theme_arg_*` pattern but sourced from the `App.skill_names` cache (populated at startup, refreshed on reload) instead of the live filesystem.
- **Reload rebuild**: `RigAgent` holds `inner: RwLock<rig::agent::Agent>` plus rebuild inputs (`cfg`, `approval_tx`, `project_root`, `gate`, `skills`). `/skills reload` rescans and rebuilds the inner agent via the extracted `build_inner` so subsequent turns see new skills. History (`Context`), recorder, gate session-allows, and mode survive (same Arcs reused). Fail-closed: rebuild error keeps the old agent. `AgentLoop` gains defaulted `list_skills` / `get_skill_body` / `reload_skills_sync` so other impls need no changes.
- **Docs**: this ADR + `docs/glossary.md` `## Skills` section.

## Alternatives considered

- **Full bodies in preamble** (like `AGENTS.md` verbatim): rejected — token cost scales with skill count; Claude's metadata-index + on-demand body is the proven shape.
- **Dedicated `skill` tool** (name → body): rejected — existing `read` (always `Allow`) suffices; preamble paths make loads auditable in the work tree.
- **Space form `/skill <name>`**: rejected — user explicitly chose colon form `/skill:<name>`.
- **Live filesystem watch**: rejected — startup snapshot + manual `/skills reload` is predictable and matches the frozen-preamble architecture.
- **Privileged script execution / `allowed-tools` grants**: rejected for v1 — skill scripts go through the standard `bash` Confirm/Deny policy (ADR-0002 unchanged).
- **List+invoke-only reload** (preamble stale until restart, AGENTS.md rule): rejected — user required new skills to auto-work in normal chat, accepting the rebuild risk ADR-0009 declined for `/init`.
- **Strict skip on malformed skills**: rejected — Claude-lenient keeps one bad hand-edited file from breaking startup or blocking direct invoke.

## Consequences

- Fresh launches with skill dirs get a preamble index with zero flags; repos without skills behave exactly as before (empty index).
- `/skills reload` rebuilds the `rig::agent::Agent` mid-session — the riskiest piece (see plan risks: TUI refuses while busy, same-Arcs reuse, fail-closed on error). If unstable in practice, fallback is list+invoke-only until restart.
- Menu owns `:` while `/skill:` is open (colon form is new syntax; `parse_command` still splits whitespace only).
- `permissions.rs`, `sessions.rs`, `config.rs`, `context.rs` untouched.
- New tests: 13 `skills::tests` + TUI `/skills`, `/skill:` invoke (unknown/single/stacked/bare), menu suggest/open.
- Follow-ups (marked `TODO(skills-extras)`): frontmatter `name` override, `disable-model-invocation` / `user-invocable`, `allowed-tools` (needs `PermissionHook` integration), `when_to_use`, `!` injection, `${VARS}`; preamble budget cap; `--debug` surfacing for parse errors.
