"""Generate images with the active Codex subscription and return inline artifacts."""

from __future__ import annotations

import base64
import binascii
import mimetypes
from pathlib import Path
from typing import Any
from uuid import uuid4

import httpx

from crabcode_core.types.tool import MAX_INLINE_IMAGE_BYTES, Tool, ToolContext, ToolResult


class ImageGenerateTool(Tool):
    name = "ImageGenerate"
    description = (
        "Generate or edit a raster image using the current Codex auth.json login; "
        "no separate OpenAI API key is needed. Describe the subject, style, composition, "
        "exact text and constraints in prompt. For edits or visual references, pass "
        "reference_image_paths after inspecting those images with Image. State each "
        "reference's role and what must remain unchanged in prompt. Saves unique files "
        "under output/imagegen in the current workspace and attaches images inline. "
        "Use once per asset; generation may take several minutes. Only use when the "
        "user requests image generation or the task needs a generated image."
    )
    is_concurrency_safe = True
    input_schema = {
        "type": "object",
        "properties": {
            "prompt": {"type": "string", "minLength": 1},
            "reference_image_paths": {
                "type": "array", "maxItems": 5,
                "items": {"type": "string", "minLength": 1},
                "description": "Optional local images for editing or visual reference, relative to the workspace or absolute.",
            },
        },
        "required": ["prompt"],
        "additionalProperties": False,
    }

    def is_available(self, context: ToolContext) -> bool:
        return self.is_enabled and bool(getattr(context.api_adapter, "supports_image_generation", False))

    async def call(self, tool_input: dict[str, Any], context: ToolContext) -> ToolResult:
        if not self.is_available(context):
            return ToolResult(is_error=True, result_for_model="ImageGenerate requires an active Codex auth.json model.")
        prompt = tool_input.get("prompt")
        if not isinstance(prompt, str) or not prompt.strip():
            return ToolResult(is_error=True, result_for_model="Error: prompt is required")
        paths = tool_input.get("reference_image_paths", [])
        if not isinstance(paths, list) or len(paths) > 5 or any(not isinstance(p, str) or not p.strip() for p in paths):
            return ToolResult(is_error=True, result_for_model="Error: provide at most five reference image paths")

        try:
            references = []
            for raw_path in paths:
                path = Path(raw_path).expanduser()
                if not path.is_absolute():
                    path = Path(context.cwd) / path
                media_type = mimetypes.guess_type(path.name)[0]
                if media_type not in {"image/png", "image/jpeg", "image/webp", "image/gif"}:
                    raise ValueError(f"Unsupported reference image type: {path}")
                if path.stat().st_size > MAX_INLINE_IMAGE_BYTES:
                    raise ValueError("Reference image exceeds the 20MB limit")
                raw = path.read_bytes()
                if not raw or len(raw) > MAX_INLINE_IMAGE_BYTES:
                    raise ValueError("Reference image is empty or exceeds the 20MB limit")
                references.append({"media_type": media_type, "data": base64.b64encode(raw).decode("ascii")})

            images = await context.api_adapter.generate_images(
                prompt.strip(), model=context.model or context.api_adapter.config.model,
                reference_images=references,
            )
            # Validate all results before saving or publishing any image.
            decoded = []
            for image in images:
                encoded = image["data"]
                if len(encoded) > 4 * ((MAX_INLINE_IMAGE_BYTES + 2) // 3):
                    raise ValueError("Generated image exceeds the 20MB limit")
                raw = base64.b64decode(encoded, validate=True)
                if not raw or len(raw) > MAX_INLINE_IMAGE_BYTES:
                    raise ValueError("Generated image is empty or exceeds the 20MB limit")
                suffix = {"image/png": ".png", "image/jpeg": ".jpg", "image/webp": ".webp"}.get(image["media_type"])
                if not suffix:
                    raise ValueError("Unsupported generated image MIME type")
                decoded.append((raw, image["media_type"], suffix))
            if not decoded:
                raise ValueError("The provider returned no generated image")

            output_dir = Path(context.cwd).resolve() / "output" / "imagegen"
            output_dir.mkdir(parents=True, exist_ok=True)
            saved = []
            for raw, media_type, suffix in decoded:
                path = output_dir / f"image-{uuid4().hex}{suffix}"
                with path.open("xb") as file:
                    file.write(raw)
                saved.append(str(path))
                context.emit_image(raw, media_type)
            message = "Generated image(s):\n" + "\n".join(saved)
            return ToolResult(
                data={"paths": saved}, result_for_model=message,
                result_for_display=message, images=context.emitted_images[-len(saved):],
            )
        except (OSError, ValueError, binascii.Error, RuntimeError, httpx.HTTPError) as exc:
            return ToolResult(is_error=True, result_for_model=f"Image generation failed: {exc}")
