"""Mid-stream reconnect and completed-item checkpoint behavior."""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, patch

import httpx

from crabcode_core.api.base import StreamChunk
from crabcode_core.api.codex_adapter import CodexAdapter, _responses_error_chunk
from crabcode_core.query.loop import QueryParams, query_loop
from crabcode_core.query.retry import ResponsesStreamRetryState, request_retry_backoff
from crabcode_core.types.config import ApiConfig
from crabcode_core.types.event import ErrorEvent, StreamRetryEvent, StreamTextEvent
from crabcode_core.types.message import AssistantMessage, ToolResultBlock, create_user_message
from crabcode_core.types.tool import Tool, ToolContext, ToolResult
from crabcode_gateway.schemas import core_event_to_payload


class ScriptedAdapter:
    emits_response_item_events = True

    def __init__(self, responses, *, max_retries=2, unbounded=False):
        self.responses = responses
        self.requests = []
        self.config = ApiConfig(
            model="test",
            thinking_enabled=False,
            max_tokens=1000,
            max_retries=max_retries,
            unbounded_connection_retries=unbounded,
        )

    async def stream_message(self, messages, system, tools, config):
        self.requests.append([message.model_copy(deep=True) for message in messages])
        response = self.responses[min(len(self.requests) - 1, len(self.responses) - 1)]
        for item in response:
            if isinstance(item, BaseException):
                raise item
            yield item

    async def count_input_tokens(self, messages, system, tools, config):
        return None

    def try_switch_fallback_transport(self):
        return False


class CountingTool(Tool):
    name = "Count"
    description = "Count executions"
    input_schema = {"type": "object", "properties": {}}
    is_read_only = True

    def __init__(self):
        self.calls = 0

    async def call(self, tool_input, context):
        self.calls += 1
        return ToolResult(result_for_model=f"call {self.calls}")


def run(adapter, *, tools=None):
    messages = [create_user_message("hello")]
    params = QueryParams(
        messages=messages,
        system_prompt=[],
        user_context={},
        system_context={},
        tools=tools or [],
        tool_context=ToolContext(messages=messages),
        api_adapter=adapter,
        api_config=adapter.config,
        auto_compact_enabled=False,
    )

    async def collect():
        with patch("crabcode_core.query.loop.asyncio.sleep", new=AsyncMock()):
            return [event async for event in query_loop(params)]

    return asyncio.run(collect()), params.messages


def completed_text(text: str, item_id: str = "msg"):
    return [
        StreamChunk(type="text", text=text),
        StreamChunk(type="response_item_done", item_id=item_id, item_type="message"),
    ]


def test_incomplete_chunked_read_retries_after_partial_text():
    adapter = ScriptedAdapter([
        [
            StreamChunk(type="text", text="partial"),
            httpx.ReadError("peer closed connection without sending complete message body"),
        ],
        [*completed_text("recovered"), StreamChunk(type="message_stop")],
    ])

    events, messages = run(adapter)

    assert len(adapter.requests) == 2
    retry = next(event for event in events if isinstance(event, StreamRetryEvent))
    assert retry.message == "Reconnecting... 1/2"
    assert "peer closed connection" in retry.error
    assert retry.discarded_text_chars == len("partial")
    assert not any(isinstance(event, ErrorEvent) for event in events)
    assert [event.text for event in events if isinstance(event, StreamTextEvent)] == [
        "partial",
        "recovered",
    ]
    durable = [message.text_content for message in messages if isinstance(message, AssistantMessage)]
    assert durable == ["recovered"]


def test_remote_protocol_error_exhausts_exact_stream_retry_budget():
    adapter = ScriptedAdapter([
        [
            StreamChunk(type="text", text="partial"),
            httpx.RemoteProtocolError("incomplete chunked read"),
        ]
    ])

    events, messages = run(adapter)

    retries = [event for event in events if isinstance(event, StreamRetryEvent)]
    errors = [event for event in events if isinstance(event, ErrorEvent)]
    assert len(adapter.requests) == 3
    assert [event.retry_count for event in retries] == [1, 2]
    assert len(errors) == 1
    assert "RemoteProtocolError" in errors[0].message
    assert not any(isinstance(message, AssistantMessage) for message in messages)


def test_completed_response_item_is_checkpointed_before_reconnect():
    adapter = ScriptedAdapter([
        [
            *completed_text("checkpoint", "msg-1"),
            StreamChunk(type="text", text="unfinished"),
            httpx.ReadError("incomplete chunked read"),
        ],
        [*completed_text("continued", "msg-2"), StreamChunk(type="message_stop")],
    ])

    events, messages = run(adapter)

    assert len(adapter.requests) == 2
    retry_request = adapter.requests[1]
    assert [
        message.text_content
        for message in retry_request
        if isinstance(message, AssistantMessage)
    ] == ["checkpoint"]
    assert [
        message.text_content for message in messages if isinstance(message, AssistantMessage)
    ] == ["checkpoint", "continued"]
    assert len([event for event in events if isinstance(event, StreamRetryEvent)]) == 1


def test_connection_failure_uses_unbounded_budget_even_when_stream_budget_is_zero():
    adapter = ScriptedAdapter([
        [httpx.ConnectError("offline")],
        [*completed_text("online"), StreamChunk(type="message_stop")],
    ], max_retries=0, unbounded=True)

    events, messages = run(adapter)

    retry = next(event for event in events if isinstance(event, StreamRetryEvent))
    assert retry.message == "Reconnecting... waiting for network"
    assert retry.unbounded is True
    assert retry.delay_seconds == 5.0
    assert len(adapter.requests) == 2
    assert messages[-1].text_content == "online"


