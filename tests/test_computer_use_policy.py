import asyncio
from pathlib import Path
from types import SimpleNamespace

import pytest

from crabcode_core.config.manager import _merge_settings
from crabcode_core.events import CoreSession
from crabcode_core.tools.computer_use import ComputerUseTool
from crabcode_core.types.config import ComputerUseSettings
from crabcode_core.types.tool import ToolContext
from crabcode_gateway.computer_use import ComputerUseBroker
from crabcode_gateway.routes.session import _bind_computer_use
from crabcode_gateway.schemas import NewSessionRequest, ResumeSessionRequest, SendMessageRequest


@pytest.mark.parametrize("mode,scope", [("background_app", "app_window"), ("foreground_desktop", "desktop")])
def test_legacy_target_never_grants_foreground_permission(mode, scope):
    settings = ComputerUseSettings(mode=mode)
    assert settings.target_scope == scope
    assert settings.delivery_policy == "strict_background"
    assert "mode" not in settings.model_dump()
    merged = _merge_settings(
        {"computer_use": {"target_scope": "app_window"}},
        {"computer_use": {"mode": mode}},
    )
    assert ComputerUseSettings(**merged["computer_use"]).target_scope == scope


def test_session_binding_preserves_permission_until_explicitly_changed():
    session = CoreSession(tools=[])
    assert session.computer_use_delivery_policy == "strict_background"
    _bind_computer_use(session, None, None, mode="foreground_desktop")
    assert session.computer_use_delivery_policy == "strict_background"
    _bind_computer_use(session, None, None, target_scope="app_window", delivery_policy="allow_foreground")
    assert session.computer_use_mode == "background_app"
    _bind_computer_use(session, None, None, enabled=True)
    assert session.computer_use_delivery_policy == "allow_foreground"
    assert session._computer_use_delivery_policy_override == "allow_foreground"
    _bind_computer_use(session, None, None, delivery_policy="strict_background")
    assert session.computer_use_delivery_policy == "strict_background"


@pytest.mark.parametrize("schema,kwargs", [(NewSessionRequest, {}), (ResumeSessionRequest, {"session_id": "s"}), (SendMessageRequest, {"text": "test"})])
def test_all_session_transports_validate_policy(schema, kwargs):
    with pytest.raises(ValueError):
        schema(**kwargs, computer_use_delivery_policy="auto")
    req = schema(**kwargs, computer_use_target_scope="app_window", computer_use_delivery_policy="strict_background")
    assert req.computer_use_delivery_policy == "strict_background"


def test_action_cannot_override_session_policy_and_permission_cache_is_scoped():
    class Backend:
        calls = []

        def is_available(self, *_args):
            return True

        async def execute(self, *_args, **kwargs):
            self.calls.append(kwargs)
            return {"ok": False, "error_code": "background_delivery_unsupported", "action_dispatched": False, "retry_safe": True}

    async def scenario():
        backend = Backend()
        session = SimpleNamespace(computer_use_backend=backend, computer_use_host_id="h", computer_use_enabled=True)
        context = ToolContext(session=session, session_id="s")
        tool = ComputerUseTool()
        await tool.setup(context)
        action = {"action": "click", "window_id": "1", "x": 1, "y": 1}
        prompt = await tool.get_prompt()
        assert "delivery_policy=strict_background" in prompt
        assert "Use focus_window when" not in prompt
        assert "allowed to become foreground" not in prompt
        assert "delivery_policy" not in tool.input_schema["properties"]
        for key in ["delivery_policy", "target_scope", "mode"]:
            result = await tool.call({**action, key: "allow_foreground"}, context)
            assert result.is_error
        assert backend.calls == []
        strict_key = tool.get_permission_key(action)
        result = await tool.call(action, context)
        assert result.data["action_dispatched"] is False
        assert backend.calls[-1]["delivery_policy"] == "strict_background"
        session.computer_use_delivery_policy = "allow_foreground"
        assert tool.get_permission_key(action) != strict_key
        await tool.call(action, context)
        assert backend.calls[-1]["delivery_policy"] == "allow_foreground"

    asyncio.run(scenario())


