# ADR-0009: Project instructions via AGENTS.md + `/init`

Status: Accepted
Date: 2026-09-26

## Context

`rem` has no per-project memory today: `Context` (`src/context.rs`) is in-memory conversation history only, and the agent preamble (`src/agent.rs:228`) is a frozen static string baked at startup. Every session starts with zero knowledge of the repo's layout, build commands, or conventions. Goal: load project instructions from a conventional `AGENTS.md` file and provide an `/init` command to generate one — following the locked spec (project-root only, preamble-append, overwrite, restart-required).

## Decision

- **Loader** (`src/agent.rs`): `load_agents_md(project_root)` reads `<project_root>/AGENTS.md` verbatim — no cap, no transform. In `RigAgent::new`, the body is appended to the static preamble after a `Project context from AGENTS.md:` header marker. Missing/unreadable file → silently skip, static preamble only.
  - Precedence: static rem instructions stay primary; project context follows. Preamble is sent on every request by Rig (system message at index 0), so project context automatically outranks history and survives compaction (`context.rs` summary lives at `messages[0]` *after* the preamble).
- **`/init`** (`src/tui.rs`): new registry entry (`init`, no arg, `desc: "generate AGENTS.md for this project"`). `submit()` rewrites it to a fixed `INIT_PROMPT` and falls through to the normal turn path — the model inspects the repo with `read`/`list_directory`/`glob`/`grep`/`git_status` tools and overwrites project-root `AGENTS.md` via `write`. Transcript shows `/init`, not the full prompt (history records `/init`; only the agent receives `INIT_PROMPT`).
  - Normal approval flow applies: `write AGENTS.md` inside the project hits the `write` fast-path (`permissions.rs`) and auto-allows; large content (>32 KiB) or sensitive-path collisions escalate to `Confirm` as usual — no special-casing.
  - Restart-required: Rig bakes the preamble at build time with no mutation API, so the fresh file takes effect on next launch. No agent rebuild, no hot-reload.
- **Menu/tests**: registry is the single source (ADR-0007), so `/help`, `/` palette, and counts update automatically. Updated the 4→5-command overflow expectations (`+2/+3` → `+3/+4`) and wrap sequence (`[1,2,3,0]` → `[1,2,3,4,0]`).

## Alternatives considered

- Hierarchical walk-up (`./AGENTS.md` → parent dirs → `~/.config/rem/`): rejected — locked spec says project-root only; keeps resolution trivial and predictable.
- Size cap / truncation: rejected — locked spec says no cap; `AGENTS.md` is human-authored and small by convention.
- Local template write for `/init` (no model call): rejected — can't inspect the codebase; model-driven turn reuses existing tools + approvals for repo-specific output.
- Hot-reload after `/init`: rejected — would require rebuilding the whole `rig::agent::Agent` mid-session and re-seeding `Context`; restart is simpler and the file is long-lived.
- Prepend before static instructions: rejected — locked spec keeps static instructions primary so project context can't override safety/approval behavior.

## Consequences

- Fresh clones with an `AGENTS.md` get project context on every turn with zero flags.
- Repos without the file behave exactly as before (silent skip) — `/init` is the discoverable path (`/help` lists it).
- `/init` overwrites unconditionally per spec — no merge, no refuse-if-exists. Users with hand-edited files should back up first.
- New tests: `load_agents_md` missing/verbatim (`agent.rs`), `/init` turn + transcript display (`tui.rs`).
