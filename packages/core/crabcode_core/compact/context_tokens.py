"""Request-scoped token accounting, independent of cumulative billing usage."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from typing import Any, Literal

from crabcode_core.api.base import ModelConfig, usage_int_field
from crabcode_core.compact.compact import estimate_token_count
from crabcode_core.types.message import Message

TokenSource = Literal["server", "calibrated", "estimated"]


def _digest(value: Any) -> str:
    encoded = json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode()).hexdigest()


@dataclass(frozen=True)
class TokenMeasurement:
    tokens: int
    source: TokenSource


@dataclass(frozen=True)
class RequestSnapshot:
    identity: str
    messages: tuple[str, ...]
    overhead: str = ""
    overhead_tokens: int = 0

    @classmethod
    def capture(
        cls, adapter: Any, config: ModelConfig, messages: list[Message],
        system: list[str], tools: list[dict[str, Any]],
    ) -> RequestSnapshot:
        adapter_config = getattr(adapter, "config", None)
        # Persist only digests, never prompts, images, endpoint credentials or headers.
        settings = adapter_config.model_dump(mode="json") if hasattr(adapter_config, "model_dump") else {}
        identity = _digest({
            "adapter": f"{type(adapter).__module__}.{type(adapter).__qualname__}",
            "endpoint": getattr(adapter, "_base_url", None),
            "settings": settings,
            "model": config.model,
            "thinking": [config.thinking_enabled, config.thinking_budget, config.reasoning_effort],
        })
        # UUIDs, billing usage and timestamps aren't model input. In particular,
        # the synthetic user-context message gets a fresh UUID on every request.
        fingerprints = tuple(_digest(message.model_dump(
            mode="json", include={"role", "content"},
        )) for message in messages)
        return cls(
            identity, fingerprints, _digest([system, tools]),
            estimate_token_count([], system=system, tools=tools),
        )


class ContextTokenTracker:
    """Anchor an unchanged input prefix to its latest server-reported count.

    Only appended, retained content is estimated. Output usage is deliberately
    excluded: reasoning tokens and other generated items may not be replayed.
    Prompt/schema changes adjust only their estimated difference. Editing/pruning
    history or changing the provider/model configuration invalidates the anchor.
    """

    def __init__(self) -> None:
        self.baseline: RequestSnapshot | None = None
        self.input_tokens = 0

    def reset(self) -> None:
        self.baseline = None
        self.input_tokens = 0

    def calibrate(self, snapshot: RequestSnapshot, tokens: Any) -> bool:
        count, valid = usage_int_field({"input_tokens": tokens}, "input_tokens")
        # Zero input for a nonempty request is commonly a proxy placeholder.
        if not valid or count <= 0:
            return False
        self.baseline = snapshot
        self.input_tokens = count
        return True

    def observe_usage(self, snapshot: RequestSnapshot, usage: dict[str, Any]) -> bool:
        key = "total_input_tokens" if "total_input_tokens" in usage else "input_tokens"
        return self.calibrate(snapshot, usage.get(key))

    def measure(
        self, snapshot: RequestSnapshot, messages: list[Message],
        system: list[str], tools: list[dict[str, Any]],
    ) -> TokenMeasurement:
        baseline = self.baseline
        if baseline is not None:
            prefix_size = len(baseline.messages)
            if (snapshot.identity == baseline.identity
                    and snapshot.messages[:prefix_size] == baseline.messages):
                delta = messages[prefix_size:]
                overhead_changed = snapshot.overhead != baseline.overhead
                overhead_delta = snapshot.overhead_tokens - baseline.overhead_tokens if overhead_changed else 0
                tokens = self.input_tokens + overhead_delta + (estimate_token_count(delta) if delta else 0)
                if tokens <= 0:
                    # A large removal cannot safely be subtracted from an
                    # unrelated provider count using heuristic ratios.
                    self.reset()
                    return TokenMeasurement(estimate_token_count(messages, system=system, tools=tools), "estimated")
                return TokenMeasurement(
                    tokens, "calibrated" if delta or overhead_changed else "server",
                )
            self.reset()
        return TokenMeasurement(estimate_token_count(messages, system=system, tools=tools), "estimated")

    def dump(self) -> dict[str, Any] | None:
        if self.baseline is None:
            return None
        return {
            "version": 1, "identity": self.baseline.identity,
            "messages": list(self.baseline.messages), "input_tokens": self.input_tokens,
            "overhead": self.baseline.overhead, "overhead_tokens": self.baseline.overhead_tokens,
        }

    def restore(self, value: Any) -> None:
        self.reset()
        if not isinstance(value, dict) or value.get("version") != 1:
            return
        identity, messages = value.get("identity"), value.get("messages")

        def is_digest(item: Any) -> bool:
            return isinstance(item, str) and len(item) == 64 and all(c in "0123456789abcdef" for c in item)

        if not is_digest(identity) or not isinstance(messages, list) or not all(map(is_digest, messages)):
            return
        overhead = value.get("overhead", "")
        overhead_tokens, valid = usage_int_field(value, "overhead_tokens")
        if overhead != "" and (not is_digest(overhead) or not valid):
            return
        self.calibrate(RequestSnapshot(identity, tuple(messages), overhead, overhead_tokens), value.get("input_tokens"))
