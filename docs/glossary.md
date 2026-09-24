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

## Input area (ADR-0004, Aster-style)

- **Input band**: 3-row shaded composer (`PANE_BG` `#191919`): 1-row vertical padding around the text line. Middle row = 1-column inset + `❯ ` prompt + input window.
- **Status row**: Zero-height when idle; 1 row when busy or approvals queued. Busy = braille spinner + `working · {s}s · esc to interrupt`. Approval = `waiting approval (N queued)` + key hints.
- **Gap row**: 1 terminal-bg row separating the transcript from the bottom pane. The shaded band starts at the input, not the gap.
- **Footer**: Single quiet line: `▶▶▶ edit · {model} · {N} turns · hints`. No busy readout; busy state lives in the status row.

## Message list (ADR-0005, Aster-style)

- **Scrollback model**: Finished rows print into the terminal's own scrollback and are never touched again. Only the bottom pane stays managed. No Tab selection, no expand/collapse, no click-toggle, no in-app scroll.
- **Chapter mark**: The user row — filled band (`rail_bg`) with accent left rail (`▌`) + `❯ ` prompt. The only banded row; not a bullet.
- **Bullet row**: Every other row — dim `• ` glyph with hanging indent (continuations indent under the bullet, 2-column gutter).
- **Tool row**: Flat, one group per tool call. Bold label line + dim nested sub-rows, `└` branch on the first sub-row.
- **Patch row**: `▸ verb path` + `+N −M` counts pushed right, then a tinted body: full-row add/del background bands with a darker mark glyph.
- **Elision**: Long tool output renders first 4 + last 4 lines with a gap marker. No expand; the tail is always visible.
- **Hidden thinking**: Reasoning recorded internally, never printed. No marker row.
- **Live stream**: Each resolved tool prints immediately into scrollback.
- **Trailer**: `• Done ({elapsed}s · N tools)`. File counts, `+/-`, cost are later slices.

## Non-goals (v1)

- True OS sandboxing (namespaces, seccomp, grsecurity). The gate is a *policy + human* layer, not a kernel boundary.
- Secrets redaction on read.
- Multi-user / remote approver.
