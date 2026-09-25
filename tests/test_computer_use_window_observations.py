import asyncio
import base64
import copy
import json

import pytest

from crabcode_core.tools import computer_use
from crabcode_gateway.computer_use import ComputerUseBroker
from test_computer_use import FakeSocket
from test_computer_use_ax import AxBackend, tool_for_window


def frame(window, data=b"pixels"):
    return {"data": base64.b64encode(data).decode(), "media_type": "image/png", "width": 100,
            "height": 80, "origin_x": 40, "origin_y": 60, "target": f"window:{window}"}


def native_result(with_ax=False):
    result = {"ok": True, "action": "click", "action_dispatched": True,
              "observation_kind": "ax_and_screenshot" if with_ax else "screenshot",
              "window_observations": [{"window_id": "9", "kind": "auxiliary", "owner_window_id": None,
                  "relationship_uncertain": True, "coordinate_space": "window", "screenshot": frame("9", b"popup")}]}
    if with_ax:
        result["accessibility"] = {"snapshot_id": "ax-root", "elements": [
            {"element_id": "e1", "role": "AXTextField", "value": "root content"}]}
        result["preview_screenshot"] = frame("7", b"monitor-only")
    else:
        result["screenshot"] = frame("7", b"root")
    return result


class PopupBackend(AxBackend):
    def __init__(self, result, version=1):
        super().__init__()
        self.result = result
        self.version = version

    def capabilities(self, host_id):
        return {**super().capabilities(host_id), "window_observation_version": self.version}

    async def execute(self, host_id, **kwargs):
        self.calls.append((host_id, kwargs))
        return self.result


@pytest.mark.parametrize("with_ax", [False, True])
def test_independent_images_keep_root_ax_and_separate_coordinate_targets(with_ax):
    original = native_result(with_ax)
    before = copy.deepcopy(original)
    backend = PopupBackend(original)
    tool, context = tool_for_window(backend)
    output = asyncio.run(tool.call({"action": "click", "window_id": "7", "x": 10, "y": 20}, context))
    assert not output.is_error
    assert len(output.images) == (1 if with_ax else 2)
    result = json.loads(output.result_for_model)
    popup = result["window_observations"][0]
    assert popup["screenshot"]["image_index"] == len(output.images)
    assert popup["screenshot"]["target"] == "window:9"
    assert popup["owner_window_id"] is None and popup["relationship_uncertain"] is True
    assert "window 9" in output.images[-1]["description"]
    assert base64.b64decode(output.images[-1]["data"]) == b"popup"
    assert backend.calls[0][1]["action"]["include_window_observations"] is True
    assert result["action_dispatched"] is True
    if with_ax:
        assert "root content" in result["accessibility"]["tree"]
        assert "screenshot" not in result and "preview_screenshot" not in result
    else:
        assert result["screenshot"]["image_index"] == 1
    for value in (output.result_for_model, json.dumps(output.data)):
        assert '"data"' not in value
        assert base64.b64encode(b"monitor-only").decode() not in value
    assert original == before


@pytest.mark.parametrize("version", [None, 0, 2])
def test_old_hosts_never_receive_unrecognized_observation_arguments(version):
    backend = PopupBackend({"ok": True}, version)
    tool, context = tool_for_window(backend)
    action = {"action": "observe", "window_id": "7"}
    asyncio.run(tool.call(action, context))
    assert backend.calls[0][1]["action"] == action
    assert asyncio.run(tool.validate_input({**action, "include_window_observations": True}))


@pytest.mark.parametrize("bad_frame", [
    {"data": "cG5n", "media_type": "text/plain"},
    {"data": "invalid base64", "media_type": "image/png"},
])
def test_invalid_popup_image_does_not_erase_dispatch_or_leak_into_text(bad_frame):
    result = native_result()
    result["window_observations"][0]["screenshot"] = bad_frame
    tool, context = tool_for_window(PopupBackend(result))
    output = asyncio.run(tool.call({"action": "observe", "window_id": "7"}, context))
    assert not output.is_error
    assert len(output.images) == 1
    popup = output.data["window_observations"][0]
    assert "screenshot" not in popup
    assert popup["screenshot_error"]
    assert output.data["action_dispatched"] is True


def test_combined_image_budget_and_window_count_are_bounded(monkeypatch):
    monkeypatch.setattr(computer_use, "MAX_INLINE_IMAGE_BYTES", 8)
    result = native_result()
    result["window_observations"] *= 4
    tool, context = tool_for_window(PopupBackend(result))
    output = asyncio.run(tool.call({"action": "observe", "window_id": "7"}, context))
    assert len(output.images) == 1  # Root is 4 bytes; each popup needs another 5.
    assert len(output.data["window_observations"]) == 3
    assert all(entry["screenshot_error"] for entry in output.data["window_observations"])
    assert output.data["window_observation_error"]
    assert '"data"' not in output.result_for_model


def test_gateway_delivers_both_images_to_core_without_promoting_monitor_frames():
    broker = ComputerUseBroker(timeout_seconds=1)
    socket = FakeSocket()
    broker.register("desktop-test", socket, enabled=True, gui_available=True, capabilities={
        "supported_modes": ["background_app"], "delivery_policy_version": 1,
        "ax_protocol_version": 1, "ax_available": True, "window_observation_version": 1,
    })
    tool, context = tool_for_window(broker)

    async def scenario():
        task = asyncio.create_task(tool.call({"action": "observe", "window_id": "7"}, context))
        await asyncio.sleep(0)
        request = socket.messages[-1]
        assert request["action"]["include_window_observations"] is True
        result = native_result()
        assert broker.resolve("desktop-test", request["request_id"], result, preview_screenshot=frame("7", b"monitor-only"))
        output = await task
        assert [base64.b64decode(image["data"]) for image in output.images] == [b"root", b"popup"]
        assert broker.restorable_previews("desktop-test")[0]["frame"] == result["screenshot"]
    asyncio.run(scenario())
