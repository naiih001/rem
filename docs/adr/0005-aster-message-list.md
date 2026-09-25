# ADR-0005: Aster-style message list (scrollback transcript)

Status: Accepted
Date: 2026-09-24
Context: rem's transcript is a managed alternate-screen block list
(`App.blocks`: User / Reply / ToolCall / Thinking / LiveTools / Trailer)
with in-app scroll, Tab selection, Enter/Space expand-collapse, and
click-toggle. The Aster reference (Zfinix/aster, `crates/aster-cli/src/tui/history.rs`,
verified) renders finished rows into the terminal's own scrollback and
manages only the bottom pane. Grill scope (user-selected): full transcript
parity — bullets, tool + diff blocks, Done trailer — with thinking hidden,
elision, and duration-first trailer. No review rows (no engine exists).

## Decision

- **Scrollback model.** Finished rows print into the terminal's scrollback
  and are never touched again; only the bottom pane (gap + status + input
  band + footer, ADR-0004) stays managed. Dropped with the block list: Tab
  selection, expand/collapse, click-toggle, in-app scroll (Shift+Up/Down,
  PgUp/PgDn, End snap). The terminal owns scroll, selection, and copy.
- **User row = chapter mark, not a bullet.** Filled band (`rail_bg`
  background) with accent left rail (`▌`) and `❯ ` prompt, per Aster
  `history::user`. The only banded row in the transcript.
- **Everything else = `•` bullets with hanging indent.** Dim bullet glyph,
  2-column gutter, continuation lines indent under the bullet (Aster
  `hang()` + `bullet()`). Agent text, notices, tool rows, trailer.
- **Assistant replies use rendered Markdown.** Parse reply bodies as
  Markdown and render headings, nested ordered/unordered lists, inline
  emphasis and code, links as `label (URL)`, fenced code blocks, blockquotes,
  and tables. Code blocks preserve their lines with indentation and a subtle
  background; table columns fit the available width and wrap cell contents.
  User prompts and tool output remain plain text.
- **Flat tool rows, one group per tool.** Bold label line, then dim nested
  sub-rows with `└` branch on the first row (Aster `history::tool` +
  `branch()`). No step grouping; no findings/review rows.
- **Patch rows.** `▸ verb path` header with `+N −M` counts pushed right
  (green/red), then a tinted patch body: full-row add/del background bands
  with a darker `+`/`-` mark glyph (Aster `history::patch` +
  `diff_lines()`).
- **Aster elision, no expand.** Long tool output renders first 4 + last 4
  lines with a gap marker (Aster `elide`, HEAD/TAIL = 4). There is no
  expand in scrollback; the tail is always visible.
- **Thinking hidden entirely.** Reasoning stays recorded internally; no
  transcript row, not even a one-line marker. (Aster shows a collapsed
  hint row; this slice hides it per grill answer.)
- **Stream live.** Each resolved tool prints its bullet rows into the
  scrollback immediately; the status spinner covers mid-turn liveliness.
- **Trailer.** `• Done ({elapsed}s · N tools)` bullet row. `busy_since`
  already exists so duration is trivial. File counts, `+/-` stats, and
  cost are later slices with their own plumbing.

## Rationale

- Verified against Aster's real `history.rs`: `hang`/`bullet`/`branch`
  structure, `user` band + rail, `tool` label + sub-rows, `patch` counts +
  `diff_lines` tinted bands, `elide` HEAD/TAIL = 4. Copying the anatomy,
  not guessing from the screenshot.
- Elision over full-print: matches Aster and bounds scrollback growth;
  the tail answers "how did it end" without interaction.
- Hiding thinking keeps the transcript a record of what was said and
  done, not internal deliberation; a marker row would add noise with no
  action behind it.
- Parsing assistant replies as Markdown avoids line-prefix guesses and
  preserves block and inline semantics without changing prompt or tool rows.
- Duration-first trailer ships value with zero new plumbing; file/cost
  stats each need data work that deserves its own slice.

## Consequences

- The managed-block interaction model (selection, expand, click, scroll
  state: `selected`, `scroll`, `pinned`, `view_h`, `rendered_total`,
  `click_toggle`, `ensure_selected_visible`) is deleted, not adapted.
- Tests: headless `TestBackend` row tests pinning bullet rows, hanging
  indent, user band + rail, patch counts + tinted bands, elision gap
  marker, trailer text. `cargo test` green; new-code regions fmt-clean.
- Deferred: review/findings engine + rows, multi-line composer, `@`
  mentions, `/` menu, file-stat and cost/token plumbing, token counters.
- `cargo fmt --check` still reports pre-existing diffs in `tui.rs`; the
  regions touched here must be fmt-clean.
