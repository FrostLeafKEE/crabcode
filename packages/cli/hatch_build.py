"""Include a prebuilt native component in platform wheels, never compile at install time.

Shared by the aggregate crabcode distribution and the standalone CLI package.
CRABCODE_COMPUTER_USE_BUNDLE points to the output of computer-use/scripts/build.mjs.
Without a bundle, source/editable and universal builds retain the Python-only CLI.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


def native_wheel_tag(bundle: Path, version: str) -> str:
    manifest = json.loads((bundle / "runtime.json").read_text(encoding="utf-8"))
    if manifest.get("protocol_version") != 1 or manifest.get("version") != version:
        raise ValueError("Computer Use bundle protocol/version does not match this package")
    native_platform, arch = manifest.get("platform"), manifest.get("arch")
    machine = {"arm64": "arm64", "aarch64": "arm64", "amd64": "x64", "x86_64": "x64"}.get(platform.machine().lower())
    if native_platform != sys.platform or arch != machine:
        raise ValueError("Build Computer Use wheels on the bundle's OS and architecture")
    if native_platform == "darwin":
        app = bundle / "Crab Computer Use.app"
        executable = app / "Contents/MacOS/crabcode-computer-use-host"
        # Preserve the exact signature, including CodeResources, inside the wheel.
        subprocess.run(["codesign", "--verify", "--strict", str(app)], check=True, capture_output=True)
        architectures = subprocess.check_output(["lipo", "-archs", str(executable)], text=True).split()
        expected_arch = "arm64" if arch == "arm64" else "x86_64"
        if architectures != [expected_arch]:
            raise ValueError("Computer Use binary architecture does not match runtime.json")
        commands = subprocess.check_output(["xcrun", "vtool", "-show-build", str(executable)], text=True)
        minimum = re.search(r"^\s*minos\s+(\d+)\.(\d+)", commands, re.MULTILINE)
        if minimum is None:
            raise ValueError("Cannot determine the native component's minimum macOS version")
        tag = f"macosx_{minimum[1]}_{minimum[2]}_{expected_arch}"
    elif native_platform == "win32" and arch in ("x64", "arm64"):
        executable = bundle / "crabcode-computer-use-host.exe"
        with executable.open("rb") as stream:
            header = stream.read(64)
            if header[:2] != b"MZ" or len(header) != 64:
                raise ValueError("Computer Use bundle does not contain a Windows executable")
            stream.seek(int.from_bytes(header[60:64], "little"))
            pe = stream.read(6)
        expected_machine = 0x8664 if arch == "x64" else 0xAA64
        if pe[:4] != b"PE\0\0" or int.from_bytes(pe[4:6], "little") != expected_machine:
            raise ValueError("Computer Use binary architecture does not match runtime.json")
        tag = "win_amd64" if arch == "x64" else "win_arm64"
    else:
        raise ValueError("Computer Use platform wheels currently support macOS and Windows")
    return f"py3-none-{tag}"


class ComputerUseBuildHook(BuildHookInterface):
    def initialize(self, version: str, build_data: dict) -> None:
        directory = os.environ.get("CRABCODE_COMPUTER_USE_BUNDLE")
        if self.target_name != "wheel" or version == "editable" or not directory:
            return
        bundle = Path(directory).expanduser().resolve()
        build_data["tag"] = native_wheel_tag(bundle, self.metadata.version)
        build_data["pure_python"] = False
        # Only include the manifest and native payload, never state/endpoint files.
        payload = "Crab Computer Use.app" if sys.platform == "darwin" else "crabcode-computer-use-host.exe"
        for name in ("runtime.json", payload):
            build_data["force_include"][str(bundle / name)] = f"crabcode_cli/_native/{name}"
