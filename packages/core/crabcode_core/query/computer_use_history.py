"""Bound desktop observation history in requests, retaining the full transcript."""

import json

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
    ax_results: list[tuple[int, ToolResultBlock, dict]] = []
    for index, message in enumerate(messages):
        content = message.content
        if not isinstance(content, list):
            continue
        results = [b for b in content if isinstance(b, ToolResultBlock)]
        if (
            len(results) == 1
            and results[0].tool_use_id in calls
            and all(isinstance(b, (ToolResultBlock, ImageBlock)) for b in content)
        ):
            if any(isinstance(b, ImageBlock) for b in content):
                eligible.append(index)
            try:
                data = json.loads(results[0].content)
            except (ValueError, TypeError):
                continue
            if (isinstance(data, dict) and isinstance(data.get("accessibility"), dict)
                    and isinstance(data["accessibility"].get("elements"), list)):
                ax_results.append((index, results[0], data))
    # Full trees contain substantially more text than image metadata. Keep two
    # recent observations for comparison; receipts and persisted UI stay intact.
    if len(eligible) <= keep and len(ax_results) <= 2:
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
    for index, block, data in ax_results[:-2]:
        tree = data["accessibility"]
        tree["omitted_element_count"] = len(tree.pop("elements"))
        tree["history_note"] = "Historical AX tree omitted; observe again before using element references."
        replacement = block.model_copy(update={"content": json.dumps(data, ensure_ascii=False)})
        message = projected[index]
        assert isinstance(message.content, list)
        projected[index] = message.model_copy(update={
            "content": [replacement if b is block else b for b in message.content],
        })
    return projected
