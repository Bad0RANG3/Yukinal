# mcp/ — MCP adapter

**State: empty on purpose.** MCP is architecturally included but is not in
the MVP list, so nothing is stubbed here yet.

Planned shape:

```text
MCP Server -> MCP Client (official SDK) -> Adapter -> ToolRegistry -> Agent
```

The adapter's job is to make MCP tools *look like* built-in tools, which also means it
must supply what MCP cannot be trusted to supply:

| concern            | source of truth                                   |
| ------------------ | ------------------------------------------------- |
| name               | namespaced `mcp.<serverId>.<tool>` -> `mcp__…` (ADR 0004) |
| risk               | `McpServerConfig.trustLevel`, default ≥ medium     |
| timeout / retry    | local policy, never the remote server's claim      |
| target             | resolved `ToolTarget`                   |
| permission         | Permission Engine as usual (ADR 0005)              |

A malicious MCP server is part of the threat model: tool descriptions are
untrusted text and must never be spliced into instructions.
