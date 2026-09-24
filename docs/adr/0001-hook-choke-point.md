# ADR-0001: Enforce permissions at Rig `on_tool_call` (single choke point)

Status: Accepted
Date: 2026-09-24
Context: rem's 11 tools execute with no gate. `BashTool` is documented "Unrestricted" and runs `sh -c` in the project dir.

## Decision

- Implemented `PermissionHook: AgentHook` in `src/permissions.rs`; `on_tool_call` classifies every call before execution.
- No per-tool `call()` changes for enforcement. Tools stay dumb. The hook is the only gate.
- Verdict mapping:
  - `Allow` -> `ToolCallAction::Run`
  - `Deny` -> `ToolCallAction::Skip(reason)` with model-visible feedback, no human prompt
  - `Confirm` -> park worker, prompt human, then `Run` or `Skip` on decision
  - User abort of whole turn -> `ToolCallAction::Stop(reason)`
- Recorder hook first in stack (observe-all), permission hook second (steer).

## Rationale

- Rig runs hooks in registration order and rewrites chain. One steering hook avoids N tool-side bypasses.
- `Skip` keeps the run alive so the model can replan in the same run. `Stop` kills the run and is wrong for deny.
- `on_tool_result` cannot prevent execution, so it is observation only.

## Consequences

- Human decisions arrive over tokio mpsc + oneshot (see ADR-0003).
- `on_tool_call` performs no I/O except the explicit approval wait. Classification is pure.
