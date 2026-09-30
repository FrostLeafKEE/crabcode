"""Resume restores the model configuration last used in that session."""

import asyncio
import json
from pathlib import Path

from crabcode_core.config.manager import ConfigManager
from crabcode_core.events import CoreSession
from crabcode_core.session.storage import SessionStorage
from crabcode_core.types.config import CrabCodeSettings


class FakeAdapter:
    def __init__(self, config):
        self.config = config


def _isolate(monkeypatch, tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    config = tmp_path / "config"
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    monkeypatch.setattr(
        "crabcode_core.session.storage.get_config_home",
        lambda: config,
    )
    monkeypatch.setattr(
        "crabcode_core.session.meta_db.get_config_home",
        lambda: config,
    )
    monkeypatch.setattr(
        "crabcode_core.api.create_adapter",
        lambda config: FakeAdapter(config),
    )


def _write_settings(project: Path, *, base_model: str = "gpt-base") -> None:
    target = project / ".crabcode"
    target.mkdir(parents=True, exist_ok=True)
    (target / "settings.json").write_text(
        json.dumps(
            {
                "api": {"provider": "openai", "model": base_model},
                "default_model": "fast",
                "models": {
                    "fast": {
                        "provider": "openai",
                        "model": "gpt-fast",
                        "base_url": "https://fast.example/v1",
                        "max_tokens": 1000,
                        "reasoning_effort": "low",
                        "thinking_enabled": True,
                    },
                    "smart": {
                        "provider": "openai",
                        "model": "gpt-smart",
                        "base_url": "https://smart.example/v1",
                        "max_tokens": 4000,
                        "reasoning_effort": "high",
                        "thinking_enabled": False,
                    },
                    "smart-backup": {
                        "provider": "openai",
                        "model": "gpt-smart",
                        "base_url": "https://backup.example/v1",
                        "max_tokens": 2000,
                        "reasoning_effort": "medium",
                    },
                },
            }
        ),
        encoding="utf-8",
    )


def _session(project: Path, *, pinned: CrabCodeSettings | None = None, current: str | None = "fast"):
    explicit = pinned if pinned is not None else CrabCodeSettings()
    settings = CrabCodeSettings()
    settings._crabcode_explicit_settings = explicit
    session = CoreSession(cwd=str(project), settings=settings)
    session._initialized = True
    session.settings = ConfigManager(cwd=str(project)).load()
    session._current_model_name = current
    session._api_adapter = FakeAdapter(session.settings.get_api_config(current))
    return session


def _resume(session: CoreSession, session_id: str) -> bool:
    return asyncio.run(session.resume(session_id))


def test_resume_restores_named_profile_and_keeps_context(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    _isolate(monkeypatch, tmp_path)
    _write_settings(project)

    original = _session(project)
    session_id = original.new_session()
    assert original.switch_model("smart")
    assert original._session_storage.meta["model_profile"] == "smart"
    original._session_storage.record_context_usage(1234, 8000, source="server")

    resumed = _session(project)
    assert _resume(resumed, session_id)
    assert resumed._current_model_name == "smart"
    active = resumed.settings.get_api_config(resumed._current_model_name)
    assert active.model == "gpt-smart"
    assert active.base_url == "https://smart.example/v1"
    assert active.max_tokens == 4000
    assert active.reasoning_effort == "high"
    assert active.thinking_enabled is False
    assert resumed._api_adapter.config is active
    assert resumed.last_context_used_tokens == 1234
    assert resumed.last_context_token_source == "server"


def test_resume_legacy_transcript_matches_unique_model(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    _isolate(monkeypatch, tmp_path)
    _write_settings(project)
    # Drop the duplicate so model+provider identifies one profile, as older
    # transcripts did not store the profile name.
    settings_path = project / ".crabcode" / "settings.json"
    raw = json.loads(settings_path.read_text(encoding="utf-8"))
    raw["models"].pop("smart-backup")
    settings_path.write_text(json.dumps(raw), encoding="utf-8")

    storage = SessionStorage(str(project), "legacy-session")
    storage.write_meta(model="gpt-smart", provider="openai")

    resumed = _session(project)
    assert _resume(resumed, storage.session_id)
    active = resumed.settings.get_api_config(resumed._current_model_name)
    assert resumed._current_model_name == "smart"
    assert active.base_url == "https://smart.example/v1"
    assert resumed._api_adapter.config is active
    assert resumed._session_storage.meta["model_profile"] == "smart"


def test_resume_uses_profile_when_model_ids_collide(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    _isolate(monkeypatch, tmp_path)
    _write_settings(project)

    storage = SessionStorage(str(project), "shared-model")
    storage.write_meta(
        model="gpt-smart",
        provider="openai",
        model_profile="smart-backup",
    )

    resumed = _session(project)
    assert _resume(resumed, storage.session_id)
    active = resumed.settings.get_api_config(resumed._current_model_name)
    assert resumed._current_model_name == "smart-backup"
    assert active.base_url == "https://backup.example/v1"
    assert active.max_tokens == 2000
    assert resumed._api_adapter.config is active


def test_explicit_model_flag_overrides_stored_profile(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    _isolate(monkeypatch, tmp_path)
    _write_settings(project)

    storage = SessionStorage(str(project), "pinned-session")
    storage.write_meta(model="gpt-smart", provider="openai", model_profile="smart")

    pinned = CrabCodeSettings()
    pinned.default_model = "fast"
    resumed = _session(project, pinned=pinned)
    previous = resumed._api_adapter
    assert _resume(resumed, storage.session_id)
    assert resumed._current_model_name == "fast"
    assert resumed._api_adapter is previous
    assert resumed.settings.get_api_config(resumed._current_model_name).model == "gpt-fast"


def test_resume_unnamed_api_does_not_jump_to_default_profile(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    _isolate(monkeypatch, tmp_path)
    _write_settings(project)

    storage = SessionStorage(str(project), "base-api")
    storage.write_meta(model="gpt-base-used", provider="openai", model_profile="")

    resumed = _session(project)
    assert _resume(resumed, storage.session_id)
    assert resumed._current_model_name is None
    assert resumed._api_adapter.config.model == "gpt-base-used"
    assert resumed._api_adapter.config.base_url is None
    assert resumed.settings.get_api_config(None).model == "gpt-base-used"


def test_prepare_selection_prefers_stored_profile_over_current(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    _isolate(monkeypatch, tmp_path)
    _write_settings(project)

    session = _session(project)
    session._pending_resume_model = {
        "kind": "profile",
        "profile": "smart",
        "model": "gpt-smart",
        "provider": "openai",
    }
    assert session._select_model_profile(session.settings) == "smart"
