"""Read real image pixels through the worker, query loop and provider adapters."""

import asyncio
import base64
from functools import partial
from io import BytesIO
import json
import struct
from unittest.mock import AsyncMock, patch

from PIL import Image
import pytest

from crabcode_core.api.anthropic_adapter import _messages_to_api
from crabcode_core.api.base import StreamChunk
from crabcode_core.api.codex_adapter import _messages_to_responses_input
from crabcode_core.api.openai_adapter import _messages_to_openai
from crabcode_core.image_io import load_image_preview
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.tools.file_read import FileReadTool
from crabcode_core.tools.image import ImageTool
from crabcode_core.types.config import ApiConfig
from crabcode_core.types.event import ErrorEvent, ToolResultEvent, TurnCompleteEvent
from crabcode_core.types.message import ImageBlock, UserMessage, create_user_message
from crabcode_core.types.tool import ToolContext
from crabcode_gateway.schemas import core_event_to_payload


def write_psd(path):
    # A real two-pixel, three-channel PSD with a saved uncompressed RGB composite.
    header = b"8BPS" + struct.pack(">H6sHIIHH", 1, b"\0" * 6, 3, 1, 2, 8, 3)
    path.write_bytes(header + b"\0" * 12 + b"\0\0" + bytes([255, 0, 0, 255, 0, 0]))


def decoded_image(result):
    assert not result.is_error, result.result_for_model
    assert len(result.images) == 1
    im = Image.open(BytesIO(base64.b64decode(result.images[0]["data"])))
    im.load()
    return im


@pytest.mark.parametrize("suffix,fmt,mime", [
    (".PNG", "PNG", "image/png"), (".jpeg", "JPEG", "image/jpeg"),
    (".webp", "WEBP", "image/webp"), (".bmp", "BMP", "image/png"),
    (".tiff", "TIFF", "image/png"), (".gif", "GIF", "image/png"),
    ("", "PNG", "image/png"), (".txt", "JPEG", "image/jpeg"),
])
def test_read_image_bytes_survive_filesystem_worker(tmp_path, suffix, fmt, mime):
    path = tmp_path / ("中文 image" + suffix)
    Image.new("RGB", (12, 8), "red").save(path, format=fmt)
    before = path.read_bytes()
    result = asyncio.run(FileReadTool().call(
        {"file_path": path.name, "limit": 5, "offset": 999}, ToolContext(cwd=str(tmp_path)),
    ))
    pixels = decoded_image(result)
    assert pixels.size == (12, 8)
    assert pixels.convert("RGB").getpixel((0, 0))[0] >= 250
    assert result.images[0]["media_type"] == mime
    assert "offset/limit ignored" in result.result_for_model
    assert path.read_bytes() == before


@pytest.mark.parametrize("tool,key", [(FileReadTool, "file_path"), (ImageTool, "path")])
def test_psd_composite_is_converted_to_real_png(tmp_path, tool, key):
    path = tmp_path / "角色介绍图.psd"
    write_psd(path)
    before = path.read_bytes()
    result = asyncio.run(tool().call({key: str(path)}, ToolContext()))
    pixels = decoded_image(result)
    assert result.images[0]["media_type"] == "image/png"
    assert pixels.format == "PNG"
    assert [pixels.getpixel((x, 0)) for x in range(2)] == [(255, 0, 0), (0, 255, 0)]
    assert "saved PSD composite preview" in result.result_for_model
    assert path.read_bytes() == before


def test_text_svg_and_empty_files_preserve_read_behavior(tmp_path):
    for name, content, expected in [
        ("code.txt", "甲\r\n乙\r\n", "     2|乙"),
        ("icon.svg", '<svg>\n<path d="M0 0"/>\n</svg>', '     2|<path d="M0 0"/>'),
        ("empty.txt", "", "File is empty."),
    ]:
        path = tmp_path / name
        path.write_bytes(content.encode("utf-8"))
        result = asyncio.run(FileReadTool().call(
            {"file_path": str(path), "offset": 2, "limit": 1}, ToolContext(),
        ))
        assert not result.is_error
        assert not result.images
        assert result.result_for_model == expected
        assert "content" in result.data


@pytest.mark.parametrize("suffix", [".png", ".psd", ".heic"])
def test_corrupt_or_unsupported_image_has_actionable_error(tmp_path, suffix):
    path = tmp_path / ("broken" + suffix)
    path.write_bytes(b"not an image\x80")
    result = asyncio.run(FileReadTool().call({"file_path": str(path)}, ToolContext()))
    assert result.is_error
    assert result.images == []
    assert "Export a flattened PNG or JPEG" in result.result_for_model
    assert "utf-8" not in result.result_for_model


def test_animated_image_returns_first_frame(tmp_path):
    path = tmp_path / "animation.gif"
    Image.new("RGB", (4, 3), "red").save(
        path, save_all=True, append_images=[Image.new("RGB", (4, 3), "blue")], duration=100,
    )
    result = asyncio.run(FileReadTool().call({"file_path": str(path)}, ToolContext()))
    assert decoded_image(result).convert("RGB").getpixel((0, 0)) == (255, 0, 0)
    assert "first frame/page only" in result.result_for_model


