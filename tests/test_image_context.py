"""Image transport bytes must not be mistaken for model context tokens."""

import asyncio
import base64
import json
import struct
import zlib
from functools import partial
from unittest.mock import AsyncMock, patch

import httpx
import pytest

from crabcode_core.api.anthropic_adapter import _messages_to_api
from crabcode_core.api.base import StreamChunk
from crabcode_core.api.codex_adapter import _messages_to_responses_input
from crabcode_core.api.openai_adapter import _messages_to_openai
from crabcode_core.compact.compact import (
    compact_conversation,
    estimate_token_count,
    _serialize_message,
)
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.tools.image import ImageTool
from crabcode_core.types.config import ApiConfig
from crabcode_core.types.event import ErrorEvent, ToolResultEvent, TurnCompleteEvent
from crabcode_core.types.message import (
    ImageBlock,
    UserMessage,
    create_assistant_message,
    create_user_message,
)
from crabcode_core.types.tool import ToolContext
from crabcode_gateway.schemas import core_event_to_payload


def png_bytes(metadata_bytes=0):
    """Same pixels with different encoded sizes, all valid PNG chunks."""
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1536, 1024, 8, 2, 0, 0, 0))
        + chunk(b"tEXt", b"Comment\0" + b"A" * metadata_bytes)
        + chunk(b"IDAT", zlib.compress((b"\0" + b"\x80" * (1536 * 3)) * 1024))
        + chunk(b"IEND", b"")
    )


def image_block(raw):
    return ImageBlock(source={
        "type": "base64", "media_type": "image/png",
        "data": base64.b64encode(raw).decode("ascii"),
    })


def test_visual_estimate_is_independent_of_encoded_file_size():
    small, large = png_bytes(), png_bytes(2_224_363)
    assert len(large) > 2_224_363
    counts = [estimate_token_count([create_user_message([image_block(raw)])])
              for raw in (small, large)]
    assert counts[0] == counts[1]
    assert 0 < counts[0] < 10_000


def test_repeated_images_each_consume_budget_without_mutation():
    image = image_block(png_bytes())
    single = create_user_message([image])
    double = create_user_message([image, image])
    before = double.model_dump_json()
    base = estimate_token_count([create_user_message([])])
    assert estimate_token_count([double]) - base == 2 * (
        estimate_token_count([single]) - base
    )
    assert double.model_dump_json() == before
    # Text, tool schemas and system instructions must still consume context.
    assert estimate_token_count([double], system=["x" * 40_000], tools=[{
        "name": "large_schema", "description": "y" * 40_000,
    }]) > estimate_token_count([double]) + 19_000


@pytest.mark.parametrize("url", [
    "https://example.invalid/image.png?signature=" + "x" * 100_000,
    "data:image/png;base64," + "A" * 3_000_000,
], ids=["signed-url", "data-url"])
def test_image_url_is_not_tokenized_or_fetched(url):
    message = create_user_message([ImageBlock(source={"type": "url", "url": url})])
    assert 0 < estimate_token_count([message]) < 10_000
    if url.startswith("data:"):
        serialized = _serialize_message(message)
        assert "data:image" not in serialized
        assert len(serialized) < 200


class Adapter:
    def __init__(self, tool_input=None, failure=None):
        self.config = ApiConfig(model="gpt-5.6-sol", max_retries=0)
        self.tool_input = tool_input
        self.failure = failure
        self.requests = []

    async def stream_message(self, messages, **kwargs):
        self.requests.append([message.model_copy(deep=True) for message in messages])
        if isinstance(self.failure, Exception):
            raise self.failure
        if self.failure:
            yield StreamChunk(type="error", error=self.failure)
        elif self.tool_input is not None and len(self.requests) == 1:
            yield StreamChunk(type="tool_use_start", tool_use_id="images", tool_name="Image")
            yield StreamChunk(type="tool_use_end", tool_use_id="images", tool_name="Image",
                              tool_input_json=json.dumps(self.tool_input))
        else:
            yield StreamChunk(type="text", text="两张图片已附上。")
            yield StreamChunk(type="message_stop")


def run_query(adapter, messages=None, context_window=1_050_000):
    params = QueryParams(
        messages=messages if messages is not None else [
            create_user_message("Earlier request"),
            create_assistant_message("Earlier reply"),
            create_user_message("请同时发送两张相同图片"),
        ],
        system_prompt=[], user_context={}, system_context={},
        tools=[ImageTool()], tool_context=ToolContext(),
        api_adapter=adapter, context_window=context_window,
    )

    async def collect():
        return [event async for event in query_loop(params)]

    return asyncio.run(collect()), params.messages