def test_broker_never_sends_input_to_old_hosts_and_preserves_policy_on_reconnect():
    class Socket:
        def __init__(self):
            self.messages = []

        async def send_json(self, payload):
            self.messages.append(payload)

    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = Socket()
        caps = {"supported_modes": ["background_app", "foreground_desktop"]}
        broker.register("h", socket, enabled=True, gui_available=True, capabilities=caps)
        for policy in ["strict_background", "allow_foreground"]:
            result = await broker.execute("h", session_id="s", agent_id=None,
                                          action={"action": "click"}, delivery_policy=policy)
            assert result["action_dispatched"] is False
            assert socket.messages == []
        result = await broker.execute("h", session_id="s", agent_id=None,
                                      action={"action": "observe"}, mode="foreground_desktop")
        assert result["action_dispatched"] is False
        assert socket.messages == []
        caps["delivery_policy_version"] = 1
        broker.register("h", socket, enabled=True, gui_available=True, capabilities=caps)
        pending = asyncio.create_task(broker.execute(
            "h", session_id="s", agent_id=None, action={"action": "click"},
            target_scope="app_window", delivery_policy="allow_foreground",
        ))
        await asyncio.sleep(0)
        message = socket.messages[-1]
        assert message["target_scope"] == "app_window"
        assert message["delivery_policy"] == "allow_foreground"
        broker.resolve("h", message["request_id"], {"ok": True, "action": "click", "action_dispatched": True})
        await pending
        broker.unregister("h", socket)
        restored = broker.restorable_previews("h")
        assert restored[0]["delivery_policy"] == "allow_foreground"

    asyncio.run(scenario())


def test_runtime_settings_mutation_is_atomic_and_uses_the_effective_layers(tmp_path, monkeypatch):
    import json
    from fastapi import HTTPException
    from crabcode_core.config.manager import ConfigManager
    from crabcode_gateway.routes import config
    from crabcode_gateway.schemas import RuntimeSettingsMutationRequest

    paths = {name: str(tmp_path / f"{name}.json") for name in ("userSettings", "projectSettings", "localSettings", "flagSettings", "policySettings")}
    monkeypatch.setattr(ConfigManager, "settings_file_paths", property(lambda _self: paths))
    monkeypatch.setattr(config, "_resolve_model_settings_cwd", lambda *_args: str(tmp_path))
    monkeypatch.setattr(config, "_settings_mutation_path", lambda _cwd, source: Path(paths[source]))
    project = tmp_path / "projectSettings.json"
    project.write_text(json.dumps({"computer_use": {"mode": "background_app"}, "unrelated": True}))
    initial = project.read_bytes()

    def mutate(**kwargs):
        return config._mutate_runtime_settings(None, RuntimeSettingsMutationRequest(
            action="set_computer_use_options", source="projectSettings", **kwargs,
        ))

    with pytest.raises(HTTPException) as exc:
        mutate(computer_use_target_scope="desktop")
    assert exc.value.status_code == 422
    assert project.read_bytes() == initial
    response = mutate(computer_use_delivery_policy="allow_foreground")
    assert response.computer_use_target_scope == "app_window"
    assert response.computer_use_delivery_policy == "allow_foreground"
    assert json.loads(project.read_text())["unrelated"] is True
    response = mutate(computer_use_target_scope="desktop")
    assert response.computer_use_target_scope == "desktop"
    response = mutate(computer_use_delivery_policy="strict_background", computer_use_target_scope="app_window")
    assert response.computer_use_delivery_policy == "strict_background"


@pytest.mark.parametrize("lock_name", ["action_lock", "send_lock"])
def test_broker_rechecks_host_policy_capability_after_waiting(lock_name):
    class Socket:
        messages = []

        async def send_json(self, payload):
            self.messages.append(payload)

    async def scenario():
        broker = ComputerUseBroker(timeout_seconds=1)
        socket = Socket()
        host = broker.register("h", socket, enabled=True, gui_available=True, capabilities={
            "supported_modes": ["background_app"], "delivery_policy_version": 1,
        })
        lock = getattr(host, lock_name)
        async with lock:
            pending = asyncio.create_task(broker.execute(
                "h", session_id="s", agent_id=None, action={"action": "click"},
            ))
            await asyncio.sleep(0)
            assert not pending.done()
            broker.update_state("h", socket, enabled=True, gui_available=True, capabilities={
                "supported_modes": ["background_app"],
            })
        result = await pending
        assert result["action_dispatched"] is False
        assert socket.messages == []
        assert broker.restorable_previews("h") == []

    asyncio.run(scenario())


def test_transport_failure_keeps_input_dispatch_uncertain_and_does_not_retry():
    class Backend:
        calls = 0

        def is_available(self, *_args):
            return True

        async def execute(self, *_args, **_kwargs):
            self.calls += 1
            raise RuntimeError("host disconnected")

    async def scenario():
        backend = Backend()
        session = SimpleNamespace(
            computer_use_backend=backend, computer_use_host_id="h", computer_use_enabled=True,
            computer_use_delivery_policy="allow_foreground",
        )
        context = ToolContext(session=session, session_id="s")
        tool = ComputerUseTool()
        await tool.setup(context)
        result = await tool.call({"action": "click", "window_id": "1", "x": 1, "y": 1}, context)
        assert result.is_error
        assert result.data["action_dispatched"] is None
        assert result.data["retry_safe"] is False
        assert result.data["delivery_policy"] == "allow_foreground"
        assert backend.calls == 1

    asyncio.run(scenario())
