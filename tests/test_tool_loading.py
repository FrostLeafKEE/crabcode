"""Discovery is a schema optimization, never an authority boundary bypass."""

import asyncio
import json
from dataclasses import replace

import pytest

from crabcode_core.api.base import StreamChunk
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.tools.loading import ToolCatalog, ToolLoadingState
from crabcode_core.tools.tool_search import ToolSearchTool
from crabcode_core.types.config import ApiConfig, ToolLoadingSettings
from crabcode_core.types.event import ToolResultEvent, TurnCompleteEvent
from crabcode_core.types.message import create_user_message
from crabcode_core.types.tool import Tool, ToolContext, ToolResult, PermissionResult, PermissionBehavior


class Example(Tool):
    input_schema = {"type": "object", "properties": {}}
    description = "Read documentation example"
    is_read_only = True

    def __init__(self, name="Deferred", read_only=True):
        self.name = name
        self.is_read_only = read_only
        self.calls = 0

    async def call(self, tool_input, context):
        self.calls += 1
        return ToolResult(result_for_model="ok")


def catalog(*tools, state=None, mode="discovery", plan=False, pinned=()):
    return ToolCatalog([ToolSearchTool(), *tools], ToolContext(), ToolLoadingSettings(mode=mode),
                       state or ToolLoadingState(), plan_mode=plan, pinned=pinned)


def test_eager_pinned_append_order_and_state_isolation():
    a, b = Example("A"), Example("B")
    state = ToolLoadingState()
    first = catalog(a, b, state=state, pinned=("B",))
    assert [t.name for t in first.loaded] == ["ToolSearch", "B"]
    first.load(["A", "A", "missing"])
    assert [t.name for t in first.loaded] == ["ToolSearch", "B", "A"]
    resumed = catalog(a, b, state=ToolLoadingState.restore(state.names))
    assert [t.name for t in resumed.loaded] == state.names
    assert [t.name for t in catalog(a, b).loaded] == ["ToolSearch"]
    assert len(catalog(a, b, mode="eager").loaded) == 3


def test_plan_disabled_removed_and_model_changes_filter_restored_state():
    write, read, disabled = Example("WriteExtra", False), Example(), Example("Disabled")
    disabled.is_enabled = False
    state = ToolLoadingState(["WriteExtra", "Deferred", "Disabled", "Removed"])
    c = catalog(write, read, disabled, state=state, plan=True)
    assert {t.name for t in c.loaded} == {"Deferred", "ToolSearch"}
    assert c.load(["WriteExtra", "Disabled", "Removed"]) == []
    assert "WriteExtra" not in c.directory()
    read.is_available = lambda context: context.model == "supported"
    c = catalog(read, state=state)
    assert "Deferred" not in c.tools
    c = ToolCatalog([read], ToolContext(model="supported"), ToolLoadingSettings(), state)
    assert [t.name for t in c.loaded] == ["Deferred"]
    assert c.mode == "eager" and c.directory() == ""


@pytest.mark.parametrize("query,name", [
    ("浏览", "Browser"), ("生成", "ImageGenerate"), ("子代理", "Agent"),
    ("会话", "SendMessage"), ("团队", "TeamCreate"), ("后台", "Monitor"),
    ("定时", "ScheduleCreate"), ("记住", "Memory"), ("回滚", "Revert"), ("目标", "get_goal"),
    ("电脑", "ComputerUse"),
])
def test_group_alias_discovery(query, name):
    c = catalog(Example(name))
    result = json.loads(c.search(names=[], group="", query=query, list_only=False, offset=0, limit=5))
    assert name in result["loaded"]


def test_pagination_exact_names_and_mcp_instructions_only_when_loaded():
    examples = [Example(f"mcp__srv__t{i}") for i in range(40)]
    for tool in examples:
        tool._server_name = "srv"
        tool.server_instructions = "Use server context."
    c = catalog(*examples)
    assert c.instructions() == ""
    assert "(+28 more)" in c.directory()
    response = json.loads(c.search(names=[], group="mcp:srv", query="", list_only=True, offset=30, limit=10))
    assert len(response["tools"]) == 10 and response["next_offset"] is None
    assert not response["loaded"]
    assert c.load(["mcp__srv__t39"]) == ["mcp__srv__t39"]
    assert c.instructions().count("Use server context.") == 1
    assert "Unknown" in c.search(names=["missing"], group="", query="", list_only=False, offset=0, limit=5)


def test_discovery_directory_requires_real_search_before_tool_use():
    directory = catalog(Example("ComputerUse")).directory()
    assert "computer: ComputerUse" in directory
    assert "names below are NOT callable yet" in directory
    assert "first make a real ToolSearch call" in directory
    assert "Never guess its arguments or print a textual/pseudo tool call" in directory
    assert "callable in the NEXT response only" in directory


def call(name, ident, inputs=None):
    return [StreamChunk(type="tool_use_start", tool_name=name, tool_use_id=ident),
            StreamChunk(type="tool_use_end", tool_name=name, tool_use_id=ident, tool_input_json=json.dumps(inputs or {}))]


