# ADR-0011: Hard-coded session permission modes

## Status

Accepted

## Decision

rem provides five hard-coded permission modes: `plan`, `manual`, `auto`, `edit`, and `yolo`. `Shift+Tab` cycles through them in that order and the selected mode is session-only. A change applies to the next user message; an in-progress run keeps its existing policy.

Read-only tools remain automatic. A prohibited call is denied with model-visible feedback so the model can replan. Sensitive files, network access, deletion, outside-project paths, and high-impact commands remain approval-controlled except in `yolo`. Annihilators and secret exfiltration remain unconditionally denied in every mode.

`plan` is read-only. `manual` asks for consequential calls. `auto` permits normal in-project work and safe development commands while prompting for high-impact operations. `edit` additionally trusts the normal development command set (`cargo test`, `cargo build`, `cargo fmt`, and `git diff`). `yolo` bypasses approvals but retains the unconditional safety floor.

The active mode is shown in the footer and is not stored in persisted sessions.

## Rationale

Modes are policy presets rather than prompt personas. Keeping them hard-coded makes their safety behavior auditable and avoids configuration silently widening permissions.
