"""Server-calibrated context sizing must govern the actual query path."""

import asyncio
import json
from dataclasses import replace
from unittest.mock import AsyncMock, patch

import httpx
import pytest

from crabcode_core.api.anthropic_adapter import AnthropicAdapter, _anthropic_usage
from crabcode_core.api.base import ModelConfig, StreamChunk, normalize_openai_usage
from crabcode_core.api.codex_adapter import CodexAdapter
from crabcode_core.compact.compact import estimate_token_count
from crabcode_core.compact.context_tokens import ContextTokenTracker, RequestSnapshot
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.session.storage import SessionStorage
from crabcode_core.types.config import ApiConfig
from crabcode_core.types.event import CompactEvent, ErrorEvent, TurnCompleteEvent
from crabcode_core.types.message import ImageBlock, create_assistant_message, create_user_message
from crabcode_core.types.tool import Tool, ToolContext, ToolResult
from crabcode_gateway.schemas import core_event_to_payload


class ReadTool(Tool):
    name = "ReadExample"
    description = "Read an example"
    input_schema = {"type": "object", "properties": {}}
    is_read_only = True

    async def call(self, tool_input, context):
        return ToolResult(result_for_model="result " * 600)


class Adapter:
    def __init__(self, responses=None, counts=None):
        self.config = ApiConfig(model="example", thinking_enabled=False, max_tokens=1000, max_retries=0)
        self.responses = responses or [[StreamChunk(type="text", text="done")]]
        self.counts = list(counts or [])
        self.requests = []
        self.count_requests = []

    async def stream_message(self, messages, system, tools, config):
        self.requests.append(([m.model_copy(deep=True) for m in messages], config))
        for chunk in self.responses[min(len(self.requests) - 1, len(self.responses) - 1)]:
            yield chunk

    async def count_input_tokens(self, messages, system, tools, config):
        self.count_requests.append((list(messages), list(system), list(tools), config))
        value = self.counts.pop(0) if self.counts else None
        if isinstance(value, Exception):
            raise value
        return value


def response(tokens=None, output=20):
    return [StreamChunk(type="text", text="done"), StreamChunk(
        type="message_stop", usage={} if tokens is None else {"input_tokens": tokens, "output_tokens": output},
    )]


def run(adapter, messages=None, tracker=None, **options):
    params = QueryParams(
        messages=messages if messages is not None else [create_user_message("hello")],
        system_prompt=["system rules"], user_context={}, system_context={},
        tools=options.pop("tools", []), tool_context=ToolContext(), api_adapter=adapter,
        api_config=adapter.config, context_window=options.pop("context_window", 32_000),
        context_token_tracker=tracker, **options,
    )

    async def collect():
        return [event async for event in query_loop(params)]

    return asyncio.run(collect()), params.messages


def test_server_count_prevents_false_compaction_and_output_reduction():
    adapter = Adapter([response(8000)], counts=[8000])
    with patch("crabcode_core.query.loop.compact_conversation", new_callable=AsyncMock) as compact:
        events, _ = run(adapter, [create_user_message("中" * 40_000)])
    compact.assert_not_awaited()
    assert len(adapter.requests) == 1
    assert adapter.requests[0][1].max_tokens == 1000
    assert events[-1].context_used_tokens < 8100
    assert events[-1].context_token_source == "calibrated"


def test_latest_usage_replaces_anchor_across_turns_and_restore():
    adapter = Adapter([response(2000, output=9000), response(2300)], counts=[None])
    tracker = ContextTokenTracker()
    first, messages = run(adapter, tracker=tracker)
    assert first[-1].usage["output_tokens"] == 9000
    assert 2000 < first[-1].context_used_tokens < 2050
    restored = ContextTokenTracker()
    restored.restore(json.loads(json.dumps(tracker.dump())))
    messages.append(create_user_message("follow-up"))
    second, _ = run(adapter, messages, restored)
    # No extra round-trip away from the threshold after a valid usage response.
    assert len(adapter.count_requests) == 1
    assert second[-1].usage["input_tokens"] == 2300
    assert 2300 < second[-1].context_used_tokens < 2350


