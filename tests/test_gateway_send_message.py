"""WebSocket send-message regression coverage."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

from crabcode_core.types.event import TurnCompleteEvent
from crabcode_gateway.routes.event import (
    _ACTIVE_SESSION_KEY,
    _WS_TASK_SESSIONS_KEY,
    _WS_TASKS_KEY,
    _handle_send_message,
)


class _EventBus:
    def __init__(self) -> None:
        self.events: list[object] = []

    async def publish(self, _session_id, event, **_kwargs) -> None:
        self.events.append(event)


class _Session:
    session_id = "session-1"

    def __init__(self) -> None:
        self.sent: list[tuple[str, dict[str, object]]] = []

    async def send_message(self, text: str, **kwargs):
        self.sent.append((text, kwargs))
        yield TurnCompleteEvent(reason="end_turn")


class _WebSocket:
    def __init__(self, state) -> None:
        self.app = SimpleNamespace(state=state)
        self.scope = {
            _ACTIVE_SESSION_KEY: "session-1",
            _WS_TASKS_KEY: set(),
            _WS_TASK_SESSIONS_KEY: {},
        }
        self.sent_json: list[dict] = []

    async def send_json(self, payload: dict) -> None:
        self.sent_json.append(payload)


def test_websocket_send_message_binds_existing_session_before_starting_query() -> None:
    async def scenario() -> None:
        session = _Session()
        event_bus = _EventBus()
        state = SimpleNamespace(
            event_bus=event_bus,
            sessions={session.session_id: session},
        )
        ws = _WebSocket(state)

        await _handle_send_message(ws, {"command": "send_message", "text": "hello"})

        tasks = list(state.background_tasks[session.session_id])
        await asyncio.gather(*tasks)

        assert session.sent == [("hello", {"max_turns": 0})]
        assert any(isinstance(event, TurnCompleteEvent) for event in event_bus.events)
        assert ws.sent_json == []

    asyncio.run(scenario())
