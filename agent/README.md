# Python Agent Node

This distribution contains the stdlib SSH service caller and the Python Agent
Runtime. It uses a shared Tool Registry, JSON Schema validation, tenant/user/
target/risk policy, parameter-bound approvals, append-only audit events, MCP
clients and a bounded LangGraph workflow. The model loop supports OpenAI
compatible endpoints, Anthropic and Gemini through their official LangChain
adapters. The desktop approval response RPC is wired through the Python sidecar;
the current desktop tool catalog remains read-only. Durable task storage and
broader write-capable desktop tools remain future work.

## Install

```powershell
python -m pip install -e "agent[agent,models]"
```

The `yukinal_ssh` client itself uses only the Python standard library. The
`yukinal_agent` package requires LangGraph, the official MCP Python SDK and
JSON Schema validation. The `models` extra installs the OpenAI, Anthropic and
Google Gemini LangChain adapters. Python 3.11+ is supported.

## Configure and run a model

Provider selection and credentials stay outside agent business logic. In
standalone use, supply the API key through an environment variable name. A host
integration can pass a per-run credential to
`ModelFactory.create_with_api_key`; the key is not added to `ModelConfig` or
audit data. OpenAI-compatible local endpoints can run without a credential and
do not inherit a machine-level OpenAI key.

```python
import os

from yukinal_agent import (
    AgentBudget, JsonlAuditSink, ModelConfig, ModelDrivenAgent, ModelFactory,
    PolicyEngine, PrincipalScope, ToolRegistry,
)
from yukinal_agent import register_ssh_tools
from yukinal_ssh import SshServiceClient

registry = ToolRegistry(
    JsonlAuditSink("var/audit/tool-calls.jsonl"),
    PolicyEngine({
        ("tenant-a", "operator-7"): PrincipalScope(
            allowed_tools=frozenset({"ssh.service_status"}),
            allowed_target_ids=frozenset({"staging-web-01"}),
            allowed_risk_levels=frozenset({"read"}),
        )
    }),
)
register_ssh_tools(registry, SshServiceClient(os.environ["YUKINAL_SSH_API_TOKEN"]))
model = ModelFactory().create(ModelConfig(
    provider="openai",  # Also supports openai-compatible, anthropic and gemini.
    model="${LLM_MODEL}",
    api_key_env="LLM_API_KEY",
    wire_api="responses",  # OpenAI-compatible only: "chat" or "responses".
    base_url="${LLM_BASE_URL}",
    timeout_seconds=60,
    max_output_tokens=2048,
))
agent = ModelDrivenAgent(registry, model, AgentBudget(max_tool_calls=8))
result = await agent.run(
    task_id="task-123", tenant_id="tenant-a", user_id="operator-7",
    prompt="Check nginx on staging-web-01.",
    allowed_tools={"ssh.service_status"},
    allowed_target_ids={"staging-web-01"},
    host_target={
        "host": "remote",
        "serverId": "staging-web-01",
        "environment": "staging",
    },
)
```

For state-changing tools, the runtime returns `awaiting_approval` with a
fingerprint bound to task, tenant, user, tool, target and the full argument
object. Only the trusted embedding host should provide the corresponding
approval reference on a subsequent run; changed arguments produce a different
fingerprint. Sensitive argument values are redacted from the review object.
Tool calls still pass through the registry and policy on every execution.

## Sidecar protocol

Install `agent[agent,models]`, set `YUKINAL_DATA_DIR` to the private per-user
data directory, and launch `yukinal-agent-node`. The process speaks version
1.0 JSON-RPC 2.0 over newline-delimited stdin/stdout; stdout is reserved for
frames and diagnostics go to stderr. Frames are capped at 24 MiB. The current
Python sidecar implements `initialize`, `system.ping`, `tools.list`,
`system.describe`, `provider.test`, text-only `agent.run.start`, and
`agent.run.stop`; `agent.approval.respond` resumes a pending approval, and
`provider.models` returns model catalogs for all three providers. Over the same
NDJSON connection it can request host-side tool
execution and fetch the host's MCP catalog. The first integrated set contains
read-only server, Docker, systemd, package and file tools, plus MCP tools the
host marks low-risk. The host remains the final authorization boundary. When a
registered tool requires approval, the sidecar emits the approval event, accepts
the desktop's response RPC, and resumes the paused call; rejection and expiry
stop the graph without retrying. Multimodal prompts, durable task state,
write-capable desktop tools and the packaged launcher switch are still being
migrated.

When `YUKINAL_SSH_API_TOKEN` is explicitly passed to the Python sidecar, it also
registers the read-only `ssh.service_status` tool backed by the C++ service. The
tool gets its target from the selected `host_target.serverId`; `target_id` is not
part of the model-visible input schema. Configure that exact server ID in the
C++ service allowlist. The packaged Node sidecar does not receive this token.

Tauri development uses this runtime by default. Create the environment from the
repository root and install the runtime dependencies:

```powershell
python -m venv agent/.venv
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models]"
pnpm --filter @yukinal/desktop tauri dev
```

`pnpm desktop:dev:python` is an equivalent shortcut.

On macOS or Linux, use `agent/.venv/bin/python` for the install command. Set
`YUKINAL_PYTHON` to another interpreter path when needed.

## Register and run tools

The standalone `ModelDrivenAgent` API receives the identity and allowlists from
its embedding host. In the desktop bridge, the Python sidecar exposes only its
registered read-only tools and passes the resolved target and call to the
desktop host, which remains responsible for its authenticated permission and
approval checks. The model never supplies those claims.

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
    register_ssh_tools(registry, SshServiceClient(
        os.environ["YUKINAL_SSH_API_TOKEN"],
        os.environ.get("YUKINAL_SSH_API_URL", "http://127.0.0.1:8765"),
    ))

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
            host_target={
                "host": "remote",
                "serverId": "staging-web-01",
                "environment": "staging",
            },
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"service": "nginx.service"},
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
redaction, approval binding, model switching, model/tool loops, timeout/output
budgets, MCP registration, and stable provider/MCP errors. Install
`agent[agent,models]` before CMake configuration for CTest to register those
tests. When Paramiko is installed too, `agent_ssh_mock_integration` exercises
`ssh.service_status` through `AgentWorkflow` and the C++ service to verify the
selected server ID, task target allowlist, and rejection of model-supplied
target overrides. A real SSH success test requires the optional
OpenSSH test container described in the repository README.