def test_tool_results_are_included_and_near_limit_is_verified():
    tool_response = [
        StreamChunk(type="tool_use_start", tool_name="ReadExample", tool_use_id="read-1"),
        StreamChunk(type="tool_use_end", tool_name="ReadExample", tool_use_id="read-1", tool_input_json="{}"),
        StreamChunk(type="message_start", usage={"input_tokens": 8500, "output_tokens": 0}),
        StreamChunk(type="message_delta", usage={"input_tokens": 8500, "output_tokens": 40}),
        StreamChunk(type="message_stop", usage={"input_tokens": 8500, "output_tokens": 40}),
    ]
    adapter = Adapter([tool_response, response(9000)], counts=[8500, 9000])
    events, _ = run(adapter, tools=[ReadTool()], compact_threshold=10_000)
    assert len(adapter.count_requests) == 2
    counted_messages, system, tools, _ = adapter.count_requests[-1]
    assert counted_messages[-1].content[0].content.startswith("result ")
    assert system == ["system rules"] and tools[0]["name"] == "ReadExample"
    done = events[-1]
    assert done.usage["input_tokens"] == 17_500
    assert done.usage["output_tokens"] == 60
    assert 9000 < done.context_used_tokens < 9100


def test_max_turns_display_includes_unsubmitted_tool_results():
    adapter = Adapter([[
        StreamChunk(type="tool_use_start", tool_name="ReadExample", tool_use_id="read"),
        StreamChunk(type="tool_use_end", tool_name="ReadExample", tool_use_id="read", tool_input_json="{}"),
        StreamChunk(type="message_stop", usage={"input_tokens": 1000, "output_tokens": 40}),
    ]])
    events, _ = run(adapter, tools=[ReadTool()], max_turns=1)
    assert events[-1].reason == "max_turns_reached"
    assert events[-1].context_used_tokens > 1800
    assert events[-1].context_token_source == "calibrated"


def test_true_server_overflow_compacts_and_recounts_changed_input():
    adapter = Adapter([response(1000)], counts=[31_000, 1000])
    messages = [create_user_message("old " * 4000), create_assistant_message("reply " * 1000),
                create_user_message("more " * 1000), create_assistant_message("answer " * 1000),
                create_user_message("continue")]
    with patch("crabcode_core.query.loop.compact_conversation", new_callable=AsyncMock,
               return_value=[create_user_message("summary")]) as compact:
        events, _ = run(adapter, messages)
    compact.assert_awaited_once()
    assert len(adapter.count_requests) == 2
    assert any(isinstance(event, CompactEvent) for event in events)
    assert not any(isinstance(event, ErrorEvent) for event in events)


@pytest.mark.parametrize("usage", [{}, {"output_tokens": 4000}, {"input_tokens": 0},
    {"input_tokens": -2}, {"input_tokens": True}, {"input_tokens": float("inf")},
    {"input_tokens": "unknown"}, {"input_tokens": 1.2}])
def test_missing_and_invalid_input_usage_stays_local(usage):
    adapter = Adapter([[StreamChunk(type="text", text="done"), StreamChunk(type="message_stop", usage=usage)]])
    events, messages = run(adapter)
    assert events[-1].context_token_source == "estimated"
    assert events[-1].context_used_tokens == estimate_token_count(messages, system=["system rules"], tools=[])


@pytest.mark.parametrize("failure", [None, RuntimeError("unsupported"), httpx.ConnectError("offline"), asyncio.TimeoutError()])
def test_count_failure_does_not_break_generation_or_usage_calibration(failure):
    adapter = Adapter([response(600)], counts=[failure])
    events, _ = run(adapter)
    assert not any(isinstance(event, ErrorEvent) for event in events)
    assert events[-1].context_token_source == "calibrated"
    assert 600 < events[-1].context_used_tokens < 650


