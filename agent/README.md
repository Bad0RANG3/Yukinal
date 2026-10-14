# Python Agent Node

This distribution contains the stdlib SSH service caller from milestone 1 and
the Python Agent Runtime foundation from milestone 2. The Agent Runtime uses a
shared Tool Registry, JSON Schema validation, default-deny tool/target checks,
parameter-bound approvals, append-only audit events, MCP clients and a bounded
LangGraph workflow. It does not load an LLM provider yet; tool calls are
explicit inputs until milestone 3 adds model planning.

## Install

```powershell
python -m pip install -e "agent[agent]"
```

The `yukinal_ssh` client itself uses only the Python standard library. The
`yukinal_agent` package requires LangGraph, the official MCP Python SDK and
JSON Schema validation. Python 3.11+ is supported.

## Register and run tools

The embedding host supplies the authenticated identity, target allowlist and
allowed-tool set. The model must never be allowed to provide those claims.

```python
import asyncio
import os

from yukinal_agent import (
    AgentWorkflow,
    JsonlAuditSink,
    MCPClientManager,
    MCPServerConfig,
    ToolRegistry,
    register_ssh_tools,
)
from yukinal_ssh import SshServiceClient


async def main() -> None:
    registry = ToolRegistry(JsonlAuditSink("var/audit/tool-calls.jsonl"))
    register_ssh_tools(registry, SshServiceClient(os.environ["YUKINAL_SSH_API_TOKEN"]))

    async with MCPClientManager() as mcp:
        await mcp.connect(MCPServerConfig(
            name="ops-tools",
            transport="streamable_http",
            url="${OPS_MCP_URL}",
            header_refs={"Authorization": "OPS_MCP_TOKEN"},
            read_only_tools=frozenset({"lookup_service"}),
        ))
        await mcp.register_tools("ops-tools", registry)
        workflow = AgentWorkflow(registry)
        result = await workflow.run(
            task_id="task-123",
            tenant_id="tenant-a",
            user_id="operator-7",
            allowed_tools={"ssh.service_status", "mcp.ops-tools.lookup_service"},
            allowed_target_ids={"staging-web-01"},
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"target_id": "staging-web-01", "service": "nginx.service"},
            }],
        )
        print(result.status, result.results)


asyncio.run(main())
```

An MCP tool is write-capable and approval-required by default. The local
`read_only_tools` configuration is the only way to classify a discovered MCP
tool as read-only; server annotations and descriptions do not change policy.
Remote MCP endpoints must use HTTPS, and credentials are read through named
environment references. Stdio servers receive only the environment values
explicitly referenced in their config. For bearer authentication, the
referenced header value should include the `Bearer ` prefix.

Audit events record task/tenant/user, target, tool, argument names and types,
timing, result shape, approval status and a stable error code. Raw arguments,
tool results and exception text are not written to the audit file. The JSONL
file is created with owner-only permissions where the platform supports them.

## Tests

```powershell
$env:PYTHONPATH = "agent/src"
python -m unittest discover -s agent/tests -p test_client.py -v
python -m unittest discover -s agent/tests -p "test_agent_*.py" -v
```

The service integration test is added to CTest and launches the built C++
service to check health, allowlist rejection, loopback origin rejection, and
SSH connection error classification. The Agent tests use the official MCP
SDK's in-process transport and cover schema/permission rejection, audit
redaction, approvals, timeout/output budgets, MCP registration, LangGraph
execution and stable MCP connection failure errors. Install `agent[agent]`
before CMake configuration for CTest to register those tests. A real SSH success test requires the optional
OpenSSH test container described in the repository README.
