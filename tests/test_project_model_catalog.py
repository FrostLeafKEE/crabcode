"""Project-scoped model discovery and live session catalog updates."""
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

from fastapi import FastAPI
from fastapi.testclient import TestClient

from crabcode_core.events import CoreSession
from crabcode_core.types.config import CrabCodeSettings
from crabcode_gateway.routes import config as routes


def write_settings(directory, models, default="DeepSeek"):
    target = directory / ".crabcode"
    target.mkdir(exist_ok=True)
    (target / "settings.json").write_text(json.dumps({
        "models": {name: {"provider": "openai", "model": model} for name, model in models.items()},
        "default_model": default,
    }), encoding="utf-8")


def test_project_catalog_merges_files_ignores_default_session_and_refreshes(tmp_path, monkeypatch):
    home, project, other = [tmp_path / name for name in ("home", "project", "other")]
    for directory in (home, project, other):
        directory.mkdir()
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    monkeypatch.chdir(other)
    write_settings(home, {"DeepSeek": "deepseek-flash"})
    write_settings(project, {"Mimo": "mimo-v2.6-pro"}, "Mimo")
    monkeypatch.setattr(routes, "_resolve_model_settings_cwd", lambda request, cwd: cwd)
    app = FastAPI()
    app.include_router(routes.router)
    legacy = SimpleNamespace(list_models=lambda: {"Legacy": "legacy"})
    app.state.sessions = {"old": legacy}
    app.state.default_session_id = "old"
    with TestClient(app) as client:
        def names(**params):
            response = client.get("/config/models", params=params)
            assert response.status_code == 200
            return [model["name"] for model in response.json()]
        assert names(cwd=str(project)) == ["DeepSeek", "Mimo"]
        assert names(cwd=str(other)) == ["DeepSeek"]
        assert names() == ["Legacy"]
        assert names(session_id="old") == ["Legacy"]
        assert client.get("/config/models", params={"session_id": "missing"}).status_code == 404
        assert client.get("/config/models", params={"session_id": "old", "cwd": str(project)}).status_code == 400
        write_settings(project, {"New": "new-id"}, "New")
        assert names(cwd=str(project)) == ["DeepSeek", "New"]


def test_existing_session_switch_reads_new_config_and_failure_preserves_state(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    write_settings(home, {"DeepSeek": "deepseek-flash"})
    session = CoreSession(settings=CrabCodeSettings(), cwd=str(tmp_path))
    session._current_model_name = "DeepSeek"
    previous = object()
    session._api_adapter = previous
    write_settings(tmp_path, {"Mimo": "mimo-v2.6-pro"}, "Mimo")
    create = Mock(side_effect=RuntimeError("invalid credentials"))
    monkeypatch.setattr("crabcode_core.api.create_adapter", create)
    before = session.settings.model_dump()
    assert not session.switch_model("Mimo")
    assert session._api_adapter is previous
    assert session._current_model_name == "DeepSeek"
    assert session.settings.model_dump() == before
    create.side_effect = None
    create.return_value = object()
    assert session.switch_model("Mimo")
    assert create.call_args.args[0].model == "mimo-v2.6-pro"
    assert session._current_model_name == "Mimo"
    assert not session.switch_model("missing")
    assert session._current_model_name == "Mimo"
    write_settings(tmp_path, {"Mimo": "updated-id"}, "DeepSeek")
    assert session._current_model_name == "Mimo"
    assert session.switch_model("Mimo")
    assert create.call_args.args[0].model == "updated-id"
    assert session._current_model_name == "Mimo"