@pytest.mark.parametrize("change", ["model", "provider", "edit", "prune"])
def test_incompatible_baseline_is_invalidated(change):
    adapter = Adapter()
    config = ModelConfig(model="example", thinking_enabled=False)
    messages = [create_user_message("old"), create_assistant_message("reply")]
    system, tools = ["rules"], []
    tracker = ContextTokenTracker()
    tracker.calibrate(RequestSnapshot.capture(adapter, config, messages, system, tools), 1000)
    if change == "model":
        config = replace(config, model="other")
    elif change == "provider":
        adapter.config.base_url = "https://other.invalid/v1"
    elif change == "edit":
        messages[0].content = "edited"
    elif change == "prune":
        messages.pop(0)
    snapshot = RequestSnapshot.capture(adapter, config, messages, system, tools)
    measured = tracker.measure(snapshot, messages, system, tools)
    assert measured.source == "estimated"
    assert tracker.dump() is None


def test_cached_tokens_normalized_without_double_counting():
    openai = normalize_openai_usage({"input_tokens": 1200, "input_tokens_details": {"cached_tokens": 1000}})
    claude = _anthropic_usage({"input_tokens": 100, "cache_read_input_tokens": 1000,
                              "cache_creation_input_tokens": 100}, include_input=True, include_output=False)
    for usage in [openai, claude]:
        tracker = ContextTokenTracker()
        snapshot = RequestSnapshot("a" * 64, ())
        assert tracker.observe_usage(snapshot, usage)
        assert tracker.input_tokens == 1200


@pytest.mark.parametrize("kind", ["responses", "anthropic"])
def test_count_endpoint_uses_actual_multimodal_request_and_tools(kind):
    adapter = object.__new__(CodexAdapter if kind == "responses" else AnthropicAdapter)
    adapter.config = ApiConfig(model="example", base_url="https://provider.invalid/v1", http_headers={"x-route": "test"})
    adapter._api_key = "test-key"
    adapter._base_url = adapter.config.base_url
    adapter._using_codex_oauth = False
    adapter._codex_oauth_account_id = None
    config = ModelConfig(model="example", thinking_enabled=False)
    messages = [create_user_message([ImageBlock(source={"type": "base64", "media_type": "image/png", "data": "aGVsbG8="})])]
    tools = [ReadTool().to_api_schema()]
    requests = []
    client = httpx.AsyncClient

    def handle(request):
        requests.append(request)
        return httpx.Response(200, json={"input_tokens": 321})

    with patch("crabcode_core.api.base.httpx.AsyncClient", side_effect=lambda **kw: client(transport=httpx.MockTransport(handle), **kw)):
        count = asyncio.run(adapter.count_input_tokens(messages, ["system"], tools, config))
    assert count == 321
    payload = json.loads(requests[0].content)
    generation = adapter._request_params(messages, ["system"], tools, config)
    for key in (["input", "instructions", "tools"] if kind == "responses" else ["messages", "system", "tools"]):
        assert payload[key] == generation[key]
    assert "stream" not in payload and "max_tokens" not in payload and "max_output_tokens" not in payload
    assert requests[0].headers["x-route"] == "test"
    assert requests[0].url.path == ("/v1/responses/input_tokens" if kind == "responses" else "/v1/messages/count_tokens")


@pytest.mark.parametrize("status", [404, 405, 501, 401, 429, 500])
def test_failed_count_endpoint_is_not_repeated(status):
    adapter = object.__new__(CodexAdapter)
    calls = []
    client = httpx.AsyncClient
    def handle(request):
        calls.append(request)
        return httpx.Response(status, json={"error": "unsupported"})
    async def probe():
        for _ in range(2):
            assert await adapter._count_tokens_http("https://provider.invalid/count", {}, {}) is None
    with patch("crabcode_core.api.base.httpx.AsyncClient", side_effect=lambda **kw: client(transport=httpx.MockTransport(handle), **kw)):
        asyncio.run(probe())
    assert len(calls) == 1


