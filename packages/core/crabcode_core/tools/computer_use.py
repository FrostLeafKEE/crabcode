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
            "x": {"type": "integer", "description": "Absolute desktop X coordinate."},
            "y": {"type": "integer", "description": "Absolute desktop Y coordinate."},
            "to_x": {"type": "integer", "description": "Drag destination X coordinate."},
            "to_y": {"type": "integer", "description": "Drag destination Y coordinate."},
            "button": {
                "type": "string",
                "enum": ["left", "middle", "right"],
                "description": "Mouse button. Defaults to left.",
            },
            "delta_x": {"type": "integer", "description": "Horizontal scroll amount."},
            "delta_y": {"type": "integer", "description": "Vertical scroll amount."},
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
            "window_id": {"type": "string", "description": "Window to observe or focus."},
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

    def _binding(self) -> tuple[Any | None, str | None, bool]:
        session = self._session
        return (
            getattr(session, "computer_use_backend", None),
            getattr(session, "computer_use_host_id", None),
            getattr(session, "computer_use_enabled", False) is True,
        )

    def is_available(self, context: ToolContext) -> bool:
        if not self.is_enabled:
            return False
        backend, host_id, enabled = self._binding()
        return bool(enabled and backend and host_id and backend.is_available(host_id))

    async def get_prompt(self, **kwargs: Any) -> str:
        return (
            "Use ComputerUse to inspect and operate the graphical desktop when a task requires native UI interaction. "
            "The coordinate space is the full desktop and may include multiple displays. Start with observe or "
            "list_displays/list_windows, then use the returned image dimensions and origin for coordinates. "
            "Prefer one deliberate action per call and observe again after navigation or any action whose result is uncertain. "
            "The user can see your actions and can disable Computer Use at any time."
        )

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
        if (
            action == "scroll"
            and tool_input.get("delta_x") is None
            and tool_input.get("delta_y") is None
        ):
            return "delta_x or delta_y required for scroll"
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
        backend, host_id, enabled = self._binding()
        if not enabled or not backend or not host_id or not backend.is_available(host_id):
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
