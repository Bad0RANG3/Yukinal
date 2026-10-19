from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import sys
import uuid
from collections import OrderedDict
from dataclasses import asdict, dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, BinaryIO, Callable
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from yukinal_ssh import SshServiceClient

from .audit import JsonlAuditSink
from .conversation_store import ConversationStore, message_bytes
from .coordination import CoordinationRuntime, COORDINATOR_TOOLS, parse_coordination
from .host_rpc import HostRpcClient
from .host_tools import (
    HOST_APPROVAL_TOOLS,
    PLAN_BOUND_HOST_ACTION_TOOLS,
    register_desktop_backup_metadata_tools,
    register_host_tools,
    register_mcp_catalog,
)
from .model_agent import AgentBudget, ModelDrivenAgent
from .models import ModelConfig, ModelFactory, ModelFactoryError, with_reasoning_effort
from .provider_store import ProviderStore, ProviderStoreError, validate_provider_save
from .ssh_tools import (
    _parse_docker_logs,
    _parse_docker_inspect,
    _parse_docker_ps,
    _parse_package_inspect,
    _parse_server_logs,
    _parse_service_inventory,
    _parse_systemd_inspect,
    register_desktop_ssh_tools,
    register_ssh_tools,
)
from .ssh_service_process import SshServiceProcess, SshServiceProcessError
from .system_snapshot import SYSTEM_SNAPSHOT_COMMAND_IDS, normalize_system_snapshot
from .network_snapshot import NETWORK_SNAPSHOT_COMMAND_IDS
from .tools import ToolCallError, ToolRegistry

RPC_VERSION = "1.0"
MAX_SIDECAR_FRAME_BYTES = 24 * 1024 * 1024
RPC_PARSE_ERROR = -32700
RPC_INVALID_REQUEST = -32600
RPC_METHOD_NOT_FOUND = -32601
RPC_INVALID_PARAMS = -32602
RPC_INTERNAL_ERROR = -32603
RPC_NOT_IMPLEMENTED = -32604

METHODS = {
    "initialize": True,
    "system.ping": True,
    "server.snapshot.normalize": True,
    "server.logs.normalize": True,
    "server.services.normalize": True,
    "docker.ps.normalize": True,
    "docker.logs.normalize": True,
    "docker.inspect.normalize": True,
    "systemd.inspect.normalize": True,
    "package.inspect.normalize": True,
    "tools.list": True,
    "system.describe": True,
    "agent.run.start": True,
    "agent.run.stop": True,
    "agent.teams.list": True,
    "agent.approval.respond": True,
    "agent.context.reset": True,
    "agent.context.resetAll": True,
    "agent.context.delete": True,
    "agent.memory.list": True,
    "agent.memory.save": True,
    "agent.memory.delete": True,
    "provider.models": True,
    "provider.test": True,
    "provider.list": True,
    "provider.save": True,
    "provider.delete": True,
    "provider.activate": True,
}


@dataclass(frozen=True)
class _RpcFault(Exception):
    code: int
    message: str
    data: Any = None


@dataclass
class _PendingApproval:
    run_id: str
    checkpoint_id: str
    fingerprint: str
    future: asyncio.Future[str]


