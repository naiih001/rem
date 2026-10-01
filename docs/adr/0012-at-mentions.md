# ADR-0012: `@` file mentions in inputs (Claude-style)

Status: Accepted
Date: 2026-09-30
Context: `rem` has no `@` handling today — only deferred mentions in ADR-0004
("`@` mentions and token/cost counters" deferred) and ADR-0005 ("`@`
mentions" deferred). Claude Code behavior (verified from Claude docs):
`@path` inlines full file content, `@dir` shows a listing not contents,
multiple refs per message, relative or absolute paths, `@` opens a path
suggestion menu where `Enter`/`Tab` accepts and a second `Enter` sends.
`rem` precedent to clone: the slash menu (ADR-0007 + ADR-0010) — popup above
the composer, fuzzy subsequence match (filename hits ranked first, then
path, then alpha), `Up`/`Down` move, `Tab`/`Enter` accept, `Esc`
dismisses, whitespace closes, `MENU_MAX_ROWS = 10` + `+N more`, gap
absorption. Tools already cover the read side (`read`, `list_directory`,
`glob`, `grep`). `rem` has no MCP, so no `@server:resource`. Grill resolved
20 questions; this ADR pins the result.

## Decision

- **Scope: files + directories.** `@file` inlines content; `@dir` inlines a
  listing. No MCP, no URLs, no symbols in v1.
- **Trigger (custom rule).** `@` starts a ref unless the char directly
  before it is an ASCII letter (`[A-Za-z]`). Start-of-line, whitespace,
  digits, and punctuation all trigger — `(@a.rs)`, `foo/@a.rs`, `hi,@a.rs`
  trigger; `user@example.com` stays literal. No `@@` escape in v1, so
  `@@foo` parses as a ref token under this rule.
- **Token shape.** Ref starts at a triggering `@`, runs to whitespace.
  Trim trailing `, . : ; ! ? ) ] }` so `@a.rs,` refs `a.rs`. No
  quoted-space paths (`@"my file.rs"`) in v1.
- **Root confinement.** Resolve every ref against the process cwd (the
  project root `main.rs` canonicalizes). Block absolute paths and any ref
  escaping the root — lexical `..`/`.` normalization plus canonical
  symlink-resolved check, same spirit as the mutate fast-path
  (`permissions.rs::inside_project`). Outside-root is a submit-time block,
  never silent.
- **Popup: clone of the `/` menu.** Same chrome/keys, mid-line, multiple
  times per input, fuzzy-matched on the active `@` partial (subsequence
  match, filename-first ranking). Source:
  filesystem walk showing project-relative paths, dirs with trailing `/`;
  respect `.gitignore`, skip `.git/` + `target/` (same skips as `GlobTool`);
  cap `MENU_MAX_ROWS = 10` + `+N more`. Fuzzy subsequence filter
  (filename > path > alpha ranking).
- **Activation + precedence.** Caret-aware: the popup follows the `@token`
  under the caret. The `/` command menu wins when the whole input is a
  `/command` line; otherwise the `@` menu owns keys when its token is
  active. `Enter`/`Tab` completes the active `@` token (never submits);
  a second `Enter` submits. `Up`/`Down` move selection (never history),
  `Esc` dismisses keeping typed text (never interrupts), any edit
  re-clamps the selection (mirrors `clamp_menu_sel`).
- **Completion.** Insert the project-relative path; dirs get a trailing `/`
  (e.g. `@src/` → `@src/utils/`). Caret lands after the completed token.
- **Submit expansion (display-vs-send split, like `/init`).** Transcript
  user row, input history, and title fallback keep the short original with
  `@` tokens. Only the model payload appends blocks, in `@` appearance
  order, after the original text:
  - file: `<path>:\n```\n<contents>\n``` `
  - dir: `<dir>/ (directory listing):\n<file:/dir: lines>`
- **Caps.** Per-file `Context::TOOL_BUDGET` (2000 chars) with a
  ` [truncated N chars]` note (same wording as `truncate_new`). Dir
  listings: non-recursive, sorted, `file:`/`dir:` lines, 500-entry cap —
  exactly `list_directory` parity.
- **`@dir` shape.** Non-recursive only. Recursive trees are a later slice.
- **Sessions persistence.** `messages_json` stores the expanded prompt
  (what the model saw). Resume/replay shows frozen contents.
- **Input history.** `App::history` (`Up` recall) stores the original short
  `@` text, so recall re-edits refs cleanly instead of flooding the
  composer with KBs of blocks.
- **Slash lines.** Autocomplete everywhere, inline only in chat. The `@`
  popup also completes inside slash args (e.g. `/rename @...`), but no
  content is ever inlined there; `/init @...` still rewrites to the fixed
  `INIT_PROMPT` (user args dropped, unchanged).
- **Permissions.** `@` reads bypass the approval modal — explicit user
  intent to read, unlike agent-initiated tool calls. Only outside-root,
  missing, binary, or unreadable refs block. Sensitive paths (`.env`,
  keys) inline without prompting in v1 (same non-goal as secrets
  redaction). `@` never writes or executes; it only adds prompt context.
- **Bad refs block.** Missing/unreadable/binary/outside-root blocks submit
  with a notice naming the bad ref (same channel as `unknown command`
  notices). Warn-and-skip is rejected: a silently dropped ref would make
  the model answer a different question than the user asked.
- **`AGENTS.md`.** Root-only loading (ADR-0009) is unchanged. `@` adds
  only the referenced file/dir, never nearby `AGENTS.md` files.

## Rationale

- Cloning the `/` menu (trigger, layout, keys, overflow, tests) gives a
  proven popup with zero design risk; the only new work is caret-aware
  mid-line activation plus a filesystem row source instead of a registry.
- Inline-into-prompt (rather than leave-tokens-for-tools) guarantees the
  model sees the referenced content — the tool loop may never call `read`.
  Display-vs-send keeps transcript, history, and title fallback readable.
- `TOOL_BUDGET` / 500-entry caps reuse existing, tested bounds instead of
  inventing new ones; `list_directory` parity makes `@dir` output
  unsurprising.
- Block-on-bad (rather than warn-and-skip) preserves prompt fidelity: the
  stored session always matches what the user thought they sent.
- Store-expanded keeps resume faithful to what the model saw; re-expanding
  on resume would silently change history.
- Root confinement + explicit-intent bypass together: reads stay sandboxed
  to the project while never modal-spamming the user for files they named.

## Consequences

- `tui.rs`: new pure `@` token helpers (trigger scan, active-token-under-
  caret, punctuation trim), `@` row source (walk + fuzzy filter +
  `.gitignore`/`.git`/`target` skips), `@` menu state alongside `menu_sel`
  with `/`-wins precedence, key-ownership branch, Tab/Enter completion
  rewrite, submit-time expand + block path. Pane-height/menu-capacity math
  gains the `@` row count (same absorption as `/`).
- `agent.rs`/`context.rs`: no new tool; expansion happens in the TUI
  submit path before `agent.chat()`. `TOOL_BUDGET` wording reused.
- Tests: trigger rule (letter-before blocks, punctuation triggers, `@@`
  parses as ref, trailing-punctuation trim), token scan (multi-ref,
  ordering), confinement (absolute + `../` + symlink escape blocked),
  caps (2000-char truncate note, 500-entry dir cap), popup (open/filter/
  navigate/complete/dismiss, `/`-wins, Enter-completes-not-submits),
  submit (original in transcript + history, expanded in payload +
  sessions), slash-arg autocomplete without inlining, bad-ref block
  naming the ref.
- Docs: glossary gains `@` mention terms; ADR-0004/0005 deferred items
  shrink to token/cost + review rows.
- Deferred: quoted-space paths, fuzzy matching, recursive `@dir`, `@@`
  escape, MCP resources, per-directory `AGENTS.md`.

## P1/P2 Resolved (plan-time, 2026-09-30)

- **P1 binary detection:** NUL-byte scan of the first 8 KiB; binary if
  any `\0` present. Lossy-UTF8 without NUL is not binary-blocked.
- **P2 dir sort order:** `lines.sort()` byte order, identical to
  `list_directory`.

## Open Questions

- P3 Dir walk cost: cap walk to 5000 entries + fuzzy filter if slow on
  huge repos. Deferred until measured.
