"""The node WebSocket channel.

A node connects *out* to the control plane, proves its identity with the same
Ed25519 signature it uses for HTTP, receives its queued tasks and reports their
outcome. The channel is where "outcome unknown" stops being a theory: if the
socket drops while a task is running, the control plane marks that task
``unknown`` instead of assuming failure, and never replays it.

Messages are JSON objects with a ``type``:

server -> node
    ``ready``     the channel is authenticated; ``assigned`` is the task count
    ``assigned``  one task to run (``task`` is a task view)
    ``synced``    response to ``sync``; ``assigned`` is the task count
    ``ack``       a reported status was applied
    ``bye``       a graceful ``close`` was recorded; ``marked`` lists the tasks
                  that became unknown
    ``error``     the message was rejected (``code``, ``message``)

``ready`` and ``synced`` also carry ``waiting``: queued tasks this node may not
run yet, each with a ``reason`` (currently ``approval_required``) and the action
``fingerprint`` that needs approving. A stalled queue is therefore reported, not
hidden.

node -> server
    ``status``    ``{taskId, status, outcome?, result?}``
    ``sync``      "assign me anything queued"
    ``close``     the node is shutting down; running work becomes unknown
    ``pong``      refreshes liveness (reserved for a future server ``ping``)

Liveness is refreshed on every node message; there is no server-side heartbeat
timer yet, and a node that goes quiet is noticed when its socket drops.
"""

from __future__ import annotations

import json
import logging

import anyio
from fastapi import WebSocket, WebSocketDisconnect
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from . import nodeauth, schemas, service
from .errors import ControlPlaneError, Unauthorized

logger = logging.getLogger("yukinal.control_plane.channel")

# Close code used when the handshake cannot be authenticated.
CLOSE_UNAUTHORIZED = 4401
# A node message larger than this is refused rather than buffered.
MAX_MESSAGE_BYTES = 64 * 1024

MESSAGE_READY = "ready"
MESSAGE_ASSIGNED = "assigned"
MESSAGE_SYNCED = "synced"
MESSAGE_ACK = "ack"
MESSAGE_BYE = "bye"
MESSAGE_ERROR = "error"

MESSAGE_SYNC = "sync"
MESSAGE_STATUS = "status"
MESSAGE_CLOSE = "close"
MESSAGE_PONG = "pong"


def _task_payload(task) -> dict:
    return schemas.TaskView.model_validate(task).model_dump(mode="json")


def _assigned_message(task) -> dict:
    return {"type": MESSAGE_ASSIGNED, "task": _task_payload(task)}


def _error(message: str, *, code: str = "invalid_message", task_id: str | None = None) -> dict:
    body = {"type": MESSAGE_ERROR, "code": code, "message": message}
    if task_id is not None:
        body["taskId"] = task_id
    return body


async def _authenticate(websocket: WebSocket, session_factory: async_sessionmaker[AsyncSession]) -> str:
    """Verify the WebSocket handshake and return the authenticated node id.

    The handshake signs ``GET <path[?query]>`` with an empty body; a replayed
    nonce or a bad signature raises ``Unauthorized`` and the caller closes with
    :data:`CLOSE_UNAUTHORIZED` without ever accepting the socket.
    """
    headers = websocket.headers
    url = websocket.url
    path = url.path + (f"?{url.query}" if url.query else "")
    async with session_factory() as session:
        async with session.begin():
            node = await service.authenticate_node(
                session,
                node_id=headers.get(nodeauth.HEADER_NODE),
                timestamp=headers.get(nodeauth.HEADER_TIMESTAMP),
                nonce=headers.get(nodeauth.HEADER_NONCE),
                signature=headers.get(nodeauth.HEADER_SIGNATURE),
                method="GET",
                path=path,
            )
            return node.id


async def run_channel(
    websocket: WebSocket, session_factory: async_sessionmaker[AsyncSession]
) -> None:
    """Serve one node connection from handshake to disconnect."""
    try:
        node_id = await _authenticate(websocket, session_factory)
    except Unauthorized as error:
        # Refuse before accepting: an unauthenticated peer never gets a socket.
        await websocket.close(code=CLOSE_UNAUTHORIZED, reason=str(error))
        return

    await websocket.accept()
    try:
        ready, assigned = await _open(session_factory, node_id)
        await websocket.send_json(ready)
        for message in assigned:
            await websocket.send_json(message)
        await _pump(websocket, session_factory, node_id)
    except WebSocketDisconnect:
        pass
    except Exception:  # never let a channel error look like a clean close
        logger.warning("node channel for %s ended with an error", node_id, exc_info=True)
    finally:
        # Shielded on purpose: a server shutdown cancels this task, and running
        # exactly when the connection did *not* end cleanly is the whole point.
        # Without the shield the outcome-unknown marking would be cancelled away.
        with anyio.CancelScope(shield=True):
            await _record_disconnect(session_factory, node_id)