class Adapter:
    def __init__(self, responses):
        self.responses = responses
        self.requests = []
        self.systems = []
        self.config = ApiConfig(model="test", thinking_enabled=False, max_tokens=1000, max_retries=0)

    async def count_input_tokens(self, *args):
        return None

    async def stream_message(self, messages, system, tools, config):
        self.requests.append([t["name"] for t in tools])
        self.systems.append(list(system))
        for chunk in self.responses[len(self.requests) - 1]:
            yield chunk
        yield StreamChunk(type="message_stop")


def run(adapter, tool, **options):
    params = QueryParams(
        messages=[create_user_message("test")], system_prompt=[], user_context={}, system_context={},
        tools=[ToolSearchTool(), tool], tool_context=ToolContext(), api_adapter=adapter,
        api_config=adapter.config, tool_loading=ToolLoadingSettings(), auto_compact_enabled=False,
        **options,
    )
    async def collect():
        return [event async for event in query_loop(params)]
    return asyncio.run(collect()), params


def test_loading_takes_effect_on_next_request_not_same_batch():
    tool = Example()
    adapter = Adapter([
        [*call("ToolSearch", "search", {"names": [tool.name]}), *call(tool.name, "too-early")],
        call(tool.name, "allowed"), [StreamChunk(type="text", text="done")],
    ])
    events, params = run(adapter, tool)
    assert adapter.requests[0] == ["ToolSearch"]
    assert adapter.requests[1] == ["ToolSearch", tool.name]
    assert tool.calls == 1
    assert any(isinstance(e, ToolResultEvent) and e.tool_use_id == "too-early" and e.is_error for e in events)
    last = next(e for e in reversed(events) if isinstance(e, TurnCompleteEvent))
    assert last.prompt_budget["loaded_tools"] == 2
    assert last.prompt_budget["source"] == "estimated"
    from crabcode_gateway.schemas import core_event_to_payload
    payload = core_event_to_payload(last)
    assert payload.prompt_budget.loaded_names == ["ToolSearch", tool.name]


def test_undiscovered_call_is_rejected_and_permissions_still_apply():
    tool = Example()
    adapter = Adapter([call(tool.name, "bad"), [StreamChunk(type="text", text="done")]])
    events, _ = run(adapter, tool)
    assert tool.calls == 0
    assert any(isinstance(e, ToolResultEvent) and e.is_error for e in events)
    async def deny(*args):
        return PermissionResult(behavior=PermissionBehavior.DENY, reason="denied for test")
    tool.check_permissions = deny
    adapter = Adapter([call("ToolSearch", "s", {"names": [tool.name]}), call(tool.name, "denied"),
                       [StreamChunk(type="text", text="done")]])
    from crabcode_core.permissions.manager import PermissionManager
    events, _ = run(adapter, tool, permission_manager=PermissionManager(), permission_queue=asyncio.Queue())
    assert tool.calls == 0
    assert any(isinstance(e, ToolResultEvent) and "denied" in str(e.result).lower() for e in events)


def test_skill_hash_dedup_and_reload_after_compaction_or_change():
    from crabcode_core.skills.context import auto_skill_messages
    from crabcode_core.skills.loader import SkillDefinition
    skill = SkillDefinition("test", "summary", "instructions", "/tmp/test")
    messages = auto_skill_messages([skill, skill], [])
    assert len(messages) == 1
    assert auto_skill_messages([skill], messages) == []
    assert len(auto_skill_messages([replace(skill, content="new version")], messages)) == 1
    assert len(auto_skill_messages([skill], [create_user_message("compacted summary")])) == 1
    shortened = messages[0].model_copy(update={"content": "[truncated]"})
    assert len(auto_skill_messages([skill], [shortened])) == 1


def test_session_storage_restore_fork_and_fresh_session(tmp_path, monkeypatch):
    from crabcode_core.events import CoreSession
    from crabcode_core.session.storage import SessionStorage
    from crabcode_core.types.message import create_assistant_message
    monkeypatch.setattr("crabcode_core.session.storage.get_config_home", lambda: tmp_path / "config")
    monkeypatch.setattr("crabcode_core.session.meta_db.get_config_home", lambda: tmp_path / "config")
    session = CoreSession(cwd=str(tmp_path))
    old_id = session.new_session()
    storage = session._session_storage
    storage.write_meta()
    storage.update_loaded_tools(["ToolSearch", "Memory"])
    reply = create_assistant_message("done")
    storage.append_message(reply)
    restored = SessionStorage(str(tmp_path), old_id)
    restored.load_messages()
    assert restored.meta["loaded_tools"] == ["ToolSearch", "Memory"]
    fork = SessionStorage.fork_from(str(tmp_path), old_id, reply.uuid)
    assert fork.meta["loaded_tools"] == ["ToolSearch", "Memory"]
    latest_reply = create_assistant_message("latest")
    storage.append_message(latest_reply)
    latest_fork = SessionStorage.fork_from(str(tmp_path), old_id)
    assert latest_fork.meta["forked_from_message_uuid"] == latest_reply.uuid
    assert latest_fork.load_messages()[-1]["uuid"] == latest_reply.uuid
    assert asyncio.run(session.resume(old_id))
    assert session._tool_loading_state.names == ["ToolSearch", "Memory"]
    session._tool_loading_state.names = ["Memory"]
    session.new_session()
    assert session._tool_loading_state.names == []


