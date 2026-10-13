#!/usr/bin/env python3
"""Package an Apple Silicon macOS release as a verified ZIP and SHA-256 checksum."""
import argparse
import hashlib
import json
import os
import platform
import plistlib
import subprocess
import sys
import tempfile
from pathlib import Path


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="Require a tag matching the Cargo version, e.g. v0.1.0")
    parser.add_argument("--check-only", action="store_true", help="Validate the host and tag without building")
    parser.add_argument("--skip-build", action="store_true", help="Package an existing release build")
    args = parser.parse_args()
    if sys.platform != "darwin" or platform.machine() != "arm64":
        parser.error("Release archives must be built on an Apple Silicon macOS host")

    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1",
                              cwd=root, capture_output=True, text=True).stdout)
    package = next(p for p in metadata["packages"] if p["name"] == "dictionary")
    version = package["version"]
    if args.tag and args.tag != f"v{version}":
        parser.error(f"Tag {args.tag!r} does not match Cargo.toml version v{version}")
    if args.check_only:
        print(f"Ready to build Dictionary {version} for macOS arm64")
        return

    command = [sys.executable, str(root / "scripts/bundle.py")]
    if args.skip_build:
        command.append("--skip-build")
    run(*command, cwd=root)
    target = Path(metadata["target_directory"])
    bundle = target / "Dictionary.app"
    with (bundle / "Contents/Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if info.get("CFBundleShortVersionString") != version or "LSEnvironment" in info:
        parser.error("Release bundle has an unexpected version or development environment")
    executable = bundle / "Contents/MacOS/dictionary"
    architectures = run("/usr/bin/lipo", "-archs", str(executable), capture_output=True, text=True).stdout.split()
    if architectures != ["arm64"]:
        parser.error(f"Release executable must contain only arm64, found {architectures}")

    destination = target / "release-assets"
    destination.mkdir(parents=True, exist_ok=True)
    archive_name = f"Dictionary-{version}-macos-arm64.zip"
    with tempfile.TemporaryDirectory(prefix="dictionary-release-", dir=target) as staging:
        staging = Path(staging)
        archive = staging / archive_name
        run("/usr/bin/ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(bundle), str(archive))

        # Validate the downloaded app, including its executable mode and signature.
        extracted = staging / "extracted"
        run("/usr/bin/ditto", "-x", "-k", str(archive), str(extracted))
        unpacked = extracted / bundle.name
        run("/usr/bin/codesign", "--verify", "--deep", "--strict", str(unpacked))
        if not os.access(unpacked / "Contents/MacOS/dictionary", os.X_OK):
            parser.error("The archived app executable lost its executable permission")
        if (unpacked / "Contents/Resources/LICENSE.txt").read_bytes() != (root / "LICENSE").read_bytes():
            parser.error("The archived app is missing the project license")

        with archive.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        checksum = staging / f"{archive_name}.sha256"
        checksum.write_text(f"{digest}  {archive_name}\n", encoding="utf-8")
        archive = archive.replace(destination / archive.name)
        checksum = checksum.replace(destination / checksum.name)

    # Upload only the files from this build, even when a restored cache contains older releases.
    if output := os.environ.get("GITHUB_OUTPUT"):
        with Path(output).open("a", encoding="utf-8") as output_file:
            output_file.write(f"archive={archive}\nchecksum={checksum}\n")
    print(archive)
    print(checksum)


if __name__ == "__main__":
    main()
