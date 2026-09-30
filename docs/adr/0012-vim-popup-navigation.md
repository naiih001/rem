# ADR-0012: Vim navigation for popups

Status: Proposed
Date: 2026-09-28

## Decision

Popup navigation supports both Vim-style keys and existing arrow keys:

- `j` / `Down`: next item
- `k` / `Up`: previous item
- `Ctrl+d`: move down half a visible page
- `Ctrl+u`: move up half a visible page
- `/`: enter inline filter mode
- `Esc`: leave filter mode first; then dismiss only if the popup policy allows it

`g` and `G` are explicitly excluded. `q` is reserved for popup-specific quit/abort semantics and is not a universal dismiss key.

## Filtering

Filtering is rendered inline inside the popup. While filter mode is active, typed characters edit the filter, `Enter` accepts the current filter/selection, and `Esc` exits filter mode without dismissing the popup. Selection remains valid when results change and resets to the first matching item when necessary.

## Consequences

- Existing arrow-key users retain their workflow.
- Page movement is predictable and bounded by the visible content window.
- Approval remains a blocking FIFO: navigation does not expose later requests or reorder the queue.
- Approval action selection uses `h/l` and Left/Right; Enter confirms the selected action and Esc aborts immediately.
- Each popup can override action keys while reusing navigation and filter behavior.

## Visual interaction contract

- The filter row is always visible in list popups, even before filtering begins.
- The popup spans the terminal width and covers the composer; the composer is hidden and cannot receive input until the popup closes.
- Resume uses left-aligned tabular rows: title, project, updated time, and id.
- Approval prioritizes reason/risk before the requested command and uses explicit risk cues. Risk colors supplement labels and are never the only signal.
