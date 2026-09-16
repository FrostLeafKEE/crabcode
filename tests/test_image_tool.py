"""Image batches, captions, and the live/durable attachment contract."""

import base64
from pathlib import Path
from unittest.mock import patch

import pytest

from crabcode_core.api.anthropic_adapter import _messages_to_api
from crabcode_core.query.loop import _run_tools
from crabcode_core.tools.image import ImageTool
from crabcode_core.types.message import AssistantMessage, ImageBlock, ToolUseBlock, UserMessage
from crabcode_core.types.tool import ToolContext
from crabcode_gateway.schemas import core_event_to_payload


@pytest.fixture
def image_context(tmp_path):
    (tmp_path / "a.png").write_bytes(b"first image")
    (tmp_path / "b.jpg").write_bytes(b"second image")
    return ToolContext(cwd=str(tmp_path))


def test_single_image_compatibility_and_caption(image_context):
    import asyncio

    for extra in ({}, {"description": "单图说明\n第二行"}):
        result = asyncio.run(ImageTool().call({"path": "a.png", **extra}, image_context))
        assert not result.is_error
        assert len(result.images) == 1
        assert result.data["path"] == str(Path(image_context.cwd) / "a.png")
        assert result.images[0].get("description", "") == extra.get("description", "")
        assert base64.b64decode(result.images[0]["data"]) == b"first image"


@pytest.mark.parametrize("descriptions", [None, ["前", "后"], ["", "后"]])
def test_batch_order_and_per_file_mime(image_context, descriptions):
    import asyncio

    tool_input = {"path": ["a.png", "b.jpg"]}
    if descriptions is not None:
        tool_input["description"] = descriptions
    result = asyncio.run(ImageTool().call(tool_input, image_context))
    assert not result.is_error
    assert [image["media_type"] for image in result.images] == ["image/png", "image/jpeg"]
    assert [base64.b64decode(image["data"]) for image in result.images] == [b"first image", b"second image"]
    assert [image.get("description", "") for image in result.images] == (descriptions or ["", ""])


@pytest.mark.parametrize("tool_input", [
    {"path": []}, {"path": 42}, {"path": ["a.png", None]}, {"path": ["a.png", " "]},
    {"path": ["a.png", "b.jpg"], "description": "ambiguous"},
    {"path": ["a.png", "b.jpg"], "description": ["missing second"]},
    {"path": "a.png", "description": [123]},
    {"path": ["a.png", "missing.png"]},
])
def test_bad_batches_do_not_emit_partial_images(image_context, tool_input):
    import asyncio

    result = asyncio.run(ImageTool().call(tool_input, image_context))
    assert result.is_error
    assert result.images == []
    assert image_context.emitted_images == []


def test_byte_limit_failure_leaves_sink_unchanged(image_context):
    import asyncio

    image_context.emit_image(b"existing", "image/png")
    before = list(image_context.emitted_images)
    with patch("crabcode_core.types.tool.MAX_INLINE_IMAGE_BYTES", 11):
        result = asyncio.run(ImageTool().call({"path": ["a.png", "b.jpg"]}, image_context))
    assert result.is_error
    assert image_context.emitted_images == before


def test_captions_survive_runner_gateway_and_history(image_context):
    import asyncio

    async def run():
        calls = [
            ToolUseBlock(id="batch", name="Image", input={"path": ["a.png", "b.jpg"], "description": ["前", "后"]}),
            ToolUseBlock(id="single", name="Image", input={"path": "a.png", "description": "单图"}),
        ]
        return [item async for item in _run_tools(calls, AssistantMessage(content=calls), [ImageTool()], image_context)]

    results = asyncio.run(run())
    for (messages, event), captions in zip(results, [["前", "后"], ["单图"]]):
        payload = core_event_to_payload(event).model_dump()
        assert [image["description"] for image in payload["images"]] == captions
        restored = UserMessage.model_validate_json(messages[-1].model_dump_json())
        images = [block for block in restored.content if isinstance(block, ImageBlock)]
        assert [image.description for image in images] == captions
        # UI metadata must never leak into the provider's image source object.
        wire_images = [block for block in _messages_to_api([restored])[0]["content"] if block["type"] == "image"]
        assert len(wire_images) == len(captions)
        assert all(set(block) == {"type", "source"} for block in wire_images)
        assert all(set(block["source"]) == {"type", "media_type", "data"} for block in wire_images)
    assert image_context.emitted_images == []
