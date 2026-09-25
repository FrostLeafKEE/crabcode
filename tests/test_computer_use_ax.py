import asyncio
import json

import pytest

from test_computer_use import FakeBackend, FakeSocket, prepared_tool
from crabcode_gateway.computer_use import ComputerUseBroker
from crabcode_core.computer_use_observation import compact_ax_result


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
    assert json.loads(result.result_for_model)["accessibility"]["tree"] == 'e1 text field (settable) "hello"'
    assert "elements" in result.data["accessibility"]  # UI/native diagnostics retain structured data.
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


def test_gateway_replays_the_full_compact_ax_preview_and_routes_it_to_the_completed_request():
    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = FakeSocket()
        caps = {"supported_modes": ["background_app"], "delivery_policy_version": 1}
        broker.register("h", socket, enabled=True, gui_available=True, capabilities=caps)
        async def complete(result):
            task = asyncio.create_task(broker.execute("h", session_id="s", agent_id="a",
                action={"action":"observe", "window_id":"7"}, target_scope="app_window"))
            await asyncio.sleep(0)
            request_id = socket.messages[-1]["request_id"]
            assert broker.resolve("h", request_id, result)
            assert await task == result
            return request_id

        frame = {"data":"cG5n", "frame_id":"older"}
        await complete({"ok":True, "screenshot":frame})
        result = {"ok":True, "observation_kind":"ax", "accessibility": {
            "snapshot_id":"s1", "truncated":True, "elements":[
                {"element_id":"e1", "role":"AXWindow", "title":"Demo"},
                {"element_id":"e2", "parent_id":"e1", "role":"AXStaticText", "value":"Content"},
            ]}}
        request_id = await complete(result)
        expected = compact_ax_result(result)["accessibility"]["tree"]
        await broker.publish_ax_preview("h", socket, request_id)
        assert socket.messages[-1] == {"type":"computer_use_ax_preview", "request_id":request_id,
            "session_id":"s", "agent_id":"a", "ax_tree":expected, "ax_element_count":2, "ax_truncated":True}
        retained = broker.restorable_previews("h")[0]
        assert retained["ax_tree"] == expected and retained["frame"] == frame
        assert "tree" not in result["accessibility"]  # Native result remains unchanged.
        broker.unregister("h", socket)
        reconnected = FakeSocket()
        broker.register("h", reconnected, enabled=True, gui_available=True, capabilities=caps)
        assert broker.restorable_previews("h")[0] == retained
        before = len(socket.messages)
        await broker.publish_ax_preview("h", socket, request_id)
        assert len(socket.messages) == before  # Never send through a replaced connection.
        socket = reconnected
        await complete({"ok":True, "screenshot":frame})
        assert broker.restorable_previews("h")[0]["ax_tree"] is None
        before = len(socket.messages)
        await broker.publish_ax_preview("h", socket, request_id)
        assert len(socket.messages) == before  # An old result cannot update the newer preview.
    asyncio.run(scenario())


def test_host_socket_delivers_formatted_ax_text_after_the_native_result():
    from types import SimpleNamespace
    from fastapi import WebSocketDisconnect
    from crabcode_gateway.routes.computer_use import computer_use_socket

    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        incoming, outgoing = asyncio.Queue(), asyncio.Queue()

        class Socket:
            app = SimpleNamespace(state=SimpleNamespace(computer_use_broker=broker, sessions={}))
            async def accept(self):
                pass
            async def receive_json(self):
                message = await incoming.get()
                if message is None:
                    raise WebSocketDisconnect()
                return message
            async def send_json(self, message):
                await outgoing.put(message)

        route = asyncio.create_task(computer_use_socket(Socket()))
        try:
            await incoming.put({"type":"computer_use_host_register", "host_id":"h", "enabled":True,
                "gui_available":True, "capabilities":{"supported_modes":["background_app"]}})
            assert (await asyncio.wait_for(outgoing.get(), 1))["type"] == "computer_use_host_registered"
            pending = asyncio.create_task(broker.execute("h", session_id="s", agent_id=None,
                action={"action":"observe", "window_id":"7"}))
            request = await asyncio.wait_for(outgoing.get(), 1)
            await incoming.put({"type":"computer_use_result", "request_id":request["request_id"], "result":{
                "ok":True, "observation_kind":"ax", "accessibility":{
                    "snapshot_id":"s1", "elements":[{"element_id":"e1", "role":"AXWindow", "title":"Demo"}]}}})
            preview = await asyncio.wait_for(outgoing.get(), 1)
            assert preview["type"] == "computer_use_ax_preview"
            assert preview["request_id"] == request["request_id"]
            assert preview["ax_tree"] == 'e1 window "Demo"'
            assert (await pending)["ok"]
        finally:
            await incoming.put(None)
            await route
    asyncio.run(scenario())
