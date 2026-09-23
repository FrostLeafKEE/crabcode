from crabcode_core.query.loop import _prepend_user_context
from crabcode_core.query.computer_use_history import project_computer_use_history
from crabcode_core.types.message import (
    ImageBlock, TextBlock, ToolUseBlock, create_assistant_message,
    create_tool_result_message, create_user_message,
)


def test_request_projection_keeps_recent_frames_and_full_persisted_history():
    history = []
    for index in range(9):
        history.extend([
            create_assistant_message([ToolUseBlock(id=str(index), name="ComputerUse", input={"action": "observe"})]),
            create_tool_result_message(str(index), f"receipt-{index}", images=[{"data": f"frame-{index}"}]),
        ])
    originals = [message.model_dump() for message in history]
    for context in ({}, {"project": "test"}):
        projected = _prepend_user_context(history, context)
        images = [b.source["data"] for m in projected if isinstance(m.content, list)
                  for b in m.content if isinstance(b, ImageBlock)]
        assert images == [f"frame-{index}" for index in range(4, 9)]
        assert [m.model_dump() for m in history] == originals
        first = projected[1 + bool(context)]
        assert first.content[0].content == "receipt-0"
        assert "omitted" in first.content[1].text
        assert first.uuid == history[1].uuid


def test_user_images_other_tools_and_ambiguous_mixed_messages_are_preserved():
    image = ImageBlock(source={"type": "base64", "data": "user-reference"})
    history = [create_user_message([TextBlock(text="Compare this reference"), image])]
    for name in ("Read", "ComputerUse", "ComputerUse"):
        call_id = str(len(history))
        history.extend([
            create_assistant_message([ToolUseBlock(id=call_id, name=name, input={})]),
            create_tool_result_message(call_id, "receipt", images=[{"data": call_id}]),
        ])
    mixed = create_tool_result_message("3", "receipt", images=[{"data": "mixed"}])
    mixed.content.append(TextBlock(text="user text"))
    history.insert(0, mixed)
    projected = project_computer_use_history(history, keep=1)
    assert projected[0] is mixed
    assert projected[1] is history[1]
    assert projected[3] is history[3]  # Read result
    assert projected[-1] is history[-1]
    assert not any(isinstance(b, ImageBlock) for b in projected[-3].content)
