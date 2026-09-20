import asyncio
import base64
from types import SimpleNamespace

from crabcode_core.api.base import StreamChunk
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.tools.computer_use import ComputerUseTool
from crabcode_core.types.config import ApiConfig, CrabCodeSettings, ToolLoadingSettings
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


def test_background_mode_requires_window_and_never_accepts_desktop_actions():
    backend = FakeBackend(available=True)
    tool, context = prepared_tool(backend)
    context.session.computer_use_mode = "background_app"
    assert asyncio.run(tool.validate_input({"action": "observe"})) == (
        "window_id is required in background_app mode; call list_windows first"
    )
    assert "unavailable in background_app mode" in asyncio.run(
        tool.validate_input({"action": "list_displays"})
    )
    assert asyncio.run(
        tool.validate_input({"action": "observe", "window_id": "42"})
    ) is None


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
        assert broker.resolve("desktop-test", request["request_id"], {"ok": True})
        assert await pending == {"ok": True}
        assert broker.update_state(
            "desktop-test",
            socket,
            enabled=False,
            gui_available=True,
        )
        assert not broker.is_available("desktop-test")

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
