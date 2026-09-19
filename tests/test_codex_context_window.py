"""Tests for Codex OAuth context-window resolution."""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace

from starlette.requests import Request

from crabcode_core.api.codex_adapter import CodexAdapter
from crabcode_core.api.model_info import DEFAULT_CONTEXT_WINDOW
from crabcode_core.types.config import ApiConfig, CrabCodeSettings
from crabcode_gateway.routes.session import session_status


def _adapter(config: ApiConfig, *, using_oauth: bool) -> CodexAdapter:
    adapter = object.__new__(CodexAdapter)
    adapter.config = config
    adapter._using_codex_oauth = using_oauth
    return adapter


def test_oauth_uses_effective_window_from_codex_models_cache(tmp_path) -> None:
    auth_path = tmp_path / "codex" / "auth.json"
    auth_path.parent.mkdir()
    (auth_path.parent / "models_cache.json").write_text(
        json.dumps(
            {
                "models": [
                    {
                        "slug": "gpt-5.6-sol",
                        "context_window": 272_000,
                        "effective_context_window_percent": 95,
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    adapter = _adapter(
        ApiConfig(
            model="gpt-5.6-sol",
            codex_auth_path=str(auth_path),
        ),
        using_oauth=True,
    )

    assert asyncio.run(adapter.resolve_context_window()) == 258_400


def test_oauth_missing_model_metadata_uses_safe_default(tmp_path) -> None:
    adapter = _adapter(
        ApiConfig(
            model="gpt-5.6-sol",
            codex_auth_path=str(tmp_path / "auth.json"),
        ),
        using_oauth=True,
    )

    assert asyncio.run(adapter.resolve_context_window()) == DEFAULT_CONTEXT_WINDOW


def test_oauth_explicit_override_has_highest_priority(tmp_path) -> None:
    adapter = _adapter(
        ApiConfig(
            model="gpt-5.6-sol",
            context_window=123_456,
            codex_auth_path=str(tmp_path / "auth.json"),
        ),
        using_oauth=True,
    )

    assert asyncio.run(adapter.resolve_context_window()) == 123_456


def test_non_oauth_keeps_public_api_model_window() -> None:
    adapter = _adapter(ApiConfig(model="gpt-5.6-sol"), using_oauth=False)

    assert asyncio.run(adapter.resolve_context_window()) == 1_050_000


def test_gateway_status_uses_codex_oauth_effective_window(tmp_path) -> None:
    auth_path = tmp_path / "codex" / "auth.json"
    auth_path.parent.mkdir()
    (auth_path.parent / "models_cache.json").write_text(
        json.dumps(
            {
                "models": [
                    {
                        "slug": "gpt-5.6-sol",
                        "context_window": 272_000,
                        "effective_context_window_percent": 95,
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    config = ApiConfig(
        provider="codex",
        model="gpt-5.6-sol",
        codex_auth_path=str(auth_path),
    )
    settings = CrabCodeSettings(default_model="gpt", models={"gpt": config})
    session = SimpleNamespace(
        session_id="session-id",
        cwd=str(tmp_path),
        settings=settings,
        messages=[],
        tools=[],
        _initialized=True,
        _current_model_name="gpt",
        _api_adapter=_adapter(config, using_oauth=True),
    )
    state = SimpleNamespace(
        sessions={session.session_id: session},
        default_session_id=session.session_id,
    )
    request = Request({"type": "http", "app": SimpleNamespace(state=state)})

    status = asyncio.run(session_status(request, session.session_id))

    assert status.context_window_tokens == 258_400
    assert status.context_remaining_tokens == 258_400
