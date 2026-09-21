from __future__ import annotations

from pydantic import ValidationError

from crabcode_gateway.routes.session import _apply_new_session_controls
from crabcode_gateway.schemas import NewSessionRequest


class _Session:
    def __init__(self) -> None:
        self.calls: list[tuple[str, object]] = []

    def set_reasoning_effort(self, value: str) -> bool:
        self.calls.append(("reasoning", value))
        return True

    def set_ultra_mode(self, value: bool) -> bool:
        self.calls.append(("ultra", value))
        return value

    def set_client_permission_mode(self, value: str) -> bool:
        self.calls.append(("permission", value))
        return True

    def switch_mode(self, value: str) -> bool:
        self.calls.append(("mode", value))
        return True


def test_new_session_request_applies_inherited_composer_controls() -> None:
    request = NewSessionRequest(
        reasoning_effort="xhigh",
        ultra_mode=True,
        mode="plan",
        permission_mode="ai_review",
    )
    session = _Session()

    _apply_new_session_controls(session, request)

    assert session.calls == [
        ("reasoning", "xhigh"),
        ("ultra", True),
        ("permission", "ai_review"),
        ("mode", "plan"),
    ]


def test_new_session_request_rejects_unknown_permission_mode() -> None:
    try:
        NewSessionRequest(permission_mode="unsafe")
    except ValidationError:
        pass
    else:
        raise AssertionError("invalid permission mode was accepted")
