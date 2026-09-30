# Popup test/spec matrix

| Area | Required behavior | First migration |
|---|---|---|
| Shell | Bottom anchored; full terminal width; covers composer; transcript remains visible | Resume |
| Shell | Shaded lower region without floating-card border | Both |
| Shell | Shared title, background, selection, footer, sizing | Resume |
| Shell | Popup-specific height while retaining generic shell | Both |
| Navigation | `j/k` and Up/Down move selection | Resume |
| Navigation | `Ctrl+d/u` move half a visible page | Resume |
| Navigation | `g/G` have no navigation behavior | Resume |
| Filter | `/` opens inline filter; matching rows update | Resume |
| Filter | Persistent filter row is visible before typing | Resume |
| Filter | Esc exits filter before any popup dismissal | Resume |
| Resume | Enter resumes selected session | Resume |
| Resume | Empty results render safely | Resume |
| Approval | Popup is blocking and FIFO | Permissions |
| Approval | Later approvals cannot be selected | Permissions |
| Approval | Existing approve/always/deny/abort actions remain unchanged | Permissions |
| Approval | Risk/reason appears before command details | Permissions |
| Approval | Risk labels and restrained color cues are visible | Permissions |
| Policy | `q` follows popup-specific quit/abort policy | Both |
| Layout | Long lists window/scroll within popup bounds | Both |
| Layout | Narrow/short terminals do not panic or hide all actions | Both |
| Regression | Composer restores after popup closes | Both |
