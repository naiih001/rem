# ADR-0011: Unified bottom popup architecture

Status: Proposed
Date: 2026-09-28

## Context

`/resume` and permission approval are modal interactions, but currently have separate rendering, state, and input-routing paths. Resume is centered while approval is a bottom sheet. Future question and confirmation prompts will need the same visual language.

## Decision

- Add `src/popup.rs` with shared popup rendering primitives.
- The shared shell owns bottom anchoring, full terminal width, composer coverage, shaded-region styling, sizing, footer hints, selection styling, and inline filter presentation.
- A popup completely covers and owns the input area while open. The transcript remains visible above it.
- While a popup is open, the TUI expands its managed viewport to the terminal height and renders the popup as a centered modal. Closing the popup restores the compact inline composer viewport.
- The modal backdrop is a shaded terminal surface without a hard outer border. The content surface uses a centered maximum width with terminal margins.
- Popup content is left-aligned and may use horizontal columns. The shell remains agnostic about whether content is a list, decision prompt, or question form.
- Feature controllers retain domain state and actions. The primitives do not own session loading, approval channels, or prompt semantics.
- `/resume` is migrated first. Permission approval is migrated in a separate commit.
- The shell must support list popups now and leave room for future question/confirmation content.

## Interaction policy

Each popup declares its own dismissal and emergency-key policy. `q` is not a generic close key; it remains reserved for quit/abort semantics. Approval remains non-dismissible and FIFO.

## Consequences

- Resume and approval look and feel consistent while retaining different domain behavior.
- Centered popup rendering is removed for resume.
- Resume may use a taller list layout; approval may use a shorter risk-focused layout.
- Approval presents a horizontal action bar with `Allow once`, `Allow always`, `Reject`, and `Abort`; the selected action uses the active theme accent.
- Small terminals require bounded content and explicit empty/overflow states.
- Shared rendering tests can cover shell behavior once instead of duplicating visual logic.
