#!/usr/bin/env python3
"""Wrap an existing debug build for native macOS development and UI inspection."""
import argparse
import plistlib
import re
import shutil
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--data-dir", type=Path, help="Optional isolated history for manual testing")
parser.add_argument("--cli", type=Path, help="Optional CLI override for setup and offline-history testing")
parser.add_argument("--name", default="Dictionary Dev", help="Distinct app name for isolated test profiles")
args = parser.parse_args()
if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 -]*", args.name):
    parser.error("App names must contain only letters, numbers, spaces, and hyphens")
root = Path(__file__).resolve().parent.parent
executable = root / "target/debug/dictionary"
if not executable.is_file():
    parser.error("Build the application first with cargo build --locked")
bundle = root / "target" / f"{args.name}.app"
macos = bundle / "Contents/MacOS"
macos.mkdir(parents=True, exist_ok=True)
shutil.copy2(executable, macos / "dictionary")
info = {
    "CFBundleIdentifier": "com.johnnyxcy.dictionary.dev" + ("" if args.name == "Dictionary Dev" else "." + args.name.lower().replace(" ", "-")),
    "CFBundleName": args.name,
    "CFBundleDisplayName": args.name,
    "CFBundleExecutable": "dictionary",
    "CFBundlePackageType": "APPL",
    "CFBundleVersion": "1",
    "CFBundleShortVersionString": "0.1.0",
    "LSMinimumSystemVersion": "15.0",
    "NSHighResolutionCapable": True,
}
environment = {}
if args.data_dir:
    environment["DICTIONARY_DATA_DIR"] = str(args.data_dir.resolve())
if args.cli:
    environment["DICTIONARY_CLI"] = str(args.cli.resolve())
if environment:
    info["LSEnvironment"] = environment
with (bundle / "Contents/Info.plist").open("wb") as output:
    plistlib.dump(info, output)
print(bundle)
