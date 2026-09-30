# ADR-0008: Cycling busy-status verbs (Claude Code / Aster style)

Status: Accepted
Date: 2026-09-26
Context: The busy status row (ADR-0004) always read `working · {s}s · esc to
interrupt`. Claude Code and Aster rotate the leading verb through a set of
playful gerunds while a task runs, which makes long waits feel alive.
Grill scope (user-selected): 10-word playful list, sequential rotation every
~2s restarting at `working` each task, styling and position unchanged
(DarkGray, same row), code + tests + docs.

## Decision

- New `BUSY_VERBS` constant in `src/tui.rs`: `working, thinking, cooking,
  pondering, reasoning, crafting, brewing, scheming, conjuring, noodling`
  (10 words, index 0 is `working`).
- New `BUSY_VERB_SECS: u64 = 2` rotation period.
- `render_status` derives the verb from busy-elapsed time:
  `BUSY_VERBS[(elapsed.as_secs() / BUSY_VERB_SECS) % BUSY_VERBS.len()]`.
  This rides the existing 50ms frame loop that already animates the braille
  spinner, so no new timer or redraw plumbing was needed. Since
  `busy_since` resets per task, each task restarts at `working`.
- Row format is otherwise unchanged:
  `{spinner} {verb} · {s}s · esc to interrupt`.

## Rationale

- Time-derived index keeps the rotation stateless and deterministic —
  trivially testable by backdating `busy_since`, no RNG seeding or
  per-task shuffle state.
- Sequential order (not random) means the first paint of every task still
  reads `working`, preserving continuity with ADR-0004's spec and the two
  pre-existing tests that assert the literal string at elapsed ≈ 0.
- 2s period: slow enough to read, fast enough to notice across a typical
  multi-second agent turn.

## Consequences

- `test`s: extended `busy_status_row_shows_spinner_and_hint` to backdate
  `busy_since` by `BUSY_VERB_SECS` and assert the second verb appears —
  pins rotation without sleeping. The two pre-existing literal-`working`
  assertions still pass (elapsed ≈ 0 → index 0).
- Glossary "Status row" entry updated: busy now reads
  `{spinner} {verb} · {s}s · esc to interrupt` with the 10-verb cycle.
- Deferred: per-tool verbs (e.g. `searching` during grep), random/shuffled
  order, user-configurable word list.
