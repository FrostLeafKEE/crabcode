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


@pytest.mark.parametrize("delivery_policy", ["strict_background", "allow_foreground"])
def test_gateway_keeps_monitor_frames_out_of_core_results_and_replays_them(delivery_policy):
    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = FakeSocket()
        caps = {"supported_modes": ["background_app"], "delivery_policy_version": 1}
        broker.register("h", socket, enabled=True, gui_available=True, capabilities=caps)
        async def complete(result):
            task = asyncio.create_task(broker.execute("h", session_id="s", agent_id="a",
                action={"action":"observe", "window_id":"7"}, target_scope="app_window", delivery_policy=delivery_policy))
            await asyncio.sleep(0)
            request_id = socket.messages[-1]["request_id"]
            assert broker.resolve("h", request_id, result)
            assert await task == {key: value for key, value in result.items()
                                  if key not in ("preview_screenshot", "preview_screenshot_error")}
            return request_id

        frame = {"data":"cG5n", "frame_id":"older"}
        await complete({"ok":True, "screenshot":frame})
        monitor_frame = {"data":"cHJldmlldy1vbmx5", "frame_id":"monitor"}
        result = {"ok":True, "preview_screenshot":monitor_frame, "observation_kind":"ax", "accessibility": {
            "snapshot_id":"s1", "truncated":True, "elements":[
                {"element_id":"e1", "role":"AXWindow", "title":"Demo"},
                {"element_id":"e2", "parent_id":"e1", "role":"AXStaticText", "value":"Content"},
            ]}}
        await complete(result)
        retained = broker.restorable_previews("h")[0]
        assert retained["frame"] == monitor_frame
        assert retained["observation_kind"] == "ax"
        assert retained["ax_element_count"] == 2
        assert result["preview_screenshot"] == monitor_frame  # Native result remains unchanged.
        broker.unregister("h", socket)
        reconnected = FakeSocket()
        broker.register("h", reconnected, enabled=True, gui_available=True, capabilities=caps)
        assert broker.restorable_previews("h")[0] == retained
        socket = reconnected
        # A failed optional preview keeps the prior image and its capture time.
        await complete({"ok":True, "accessibility":result["accessibility"], "preview_screenshot_error":"No capture permission"})
        assert broker.restorable_previews("h")[0]["frame"] == monitor_frame
        assert broker.restorable_previews("h")[0]["frame_updated_at_ms"] == retained["frame_updated_at_ms"]
        # An explicitly requested model screenshot still reaches Core normally.
        await complete({"ok":True, "screenshot":frame})
        assert broker.restorable_previews("h")[0]["frame"] == frame
        assert broker.restorable_previews("h")[0]["observation_kind"] == "screenshot"
    asyncio.run(scenario())


@pytest.mark.parametrize("with_model_screenshot", [False, True])
def test_core_never_promotes_monitor_frames_to_model_images_or_history(with_model_screenshot):
    class PreviewBackend(AxBackend):
        async def execute(self, *args, **kwargs):
            result = await super().execute(*args, **kwargs)
            result["preview_screenshot"] = {"data": "cHJldmlldy1vbmx5", "media_type": "image/png"}
            result["preview_screenshot_error"] = "monitor-only error"
            if with_model_screenshot:
                result["screenshot"] = {"data": "cG5n", "media_type": "image/png"}
            return result

    tool, context = tool_for_window(PreviewBackend())
    result = asyncio.run(tool.call({"action": "observe", "window_id": "7"}, context))
    assert not result.is_error
    assert len(result.images) == int(with_model_screenshot)
    assert all(image["data"] == "cG5n" for image in result.images)
    for value in (result.result_for_model, json.dumps(result.data), result.result_for_display):
        assert "preview_screenshot" not in value
        assert "cHJldmlldy1vbmx5" not in value
        assert "monitor-only error" not in value
    assert json.loads(result.result_for_model)["accessibility"]["tree"]


@pytest.mark.parametrize("action", ["observe", "click", "focus_window", "press"])
def test_empty_ax_fallback_delivers_an_image_to_the_model(action):
    class EmptyAxBackend(AxBackend):
        async def execute(self, *args, **kwargs):
            return {
                "ok": True, "action": action, "action_dispatched": action != "observe",
                "observation_kind": "screenshot", "fallback_reason": "ax_empty",
                "ax_error": "The window exposes only window chrome",
                "screenshot": {"data": "cG5n", "media_type": "image/png"},
            }

    tool, context = tool_for_window(EmptyAxBackend())
    command = {"action": action, "window_id": "7"}
    if action == "click":
        command.update(x=10, y=20)
    if action == "press":
        command.update(snapshot_id="s", element_id="e1")
    result = asyncio.run(tool.call(command, context))
    assert not result.is_error
    assert len(result.images) == 1
    assert result.images[0]["data"] == "cG5n"
    receipt = json.loads(result.result_for_model)
    assert receipt["fallback_reason"] == "ax_empty"
    assert receipt["observation_kind"] == "screenshot"
    assert receipt["action_dispatched"] == (action != "observe")


def test_host_socket_retains_preview_image_without_including_it_in_the_tool_result():
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
            frame = {"data":"cHJldmlldy1vbmx5", "frame_id":"monitor"}
            await incoming.put({"type":"computer_use_result", "request_id":request["request_id"], "preview_screenshot":frame, "result":{
                "ok":True, "observation_kind":"ax", "accessibility":{
                    "snapshot_id":"s1", "elements":[{"element_id":"e1", "role":"AXWindow", "title":"Demo"}]}}})
            result = await asyncio.wait_for(pending, 1)
            assert result["ok"] and result["accessibility"]["snapshot_id"] == "s1"
            assert "screenshot" not in result and "preview_screenshot" not in result
            assert broker.restorable_previews("h")[0]["frame"] == frame
            assert outgoing.empty()  # AX belongs in the tool card, not a monitor callback.
        finally:
            await incoming.put(None)
            await route
    asyncio.run(scenario())
