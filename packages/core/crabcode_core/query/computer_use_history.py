"""Bound desktop screenshot history in requests, retaining the full transcript."""

from crabcode_core.types.message import ImageBlock, Message, TextBlock, ToolResultBlock


def project_computer_use_history(messages: list[Message], keep: int = 5) -> list[Message]:
    """Keep recent ComputerUse frames; never remove user or other tool images.

    Only unambiguous ComputerUse result messages are eligible. Their receipts
    (window IDs, action, outcome and screenshot metadata) remain in the request.
    Shallow message copies ensure persisted images and the UI stay unchanged.
    """
    if keep < 1:
        raise ValueError("At least one desktop observation must be retained")
    calls = {
        block.id
        for message in messages
        for block in message.tool_use_blocks
        if block.name == "ComputerUse"
    }
    eligible = []
    for index, message in enumerate(messages):
        content = message.content
        if not isinstance(content, list) or not any(isinstance(b, ImageBlock) for b in content):
            continue
        results = [b for b in content if isinstance(b, ToolResultBlock)]
        if (
            len(results) == 1
            and results[0].tool_use_id in calls
            and all(isinstance(b, (ToolResultBlock, ImageBlock)) for b in content)
        ):
            eligible.append(index)
    if len(eligible) <= keep:
        return messages
    projected = list(messages)
    for index in eligible[:-keep]:
        message = messages[index]
        assert isinstance(message.content, list)
        content = [b for b in message.content if not isinstance(b, ImageBlock)]
        content.append(TextBlock(text=(
            "[Older ComputerUse screenshot omitted from this request; its action and window "
            "receipt are retained above. Use the recent screenshots to judge the current UI.]"
        )))
        projected[index] = message.model_copy(update={"content": content})
    return projected