def test_large_preview_and_exif_rotation(tmp_path):
    path = tmp_path / "large.jpg"
    exif = Image.Exif()
    exif[274] = 6
    Image.new("RGB", (6000, 3000), "red").save(path, exif=exif)
    result = asyncio.run(FileReadTool().call({"file_path": str(path)}, ToolContext()))
    assert decoded_image(result).size == (2048, 4096)
    assert "original 6000x3000" in result.result_for_model


def test_preview_obeys_byte_and_decode_limits(tmp_path):
    path = tmp_path / "noise.bmp"
    Image.effect_noise((128, 128), 100).convert("RGB").save(path)
    with patch("crabcode_core.types.tool.MAX_INLINE_IMAGE_BYTES", 2048):
        preview = load_image_preview(path)
    assert len(preview.data) <= 2048
    assert max(preview.width, preview.height) < 128
    with patch("crabcode_core.image_io.MAX_DECODE_PIXELS", 100):
        with pytest.raises(ValueError, match="decode limit"):
            load_image_preview(path)


def test_image_batch_decode_failure_does_not_attach_partial_results(tmp_path):
    Image.new("RGB", (4, 4), "red").save(tmp_path / "good.png")
    (tmp_path / "bad.png").write_bytes(b"bad image")
    context = ToolContext(cwd=str(tmp_path))
    result = asyncio.run(ImageTool().call({"path": ["good.png", "bad.png"]}, context))
    assert result.is_error
    assert result.images == context.emitted_images == []


def test_image_mime_matches_content_not_filename(tmp_path):
    path = tmp_path / "misnamed.jpg"
    Image.new("RGB", (4, 4), "red").save(path, format="PNG")
    result = asyncio.run(ImageTool().call({"path": str(path)}, ToolContext()))
    assert result.images[0]["media_type"] == "image/png"
    rejected = asyncio.run(ImageTool().call(
        {"path": str(path), "mime_type": "image/jpeg"}, ToolContext(),
    ))
    assert rejected.is_error
    assert rejected.images == []


def test_image_read_does_not_notify_text_language_server(tmp_path):
    path = tmp_path / "image.png"
    Image.new("RGB", (4, 4), "red").save(path)
    lsp = AsyncMock()
    result = asyncio.run(FileReadTool().call(
        {"file_path": str(path)}, ToolContext(lsp_manager=lsp),
    ))
    assert not result.is_error
    lsp.touch_file.assert_not_called()


@pytest.mark.parametrize("tool_name,key", [("Read", "file_path"), ("Image", "path")])
def test_psd_pixels_reach_next_model_request_and_clients(tmp_path, tool_name, key):
    path = tmp_path / "preview.psd"
    write_psd(path)

    class Adapter:
        config = ApiConfig(model="vision-test", max_retries=0)

        def __init__(self):
            self.requests = []

        async def stream_message(self, messages, **kwargs):
            self.requests.append([message.model_copy(deep=True) for message in messages])
            if len(self.requests) == 1:
                yield StreamChunk(type="tool_use_start", tool_use_id="view", tool_name=tool_name)
                yield StreamChunk(type="tool_use_end", tool_use_id="view", tool_name=tool_name,
                                  tool_input_json=json.dumps({key: str(path)}))
            else:
                yield StreamChunk(type="text", text="Image received")
                yield StreamChunk(type="message_stop")

    adapter = Adapter()
    params = QueryParams(
        messages=[create_user_message("Inspect this PSD")], system_prompt=[],
        user_context={}, system_context={}, tools=[FileReadTool(), ImageTool()],
        tool_context=ToolContext(), api_adapter=adapter,
    )

    async def collect():
        return [event async for event in query_loop(params)]

    events = asyncio.run(collect())
    assert not [event for event in events if isinstance(event, ErrorEvent)]
    assert isinstance(events[-1], TurnCompleteEvent)
    assert len(adapter.requests) == 2
    result = next(event for event in events if isinstance(event, ToolResultEvent))
    assert len(core_event_to_payload(result).model_dump()["images"]) == 1
    restored = UserMessage.model_validate_json(adapter.requests[-1][-1].model_dump_json())
    images = [block for block in restored.content if isinstance(block, ImageBlock)]
    assert len(images) == 1
    expected = base64.b64decode(images[0].source["data"])
    with Image.open(BytesIO(expected)) as pixels:
        assert [pixels.getpixel((x, 0)) for x in range(2)] == [(255, 0, 0), (0, 255, 0)]
    # Check actual provider wire serializers, not just UI events or a path string.
    for convert, kind, image_key in (
        (_messages_to_responses_input, "input_image", "image_url"),
        (partial(_messages_to_openai, system=[]), "image_url", "image_url"),
        (_messages_to_api, "image", "source"),
    ):
        wire = convert(adapter.requests[-1])
        blocks = [block for item in wire if isinstance(item.get("content"), list)
                  for block in item["content"] if block.get("type") == kind]
        assert len(blocks) == 1
        payload = blocks[0][image_key]
        if kind == "image":
            data = base64.b64decode(payload["data"])
        else:
            url = payload["url"] if isinstance(payload, dict) else payload
            assert url.startswith("data:image/png;base64,")
            data = base64.b64decode(url.split(",", 1)[1])
        assert data == expected
