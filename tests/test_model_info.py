"""Tests for model context-window resolution."""

from __future__ import annotations

import pytest

from crabcode_core.api.model_info import KNOWN_CONTEXT_WINDOWS, lookup_context_window
from crabcode_core.prompts.templates import CLAUDE_MODEL_IDS


@pytest.mark.parametrize(
    ("model", "expected"),
    [
        ("claude-fable-5-1", 1_000_000),
        ("claude-mythos-5-1", 1_000_000),
        ("gpt-6-astra", 1_050_000),
        ("gpt-daybreak-blue-latest", 1_050_000),
        ("gpt-daybreak-red-latest", 400_000),
        ("deepseek-flash", 1_000_000),
        ("glm-5.3", 1_000_000),
        ("glm-5.3-flash", 1_000_000),
        ("qwen3.8-flash", 1_000_000),
        ("MiniMax-M3", 1_000_000),
        ("skywork-ai/skyclaw-v1", 1_000_000),
        ("skywork-ai/skyclaw-v1-lite", 1_000_000),
        ("muse-spark-1.3", 1_000_000),
        ("muse-spark-1.3-contributor", 1_000_000),
        ("meta-llama/Llama-4-Scout-17B-16E-Instruct", 10_000_000),
        ("meta-llama/Llama-4-Maverick-17B-128E-Instruct", 1_000_000),
        ("mistral-medium-3-5", 256_000),
        ("mistral-small-2603", 256_000),
        ("mistral-large-2512", 256_000),
        ("open-mixtral-8x22b", 64_000),
        ("open-mixtral-8x7b", 32_000),
        ("mistralai/Mixtral-8x22B-Instruct-v0.1", 64_000),
        ("mistralai/Mixtral-8x7B-Instruct-v0.1", 32_000),
        ("gemini-3.8-flash", 1_048_576),
        ("gemini-3.8-live", 131_072),
        ("gemini-3.8-live-extended-thinking", 131_072),
        ("gemini-3.7-flash", 1_048_576),
    ],
)
def test_exact_model_context_window(model: str, expected: int) -> None:
    """Return official limits for exact model IDs."""
    assert lookup_context_window(model) == expected


def test_versioned_model_uses_base_context_window() -> None:
    """Resolve a dated snapshot from its base model."""
    assert lookup_context_window("gpt-4o-2024-11-20") == 128_000


def test_colon_tag_uses_base_context_window() -> None:
    """Resolve a tagged model from its base model."""
    assert lookup_context_window("mistral:latest") == 32_000


def test_longest_model_name_wins_for_version_suffix() -> None:
    """Prefer a specific variant over a shorter base model."""
    assert lookup_context_window("gpt-5.6-cyber-2026-09-01") == 400_000
    assert lookup_context_window("skywork-ai/skyclaw-v1-lite-2026-05-19") == 1_000_000


@pytest.mark.parametrize("model", [None, "", "unknown-model", "gpt-4oextra"])
def test_unknown_or_similar_model_does_not_match(model: str | None) -> None:
    """Reject empty, unknown, and unbounded-prefix model names."""
    assert lookup_context_window(model) is None


def test_builtin_claude_presets_have_context_windows() -> None:
    """Keep built-in Claude presets aligned with the context map."""
    assert all(
        lookup_context_window(model) is not None for model in CLAUDE_MODEL_IDS.values()
    )


def test_context_windows_are_positive_integers() -> None:
    """Require usable token limits for every known model."""
    assert all(
        isinstance(context_window, int) and context_window > 0
        for context_window in KNOWN_CONTEXT_WINDOWS.values()
    )
