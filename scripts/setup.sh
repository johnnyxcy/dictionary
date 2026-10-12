#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"

rustup component add rustfmt clippy
git config --local core.hooksPath .githooks

printf '%s\n' 'Git hooks enabled for this checkout. Commits now check Rust formatting and lints.'