def test_core_session_sends_discovery_schema_and_restores_it(tmp_path, monkeypatch):
    from crabcode_core.events import CoreSession
    from crabcode_core.types.config import CrabCodeSettings
    monkeypatch.setattr("crabcode_core.session.storage.get_config_home", lambda: tmp_path / "config")
    monkeypatch.setattr("crabcode_core.session.meta_db.get_config_home", lambda: tmp_path / "config")
    monkeypatch.setattr("crabcode_core.prompts.context.get_system_context", lambda cwd: {"gitStatus": "clean"})
    tool = Example()
    adapter = Adapter([
        call("ToolSearch", "search", {"names": [tool.name]}),
        call(tool.name, "execute"), [StreamChunk(type="text", text="done")],
        [StreamChunk(type="text", text="second")],
    ])
    session = CoreSession(cwd=str(tmp_path), settings=CrabCodeSettings(api=adapter.config),
                          tools=[ToolSearchTool(), tool])
    session._initialized = True
    session._api_adapter = adapter
    monkeypatch.setattr(session, "_maybe_generate_title", lambda: None)

    async def scenario():
        events = [e async for e in session.send_message("first")]
        session_id = session.session_id
        assert tool.calls == 1
        assert session.last_prompt_budget["loaded_tools"] == 2
        assert session._session_storage.meta["loaded_tools"] == ["ToolSearch", tool.name]
        session.new_session()
        assert await session.resume(session_id)
        assert session._tool_loading_state.names == ["ToolSearch", tool.name]
        events.extend([e async for e in session.send_message("second")])
        await session.close()
        return events

    asyncio.run(scenario())
    assert adapter.requests[0] == ["ToolSearch"]
    assert adapter.requests[-1] == ["ToolSearch", tool.name]
    assert "Is a git repository: Yes" in "\n".join(adapter.systems[0])


def test_subagent_discovery_is_isolated_and_snapshot_restores_names(tmp_path):
    from crabcode_core.agent_manager import AgentManager
    from crabcode_core.types.config import CrabCodeSettings, AgentTypeConfig
    tool = Example()
    adapters = []
    snapshots = []
    settings = CrabCodeSettings(api=ApiConfig(model="test", thinking_enabled=False))

    def adapter_provider(_):
        adapter = Adapter([call("ToolSearch", "s", {"names": [tool.name]}),
                           call(tool.name, "d"), [StreamChunk(type="text", text="done")]])
        adapters.append(adapter)
        return adapter

    async def sink(_):
        pass

    manager = AgentManager(
        settings=settings, agent_settings=settings.agent,
        tools_provider=lambda: [ToolSearchTool(), tool], adapter_provider=adapter_provider,
        event_sink=sink, permission_manager=None, prompt_profile=None, cwd=str(tmp_path),
        env={}, session_id="test-session", persistence_callback=lambda value: snapshots.append(value),
    )
    restricted = manager._resolve_tools("generalPurpose", AgentTypeConfig(allowed_tools=["ToolSearch"]))
    restricted_catalog = ToolCatalog(restricted, ToolContext(), settings.tool_loading, ToolLoadingState())
    assert restricted_catalog.load([tool.name]) == []

    async def scenario():
        for _ in range(2):
            ident = await manager.spawn_agent(prompt="test")
            snapshot = await manager.wait_agent(ident, timeout_ms=3000)
            assert snapshot and snapshot.status == "completed", snapshot
            assert snapshot.loaded_tools == ["ToolSearch", tool.name]
        persisted = snapshots[-1]
        manager.restore_snapshots(persisted)
        assert all(item.loaded_tools == ["ToolSearch", tool.name] for item in manager.list_agents())
        await manager.close()

    asyncio.run(scenario())
    assert tool.calls == 2
    assert [adapter.requests[0] for adapter in adapters] == [["ToolSearch"], ["ToolSearch"]]


@pytest.mark.parametrize("arguments", [{"limit": 0}, {"limit": 31}, {"offset": -1}, {"names": "Browser"}, {"query": []}, {"list": "false"}])
def test_discovery_rejects_invalid_pagination(arguments):
    assert asyncio.run(ToolSearchTool().validate_input(arguments)) is not None


def test_disabling_tool_while_awaiting_pre_hook_prevents_execution():
    from types import SimpleNamespace
    tool = Example()
    async def hook(event, data, **kwargs):
        if event == "pre_tool_call" and data["tool_name"] == tool.name:
            tool.is_enabled = False
        return SimpleNamespace(blocked=False, feedback=[], details=[])
    adapter = Adapter([call("ToolSearch", "s", {"names": [tool.name]}), call(tool.name, "d"),
                       [StreamChunk(type="text", text="done")]])
    events, _ = run(adapter, tool, hook_manager=SimpleNamespace(run=hook))
    assert tool.calls == 0
    assert any(isinstance(event, ToolResultEvent) and event.tool_use_id == "d" and event.is_error for event in events)
