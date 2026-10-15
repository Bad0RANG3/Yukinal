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
from .host_rpc import HostRpcClient
from .host_tools import (
    HOST_APPROVAL_TOOLS,
    PLAN_BOUND_HOST_ACTION_TOOLS,
    register_desktop_backup_metadata_tools,
    register_host_tools,
    register_mcp_catalog,
)
from .model_agent import AgentBudget, ModelDrivenAgent
from .models import ModelConfig, ModelFactory, ModelFactoryError
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
    "agent.approval.respond": True,
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
        if self._active_runs:
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
            elif method == "agent.approval.respond":
                result = self._approval_respond(params)
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

        if run_id in self._active_runs:
            raise _RpcFault(RPC_INVALID_PARAMS, "runId is already running")
        if run_id in self._completed_runs:
            if self._completed_runs[run_id] != fingerprint:
                raise _RpcFault(RPC_INVALID_PARAMS, "runId was already used with different content")
            return {"runId": run_id, "started": False, "duplicate": True}

        model, _timeout_ms = await self._run_provider_model(
            request.get("providerId"), request.get("model")
        )
        try:
            agent = ModelDrivenAgent(
                self._registry,
                model,
                request["agentBudget"],
                event_sink=self._emit,
                checkpointer=self._checkpointer,
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
        task = asyncio.create_task(self._execute_run(request, run_id, agent))
        self._active_runs[run_id] = task
        if request["delivery"] == "sync":
            result = await task
            response: dict[str, Any] = {"runId": run_id, "started": True, "result": result}
        else:
            response = {"runId": run_id, "started": True}
        if resumed:
            response["resumed"] = True
        return response

    def _run_stop(self, params: object) -> dict[str, bool]:
        parsed = _strict_object(params, {"runId"})
        run_id = parsed.get("runId")
        if not isinstance(run_id, str) or not run_id.strip() or len(run_id) > 256:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
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
        pending.future.set_result(decision)
        return {"accepted": True}

    async def _execute_run(
        self,
        request: dict[str, Any],
        run_id: str,
        agent: ModelDrivenAgent,
    ) -> dict[str, Any]:
        task_id = request.get("taskId")
        now = _timestamp
        self._emit({
            "type": "agent.started",
            "runId": run_id,
            **({"taskId": task_id} if task_id else {}),
            "at": now(),
        })
        try:
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
            }
            if result.status == "succeeded":
                for offset in range(0, len(text), 20_000):
                    self._emit({"type": "agent.text", "runId": run_id, "textDelta": text[offset:offset + 20_000], "at": now()})
                self._emit({
                    "type": "agent.completed",
                    "runId": run_id,
                    **({"taskId": task_id} if task_id else {}),
                    "result": base_result,
                    "at": now(),
                })
            else:
                message = result.error_code or "agent_run_failed"
                base_result["error"] = message
                self._emit({
                    "type": "agent.failed",
                    "runId": run_id,
                    **({"taskId": task_id} if task_id else {}),
                    "error": message,
                    "at": now(),
                })
            return base_result
        except asyncio.CancelledError:
            result = {"runId": run_id, "state": "cancelled", "text": "", "steps": 0, "toolCalls": 0}
            self._emit({
                "type": "agent.completed",
                "runId": run_id,
                **({"taskId": task_id} if task_id else {}),
                "result": result,
                "at": now(),
            })
            return result
        except Exception:
            message = "agent_run_failed"
            result = {"runId": run_id, "state": "failed", "text": "", "steps": 0, "toolCalls": 0, "error": message}
            self._emit({
                "type": "agent.failed",
                "runId": run_id,
                **({"taskId": task_id} if task_id else {}),
                "error": message,
                "at": now(),
            })
            return result
        finally:
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

    def _prune_admissions(self) -> None:
        while len(self._admissions) > 256:
            for message_id, admission in list(self._admissions.items()):
                if admission["completed"]:
                    del self._admissions[message_id]
                    break
            else:
                return

    def _emit(self, event: dict[str, Any]) -> None:
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
                "streaming": False,
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
            or set(outputs) != set(SYSTEM_SNAPSHOT_COMMAND_IDS)
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid snapshot command set")
        normalized_outputs: dict[str, tuple[int, str]] = {}
        total_bytes = 0
        for command_id in SYSTEM_SNAPSHOT_COMMAND_IDS:
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
        "providerId", "model", "requireCheckpoint", "planBinding",
    }
    parsed = _strict_object(value, fields)
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
    allowed = set(_allowed_tool_names(registry, mode))
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
    )
    binding = {field: request[field] for field in fields if field in request}
    budget = request.get("agentBudget")
    if isinstance(budget, AgentBudget):
        binding["agentBudget"] = asdict(budget)
    canonical = json.dumps(binding, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


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
