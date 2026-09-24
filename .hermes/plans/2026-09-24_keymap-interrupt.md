# Keymap Interrupt (Esc abort / Ctrl+D quit / Ctrl+C clear) — Implementation Plan

> **For implementers:** Execute task-by-task via fresh subagents. No execution in this plan.

**Goal:** Make `Esc` interrupt a busy turn (abort worker, keep partial), remove double-`Esc` quit, make `Ctrl+D` quit-when-empty (abort-then-quit from busy/modal), and lock `Ctrl+C` to clear-only (ignored in modal).

**Architecture:** Keep Rig + Tokio agent loop + std `mpsc` TurnResult channel; store the turn `tokio::task::JoinHandle` in `App`, abort it on `Esc`-busy / `Ctrl+D`-busy; synthesize the interrupted trailer locally instead of waiting for the channel; centralize `Esc`/`Ctrl+D`/`Ctrl+C` routing in `handle_key` / `handle_selected_key` / `handle_ctrl`.

**Tech Stack:** Rust, Ratatui 0.29 (crossterm backend), Tokio spawn + 80ms watcher poll, std mpsc TurnResult/ThinkMsg channels.

---

## Current context / assumptions (read-only probe, verified)

- `src/tui.rs:61-67` `TurnResult{result, events, reasoning}`; `~70-76` `ThinkMsg{name,preview,ok,summary}`.
- `src/tui.rs:222-256` `struct App{busy, busy_since, status, selected, last_esc, pending_approvals: VecDeque, ...}` — no task handle stored.
- `src/tui.rs:~285-310` `App::new()` boot line: `rem — /quit or esc×2 exits, /clear clears, tab selects blocks.`
- `src/tui.rs:743-830` `submit()`: guards `empty||busy`, pushes user block, sets `busy=true/busy_since/pinned`, snapshots `seen=len`, `tokio::spawn` outer task + nested `watcher` polling `last_tool_events()` every 80ms → `think_tx`; on done `watcher.abort()` + `tx.send(TurnResult)`.
- `src/tui.rs:461-538` `event_loop`: `try_recv` think → `push_live_tool`, approval → queue, turn → `busy=false; turns+=1; push_turn`; renders when `dirty||busy||!pending.empty`.
- `src/tui.rs:545-645` `handle_key`: modal-first (`y/a/n/x/Enter`, `CONTROL` ignored, `Esc` currently never dismisses — superseded by locked spec item 2: modal `Esc`→`AbortTurn`), then `CONTROL→handle_ctrl`, then `selected→handle_selected_key`, then `Enter=submit`, `Tab` select, history/scroll, `Esc` double-`<600ms` via `last_esc` → quit else `input.clear()`.
- `src/tui.rs:~653-700` `handle_selected_key`: `Enter/Space` toggle, `Up/Down/Tab` move, `Esc` double-quit else `selected=None`, printable drops selection into input.
- `src/tui.rs:~701-730` `handle_ctrl`: `Ctrl+C`/`Ctrl+U` clear input, `Ctrl+W` delete-word; no `Ctrl+D` arm.
- `src/tui.rs:~1475-1495` `render_status` busy already says `working · {s} · esc to interrupt`; `~1500-1520` `render_input` busy placeholder `… (esc to interrupt)`.
- `src/tui.rs:~1525-1555` `render_footer`: idle `tab selects block · esc×2 quit`, selected `enter/space expand · esc input`.
- `src/tui.rs:1560-1946` tests: TestBackend chrome/placeholder/busy-status/modal/click, `StubAgent`, `approval_req` helper.

## Locked spec (user-confirmed)

1. `Esc`-busy aborts worker + watcher, `busy=false`, keep partial blocks + `interrupted` marker.
2. `Esc` matrix: busy→abort worker, modal→same as `x`/`AbortTurn` → `ToolCallAction::stop` (per ADR-0006 + docs/adr/0003-interrupt-resume.md; ADR-0003 Q7 "never dismisses" = never silently dismiss, resolves AbortTurn), selected→deselect, idle→clear input; delete double-`Esc` quit everywhere.
3. `Ctrl+D` quits only when input empty; from busy/modal it aborts first, then quits.
4. `Ctrl+C` is strict clear-only; ignored while modal is open.
5. Hints + tests updated (busy hint, modal hints, quit paths).
6. Glossary terms updated if stale.

## Open / plan-time discovery (delegated, NOT assumed)

