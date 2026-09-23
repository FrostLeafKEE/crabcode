import asyncio
import base64
import json
from types import SimpleNamespace

import pytest

from crabcode_core.api.base import StreamChunk
from crabcode_core.events import CoreSession
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.tools.computer_use import ComputerUseTool
from crabcode_core.types.config import ApiConfig, CrabCodeSettings, ToolLoadingSettings
from crabcode_core.types.event import TurnCompleteEvent
from crabcode_core.types.message import create_user_message
from crabcode_core.types.tool import ToolContext
from crabcode_gateway.computer_use import ComputerUseBroker
from crabcode_gateway.routes.computer_use import _sync_bound_sessions


class FakeBackend:
    def __init__(self, available: bool = True):
        self.available = available
        self.calls = []

    def is_available(self, host_id, mode=None):
        return self.available and host_id == "desktop-test"

    async def execute(self, host_id, **kwargs):
        self.calls.append((host_id, kwargs))
        return {
            "ok": True,
            "summary": "Observed desktop",
            "screenshot": {
                "data": base64.b64encode(b"png-data").decode(),
                "media_type": "image/png",
                "width": 10,
                "height": 10,
            },
        }


class SessionBinding:
    def __init__(self, backend):
        self.computer_use_backend = backend
        self.computer_use_host_id = "desktop-test"
        self.computer_use_enabled = True
        self.computer_use_mode = "foreground_desktop"
        self.computer_use_delivery_policy = "allow_foreground"


class RecordingAdapter:
    def __init__(self):
        self.requests = []
        self.systems = []
        self.config = ApiConfig(model="test", thinking_enabled=False, max_tokens=1000, max_retries=0)

    async def count_input_tokens(self, *args):
        return None

    async def stream_message(self, messages, system, tools, config):
        self.requests.append(tools)
        self.systems.append(system)
        yield StreamChunk(type="text", text="done")
        yield StreamChunk(type="message_stop")


def prepared_tool(backend):
    tool = ComputerUseTool()
    context = ToolContext(session=SessionBinding(backend), session_id="session-test")
    asyncio.run(tool.setup(context))
    asyncio.run(tool.resolve_prompt())
    return tool, context


def test_disabled_or_missing_gui_omits_schema_and_prompt_from_model_context():
    backend = FakeBackend(available=False)
    tool, context = prepared_tool(backend)
    adapter = RecordingAdapter()
    params = QueryParams(
        messages=[create_user_message("inspect the desktop")],
        system_prompt=[],
        user_context={},
        system_context={},
        tools=[tool],
        tool_context=context,
        api_adapter=adapter,
        api_config=adapter.config,
        tool_loading=ToolLoadingSettings(mode="discovery"),
        auto_compact_enabled=False,
    )

    async def collect():
        return [event async for event in query_loop(params)]

    asyncio.run(collect())
    assert adapter.requests == [[]]
    assert "ComputerUse" not in str(adapter.systems)

    enabled_backend = FakeBackend(available=True)
    disabled_tool, disabled_context = prepared_tool(enabled_backend)
    disabled_context.session.computer_use_enabled = False
    assert not disabled_tool.is_available(disabled_context)


def test_computer_use_mode_defaults_to_background_and_rejects_unknown_values():
    assert CrabCodeSettings().computer_use.mode == "background_app"
    assert (
        CrabCodeSettings(computer_use={"mode": "foreground_desktop"}).computer_use.mode
        == "foreground_desktop"
    )
    try:
        CrabCodeSettings(computer_use={"mode": "automatic"})
    except ValueError:
        pass
    else:
        raise AssertionError("unknown Computer Use modes must be rejected")


def test_available_host_exposes_schema_and_returns_screenshot_attachment():
    backend = FakeBackend(available=True)
    tool, context = prepared_tool(backend)
    assert tool.is_available(context)
    result = asyncio.run(tool.call({"action": "observe"}, context))
    assert not result.is_error
    assert result.images == [{
        "media_type": "image/png",
        "data": base64.b64encode(b"png-data").decode(),
        "description": "Computer Use desktop observation",
    }]
    assert base64.b64encode(b"png-data").decode() not in result.result_for_model
    assert backend.calls[0][0] == "desktop-test"
    assert backend.calls[0][1]["mode"] == "foreground_desktop"


