"""ComputerUseTool — control a graphical desktop through a connected host."""

from __future__ import annotations

import base64
import binascii
import json
from typing import Any

from crabcode_core.types.tool import (
    MAX_INLINE_IMAGE_BYTES,
    PermissionBehavior,
    PermissionResult,
    Tool,
    ToolContext,
    ToolResult,
)


_ACTIONS = {
    "observe",
    "list_displays",
    "list_windows",
    "move",
    "click",
    "double_click",
    "drag",
    "scroll",
    "type",
    "keypress",
    "focus_window",
    "open_app",
    "wait",
}
_READ_ONLY_ACTIONS = {"observe", "list_displays", "list_windows", "wait"}
_COMPUTER_USE_MODES = {"background_app", "foreground_desktop"}
_MAX_SCROLL_DELTA = 10_000


class ComputerUseTool(Tool):
    """Bridge agent desktop actions to a GUI host connected to the Gateway."""

    name = "ComputerUse"
    description = (
        "Observe and control the user's graphical desktop across applications. "
        "This tool is only exposed while an enabled GUI host is connected."
    )
    is_read_only = False
    is_concurrency_safe = False
    uses_tool_permission_policy = True
    input_schema = {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": sorted(_ACTIONS),
                "description": "Desktop action to perform.",
            },
            "x": {
                "type": "integer",
                "description": "X coordinate: window-local screenshot coordinate in background_app; absolute desktop coordinate in foreground_desktop.",
            },
            "y": {
                "type": "integer",
                "description": "Y coordinate: window-local screenshot coordinate in background_app; absolute desktop coordinate in foreground_desktop.",
            },
            "to_x": {
                "type": "integer",
                "description": "Drag destination X in the active mode's coordinate space.",
            },
            "to_y": {
                "type": "integer",
                "description": "Drag destination Y in the active mode's coordinate space.",
            },
            "button": {
                "type": "string",
                "enum": ["left", "middle", "right"],
                "description": "Mouse button. Defaults to left.",
            },
            "delta_x": {
                "type": "integer", "minimum": -_MAX_SCROLL_DELTA, "maximum": _MAX_SCROLL_DELTA,
                "description": "Horizontal scroll: positive right, negative left. Pixels in both macOS modes; wheel steps on other platforms.",
            },
            "delta_y": {
                "type": "integer", "minimum": -_MAX_SCROLL_DELTA, "maximum": _MAX_SCROLL_DELTA,
                "description": "Vertical scroll: positive down, negative up. Pixels in both macOS modes; wheel steps on other platforms.",
            },
            "text": {
                "type": "string",
                "description": "Text to type, or an application/process name to open or focus.",
            },
            "keys": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Keys to press together, for example ['CTRL', 'L'].",
            },
            "display_id": {"type": "string", "description": "Display to observe."},
            "window_id": {
                "type": "string",
                "description": "Target window. Required for observation and input in background_app mode.",
            },
            "duration_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": 30000,
                "description": "Wait or drag duration in milliseconds.",
            },
            "include_screenshot": {
                "type": "boolean",
                "description": "Capture the desktop after the action. Defaults to true for control actions.",
            },
        },
        "required": ["action"],
        "additionalProperties": False,
    }

    def __init__(self) -> None:
        self._session: Any | None = None

    async def setup(self, context: ToolContext) -> None:
        await super().setup(context)
        self._session = context.session
        self.is_enabled = bool(context.tool_config.get("enabled", True))

    def _binding(self) -> tuple[Any | None, str | None, bool, str]:
        session = self._session
        mode = str(getattr(session, "computer_use_mode", "background_app"))
        if mode not in _COMPUTER_USE_MODES:
            mode = "background_app"
        return (
            getattr(session, "computer_use_backend", None),
            getattr(session, "computer_use_host_id", None),
            getattr(session, "computer_use_enabled", False) is True,
            mode,
        )

    def is_available(self, context: ToolContext) -> bool:
        if not self.is_enabled:
            return False
        backend, host_id, enabled, _mode = self._binding()
        return bool(enabled and backend and host_id and backend.is_available(host_id, _mode))

    async def get_prompt(self, **kwargs: Any) -> str:
        _backend, _host_id, _enabled, mode = self._binding()
        scroll_guidance = (
            " For scroll, provide x/y over the intended scroll area (required in background_app). "
            "On macOS both modes use pixels: positive delta_y scrolls down, negative scrolls up; "
            "positive delta_x scrolls right. Other platforms use wheel steps. "
            "A successful call confirms input dispatch, not that the application scrolled. Compare "
            "the target area's content before/after; observe again if uncertain. An unchanged image "
            "does not establish that all messages/history are visible or a boundary was reached."
        )
        if mode == "background_app":
            return (
                "Use ComputerUse in background_app mode to inspect and operate one macOS application window. "
                "The target application is allowed to become foreground; this mode does not guarantee focus isolation. "
                "Start with list_windows, then pass window_id to observe, focus_window, "
                "and every pointer or keyboard action. Pointer coordinates are window-local screenshot coordinates: "
                "the image top-left is (0,0), and you must not add origin_x/origin_y. Full-desktop capture, display "
                "selection, and automatic switching to foreground_desktop are unavailable in this mode. "
                "Use focus_window when the target needs activation, then observe again before further input. "
                "Single left clicks prefer accessibility actions and fall back to window-targeted mouse events when "
                "unsupported. Double-click, right-click, and middle-click also use window-targeted mouse events. "
                "ok and dispatch_succeeded report whether click dispatch succeeded, not whether the intended UI "
                "effect occurred. effect_verified and visual_change_detected are screenshot-based evidence only. "
                "When effect_verified is false, verification_warning is informational, not a dispatch failure: "
                "inspect the returned screenshot or observe again and judge whether the intended action took effect. "
                "Do not repeat a click solely because its effect was not verified. foreground_activated is diagnostic only: "
                "focus changes are allowed and are not a reason to stop using ComputerUse. "
                "background_click_unsupported means no supported background action was dispatched, and "
                "background_click_dispatch_unverified means dispatch itself was not acknowledged; the action may "
                "still have arrived, so observe before deciding whether another action is needed. A hidden "
                "auxiliary window is not clicked from stale pixels; use focus_window and observe again. Prefer one "
                "deliberate action per call and observe again after navigation or any uncertain action. The host "
                "composites AX-confirmed same-process auxiliary windows over the selected root window; stale hidden "
                "backing stores are ignored. If an observation reports "
                "background_observation_limited, the app ordered an auxiliary window off-screen while backgrounded; "
                "do not infer or click content that is absent from its retained pixels."
            ) + scroll_guidance
        return (
            "Use ComputerUse in foreground_desktop mode to inspect and operate the graphical desktop when a task "
            "requires native UI interaction. The coordinate space is the full desktop and may include multiple "
            "displays. Start with observe or list_displays/list_windows, then use the returned image dimensions and "
            "origin for coordinates. This mode controls the visible pointer and keyboard and may interrupt the user. "
            "Prefer one deliberate action per call and observe again after uncertain navigation."
        ) + scroll_guidance

    async def validate_input(self, tool_input: dict[str, Any]) -> str | None:
        action = str(tool_input.get("action", "")).strip()
        if action not in _ACTIONS:
            return f"action must be one of: {', '.join(sorted(_ACTIONS))}"
        required: dict[str, tuple[str, ...]] = {
            "move": ("x", "y"),
            "click": ("x", "y"),
            "double_click": ("x", "y"),
            "drag": ("x", "y", "to_x", "to_y"),
            "type": ("text",),
            "open_app": ("text",),
            "keypress": ("keys",),
        }
        missing = [name for name in required.get(action, ()) if tool_input.get(name) is None]
        if missing:
            return f"{', '.join(missing)} required for {action}"
        if action == "focus_window" and not tool_input.get("window_id") and not tool_input.get("text"):
            return "window_id or text required for focus_window"
        if action == "keypress" and not isinstance(tool_input.get("keys"), list):
            return "keys must be an array"
        if action == "keypress" and not tool_input.get("keys"):
            return "keys must not be empty"
        if action == "scroll":
            deltas = [tool_input.get(key, 0) for key in ("delta_x", "delta_y")]
            if any(
                not isinstance(delta, int) or isinstance(delta, bool)
                or not -_MAX_SCROLL_DELTA <= delta <= _MAX_SCROLL_DELTA
                for delta in deltas
            ):
                return f"scroll deltas must be integers between -{_MAX_SCROLL_DELTA} and {_MAX_SCROLL_DELTA}"
            if not any(deltas):
                return "scroll requires a non-zero delta_x or delta_y"
            if (tool_input.get("x") is None) != (tool_input.get("y") is None):
                return "x and y must be provided together for scroll"
        _backend, _host_id, _enabled, mode = self._binding()
        if mode == "background_app":
            if action == "list_displays":
                return (
                    f"{action} is unavailable in background_app mode; use list_windows"
                )
            if action in {
                "observe",
                "focus_window",
                "move",
                "click",
                "double_click",
                "drag",
                "scroll",
                "type",
                "keypress",
            } and not tool_input.get("window_id"):
                return "window_id is required in background_app mode; call list_windows first"
            if action == "scroll" and tool_input.get("x") is None:
                return "x and y are required over the target scroll area in background_app mode"
        return None

    def get_permission_key(self, tool_input: dict[str, Any]) -> str:
        return f"{self.name}:{str(tool_input.get('action', 'unknown')).strip() or 'unknown'}"

    async def check_permissions(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> PermissionResult:
        action = str(tool_input.get("action", "")).strip()
        key = self.get_permission_key(tool_input)
        if action in _READ_ONLY_ACTIONS:
            return PermissionResult(behavior=PermissionBehavior.ALLOW, permission_key=key)
        return PermissionResult(
            behavior=PermissionBehavior.ASK,
            reason=f"Computer Use action '{action}' will control the desktop",
            permission_key=key,
        )

    async def call(self, tool_input: dict[str, Any], context: ToolContext) -> ToolResult:
        backend, host_id, enabled, mode = self._binding()
        if not enabled or not backend or not host_id or not backend.is_available(host_id, mode):
            return ToolResult(
                result_for_model="Computer Use is unavailable or has been disabled by the user.",
                result_for_display="Computer Use 不可用或已被用户关闭",
                is_error=True,
            )

        try:
            result = await backend.execute(
                host_id,
                session_id=context.session_id,
                agent_id=context.agent_id,
                action=dict(tool_input),
                mode=mode,
            )
        except Exception as exc:
            return ToolResult(
                result_for_model=f"Computer Use failed: {exc}",
                result_for_display=f"Computer Use 失败：{exc}",
                is_error=True,
            )

        model_result = dict(result)
        screenshot = model_result.pop("screenshot", None)
        images: list[dict[str, str]] = []
        if isinstance(screenshot, dict):
            encoded = screenshot.get("data")
            media_type = str(screenshot.get("media_type") or "image/png")
            if isinstance(encoded, str) and encoded:
                if not media_type.lower().startswith("image/"):
                    model_result["screenshot_error"] = "host returned a non-image media type"
                    encoded = ""
            if isinstance(encoded, str) and encoded:
                try:
                    raw = base64.b64decode(encoded, validate=True)
                except (binascii.Error, ValueError):
                    model_result["screenshot_error"] = "host returned invalid base64 image data"
                else:
                    if len(raw) > MAX_INLINE_IMAGE_BYTES:
                        model_result["screenshot_error"] = "host screenshot exceeds the 20MB image limit"
                        raw = b""
                    if raw:
                        images.append({
                            "media_type": media_type,
                            "data": encoded,
                            "description": "Computer Use desktop observation",
                        })
                        model_result["screenshot"] = {
                            key: value for key, value in screenshot.items() if key != "data"
                        }

        failed = result.get("ok") is False
        return ToolResult(
            data=model_result,
            result_for_model=json.dumps(model_result, ensure_ascii=False, default=str),
            result_for_display=str(
                result.get("summary")
                or model_result.get("error")
                or "Computer Use action completed"
            ),
            is_error=failed,
            images=images,
        )
