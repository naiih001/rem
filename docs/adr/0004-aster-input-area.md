# ADR-0004: Aster-style input area (shaded composer band + status row + footer)

Status: Accepted
Date: 2026-09-24
Context: rem's input area was a single bare line (`› ` + placeholder on the
terminal background) with a separate two-sided footer row (left hints,
right-aligned busy/ready status). The Aster reference (Zfinix/aster) uses a
composer bottom pane: gap row, busy status row, shaded input band with
padding, and a quiet footer. The original decision kept input single-line;
the composer now supports wrapped multi-line editing.

## Decision

- The bottom-anchored pane grows with the composer: gap(1), status(1),
  shaded input band(3–7), and footer(1). A single-line composer uses a
  2-row band while the slash menu is open. The status row is blank while
  idle; the slash menu can reclaim its space.
- `render_input` keeps the Aster shaded band (`PANE_BG` `#191919`, Aster
  `pane_bg`) and grows it with the text, retaining one row of vertical
  padding. It wraps at the available display width and shows up to five
  visual text rows; additional content scrolls internally with `↑` / `↓`
  cues for hidden rows. The `❯ ` prompt remains bold in the warm orange
  accent (`ACCENT` `Rgb(242,118,79)`); continuation rows align with the text.
- `Shift+Enter` inserts a newline on terminals supporting enhanced keyboard
  reporting; `Ctrl+O` is the portable fallback. `Enter` submits. Up/Down move
  the caret between visual rows, then recall prompt history when pressed
  beyond the first/last row. Home/End move to the current visual row's edges.
- Empty + idle: `Message rem…  (/ for commands)`. Empty + busy:
  `…  (esc to interrupt)`.
- `render_status` (new, terminal background above the band):
  - Busy: braille spinner (`SPINNER`, same 10-frame set Aster uses) +
    `working · {secs}s · esc to interrupt`.
  - Approval queued: `◌ waiting approval (N queued) · y approve · n deny · x abort`.
  - Idle: blank row; the slash menu may reclaim it.
- `render_footer` is one left-aligned line with the active mode, model,
  effort, and `Enter` / newline hints. Busy state lives in the status row.
- Cursor position follows the wrapped row and internal scroll offset. Text
  keeps the 1-column inset + 2-column `❯ ` prompt, so max width is
  `term_width - 4`; the pane viewport resizes while remaining bottom-anchored.
- Click-to-toggle body mapping comment updated (body rows are
  `1..1+view_h`); behavior unchanged.

## Rationale

- Preserves the Aster composer anatomy while adding wrapped multi-line
  editing without changing the terminal-owned transcript scrollback.
- A blank idle status and menu-space reclamation keep the pane compact when
  the composer is short.
- Footer stops duplicating busy state, so there is exactly one place to look
  during a turn (the status row).

## Consequences

- Tests: headless `TestBackend` regression tests pin the chrome and dynamic
  pane resizing; editor tests cover wrapping, Unicode widths, caret movement,
  overflow cues, history, and submission. Existing tests pin:
  band shading rows + prompt/typed text + caret math + footer content;
  idle placeholder vs busy status + busy hint; approval status row above a
  still-shaded band.
- Deferred (not this ADR): `@` mentions and token/cost counters in the
  footer (Aster shows `↑/↓` + `$`; rem has no usage plumbing). Slash-menu
  behavior is specified in ADR-0007.
- `cargo fmt --check` still reports pre-existing diffs elsewhere in
  `tui.rs`; the regions touched here are fmt-clean. The one clippy
  `bool_comparison` hit in tests predates this change.