def test_background_mode_allows_focus_changes_but_keeps_window_coordinates():
    backend = FakeBackend(available=True)
    tool, context = prepared_tool(backend)
    context.session.computer_use_mode = "background_app"
    prompt = asyncio.run(tool.get_prompt())
    assert "allowed to become foreground" in prompt
    assert "background_delivery_unsupported" in prompt
    assert "window-local screenshot coordinates" in prompt
    assert "must not add origin_x/origin_y" in prompt
    assert "only auxiliary windows with proven AX ownership" in prompt
    assert "sibling documents are excluded" in prompt
    assert "never raise another document on a focus mismatch" in prompt
    assert "Never substitute a similar window ID and replay automatically" in prompt
    assert "stale hidden backing stores are ignored" in prompt
    assert "background_observation_limited" in prompt
    assert "auxiliary window is not clicked from stale pixels" in prompt
    assert "background_click_foreground_violation" not in prompt
    assert "fall back to window-targeted mouse events" in prompt
    assert "focus changes are allowed" in prompt
    assert "action_dispatched reports submission, not UI success" in prompt
    assert "Use the returned screenshot to judge the intended effect" in prompt
    assert "Uncertain dispatch" in prompt
    assert "A click succeeds when ok and effect_verified are true" not in prompt
    assert asyncio.run(tool.validate_input({"action": "observe"})) == (
        "window_id is required in background_app mode; call list_windows first"
    )
    assert "unavailable in background_app mode" in asyncio.run(
        tool.validate_input({"action": "list_displays"})
    )
    assert asyncio.run(
        tool.validate_input({"action": "observe", "window_id": "42"})
    ) is None
    assert asyncio.run(
        tool.validate_input({"action": "focus_window", "window_id": "42"})
    ) is None
    assert "window_id is required" in asyncio.run(
        tool.validate_input({"action": "focus_window", "text": "desktop"})
    )


@pytest.mark.parametrize("dispatched", [False, True, None])
def test_window_lifecycle_and_focus_diagnostics_survive_model_projection(dispatched):
    class WindowBackend(FakeBackend):
        async def execute(self, host_id, **kwargs):
            result = await super().execute(host_id, **kwargs)
            result.update({
                "ok": False,
                "error_code": "target_stale",
                "requested_window_id": "42",
                "resolved_window_id": "43",
                "focused_window_id": "43",
                "focused_element_role": "AXTextField",
                "focus_resolution": "owned_auxiliary",
                "focus_changed_by_tool": False,
                "action_dispatched": dispatched,
                "retry_safe": dispatched is False,
                "requires_observation": True,
                "window_lifecycle": {"target_resolvable": False, "focused_window_id": "44",
                                     "appeared_windows": [{"id": "44", "pid": 7}],
                                     "disappeared_windows": [{"id": "42", "pid": 7}]},
            })
            result["screenshot"]["window_components"] = [{
                "window_id": "43", "kind": "sheet", "owner_window_id": "42",
                "relationship_evidence": [{"owner_window_id": "42", "attributes": ["AXSheets"]}],
            }]
            result["screenshot"]["excluded_windows"] = [{
                "window_id": "45", "kind": "unknown", "excluded_reason": "ownership_unproven",
            }]
            return result

    backend = WindowBackend()
    tool, context = prepared_tool(backend)
    context.session.computer_use_mode = "background_app"
    result = asyncio.run(tool.call({"action": "type", "window_id": "42", "text": "name"}, context))
    projected = json.loads(result.result_for_model)
    assert result.is_error
    assert len(backend.calls) == 1
    assert projected["error_code"] == "target_stale"
    assert projected["action_dispatched"] is dispatched
    assert projected["retry_safe"] is (dispatched is False)
    assert projected["focus_changed_by_tool"] is False
    assert projected["resolved_window_id"] == "43"
    assert projected["window_lifecycle"]["focused_window_id"] == "44"
    assert projected["screenshot"]["window_components"][0]["relationship_evidence"][0]["attributes"] == ["AXSheets"]
    assert projected["screenshot"]["excluded_windows"][0]["excluded_reason"] == "ownership_unproven"
    assert "data" not in projected["screenshot"]
    assert len(result.images) == 1


@pytest.mark.parametrize("effect_verified", [True, False])
def test_dispatched_click_is_success_independent_of_effect_and_foreground(effect_verified):
    class ClickBackend(FakeBackend):
        async def execute(self, host_id, **kwargs):
            return {
                "ok": True,
                "summary": "点击已发送",
                "dispatch_succeeded": True,
                "effect_verified": effect_verified,
                "foreground_activated": True,
                **({"verification_warning": "Click effect is unverified; observe again"}
                   if not effect_verified else {}),
            }

    tool, context = prepared_tool(ClickBackend())
    context.session.computer_use_mode = "background_app"
    result = asyncio.run(tool.call(
        {"action": "click", "window_id": "42", "x": 100, "y": 100}, context,
    ))
    assert not result.is_error
    assert result.result_for_display == "点击已发送"
    projected = json.loads(result.result_for_model)
    assert projected["foreground_activated"] is True
    assert projected["dispatch_succeeded"] is True
    assert projected["effect_verified"] is effect_verified
    assert "error_code" not in projected
    if not effect_verified:
        assert "observe again" in projected["verification_warning"]