async def _open(session_factory: async_sessionmaker[AsyncSession], node_id: str) -> tuple[dict, list[dict]]:
    """Touch the node and hand it everything already queued for it."""
    async with session_factory() as session:
        async with session.begin():
            node = await service.require_node_by_id(session, node_id)
            await service.touch_node(session, node)
            assigned, waiting = await service.assign_queued_tasks(session, node, actor=node.id)
            ready = {
                "type": MESSAGE_READY,
                "nodeId": node.id,
                "assigned": len(assigned),
                # Queued work this node may not run yet, with the reason, so a
                # stalled queue is visible instead of looking idle.
                "waiting": waiting,
            }
            messages = [_assigned_message(task) for task in assigned]
    return ready, messages


async def _pump(
    websocket: WebSocket, session_factory: async_sessionmaker[AsyncSession], node_id: str
) -> None:
    while True:
        raw = await websocket.receive_text()
        if len(raw.encode("utf-8")) > MAX_MESSAGE_BYTES:
            await websocket.send_json(_error("the message is too large", code="message_too_large"))
            continue
        try:
            message = json.loads(raw)
        except json.JSONDecodeError:
            await websocket.send_json(_error("the message is not valid JSON", code="invalid_json"))
            continue
        if not isinstance(message, dict):
            await websocket.send_json(_error("the message must be a JSON object"))
            continue

        kind = message.get("type")
        if kind == MESSAGE_PONG:
            await _touch(session_factory, node_id)
        elif kind == MESSAGE_SYNC:
            await _sync(websocket, session_factory, node_id)
        elif kind == MESSAGE_STATUS:
            await _status(websocket, session_factory, node_id, message)
        elif kind == MESSAGE_CLOSE:
            marked = await _record_disconnect(session_factory, node_id, reason="node_closed")
            await websocket.send_json({"type": MESSAGE_BYE, "marked": marked})
            await websocket.close(code=1000)
            return
        else:
            await websocket.send_json(
                _error(f"unknown message type `{kind}`", code="unknown_message")
            )


async def _touch(session_factory: async_sessionmaker[AsyncSession], node_id: str) -> None:
    async with session_factory() as session:
        async with session.begin():
            node = await service.require_node_by_id(session, node_id)
            await service.touch_node(session, node)


async def _sync(
    websocket: WebSocket, session_factory: async_sessionmaker[AsyncSession], node_id: str
) -> None:
    async with session_factory() as session:
        async with session.begin():
            node = await service.require_node_by_id(session, node_id)
            await service.touch_node(session, node)
            assigned, waiting = await service.assign_queued_tasks(session, node, actor=node.id)
            messages = [_assigned_message(task) for task in assigned]
    await websocket.send_json(
        {"type": MESSAGE_SYNCED, "assigned": len(messages), "waiting": waiting}
    )
    for message in messages:
        await websocket.send_json(message)


async def _status(
    websocket: WebSocket,
    session_factory: async_sessionmaker[AsyncSession],
    node_id: str,
    message: dict,
) -> None:
    task_id = message.get("taskId")
    status = message.get("status")
    if not isinstance(task_id, str) or not isinstance(status, str):
        await websocket.send_json(
            _error("a status message needs `taskId` and `status`", code="invalid_status")
        )
        return
    outcome = message.get("outcome")
    result = message.get("result")
    try:
        async with session_factory() as session:
            async with session.begin():
                node = await service.require_node_by_id(session, node_id)
                await service.touch_node(session, node)
                task = await service.transition_node_task(
                    session,
                    node=node,
                    task_id=task_id,
                    status=status,
                    outcome=outcome if isinstance(outcome, str) else None,
                    result=result if isinstance(result, dict) else None,
                )
                applied = task.status
    except ControlPlaneError as error:
        await websocket.send_json(_error(error.message, code=error.code, task_id=task_id))
        return
    await websocket.send_json({"type": MESSAGE_ACK, "taskId": task_id, "status": applied})


async def _mark_disconnected(
    session_factory: async_sessionmaker[AsyncSession], node_id: str, *, reason: str
) -> list[str]:
    """Mark the node's running work unknown. Idempotent: safe to call twice."""
    async with session_factory() as session:
        async with session.begin():
            node = await service.require_node_by_id(session, node_id)
            return await service.mark_node_disconnected(session, node, reason=reason)


async def _record_disconnect(
    session_factory: async_sessionmaker[AsyncSession],
    node_id: str,
    *,
    reason: str = "node_disconnected",
) -> list[str]:
    try:
        marked = await _mark_disconnected(session_factory, node_id, reason=reason)
        if marked:
            logger.info("node %s disconnected; %d task(s) are now unknown", node_id, len(marked))
        return marked
    except Exception:
        # The socket is already gone and the outcome is already unknown to the
        # node; log rather than raise into a closed connection.
        logger.warning("could not record the disconnect of node %s", node_id, exc_info=True)
        return []