@pytest.mark.parametrize("descriptions", [None, ["第一张", "第二张"]])
def test_two_large_identical_images_reach_next_request_and_both_clients(tmp_path, descriptions):
    path = tmp_path / "image.png"
    path.write_bytes(png_bytes(2_224_363))
    tool_input = {"path": [str(path), str(path)]}
    if descriptions is not None:
        tool_input["description"] = descriptions
    adapter = Adapter(tool_input=tool_input)
    with patch("crabcode_core.query.loop.compact_conversation", new_callable=AsyncMock) as compact:
        events, messages = run_query(adapter)
    compact.assert_not_awaited()
    assert not [event for event in events if isinstance(event, ErrorEvent)]
    assert len(adapter.requests) == 2
    assert isinstance(events[-1], TurnCompleteEvent)
    assert events[-1].reason == "end_turn"
    assert events[-1].context_used_tokens < 20_000

    result = next(event for event in events if isinstance(event, ToolResultEvent))
    assert not result.is_error
    # The shared Gateway payload used by Desktop and VS Code retains both copies.
    payload = core_event_to_payload(result).model_dump()
    assert len(payload["images"]) == 2
    assert payload["images"][0]["data"] == payload["images"][1]["data"]
    assert [image.get("description", "") for image in result.images] == (descriptions or ["", ""])

    result_message = adapter.requests[-1][-1]
    restored = UserMessage.model_validate_json(result_message.model_dump_json())
    assert len([block for block in restored.content if isinstance(block, ImageBlock)]) == 2
    assert any(message.uuid == restored.uuid for message in messages)
    # Verify actual provider serialization, including exactly identical attachments.
    for convert, image_type in (
        (_messages_to_responses_input, "input_image"),
        (partial(_messages_to_openai, system=[]), "image_url"),
        (_messages_to_api, "image"),
    ):
        wire_images = [block for item in convert([restored])
                       if isinstance(item.get("content"), list)
                       for block in item["content"] if block.get("type") == image_type]
        assert len(wire_images) == 2
        assert wire_images[0] == wire_images[1]


def test_real_text_overflow_is_still_blocked():
    adapter = Adapter()
    events, _ = run_query(adapter, [create_user_message("x" * 200_000)], context_window=32_000)
    assert adapter.requests == []
    assert any(isinstance(event, ErrorEvent) and event.error_type == "context_overflow"
               for event in events)


def test_compaction_keeps_current_images_but_omits_old_binary_payloads():
    image = image_block(png_bytes(2_224_363))
    messages = [
        create_user_message([image]), create_assistant_message("Old reply"),
        create_user_message([image, image]), create_assistant_message("Current reply"),
    ]
    compacted = asyncio.run(compact_conversation(messages, custom_summary="Old image summarized",
                                                 keep_tokens=512))
    assert compacted is not None
    images = [block for message in compacted if isinstance(message.content, list)
              for block in message.content if isinstance(block, ImageBlock)]
    assert len(images) == 2
    assert images[0] == images[1] == image
    assert estimate_token_count(compacted) < estimate_token_count(messages)


@pytest.mark.parametrize("failure", [
    "HTTP 413; request too large for model",
    "Error code: 413 - request body exceeded the size limit",
    "request_too_large",
    "Payload Too Large",
    httpx.HTTPStatusError("body rejected", request=httpx.Request("POST", "https://example.invalid"),
                          response=httpx.Response(413)),
])
def test_request_byte_limit_is_reported_separately_without_compaction_or_retry(failure):
    adapter = Adapter(failure=failure)
    with patch("crabcode_core.query.loop.compact_conversation", new_callable=AsyncMock) as compact:
        events, _ = run_query(adapter)
    compact.assert_not_awaited()
    assert len(adapter.requests) == 1
    errors = [event for event in events if isinstance(event, ErrorEvent)]
    assert len(errors) == 1
    assert errors[0].error_type == "request_too_large"
    assert errors[0].recoverable
    assert "attachment file sizes" in errors[0].message


def test_context_error_containing_number_413_is_not_a_byte_limit_error():
    adapter = Adapter(failure="context window exceeded: input has 413 tokens")
    with patch("crabcode_core.query.loop.compact_conversation", new_callable=AsyncMock,
               return_value=None) as compact:
        events, _ = run_query(adapter)
    compact.assert_awaited_once()
    assert any(isinstance(event, ErrorEvent) and event.error_type == "context_overflow"
               for event in events)