@pytest.mark.parametrize("mode", ["background_app", "foreground_desktop"])
@pytest.mark.parametrize("arguments", [
    {}, {"delta_y": 0}, {"delta_y": None}, {"delta_y": True},
    {"delta_y": "800"}, {"delta_y": 1.5}, {"delta_y": -10001},
    {"delta_y": 800, "x": 1400},
])
def test_scroll_rejects_invalid_input_in_both_modes(mode, arguments):
    tool, context = prepared_tool(FakeBackend())
    context.session.computer_use_mode = mode
    assert asyncio.run(tool.validate_input({"action": "scroll", "window_id": "14461", **arguments}))


def test_background_scroll_requires_a_point_but_foreground_can_use_current_pointer():
    tool, context = prepared_tool(FakeBackend())
    request = {"action": "scroll", "window_id": "14461", "delta_y": -800}
    assert asyncio.run(tool.validate_input(request)) is None
    context.session.computer_use_mode = "background_app"
    assert "x and y are required" in asyncio.run(tool.validate_input(request))
    assert asyncio.run(tool.validate_input({**request, "x": 1400, "y": 700})) is None


def test_unverified_scroll_receipt_survives_gateway_tool_projection():
    class ScrollBackend(FakeBackend):
        async def execute(self, host_id, **kwargs):
            self.calls.append((host_id, kwargs))
            return {
                "ok": True,
                "summary": "Scroll input sent to application window; movement unverified",
                "effect_verified": False,
                "scroll": {"unit": "pixels", "delta_x": 0, "delta_y": -800},
            }

    backend = ScrollBackend()
    tool, context = prepared_tool(backend)
    context.session.computer_use_mode = "background_app"
    request = {"action": "scroll", "window_id": "14461", "x": 1400, "y": 700, "delta_y": -800}
    result = asyncio.run(tool.call(request, context))
    assert not result.is_error
    assert json.loads(result.result_for_model)["effect_verified"] is False
    assert "unverified" in result.result_for_display
    assert backend.calls[0][1]["action"] == request


def test_failed_background_click_receipt_is_not_projected_as_success():
    class ClickBackend(FakeBackend):
        async def execute(self, host_id, **kwargs):
            self.calls.append((host_id, kwargs))
            return {
                "ok": False,
                "summary": "Background click is unsupported by the target application",
                "error_code": "background_click_unsupported",
                "dispatch_succeeded": False,
                "effect_verified": False,
                "visual_change_detected": False,
                "foreground_activated": False,
                "real_cursor_moved": False,
            }

    backend = ClickBackend()
    tool, context = prepared_tool(backend)
    context.session.computer_use_mode = "background_app"
    request = {"action": "click", "window_id": "14461", "x": 300, "y": 790}
    result = asyncio.run(tool.call(request, context))
    projected = json.loads(result.result_for_model)
    assert result.is_error
    assert projected["error_code"] == "background_click_unsupported"
    assert projected["dispatch_succeeded"] is False
    assert projected["effect_verified"] is False
    assert projected["foreground_activated"] is False
    assert projected["real_cursor_moved"] is False


class FakeSocket:
    def __init__(self):
        self.messages = []

    async def send_json(self, message):
        self.messages.append(message)


