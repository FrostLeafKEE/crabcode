"""Compact, indexed AX text for model requests; native receipts stay structured."""

from __future__ import annotations

import json
import re
from difflib import unified_diff
from typing import Any


TREE_FORMAT = "indexed_text_v1"
_ELEMENT_ID = re.compile(r"e[0-9]+\Z")
_ROLES = {
    "AXWindow": "window", "AXGroup": "container", "AXWebArea": "web content",
    "AXStaticText": "text", "AXTextField": "text field", "AXTextArea": "text area",
    "AXButton": "button", "AXImage": "image", "AXLink": "link",
    "AXScrollArea": "scroll area", "AXCheckBox": "checkbox", "AXRadioButton": "radio button",
    "AXPopUpButton": "pop up button", "AXComboBox": "combo box", "AXMenuItem": "menu item",
    "AXCloseButton": "close button", "AXMinimizeButton": "minimize button",
    "AXZoomButton": "zoom button", "AXFullScreenButton": "full screen button",
    "AXStandardWindow": "window", "AXSecureTextField": "secure text field",
}


def _label(value: Any) -> str:
    # Keep UI content on one quoted line, including text that resembles tree syntax.
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _node_text(node: dict[str, Any]) -> str:
    role = str(node.get("role") or "element")
    label_role = str(node.get("subrole") or role)
    parts = [_ROLES.get(label_role, label_role.removeprefix("AX"))]
    flags = []
    allowed = node.get("allowed_actions") or []
    if node.get("enabled") is False:
        flags.append("disabled")
        allowed = []
    elif not node.get("protected"):
        if "AXPress" in allowed or "AXPick" in allowed:
            flags.append("press")
        if node.get("allow_set_value"):
            flags.append("settable")
    if node.get("selected"):
        flags.append("selected")
    if node.get("focused"):
        flags.append("focused")
    if node.get("protected"):
        flags.append("protected")
        allowed = []
    if flags:
        parts.append(f"({', '.join(flags)})")
    seen = []
    for field in ("title", "description", "value"):
        value = node.get(field)
        if node.get("protected") and field != "title":
            continue
        if value is None or value == "" or value in seen:
            continue
        seen.append(value)
        parts.append(("Value: " if field == "value" and len(seen) > 1 else "") + _label(value))
    secondary = [a for a in allowed if a not in ("AXPress", "AXPick") and not (
        role in ("AXStaticText", "AXImage", "AXGroup") and a in ("AXShowMenu", "AXScrollToVisible")
    )]
    if secondary:
        parts.append("Secondary Actions: " + ", ".join(secondary))
    return " ".join(parts)


def compact_ax_result(result: dict[str, Any]) -> dict[str, Any]:
    """Return a model-only projection, without mutating the host data or images."""
    accessibility = result.get("accessibility")
    if not isinstance(accessibility, dict) or not isinstance(accessibility.get("elements"), list):
        return result
    nodes = {
        node["element_id"]: node for node in accessibility["elements"]
        if isinstance(node, dict) and isinstance(node.get("element_id"), str)
        and _ELEMENT_ID.fullmatch(node["element_id"])
    }
    children: dict[str | None, list[str]] = {}
    for element_id, node in nodes.items():
        parent = node.get("parent_id")
        if parent not in nodes or parent == element_id:
            parent = None
        children.setdefault(parent, []).append(element_id)
    merged_values: dict[str, str] = {}
    merged_away: set[str] = set()
    # Rich text often exposes one AXStaticText per styled span. Join adjacent
    # passive leaves under the same parent, never controls or protected values.
    # The representative ID is still a real native node; it advertises no input.
    for siblings in children.values():
        run: list[str] = []
        for element_id in [*siblings, None]:
            node = nodes.get(element_id, {})
            plain_text = (node.get("role") == "AXStaticText" and isinstance(node.get("value"), str)
                          and not node.get("title") and not node.get("description")
                          and not children.get(element_id) and node.get("enabled") is not False
                          and not any(node.get(flag) for flag in ("protected", "focused", "selected", "allow_set_value"))
                          and set(node.get("allowed_actions") or []) <= {"AXShowMenu", "AXScrollToVisible"})
            if plain_text:
                run.append(element_id)
                continue
            if len(run) > 1:
                merged_values[run[0]] = " ".join(nodes[key]["value"] for key in run)
                merged_away.update(run[1:])
            run = []
    lines = []
    visited = set()
    # DFS makes document text readable even when an older host returns BFS JSON.
    pending = [(root, 0) for root in reversed(children.get(None, []))]
    # Appending disconnected nodes also tolerates malformed cyclic parents.
    pending = [(element_id, 0) for element_id in reversed(nodes)] + pending
    while pending:
        element_id, depth = pending.pop()
        if element_id in visited:
            continue
        visited.add(element_id)
        if element_id in merged_away:
            continue
        node = nodes[element_id]
        if element_id in merged_values:
            node = {**node, "value": merged_values[element_id]}
        lines.append("\t" * min(depth, 64) + element_id + " " + _node_text(node))
        pending.extend((child, depth + 1) for child in reversed(children.get(element_id, [])))
    tree = {k: v for k, v in accessibility.items() if k not in ("elements", "coordinate_space")}
    tree.update(tree_format=TREE_FORMAT, element_count=len(lines), source_element_count=len(nodes), tree="\n".join(lines))
    return {**result, "accessibility": tree}


def diff_ax_tree(previous: dict[str, Any], current: dict[str, Any]) -> dict[str, Any]:
    """Use a delta only when it is smaller and its full base remains in context."""
    if (previous.get("tree_format") != TREE_FORMAT or current.get("tree_format") != TREE_FORMAT
            or not previous.get("snapshot_id") or not current.get("snapshot_id")
            or not isinstance(previous.get("tree"), str) or not isinstance(current.get("tree"), str)):
        return current
    # Standard hunks preserve parent indentation AND sibling reading order,
    # including inserted/removed/reordered elements whose IDs may be renumbered.
    changes = list(unified_diff(previous["tree"].splitlines(), current["tree"].splitlines(), n=1, lineterm=""))[2:]
    delta = {
        **{k: v for k, v in current.items() if k != "tree"},
        "base_snapshot_id": previous["snapshot_id"],
        "tree_delta": "\n".join(changes) if changes else "No change in the accessibility tree.",
    }
    return delta if len(json.dumps(delta, ensure_ascii=False)) < len(json.dumps(current, ensure_ascii=False)) else current
