"""Model settings deletion reports a wrong layer instead of silently succeeding."""

import json
from pathlib import Path

from fastapi import FastAPI
from fastapi.testclient import TestClient

from crabcode_gateway.routes import config as routes


def test_delete_model_requires_the_selected_layer_to_contain_it(tmp_path, monkeypatch):
    home = tmp_path / "home"
    project = tmp_path / "project"
    home.mkdir()
    project.mkdir()
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    monkeypatch.setattr(routes, "_resolve_model_settings_cwd", lambda request, cwd: cwd)
    user_file = home / ".crabcode" / "settings.json"
    user_file.parent.mkdir()
    user_file.write_text(json.dumps({"models": {"example": {"provider": "openai", "model": "example"}}}), encoding="utf-8")
    project_file = project / ".crabcode" / "settings.json"
    project_file.parent.mkdir()
    project_file.write_text(json.dumps({"groups": {"shared": {"provider": "openai"}}}), encoding="utf-8")

    app = FastAPI()
    app.include_router(routes.router)
    with TestClient(app) as client:
        wrong_layer = client.post("/config/model-settings", json={
            "action": "delete_model", "source": "projectSettings", "cwd": str(project), "name": "example",
        })
        assert wrong_layer.status_code == 404
        assert "不在所选配置层中" in wrong_layer.json()["detail"]
        assert "example" in json.loads(user_file.read_text(encoding="utf-8"))["models"]

        deleted = client.post("/config/model-settings", json={
            "action": "delete_model", "source": "userSettings", "cwd": str(project), "name": "example",
        })
        assert deleted.status_code == 200
        assert all(model["name"] != "example" for model in deleted.json()["models"])
        assert "models" not in json.loads(user_file.read_text(encoding="utf-8"))

        wrong_group_layer = client.post("/config/model-settings", json={
            "action": "delete_group", "source": "userSettings", "cwd": str(project), "name": "shared",
        })
        assert wrong_group_layer.status_code == 404
        assert deleted.json()["group_sources"]["shared"] == [str(project_file)]
        removed_group = client.post("/config/model-settings", json={
            "action": "delete_group", "source": "projectSettings", "cwd": str(project), "name": "shared",
        })
        assert removed_group.status_code == 200
        assert "shared" not in removed_group.json()["groups"]
