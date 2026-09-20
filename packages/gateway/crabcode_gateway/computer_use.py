"""Broker Computer Use requests between Core sessions and Desktop hosts."""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass, field
from typing import Any

from fastapi import WebSocket


@dataclass
class ComputerUseHost:
    host_id: str
    websocket: WebSocket
    enabled: bool
    gui_available: bool
    capabilities: dict[str, Any]
    action_lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    send_lock: asyncio.Lock = field(default_factory=asyncio.Lock)


class ComputerUseBroker:
    """Tracks connected GUI hosts and dispatches one action at a time per host."""

    def __init__(self, timeout_seconds: float = 90.0) -> None:
        self.timeout_seconds = timeout_seconds
        self._hosts: dict[str, ComputerUseHost] = {}
        self._pending: dict[str, tuple[str, asyncio.Future[dict[str, Any]]]] = {}
        self._active_leases: set[tuple[str, str, str | None]] = set()

    def register(
        self,
        host_id: str,
        websocket: WebSocket,
        *,
        enabled: bool,
        gui_available: bool,
        capabilities: dict[str, Any] | None = None,
    ) -> ComputerUseHost:
        previous = self._hosts.get(host_id)
        if previous is not None and previous.websocket is not websocket:
            for request_id, (pending_host_id, future) in list(self._pending.items()):
                if pending_host_id != host_id:
                    continue
                self._pending.pop(request_id, None)
                if not future.done():
                    future.set_exception(RuntimeError("Computer Use host reconnected"))
        host = ComputerUseHost(
            host_id=host_id,
            websocket=websocket,
            enabled=enabled,
            gui_available=gui_available,
            capabilities=dict(capabilities or {}),
        )
        self._hosts[host_id] = host
        return host

    def update_state(
        self,
        host_id: str,
        websocket: WebSocket,
        *,
        enabled: bool,
        gui_available: bool,
        capabilities: dict[str, Any] | None = None,
    ) -> bool:
        host = self._hosts.get(host_id)
        if host is None or host.websocket is not websocket:
            return False
        host.enabled = enabled
        host.gui_available = gui_available
        if capabilities is not None:
            host.capabilities = dict(capabilities)
        return True

    def unregister(self, host_id: str, websocket: WebSocket) -> None:
        host = self._hosts.get(host_id)
        if host is None or host.websocket is not websocket:
            return
        self._hosts.pop(host_id, None)
        for request_id, (pending_host_id, future) in list(self._pending.items()):
            if pending_host_id != host_id:
                continue
            self._pending.pop(request_id, None)
            if not future.done():
                future.set_exception(RuntimeError("Computer Use host disconnected"))

    def is_available(self, host_id: str | None, mode: str | None = None) -> bool:
        if not host_id:
            return False
        host = self._hosts.get(host_id)
        if not (host and host.enabled and host.gui_available):
            return False
        if mode is None:
            return True
        supported_modes = host.capabilities.get("supported_modes")
        if not isinstance(supported_modes, list):
            # Hosts predating mode negotiation only implement the legacy
            # foreground desktop behavior.
            return mode == "foreground_desktop"
        return mode in supported_modes

    def resolve(self, host_id: str, request_id: str, result: dict[str, Any]) -> bool:
        pending = self._pending.get(request_id)
        if pending is None or pending[0] != host_id:
            return False
        self._pending.pop(request_id, None)
        future = pending[1]
        if not future.done():
            future.set_result(result)
        return True

    async def release(
        self,
        host_id: str,
        *,
        session_id: str,
        agent_id: str | None = None,
        all_agents: bool = False,
    ) -> bool:
        """Tell a host that an agent/session no longer owns its preview.

        Action completion is deliberately separate from release: a model may
        spend an arbitrary amount of time thinking between Computer Use calls.
        The Desktop starts its retention timer only after this message.
        """
        host = self._hosts.get(host_id)
        if host is None:
            return False
        async with host.send_lock:
            if self._hosts.get(host_id) is not host:
                return False
            if all_agents:
                leases = {
                    lease for lease in self._active_leases
                    if lease[0] == host_id and lease[1] == session_id
                }
            else:
                lease = (host_id, session_id, agent_id)
                leases = {lease} if lease in self._active_leases else set()
            if not leases:
                return False

            message: dict[str, Any] = {
                "type": "computer_use_release",
                "session_id": session_id,
            }
            if all_agents:
                message["all_agents"] = True
            elif agent_id is not None:
                message["agent_id"] = agent_id
            await host.websocket.send_json(message)
            self._active_leases.difference_update(leases)
            return True

    async def execute(
        self,
        host_id: str,
        *,
        session_id: str,
        agent_id: str | None,
        action: dict[str, Any],
        mode: str = "background_app",
    ) -> dict[str, Any]:
        host = self._hosts.get(host_id)
        if host is None or not host.enabled or not host.gui_available:
            raise RuntimeError("Computer Use is unavailable or disabled")
        supported_modes = host.capabilities.get("supported_modes")
        if not isinstance(supported_modes, list):
            supported_modes = ["foreground_desktop"]
        if mode not in supported_modes:
            raise RuntimeError(f"Computer Use mode '{mode}' is unavailable on this host")

        async with host.action_lock:
            # State may have changed while this request waited for the host.
            if self._hosts.get(host_id) is not host or not host.enabled or not host.gui_available:
                raise RuntimeError("Computer Use became unavailable")
            request_id = str(uuid.uuid4())
            future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
            self._pending[request_id] = (host_id, future)
            try:
                self._active_leases.add((host_id, session_id, agent_id))
                async with host.send_lock:
                    await host.websocket.send_json({
                        "type": "computer_use_request",
                        "request_id": request_id,
                        "session_id": session_id,
                        "agent_id": agent_id,
                        "mode": mode,
                        "action": action,
                    })
                return await asyncio.wait_for(future, timeout=self.timeout_seconds)
            except asyncio.TimeoutError as exc:
                raise RuntimeError("Computer Use host timed out") from exc
            finally:
                self._pending.pop(request_id, None)
