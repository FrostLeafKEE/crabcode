"""Model probes use isolated adapters and never expose provider secrets."""
import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from crabcode_core.api import StreamChunk
from crabcode_core.types.config import ApiConfig, CrabCodeSettings
from crabcode_gateway.routes import config as routes


def test_probe_closes_stream_and_client_and_disables_retries(monkeypatch):
    closed = []
    client = SimpleNamespace(close=AsyncMock())
    async def stream_message(**kwargs):
        assert kwargs["tools"] == []
        assert kwargs["messages"][0].content == "Reply with OK."
        try:
            yield StreamChunk(type="text", text="OK")
            raise AssertionError("Probe should stop after receiving text")
        finally:
            closed.append(True)
    config = ApiConfig(provider="openai", model="test")
    def create_adapter(cfg):
        assert cfg.request_max_retries == 0
        assert not cfg.unbounded_connection_retries
        return SimpleNamespace(client=client, stream_message=stream_message)
    monkeypatch.setattr("crabcode_core.api.create_adapter", create_adapter)
    asyncio.run(routes._probe_model(config))
    assert closed == [True]
    client.close.assert_awaited_once()


@pytest.mark.parametrize("kind", ["empty", "error", "timeout"])
def test_failed_probe_cleans_up(monkeypatch, kind):
    client = SimpleNamespace(close=AsyncMock())
    async def stream_message(**kwargs):
        if kind == "error":
            yield StreamChunk(type="error", error="provider failure")
        elif kind == "timeout":
            await asyncio.sleep(10)
    monkeypatch.setattr("crabcode_core.api.create_adapter", lambda cfg: SimpleNamespace(client=client, stream_message=stream_message))
    async def run():
        await asyncio.wait_for(routes._probe_model(ApiConfig(model="test")), timeout=0.02)
    with pytest.raises((RuntimeError, TimeoutError)):
        asyncio.run(run())
    client.close.assert_awaited_once()


@pytest.mark.parametrize("status", [400, 401, 403, 404, 429, 500])
def test_provider_error_redacts_body(status):
    error = RuntimeError("Authorization: Bearer secret-value; https://secret-value@example.com")
    error.status_code = status
    message = routes._model_test_error(error)
    assert str(status) in message
    assert "secret-value" not in message


def test_endpoint_uses_project_config_without_switching_session(monkeypatch):
    settings = CrabCodeSettings(models={"test": ApiConfig(provider="openai", model="test-id")})
    monkeypatch.setattr(routes, "_resolve_model_settings_cwd", lambda request, cwd: cwd)
    def manager(cwd):
        assert cwd == "D:/project"
        return SimpleNamespace(load=lambda: settings)
    monkeypatch.setattr(routes, "ConfigManager", manager)
    probe = AsyncMock()
    monkeypatch.setattr(routes, "_probe_model", probe)
    app = FastAPI()
    app.include_router(routes.router)
    with TestClient(app) as client:
        response = client.post("/config/test-model", json={"name": "test", "cwd": "D:/project"})
        assert response.json()["ok"] is True
        probe.assert_awaited_once()
        assert probe.call_args.args[0].model == "test-id"
        probe.side_effect = RuntimeError("Missing credentials api_key secret-value")
        response = client.post("/config/test-model", json={"name": "test", "cwd": "D:/project"})
        assert response.json()["ok"] is False
        assert "缺少认证" in response.json()["message"]
        assert "secret-value" not in response.text
        assert client.post("/config/test-model", json={"name": "unknown", "cwd": "D:/project"}).status_code == 404
