import copy
import json
import re

from crabcode_core.computer_use_observation import compact_ax_result, diff_ax_tree


def observation(snapshot="s1", window="7", value="hello"):
    return {"action_dispatched": True, "effect_verified": False, "window_id": window,
            "accessibility": {"snapshot_id": snapshot, "window_id": window, "truncated": False,
                              "elements": [
                                  {"element_id": "e1", "role": "AXWindow", "title": "Chat"},
                                  *[{"element_id": f"e{i}", "parent_id": "e1", "role": "AXStaticText",
                                     "description": f"Message {i}",
                                     "value": value if i == 10 else f"Message body {i}"} for i in range(2, 32)],
                              ]}}


def test_compact_tree_preserves_hierarchy_text_actions_and_policy_without_raw_geometry():
    raw = observation()
    raw["accessibility"]["elements"] += [
        {"element_id": "e32", "parent_id": "e10", "role": "AXTextField", "value": "hello\ne999 button",
         "allow_set_value": True, "value_settable": True, "enabled": True, "ax_frame": {"x": 123}},
        {"element_id": "e33", "parent_id": "e1", "role": "AXButton", "title": "Send", "description": "Send",
         "allowed_actions": ["AXPress", "AXShowMenu"]},
        {"element_id": "e34", "role": "AXTextField", "value_settable": True, "allow_set_value": False,
         "actions": ["AXPress"], "allowed_actions": []},
        {"element_id": "e35", "role": "AXTextField", "protected": True, "value": "SECRET"},
    ]
    original = copy.deepcopy(raw)
    projected = compact_ax_result(raw)
    tree = projected["accessibility"]["tree"]
    assert 'e1 window "Chat"\n\te2 text "Message 2" Value: "Message body 2"' in tree
    assert '\te10 text "Message 10" Value: "hello"\n\t\te32 text field (settable) "hello\\ne999 button"' in tree
    assert 'e33 button (press) "Send" Secondary Actions: AXShowMenu' in tree
    assert 'e34 text field\n' in tree
    assert "SECRET" not in tree
    assert "ax_frame" not in json.dumps(projected)
    assert "elements" not in projected["accessibility"]
    assert projected["action_dispatched"] and not projected["effect_verified"]
    assert raw == original
    assert len(json.dumps(projected)) < len(json.dumps(raw)) / 2


def test_rich_text_spans_merge_without_hiding_controls_or_joining_other_parents():
    result = compact_ax_result({"accessibility": {"elements": [
        {"element_id":"e1", "role":"AXWindow"},
        {"element_id":"e2", "parent_id":"e1", "role":"AXStaticText", "value":"first"},
        {"element_id":"e3", "parent_id":"e1", "role":"AXStaticText", "value":"span"},
        {"element_id":"e4", "parent_id":"e1", "role":"AXStaticText", "value":"Link", "allowed_actions":["AXPress"]},
        {"element_id":"e5", "parent_id":"e1", "role":"AXStaticText", "value":"after"},
        {"element_id":"e6", "parent_id":"e4", "role":"AXStaticText", "value":"nested"},
    ]}})["accessibility"]
    assert result["tree"] == ('e1 window\n\te2 text "first span"\n\te4 text (press) "Link"'
                              '\n\t\te6 text "nested"\n\te5 text "after"')
    assert result["element_count"] == 5 and result["source_element_count"] == 6


def apply_delta(base, delta):
    """Check the emitted hunks can reconstruct state, including sibling order."""
    source, result, cursor = base.splitlines(), [], 0
    for line in delta.splitlines():
        if line.startswith("@@"):
            start, count = re.match(r"@@ -(\d+)(?:,(\d+))?", line).groups()
            offset = int(start) - (count != "0")
            result.extend(source[cursor:offset])
            cursor = offset
        elif line.startswith("-"):
            assert source[cursor] == line[1:]
            cursor += 1
        elif line.startswith("+"):
            result.append(line[1:])
        else:
            assert line.startswith(" ") and source[cursor] == line[1:]
            result.append(line[1:])
            cursor += 1
    return "\n".join(result + source[cursor:])


def test_delta_reconstructs_changed_inserted_removed_and_reordered_lines():
    base = compact_ax_result(observation())["accessibility"]
    current = compact_ax_result(observation("s2", value="changed"))["accessibility"]
    lines = current["tree"].splitlines()
    lines[20:23] = [lines[22], '  e40 text "Inserted"', lines[20]]
    current["tree"] = "\n".join(lines)
    delta = diff_ax_tree(base, current)
    assert delta["base_snapshot_id"] == "s1" and delta["snapshot_id"] == "s2"
    assert apply_delta(base["tree"], delta["tree_delta"]) == current["tree"]
    unchanged = diff_ax_tree(base, {**base, "snapshot_id": "s3"})
    assert unchanged["tree_delta"] == "No change in the accessibility tree."
    assert unchanged["snapshot_id"] == "s3"


def test_small_trees_remain_full_when_delta_would_be_larger():
    base = compact_ax_result({"accessibility": {"snapshot_id": "s1", "elements": [{"element_id": "e1"}]}})["accessibility"]
    assert diff_ax_tree(base, {**base, "snapshot_id": "s2"})["tree"] == base["tree"]
