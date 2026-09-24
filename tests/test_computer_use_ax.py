import asyncio
import json

import pytest

from test_computer_use import FakeBackend, FakeSocket, prepared_tool
from crabcode_gateway.computer_use import ComputerUseBroker


class AxBackend(FakeBackend):
    def capabilities(self, host_id):
        return {"ax_protocol_version": 1, "ax_available": True, "capture_available": False}

    async def execute(self, host_id, **kwargs):
        self.calls.append((host_id, kwargs))
        return {"ok": True, "observation_kind": "ax", "action_dispatched": False,
                "accessibility": {"snapshot_id": "snapshot", "elements": [
                    {"element_id": "e1", "role": "AXTextField", "value": "hello", "allow_set_value": True}]}}


def tool_for_window(backend=None):
    tool, context = prepared_tool(backend or AxBackend())
    context.session.computer_use_mode = "background_app"
    return tool, context


def test_ax_capability_exposes_semantic_actions_and_returns_text_without_images():
    tool, context = tool_for_window()
    props = tool.to_api_schema()["input_schema"]["properties"]
    assert {"press", "set_value", "perform_action"} <= set(props["action"]["enum"])
    assert "observation" in props
    result = asyncio.run(tool.call({"action": "observe", "window_id": "7"}, context))
    assert not result.is_error
    assert not result.images
    assert json.loads(result.result_for_model)["accessibility"]["snapshot_id"] == "snapshot"
    assert "SAME delivery policy" in asyncio.run(tool.get_prompt())


@pytest.mark.parametrize("mode, backend", [("background_app", FakeBackend()), ("foreground_desktop", AxBackend())])
def test_legacy_and_desktop_hosts_keep_coordinate_schema(mode, backend):
    tool, context = prepared_tool(backend)
    context.session.computer_use_mode = mode
    props = tool.to_api_schema()["input_schema"]["properties"]
    assert "press" not in props["action"]["enum"]
    assert "observation" not in props
    error = asyncio.run(tool.validate_input({"action": "press", "window_id": "7", "snapshot_id": "s", "element_id": "e1"}))
    assert "AX-capable" in error


@pytest.mark.parametrize("extra", [{"x": 1}, {"keys": ["CMD", "A"]}, {"observation": "ax"}])
def test_element_actions_cannot_mix_coordinate_or_observation_commands(extra):
    tool, _ = tool_for_window()
    assert asyncio.run(tool.validate_input({"action": "press", "window_id": "7", "snapshot_id": "s", "element_id": "e1", **extra}))


def test_set_value_accepts_empty_text_but_requires_scoped_references():
    tool, _ = tool_for_window()
    action = {"action": "set_value", "window_id": "7", "snapshot_id": "s", "element_id": "e1", "text": ""}
    assert asyncio.run(tool.validate_input(action)) is None
    assert asyncio.run(tool.validate_input({**action, "element_id": ""}))
    assert asyncio.run(tool.validate_input({**action, "text": "中" * 22000}))
    assert asyncio.run(tool.validate_input({"action": "click", "window_id": "7", "x": 1, "y": 2, "element_id": "e1"}))


def test_policy_change_does_not_remove_ax_observation_or_grant_focus():
    tool, context = tool_for_window()
    context.session.computer_use_delivery_policy = "strict_background"
    props = tool.to_api_schema()["input_schema"]["properties"]
    assert "press" in props["action"]["enum"]
    assert "focus_window" not in props["action"]["enum"]
    assert "only execute validated element operations" in asyncio.run(tool.get_prompt())


def test_gateway_negotiates_ax_and_preserves_preview_age_without_an_image():
    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = FakeSocket()
        caps = {"supported_modes": ["background_app"], "delivery_policy_version": 1}
        broker.register("h", socket, enabled=True, gui_available=True, capabilities=caps)
        action = {"action": "press", "window_id": "7", "snapshot_id": "s", "element_id": "e1"}
        rejected = await broker.execute("h", session_id="session", agent_id=None, action=action,
                                        target_scope="app_window", delivery_policy="allow_foreground")
        assert rejected["action_dispatched"] is False
        assert not socket.messages
        caps.update(ax_protocol_version=1, ax_available=True, capture_available=False)
        broker.update_state("h", socket, enabled=True, gui_available=True, capabilities=caps)
        assert broker.is_available("h", "background_app")
        assert broker.capabilities("h")["ax_available"]
        task = asyncio.create_task(broker.execute("h", session_id="session", agent_id="child", action=action,
                                                  target_scope="app_window", delivery_policy="strict_background"))
        await asyncio.sleep(0)
        message = socket.messages[-1]
        assert message["session_id"] == "session" and message["agent_id"] == "child"
        assert message["action"] == action
        result = {"ok": True, "observation_kind": "ax", "accessibility": {"snapshot_id": "next", "elements": [{"element_id": "e2"}]}}
        assert broker.resolve("h", message["request_id"], result)
        assert await task == result
        preview = broker.restorable_previews("h")[0]
        assert preview["observation_kind"] == "ax"
        assert preview["ax_element_count"] == 1
        assert preview["frame"] is None
    asyncio.run(scenario())
