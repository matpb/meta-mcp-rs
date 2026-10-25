#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
export GOTOOLCHAIN="${GOTOOLCHAIN:-local}" CGO_ENABLED=0

step() { printf '\n==> %s\n' "$1"; }

step "go vet"
go vet ./...

step "go test"
go test -count=1 ./...

step "go build"
# Symbols stay in: govulncheck -mode=binary on a stripped binary flags whole modules.
go build -trimpath -ldflags="-buildid=" -o bin/meta-whatsapp-sidecar .
# Only Linux binaries are static; macOS links libSystem dynamically.
if [ "$(go env GOOS)" = linux ] && ! file bin/meta-whatsapp-sidecar | grep -q "statically linked"; then
  echo "error: bin/meta-whatsapp-sidecar is not statically linked" >&2
  exit 1
fi

# govulncheck exits 3 on a reachable vulnerability, which set -e turns into a failure.
step "govulncheck source"
govulncheck ./...

step "govulncheck binary"
govulncheck -mode=binary bin/meta-whatsapp-sidecar

step "done"
echo "$PWD/bin/meta-whatsapp-sidecar"
