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

## Non-goals (v1)

- True OS sandboxing (namespaces, seccomp, grsecurity). The gate is a *policy + human* layer, not a kernel boundary.
- Secrets redaction on read.
- Multi-user / remote approver.
