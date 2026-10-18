"""Keep provider diagnostics across SDK/HTTP streams and the VS Code log API."""

import asyncio
import json
import logging
from types import SimpleNamespace

import httpx
import openai
import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from crabcode_core.api.anthropic_adapter import AnthropicAdapter
from crabcode_core.api.base import ModelConfig, StreamChunk
from crabcode_core.api.codex_adapter import CodexAdapter
from crabcode_core.api.error_logging import log_api_error
from crabcode_core.api.openai_adapter import OpenAIAdapter
from crabcode_core.logging_utils import _AsyncFileHandler
from crabcode_core.types.config import ApiConfig
from crabcode_core.types.message import create_user_message
from crabcode_core.usage import tracked_stream_message


@pytest.fixture(autouse=True)
def isolated_home(tmp_path, monkeypatch):
    monkeypatch.setenv("CRABCODE_HOME", str(tmp_path / "home"))


def diagnostics(caplog):
    return [json.loads(record.getMessage().split("API error details: ", 1)[1])
            for record in caplog.records if "API error details: " in record.getMessage()]


async def collect(adapter):
    return [chunk async for chunk in tracked_stream_message(
        adapter, messages=[create_user_message("private request content")],
        system=["private system prompt"], tools=[], config=ModelConfig(model="test-model"),
        session_id="session-error-test",
    )]


@pytest.mark.parametrize("status", [200, 500])
def test_real_openai_sdk_keeps_full_error_body(status, caplog):
    body = {"error": {"message": "upstream sent an error event",
                      "code": "upstream_error", "request_id": "upstream-request-1",
                      "details": {"cause": "渠道不可用", "trace": "x" * 20000}}}

    def handle(request):
        if status == 200:
            return httpx.Response(200, headers={"content-type": "text/event-stream"},
                                  text="data: " + json.dumps(body) + "\n\n")
        return httpx.Response(status, headers={"x-request-id": "http-request-1"}, json=body)

    async def exercise():
        adapter = object.__new__(OpenAIAdapter)
        adapter.config = ApiConfig(provider="openai", model="test-model")
        adapter.client = openai.AsyncOpenAI(
            api_key="test-credential", base_url="https://provider.invalid/v1", max_retries=0,
            http_client=httpx.AsyncClient(transport=httpx.MockTransport(handle)),
        )
        try:
            with pytest.raises(openai.APIError, match="upstream sent an error event"):
                await collect(adapter)
        finally:
            await adapter.client.close()

    asyncio.run(exercise())
    [entry] = diagnostics(caplog)
    assert entry["session_id"] == "session-error-test"
    assert entry["model"] == "test-model"
    assert entry["attempt_id"]
    error = entry["errors"][0]
    assert error["body"] == (body["error"] if status == 200 else body)
    if status == 500:
        assert error["status_code"] == 500
        assert error["request_id"] == "http-request-1"
    assert "private request content" not in caplog.text
    assert "private system prompt" not in caplog.text
    assert "test-credential" not in caplog.text


@pytest.mark.parametrize("provider", ["anthropic", "codex"])
@pytest.mark.parametrize("status", [200, 502])
def test_direct_http_stream_preserves_error_payload(provider, status, monkeypatch, caplog):
    body = {"type": "error", "request_id": "event-request-1", "error": {
        "message": "upstream failure", "code": "server_error", "extra": [1, 2, 3],
    }}
    client_class = httpx.AsyncClient

    def handle(request):
        if status == 200:
            return httpx.Response(200, headers={"content-type": "text/event-stream",
                                               "request-id": "http-request-2"},
                                  text="event: error\ndata: " + json.dumps(body) + "\n\n")
        return httpx.Response(status, headers={"request-id": "http-request-2"}, json=body)

    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: client_class(
        transport=httpx.MockTransport(handle), **kwargs,
    ))
    config = ApiConfig(provider=provider, model="test-model", base_url="https://provider.invalid",
                       thinking_enabled=False, request_max_retries=0)
    if provider == "anthropic":
        adapter = object.__new__(AnthropicAdapter)
        adapter.config = config
        adapter._api_key = "test-credential"
    else:
        adapter = object.__new__(CodexAdapter)
        adapter.config = config
        adapter._using_codex_oauth = True
        adapter._base_url = "https://provider.invalid"
        adapter._raw_responses_headers = lambda: {"Authorization": "Bearer test-credential"}
    chunks = asyncio.run(collect(adapter))
    assert chunks[-1].type == "error"
    [entry] = diagnostics(caplog)
    error = entry["errors"][0]
    assert error["body"] == body
    assert error["status_code"] == status
    assert error["request_id"] == "http-request-2"


