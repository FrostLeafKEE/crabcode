"""CLI adapter for the shared native executor (no Desktop, Gateway or Node)."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
from pathlib import Path
import sys
import time
from typing import Any
from uuid import uuid4

from crabcode_core.logging_utils import get_logger

logger = get_logger(__name__)
_MAX_RESPONSE = 64 * 1024 * 1024
_HEARTBEAT_SECONDS = 30


class ComputerUseError(RuntimeError):
    pass


def state_directory() -> Path:
    if sys.platform == "darwin":
        base = Path.home() / "Library/Application Support"
    elif sys.platform == "win32":
        base = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData/Local")
    else:
        base = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    return base / "CrabCode/ComputerUse"


def bundled_executable(directory: Path) -> Path:
    if sys.platform == "darwin":
        app = directory if directory.suffix == ".app" else directory / "Crab Computer Use.app"
        return app / "Contents/MacOS/crabcode-computer-use-host"
    return directory / ("crabcode-computer-use-host.exe" if sys.platform == "win32" else "crabcode-computer-use-host")


class NativeClient:
    def __init__(self, state_dir: Path | None = None) -> None:
        self.state_dir = state_dir or state_directory()

    @property
    def installed(self) -> bool:
        return bundled_executable(self.state_dir).is_file()

    async def start(self, bundle: Path | None = None) -> None:
        executable = bundled_executable(bundle or self.state_dir)
        if not executable.is_file():
            raise ComputerUseError(
                "Computer Use 原生组件未安装；请运行 crabcode computer-use install --bundle <组件目录>。"
            )
        process = None
        try:
            process = await asyncio.create_subprocess_exec(
                str(executable), "--install-and-start", "--state-dir", str(self.state_dir),
                stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE,
            )
            _, stderr = await asyncio.wait_for(process.communicate(), 25)
            if process.returncode:
                raise ComputerUseError(stderr.decode("utf-8", errors="replace").strip() or "Computer Use 组件启动失败")
        except (OSError, asyncio.TimeoutError) as exc:
            raise ComputerUseError("Computer Use 组件启动失败或超时；请检查 crabcode computer-use status。") from exc
        finally:
            if process is not None and process.returncode is None:
                process.kill()
                await process.wait()

    def endpoint(self) -> dict[str, Any]:
        try:
            endpoint = json.loads((self.state_dir / "endpoint.json").read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise ComputerUseError("Computer Use 执行器未连接；请运行 crabcode computer-use status。") from exc
        if not (
            isinstance(endpoint, dict) and endpoint.get("protocol_version") == 1
            and type(endpoint.get("port")) is int and 0 < endpoint["port"] <= 65535
            and isinstance(endpoint.get("token"), str) and len(endpoint["token"]) == 64
            and isinstance(endpoint.get("instance_id"), str) and endpoint["instance_id"]
        ):
            raise ComputerUseError("Computer Use 组件版本不兼容，请更新组件。")
        return endpoint

    async def request(
        self, method: str, params: dict[str, Any] | None = None,
        *, instance_id: str | None = None, timeout: float = 95,
    ) -> Any:
        endpoint = self.endpoint()
        if instance_id is not None and endpoint["instance_id"] != instance_id:
            raise ComputerUseError("Computer Use 执行器已重启，请重新检测并观察窗口。")
        message = json.dumps({
            "token": endpoint["token"], "instance_id": endpoint["instance_id"],
            "method": method, "params": params or {},
            "deadline_ms": int((time.time() + min(85, timeout)) * 1000),
        }, ensure_ascii=False).encode("utf-8") + b"\n"
        if len(message) > 1024 * 1024:
            raise ComputerUseError("Computer Use 请求超过大小限制")

        async def exchange() -> Any:
            reader, writer = await asyncio.open_connection("127.0.0.1", endpoint["port"], limit=_MAX_RESPONSE)
            try:
                writer.write(message)
                await writer.drain()
                # Requests are never replayed: a missing reply cannot establish
                # whether native input was already delivered.
                try:
                    line = await reader.readuntil(b"\n")
                except asyncio.IncompleteReadError as exc:
                    raise ComputerUseError("Computer Use 响应不完整；请先观察结果，不要直接重复输入。") from exc
                except asyncio.LimitOverrunError as exc:
                    raise ComputerUseError("Computer Use 响应超过大小限制") from exc
                try:
                    response = json.loads(line)
                except (ValueError, UnicodeError) as exc:
                    raise ComputerUseError("Computer Use 响应无效；请先观察结果。") from exc
                if not isinstance(response, dict) or response.get("ok") is not True:
                    error = response.get("error") if isinstance(response, dict) else None
                    raise ComputerUseError(str(error or "Computer Use 执行失败"))
                return response.get("result")
            finally:
                writer.close()
                with contextlib.suppress(OSError):
                    await writer.wait_closed()

        try:
            return await asyncio.wait_for(exchange(), timeout)
        except asyncio.TimeoutError as exc:
            raise ComputerUseError("Computer Use 执行器响应超时；请先观察结果，不要直接重复输入。") from exc
        except OSError as exc:
            raise ComputerUseError("Computer Use 执行器连接中断；请重新检测并观察结果。") from exc


class LocalComputerUse:
    """The CoreSession backend contract, using the same IPC as the GUI clients."""

    def __init__(self, client: NativeClient | None = None) -> None:
        self.client = client or NativeClient()
        self.host_id = f"cli-{uuid4()}"
        self.connection_id = str(uuid4())
        self._caps: dict[str, Any] = {}
        self._heartbeat: asyncio.Task[None] | None = None
        self._closed = False

    async def connect(self) -> None:
        await self.refresh()
        self._heartbeat = asyncio.create_task(self._keep_alive())

    async def refresh(self) -> None:
        await self.client.start()
        caps = await self.client.request("capabilities", timeout=25)
        if not isinstance(caps, dict) or not caps.get("instance_id") or caps.get("host_protocol_version") != 1:
            raise ComputerUseError("Computer Use 组件版本不兼容，请更新组件。")
        if self._caps.get("instance_id") != caps["instance_id"]:
            self.connection_id = str(uuid4())
        self._caps = caps

    def capabilities(self, host_id: str | None) -> dict[str, Any]:
        return dict(self._caps) if not self._closed and host_id == self.host_id else {}

    def is_available(self, host_id: str | None, mode: str | None = None) -> bool:
        caps = self.capabilities(host_id)
        return bool(caps.get("gui_available") and (mode is None or mode in caps.get("supported_modes", [])))

    async def execute(
        self, host_id: str, *, session_id: str, agent_id: str | None,
        action: dict[str, Any], mode: str | None = None, target_scope: str | None = None,
        delivery_policy: str = "strict_background",
    ) -> dict[str, Any]:
        scope = target_scope or ("desktop" if mode == "foreground_desktop" else "app_window")
        expected_mode = "foreground_desktop" if scope == "desktop" else "background_app"
        if scope not in ("desktop", "app_window") or (mode is not None and mode != expected_mode):
            raise ComputerUseError("Invalid Computer Use mode or target scope")
        if delivery_policy not in ("strict_background", "allow_foreground"):
            raise ComputerUseError("Invalid Computer Use delivery policy")
        if scope == "desktop" and delivery_policy == "strict_background":
            raise ComputerUseError("Desktop scope requires allow_foreground")
        if not session_id or not self.is_available(host_id, expected_mode):
            raise ComputerUseError("Computer Use is unavailable for this session or mode")
        if action.get("action") not in ("observe", "list_windows", "list_displays", "wait") and self._caps.get("delivery_policy_version") != 1:
            raise ComputerUseError("Computer Use host must be upgraded to enforce the delivery policy")
        owner = {"host_id": self.host_id, "connection_id": self.connection_id,
                 "session_id": session_id, "agent_id": agent_id}
        request = {"mode": expected_mode, "target_scope": scope,
                   "delivery_policy": delivery_policy, "action": action}
        if self._caps.get("ax_protocol_version") == 1:
            request["owner"] = owner
        instance = self._caps["instance_id"]
        result = await self.client.request("execute", {
            "instanceId": instance, "lease": owner, "request": request,
        }, instance_id=instance)
        if not isinstance(result, dict):
            raise ComputerUseError("Computer Use 执行器返回了无效结果")
        # CLI has no monitor; monitor-only frames must not enter model/history.
        result.pop("preview_screenshot", None)
        result.pop("preview_screenshot_error", None)
        return result

    async def release(
        self, host_id: str, *, session_id: str | None = None,
        agent_id: str | None = None, all_agents: bool = False,
    ) -> None:
        if host_id != self.host_id or not self._caps:
            return
        await self.client.request("release", {
            "hostId": self.host_id, "connectionId": self.connection_id,
            "sessionId": session_id, "agentId": agent_id, "allAgents": all_agents,
        }, instance_id=self._caps["instance_id"], timeout=5)

    async def _keep_alive(self) -> None:
        while True:
            await asyncio.sleep(_HEARTBEAT_SECONDS)
            try:
                await self.client.request("heartbeat", {
                    "hostId": self.host_id, "connectionId": self.connection_id,
                }, instance_id=self._caps.get("instance_id"), timeout=5)
            except ComputerUseError:
                self._caps["gui_available"] = False
            try:
                # Also recover a stopped executor and pick up permission changes.
                # Only capabilities are refreshed; input is never retried.
                await self.refresh()
            except ComputerUseError:
                self._caps["gui_available"] = False

    async def close(self) -> None:
        self._closed = True
        if self._heartbeat is not None:
            self._heartbeat.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._heartbeat
            self._heartbeat = None
        with contextlib.suppress(ComputerUseError):
            await self.release(self.host_id, all_agents=True)
        self._caps = {}


async def connect_session(session: Any) -> LocalComputerUse | None:
    """Discover an installed component automatically; ordinary CLI still works without it."""
    backend = LocalComputerUse()
    if not backend.client.installed:
        return None
    try:
        await backend.connect()
    except ComputerUseError as exc:
        logger.warning("Computer Use unavailable: %s", exc)
        await backend.close()
        return None
    session.computer_use_backend = backend
    session.computer_use_host_id = backend.host_id
    session.computer_use_enabled = True
    return backend
