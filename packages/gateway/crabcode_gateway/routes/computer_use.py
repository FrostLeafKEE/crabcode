"""Dedicated WebSocket transport for Desktop Computer Use hosts."""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from crabcode_core.logging_utils import get_logger
from crabcode_gateway.session_registry import get_session_lock

logger = get_logger(__name__)
router = APIRouter(tags=["computer-use"])


async def _sync_bound_sessions(state: Any, host_id: str, enabled: bool) -> None:
    async with get_session_lock(state):
        for session in state.sessions.values():
            if getattr(session, "computer_use_host_id", None) == host_id:
                session.computer_use_enabled = enabled


@router.websocket("/computer-use/ws")
async def computer_use_socket(websocket: WebSocket) -> None:
    await websocket.accept()
    broker = websocket.app.state.computer_use_broker
    host_id: str | None = None
    try:
        while True:
            message: dict[str, Any] = await websocket.receive_json()
            kind = str(message.get("type") or "")
            if kind == "computer_use_host_register":
                requested_id = str(message.get("host_id") or "").strip()
                if not requested_id or len(requested_id) > 200:
                    await websocket.send_json({"type": "computer_use_error", "error": "invalid host_id"})
                    continue
                if host_id and host_id != requested_id:
                    broker.unregister(host_id, websocket)
                host_id = requested_id
                capabilities = message.get("capabilities")
                broker.register(
                    host_id,
                    websocket,
                    enabled=message.get("enabled") is True,
                    gui_available=message.get("gui_available") is True,
                    capabilities=capabilities if isinstance(capabilities, dict) else {},
                )
                available = broker.is_available(host_id)
                await _sync_bound_sessions(websocket.app.state, host_id, available)
                await websocket.send_json({
                    "type": "computer_use_host_registered",
                    "host_id": host_id,
                    "available": available,
                    "previews": broker.restorable_previews(host_id),
                })
                continue

            if host_id is None:
                await websocket.send_json({"type": "computer_use_error", "error": "host is not registered"})
                continue

            if kind == "computer_use_host_state":
                capabilities = message.get("capabilities")
                updated = broker.update_state(
                    host_id,
                    websocket,
                    enabled=message.get("enabled") is True,
                    gui_available=message.get("gui_available") is True,
                    capabilities=capabilities if isinstance(capabilities, dict) else None,
                )
                available = broker.is_available(host_id) if updated else False
                if updated:
                    await _sync_bound_sessions(websocket.app.state, host_id, available)
                await websocket.send_json({
                    "type": "computer_use_host_state_ack",
                    "available": available,
                })
            elif kind == "computer_use_result":
                request_id = str(message.get("request_id") or "")
                result = message.get("result")
                broker.resolve(
                    host_id,
                    request_id,
                    result if isinstance(result, dict) else {"ok": False, "error": "invalid host result"},
                )
            else:
                await websocket.send_json({"type": "computer_use_error", "error": f"unknown message type: {kind}"})
    except WebSocketDisconnect:
        pass
    except Exception:
        logger.warning("Computer Use host socket failed", exc_info=True)
    finally:
        if host_id:
            broker.unregister(host_id, websocket)
            if not broker.is_available(host_id):
                await _sync_bound_sessions(websocket.app.state, host_id, False)
