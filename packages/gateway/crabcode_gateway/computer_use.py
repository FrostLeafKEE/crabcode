"""Broker Computer Use requests between Core sessions and Desktop hosts."""

from __future__ import annotations

import asyncio
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

from fastapi import WebSocket


COMPUTER_USE_RELEASE_RETENTION_MS = 15_000


LeaseKey = tuple[str, str, str | None]


@dataclass
class PendingComputerUseRequest:
    host_id: str
    future: asyncio.Future[dict[str, Any]]
    lease: LeaseKey


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
        self._pending: dict[str, PendingComputerUseRequest] = {}
        self._active_leases: set[LeaseKey] = set()
        # Keep the latest preview for each lease in the Gateway so a Desktop
        # webview reload can reconstruct its UI.  Released previews remain
        # here only through the same 15-second retention window as Desktop.
        self._lease_previews: dict[LeaseKey, dict[str, Any]] = {}

    @staticmethod
    def _now_ms() -> int:
        return int(time.time() * 1000)

    def _purge_expired_previews(self, now_ms: int | None = None) -> None:
        now = self._now_ms() if now_ms is None else now_ms
        for lease, preview in list(self._lease_previews.items()):
            deadline = preview.get("release_deadline_ms")
            if (
                lease not in self._active_leases
                and isinstance(deadline, (int, float))
                and deadline <= now
            ):
                self._lease_previews.pop(lease, None)

    def restorable_previews(self, host_id: str) -> list[dict[str, Any]]:
        """Return active or retained previews for a reconnecting Desktop host."""
        self._purge_expired_previews()
        return [
            dict(preview)
            for lease, preview in self._lease_previews.items()
            if lease[0] == host_id
        ]

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
            for request_id, pending in list(self._pending.items()):
                if pending.host_id != host_id:
                    continue
                self._pending.pop(request_id, None)
                if not pending.future.done():
                    pending.future.set_exception(RuntimeError("Computer Use host reconnected"))
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
        for request_id, pending in list(self._pending.items()):
            if pending.host_id != host_id:
                continue
            self._pending.pop(request_id, None)
            if not pending.future.done():
                pending.future.set_exception(RuntimeError("Computer Use host disconnected"))

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
        if pending is None or pending.host_id != host_id:
            return False
        self._pending.pop(request_id, None)
        previous = self._lease_previews.get(pending.lease, {})
        self._lease_previews[pending.lease] = {
            **previous,
            "session_id": pending.lease[1],
            "agent_id": pending.lease[2],
            "mode": previous.get("mode", "background_app"),
            "target_scope": previous.get("target_scope", "app_window"),
            "delivery_policy": previous.get("delivery_policy", "allow_foreground"),
            "status": "error" if result.get("ok") is False else "ready",
            "action": str(result.get("action") or previous.get("action") or "unknown"),
            "summary": str(
                result.get("summary")
                or result.get("error")
                or previous.get("summary")
                or "Computer Use action completed"
            ),
            "frame": result.get("screenshot") or previous.get("frame"),
            "cursor": result.get("cursor") or previous.get("cursor"),
            "updated_at_ms": self._now_ms(),
            "release_deadline_ms": previous.get("release_deadline_ms"),
        }
        if not pending.future.done():
            pending.future.set_result(result)
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

        deadline_ms = self._now_ms() + COMPUTER_USE_RELEASE_RETENTION_MS
        self._active_leases.difference_update(leases)
        for lease in leases:
            preview = self._lease_previews.get(lease)
            if preview is not None:
                preview["release_deadline_ms"] = deadline_ms

        message: dict[str, Any] = {
            "type": "computer_use_release",
            "session_id": session_id,
            "release_deadline_ms": deadline_ms,
        }
        if all_agents:
            message["all_agents"] = True
        elif agent_id is not None:
            message["agent_id"] = agent_id

        # The lifecycle state is released even if Desktop is briefly absent
        # during a reload.  A reconnect receives the retained preview and the
        # original absolute deadline from restorable_previews().
        host = self._hosts.get(host_id)
        if host is not None:
            async with host.send_lock:
                if self._hosts.get(host_id) is host:
                    await host.websocket.send_json(message)
        return True

    async def execute(
        self,
        host_id: str,
        *,
        session_id: str,
        agent_id: str | None,
        action: dict[str, Any],
        mode: str | None = None,
        target_scope: str | None = None,
        delivery_policy: str = "allow_foreground",
    ) -> dict[str, Any]:
        host = self._hosts.get(host_id)
        if host is None or not host.enabled or not host.gui_available:
            raise RuntimeError("Computer Use is unavailable or disabled")
        if target_scope is None:
            target_scope = "desktop" if mode == "foreground_desktop" else "app_window"
        if target_scope not in ("app_window", "desktop") or delivery_policy not in (
            "strict_background", "allow_foreground"
        ):
            raise ValueError("Invalid Computer Use target scope or delivery policy")
        expected_mode = "foreground_desktop" if target_scope == "desktop" else "background_app"
        if mode is None:
            mode = expected_mode
        if mode != expected_mode:
            raise ValueError("Conflicting Computer Use mode and target scope")
        read_only = action.get("action") in ("observe", "list_windows", "list_displays", "wait")

        async with host.action_lock:
            request_id = str(uuid.uuid4())
            try:
                async with host.send_lock:
                    # Recheck immediately before sending: either lock can wait
                    # while the host disconnects or changes its capabilities.
                    if self._hosts.get(host_id) is not host or not host.enabled or not host.gui_available:
                        raise RuntimeError("Computer Use became unavailable")
                    supported_modes = host.capabilities.get("supported_modes")
                    if not isinstance(supported_modes, list):
                        supported_modes = ["foreground_desktop"]
                    if mode not in supported_modes:
                        raise RuntimeError(f"Computer Use mode '{mode}' is unavailable on this host")
                    error = None
                    if delivery_policy == "strict_background" and target_scope == "desktop":
                        error = "Desktop scope requires allow_foreground"
                    elif not read_only and host.capabilities.get("delivery_policy_version") != 1:
                        # Old hosts can silently ignore unknown policy fields.
                        error = "This host must be upgraded before it can enforce delivery policy"
                    if error:
                        return {
                            "ok": False, "action": action.get("action"), "error": error, "summary": error,
                            "error_code": "background_delivery_unsupported", "action_dispatched": False,
                            "dispatch_succeeded": False, "effect_verified": False, "retry_safe": True,
                            "target_scope": target_scope, "delivery_policy": delivery_policy,
                            "focus_isolation": "unavailable",
                        }
                    future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
                    lease = (host_id, session_id, agent_id)
                    self._pending[request_id] = PendingComputerUseRequest(
                        host_id=host_id,
                        future=future,
                        lease=lease,
                    )
                    self._purge_expired_previews()
                    self._active_leases.add(lease)
                    previous = self._lease_previews.get(lease, {})
                    self._lease_previews[lease] = {
                        **previous,
                        "session_id": session_id,
                        "agent_id": agent_id,
                        "mode": mode,
                        "target_scope": target_scope,
                        "delivery_policy": delivery_policy,
                        "status": "busy",
                        "action": str(action.get("action") or "unknown"),
                        "summary": "正在执行…",
                        "updated_at_ms": self._now_ms(),
                        "release_deadline_ms": None,
                    }
                    await host.websocket.send_json({
                        "type": "computer_use_request",
                        "request_id": request_id,
                        "session_id": session_id,
                        "agent_id": agent_id,
                        "mode": mode,
                        "target_scope": target_scope,
                        "delivery_policy": delivery_policy,
                        "action": action,
                    })
                return await asyncio.wait_for(future, timeout=self.timeout_seconds)
            except asyncio.TimeoutError as exc:
                raise RuntimeError("Computer Use host timed out; input may have arrived. Observe before retrying.") from exc
            finally:
                self._pending.pop(request_id, None)
