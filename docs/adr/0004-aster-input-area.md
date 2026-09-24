# ADR-0004: Aster-style input area (shaded composer band + status row + footer)

Status: Accepted
Date: 2026-09-24
Context: rem's input area was a single bare line (`› ` + placeholder on the
terminal background) with a separate two-sided footer row (left hints,
right-aligned busy/ready status). The Aster reference (Zfinix/aster) uses a
composer bottom pane: gap row, busy status row, shaded input band with
padding, and a quiet single-line footer. Grill scope (user-selected):
Aster look, single-line editing kept as-is (no multi-line composer).

## Decision

- Layout is now header(1) / body / gap(1) / status(0 idle, 1 busy-or-approval)
  / input band(3) / footer(1). The status row costs zero rows when idle.
- `render_input` draws a 3-row shaded band (`PANE_BG` `#191919`, Aster
  `pane_bg`) with 1-row vertical padding: `❯ ` prompt in warm orange accent
  (`ACCENT` `Rgb(242,118,79)`, Aster accent) + bold, typed text, or an
  italic faint placeholder (`PLACEHOLDER` `#4d4d4d`).
  - Empty + idle: `Message rem…  (/ for commands)`.
  - Empty + busy: `…  (esc to interrupt)`.
- `render_status` (new, terminal background above the band):
  - Busy: braille spinner (`SPINNER`, same 10-frame set Aster uses) +
    `working · {secs}s · esc to interrupt`.
  - Approval queued: `◌ waiting approval (N queued) · y approve · n deny · x abort`.
  - Idle: zero-height, renders nothing.
- `render_footer` simplified to one left-aligned Aster-style line:
  `▶▶▶ edit · {model} · {N} turn(s) · tab selects block · esc×2 quit`
  (selection hint swaps to `enter/space expand · esc input` when a block is
  selected; `error — see log` in red appended on `Status::Error`). The old
  right-aligned `thinking…`/`ready` readout is gone; busy state lives in the
  status row.
- `cursor_x` / `visible_window` budget updated for the band geometry:
  1-column inset + 2-column `❯ ` prompt, so max width is `term_width - 4`
  and caret x is `3 + caret_col`. Hardware cursor placed on the band's
  middle row (`height - 3`); the middle row is invariant because the footer
  is always 1 row and the band always 3.
- Click-to-toggle body mapping comment updated (body rows are
  `1..1+view_h`); behavior unchanged.

## Rationale

- Matches the Aster reference's composer anatomy (gap, status, shaded band,
  footer) without taking on multi-line editing, mention menus, or slash-menu
  scope that was explicitly deferred by the grill answer.
- Zero-height idle status keeps every freed row in the messages area, which
  owns inline tool/thinking blocks.
- Footer stops duplicating busy state, so there is exactly one place to look
  during a turn (the status row).

## Consequences

- `test`s: 3 new headless `TestBackend` regression tests pin the chrome:
  band shading rows + prompt/typed text + caret math + footer content;
  idle placeholder vs busy status + busy hint; approval status row above a
  still-shaded band.
- Deferred (not this ADR): multi-line composer, `@` mentions, `/` slash
  menu, token/cost counters in the footer (Aster shows `↑/↓` + `$`; rem
  has no usage plumbing yet).
- `cargo fmt --check` still reports pre-existing diffs elsewhere in
  `tui.rs`; the regions touched here are fmt-clean. The one clippy
  `bool_comparison` hit in tests predates this change.
