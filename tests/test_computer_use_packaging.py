"""Platform wheel hooks must retain the native identity and advertise the right ABI."""

import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("computer_use_build", ROOT / "packages/cli/hatch_build.py")
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


def bundle_at(path, platform="win32", arch="x64", version="0.1.5"):
    path.mkdir()
    (path / "runtime.json").write_text(json.dumps({
        "protocol_version": 1, "version": version, "platform": platform, "arch": arch,
    }))
    header = bytearray(64)
    header[:2] = b"MZ"
    header[60:64] = (64).to_bytes(4, "little")
    (path / "crabcode-computer-use-host.exe").write_bytes(
        header + b"PE\0\0" + (0x8664 if arch == "x64" else 0xAA64).to_bytes(2, "little")
    )
    return path


def builder():
    return hook.ComputerUseBuildHook(str(ROOT), {}, None, SimpleNamespace(version="0.1.5"), "dist", "wheel")


def test_platform_hook_includes_only_payload_and_uses_python_independent_tag(tmp_path, monkeypatch):
    bundle = bundle_at(tmp_path / "bundle")
    (bundle / "endpoint.json").write_text("never distribute local credentials")
    monkeypatch.setenv("CRABCODE_COMPUTER_USE_BUNDLE", str(bundle))
    monkeypatch.setattr(hook.sys, "platform", "win32")
    monkeypatch.setattr(hook.platform, "machine", lambda: "AMD64")
    data = {"force_include": {}}
    builder().initialize("standard", data)
    assert data["tag"] == "py3-none-win_amd64"
    assert data["pure_python"] is False
    assert set(data["force_include"].values()) == {
        "crabcode_cli/_native/runtime.json", "crabcode_cli/_native/crabcode-computer-use-host.exe",
    }


def test_macos_tag_comes_from_native_minimum_not_build_machine(tmp_path, monkeypatch):
    bundle = bundle_at(tmp_path / "bundle", platform="darwin", arch="arm64")
    monkeypatch.setattr(hook.sys, "platform", "darwin")
    monkeypatch.setattr(hook.platform, "machine", lambda: "arm64")
    commands = []

    def run(command, **kwargs):
        commands.append(command)

    monkeypatch.setattr(hook.subprocess, "run", run)
    monkeypatch.setattr(hook.subprocess, "check_output", lambda command, **kwargs:
                        "arm64\n" if command[0] == "lipo" else " platform MACOS\n minos 11.0\n sdk 26.5\n")
    assert hook.native_wheel_tag(bundle, "0.1.5") == "py3-none-macosx_11_0_arm64"
    assert commands[0][:3] == ["codesign", "--verify", "--strict"]


@pytest.mark.parametrize("mutation,error", [
    ({"version": "0.1.4"}, "protocol/version"),
    ({"arch": "arm64"}, "OS and architecture"),
    ({"platform": "linux"}, "OS and architecture"),
])
def test_mismatched_native_bundles_fail_the_build(tmp_path, monkeypatch, mutation, error):
    bundle = bundle_at(tmp_path / "bundle", **mutation)
    monkeypatch.setattr(hook.sys, "platform", "win32")
    monkeypatch.setattr(hook.platform, "machine", lambda: "AMD64")
    with pytest.raises(ValueError, match=error):
        hook.native_wheel_tag(bundle, "0.1.5")


def test_pure_and_editable_builds_do_not_embed_platform_binaries(tmp_path, monkeypatch):
    monkeypatch.delenv("CRABCODE_COMPUTER_USE_BUNDLE", raising=False)
    data = {"force_include": {}}
    builder().initialize("standard", data)
    assert data == {"force_include": {}}
    monkeypatch.setenv("CRABCODE_COMPUTER_USE_BUNDLE", str(tmp_path / "missing"))
    builder().initialize("editable", data)
    assert data == {"force_include": {}}


def test_package_hook_is_available_to_both_distributions():
    try:
        import tomllib
    except ModuleNotFoundError:
        import tomli as tomllib
    for root in (ROOT, ROOT / "packages/cli"):
        config = tomllib.loads((root / "pyproject.toml").read_text())
        assert (root / config["tool"]["hatch"]["build"]["hooks"]["custom"]["path"]).is_file()
