# ADR-0013: MCP client support (stdio + streamable HTTP, via Rig `rmcp`)

Status: Accepted
Date: 2026-09-30

## Context

rem's agent loop (`RigAgent`, `src/agent.rs`) wires 11 static built-in tools
(`read`, `write`, `edit`, `bash`, `list_directory`, `git_status`, `git_diff`,
`grep`, `glob`, `web_fetch`, `web_search`) through Rig's `AgentBuilder::tool()`.
There is no way to reach external tools (issue trackers, databases, Figma,
Sentry, etc.) without pasting data into chat.

Rig 0.42 already ships native MCP client support behind its `rmcp` feature:
`ToolServer` / `ToolServerHandle`, `McpClientHandler::connect(transport)` with
automatic `notifications/tools/list_changed` refresh, and per-call timeouts
(`DEFAULT_MCP_TOOL_TIMEOUT`, 300s). rem pins `rig = "0.42"`, whose
`rig-agent` expects `rmcp` v2 (`features = ["client"]`), so rem must use the
rmcp version Rig re-exports — not a separately-pinned rmcp 3.x.

## Decision

- **Direction**: MCP **client only**. rem launches/connects to external MCP
  servers and exposes their tools to the agent loop. No MCP server surface
  (rem does not serve its own tools over MCP). Both directions explicitly
  deferred.
- **Transports (v1)**: `stdio` (local commands: `command` + `args` + `env`,
  via rmcp `transport-child-process`) and **streamable HTTP** (remote `url`,
  via rmcp `transport-streamable-http-client-reqwest` with rustls). Legacy SSE
  explicitly out of scope.
- **Wiring**: build a shared `ToolServer`, register the 11 static tools on it,
  connect each configured MCP server via `McpClientHandler::new(client_info,
  handle).connect(transport)`, and hand the agent
  `AgentBuilder::tool_server_handle(handle)` instead of the current
  `.tool(...)` chain. `RunningService` handles are held for the process
  lifetime so connections stay alive; `McpClientHandler` owns list-changed
  refresh (no rem-side polling).
- **Naming (Claude convention)**: every MCP tool is registered as
  `mcp__<server>__<tool>` (sanitize server name to `[A-Za-z0-9_-]`; collision
  impossible by construction). The prefix makes origin visible in the
  transcript tool rows, the approval modal, and `rule_key`. Bare upstream
  names are never exposed. Server `instructions` (if any) are prepended to the
  tool description so the model knows when to reach for them.
- **Permissions**: `mcp__*` classifies `Confirm` in **every** mode except
  `Yolo` (where it is `Allow`, consistent with ADR-0011's approval bypass;
  unconditional Deny floor never applies to MCP names — there is no
  annihilator pattern for them). `Plan` denies `mcp__*` like any non-read
  tool. Unknown-tool fallthrough already fails closed to `Confirm`, so the
  explicit `mcp__` arm is defense-in-depth plus Yolo/Plan semantics.
  Session `always allow` rules apply per `rule_key` (`mcp__server__tool:args`),
  same as built-ins.
- **Config**: separate `~/.config/rem/mcp.json` in Claude `mcpServers` shape
  (portable with Claude Desktop / Cursor configs), NOT `config.toml` sections:
  ```json
  { "mcpServers": {
      "github": { "url": "https://mcp.example.com/mcp",
                  "headers": { "Authorization": "Bearer ${GITHUB_TOKEN}" } },
      "sqlite":  { "command": "uvx", "args": ["mcp-server-sqlite", "--db", "/path/to.db"],
                   "env": { "FOO": "${FOO:-default}" } }
  } }
  ```
  A `url` key selects streamable-HTTP; a `command` key selects stdio.
  `${VAR}` / `${VAR:-default}` expansion in `command`, `args`, `env`, `url`,
  `headers`. Missing file = zero MCP servers (no error). Invalid entry =
  warn + skip that server.
- **Failure handling**: warn-and-continue. A server that fails to spawn /
  connect / list-tools logs to stderr, is marked failed for `/mcp` display,
  and the session proceeds with remaining servers + built-ins. Startup never
  fails because of MCP. Mid-session drops retire that server's tools
  (Rig handles disconnect); the model sees a tool error and replans.