def test_gateway_broker_tracks_host_state_and_routes_result():
    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = FakeSocket()
        broker.register(
            "desktop-test",
            socket,
            enabled=True,
            gui_available=True,
            capabilities={
                "platform": "test",
                "supported_modes": ["background_app", "foreground_desktop"],
            },
        )
        assert broker.is_available("desktop-test")
        assert broker.is_available("desktop-test", "background_app")
        pending = asyncio.create_task(broker.execute(
            "desktop-test",
            session_id="session-test",
            agent_id="agent-test",
            action={"action": "observe"},
        ))
        await asyncio.sleep(0)
        request = socket.messages[0]
        assert request["session_id"] == "session-test"
        assert request["mode"] == "background_app"
        first_result = {
            "ok": True,
            "action": "observe",
            "screenshot": {"frame_id": "frame-before-reload", "data": "cG5n"},
        }
        assert broker.resolve("desktop-test", request["request_id"], first_result)
        assert await pending == first_result
        restored = broker.restorable_previews("desktop-test")
        assert len(restored) == 1
        assert restored[0]["session_id"] == "session-test"
        assert restored[0]["status"] == "ready"
        assert restored[0]["frame"]["frame_id"] == "frame-before-reload"

        broker.unregister("desktop-test", socket)
        reconnected_socket = FakeSocket()
        broker.register(
            "desktop-test",
            reconnected_socket,
            enabled=True,
            gui_available=True,
            capabilities={
                "platform": "test",
                "supported_modes": ["background_app", "foreground_desktop"],
            },
        )
        assert broker.restorable_previews("desktop-test") == restored
        assert await broker.release(
            "desktop-test",
            session_id="session-test",
            agent_id="agent-test",
        )
        release_message = reconnected_socket.messages[-1]
        release_deadline = release_message.pop("release_deadline_ms")
        assert release_deadline > 0
        assert release_message == {
            "type": "computer_use_release",
            "session_id": "session-test",
            "agent_id": "agent-test",
        }
        retained = broker.restorable_previews("desktop-test")
        assert retained[0]["release_deadline_ms"] == release_deadline
        assert not await broker.release(
            "desktop-test",
            session_id="session-test",
            agent_id="agent-test",
        )
        pending = asyncio.create_task(broker.execute(
            "desktop-test",
            session_id="session-test",
            agent_id=None,
            action={"action": "observe"},
        ))
        await asyncio.sleep(0)
        request = reconnected_socket.messages[-1]
        assert broker.resolve("desktop-test", request["request_id"], {"ok": True})
        assert await pending == {"ok": True}
        assert await broker.release(
            "desktop-test",
            session_id="session-test",
            all_agents=True,
        )
        release_message = reconnected_socket.messages[-1]
        assert release_message.pop("release_deadline_ms") > 0
        assert release_message == {
            "type": "computer_use_release",
            "session_id": "session-test",
            "all_agents": True,
        }
        assert broker.update_state(
            "desktop-test",
            reconnected_socket,
            enabled=False,
            gui_available=True,
        )
        assert not broker.is_available("desktop-test")

    asyncio.run(scenario())


def test_gateway_broker_retains_release_during_desktop_reload_gap():
    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = FakeSocket()
        broker.register(
            "desktop-reload",
            socket,
            enabled=True,
            gui_available=True,
            capabilities={"supported_modes": ["background_app"]},
        )
        pending = asyncio.create_task(broker.execute(
            "desktop-reload",
            session_id="session-reload",
            agent_id=None,
            action={"action": "observe"},
        ))
        await asyncio.sleep(0)
        request = socket.messages[-1]
        result = {
            "ok": True,
            "action": "observe",
            "screenshot": {"frame_id": "reload-frame", "data": "cG5n"},
        }
        assert broker.resolve("desktop-reload", request["request_id"], result)
        assert await pending == result

        broker.unregister("desktop-reload", socket)
        assert await broker.release(
            "desktop-reload",
            session_id="session-reload",
        )
        retained = broker.restorable_previews("desktop-reload")
        assert len(retained) == 1
        assert retained[0]["frame"]["frame_id"] == "reload-frame"
        assert retained[0]["release_deadline_ms"] > 0

        reconnected_socket = FakeSocket()
        broker.register(
            "desktop-reload",
            reconnected_socket,
            enabled=True,
            gui_available=True,
            capabilities={"supported_modes": ["background_app"]},
        )
        assert broker.restorable_previews("desktop-reload") == retained

    asyncio.run(scenario())


def test_core_session_releases_computer_use_at_the_real_turn_boundary():
    class ReleaseBackend:
        def __init__(self):
            self.calls = []

        async def release(self, host_id, **kwargs):
            self.calls.append((host_id, kwargs))
            return True

    async def scenario():
        backend = ReleaseBackend()
        session = CoreSession(tools=[])
        session.session_id = "session-test"
        session.computer_use_backend = backend
        session.computer_use_host_id = "desktop-test"

        async def initialize():
            return None

        async def send_message_impl(*_args, **_kwargs):
            yield TurnCompleteEvent()

        session.initialize = initialize
        session._send_message_impl = send_message_impl
        events = [event async for event in session.send_message("done")]
        assert isinstance(events[-1], TurnCompleteEvent)
        assert backend.calls == [(
            "desktop-test",
            {
                "session_id": "session-test",
                "agent_id": None,
                "all_agents": False,
            },
        )]

    asyncio.run(scenario())


def test_host_state_updates_every_bound_session():
    matching = SimpleNamespace(
        computer_use_host_id="desktop-test",
        computer_use_enabled=True,
    )
    other = SimpleNamespace(
        computer_use_host_id="desktop-other",
        computer_use_enabled=True,
    )
    state = SimpleNamespace(sessions={"matching": matching, "other": other})
    asyncio.run(_sync_bound_sessions(state, "desktop-test", False))
    assert matching.computer_use_enabled is False
    assert other.computer_use_enabled is True
