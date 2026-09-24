# ADR-0006: Keymap — Esc sole interrupt, Ctrl+C clear-only, Ctrl+D quit-on-empty

Status: Accepted
Date: 2026-09-24
Context: The current keymap conflates clear / interrupt / quit. `handle_ctrl` only handles `c/u/w` (`src/tui.rs:720-736`) with no `Ctrl+D`; `Esc` merely clears input or deselects and quits on double-press (`src/tui.rs:641-652`, `src/tui.rs:687-698`); the approval modal owns all keys (`src/tui.rs:555-578`). Submit spawns a fire-and-forget chat task (`src/tui.rs:786-820`, `busy=true` at `src/tui.rs:766`, `busy=false` on drain at `src/tui.rs:492`) with an unabortable 80ms watcher, so the `esc to interrupt` labels (`src/tui.rs:1478-1482`, `src/tui.rs:1506-1507`) are unwired. `AbortTurn => Stop` already exists (`src/permissions.rs:625-626`).

## Decision

- `Ctrl+C` = clear input line only, always. Never interrupts, never quits. Even when busy + input-empty → clear-only, no interrupt. Ignored in the approval modal (no input to clear).
- `Ctrl+D` = quit rem, ONLY when the input line is empty. Non-empty input → no-op (Unix convention). If busy or approval-modal pending (and input empty) → abort the turn, then quit immediately. All other modal `Ctrl` combos remain ignored.
- `Esc` = SOLE interrupt path:
  - Busy → abort the in-flight chat `JoinHandle` (store the handle; `tokio::sync::Mutex` guard drops + unlocks on abort, partial messages persist), abort the leaked 80ms watcher, synthesize `TurnResult` cleanup (`busy=false`, keep partial tool blocks + system `interrupted` marker).
  - Modal pending → same as `x` / `AbortTurn` → `ToolCallAction::stop`.
  - Block-selected + busy → interrupt wins. Selected + idle → deselect (`Esc`). Idle input, nothing selected → no-op.
  - Double-`Esc` quit is REMOVED. The old `esc to interrupt` labels become real wiring.
- `/quit` alias kept. The new `Ctrl+D` quit path clears selection/history consistent with `/quit` (note: current `/quit` pushes history before quit and does not clear `selected` — flag to fix for parity).

## Rationale

- One key, one job: `Ctrl+C` (edit), `Esc` (interrupt), `Ctrl+D` (quit-on-empty) removes the dangerous clear-means-quit and clear-means-interrupt overloads.
- `Ctrl+D`-on-empty matches Unix shell convention, so muscle memory quits only when there is nothing to lose.
- Aborting the stored `JoinHandle` (rather than a flag) is the only way to stop the parked `chat()` worker from ADR-0003; the `Mutex` guard drop keeps the recorder lock sound and partial tools stay visible.
- Modal `Esc => AbortTurn` reuses the existing `Stop` path instead of inventing a second cancel channel; `Esc` never silently dismisses (ADR-0003 Q7 holds).

## Consequences

- Submit must store the chat `JoinHandle` (+ watcher handle) on `App` so `Esc`/`Ctrl+D`-while-busy can abort; turn drain must accept a synthesized `interrupted` `TurnResult`.
- Footer hint `esc×2 quit` (`src/tui.rs:1506-1530`) must be rewritten to the new map (`esc interrupt · ^D quit`); status/input `esc to interrupt` strings stay and become true.
- `handle_ctrl` gains a `d` arm with the empty-check + abort-then-quit branch; `handle_key`/`handle_selected_key` `Esc` arms lose the `last_esc` double-press quit and gain the busy-first precedence.
- Tests: `Ctrl+C`-busy-empty does not clear `busy`; `Ctrl+D` non-empty no-ops; `Ctrl+D` empty quits; `Esc` busy aborts + marks interrupted; `Esc` modal resolves `AbortTurn`; double-`Esc` no longer quits.
