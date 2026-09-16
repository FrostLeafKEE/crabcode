"""ImageTool - attach local images with optional inline descriptions."""

from __future__ import annotations

import mimetypes
from dataclasses import replace
from pathlib import Path
from typing import Any

from crabcode_core.types.tool import PermissionBehavior, PermissionResult, Tool, ToolContext, ToolResult


class ImageTool(Tool):
    """Emit local images as separate ImageContent blocks.

    Codex tools send image bytes through ``emitImage`` instead of putting a
    filesystem path in Markdown.  This tool provides the same explicit
    boundary to the model after another tool creates an image file.
    """

    name = "Image"
    is_read_only = True
    is_concurrency_safe = True
    input_schema: dict[str, Any] = {
        "type": "object",
        "properties": {
            "path": {
                "anyOf": [
                    {"type": "string", "minLength": 1},
                    {"type": "array", "items": {"type": "string", "minLength": 1}, "minItems": 1},
                ],
                "description": "One absolute or session-relative image path, or a non-empty list of image paths in display order.",
            },
            "description": {
                "anyOf": [
                    {"type": "string"},
                    {"type": "array", "items": {"type": "string"}, "minItems": 1},
                ],
                "description": "Optional plain-text caption below each image. Use a string for one image, or a list matching the paths in length and order. Use an empty string to omit an individual caption.",
            },
            "mime_type": {
                "type": "string",
                "description": "Optional image MIME type override applied to all paths. Omit to detect each file's MIME type independently.",
            },
        },
        "required": ["path"],
    }

    async def get_prompt(self, **kwargs: Any) -> str:
        return (
            "Attach one or more local images to the conversation as inline image results. "
            "Use this after an image is generated or saved to disk. The image "
            "bytes are sent as a separate ImageContent block, so do not put a "
            "file:// or local filesystem path in Markdown. Pass path as a string "
            "or a non-empty list of paths. Optionally pass description as a string "
            "for one image or a list matching the paths in length and order; each "
            "description appears below its image as plain text. Empty descriptions "
            "are allowed. All files are validated before any images are attached. "
            "Only pass mime_type when an override is needed for all images."
        )

    async def check_permissions(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> PermissionResult:
        return PermissionResult(behavior=PermissionBehavior.ALLOW)

    async def call(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> ToolResult:
        raw_paths = tool_input.get("path", tool_input.get("file_path", ""))
        if raw_paths == "":
            raw_paths = tool_input.get("file_path", "")
        paths = [raw_paths] if isinstance(raw_paths, str) else raw_paths
        if not isinstance(paths, list) or not paths:
            return ToolResult(result_for_model="Error: path must be a string or a non-empty list of strings", is_error=True)
        if any(not isinstance(path, str) or not path.strip() for path in paths):
            return ToolResult(result_for_model="Error: every path must be a non-empty string", is_error=True)

        descriptions = tool_input.get("description", [""] * len(paths))
        if isinstance(descriptions, str):
            descriptions = [descriptions]
        if (
            not isinstance(descriptions, list)
            or len(descriptions) != len(paths)
            or any(not isinstance(description, str) for description in descriptions)
        ):
            return ToolResult(
                result_for_model="Error: description must be a string for one image or a list of strings matching the paths in length and order",
                is_error=True,
            )

        # Stage attachments so a later invalid file cannot leave a partial batch
        # in the invocation sink, which the tool runner merges even on errors.
        staged = replace(context, emitted_images=[])
        details = []
        messages = []
        for raw_path, description in zip(paths, descriptions):
            result = self._attach_one(raw_path.strip(), description, tool_input, staged)
            if result.is_error:
                return result
            details.append(result.data)
            messages.append(result.result_for_model)
        context.emitted_images.extend(staged.emitted_images)
        message = "\n".join(messages)
        return ToolResult(
            data=details[0] if isinstance(raw_paths, str) else {"images": details},
            result_for_model=message,
            result_for_display=message,
            images=staged.emitted_images,
        )

    def _attach_one(
        self, raw_path: str, description: str, tool_input: dict[str, Any], context: ToolContext,
    ) -> ToolResult:
        if not raw_path:
            return ToolResult(result_for_model="Error: path is required", is_error=True)

        path = Path(raw_path).expanduser()
        if not path.is_absolute():
            path = Path(context.cwd) / path
        try:
            path = path.resolve()
        except OSError:
            # Keep the original path for a useful error if resolution fails.
            pass

        if not path.exists():
            return ToolResult(result_for_model=f"Error: image not found: {path}", is_error=True)
        if not path.is_file():
            return ToolResult(result_for_model=f"Error: not a file: {path}", is_error=True)

        mime_type = str(
            tool_input.get("mime_type", "")
            or tool_input.get("mimeType", "")
            or ""
        ).strip()
        if not mime_type:
            mime_type = mimetypes.guess_type(path.name)[0] or ""
        if not mime_type.lower().startswith("image/"):
            return ToolResult(
                result_for_model=(
                    f"Error: cannot determine an image MIME type for {path}; "
                    "pass mime_type explicitly"
                ),
                is_error=True,
            )

        try:
            data = path.read_bytes()
            context.emit_image(data, mime_type, description=description)
        except (OSError, TypeError, ValueError) as exc:
            return ToolResult(result_for_model=f"Error attaching image {path}: {exc}", is_error=True)

        message = f"Image attached: {path} ({mime_type}, {len(data)} bytes)"
        if description:
            message += f"\nDescription: {description}"
        return ToolResult(
            data={"path": str(path), "media_type": mime_type, "bytes": len(data), **({"description": description} if description else {})},
            result_for_model=message,
            result_for_display=message,
            images=[context.emitted_images[-1]],
        )
