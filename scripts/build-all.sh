#!/usr/bin/env bash
# Builds the release binary and both personal-mode sidecars into dist/ (static Go binaries, CGO off).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="${DIST:-$ROOT/dist}"
mkdir -p "$DIST"

echo "== meta-mcp (cargo build --release)"
(cd "$ROOT" && cargo build --release --locked)
cp "$ROOT/target/release/meta-mcp" "$DIST/meta-mcp"

for side in messenger whatsapp; do
  echo "== sidecar: $side"
  (cd "$ROOT/sidecars/$side" && GOTOOLCHAIN=local ./build.sh)
  cp "$ROOT/sidecars/$side/bin/meta-$side-sidecar" "$DIST/meta-$side-sidecar"
done

echo
echo "Binaries in $DIST:"
ls -la "$DIST"
echo
echo "Install: copy the three files to one directory on your PATH (meta-mcp finds the sidecars next to itself),"
echo "or point META_MCP_SIDECAR_DIR at $DIST."