class PythonSidecar:
    """Minimal version-1 desktop JSON-RPC boundary for the Python Agent Node."""

    def __init__(
        self,
        registry: ToolRegistry,
        *,
        agent_version: str = "0.1.0",
        model_factory: ModelFactory | None = None,
        notification_sink: Callable[[str, dict[str, Any]], None] | None = None,
        approval_ttl_seconds: int = 120,
        checkpointer: Any | None = None,
        managed_ssh_service_binary: Path | None = None,
        provider_store: ProviderStore | None = None,
        conversation_store: ConversationStore | None = None,
    ) -> None:
        if isinstance(approval_ttl_seconds, bool) or not isinstance(approval_ttl_seconds, int) or not 1 <= approval_ttl_seconds <= 3600:
            raise ValueError("approval_ttl_seconds must be between 1 and 3600")
        self._registry = registry
        self._agent_version = agent_version
        self._model_factory = model_factory or ModelFactory()
        self._notification_sink = notification_sink
        self._approval_ttl_seconds = approval_ttl_seconds
        self._checkpointer = checkpointer
        self._initialized = False
        self._active_runs: dict[str, asyncio.Task[dict[str, Any]]] = {}
        self._completed_runs: OrderedDict[str, str] = OrderedDict()
        self._admissions: OrderedDict[str, dict[str, Any]] = OrderedDict()
        self._host_rpc: HostRpcClient | None = None
        self._host_tools_registered = False
        self._backup_metadata_tools_registered = False
        self._mcp_loaded = False
        self._pending_approvals: dict[str, _PendingApproval] = {}
        self._managed_ssh_service_binary = managed_ssh_service_binary
        self._managed_ssh_tools_registered = False
        self._provider_store = provider_store
        self._conversations = conversation_store or ConversationStore()
        self._session_locks: dict[str, asyncio.Lock] = {}
        self._run_sessions: dict[str, str] = {}
        self._resetting_sessions: set[str] = set()
        self._resetting_all = False
        self._coordination = CoordinationRuntime(self._registry, self._conversations, self._emit, self._launch_team)

    def set_notification_sink(self, sink: Callable[[str, dict[str, Any]], None] | None) -> None:
        self._notification_sink = sink

    def set_host_rpc(self, host: HostRpcClient | None) -> None:
        self._host_rpc = host
        direct_tool_names: set[str] = set()
        if host is not None and not self._backup_metadata_tools_registered:
            direct_tool_names.update(register_desktop_backup_metadata_tools(self._registry, host))
            self._backup_metadata_tools_registered = True
        if (
            host is not None
            and self._managed_ssh_service_binary is not None
            and not self._managed_ssh_tools_registered
        ):
            direct_tool_names.update(register_desktop_ssh_tools(
                self._registry, host, self._managed_ssh_service_binary
            ))
            self._managed_ssh_tools_registered = True
        if host is not None and not self._host_tools_registered:
            register_host_tools(self._registry, host, excluded_names=frozenset(direct_tool_names))
            self._host_tools_registered = True

    def set_checkpointer(self, checkpointer: Any | None) -> None:
        if self._run_sessions or self._resetting_sessions or self._resetting_all:
            raise RuntimeError("cannot replace the Agent checkpointer while runs are active")
        self._checkpointer = checkpointer

    def handle(self, request: object) -> dict[str, Any] | None:
        request_id: int | None = None
        if isinstance(request, dict) and isinstance(request.get("id"), int) and not isinstance(request.get("id"), bool):
            request_id = request["id"]
        if not isinstance(request, dict) or request.get("jsonrpc") != "2.0" or not isinstance(request.get("method"), str):
            return _failure(request_id, RPC_INVALID_REQUEST, "Invalid JSON-RPC request")
        method = request["method"]
        if request_id is None:
            # Notifications do not receive a response. The Python sidecar has no
            # client-originated notification methods in protocol version 1.0.
            return None
        params = request.get("params", {})
        try:
            if not self._initialized and method != "initialize":
                raise _RpcFault(RPC_INVALID_REQUEST, "initialize must be the first call")
            if method == "initialize":
                result = self._initialize(params)
            elif method == "system.ping":
                result = self._ping(params)
            elif method == "server.snapshot.normalize":
                result = self._normalize_snapshot(params)
            elif method == "server.logs.normalize":
                result = self._normalize_server_logs(params)
            elif method == "server.services.normalize":
                result = self._normalize_server_services(params)
            elif method == "docker.ps.normalize":
                result = self._normalize_docker_ps(params)
            elif method == "docker.logs.normalize":
                result = self._normalize_docker_logs(params)
            elif method == "docker.inspect.normalize":
                result = self._normalize_docker_inspect(params)
            elif method == "systemd.inspect.normalize":
                result = self._normalize_systemd_inspect(params)
            elif method == "package.inspect.normalize":
                result = self._normalize_package_inspect(params)
            elif method == "tools.list":
                _strict_object(params, set())
                result = {"tools": [self._declaration(spec) for spec in self._registry.list()]}
            elif method == "system.describe":
                _strict_object(params, set())
                result = {
                    "providers": [],
                    "toolCount": len(self._registry.list()),
                    "permissionPolicyIds": [],
                    "toolNameCollisions": [],
                    "implemented": dict(METHODS),
                }
            elif method == "agent.teams.list":
                parsed = _strict_object(params, {"sessionId", "focusServerId"})
                session_id = parsed.get("sessionId")
                if session_id is not None and (not isinstance(session_id, str) or not session_id.strip() or len(session_id) > 256):
                    raise _RpcFault(RPC_INVALID_PARAMS, "A conversation is required")
                if parsed.get("focusServerId") is not None and (not session_id or not _valid_server_id(parsed["focusServerId"])):
                    raise _RpcFault(RPC_INVALID_PARAMS, "Invalid conversation scope")
                try:
                    if session_id is not None:
                        self._conversations.bind_session(session_id, parsed.get("focusServerId"))
                except ValueError as error:
                    raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
                result = {"tasks": [self._coordination.public(job) for job in self._coordination.list(session_id)]}
            elif method == "agent.approval.respond":
                result = self._approval_respond(params)
            elif method == "agent.memory.list":
                result = self._memory_list(params)
            elif method == "agent.memory.save":
                result = self._memory_save(params)
            elif method == "agent.memory.delete":
                result = self._memory_delete(params)
            elif method in METHODS:
                raise _RpcFault(RPC_NOT_IMPLEMENTED, f"Method {method} is not implemented by this Agent Node")
            else:
                raise _RpcFault(RPC_METHOD_NOT_FOUND, f"Unknown method {method}")
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        except _RpcFault as error:
            response = _failure(request_id, error.code, error.message)
            if error.data is not None:
                response["error"]["data"] = error.data
            return response
        except Exception:
            return _failure(request_id, RPC_INTERNAL_ERROR, "Internal sidecar error")

    async def handle_async(self, request: object) -> dict[str, Any] | None:
        """Handle run and provider RPCs that may wait on model work."""
        if (
            not isinstance(request, dict)
            or request.get("jsonrpc") != "2.0"
            or not isinstance(request.get("method"), str)
            or request.get("method") not in (
                "initialize", "provider.models", "provider.test", "provider.list", "provider.save",
                "provider.delete", "provider.activate", "agent.run.start", "agent.run.stop",
                "agent.approval.respond",
                "agent.context.reset", "agent.context.resetAll", "agent.context.delete", "agent.memory.list", "agent.memory.save", "agent.memory.delete",
            )
            or not isinstance(request.get("id"), int)
            or isinstance(request.get("id"), bool)
        ):
            return self.handle(request)
        request_id = request["id"]
        try:
            method = request["method"]
            if method == "initialize":
                result = self._initialize(request.get("params", {}))
                if self._host_rpc is not None:
                    try:
                        catalog = await self._host_rpc.request("host.mcp.catalog", {}, timeout_seconds=8.0)
                        registered = register_mcp_catalog(self._registry, self._host_rpc, catalog)
                        self._mcp_loaded = True
                        print(f"Loaded {len(registered)} MCP tools from the desktop host", file=sys.stderr, flush=True)
                    except Exception:
                        print("Desktop MCP catalog is unavailable; continuing without its tools", file=sys.stderr, flush=True)
                result["capabilities"]["toolCalling"] = bool(self._registry.list())
                result["capabilities"]["mcp"] = self._mcp_loaded
            elif not self._initialized:
                raise _RpcFault(RPC_INVALID_REQUEST, "initialize must be the first call")
            elif method == "provider.models":
                result = await self._provider_models(request.get("params", {}))
            elif method == "provider.test":
                result = await self._provider_test(request.get("params", {}))
            elif method == "provider.list":
                result = await self._provider_list(request.get("params", {}))
            elif method == "provider.save":
                result = await self._provider_save(request.get("params", {}))
            elif method == "provider.delete":
                result = await self._provider_delete(request.get("params", {}))
            elif method == "provider.activate":
                result = await self._provider_activate(request.get("params", {}))
            elif method == "agent.run.start":
                result = await self._run_start(request.get("params", {}))
            elif method == "agent.run.stop":
                result = self._run_stop(request.get("params", {}))
            elif method == "agent.approval.respond":
                result = self._approval_respond(request.get("params", {}))
            elif method == "agent.context.reset":
                result = await self._context_reset(request.get("params", {}))
            elif method == "agent.context.resetAll":
                result = await self._context_reset_all(request.get("params", {}))
            elif method == "agent.context.delete":
                result = await self._context_reset(request.get("params", {}), delete=True)
            elif method == "agent.memory.list":
                result = self._memory_list(request.get("params", {}))
            elif method == "agent.memory.save":
                result = self._memory_save(request.get("params", {}))
            elif method == "agent.memory.delete":
                result = self._memory_delete(request.get("params", {}))
            else:
                return self.handle(request)
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        except _RpcFault as error:
            response = _failure(request_id, error.code, error.message)
            if error.data is not None:
                response["error"]["data"] = error.data
            return response
        except Exception:
            return _failure(request_id, RPC_INTERNAL_ERROR, "Agent request failed")

    async def _test_provider(self, params: object) -> dict[str, bool]:
        model, timeout_ms = self._create_runtime_model(params)
        try:
            async with asyncio.timeout(timeout_ms / 1000):
                response = await model.ainvoke("Reply with OK only.")
        except Exception:
            raise _RpcFault(
                RPC_INTERNAL_ERROR,
                "Model test failed; check the endpoint, credentials, and model configuration",
            ) from None
        content = getattr(response, "content", None)
        if isinstance(content, str):
            has_text = bool(content.strip())
        elif isinstance(content, list):
            has_text = any(
                isinstance(block, dict)
                and isinstance(block.get("text"), str)
                and bool(block["text"].strip())
                for block in content
            )
        else:
            has_text = False
        if not has_text:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Model returned no text; check the model and endpoint settings")
        return {"ok": True}

    async def _list_provider_models(self, params: object) -> dict[str, Any]:
        config, api_key, timeout_ms = _parse_runtime_model_config(params)
        try:
            models = await asyncio.to_thread(_fetch_model_catalog, config, api_key, timeout_ms)
        except _RpcFault:
            raise
        except Exception:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider model catalog request failed") from None
        return {"models": models}

    def _require_provider_store(self) -> ProviderStore:
        if self._provider_store is None:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider storage is unavailable")
        return self._provider_store

    async def _provider_credential(self, reference: str) -> str:
        if self._host_rpc is None:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider credential service is unavailable")
        try:
            result = await self._host_rpc.request(
                "host.credentials.get", {"reference": reference}, timeout_seconds=8.0
            )
        except Exception:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider credential is unavailable") from None
        secret = result.get("secret") if isinstance(result, dict) else None
        if not isinstance(secret, str):
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider credential is unavailable")
        return secret

    async def _provider_key(self, provider: dict[str, Any]) -> str | None:
        reference = provider.get("apiKeyCredentialRef")
        if reference is None:
            return None
        if not isinstance(reference, str):
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider credential reference is invalid")
        return await self._provider_credential(reference)

    def _provider_runtime_config(
        self, provider: dict[str, Any], api_key: str | None, timeout_ms: int = 30_000
    ) -> dict[str, Any]:
        kind = provider["kind"]
        base_url = provider.get("baseUrl") or ""
        if kind == "anthropic" and not base_url.strip():
            base_url = "https://api.anthropic.com"
        elif kind == "gemini" and not base_url.strip():
            base_url = "https://generativelanguage.googleapis.com"
        runtime: dict[str, Any] = {
            "kind": kind,
            "baseUrl": base_url,
            "model": provider["model"],
            "timeoutMs": timeout_ms,
        }
        if api_key is not None:
            runtime["apiKey"] = api_key
        if provider.get("customHeaders") is not None:
            runtime["customHeaders"] = provider["customHeaders"]
        if kind == "anthropic" and provider.get("apiVersion"):
            runtime["apiVersion"] = provider["apiVersion"]
        if kind == "openai-compatible":
            runtime["wireApi"] = provider.get("wireApi", "chat")
        return runtime

    async def _provider_list(self, params: object) -> dict[str, Any]:
        _strict_object(params, set())
        store = self._require_provider_store()
        providers = store.list_ai()
        usable_ids: set[str] = set()
        for provider in providers:
            reference = provider.get("apiKeyCredentialRef")
            if isinstance(reference, str):
                try:
                    key = await self._provider_credential(reference)
                except _RpcFault:
                    continue
                if key.strip():
                    usable_ids.add(provider["id"])
            elif _is_local_provider_endpoint(provider.get("baseUrl", "")):
                usable_ids.add(provider["id"])
        return {"providers": store.normalize_ai_state(usable_ids)}

    async def _provider_save(self, params: object) -> dict[str, Any]:
        store = self._require_provider_store()
        try:
            config = validate_provider_save(params)
            existing = store.get_save_target(config["providerId"])
        except ProviderStoreError as error:
            raise _provider_store_fault(error) from None
        provider_id = (
            existing["id"] if existing is not None else config["providerId"]
        ) or f"prv_{uuid.uuid4().hex}"
        if config["kind"] == "openai-compatible":
            config["wireApi"] = config["wireApi"] or (existing or {}).get("wireApi") or "chat"
        new_reference: str | None = None
        api_key = config["apiKey"]
        if isinstance(api_key, str):
            account = f"provider_{provider_id}_{uuid.uuid4().hex}"
            if self._host_rpc is None:
                raise _RpcFault(RPC_INTERNAL_ERROR, "Provider credential service is unavailable")
            try:
                stored = await self._host_rpc.request(
                    "host.credentials.set",
                    {"service": "openai", "account": account, "secret": api_key},
                    timeout_seconds=8.0,
                )
            except Exception:
                raise _RpcFault(RPC_INTERNAL_ERROR, "Could not store provider credential") from None
            new_reference = stored.get("credentialRef") if isinstance(stored, dict) else None
            if not isinstance(new_reference, str):
                raise _RpcFault(RPC_INTERNAL_ERROR, "Could not store provider credential")
        try:
            saved, old_reference = store.save_ai(provider_id, config, new_reference)
        except ProviderStoreError as error:
            if new_reference is not None:
                await self._try_reclaim_provider_credential(new_reference)
            raise _provider_store_fault(error) from None
        if old_reference and old_reference != new_reference:
            try:
                if not store.credential_ref_used_elsewhere(old_reference, provider_id):
                    await self._reclaim_provider_credential(old_reference)
            except _RpcFault:
                # The provider is already saved; matching the desktop behavior, a failed
                # cleanup must not undo the successful configuration update.
                pass
        return {"provider": saved}

    async def _provider_delete(self, params: object) -> dict[str, Any]:
        parsed = _strict_object(params, {"providerId"})
        provider_id = parsed.get("providerId")
        if not isinstance(provider_id, str) or not provider_id.strip():
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        store = self._require_provider_store()
        try:
            reference, shared = store.delete_ai(provider_id)
        except ProviderStoreError as error:
            raise _provider_store_fault(error) from None
        reclaimed = False
        if reference and not shared:
            result = await self._reclaim_provider_credential(reference)
            reclaimed = result
        return {"deleted": True, "credentialReclaimed": reclaimed}

    async def _provider_activate(self, params: object) -> dict[str, Any]:
        parsed = _strict_object(params, {"providerId"})
        provider_id = parsed.get("providerId")
        if not isinstance(provider_id, str) or not provider_id.strip():
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        try:
            provider = self._require_provider_store().activate_ai(provider_id)
        except ProviderStoreError as error:
            raise _provider_store_fault(error) from None
        return {"provider": provider}

    async def _provider_models(self, params: object) -> dict[str, Any]:
        parsed = _strict_object(params, {"providerId"})
        provider_id = parsed.get("providerId")
        if not isinstance(provider_id, str) or not provider_id.strip():
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        store = self._require_provider_store()
        try:
            provider = store.get_ai(provider_id)
        except ProviderStoreError as error:
            raise _provider_store_fault(error) from None
        if provider is None:
            raise _RpcFault(RPC_INTERNAL_ERROR, f"Provider `{provider_id}` was not found")
        cached = provider.get("models") or [{
            "id": provider["model"], "label": provider["model"],
            "supportsToolCalling": True, "supportsStreaming": True,
        }]
        try:
            api_key = await self._provider_key(provider)
            response = await self._list_provider_models(
                self._provider_runtime_config(provider, api_key)
            )
        except _RpcFault:
            raise
        models = response.get("models") if isinstance(response, dict) else None
        if not isinstance(models, list) or not models:
            models = cached
        try:
            store.update_models(provider_id, models)
        except ProviderStoreError as error:
            raise _provider_store_fault(error) from None
        return {"models": models}

    async def _provider_test(self, params: object) -> dict[str, bool]:
        parsed = _strict_object(params, {"providerId"})
        provider_id = parsed.get("providerId")
        if not isinstance(provider_id, str) or not provider_id.strip():
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        try:
            provider = self._require_provider_store().get_ai(provider_id)
        except ProviderStoreError as error:
            raise _provider_store_fault(error) from None
        if provider is None:
            raise _RpcFault(RPC_INTERNAL_ERROR, f"Provider `{provider_id}` was not found")
        api_key = await self._provider_key(provider)
        return await self._test_provider(self._provider_runtime_config(provider, api_key))

    async def _run_provider_model(
        self, provider_id: object, selected_model: object
    ) -> tuple[Any, int]:
        listed = await self._provider_list({})
        providers = listed["providers"]
        if provider_id is None:
            provider = next((item for item in providers if item.get("enabled") is True), None)
        else:
            provider = next(
                (
                    item for item in providers
                    if item.get("id") == provider_id and item.get("enabled") is True
                ),
                None,
            )
        if provider is None:
            raise _RpcFault(
                RPC_INVALID_PARAMS,
                "No enabled AI provider is available; configure one in Settings > Provider",
            )
        model_name = selected_model.strip() if isinstance(selected_model, str) else ""
        if not model_name:
            model_name = provider["model"]
        if len(model_name) > 256 or "\r" in model_name or "\n" in model_name:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid model selection")
        api_key = await self._provider_key(provider)
        config = self._provider_runtime_config(provider, api_key, 120_000)
        config["model"] = model_name
        return self._create_runtime_model(config)

    async def _reclaim_provider_credential(self, reference: str) -> bool:
        if self._host_rpc is None:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Provider credential service is unavailable")
        try:
            result = await self._host_rpc.request(
                "host.credentials.reclaim", {"reference": reference}, timeout_seconds=8.0
            )
        except Exception:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Could not reclaim provider credential") from None
        return isinstance(result, dict) and result.get("reclaimed") is True

    async def _try_reclaim_provider_credential(self, reference: str) -> None:
        try:
            await self._reclaim_provider_credential(reference)
        except _RpcFault:
            pass

    def _create_runtime_model(self, params: object) -> tuple[Any, int]:
        config, api_key, timeout_ms = _parse_runtime_model_config(params)
        try:
            model = self._model_factory.create_with_api_key(config, api_key)
        except ModelFactoryError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        return model, timeout_ms

    async def _run_start(self, params: object) -> dict[str, Any]:
        request = _parse_run_request(params)
        if self._resetting_all or request["sessionId"] in self._resetting_sessions:
            raise _RpcFault(RPC_INVALID_PARAMS, "Wait for the context reset to finish before starting a run")
        try:
            self._conversations.bind_session(request["sessionId"], request.get("targetId"))
        except ValueError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        run_id = request["runId"]
        message_id = request.get("messageId")
        fingerprint = _run_fingerprint(request)
        admission = self._admissions.get(message_id) if message_id else None
        resumed = False
        if admission is not None:
            if admission["sessionId"] != request["sessionId"] or admission["fingerprint"] != fingerprint:
                raise _RpcFault(RPC_INVALID_PARAMS, "messageId was already admitted with different content")
            if admission["started"]:
                return {"runId": admission["runId"], "started": False, "duplicate": True}
            if not request["resume"]:
                return {"runId": admission["runId"], "started": False, "duplicate": True}
            run_id = admission["runId"]
            resumed = True

        if not request["resume"]:
            if not message_id:
                raise _RpcFault(RPC_INVALID_PARAMS, "resume false requires messageId")
            self._admissions[message_id] = {
                "sessionId": request["sessionId"],
                "fingerprint": fingerprint,
                "runId": run_id,
                "started": False,
                "completed": False,
            }
            self._prune_admissions()
            return {"runId": run_id, "started": False}

        if run_id in self._run_sessions:
            raise _RpcFault(RPC_INVALID_PARAMS, "runId is already running")
        if run_id in self._completed_runs:
            if self._completed_runs[run_id] != fingerprint:
                raise _RpcFault(RPC_INVALID_PARAMS, "runId was already used with different content")
            return {"runId": run_id, "started": False, "duplicate": True}

        # Reserve before the first host/provider await: a reset or delete must
        # not replace the context while a run is still being admitted.
        self._run_sessions[run_id] = request["sessionId"]
        try:
            return await self._admit_run(request, run_id, admission, resumed)
        finally:
            if run_id not in self._active_runs:
                self._coordination.parents.pop(run_id, None)
                self._run_sessions.pop(run_id, None)
                if request["sessionId"] not in self._run_sessions.values():
                    self._session_locks.pop(request["sessionId"], None)

    async def _admit_run(
        self, request: dict[str, Any], run_id: str,
        admission: dict[str, Any] | None, resumed: bool,
    ) -> dict[str, Any]:
        message_id = request.get("messageId")
        fingerprint = _run_fingerprint(request)
        await self._hydrate_conversation(request)
        try:
            self._conversations.validate_participant(request["sessionId"], request.get("targetId"), request.get("participant"))
        except ValueError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        model, _timeout_ms = await self._run_provider_model(
            request.get("providerId"), request.get("model")
        )
        if "reasoningEffort" in request:
            try:
                model = with_reasoning_effort(model, request["reasoningEffort"])
            except ModelFactoryError as error:
                raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        if request.get("coordination"):
            self._coordination.activate(request)
        try:
            agent = ModelDrivenAgent(
                self._registry,
                model,
                request["agentBudget"],
                tool_names=_allowed_tools_for_run(self._registry, request.get("mode", "goal"), request),
                event_sink=self._emit,
                checkpointer=self._checkpointer,
                transcript_sink=lambda messages: self._conversations.update_messages(run_id, messages),
            )
        except Exception:
            raise _RpcFault(RPC_INVALID_PARAMS, "configured model cannot run the registered tools") from None

        if admission is not None:
            admission["started"] = True
        elif message_id:
            self._admissions[message_id] = {
                "sessionId": request["sessionId"],
                "fingerprint": fingerprint,
                "runId": run_id,
                "started": True,
                "completed": False,
            }
            self._prune_admissions()

        request["runId"] = run_id
        try:
            self._conversations.begin(request, _run_fingerprint(request))
        except ValueError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        task = asyncio.create_task(self._execute_run(request, run_id, agent))
        self._active_runs[run_id] = task
        self._run_sessions[run_id] = request["sessionId"]
        if request["delivery"] == "sync":
            result = await task
            response: dict[str, Any] = {"runId": run_id, "started": True, "result": result}
        else:
            response = {"runId": run_id, "started": True}
        if resumed:
            response["resumed"] = True
        return response

    async def _launch_team(self, job: dict[str, Any], parent: dict[str, Any], role: dict[str, Any]) -> dict[str, Any]:
        # Child sessions contain only the bounded task brief. They never hydrate
        # the native user conversation or wait for its foreground session lock.
        payload: dict[str, Any] = {
            "runId": job["runId"], "sessionId": job["workerSessionId"], "prompt": job["goal"],
            "delivery": "sync", "mode": parent.get("mode", "goal") if job["access"] == "write" else "readonly",
            "permissionMode": parent.get("permissionMode", "ask"),
            "persona": {"id": role["id"], "label": role["label"], "systemPrompt": role["systemPrompt"] +
                "\n你是后台团队成员。只完成当前任务，不能再分派团队。给协调助手返回简短结果、证据、失败和待确认事项。没有执行证据不能声称完成。"},
            "participant": {"id": role["id"], "label": role["label"], "kind": "agent"},
        }
        role_provider = role.get("providerId")
        provider_id = role_provider or parent.get("providerId")
        if provider_id:
            payload["providerId"] = provider_id
        # A separately selected provider owns its default model. Never carry a
        # model name from the parent's provider into a provider-only binding.
        model = role.get("model")
        if not role_provider:
            model = model or parent.get("model")
        if model:
            payload["model"] = model
        # Reasoning settings belong to the inherited model. A model-only
        # override can be incompatible with the parent's reasoning settings.
        inherits_model = not role_provider and (
            not role.get("model") or role["model"] == parent.get("model")
        )
        if inherits_model and parent.get("reasoningEffort"):
            payload["reasoningEffort"] = parent["reasoningEffort"]
        for field in ("taskBudget", "toolAccess", "policyId"):
            if field in parent:
                payload[field] = parent[field]
        if job.get("serverId"):
            payload["focusServerId"] = job["serverId"]
            server = next(item for item in parent["coordination"]["servers"] if item["id"] == job["serverId"])
            payload["target"] = {"host": "remote", "serverId": job["serverId"], "environment": server["environment"]}
        # An existing durable task binding can be inherited only on its exact
        # original server. Other jobs do not acquire a host action plan by role.
        if parent.get("taskId") and parent.get("targetId") == job.get("serverId"):
            payload["taskId"] = parent["taskId"]
        self._conversations.import_legacy(payload["sessionId"], job.get("serverId"), [])
        response = await self._run_start(payload)
        return response.get("result", {"state": "failed", "error": "team_run_not_started"})

    async def _hydrate_conversation(self, request: dict[str, Any]) -> None:
        # Concurrent first turns must share one native snapshot, even while its
        # host request is pending. Use the same ordering as conversation runs.
        lock = self._session_locks.setdefault(request["sessionId"], asyncio.Lock())
        async with lock:
            await self._hydrate_session_history(request)

    async def _hydrate_session_history(self, request: dict[str, Any]) -> None:
        session_id = request["sessionId"]
        server_id = request.get("targetId")
        if not self._conversations.needs_hydration(session_id, server_id):
            return
        if self._host_rpc is None:
            self._conversations.import_legacy(session_id, server_id, [])
            return
        params = {"sessionId": session_id, "limit": 1000,
                  **({"serverId": server_id} if server_id is not None else {}),
                  **({"excludeMessageId": request["messageId"]} if request.get("messageId") else {})}
        try:
            response = await self._host_rpc.request("host.chat.context", params, timeout_seconds=8.0)
        except Exception:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Could not load this conversation's saved context; retry the run") from None
        if not isinstance(response, dict) or not isinstance(response.get("session"), dict) or not isinstance(response.get("messages"), list):
            raise _RpcFault(RPC_INTERNAL_ERROR, "Saved conversation context is malformed")
        session = response["session"]
        if session.get("id") != session_id or session.get("serverId") != server_id:
            raise _RpcFault(RPC_INVALID_PARAMS, "Saved conversation belongs to a different session or server")
        messages = response["messages"]
        if len(messages) > 1000 or len(json.dumps(messages, ensure_ascii=False, separators=(",", ":")).encode("utf-8")) > 512 * 1024:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Saved conversation context exceeds its hydration limit")
        history: list[dict[str, Any]] = []
        for message in messages:
            if not isinstance(message, dict):
                raise _RpcFault(RPC_INTERNAL_ERROR, "Saved conversation message is malformed")
            if message.get("id") == request.get("messageId") and request.get("messageId") is not None:
                break
            if message.get("role") not in ("user", "assistant"):
                continue
            if (not isinstance(message.get("id"), str) or not message["id"] or len(message["id"]) > 256
                or not isinstance(message.get("content"), str) or len(message["content"]) > 100_000
                or message.get("sessionId", session_id) != session_id):
                raise _RpcFault(RPC_INTERNAL_ERROR, "Saved conversation message is malformed")
            clean = {key: message[key] for key in ("id", "role", "content", "createdAt", "model") if key in message}
            for field, limit in (("createdAt", 80), ("model", 256)):
                if field in clean and (not isinstance(clean[field], str) or not clean[field].strip() or len(clean[field]) > limit):
                    raise _RpcFault(RPC_INTERNAL_ERROR, "Saved conversation metadata is malformed")
            participant = message.get("participant")
            if participant is not None:
                # Reuse the run boundary's structural checks, without granting a
                # legacy subagent new authority or replaying old system prompts.
                checked = _parse_run_request({"runId": "hydrate", "sessionId": session_id, "prompt": "hydrate", "participant": participant})
                clean["participant"] = checked["participant"]
            history.append(clean)
        self._conversations.import_legacy(session_id, server_id, history)

    def _run_stop(self, params: object) -> dict[str, bool]:
        parsed = _strict_object(params, {"runId"})
        run_id = parsed.get("runId")
        if not isinstance(run_id, str) or not run_id.strip() or len(run_id) > 256:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        if self._coordination.stop(run_id):
            return {"stopped": True}
        task = self._active_runs.get(run_id)
        if task is None or task.done():
            return {"stopped": False}
        task.cancel()
        return {"stopped": True}

    def _approval_respond(self, params: object) -> dict[str, bool]:
        parsed = _strict_object(params, {"approvalId", "runId", "decision", "respondedAt"})
        approval_id = parsed.get("approvalId")
        run_id = parsed.get("runId")
        decision = parsed.get("decision")
        responded_at = parsed.get("respondedAt")
        if (
            not isinstance(approval_id, str) or not approval_id.strip() or len(approval_id) > 256
            or not isinstance(run_id, str) or not run_id.strip() or len(run_id) > 256
            or decision not in ("approve_once", "reject")
            or not isinstance(responded_at, str) or not responded_at.strip() or len(responded_at) > 80
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid approval response")
        pending = self._pending_approvals.get(approval_id)
        if pending is None or pending.run_id != run_id or pending.future.done():
            return {"accepted": False}
        self._conversations.record_event(run_id, {"type": "approval.decision", **parsed})
        pending.future.set_result(decision)
        return {"accepted": True}

    async def _delete_context_checkpoints(self, session_id: str | None = None) -> None:
        if self._checkpointer is None:
            return
        delete = getattr(self._checkpointer, "adelete_thread", None)
        if delete is None:
            raise _RpcFault(RPC_INTERNAL_ERROR, "The checkpoint store cannot reset model context")
        for binding in self._conversations.checkpoint_bindings(session_id):
            material = "\x1f".join(("local", _safe_context_id("session", binding["sessionId"]),
                                      _safe_context_id("task", binding["taskId"]), binding["runId"]))
            await delete("agent-" + hashlib.sha256(material.encode("utf-8")).hexdigest())

    async def _context_reset(self, params: object, *, delete: bool = False) -> dict[str, bool]:
        parsed = _strict_object(params, {"sessionId", "focusServerId"})
        session_id = parsed.get("sessionId")
        server_id = parsed.get("focusServerId")
        if not isinstance(session_id, str) or not session_id.strip() or len(session_id) > 256 or server_id is not None and not _valid_server_id(server_id):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid context reset scope")
        if session_id in self._run_sessions.values() or self._coordination.session_active(session_id):
            raise _RpcFault(RPC_INVALID_PARAMS, "Stop the active session run before resetting its context")
        try:
            self._conversations.bind_session(session_id, server_id)
        except ValueError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        if self._resetting_all or session_id in self._resetting_sessions:
            raise _RpcFault(RPC_INVALID_PARAMS, "A context reset is already in progress")
        self._resetting_sessions.add(session_id)
        try:
            await self._delete_context_checkpoints(session_id)
            if delete:
                self._conversations.delete_session(session_id, server_id)
                # Removing the visible conversation and starting a new one must
                # not inherit this process's duplicate admission receipts either.
                for message_id, admission in list(self._admissions.items()):
                    if admission["sessionId"] == session_id:
                        self._admissions.pop(message_id, None)
                return {"deleted": True}
            self._conversations.reset(session_id, server_id)
            return {"reset": True}
        finally:
            self._resetting_sessions.discard(session_id)

    async def _context_reset_all(self, params: object) -> dict[str, Any]:
        _strict_object(params, set())
        if self._run_sessions or self._coordination.active:
            raise _RpcFault(RPC_INVALID_PARAMS, "Stop all active runs before resetting model context")
        if self._resetting_all or self._resetting_sessions:
            raise _RpcFault(RPC_INVALID_PARAMS, "A context reset is already in progress")
        self._resetting_all = True
        try:
            await self._delete_context_checkpoints()
            return {"reset": True, "count": self._conversations.reset_all()}
        finally:
            self._resetting_all = False

    def _memory_list(self, params: object) -> dict[str, Any]:
        parsed = _parse_memory_params(params, "list")
        return {"memories": self._conversations.list_memories(
            server_id=parsed.get("serverId"), scope=parsed.get("scope"), query=parsed.get("query", ""), limit=parsed.get("limit", 100),
        )}

    def _memory_save(self, params: object) -> dict[str, Any]:
        parsed = _parse_memory_params(params, "save")
        try:
            memory = self._conversations.save_memory(scope=parsed["scope"], server_id=parsed.get("serverId"),
                                                      title=parsed["title"], content=parsed["content"], memory_id=parsed.get("id"))
        except ValueError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
        return {"memory": memory}

    def _memory_delete(self, params: object) -> dict[str, bool]:
        parsed = _parse_memory_params(params, "delete")
        return {"deleted": self._conversations.delete_memory(parsed["id"], scope=parsed["scope"], server_id=parsed.get("serverId"))}

    async def _execute_run(
        self,
        request: dict[str, Any],
        run_id: str,
        agent: ModelDrivenAgent,
    ) -> dict[str, Any]:
        lock = self._session_locks.setdefault(request["sessionId"], asyncio.Lock())
        try:
            async with lock:
                return await self._execute_session_run(request, run_id, agent)
        except asyncio.CancelledError:
            # A queued turn can be stopped before it acquires the session lock.
            result = {"runId": run_id, "state": "cancelled", "text": "", "steps": 0, "toolCalls": 0}
            self._finish_run(request, run_id, "cancelled")
            self._emit_terminal({
                "type": "agent.completed", "runId": run_id,
                **({"taskId": request["taskId"]} if request.get("taskId") else {}),
                "result": result, "at": _timestamp(),
            })
            return result
        finally:
            self._run_sessions.pop(run_id, None)
            self._active_runs.pop(run_id, None)
            if not any(session == request["sessionId"] for session in self._run_sessions.values()):
                self._session_locks.pop(request["sessionId"], None)

    async def _execute_session_run(
        self, request: dict[str, Any], run_id: str, agent: ModelDrivenAgent,
    ) -> dict[str, Any]:
        task_id = request.get("taskId")
        final_status = "failed"
        context_usage: dict[str, Any] | None = None
        now = _timestamp
        try:
            # Freeze a fresh status projection at each coordinator turn. Results
            # completed after dispatch are visible without polluting worker history.
            persona = request.get("persona")
            if request.get("coordination"):
                persona = {"id": "coordinator", "label": "协调助手", "systemPrompt": self._coordination.context(request) +
                           "\n当前界面参考：\n" + (request.get("persona") or {}).get("systemPrompt", "")[:4000]}
            available_bytes = agent.conversation_budget_bytes(request["promptText"], persona)
            memory_budget = min(16 * 1024, available_bytes // 8)
            context = self._conversations.context(request["sessionId"], request.get("targetId"), exclude_run=run_id,
                                                  max_bytes=available_bytes - memory_budget, query_text=request["promptText"])
            memories, sources = self._conversations.memory_context(request.get("targetId"), request["promptText"], max_bytes=memory_budget)
            context_usage = {**context.usage, "memorySources": sources}
            context_usage["inputBytes"] = message_bytes([*memories, *context.messages]) if memories or context.messages else 0
            context_usage["maxInputBytes"] = max(1, available_bytes)
            self._conversations.record_event(run_id, {"type": "context.loaded", "context": context_usage})
            self._emit({
                "type": "agent.started",
                "runId": run_id,
                **({"taskId": task_id} if task_id else {}),
                "at": now(),
            })
            target_id = request.get("targetId")
            mode = request.get("mode", "goal")
            allowed_tools = set(_allowed_tools_for_run(self._registry, mode, request))
            result = await agent.run(
                task_id=_safe_context_id("task", task_id or run_id),
                tenant_id="local",
                user_id=_safe_context_id("session", request["sessionId"]),
                prompt=request["promptText"],
                allowed_tools=allowed_tools,
                allowed_target_ids=frozenset({target_id}) if target_id else frozenset(),
                host_target=request.get("hostTarget"),
                host_task_id=task_id,
                plan_id=(request["planBinding"]["planId"] if request.get("planBinding") else None),
                plan_step_id=(request["planBinding"]["planStepId"] if request.get("planBinding") else None),
                evidence_ids=(tuple(request["planBinding"]["evidenceIds"]) if request.get("planBinding") else ()),
                run_id=run_id,
                trace_id=_safe_context_id("trace", run_id),
                checkpoint_binding=_run_checkpoint_binding(request),
                require_checkpoint=request["requireCheckpoint"],
                persona=persona,
                conversation_messages=[*memories, *context.messages],
            )
            while result.status == "awaiting_approval":
                result = await self._await_approval(request, run_id, agent, result)
            text = result.answer[:200_000]
            base_result: dict[str, Any] = {
                "runId": run_id,
                "state": "completed" if result.status == "succeeded" else "failed",
                "text": text,
                "steps": result.tool_calls + (1 if result.status == "succeeded" else 0),
                "toolCalls": result.tool_calls,
                "context": context_usage,
            }
            final_status = result.status
            if result.status == "succeeded":
                # Model text already streamed. The full terminal answer is a
                # reconciliation snapshot, not another append-only delta.
                self._emit_terminal({
                    "type": "agent.completed",
                    "runId": run_id,
                    **({"taskId": task_id} if task_id else {}),
                    "result": base_result,
                    "at": now(),
                })
            else:
                message = result.error_code or "agent_run_failed"
                base_result["error"] = message
                self._emit_terminal({
                    "type": "agent.failed",
                    "runId": run_id,
                    **({"taskId": task_id} if task_id else {}),
                    "error": message,
                    "at": now(),
                })
            return base_result
        except asyncio.CancelledError:
            final_status = "cancelled"
            result = {"runId": run_id, "state": "cancelled", "text": "", "steps": 0, "toolCalls": 0,
                      **({"context": context_usage} if context_usage is not None else {})}
            self._emit_terminal({
                "type": "agent.completed",
                "runId": run_id,
                **({"taskId": task_id} if task_id else {}),
                "result": result,
                "at": now(),
            })
            return result
        except Exception:
            final_status = "failed"
            message = "agent_run_failed"
            result = {"runId": run_id, "state": "failed", "text": "", "steps": 0, "toolCalls": 0, "error": message,
                      **({"context": context_usage} if context_usage is not None else {})}
            self._emit_terminal({
                "type": "agent.failed",
                "runId": run_id,
                **({"taskId": task_id} if task_id else {}),
                "error": message,
                "at": now(),
            })
            return result
        finally:
            self._finish_run(request, run_id, final_status)

    def _finish_run(self, request: dict[str, Any], run_id: str, status: str) -> None:
        # A failing transcript store must not retain active reservations or invite
        # an automatic replay. Never print the underlying error or run payload.
        try:
            self._conversations.finish(run_id, status)
        except Exception:
            print("Agent run status could not be saved to conversation history", file=sys.stderr, flush=True)
        self._coordination.parents.pop(run_id, None)
        for approval_id, pending in list(self._pending_approvals.items()):
            if pending.run_id == run_id:
                self._pending_approvals.pop(approval_id, None)
                if not pending.future.done():
                    pending.future.cancel()
        self._active_runs.pop(run_id, None)
        self._completed_runs[run_id] = _run_fingerprint(request)
        self._completed_runs.move_to_end(run_id)
        while len(self._completed_runs) > 256:
            self._completed_runs.popitem(last=False)
        message_id = request.get("messageId")
        admission = self._admissions.get(message_id) if message_id else None
        if admission and admission["runId"] == run_id:
            admission["completed"] = True
            self._prune_admissions()

    def _emit_terminal(self, event: dict[str, Any]) -> None:
        # Exactly one terminal delivery attempt, even if history persistence or
        # coordinator bookkeeping fails. Ordinary events still fail closed.
        try:
            self._coordination.observe(event)
        except Exception:
            print("Agent terminal coordinator status could not be saved", file=sys.stderr, flush=True)
        try:
            self._conversations.record_event(event["runId"], event)
        except Exception:
            print("Agent terminal event could not be saved to conversation history", file=sys.stderr, flush=True)
        if self._notification_sink is not None:
            try:
                self._notification_sink("agent.stream", event)
            except Exception:
                print("Agent terminal notification could not be delivered", file=sys.stderr, flush=True)

    def _prune_admissions(self) -> None:
        while len(self._admissions) > 256:
            for message_id, admission in list(self._admissions.items()):
                if admission["completed"]:
                    del self._admissions[message_id]
                    break
            else:
                return

    def _emit(self, event: dict[str, Any]) -> None:
        if event.get("type") != "agent.team_task" and hasattr(self, "_coordination"):
            self._coordination.observe(event)
        if isinstance(event.get("runId"), str):
            self._conversations.record_event(event["runId"], event)
        if self._notification_sink is not None:
            self._notification_sink("agent.stream", event)

    async def _await_approval(
        self,
        request: dict[str, Any],
        run_id: str,
        agent: ModelDrivenAgent,
        result: Any,
    ) -> Any:
        if result.checkpoint_id is None or not result.approval_requests:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Agent paused without a resumable approval")
        approval = result.approval_requests[0]
        host_input = approval.host_input
        pending_call = next(
            (
                item for item in reversed(result.tool_results)
                if isinstance(item, dict) and item.get("status") == "awaiting_approval"
            ),
            None,
        )
        call_id = pending_call.get("call_id") if isinstance(pending_call, dict) else None
        if not isinstance(call_id, str) or not call_id:
            raise _RpcFault(RPC_INTERNAL_ERROR, "Agent approval is missing its tool call identity")

        fingerprint = approval.fingerprint
        plan_binding = request.get("planBinding")
        if approval.tool_name in PLAN_BOUND_HOST_ACTION_TOOLS and not isinstance(plan_binding, dict):
            raise _RpcFault(RPC_INTERNAL_ERROR, "host action approval is missing its durable plan binding")
        if approval.tool_name in HOST_APPROVAL_TOOLS and not isinstance(host_input, dict):
            raise _RpcFault(RPC_INTERNAL_ERROR, "host action approval is missing its exact input")
        approval_id = "apr_" + hashlib.sha256(
            f"{run_id}\x1f{call_id}\x1f{fingerprint}".encode("utf-8")
        ).hexdigest()[:32]
        loop = asyncio.get_running_loop()
        future: asyncio.Future[str] = loop.create_future()
        self._pending_approvals[approval_id] = _PendingApproval(run_id, result.checkpoint_id, fingerprint, future)
        target = request.get("hostTarget")
        if not isinstance(target, dict):
            target = {"host": "local", "environment": "unknown"}
        expires_at = (datetime.now(UTC) + timedelta(seconds=self._approval_ttl_seconds)).isoformat()
        self._emit({
            "type": "agent.waiting_approval",
            "runId": run_id,
            **({"taskId": request["taskId"]} if request.get("taskId") else {}),
            "approval": {
                "approvalId": approval_id,
                "runId": run_id,
                "traceId": _safe_context_id("trace", run_id),
                "callId": call_id,
                "inputFingerprint": fingerprint,
                "toolName": approval.tool_name,
                "input": approval.arguments,
                **({"hostInput": host_input} if approval.tool_name in HOST_APPROVAL_TOOLS else {}),
                "reason": f"{approval.tool_name} needs your approval before it can run.",
                "factsSummary": [f"Risk level: {approval.risk_level}", "The tool has not run yet."],
                "target": dict(target),
                "expiresAt": expires_at,
                "sessionGrantable": False,
                **({
                    "planId": plan_binding["planId"],
                    "planStepId": plan_binding["planStepId"],
                    "evidenceIds": list(plan_binding["evidenceIds"]),
                } if approval.tool_name in PLAN_BOUND_HOST_ACTION_TOOLS and isinstance(plan_binding, dict) else {}),
            },
            "at": _timestamp(),
        })
        try:
            decision = await asyncio.wait_for(future, timeout=self._approval_ttl_seconds)
        except TimeoutError:
            self._emit({"type": "agent.approval_expired", "runId": run_id, "approvalId": approval_id, "at": _timestamp()})
            decision = "reject"
            rejection_code = "approval_timeout"
        else:
            rejection_code = "approval_rejected"
        finally:
            self._pending_approvals.pop(approval_id, None)
        resume_args = (
            result.checkpoint_id,
            fingerprint,
            approval_id if decision == "approve_once" else None,
        )
        if decision == "approve_once":
            return await agent.resume_after_approval(*resume_args, approved=True)
        return await agent.resume_after_approval(
            *resume_args,
            approved=False,
            rejection_code=rejection_code,
        )

    async def shutdown(self) -> None:
        await self._coordination.shutdown()
        active = list(self._active_runs.values())
        for task in active:
            task.cancel()
        if active:
            await asyncio.gather(*active, return_exceptions=True)

    def _initialize(self, params: object) -> dict[str, Any]:
        if self._initialized:
            raise _RpcFault(RPC_INVALID_REQUEST, "initialize has already completed")
        parsed = _strict_object(params, {"protocolVersion", "clientVersion", "dataDir"})
        if (
            parsed.get("protocolVersion") != RPC_VERSION
            or not isinstance(parsed.get("clientVersion"), str)
            or not parsed["clientVersion"].strip()
            or len(parsed["clientVersion"]) > 128
            or not isinstance(parsed.get("dataDir"), str)
            or len(parsed["dataDir"]) > 4096
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        self._initialized = True
        return {
            "protocolVersion": RPC_VERSION,
            "agentVersion": self._agent_version,
            "capabilities": {
                "streaming": True,
                "toolCalling": bool(self._registry.list()),
                "cancellation": True,
                "mcp": self._mcp_loaded,
            },
        }

    @staticmethod
    def _ping(params: object) -> dict[str, Any]:
        parsed = _strict_object(params, {"echo"})
        echo = parsed.get("echo", "pong")
        if not isinstance(echo, str) or len(echo) > 1000:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        return {"pong": echo, "agentPid": os.getpid(), "agentParentPid": os.getppid()}

    @staticmethod
    def _normalize_snapshot(params: object) -> dict[str, Any]:
        parsed = _strict_object(params, {"outputs"})
        outputs = parsed.get("outputs")
        if (
            not isinstance(outputs, dict)
            or not set(SYSTEM_SNAPSHOT_COMMAND_IDS) <= set(outputs)
            or not set(outputs) <= set(SYSTEM_SNAPSHOT_COMMAND_IDS + NETWORK_SNAPSHOT_COMMAND_IDS)
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid snapshot command set")
        normalized_outputs: dict[str, tuple[int, str]] = {}
        total_bytes = 0
        for command_id in outputs:
            result = outputs[command_id]
            if not isinstance(result, dict) or set(result) != {"exitCode", "stdout"}:
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid snapshot command result")
            exit_code = result["exitCode"]
            stdout = result["stdout"]
            if (
                isinstance(exit_code, bool)
                or not isinstance(exit_code, int)
                or not 0 <= exit_code <= 255
                or not isinstance(stdout, str)
            ):
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid snapshot command result")
            total_bytes += len(stdout.encode("utf-8"))
            normalized_outputs[command_id] = (exit_code, stdout)
        if total_bytes > 64 * 1024:
            raise _RpcFault(RPC_INVALID_PARAMS, "snapshot command output exceeds its limit")
        try:
            snapshot_data = normalize_system_snapshot(normalized_outputs)
        except ValueError:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid snapshot command output") from None
        return {"snapshotData": snapshot_data}

    @staticmethod
    def _normalize_server_logs(params: object) -> dict[str, Any]:
        return _normalize_ssh_inventory(params, _parse_server_logs, {"sinceSeconds", "unit"}, "logs")

    @staticmethod
    def _normalize_server_services(params: object) -> dict[str, Any]:
        return _normalize_ssh_inventory(params, _parse_service_inventory, {"name", "state"}, "services")

    @staticmethod
    def _normalize_docker_ps(params: object) -> dict[str, Any]:
        return _normalize_ssh_parser(
            params, set(), lambda stdout, _arguments: _parse_docker_ps(stdout)
        )

    @staticmethod
    def _normalize_docker_logs(params: object) -> dict[str, Any]:
        return _normalize_ssh_inventory(
            params,
            lambda stdout, arguments: _parse_docker_logs(
                stdout, arguments["container"], arguments["tail"]
            ),
            {"container", "tail"},
            "docker_logs",
        )

    @staticmethod
    def _normalize_docker_inspect(params: object) -> dict[str, Any]:
        return _normalize_ssh_inventory(
            params,
            lambda stdout, _arguments: _parse_docker_inspect(stdout),
            {"container"},
            "docker_inspect",
        )

    @staticmethod
    def _normalize_systemd_inspect(params: object) -> dict[str, Any]:
        return _normalize_ssh_inventory(
            params,
            lambda stdout, arguments: _parse_systemd_inspect(stdout, arguments["service"]),
            {"service"},
            "systemd_inspect",
        )

    @staticmethod
    def _normalize_package_inspect(params: object) -> dict[str, Any]:
        return _normalize_ssh_inventory(
            params,
            lambda stdout, arguments: _parse_package_inspect(
                stdout, arguments["manager"], arguments["package"]
            ),
            {"manager", "package"},
            "package_inspect",
        )

    @staticmethod
    def _declaration(spec: Any) -> dict[str, Any]:
        return {
            "name": spec.name,
            "description": spec.description,
            "risk": {"read": "read", "write": "medium", "destructive": "critical"}[spec.risk_level],
            "timeoutMs": spec.timeout_ms,
            "cancellable": spec.cancellable,
            "retry": {"maxAttempts": 1, "backoffMs": 0},
            "inputSchema": spec.input_schema,
            "origin": spec.origin or {"kind": "builtin"},
            "effectful": spec.risk_level != "read",
        }


def _strict_object(value: object, allowed_keys: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if set(value) - allowed_keys:
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    return value


def _normalize_ssh_inventory(
    params: object,
    parser: Callable[[str, dict[str, Any]], dict[str, object]],
    allowed_arguments: set[str],
    kind: str,
) -> dict[str, Any]:
    parsed = _strict_object(params, {"stdout", "arguments"})
    stdout = parsed.get("stdout")
    arguments = _strict_object(parsed.get("arguments"), allowed_arguments)
    if not isinstance(stdout, str) or len(stdout.encode("utf-8")) > 1024 * 1024:
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid SSH command output")

    if kind == "logs":
        since_seconds = arguments.get("sinceSeconds")
        if since_seconds is not None and (
            isinstance(since_seconds, bool)
            or not isinstance(since_seconds, int)
            or not 1 <= since_seconds <= 604_800
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid server.logs filters")
        unit = arguments.get("unit")
        if unit is not None and (
            not isinstance(unit, str)
            or len(unit) > 128
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.@:-]*\.service", unit) is None
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid server.logs filters")
    elif kind == "services":
        state = arguments.get("state")
        if state is not None and (
            not isinstance(state, str)
            or state not in {"running", "stopped", "failed", "unknown"}
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid server.services filters")
    elif kind == "docker_logs":
        container = arguments.get("container")
        tail = arguments.get("tail")
        if (
            not isinstance(container, str)
            or len(container) > 128
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", container) is None
            or isinstance(tail, bool)
            or not isinstance(tail, int)
            or not 1 <= tail <= 500
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid docker.logs arguments")
    elif kind == "docker_inspect":
        container = arguments.get("container")
        if (
            not isinstance(container, str)
            or len(container) > 128
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", container) is None
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid docker.inspect arguments")
    elif kind == "systemd_inspect":
        service = arguments.get("service")
        if (
            not isinstance(service, str)
            or len(service) > 128
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.@:-]*\.service", service) is None
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid systemd.inspect arguments")
    elif kind == "package_inspect":
        manager = arguments.get("manager")
        package = arguments.get("package")
        if (
            not isinstance(manager, str)
            or manager not in {"apt", "dnf"}
            or not isinstance(package, str)
            or len(package) > 128
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9+._:-]*", package) is None
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid package.inspect arguments")

    try:
        output = parser(stdout, arguments)
    except ToolCallError as error:
        return {"ok": False, "code": error.code, "message": str(error)}
    return {"ok": True, "output": output}


def _normalize_ssh_parser(
    params: object,
    allowed_arguments: set[str],
    parser: Callable[[str, dict[str, Any]], Any],
) -> dict[str, Any]:
    parsed = _strict_object(params, {"stdout", "arguments"})
    stdout = parsed.get("stdout")
    arguments = _strict_object(parsed.get("arguments"), allowed_arguments)
    if not isinstance(stdout, str) or len(stdout.encode("utf-8")) > 1024 * 1024:
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid SSH command output")
    return _parser_result(parser, stdout, arguments)


def _parser_result(
    parser: Callable[[str, dict[str, Any]], Any],
    stdout: str,
    arguments: dict[str, Any],
) -> dict[str, Any]:
    try:
        output = parser(stdout, arguments)
    except ToolCallError as error:
        return {"ok": False, "code": error.code, "message": str(error)}
    return {"ok": True, "output": output}


def _parse_runtime_model_config(params: object) -> tuple[ModelConfig, str | None, int]:
    fields = {"kind", "baseUrl", "model", "apiKey", "customHeaders", "apiVersion", "timeoutMs", "wireApi"}
    parsed = _strict_object(params, fields)
    kind = parsed.get("kind")
    base_url = parsed.get("baseUrl")
    model_name = parsed.get("model")
    api_key = parsed.get("apiKey")
    custom_headers = parsed.get("customHeaders")
    api_version = parsed.get("apiVersion")
    wire_api = parsed.get("wireApi")
    timeout_ms = parsed.get("timeoutMs", 30_000)
    optional_strings = {"apiKey": api_key, "apiVersion": api_version, "wireApi": wire_api}
    if (
        kind not in ("openai-compatible", "anthropic", "gemini")
        or not isinstance(base_url, str)
        or len(base_url) > 2048
        or not isinstance(model_name, str)
        or len(model_name) > 256
        or any(name in parsed and not isinstance(value, str) for name, value in optional_strings.items())
        or "customHeaders" in parsed and not isinstance(custom_headers, dict)
        or isinstance(timeout_ms, bool)
        or not isinstance(timeout_ms, int)
        or not 100 <= timeout_ms <= 600_000
    ):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    base_url = base_url.strip()
    model_name = model_name.strip()
    api_version = api_version.strip() if api_version is not None else None
    wire_api = wire_api.strip() if wire_api is not None else None
    try:
        config = ModelConfig(
            provider=kind,
            model=model_name,
            temperature=0,
            timeout_seconds=timeout_ms / 1000,
            max_retries=0,
            max_output_tokens=256,
            base_url=base_url,
            custom_headers=custom_headers,
            api_version=api_version,
            wire_api=wire_api,
        )
    except (TypeError, ValueError):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params") from None
    if kind in ("anthropic", "gemini") and not api_key:
        raise _RpcFault(RPC_INVALID_PARAMS, "Configured model API key is unavailable")
    if api_key is not None and (len(api_key) > 4096 or "\r" in api_key or "\n" in api_key):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    return config, api_key, timeout_ms


def _provider_store_fault(error: ProviderStoreError) -> _RpcFault:
    code = RPC_INVALID_PARAMS if error.code == "invalid_params" else RPC_INTERNAL_ERROR
    if error.code == "not_found":
        message = str(error)
    elif error.code == "conflict":
        message = str(error)
    else:
        message = str(error)
    return _RpcFault(code, message)


def _is_local_provider_endpoint(value: str) -> bool:
    normalized = value.strip().lower()
    prefixes = (
        "http://localhost", "https://localhost",
        "http://127.0.0.1", "https://127.0.0.1",
        "http://[::1]", "https://[::1]",
    )
    return any(
        normalized == prefix
        or normalized.startswith(f"{prefix}:")
        or normalized.startswith(f"{prefix}/")
        for prefix in prefixes
    )


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request: Request, file: Any, code: int, message: str, headers: Any, new_url: str) -> None:
        return None


def _fetch_model_catalog(config: ModelConfig, api_key: str | None, timeout_ms: int) -> list[dict[str, Any]]:
    base_url = config.base_url.rstrip("/") if config.base_url else ""
    if config.provider == "openai-compatible":
        endpoint = f"{base_url}/models"
    elif config.provider == "anthropic":
        endpoint = f"{base_url}/v1/models"
    else:
        endpoint = f"{base_url}/v1beta/models"

    headers = dict(config.custom_headers or {})
    headers["Accept"] = "application/json"
    if config.provider == "openai-compatible":
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
    elif config.provider == "anthropic":
        headers["anthropic-version"] = config.api_version or "2023-06-01"
        headers["x-api-key"] = api_key or ""
    elif api_key:
        headers["x-goog-api-key"] = api_key

    request = Request(endpoint, headers=headers, method="GET")
    opener = build_opener(ProxyHandler({}), _NoRedirect())
    try:
        with opener.open(request, timeout=timeout_ms / 1000) as response:
            if response.status != 200:
                raise _RpcFault(RPC_INTERNAL_ERROR, f"Provider model catalog request failed (HTTP {response.status})")
            body = response.read(4 * 1024 * 1024 + 1)
    except HTTPError as error:
        raise _RpcFault(RPC_INTERNAL_ERROR, f"Provider model catalog request failed (HTTP {error.code})") from None
    except (URLError, TimeoutError, OSError):
        raise _RpcFault(RPC_INTERNAL_ERROR, "Provider model catalog request failed") from None
    if len(body) > 4 * 1024 * 1024:
        raise _RpcFault(RPC_INTERNAL_ERROR, "Provider model catalog response exceeds the size limit")
    try:
        payload = json.loads(body.decode("utf-8"))
        models = _map_provider_models(config.provider, payload)
    except (UnicodeDecodeError, json.JSONDecodeError, TypeError, ValueError, KeyError):
        raise _RpcFault(RPC_INTERNAL_ERROR, "Provider returned an invalid model catalog") from None
    if len(models) > 1000:
        raise _RpcFault(RPC_INTERNAL_ERROR, "Provider model catalog exceeds the entry limit")
    return models


def _map_provider_models(provider: str, payload: object) -> list[dict[str, Any]]:
    if not isinstance(payload, dict):
        raise ValueError("invalid catalog")
    if provider == "openai-compatible":
        if set(payload) - {"data", "object"}:
            raise ValueError("invalid catalog")
        entries = payload.get("data", [])
        label_key = "id"
        id_key = "id"
    elif provider == "anthropic":
        if set(payload) - {"data", "has_more", "first_id", "last_id"}:
            raise ValueError("invalid catalog")
        entries = payload.get("data")
        label_key = "display_name"
        id_key = "id"
    else:
        entries = payload.get("models")
        label_key = "displayName"
        id_key = "name"
    if not isinstance(entries, list):
        raise ValueError("invalid catalog")
    models: list[dict[str, Any]] = []
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("invalid model entry")
        if provider == "gemini":
            methods = entry.get("supportedGenerationMethods")
            if not isinstance(methods, list) or "generateContent" not in methods:
                continue
        model_id = entry.get(id_key)
        label = entry.get(label_key)
        if not isinstance(model_id, str) or not model_id.strip():
            raise ValueError("invalid model id")
        if provider == "gemini" and model_id.startswith("models/"):
            model_id = model_id.removeprefix("models/")
        model_id = model_id.strip()
        if len(model_id) > 256:
            raise ValueError("model id exceeds the limit")
        if label is not None and (not isinstance(label, str) or not label.strip()):
            raise ValueError("invalid model label")
        if label is None:
            label = model_id
        label = label.strip()
        if len(label) > 256:
            raise ValueError("model label exceeds the limit")
        result: dict[str, Any] = {
            "id": model_id,
            "label": label,
            "supportsToolCalling": True,
            "supportsStreaming": True,
        }
        context_window = entry.get("inputTokenLimit") if provider == "gemini" else None
        if isinstance(context_window, int) and not isinstance(context_window, bool) and 0 < context_window <= 10_000_000:
            result["contextWindow"] = context_window
        models.append(result)
    return models


def _parse_run_request(value: object) -> dict[str, Any]:
    fields = {
        "runId", "sessionId", "taskId", "taskBudget", "prompt", "messageId", "parts", "delivery",
        "resume", "workspaceId", "focusServerId", "target", "policyId", "permissionMode", "mode",
        "providerId", "model", "reasoningEffort", "toolAccess", "requireCheckpoint", "planBinding",
        "persona", "participant", "coordination",
    }
    parsed = _strict_object(value, fields)
    if "coordination" in parsed:
        try:
            parsed["coordination"] = parse_coordination(parsed["coordination"])
        except ValueError as error:
            raise _RpcFault(RPC_INVALID_PARAMS, str(error)) from None
    if "persona" in parsed:
        persona = _strict_object(parsed["persona"], {"id", "label", "systemPrompt"})
        for field, limit in (("id", 256), ("label", 200), ("systemPrompt", 20_000)):
            item = persona.get(field)
            if not isinstance(item, str) or not item.strip() or len(item) > limit:
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid persona")
        parsed["persona"] = {field: item.strip() for field, item in persona.items()}
    if "participant" in parsed:
        participant = _strict_object(parsed["participant"], {"id", "label", "kind", "parentId"})
        if participant.get("kind") not in ("agent", "subagent"):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid participant")
        for field, limit in (("id", 256), ("label", 200), ("parentId", 256)):
            if field == "parentId" and field not in participant:
                continue
            item = participant.get(field)
            if not isinstance(item, str) or not item.strip() or len(item) > limit:
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid participant")
        parsed["participant"] = {field: item.strip() for field, item in participant.items()}
    run_id = parsed.get("runId")
    session_id = parsed.get("sessionId")
    prompt = parsed.get("prompt")
    if (
        not isinstance(run_id, str) or not run_id.strip() or len(run_id) > 256
        or not isinstance(session_id, str) or not session_id.strip() or len(session_id) > 256
        or not isinstance(prompt, str) or len(prompt) > 100_000
    ):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    for field in ("taskId", "messageId", "workspaceId", "focusServerId"):
        item = parsed.get(field)
        if field in parsed and (not isinstance(item, str) or not item.strip() or len(item) > 256):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    provider_id = parsed.get("providerId")
    if provider_id is not None and (
        not isinstance(provider_id, str) or not provider_id.strip() or len(provider_id) > 256
    ):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    model = parsed.get("model")
    if model is not None and (
        not isinstance(model, str) or len(model) > 256 or "\r" in model or "\n" in model
    ):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if parsed.get("delivery", "async") not in ("async", "sync"):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if not isinstance(parsed.get("resume", True), bool):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if not isinstance(parsed.get("requireCheckpoint", False), bool):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if parsed.get("permissionMode", "ask") not in ("ask", "auto"):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if "toolAccess" in parsed and parsed["toolAccess"] != "none":
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid tool access")
    if "reasoningEffort" in parsed and parsed["reasoningEffort"] not in ("none", "low", "high", "max"):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid reasoning effort")
    if parsed.get("mode", "goal") not in ("goal", "plan", "readonly"):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if "policyId" in parsed:
        if not isinstance(parsed["policyId"], str) or not parsed["policyId"].strip() or len(parsed["policyId"]) > 256:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        raise _RpcFault(RPC_INVALID_PARAMS, "policy selection is not supported by this Python Agent Node yet")

    target_id = parsed.get("focusServerId")
    if target_id is not None and not _valid_server_id(target_id):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    target = parsed.get("target")
    host_target: dict[str, Any] | None = None
    if "target" in parsed:
        target = _strict_object(target, {"host", "serverId", "workspaceId", "environment"})
        if (
            target.get("host") not in ("local", "remote")
            or target.get("environment") not in ("local", "development", "staging", "production", "unknown")
            or "workspaceId" in target
            and (not isinstance(target["workspaceId"], str) or not target["workspaceId"].strip() or len(target["workspaceId"]) > 256)
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        scoped_server = target.get("serverId")
        if "serverId" in target and not isinstance(scoped_server, str):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        if target["host"] == "remote" and not _valid_server_id(scoped_server):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        if target["host"] == "local" and scoped_server is not None:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        if scoped_server is not None and target_id is not None and scoped_server != target_id:
            raise _RpcFault(RPC_INVALID_PARAMS, "target conflicts with focusServerId")
        target_id = scoped_server or target_id
        if target["host"] == "local" and target_id is not None:
            raise _RpcFault(RPC_INVALID_PARAMS, "focusServerId conflicts with a local target")
        host_target = {
            "host": target["host"],
            **({"serverId": scoped_server} if scoped_server is not None else {}),
            **({"workspaceId": target["workspaceId"]} if target.get("workspaceId") is not None else {}),
            "environment": target["environment"],
        }
    elif target_id is not None:
        host_target = {"host": "remote", "serverId": target_id, "environment": "unknown"}
    else:
        host_target = {
            "host": "local",
            "environment": "local",
            **({"workspaceId": parsed["workspaceId"]} if parsed.get("workspaceId") else {}),
        }

    plan_binding: dict[str, Any] | None = None
    if "planBinding" in parsed:
        raw_binding = _strict_object(parsed["planBinding"], {"planId", "planStepId", "evidenceIds"})
        plan_id = raw_binding.get("planId")
        plan_step_id = raw_binding.get("planStepId")
        evidence_ids = raw_binding.get("evidenceIds")
        def valid_identifier(item: object) -> bool:
            return isinstance(item, str) and bool(
                re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:@/-]{0,255}", item)
            )
        if (
            not valid_identifier(plan_id)
            or not valid_identifier(plan_step_id)
            or not isinstance(evidence_ids, list)
            or len(evidence_ids) > 128
            or any(not valid_identifier(item) for item in evidence_ids)
            or len(set(evidence_ids)) != len(evidence_ids)
            or not parsed.get("taskId")
            or not isinstance(host_target, dict)
            or host_target.get("host") != "remote"
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid durable plan binding")
        plan_binding = {
            "planId": plan_id,
            "planStepId": plan_step_id,
            "evidenceIds": list(evidence_ids),
        }

    if "parts" in parsed and not isinstance(parsed["parts"], list):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid prompt parts")
    if "taskBudget" in parsed and not isinstance(parsed["taskBudget"], dict):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid taskBudget")
    prompt_text = _prompt_text(prompt, parsed.get("parts"))
    if not prompt_text.strip() or len(prompt_text.encode("utf-8")) > 64 * 1024:
        raise _RpcFault(RPC_INVALID_PARAMS, "prompt is empty or exceeds the Python Agent input limit")
    task_budget = _run_budget(parsed.get("taskBudget"))
    return {
        **parsed,
        "runId": run_id.strip(),
        "sessionId": session_id.strip(),
        "delivery": parsed.get("delivery", "async"),
        "resume": parsed.get("resume", True),
        "providerId": provider_id.strip() if isinstance(provider_id, str) else None,
        "model": model.strip() if isinstance(model, str) else None,
        "requireCheckpoint": parsed.get("requireCheckpoint", False),
        "promptText": prompt_text,
        "targetId": target_id,
        "hostTarget": host_target,
        **({"planBinding": plan_binding} if plan_binding is not None else {}),
        "agentBudget": task_budget,
    }


def _allowed_tool_names(registry: ToolRegistry, mode: str) -> frozenset[str]:
    return frozenset(
        spec.name for spec in registry.list()
        if (spec.risk_level == "read" and not spec.requires_approval)
        or (
            mode == "goal"
            and spec.risk_level in ("write", "destructive")
            and spec.requires_approval
        )
    )


def _allowed_tools_for_run(
    registry: ToolRegistry,
    mode: str,
    request: dict[str, Any],
) -> frozenset[str]:
    if request.get("toolAccess") == "none":
        return frozenset()
    if request.get("coordination"):
        return COORDINATOR_TOOLS
    allowed = set(_allowed_tool_names(registry, mode)) - COORDINATOR_TOOLS
    if not _host_action_has_plan_binding(request):
        allowed.difference_update(PLAN_BOUND_HOST_ACTION_TOOLS)
    return frozenset(allowed)


def _prompt_text(prompt: str, parts: object) -> str:
    if parts is None:
        return prompt.strip()
    if not isinstance(parts, list) or not 1 <= len(parts) <= 32:
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid prompt parts")
    text_parts: list[str] = []
    files: list[tuple[str, str]] = []
    text_size = 0
    for part in parts:
        if not isinstance(part, dict) or not isinstance(part.get("type"), str):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid prompt parts")
        if part["type"] == "text":
            parsed = _strict_object(part, {"type", "text"})
            if not isinstance(parsed.get("text"), str) or not parsed["text"]:
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid prompt parts")
            text_parts.append(parsed["text"])
            text_size += len(parsed["text"].encode("utf-8"))
        elif part["type"] == "file":
            parsed = _strict_object(part, {"type", "mediaType", "data", "name"})
            name, data = parsed.get("name"), parsed.get("data")
            if (
                parsed.get("mediaType") != "text/plain"
                or not isinstance(name, str) or not name.strip()
                or len(name) > 256
                or not isinstance(data, str) or not data
                or re.search(r"[\x00-\x1f\x7f]", name)
                or "/" in name or "\\" in name
                or re.search(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]", data)
            ):
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid prompt parts")
            files.append((name.strip(), data))
            text_size += len(data.encode("utf-8"))
        else:
            raise _RpcFault(RPC_INVALID_PARAMS, "image, document, and audio parts are not supported by the Python Agent Node yet")
        if text_size > 64 * 1024:
            raise _RpcFault(RPC_INVALID_PARAMS, "prompt parts exceed the Python Agent input limit")
    prompt_text = "\n".join(text_parts).strip() or prompt.strip()
    file_blocks = [
        f"--- BEGIN ATTACHED TEXT FILE: {name} ---\n{data}\n--- END ATTACHED TEXT FILE ---"
        for name, data in files
    ]
    return "\n\n".join([prompt_text, *file_blocks]).strip()


def _run_budget(value: object) -> AgentBudget:
    max_steps = 8
    max_run_ms = 120_000
    if value is not None:
        parsed = _strict_object(value, {"maxSteps", "maxRunMs", "maxAttempts"})
        max_steps = parsed.get("maxSteps")
        max_run_ms = parsed.get("maxRunMs")
        max_attempts = parsed.get("maxAttempts")
        if any(isinstance(item, bool) or not isinstance(item, int) for item in (max_steps, max_run_ms, max_attempts)):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid taskBudget")
        if not 1 <= max_steps <= 10_000 or not 1 <= max_run_ms <= 86_400_000 or not 1 <= max_attempts <= 32:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid taskBudget")
    steps = min(max_steps, 8)
    return AgentBudget(
        max_tool_calls=min(12, steps),
        max_model_rounds=steps,
        max_total_seconds=max(0.01, min(120.0, max_run_ms / 1000)),
        max_output_bytes=200_000,
    )


def _run_fingerprint(request: dict[str, Any]) -> str:
    canonical = json.dumps(
        {
            "prompt": request["prompt"],
            "parts": request.get("parts"),
            "requireCheckpoint": request.get("requireCheckpoint", False),
            "planBinding": request.get("planBinding"),
            "sessionId": request["sessionId"],
            "targetId": request.get("targetId"),
            "persona": request.get("persona"),
            "participant": request.get("participant"),
            **({"coordination": request["coordination"]} if "coordination" in request else {}),
            "providerId": request.get("providerId"),
            "model": request.get("model"),
            "reasoningEffort": request.get("reasoningEffort"),
            **({"toolAccess": request["toolAccess"]} if "toolAccess" in request else {}),
            "permissionMode": request.get("permissionMode", "ask"),
            "mode": request.get("mode", "goal"),
            "policyId": request.get("policyId"),
        },
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _run_checkpoint_binding(request: dict[str, Any]) -> str:
    # Bind persisted graph state to the user prompt and authorization scope,
    # while excluding provider credentials so rotated secrets stay out of disk.
    fields = (
        "sessionId", "taskId", "taskBudget", "prompt", "parts", "workspaceId",
        "focusServerId", "target", "policyId", "permissionMode", "mode",
        "planBinding",
        "persona", "participant", "reasoningEffort", "toolAccess", "coordination",
    )
    binding = {field: request[field] for field in fields if field in request}
    budget = request.get("agentBudget")
    if isinstance(budget, AgentBudget):
        binding["agentBudget"] = asdict(budget)
    canonical = json.dumps(binding, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _parse_memory_params(value: object, action: str) -> dict[str, Any]:
    fields = {"scope", "serverId"} | ({"query", "limit"} if action == "list" else {"id", "title", "content"} if action == "save" else {"id"})
    parsed = _strict_object(value, fields)
    scope = parsed.get("scope")
    server_id = parsed.get("serverId")
    if scope not in ((None, "global", "server") if action == "list" else ("global", "server")):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid memory scope")
    if server_id is not None and not _valid_server_id(server_id):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid memory server")
    if scope == "server" and server_id is None or scope == "global" and server_id is not None:
        raise _RpcFault(RPC_INVALID_PARAMS, "server memory requires serverId; global memory cannot have one")
    if "id" in parsed or action == "delete":
        item = parsed.get("id")
        if not isinstance(item, str) or not item.strip() or len(item) > 256:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid memory id")
    for field, limit in (("title", 200), ("content", 12_000)):
        if action == "save":
            item = parsed.get(field)
            if not isinstance(item, str) or not item.strip() or len(item) > limit:
                raise _RpcFault(RPC_INVALID_PARAMS, "invalid memory " + field)
            parsed[field] = item.strip()
    if action == "list":
        query = parsed.get("query", "")
        limit = parsed.get("limit", 100)
        if not isinstance(query, str) or len(query) > 200 or isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= 100:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid memory search")
    return parsed


def _safe_context_id(prefix: str, value: str) -> str:
    return f"{prefix}-{hashlib.sha256(value.encode('utf-8')).hexdigest()[:32]}"


def _host_action_has_plan_binding(request: dict[str, Any]) -> bool:
    binding = request.get("planBinding")
    target = request.get("hostTarget")
    return bool(
        request.get("taskId")
        and isinstance(binding, dict)
        and isinstance(target, dict)
        and target.get("host") == "remote"
    )


def _valid_server_id(value: object) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"srv_[a-z0-9]+", value))


def _timestamp() -> str:
    return datetime.now(UTC).isoformat()


def _failure(request_id: int | None, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


async def serve_stdio(
    sidecar: PythonSidecar,
    *,
    reader: BinaryIO | None = None,
    writer: BinaryIO | None = None,
) -> None:
    """Serve bounded NDJSON frames; stdout is protocol-only and diagnostics use stderr."""
    input_stream = reader or sys.stdin.buffer
    output_stream = writer or sys.stdout.buffer
    write_lock = asyncio.Lock()
    tasks: set[asyncio.Task[None]] = set()

    async def write_frame(frame: dict[str, Any]) -> None:
        encoded = json.dumps(frame, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8") + b"\n"
        if len(encoded) > MAX_SIDECAR_FRAME_BYTES:
            if isinstance(frame.get("method"), str):
                raise ValueError("sidecar request exceeds the frame limit")
            if "id" not in frame:
                print("dropped oversized sidecar notification", file=sys.stderr, flush=True)
                return
            frame_id = frame.get("id")
            encoded = json.dumps(
                _failure(frame_id if isinstance(frame_id, int) else None, RPC_INTERNAL_ERROR, "Response exceeds the frame limit"),
                separators=(",", ":"),
            ).encode("utf-8") + b"\n"
        async with write_lock:
            output_stream.write(encoded)
            output_stream.flush()

    host_rpc = HostRpcClient(write_frame)
    sidecar.set_host_rpc(host_rpc)

    def notify(method: str, params: dict[str, Any]) -> None:
        task = asyncio.create_task(write_frame({"jsonrpc": "2.0", "method": method, "params": params}))
        tasks.add(task)
        task.add_done_callback(tasks.discard)

    sidecar.set_notification_sink(notify)

    async def respond(value: object) -> None:
        result = await sidecar.handle_async(value)
        if result is not None:
            await write_frame(result)

    while True:
        line = await asyncio.to_thread(input_stream.readline, MAX_SIDECAR_FRAME_BYTES + 1)
        if not line:
            break
        if len(line) > MAX_SIDECAR_FRAME_BYTES:
            print("dropped oversized sidecar frame", file=sys.stderr, flush=True)
            if not line.endswith(b"\n"):
                await _discard_to_newline(input_stream)
            continue
        try:
            value = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            print("dropped malformed sidecar frame", file=sys.stderr, flush=True)
            continue
        if host_rpc.handle_response(value):
            continue
        task = asyncio.create_task(respond(value))
        tasks.add(task)
        task.add_done_callback(tasks.discard)
    await sidecar.shutdown()
    host_rpc.close()
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)


async def _discard_to_newline(stream: BinaryIO) -> None:
    while True:
        remainder = await asyncio.to_thread(stream.readline, 64 * 1024)
        if not remainder or remainder.endswith(b"\n"):
            return


async def _serve_with_checkpoint_store(sidecar: PythonSidecar, data_dir: str) -> None:
    if not data_dir:
        await serve_stdio(sidecar)
        return
    from .checkpoints import open_checkpoint_store

    async with open_checkpoint_store(data_dir) as checkpointer:
        sidecar.set_checkpointer(checkpointer)
        try:
            await serve_stdio(sidecar)
        finally:
            sidecar.set_checkpointer(None)


async def _serve_runtime(sidecar: PythonSidecar, data_dir: str, tool_count: int) -> None:
    service = SshServiceProcess.from_environment()
    try:
        if service is not None:
            await service.start()
        print(f"Python Agent Node ready (tools={tool_count})", file=sys.stderr, flush=True)
        await _serve_with_checkpoint_store(sidecar, data_dir)
    finally:
        if service is not None:
            await service.stop()


def main() -> None:
    """Entry point for the Python Agent Node Sidecar."""
    data_dir = os.environ.get("YUKINAL_DATA_DIR", "").strip()
    audit = JsonlAuditSink(Path(data_dir) / "audit" / "python-agent-tools.jsonl") if data_dir else _StderrAuditSink()
    registry = ToolRegistry(audit)
    token = os.environ.get("YUKINAL_SSH_API_TOKEN", "")
    if token:
        try:
            service_url = os.environ.get("YUKINAL_SSH_API_URL", "http://127.0.0.1:8765")
            register_ssh_tools(registry, SshServiceClient(token, base_url=service_url))
        except ValueError:
            print("SSH service client configuration is invalid", file=sys.stderr, flush=True)
    managed_ssh_service_binary = None
    if os.environ.get("YUKINAL_SSH_SERVICE_AUTOCONFIG") == "1":
        binary_value = os.environ.get("YUKINAL_SSH_SERVICE_BINARY", "").strip()
        if binary_value and Path(binary_value).is_file():
            managed_ssh_service_binary = Path(binary_value).expanduser()
        else:
            print("C++ SSH service is unavailable; desktop SSH tools are disabled", file=sys.stderr, flush=True)
    provider_store = ProviderStore(Path(data_dir) / "yukinal.db") if data_dir else None
    sidecar = PythonSidecar(
        registry,
        managed_ssh_service_binary=managed_ssh_service_binary,
        provider_store=provider_store,
        conversation_store=ConversationStore(Path(data_dir) / "agent-state" / "conversations.sqlite") if data_dir else None,
    )
    try:
        asyncio.run(_serve_runtime(sidecar, data_dir, len(registry.list())))
    except SshServiceProcessError as error:
        print(f"SSH service startup failed: {error}", file=sys.stderr, flush=True)
        raise SystemExit(2) from None


class _StderrAuditSink:
    """Fail closed when a desktop did not supply its private data directory."""

    async def record(self, _event: Any) -> None:
        raise RuntimeError("YUKINAL_DATA_DIR is required for tool audit persistence")


if __name__ == "__main__":
    main()
