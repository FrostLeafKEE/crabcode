"""Check the native payload of a CrabCode platform wheel without running GUI code."""

import argparse
import json
from pathlib import Path
import plistlib
import struct
import zipfile


def check_windows_icon(binary: bytes) -> None:
    """Compare the embedded default icon with every rendition in the master ICO."""
    import pefile  # Build-time verification only: python -m pip install pefile

    source = (Path(__file__).resolve().parents[1] / "icons/icon.ico").read_bytes()
    reserved, kind, count = struct.unpack_from("<HHH", source)
    assert (reserved, kind) == (0, 1) and count > 0, "invalid source ICO"
    with pefile.PE(data=binary, fast_load=True) as executable:
        executable.parse_data_directories(directories=[
            pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_RESOURCE"],
        ])
        assert hasattr(executable, "DIRECTORY_ENTRY_RESOURCE"), "exe has no icon resources"
        types = {entry.id: entry for entry in executable.DIRECTORY_ENTRY_RESOURCE.entries}
        assert 14 in types and 3 in types, "exe is missing its icon group or images"
        groups = {entry.id: entry for entry in types[14].directory.entries}
        assert 1 in groups, "exe is missing its default icon (resource ID 1)"
        images = {entry.id: entry for entry in types[3].directory.entries}

        def data(entry):
            resource = entry.data.struct
            return executable.get_data(resource.OffsetToData, resource.Size)

        languages = groups[1].directory.entries
        assert languages, "embedded icon group is empty"
        for language in languages:
            group = data(language)
            assert struct.unpack_from("<HHH", group) == (0, 1, count), "embedded icon sizes differ"
            for index in range(count):
                expected = struct.unpack_from("<BBBBHHII", source, 6 + index * 16)
                actual = struct.unpack_from("<BBBBHHIH", group, 6 + index * 14)
                assert actual[:7] == expected[:7], "embedded icon rendition differs"
                image = images.get(actual[7])
                assert image is not None, "embedded icon references a missing image"
                localized = next((entry for entry in image.directory.entries if entry.id == language.id), None)
                assert localized is not None, "embedded icon image language missing"
                assert data(localized) == source[expected[7]:expected[7] + expected[6]], "embedded icon pixels differ"


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
            check_windows_icon(archive.read(binary))
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
