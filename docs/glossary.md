# Glossary — Permission System (rem)

> Living doc. Updated as grilling resolves. Source of truth for terms used in ADRs and code.

## Core

- **Principal**: The LLM-driven agent loop (`RigAgent`). Untrusted for side effects; all mutations go through the gate.
- **Gate**: The enforcement point. A Rig `AgentHook::on_tool_call` implementation that classifies every tool call before execution. Single choke point, no tool-side bypass.
- **Policy**: Pure function `ToolCall -> Verdict`. Decides `Allow | Confirm | Deny` from tool name + args + session rules. No I/O, fully unit-testable.
- **Approver**: The human in the TUI. The only entity that can resolve a `Confirm` into `Allow | Deny`.
- **Run**: One `RigAgent::chat()` invocation, which internally may take up to 100 model/tool steps. Approvals must block *inside* the run, never kill it.

## Verdicts

- **Allow (automatic)**: Execute immediately, no human involved. Read-only tools, safe-listed commands.
- **Confirm (approval)**: Suspend tool execution, render approval modal, wait for human. Resume same run on decision.
- **Deny (blocked)**: Refuse without asking. Returns model-visible feedback so the model can replan. Used for `rm -rf /`, absolute destructive paths, exfiltration patterns.

## Tool classes

- **Read**: `read`, `list_directory`, `glob`, `grep`, `git_status`, `git_diff`. Default `Allow`. Caveat: secrets redaction is out of scope for v1.
- **Mutate**: `write`, `edit`. Default `Confirm`, with an auto fast-path when ALL hold: path inside project root, not sensitive (`.env`, `*.pem`, `*secret*`, `*credential*`, `~/.ssh`), change under 32 KiB.
- **Execute**: `bash` (`sh -c`). Classified by command text: safe-list `Allow`, destructive/irreversible/network `Confirm`, annihilator patterns `Deny`. Shell parsing is heuristic, never a sandbox guarantee.
- **Delete**: No dedicated tool today. Deletion currently only reachable via `bash rm`. Treated as `Execute` subclass: any `rm`, `rmdir`, `unlink`, `shred`, `mv ... /tmp`, `> file` truncation patterns default `Confirm` minimum, some `Deny`.
- **Network**: `web_fetch`, `web_search` plus `bash` network verbs (`curl`, `wget`, `ssh`, `nc`, `pip install`, `cargo fetch`, `apt-get`). Default `Confirm` per user spec.

## Approval interaction

- **Interrupt**: Hook sends `ApprovalRequest` over an mpsc channel and awaits a oneshot reply. The agent worker is parked; the model sees nothing until resolved.
- **Resume**: On approve, hook returns `ToolCallAction::Run`. On deny, `ToolCallAction::Skip(reason)` so the model receives feedback *inside the same run* and replans. `Stop` is reserved for user abort of the whole turn.
- **Session rule**: An `always allow` decision cached for the process lifetime (e.g. always allow `cargo test`). Scoped by exact tool + normalized args, never by prefix alone.
- **Audit trail**: Every decision appends a `permission ...` system block to the transcript.

## Popups (ADR-0011, ADR-0012)

- **Popup shell**: Shared centered modal surface shown inside a full-height managed viewport while open. It provides common layout, styling, selection, footer, and sizing behavior without prescribing content semantics; closing it restores the compact composer viewport.
- **Popup controller**: Feature-specific state and actions layered over the shared shell, such as resume, approval, or a future question prompt.
- **Inline filter**: Popup-owned filter mode rendered inside the shell. `/` enters it; `Esc` leaves filter mode before any popup dismissal policy is applied.
- **Popup navigation**: `j/k` and Up/Down move one item; `Ctrl+d/u` move half a visible page. `g/G` are not supported. `q` is reserved for popup-specific quit/abort semantics.
- **Blocking FIFO popup**: Approval popup mode showing only the oldest pending request. Later requests remain queued and cannot be selected or reordered.
- **Risk-first approval**: Approval presentation that shows the reason and risk classification before command details, with labels and restrained color cues supporting the decision.
- **Approval action bar**: Horizontal themed action row containing `Allow once`, `Allow always`, `Reject`, and `Abort`; `h/l` or Left/Right selects and Enter confirms.

## Input area (ADR-0004, Aster-style)

- **Input band**: Shaded composer (`PANE_BG` `#191919`) that grows with wrapped or explicit lines to five visible rows (fewer on short terminals), then scrolls internally. Normally one row of vertical padding surrounds the text; the slash menu may reclaim a padding row. Overflow uses `↑` / `↓` cues. `Shift+Enter` inserts a newline when enhanced keyboard reporting is available; `Ctrl+O` is the portable fallback. `Enter` submits.
- **Status row**: 1 row, blank while idle; the slash menu may reclaim it. Busy = braille spinner + `{verb} · {s}s · esc to interrupt`, where `{verb}` cycles sequentially through 10 verbs (`working, thinking, cooking, pondering, reasoning, crafting, brewing, scheming, conjuring, noodling`), advancing every 2s and restarting at `working` each task (ADR-0008). Approval = `waiting approval (N queued)` + key hints.
- **Gap row**: 1 terminal-bg row separating the transcript from the bottom pane. The shaded band starts at the input, not the gap.
- **Footer**: Single quiet line: `▶▶▶ edit · {model} · {N} turns · hints`. No busy readout; busy state lives in the status row.

