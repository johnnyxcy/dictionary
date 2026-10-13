#!/usr/bin/env python3
"""Build a local macOS application bundle for the current architecture."""
import argparse
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main(default_profile="release"):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("release", "dev"), default=default_profile)
    parser.add_argument("--skip-build", action="store_true", help="Package an existing build")
    parser.add_argument("--name", help="Distinct app name for an isolated test profile")
    parser.add_argument("--data-dir", type=Path, help="Optional isolated history directory")
    parser.add_argument("--cli", type=Path, help="Optional development CLI override")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("Application bundles must be built on macOS")
    name = args.name or ("Dictionary" if args.profile == "release" else "Dictionary Dev")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 -]*", name):
        parser.error("App names must contain only letters, numbers, spaces, and hyphens")
    if (args.data_dir or args.cli) and name == "Dictionary":
        parser.error("Use a distinct --name when packaging a development profile")

    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1",
                              cwd=root, capture_output=True, text=True).stdout)
    package = next(p for p in metadata["packages"] if p["name"] == "dictionary")
    target = Path(metadata["target_directory"])
    if not args.skip_build:
        environment = dict(os.environ, MACOSX_DEPLOYMENT_TARGET="15.0")
        run("cargo", "build", "--locked", "--profile", args.profile, cwd=root, env=environment)
    executable = target / ("debug" if args.profile == "dev" else "release") / "dictionary"
    if not executable.is_file():
        parser.error(f"Build not found: {executable}")
    linked = run("/usr/bin/otool", "-L", str(executable), capture_output=True, text=True).stdout
    libraries = [line.strip().split(" (", 1)[0] for line in linked.splitlines()[1:]]
    external = [path for path in libraries if not path.startswith(("/System/Library/", "/usr/lib/"))]
    if external:
        parser.error(f"Unbundled dynamic libraries: {', '.join(external)}")

    target.mkdir(parents=True, exist_ok=True)
    bundle = target / f"{name}.app"
    with tempfile.TemporaryDirectory(prefix="dictionary-bundle-", dir=target) as staging:
        built = Path(staging) / bundle.name
        contents = built / "Contents"
        macos = contents / "MacOS"
        resources = contents / "Resources"
        macos.mkdir(parents=True)
        resources.mkdir()
        shutil.copy2(executable, macos / "dictionary")
        shutil.copy2(root / "LICENSE", resources / "LICENSE.txt")
        info = {
            "CFBundleIdentifier": "com.johnnyxcy.dictionary" + ("" if name == "Dictionary" else ".dev." + name.lower().replace(" ", "-")),
            "CFBundleName": name,
            "CFBundleDisplayName": name,
            "CFBundleExecutable": "dictionary",
            "CFBundlePackageType": "APPL",
            "CFBundleVersion": package["version"],
            "CFBundleShortVersionString": package["version"],
            "CFBundleIconFile": "AppIcon",
            "LSMinimumSystemVersion": "15.0",
            "NSHighResolutionCapable": True,
        }
        environment = {}
        if args.data_dir:
            environment["DICTIONARY_DATA_DIR"] = str(args.data_dir.resolve())
        if args.cli:
            environment["DICTIONARY_CLI"] = str(args.cli.absolute())
        if environment:
            info["LSEnvironment"] = environment
        with (contents / "Info.plist").open("wb") as output:
            plistlib.dump(info, output)

        iconset = Path(staging) / "AppIcon.iconset"
        iconset.mkdir()
        png = Path(staging) / "AppIcon.png"
        run("/usr/bin/swift", str(root / "scripts/macos-icon.swift"), str(png))
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = str(size * scale)
                filename = f"icon_{size}x{size}" + ("@2x" if scale == 2 else "") + ".png"
                run("/usr/bin/sips", "-z", pixels, pixels, str(png), "--out", str(iconset / filename), stdout=subprocess.DEVNULL)
        run("/usr/bin/iconutil", "-c", "icns", str(iconset), "-o", str(resources / "AppIcon.icns"))
        run("/usr/bin/plutil", "-lint", str(contents / "Info.plist"))
        # Local ad-hoc signing needs no account or certificate. Public Developer
        # ID signing and notarization are separate distribution requirements.
        run("/usr/bin/codesign", "--force", "--sign", "-", str(built))
        run("/usr/bin/codesign", "--verify", "--deep", "--strict", str(built))
        if bundle.exists():
            shutil.rmtree(bundle)
        built.rename(bundle)
    print(bundle)


if __name__ == "__main__":
    main()
