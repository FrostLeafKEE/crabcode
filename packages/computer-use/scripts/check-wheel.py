"""Check the native payload of a CrabCode platform wheel without running GUI code."""

import argparse
import json
from pathlib import Path
import plistlib
import zipfile


def check(wheel: Path) -> None:
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        metadata = archive.read(next(n for n in names if n.endswith(".dist-info/WHEEL"))).decode()
        assert "Root-Is-Purelib: false" in metadata, "native payload mislabeled as pure Python"
        assert not wheel.name.endswith("-any.whl"), "native payload mislabeled as universal"
        prefix = "crabcode_cli/_native/"
        manifest = json.loads(archive.read(prefix + "runtime.json"))
        assert manifest["protocol_version"] == 1
        if manifest["platform"] == "darwin":
            app = prefix + "Crab Computer Use.app/Contents/"
            binary = app + "MacOS/crabcode-computer-use-host"
            assert app + "Info.plist" in names
            plist = plistlib.loads(archive.read(app + "Info.plist"))
            icon = archive.read(app + "Resources/" + plist["CFBundleIconFile"])
            assert icon[:4] == b"icns" and len(icon) > 8, "app icon missing or invalid"
            assert app + "_CodeSignature/CodeResources" in names
            assert archive.getinfo(binary).external_attr >> 16 & 0o111, "executable bit lost"
            assert "-macosx_" in wheel.name
        else:
            binary = prefix + "crabcode-computer-use-host.exe"
            assert "-win_" in wheel.name
        assert archive.getinfo(binary).file_size > 0
        assert not any(n.endswith(("endpoint.json", ".lock")) for n in names if n.startswith(prefix))
    print(f"Verified native payload: {wheel.name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    directory = parser.parse_args().directory
    wheels = list(directory.glob("*.whl"))
    assert wheels, f"No wheels found in {directory}"
    for wheel in wheels:
        check(wheel)
