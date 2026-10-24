#!/usr/bin/env bash
# Local gate: formatting, lints and tests. Stops at the first failure.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