- D1: Outer-handle abort kills nested watcher cleanly, or must both handles be stored? (watcher is child of outer spawn — confirm abort propagation vs explicit second handle.)
- D2: Late `TurnResult` race after abort: does aborted outer task guarantee no send, or is a `turn_id`/generation guard needed in `event_loop` drain?
- D3: `Ctrl+D`-in-modal HOW (quit-after-abort is locked; only the mechanism is open): resolve head approval as `AbortTurn` then quit, vs abort worker directly then quit? Confirm against `resolve_approval` + parked-worker behavior.

---

## Step-by-step plan (delegate each task, 2-5 min each)

### Task 1: Abortable turn handle + Esc-busy interrupt path
**Objective:** `Esc` while busy aborts the turn, keeps partial output, marks interrupted.
**Files:** Modify: `src/tui.rs:1-30` (tokio task import), `src/tui.rs:222-256` (`App` + new field), `src/tui.rs:743-830` (`submit`), `src/tui.rs:461-538` (`event_loop` drain); Read: `src/agent.rs` `ToolEvent`/`arg_preview`.
**Steps:**
1. Add `current_turn: Option<tokio::task::JoinHandle<()>>` (or `turn_id: u64` if D2 needs it) to `App`; init `None` in `App::new`, clear on normal turn completion in `event_loop`.
2. In `submit`, store outer `tokio::spawn` handle in `app.current_turn`.
3. Add `abort_turn(app)` helper: `take()` handle → `.abort()`, `busy=false`, `status=Ready`, `remove_live_row()` then re-push kept partial as final `ToolCall` blocks (or keep `LiveTools` row converted), push `System{"interrupted."}` / `Error` marker + `Trailer{"Interrupted (N tools)"}`, `pinned=true`, `dirty=true`.
4. Wire `Esc`-busy to `abort_turn` (full routing lands in Task 2; this task provides the helper + unit path).
5. Answer D1/D2 in code comments: document watcher-abort propagation + stale-result guard choice.
**Verify:** `cargo check`; manual: `cargo run` → long `bash` turn → `Esc` stops spinner, partial tools remain + interrupted marker, new prompt submittable. **Commit:** `feat(tui): abortable turn with Esc interrupt`.

### Task 2: Esc routing matrix + remove double-Esc quit
**Objective:** Single-`Esc` semantics everywhere; no quit on `Esc`.
**Files:** Modify: `src/tui.rs:545-700` (`handle_key`, `handle_selected_key`, `last_esc` field + `App::new` init); Read: `src/tui.rs:~285-310` boot text (note only, edit in Task 5).
**Steps:**
1. Delete `last_esc` field + both double-`<600ms → return true` branches (idle + selected).
2. Implement matrix at top of `handle_key`: modal pending → resolve head approval as `AbortTurn` (same as `x` → `ToolCallAction::stop`; modal wins over busy since worker is parked on oneshot); `else if busy → abort_turn + return false`; `else if selected → selected=None`; `else → input.clear()+cursor=0`.
3. Mirror in `handle_selected_key`: `Esc → selected=None` (or `abort_turn` if busy+selected — busy wins), never quits.
4. In modal guard, route `Esc` to `reply.send(AbortTurn)` (same path as `x`); comment that ADR-0003 Q7 "never dismisses" means never silently dismiss — Esc resolves AbortTurn via the existing Stop path.
**Verify:** `cargo check && cargo test`; manual: idle `Esc` clears input only, selected `Esc` deselects, busy `Esc` aborts, modal `Esc` aborts turn via AbortTurn (same as `x`), no `Esc` sequence quits. **Commit:** `refactor(tui): rework Esc routing, drop double-Esc quit`.

### Task 3: Ctrl+D quit-when-empty incl. abort-then-quit
**Objective:** `Ctrl+D` with empty input quits from any state; aborts first when busy/modal.
**Files:** Modify: `src/tui.rs:545-600` (modal `CONTROL` guard), `src/tui.rs:~701-730` (`handle_ctrl` — needs `&mut` access to turn handle + approval queue, or route via `handle_key`); Read: `src/tui.rs:250-280` `resolve_approval`.
**Steps:**
1. Add `Ctrl+D` arm: non-empty input → `return false` (no-op, never deletes); empty input → if `busy` → `abort_turn`; if modal non-empty → abort-then-quit per locked spec (D3: HOW — resolve head as `AbortTurn` vs abort worker directly; document choice) then `return true`; else `return true`.
2. Exempt `Ctrl+D` from the modal `CONTROL → return false` blanket ignore so it still quits.
3. Cover selected-focus too: `Ctrl+D` should work from `handle_selected_key` path (route through same helper before selection commands win).
**Verify:** `cargo check && cargo test`; manual: empty input `Ctrl+D` quits idle/selected/busy/modal; non-empty `Ctrl+D` does nothing; busy `Ctrl+D` leaves interrupted marker then exits. **Commit:** `feat(tui): Ctrl+D quit-when-empty with abort-then-quit`.

