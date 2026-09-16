"""The workspace reports the Gateway process's actual runtime, not the client's."""

import platform
import sys
from pathlib import Path
from unittest.mock import patch

from crabcode_gateway import __version__
from crabcode_gateway.routes.workspace import build_workspace_info
from crabcode_gateway.schemas import WorkspaceInfo


def test_workspace_reports_running_python_and_gateway(tmp_path):
    runtime = build_workspace_info(str(tmp_path), []).runtime
    assert runtime is not None
    assert runtime.python_executable == sys.executable
    assert runtime.python_version == platform.python_version()
    assert runtime.python_prefix == sys.prefix
    assert runtime.gateway_version == __version__
    assert Path(runtime.gateway_path).name == "crabcode_gateway"
    assert runtime.platform == f"{platform.system()} {platform.machine()}"


def test_workspace_distinguishes_virtual_and_conda_environments(tmp_path):
    with patch.object(sys, "prefix", str(tmp_path)), patch.object(sys, "base_prefix", "/base"):
        assert build_workspace_info(str(tmp_path), []).runtime.environment_kind == "venv"
        (tmp_path / "conda-meta").mkdir()
        assert build_workspace_info(str(tmp_path), []).runtime.environment_kind == "conda"


def test_workspace_supports_older_payloads_without_runtime():
    assert WorkspaceInfo(startup_cwd="/work", home="/home").runtime is None
