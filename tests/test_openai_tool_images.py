"""Tool images must not interrupt a Chat Completions tool-result batch."""

import pytest

from crabcode_core.api.openai_adapter import _messages_to_openai
from crabcode_core.types.message import (
    ToolUseBlock,
    create_assistant_message,
    create_tool_result_message,
    create_user_message,
    message_from_entry,
)


@pytest.mark.parametrize("order", [("image", "bash"), ("bash", "image")])
@pytest.mark.parametrize("restore", [False, True])
def test_tool_images_follow_all_results(order, restore):
    messages = [create_assistant_message([
        ToolUseBlock(id="bash", name="Bash", input={}),
        ToolUseBlock(id="image", name="Image", input={}),
    ])]
    image = {"data": "AA==", "media_type": "image/png"}
    for tool_id in order:
        messages.append(create_tool_result_message(
            tool_id, "done", images=[image, image] if tool_id == "image" else [],
        ))
    messages.append(create_assistant_message("Finished"))
    if restore:
        messages = [message_from_entry({**m.model_dump(mode="json"), "type": m.role.value})
                    for m in messages]
    before = [m.model_dump_json() for m in messages]
    wire = _messages_to_openai(messages, [])
    assert [m["role"] for m in wire] == ["assistant", "tool", "tool", "user", "assistant"]
    assert [m["tool_call_id"] for m in wire if m["role"] == "tool"] == list(order)
    assert len(wire[3]["content"]) == 2
    assert wire[3]["content"][0] == wire[3]["content"][1]
    assert [m.model_dump_json() for m in messages] == before


def test_multiple_image_tools_and_batches_keep_image_order():
    messages = []
    for batch in range(2):
        ids = [f"{batch}-a", f"{batch}-b"]
        messages.append(create_assistant_message([
            ToolUseBlock(id=tool_id, name="Image", input={}) for tool_id in ids
        ]))
        for tool_id in reversed(ids):
            messages.append(create_tool_result_message(
                tool_id, "done", images=[{"data": tool_id, "media_type": "image/png"}],
            ))
    wire = _messages_to_openai(messages, [])
    assert [m["role"] for m in wire] == ["assistant", "tool", "tool", "user", "user"] * 2
    urls = [m["content"][0]["image_url"]["url"] for m in wire if m["role"] == "user"]
    assert urls == [f"data:image/png;base64,{i}" for i in ("0-b", "0-a", "1-b", "1-a")]


@pytest.mark.parametrize("boundary", [None, "user", "assistant"])
def test_incomplete_batch_is_reported_without_dropping_images(boundary):
    messages = [
        create_assistant_message([
            ToolUseBlock(id="missing", name="Bash", input={}),
            ToolUseBlock(id="image", name="Image", input={}),
        ]),
        create_tool_result_message("image", "done", images=[{"data": "AA=="}]),
    ]
    if boundary == "user":
        messages.append(create_user_message("Continue"))
    elif boundary == "assistant":
        messages.append(create_assistant_message("Finished"))
    with pytest.raises(ValueError, match="missing"):
        _messages_to_openai(messages, [])
