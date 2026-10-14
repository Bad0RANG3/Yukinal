from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from typing import Any


class HostRpcError(RuntimeError):
    """A request made by the Agent sidecar to the desktop host failed."""


FrameSender = Callable[[dict[str, Any]], Awaitable[None]]


class HostRpcClient:
    """Multiplex requests from the Python sidecar over the host's JSON-RPC pipe."""

    def __init__(self, send: FrameSender) -> None:
        self._send = send
        self._next_id = 1
        self._pending: dict[int, asyncio.Future[Any]] = {}
        self._closed = False

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float = 30.0) -> Any:
        if self._closed:
            raise HostRpcError("host connection is closed")
        if not isinstance(method, str) or not method or not isinstance(params, dict):
            raise ValueError("invalid host request")
        request_id = self._allocate_id()
        future = asyncio.get_running_loop().create_future()
        self._pending[request_id] = future
        try:
            await self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
            return await asyncio.wait_for(future, timeout=timeout_seconds)
        except asyncio.CancelledError:
            self._pending.pop(request_id, None)
            self._cancel_host_request(request_id)
            raise
        except TimeoutError:
            self._pending.pop(request_id, None)
            self._cancel_host_request(request_id)
            raise HostRpcError("host request timed out") from None
        except Exception:
            self._pending.pop(request_id, None)
            raise
        finally:
            self._pending.pop(request_id, None)

    def handle_response(self, frame: object) -> bool:
        """Consume a host response frame, leaving sidecar requests for the RPC router."""
        if not isinstance(frame, dict) or frame.get("jsonrpc") != "2.0":
            return False
        request_id = frame.get("id")
        if isinstance(request_id, bool) or not isinstance(request_id, int):
            return False
        if "result" not in frame and "error" not in frame:
            return False
        future = self._pending.get(request_id)
        if future is None or future.done():
            return True
        if "error" in frame:
            error = frame["error"]
            message = error.get("message") if isinstance(error, dict) else None
            future.set_exception(HostRpcError(message if isinstance(message, str) else "host request failed"))
        else:
            future.set_result(frame["result"])
        return True

    def close(self) -> None:
        self._closed = True
        pending = list(self._pending.values())
        self._pending.clear()
        for future in pending:
            if not future.done():
                future.set_exception(HostRpcError("host connection closed"))

    def _allocate_id(self) -> int:
        request_id = self._next_id
        self._next_id += 1
        if self._next_id > 2**53 - 1:
            self._next_id = 1
        while request_id in self._pending:
            request_id = self._next_id
            self._next_id += 1
        return request_id

    def _cancel_host_request(self, request_id: int) -> None:
        if self._closed:
            return
        cancel_id = self._allocate_id()

        async def send_cancel() -> None:
            try:
                await self._send({
                    "jsonrpc": "2.0",
                    "id": cancel_id,
                    "method": "host.tool.cancel",
                    "params": {"requestId": request_id},
                })
            except Exception:
                return

        asyncio.create_task(send_cancel())
