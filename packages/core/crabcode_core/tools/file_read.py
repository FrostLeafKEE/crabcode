"""FileReadTool — read file contents."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from crabcode_core.logging_utils import get_logger
from crabcode_core.text_io import read_utf8_text
from crabcode_core.tools._input_helpers import first_non_empty_str
from crabcode_core.types.tool import Tool, ToolContext, ToolResult
from crabcode_core.io_worker import run_file_tool

logger = get_logger(__name__)


class FileReadTool(Tool):
    name = "Read"
    description = "Read a text file or view a local image, including a saved PSD composite preview."
    is_read_only = True
    is_concurrency_safe = True
    input_schema = {
        "type": "object",
        "properties": {
            "file_path": {
                "type": "string",
                "minLength": 1,
                "description": "The absolute or relative path of the file to read.",
            },
            "offset": {
                "type": "integer",
                "description": "Text only: line number to start reading from (1-based). Negative values count from end. Ignored for images.",
            },
            "limit": {
                "type": "integer",
                "description": "Text only: number of lines to read. Ignored for images.",
            },
        },
        "required": ["file_path"],
    }

    async def get_prompt(self, **kwargs: Any) -> str:
        return (
            "Read text or visually inspect an image from the local filesystem. "
            "For images, the actual image content is returned to you for visual analysis, "
            "not just a filename or a UI preview. Supports PNG, JPEG, WebP, GIF, BMP, TIFF "
            "and saved PSD composite previews. Non-native formats are converted to PNG; "
            "large images may be resized. Animated/multipage images return the first frame/page. "
            "PSD previews show the saved merged appearance, not editable layers. "
            "offset and limit apply only to text and are ignored for images.\n\n"
            "You can optionally specify a line offset and limit (especially "
            "useful for large files), but it's recommended to read the whole "
            "file by not providing these parameters.\n\n"
            "Lines in the output are numbered starting at 1, in the format "
            "LINE_NUMBER|LINE_CONTENT. The LINE_NUMBER prefix is metadata — "
            "do NOT treat it as part of the actual code.\n\n"
            "IMPORTANT: You MUST read a file at least once before editing it. "
            "You can call Read on multiple files in a single response to read "
            "them in parallel.\n\n"
            "If the file exists but is empty, you will receive 'File is empty.'.\n"
            "If the file does not exist, you will receive an error."
        )

    async def call(
        self, tool_input: dict[str, Any], context: ToolContext,
    ) -> ToolResult:
        result = await run_file_tool(self.name, tool_input, context)
        if (not result.is_error and result.data and not result.images
                and context.lsp_manager is not None):
            asyncio.create_task(_lsp_touch(result.data["file_path"], context.lsp_manager))
        return result

    async def _call_local(
        self,
        tool_input: dict[str, Any],
        context: ToolContext,
    ) -> ToolResult:
        file_path = first_non_empty_str(
            tool_input,
            ("file_path", "filePath", "path", "target_file", "filepath", "file"),
        )
        offset = tool_input.get("offset")
        limit = tool_input.get("limit")

        if not file_path:
            return ToolResult(
                result_for_model=(
                    "Error: file_path is required. Pass the file path as \"file_path\" "
                    "(aliases: filePath, path, target_file, filepath, file)."
                ),
                is_error=True,
            )

        path = Path(file_path).expanduser()
        if not path.is_absolute():
            path = Path(context.cwd) / path

        if not path.exists():
            return ToolResult(
                result_for_model=f"Error: file not found: {path}",
                is_error=True,
            )

        if not path.is_file():
            return ToolResult(
                result_for_model=f"Error: not a file: {path}",
                is_error=True,
            )

        try:
            from crabcode_core.image_io import is_image_file, load_image_preview

            if is_image_file(path):
                preview = load_image_preview(path)
                context.emit_image(preview.data, preview.media_type)
                message = f"Image read: {path} ({preview.summary()})"
                if offset is not None or limit is not None:
                    message += "\nText offset/limit ignored for this image."
                return ToolResult(
                    data={"file_path": str(path), "media_type": preview.media_type,
                          "width": preview.width, "height": preview.height,
                          "source_format": preview.source_format},
                    result_for_model=message,
                    result_for_display=message,
                    images=[context.emitted_images[-1]],
                )
            content = read_utf8_text(path).text
        except Exception as e:
            return ToolResult(
                result_for_model=f"Error reading file: {e}",
                is_error=True,
            )

        if not content:
            return ToolResult(
                result_for_model="File is empty.",
                data={"file_path": str(path), "content": ""},
            )

        lines = content.splitlines(keepends=True)
        total_lines = len(lines)

        if offset is not None:
            if offset < 0:
                start = max(0, total_lines + offset)
            else:
                start = max(0, offset - 1)
        else:
            start = 0

        if limit is not None:
            end = min(total_lines, start + limit)
        else:
            end = total_lines

        selected = lines[start:end]

        numbered = []
        for i, line in enumerate(selected, start=start + 1):
            numbered.append(f"{i:6d}|{line.rstrip()}")

        result_text = "\n".join(numbered)

        # Background LSP touch — notifies language servers about this file
        # so diagnostics are ready when the user writes later. Non-blocking.
        lsp = context.lsp_manager
        if lsp is not None:
            try:
                asyncio.get_event_loop().create_task(
                    _lsp_touch(str(path), lsp),
                )
            except Exception:
                pass

        return ToolResult(
            data={"file_path": str(path), "content": content, "total_lines": total_lines},
            result_for_model=result_text,
        )


async def _lsp_touch(file_path: str, lsp: Any) -> None:
    """Fire-and-forget LSP touch_file call."""
    try:
        await lsp.touch_file(file_path)
    except Exception:
        logger.debug("LSP touch_file failed for %s", file_path, exc_info=True)