def test_credentials_are_redacted_without_losing_diagnostics(monkeypatch, caplog):
    monkeypatch.setenv("DIAGNOSTIC_TEST_KEY", "private-key-value")
    adapter = SimpleNamespace(config=ApiConfig(
        api_key_env="DIAGNOSTIC_TEST_KEY", http_headers={"X-Api-Key": "header-key-value"},
    ))
    body = {"error": {"message": "private-key-value header-key-value Bearer another-secret",
                      "metadata": {"api_key": "different-secret", "request_id": "kept-id"},
                      "nested": [{"Authorization": "secret", "code": "kept-code"}]}}
    log_api_error(adapter, session_id="sid", model="model", attempt_id="attempt", purpose="test",
                  chunk=StreamChunk(type="error", error="failed", error_body=json.dumps(body)))
    [entry] = diagnostics(caplog)
    error = entry["errors"][0]["body"]["error"]
    assert error["metadata"] == {"api_key": "[REDACTED]", "request_id": "kept-id"}
    assert error["nested"][0] == {"Authorization": "[REDACTED]", "code": "kept-code"}
    for secret in ("private-key-value", "header-key-value", "another-secret", "different-secret"):
        assert secret not in caplog.text


def test_wrapped_non_json_http_error_keeps_body_and_request_id(caplog):
    response = httpx.Response(502, text="<html>proxy unavailable</html>" + "x" * 20000,
                              headers={"x-request-id": "proxy-request"},
                              request=httpx.Request("POST", "https://provider.invalid"))
    error = RuntimeError("wrapped")
    error.__cause__ = httpx.HTTPStatusError("bad gateway", request=response.request, response=response)
    log_api_error(SimpleNamespace(), session_id="sid", model="model", attempt_id="attempt",
                  purpose="test", exception=error)
    [entry] = diagnostics(caplog)
    nested = entry["errors"][1]
    assert nested["body"] == response.text
    assert nested["request_id"] == "proxy-request"
    assert nested["status_code"] == 502


def test_error_diagnostics_failure_does_not_hide_original_exception():
    class BrokenBody:
        def model_dump(self, **kwargs):
            raise ValueError("cannot serialize")

    error = openai.APIError("original failure", request=httpx.Request("POST", "https://p.invalid"),
                            body=BrokenBody())

    class Adapter:
        config = ApiConfig()

        async def stream_message(self, **kwargs):
            raise error
            yield

    with pytest.raises(openai.APIError) as caught:
        asyncio.run(collect(Adapter()))
    assert caught.value is error


def test_vscode_log_endpoint_returns_persisted_full_error(tmp_path, monkeypatch):
    # VS Code fetchLogs and Desktop use this same Gateway endpoint.
    from crabcode_core import logging_utils
    from crabcode_gateway.routes.config import router

    root = logging.getLogger(logging_utils.LOG_NAMESPACE)
    monkeypatch.setattr(root, "handlers", [])
    monkeypatch.setattr(root, "level", logging.WARNING)
    monkeypatch.setattr(root, "propagate", False)
    monkeypatch.setattr(logging_utils, "_configured_signature", None)
    logging_utils.configure_logging(str(tmp_path))
    body = {"message": "upstream sent an error event", "details": "complete-body-" + "x" * 20000}

    class Adapter:
        config = ApiConfig(provider="openai")

        async def stream_message(self, **kwargs):
            yield StreamChunk(type="error", error=body["message"], error_body=body)

    try:
        asyncio.run(collect(Adapter()))
    finally:
        for handler in root.handlers:
            assert isinstance(handler, _AsyncFileHandler)
            handler.close()

    app = FastAPI()
    app.include_router(router)
    app.state.sessions = {"session-error-test": SimpleNamespace(cwd=str(tmp_path))}
    with TestClient(app) as client:
        response = client.get("/logs", params={"session_id": "session-error-test", "name": "crabcode"})
    assert response.status_code == 200
    [line] = [line for line in response.json()["lines"] if "API error details: " in line]
    entry = json.loads(line.split("API error details: ", 1)[1])
    assert entry["errors"][0]["body"] == body
    assert entry["session_id"] == "session-error-test"