def test_oauth_does_not_send_credentials_to_public_count_endpoint():
    adapter = object.__new__(CodexAdapter)
    adapter._using_codex_oauth = True
    adapter._count_tokens_http = AsyncMock()
    assert asyncio.run(adapter.count_input_tokens([], [], [], ModelConfig())) is None
    adapter._count_tokens_http.assert_not_awaited()


def test_persisted_baseline_and_source_restore_with_context_usage(tmp_path):
    tracker = ContextTokenTracker()
    tracker.calibrate(RequestSnapshot("a" * 64, ("b" * 64,)), 1000)
    storage = SessionStorage(str(tmp_path), "test-context")
    storage._transcript_path = str(tmp_path / "transcript.jsonl")
    storage._append_transcript_line = lambda entry: (tmp_path / "transcript.jsonl").write_text(json.dumps(entry) + "\n")
    storage.record_context_usage(1050, 32_000, source="calibrated", baseline=tracker.dump())
    raw = (tmp_path / "transcript.jsonl").read_text()
    restored = SessionStorage(str(tmp_path), "test-context")
    restored.load_messages(_transcript_text=raw)
    assert restored.last_context_used_tokens == 1050
    assert restored.last_context_token_source == "calibrated"
    recovered = ContextTokenTracker()
    recovered.restore(restored.last_context_token_baseline)
    assert recovered.dump() == tracker.dump()
    recovered.restore({"version": 1, "identity": [], "messages": None})
    assert recovered.dump() is None


def test_gateway_preserves_estimation_flag():
    for source in ["server", "calibrated", "estimated"]:
        payload = core_event_to_payload(TurnCompleteEvent(context_token_source=source, context_used_tokens=123))
        assert payload.context_token_source == source


def test_dynamic_git_context_and_tool_changes_keep_calibrated_history():
    adapter = Adapter()
    config = ModelConfig(model="example")
    messages = [create_user_message("history " * 40_000)]
    tracker = ContextTokenTracker()
    original = RequestSnapshot.capture(adapter, config, messages, ["clean"], [])
    tracker.calibrate(original, 2000)
    system, tools = ["modified: example.py"], [ReadTool().to_api_schema()]
    updated = RequestSnapshot.capture(adapter, config, messages, system, tools)
    measurement = tracker.measure(updated, messages, system, tools)
    assert measurement.source == "calibrated"
    assert 2000 < measurement.tokens < 2200
    assert tracker.input_tokens == 2000


def test_cancelling_token_count_does_not_send_a_model_request():
    adapter = Adapter()
    async def exercise():
        started = asyncio.Event()
        async def count(*args):
            started.set()
            await asyncio.Event().wait()
        adapter.count_input_tokens = count
        params = QueryParams(messages=[create_user_message("hello")], system_prompt=[], user_context={},
                             system_context={}, tools=[], tool_context=ToolContext(), api_adapter=adapter,
                             api_config=adapter.config, context_window=32_000)
        async def collect():
            return [event async for event in query_loop(params)]
        task = asyncio.create_task(collect())
        await started.wait()
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    asyncio.run(exercise())
    assert not adapter.requests


def test_absent_later_usage_preserves_previous_calibration():
    adapter = Adapter([response(2400), response()], counts=[None])
    tracker = ContextTokenTracker()
    _, messages = run(adapter, tracker=tracker)
    messages.append(create_user_message("continue"))
    events, _ = run(adapter, messages, tracker)
    assert events[-1].context_token_source == "calibrated"
    assert 2400 < events[-1].context_used_tokens < 2500
    assert len(adapter.count_requests) == 1