## Message list (ADR-0005, Aster-style)

- **Scrollback model**: Finished rows print into the terminal's own scrollback and are never touched again. Only the bottom pane stays managed. No Tab selection, no expand/collapse, no click-toggle, no in-app scroll.
- **Rendered Markdown reply**: Assistant reply text parsed as Markdown for headings, nested lists, inline emphasis/code, links, fenced code blocks, blockquotes, and tables. Prompts and tool output remain plain text.
- **Markdown link**: Rendered as its label followed by its destination in parentheses.
- **Markdown table**: Columns are sized to fit the available transcript width; cell contents wrap within those columns.
- **Markdown code block**: Fenced code rendered with preserved line breaks, indentation, and a subtle background.
- **Chapter mark**: The user row — filled band (`rail_bg`) with accent left rail (`▌`) + `❯ ` prompt. The only banded row; not a bullet.
- **Bullet row**: Every other row — dim `• ` glyph with hanging indent (continuations indent under the bullet, 2-column gutter).
- **Tool row**: Flat, one group per tool call. Bold label line + dim nested sub-rows, `└` branch on the first sub-row.
- **Patch row**: `▸ verb path` + `+N −M` counts pushed right, then a tinted body: full-row add/del background bands with a darker mark glyph.
- **Elision**: Long tool output renders first 4 + last 4 lines with a gap marker. No expand; the tail is always visible.
- **Hidden thinking**: Reasoning recorded internally, never printed. No marker row.
- **Live stream**: Each resolved tool prints immediately into scrollback.
- **Trailer**: `• Done ({elapsed}s · N tools)`. File counts, `+/-`, cost are later slices.

## Sessions (ADR-0010)

- **Session**: One persisted conversation row in `sessions.db` (`id, project_root, title, created, updated, model, messages_json`). Created at startup, resumed via `--resume <id>` or `/resume`.
- **Picker**: Bottom popup listing project sessions by `updated_at DESC`. Opened by bare `/resume`; `/resume <id>` bypasses it.
- **Fork**: Clone of the current session's `messages_json` into a new session id. History verbatim, title reset.
- **Title**: Human label for a session row. Fallback is first user message truncated to 40 chars; LLM title (`generate_title`, 2-5 words) is TODO.
- **Autosave**: Per-turn persist via `blocking_lock` `export_sync` -> `save_messages`. Sync TUI submit path; failures must surface, never silent.
- **Project key**: Literal canonicalized cwd string stored as `project_root`. Picker filters by exact match (`list_for_project`), no prefix/glob.

## Skills (ADR-0013)

- **Skill**: Claude-compatible `<name>/SKILL.md` bundle (YAML frontmatter + Markdown body + optional sibling files). Identity is the folder name; frontmatter `name` ignored for v1.
- **Discovery roots**: `<project>/.agents/skills/` → `~/.config/rem/skills/` → `~/.agents/skills/`, first wins on name collision. Missing roots skipped.
- **Preamble index**: startup list of `name — description — SKILL.md path (+ bundle siblings)` after `AGENTS.md`. Bodies load on demand via `read`; scripts run via `bash` approvals.
- **Invoke**: `/skill:<name> [task]` colon form; leading stacked mentions (up to 6) expand bodies in order with `$ARGUMENTS` substitution. Transcript shows the short form.
- **Reload**: `/skills reload` re-scans + rebuilds the inner agent so normal chat auto-knows new skills; history/hooks/mode preserved, fail-closed on error. Refused while busy.
- **Lenient load**: missing `description` → first body line; bad YAML → name-only with note, direct invoke still works.

## Non-goals (v1)

- True OS sandboxing (namespaces, seccomp, grsecurity). The gate is a *policy + human* layer, not a kernel boundary.
- Secrets redaction on read.
- Multi-user / remote approver.

## Project instructions (ADR-0009)

- **AGENTS.md**: Verbatim project instructions loaded once at startup from `<project_root>/AGENTS.md` only (no walk-up, no global file, no cap). Appended after the static preamble under a `Project context from AGENTS.md:` header; silently skipped when missing. Outranks history, survives compaction, takes effect on next restart.
- **/init**: Slash command that rewrites to a fixed instruction and runs a normal agent turn — the model inspects the repo and overwrites project-root `AGENTS.md`. Transcript shows `/init`; standard write/approval flow applies.
- **Permission mode**: A session-local policy preset controlling which tool calls run automatically, ask for approval, or are denied. Modes are `plan`, `manual`, `auto`, `edit`, and `yolo`.
- **Plan mode**: Read-only permission mode. Prohibited calls receive model-visible denial feedback so the agent can replan.
- **Yolo mode**: Approval-free mode that still preserves unconditional denials for annihilators and secret exfiltration.
