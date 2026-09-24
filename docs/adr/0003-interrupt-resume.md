# ADR-0003: Human interrupt/resume inside the same run

Status: Accepted
Date: 2026-09-24
Context: `RigAgent::chat()` runs a blocking multi-step loop on a tokio worker. The TUI owns the event loop. Approval parks the worker, prompts, and resumes without dropping history.

## Decision

- `PermissionHook` holds `ApprovalTx = tokio::sync::mpsc::UnboundedSender<ApprovalRequest>`.
- `ApprovalRequest { tool_name, args_preview, full_args, verdict_reason, reply: oneshot::Sender<ApprovalDecision> }`.
- `ApprovalDecision = Approve | ApproveAlways(rule) | Deny | AbortTurn`.
- `on_tool_call` for `Confirm`: `send(request).await`, then `reply.await`. This parks the agent worker only; TUI stays live.
- TUI `event_loop` polls an `ApprovalRx` alongside turn/live channels, renders a modal (`y approve / a always / n deny / x abort`), routes the keypress to `reply.send(...)`.
- Mapping: `Approve|ApproveAlways -> Run`, `Deny -> Skip("user denied ...")`, `AbortTurn -> Stop("aborted by user")`.
- Deny feedback text goes to the model so it replans in the same run. Abort ends the run with history intact for the next prompt.

## Rationale

- `oneshot` gives exactly-once decision semantics with no polling.
- Unbounded mpsc avoids hook-side backpressure deadlock; at most a handful of pending approvals exist since the worker is parked.
- Keeping `Recorder` before `PermissionHook` preserves full observation even for denied calls.

## Consequences

- `RigAgent::new` takes `approval_tx` + `project_root`. TUI `run_app`/`event_loop` own the `ApprovalRx` and modal state (`y` approve / `a` always / `n` deny / `x` abort).
- Approval wait is cancellation-safe: a dropped oneshot maps to `Stop`, never a hang.
- Every decision appends an audit `System` block to the transcript (`permission approved/denied/...`).

## Resolved defaults (grill Q5-Q9, no user reply; recommended values applied)

- Q5 placement: `src/permissions.rs` (top-level, beside `agent.rs`/`tui.rs`).
- Q6 keys: `y` approve, `a` always-allow, `n` deny, `x` abort turn. `always` lasts the session, scoped to exact tool + normalized args via `rule_key`.
- Q7 absence: no timeout; the modal owns all keys until resolved. Esc never dismisses.
- Q9 headless: fail closed to `Skip` (deny in-run, model replans). No stdin prompt.