### Task 4: Ctrl+C strict clear-only + modal ignore
**Objective:** `Ctrl+C` never quits/aborts; does nothing while modal is open.
**Files:** Modify: `src/tui.rs:~701-730` (`handle_ctrl`), `src/tui.rs:545-600` (modal guard — keep); Tests: `src/tui.rs:1560-1946`.
**Steps:**
1. Lock `Char('c')` arm to `input.clear(); cursor=0; return false` only; assert no `busy`/handle/modal interaction.
2. Keep modal `CONTROL → return false` early return (covers `Ctrl+C`/`Ctrl+U`/`Ctrl+W` ignore); add comment.
3. Add unit tests: idle `Ctrl+C` clears input; busy `Ctrl+C` clears input but `busy` stays true and handle kept; modal `Ctrl+C` leaves `pending_approvals` + input untouched.
**Verify:** `cargo test` (new + existing modal tests green). **Commit:** `fix(tui): Ctrl+C strict clear-only, ignored in modal`.

### Task 5: Status/footer hint text + keymap tests
**Objective:** No stale `esc×2`/`ctrl+d` strings; hints teach the new map; regression tests lock it.
**Files:** Modify: `src/tui.rs:~285-310` (`App::new` boot text), `src/tui.rs:~1475-1495` (`render_status`), `src/tui.rs:~1500-1555` (`render_input`, `render_footer`), `src/tui.rs:1560-1946` (tests).
**Steps:**
1. Boot text → e.g. `rem — /quit or ctrl+d exits, esc interrupts, /clear clears, tab selects blocks.`
2. Footer idle → `tab selects block · esc interrupt · ctrl+d quit`; selected → `enter/space expand · esc input`; keep busy status `esc to interrupt` + input placeholder as-is (assert, don't regress).
3. Modal status line keeps `y approve · n deny · x abort`; add `ctrl+d quit` suffix if Task 3 implements it.
4. Tests: headless TestBackend footer assertions (idle/selected/busy), `Esc`-matrix unit tests (idle-clear/selected-deselect/busy-abort/modal-abort-via-AbortTurn, no-quit), `Ctrl+D` empty/non-empty/busy/modal cases, old-chrome negative asserts (`esc×2` gone).
**Verify:** `cargo check && cargo test`; headless render contains new hints, zero `esc×2` occurrences in frame text. **Commit:** `feat(tui): update keymap hints and tests for interrupt model`.

### Task 6: Glossary term updates
**Objective:** Docs glossary matches the new keymap; no stale quit/interrupt terms.
**Files:** Search-then-modify: `grep -rn "esc×2\|esc x2\|double.*esc\|last_esc\|Ctrl+D\|Ctrl+C\|interrupt" --include="*.md" .` (likely `docs/` / `.hermes/`); Read-only first, edit only hits.
**Steps:**
1. Subagent greps markdown for `esc`, `quit`, `interrupt`, `abort`, `ctrl` terms; reports file:line table.
2. Update stale entries: double-`Esc` quit → removed; `Esc` → interrupt/deselect/clear per state; `Ctrl+D` → quit-when-empty; `Ctrl+C` → clear-only.
3. If no glossary file exists, report NO-OP with search evidence instead of inventing one.
**Verify:** `grep` re-run shows zero stale `esc×2` references; `cargo check` untouched (docs-only). **Commit:** `docs: update keymap glossary for interrupt model` (or none if NO-OP).

## Tests / validation
- `cargo check` after Tasks 1, 2; `cargo test` after Tasks 3, 4, 5 (existing modal/chrome/click tests must stay green).
- Manual: `cargo run` → (a) long turn + `Esc` aborts with partial + marker, (b) `Esc` idle/selected never quits, modal `Esc` aborts turn via AbortTurn (same as `x`), (c) empty `Ctrl+D` quits everywhere, non-empty no-op, (d) `Ctrl+C` clears only, modal no-op.

## Risks / tradeoffs
- Aborting the outer Tokio task leaves Rig `Chat` futures cancelled mid-flight — partial `ToolEvent`s already recorded stay; unflushed reasoning may be lost (accepted: keep-partial scope is tools+marker).
- Stale `TurnResult` arriving post-abort would resurrect `busy=false→push_turn`; Task 1 must prove abort-cancels-send or add a generation guard.
- `Ctrl+D` in modal conflates quit with the parked worker: Task 3 must not strand the oneshot — prefer `AbortTurn` decision before exiting.
- Touching `handle_key` routing risks breaking `Enter`=submit + history `Up/Down` — Tasks 2/3 must preserve those paths with explicit tests.