- **Auth (v1)**: static `headers` with `${VAR}` expansion **plus full MCP
  OAuth 2.0** (dynamic client registration, localhost-callback browser flow,
  token storage, refresh). Tokens live in `~/.config/rem/mcp-tokens.json`
  (0600, never logged). Login UX: on startup rem prints the auth URL and
  tries `open`/`xdg-open` when a token is missing/expired; `/mcp login
  <name>` / `/mcp logout <name>` slash commands drive (re-)auth from inside
  the TUI. On mid-session 401, rem refreshes once, retries once, then marks
  the server `needs-auth` (model gets a tool error naming the server).
- **TUI**: `/mcp` panel lists servers (connected / failed + reason /
  needs-auth), tool counts, and per-server enable/disable toggle (session
  scope). Approval modal shows `mcp__server__tool` + server origin + reason
  before args (risk-first, ADR-0011 popup pattern). `ToolEvent::preview_args`
  gains an `mcp__*` arm (compact args fallback — no change needed beyond the
  wildcard, since `_ => compact_args` already covers it).
- **Non-goals (v1)**: MCP resources, prompts, sampling, elicitation, SSE
  transport, `roots/list`, serving rem's tools over MCP, per-tool allow-lists
  in config, headless (non-interactive) OAuth.

## Rationale

- Rig already owns the hard parts (ToolServer multiplexing, list-changed
  refresh with versioned commits that can't roll back, per-call timeouts,
  disconnect retirement). Hand-rolling MCP framing or tool multiplexing would
  duplicate `rig-agent/src/tool/{server,rmcp}.rs`.
- `tool_server_handle` (not `.tool()` / `.rmcp_tools()`) is the only path
  that supports live list-changed updates: builder-registered tools are a
  snapshot, handler-registered tools track the server.
- Claude's `mcp__server__tool` naming is the de-facto standard users already
  know from Claude Code permission rules and hook matchers; bare names
  collide across servers and poison `rule_key` scoping.
- Separate `mcp.json` keeps secrets out of `config.toml`, allows copy-paste
  from other clients' setup docs, and avoids TOML-schema churn for a
  map-shaped value.
- Warn-and-continue matches Claude Code (`✘ Failed to connect` per server,
  session proceeds) and keeps one broken server from bricking the TUI.
- Full OAuth (not just static headers) was an explicit user call: static
  headers alone lock out GitHub/Linear/Notion-style hosted servers that only
  speak OAuth + dynamic registration.

## Consequences

- `rig` dependency gains `features = ["rmcp"]`; new deps: `rmcp` (version
  matching rig-agent 0.42's `rmcp v2`, `features = ["client",
  "transport-child-process", "transport-streamable-http-client-reqwest",
  "auth"]`), plus `open` (or equivalent) for the browser step. Compile-time
  cost is one feature-unification bump, not a new HTTP stack (reqwest already
  present via `rig`'s rmcp transport).
- `RigAgent::new` becomes async-adjacent: MCP connects are async, so startup
  (`main.rs`) must connect servers before constructing the agent, or the
  agent takes a pre-built `ToolServerHandle`. `AgentLoop` trait is untouched
  (tools resolve through the handle at dispatch).
- `classify_mode` gains one arm: `tool_name.starts_with("mcp__")` → Plan:
  Deny; Yolo: Allow; else Confirm. `PermissionHook::risk` gains an
  `mcp__*` arm (High, "Third-party. {reason}"). `preview` needs no change
  (wildcard covers it) but gains a test pinning `mcp__github__create_issue`.
- New modules: `src/mcp.rs` (config load + expand + connect + name-prefix
  rename + OAuth token store) and `src/mcp_auth.rs` (browser flow, callback
  server, refresh). New tests: prefix sanitization, `${VAR}` expansion,
  classify arms, warn-and-continue on bad entry, token file 0600.
- OAuth is the dominant cost: localhost callback listener, `open` fallback
  to printed URL (SSH/headless), token refresh on 401, `/mcp login|logout`.
  If it slips, static-headers-only still ships first with the OAuth arm
  stubbed to `needs-auth` — the transport/config/naming/permission work is
  independent of it.

## Resolved defaults (grill, 2026-09-30)

- Q1 direction: client (server + both deferred).
- Q2 transports: stdio + streamable HTTP (SSE out).
- Q3 permissions: Confirm-by-default; Yolo Allows, Plan denies.
- Q4 naming: Claude-style `mcp__server__tool` (user: "how would claude do it").
- Q5 config: separate `~/.config/rem/mcp.json` (Claude `mcpServers` shape).
- Q6 failure: warn-and-continue (never fail startup).
- Q7 auth: full OAuth (user overrode static-headers-only recommendation).
- Q8 OAuth UX: printed URL + `open` attempt, `/mcp login|logout`, 0600 token file.
