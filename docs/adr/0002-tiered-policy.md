# ADR-0002: Tiered default policy (read auto, mutate/command conditional, delete/network confirm, annihilators deny)

Status: Accepted
Date: 2026-09-24

## Decision

Pure classifier `classify(tool_name, args_json) -> Allow | Confirm | Deny` in `src/permissions.rs`:

| Class | Members | Default |
|---|---|---|
| Read | `read`, `list_directory`, `glob`, `grep`, `git_status`, `git_diff` | `Allow` |
| Mutate | `write`, `edit` | `Confirm`; auto fast-path only if ALL hold: path inside project root, not sensitive (`.env`, `*.pem`, `*secret*`, `*credential*`, `~/.ssh`), size delta small (proposed < 32KB), and no session deny rule |
| Execute | `bash` | pattern match (below) |
| Network-in-tool | `web_fetch`, `web_search` | `Confirm` per user spec (noise risk, see Q4) |
| Network-via-bash | `curl wget ssh scp nc telnet ftp pip npm cargo apt brew docker` inside `bash` | `Confirm` minimum |
| Delete-via-bash | `rm rmdir unlink shred`, `mv X /tmp|/dev/null`, `> file`, `dd`, `mkfs`, `chmod -R 777 /`, fork bomb `:(){:|:&};:` | `Confirm` minimum; annihilators `Deny` |
| Annihilators (`Deny`, never prompt) | `rm -rf /`, `rm -rf /*`, `rm -rf ~`, `rm -rf $HOME`, `mkfs.* /dev/`, `dd ... of=/dev/`, `:(){:|:&};:` | `Deny` with feedback |

Bash matching is syntactic on `sh -c` text: strip `sudo`, split on `; && || | & $( )` `` ` ``, then match verbs. Documented as heuristic, not a sandbox.

## Rationale

- Matches your stated tiers exactly, plus the missing `Deny` tier your `rm -rf /` example demands. Prompting on an annihilator still lets a tired human approve catastrophe; deny removes the option.
- No dedicated delete tool exists, so delete policy must live inside bash classification.

## Consequences

- False positives on bash (e.g. `echo "rm -rf /"` in a string) are reduced by stripping quoted spans before matching; residual over-prompting fails safe toward Confirm.
- `web_search` on Confirm may be noisy on research tasks; kept per user spec. Session `always` rules mitigate repeat prompts.

## Resolved defaults (grill Q1-Q4, no user reply; recommended values applied)

- Q1 edit/write fast-path: kept. Auto only when in-project + non-sensitive + under 32 KiB.
- Q2 annihilators: hard deny, never prompt.
- Q3 bash safe-list: `ls echo printf cat head tail wc pwd true uname date whoami which file stat basename dirname` + read-only `git status/diff/log/show/branch/remote/stash/tag`. Everything else Confirm minimum.
- Q4 network: Confirm for both `web_search` and `web_fetch` per user spec.
