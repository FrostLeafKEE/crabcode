"""Provider error diagnostics shared by CLI and Gateway-backed clients."""

from __future__ import annotations

import json
import os
import re
from typing import Any

from crabcode_core.logging_utils import get_logger
from crabcode_core.utf8_sanitize import safe_utf8_str

logger = get_logger(__name__)

_CREDENTIAL_KEYS = {
    "authorization", "proxyauthorization", "apikey", "xapikey", "token",
    "accesstoken", "refreshtoken", "idtoken", "password", "secret",
    "clientsecret", "cookie", "setcookie",
}


def _credential_key(key: str) -> bool:
    return re.sub(r"[^a-z0-9]", "", key.lower()) in _CREDENTIAL_KEYS


def response_request_id(response: Any) -> str | None:
    headers = getattr(response, "headers", {})
    return next((headers.get(key) for key in (
        "x-request-id", "request-id", "x-amzn-requestid",
    ) if headers.get(key)), None)


def _exception_details(exc: BaseException) -> list[dict[str, Any]]:
    details = []
    pending = [exc]
    seen: set[int] = set()
    while pending:
        current = pending.pop()
        if id(current) in seen:
            continue
        seen.add(id(current))
        response = getattr(current, "response", None)
        body = getattr(current, "body", None)
        # HTTP errors may contain an envelope beyond SDK exception.body.
        # Only inspect an already-read response; never consume a live stream.
        if response is not None:
            try:
                body = response.text or body
            except Exception:
                pass
        details.append({
            "exception": type(current).__name__,
            # SDK messages often repeat body as a Python repr, which would
            # bypass structured credential redaction. The body retains it.
            "message": str(current) if body is None else None,
            "status_code": getattr(current, "status_code", None)
            or getattr(response, "status_code", None),
            "request_id": getattr(current, "request_id", None)
            or response_request_id(response),
            "body": body,
        })
        pending.extend(item for item in (current.__context__, current.__cause__)
                       if item is not None)
    return details


def _redact(value: Any, secrets: set[str]) -> Any:
    if hasattr(value, "model_dump"):
        value = value.model_dump(mode="json")
    if isinstance(value, dict):
        return {str(key): "[REDACTED]" if _credential_key(str(key))
                else _redact(item, secrets) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_redact(item, secrets) for item in value]
    if isinstance(value, bytes):
        value = value.decode("utf-8", "replace")
    if isinstance(value, str):
        # Raw HTTP bodies can be JSON too. Preserve every diagnostic field.
        try:
            parsed = json.loads(value)
        except (ValueError, TypeError):
            parsed = None
        if isinstance(parsed, (dict, list)):
            return _redact(parsed, secrets)
        for secret in sorted(secrets, key=len, reverse=True):
            value = value.replace(secret, "[REDACTED]")
        return re.sub(r"(?i)\bBearer\s+[^\s\"'<>]+", "Bearer [REDACTED]", value)
    return value


def log_api_error(
    adapter: Any, *, session_id: str | None, model: str, attempt_id: str,
    purpose: str, exception: BaseException | None = None, chunk: Any = None,
) -> None:
    """Log the full error body without dumping requests or credential headers.

    Diagnostics must never replace the original error or change retry behavior.
    Bodies are not truncated; credentials are the only redacted fields.
    """
    try:
        config = getattr(adapter, "config", None)
        secrets = set()
        for name in (getattr(config, "api_key_env", None),
                     "OPENAI_API_KEY", "ANTHROPIC_API_KEY"):
            if name and os.environ.get(name):
                secrets.add(os.environ[name])
        for value in (getattr(adapter, "_api_key", None),
                      getattr(getattr(adapter, "client", None), "api_key", None)):
            if isinstance(value, str) and value:
                secrets.add(value)
        for key, value in (getattr(config, "http_headers", None) or {}).items():
            if _credential_key(key) and value:
                secrets.add(value)
                if value.lower().startswith("bearer "):
                    secrets.add(value[7:])
        details = {
            "session_id": session_id, "model": model,
            "provider": getattr(config, "provider", None) or type(adapter).__name__,
            "attempt_id": attempt_id, "purpose": purpose,
            "errors": _exception_details(exception) if exception is not None else [{
                "message": chunk.error if chunk.error_body is None else None,
                "status_code": chunk.error_status_code,
                "request_id": chunk.error_request_id,
                "body": chunk.error_body,
            }],
        }
        logger.warning("API error details: %s", safe_utf8_str(json.dumps(
            _redact(details, secrets), ensure_ascii=False, default=str,
        )))
    except Exception:
        logger.warning("Unable to serialize API error details: session=%s attempt=%s",
                       session_id, attempt_id)