def test_completed_tool_call_runs_before_reconnect_and_is_not_replayed():
    tool = CountingTool()
    adapter = ScriptedAdapter([
        [
            StreamChunk(type="tool_use_start", tool_use_id="call-1", tool_name="Count"),
            StreamChunk(
                type="tool_use_end",
                tool_use_id="call-1",
                tool_name="Count",
                tool_input_json="{}",
            ),
            StreamChunk(
                type="response_item_done",
                item_id="fc-1",
                item_type="function_call",
            ),
            httpx.ReadError("incomplete chunked read"),
        ],
        [*completed_text("done", "msg-2"), StreamChunk(type="message_stop")],
    ])

    events, messages = run(adapter, tools=[tool])

    assert tool.calls == 1
    assert len(adapter.requests) == 2
    retry_request = adapter.requests[1]
    assert any(
        isinstance(block, ToolResultBlock) and block.tool_use_id == "call-1"
        for message in retry_request
        if isinstance(message.content, list)
        for block in message.content
    )
    assert len([event for event in events if isinstance(event, StreamRetryEvent)]) == 1
    assert messages[-1].text_content == "done"


def test_replayed_tool_call_id_reuses_result_without_side_effect():
    tool = CountingTool()
    tool_call = [
        StreamChunk(type="tool_use_start", tool_use_id="same-id", tool_name="Count"),
        StreamChunk(
            type="tool_use_end",
            tool_use_id="same-id",
            tool_name="Count",
            tool_input_json="{}",
        ),
        StreamChunk(type="response_item_done", item_id="fc", item_type="function_call"),
        StreamChunk(type="message_stop"),
    ]
    adapter = ScriptedAdapter([
        tool_call,
        tool_call,
        [*completed_text("done"), StreamChunk(type="message_stop")],
    ])

    _events, messages = run(adapter, tools=[tool])

    assert tool.calls == 1
    assert len(adapter.requests) == 3
    assert messages[-1].text_content == "done"


def test_retry_state_covers_backoff_unbounded_and_fallback():
    state = ResponsesStreamRetryState()
    with patch("crabcode_core.query.retry.random.uniform", return_value=1.0):
        first = state.schedule(error="drop", max_retries=2)
        second = state.schedule(error="drop", max_retries=2)
    assert first is not None and first.delay_seconds == 0.2
    assert second is not None and second.delay_seconds == 0.4

    fallback_calls = 0

    def fallback():
        nonlocal fallback_calls
        fallback_calls += 1
        return True

    switched = state.schedule(
        error="drop",
        max_retries=2,
        try_transport_fallback=fallback,
    )
    assert switched is not None and switched.transport_fallback
    assert fallback_calls == 1

    connection_state = ResponsesStreamRetryState()
    delays = [
        connection_state.schedule(
            error="offline",
            max_retries=0,
            connection_failed=True,
        ).delay_seconds
        for _ in range(6)
    ]
    assert delays == [5.0, 10.0, 20.0, 40.0, 60.0, 60.0]


def test_request_layer_retries_transport_and_5xx_but_not_429():
    async def exercise(statuses):
        calls = []

        async def handler(request):
            calls.append(request)
            status = statuses[min(len(calls) - 1, len(statuses) - 1)]
            if isinstance(status, BaseException):
                raise status
            return httpx.Response(status, request=request)

        adapter = object.__new__(CodexAdapter)
        adapter.config = ApiConfig(
            model="test",
            thinking_enabled=False,
            request_max_retries=4,
        )
        transport = httpx.MockTransport(handler)
        async with httpx.AsyncClient(transport=transport) as client:
            with patch(
                "crabcode_core.api.codex_adapter.asyncio.sleep",
                new=AsyncMock(),
            ):
                response = await adapter._send_httpx_stream_request(
                    client,
                    url="https://example.test/responses",
                    headers={},
                    params={"model": "test"},
                )
            status_code = response.status_code
            await response.aclose()
        return len(calls), status_code

    calls, status = asyncio.run(exercise([500, 502, 200]))
    assert (calls, status) == (3, 200)

    request = httpx.Request("POST", "https://example.test/responses")
    calls, status = asyncio.run(exercise([
        httpx.ConnectError("offline", request=request),
        200,
    ]))
    assert (calls, status) == (2, 200)

    calls, status = asyncio.run(exercise([429, 200]))
    assert (calls, status) == (1, 429)


def test_retry_delays_and_response_error_semantics():
    with patch("crabcode_core.query.retry.random.uniform", return_value=1.0):
        assert request_retry_backoff(1) == 0.2
        assert request_retry_backoff(2) == 0.4

    rate_limit = _responses_error_chunk(
        {
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Please try again in 750ms",
            }
        },
        "rate limited",
    )
    assert rate_limit.retryable is True
    assert rate_limit.retry_after == 0.75

    overloaded = _responses_error_chunk(
        {"error": {"code": "server_is_overloaded", "message": "busy"}},
        "busy",
    )
    assert overloaded.retryable is False


def test_stream_retry_wire_payload_is_non_terminal():
    payload = core_event_to_payload(StreamRetryEvent(
        message="Reconnecting... 1/5",
        error="drop",
        retry_count=1,
        max_retries=5,
        delay_seconds=0.2,
    ))
    assert payload.type == "stream_retry"
    assert payload.retry_count == 1
    assert payload.error == "drop"
