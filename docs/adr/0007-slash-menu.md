# ADR-0007: Slash-command menu palette (Aster-style)

Status: Proposed
Date: 2026-09-24
Context: rem has slash commands but no popup today: `Tab`=block-select, `Up/Down`=history, `Enter`=submit; `submit()` routes `/quit` / `/clear` / `/unknown` (`src/tui.rs:882-895`). The input placeholder already says `(/ for commands)` but typing `/` shows nothing. Goal is an Aster-style palette (screenshot: two-column command + description, highlight-selected, `+N more`) cloned for rem's commands. Reference mechanism (already fetched): Aster `BottomPane` `command_matches` / `menu_lines` / `composer_key` + `CHAT_COMMANDS` registry. Menu must integrate with `handle_key` precedence + the ADR-0006 `Esc` / `Ctrl+D` / `Ctrl+C` matrix.

## Decision

- Commands registry (Aster `CommandDesc`-style: `name`, `takes_arg`, `desc`):
  - `/quit` + `/clear` gain descriptions; NEW `/help` lists commands from the registry.
  - Registry is the single source for menu rows and `/help` output.
- Trigger + filter + layout (clone Aster):
  - Menu opens on `/` prefix; prefix-filters on first token only; any whitespace closes menu.
  - Cap 10 rows + `+N more` overflow line, windowed around the selected command when the pane cannot show every match.
  - Two columns: command + description; selected row has marker/highlight.
  - Renders directly above the composer. In rem's bottom-anchored pane, the menu uses the idle gap/status space and composer's vertical padding; while busy, the status remains visible and the menu uses fewer rows. The composer and caret stay visible as the pane grows with multi-line input. If matches exceed the available menu rows, the selected window is shown with `+N more`.
- Menu owns keys when open:
  - `Up/Down` move selection (not history), `Tab` completes selection into input (not block-select), `Enter` runs highlighted command (not raw submit), `Shift+Enter` (enhanced terminals) or `Ctrl+O` inserts a newline, and `Esc` dismisses menu (NOT interrupt).
  - Busy+menu-open `Esc` precedence (dismiss-first vs interrupt-first) is explicitly LEFT OPEN as a plan-time D-question (see below); menu-open `Esc` never interrupts on its own.

## Rationale

- Cloning Aster's filter/layout gives a proven palette with zero design risk: prefix-match, whitespace-dismiss, 10-row cap, description column.
- Registry-first means `/help` and menu cannot drift; `takes_arg` reserves future `/cmd <arg>` shape without committing to it.
- Menu-owns-keys-when-open is the only way to reuse `Up/Down`/`Tab`/`Enter` without breaking their idle bindings; `Esc`-dismisses (not interrupt) preserves ADR-0006's sole-interrupt meaning by scoping `Esc` to the topmost layer.

## Consequences

- `handle_key` needs a menu-open branch ahead of history/block-select/submit dispatch; `submit()` slash routing stays but runs post-completion.
- The bottom-anchored viewport grows with the composer; menu rows are fitted into the remaining pane space instead of covering the composer.
- The placeholder keeps `(/ for commands)`; the footer advertises multiline keys. `Up/Down` history and `Tab` block-select are shadowed only while menu is open.
- ADR-0006 keymap work must account for the menu layer: `Ctrl+C` (clear-only) and `Ctrl+D` (quit-on-empty) behavior while menu-open falls out of the `Esc`-precedence decision.
- Tests: `/` opens without hiding the composer or caret in the bottom-anchored pane, prefix filters, whitespace closes, 10-cap + `+N more`, `Up/Down`/`Tab`/`Enter`/`Esc` ownership, `/help` lists registry, `/unknown` path unchanged.

## Open Questions (plan-time)

- D1 registry location: where does the name+desc registry live (TUI vs shared commands module) so menu + `/help` + `submit()` share it?
- D2 `menu_sel` state: field shape + reset rules (on edit, on dismiss, on submit, on busy change)?
- D3 exact render slot above composer in shaded band (which `tui.rs` composer block owns it)?
- D4 mouse click/scroll: Aster has it; what is rem's mouse-capture status — include or explicitly defer?
- D5 `/help` output format (system block vs chat block vs transient menu text)?
- D6 exact `Esc` precedence when busy+menu-open: dismiss-first (then second `Esc` interrupts per ADR-0006) or interrupt-first? Must reconcile with ADR-0006 `Esc`-sole-interrupt + modal precedence.
