# Builds meta-mcp.exe and both personal-mode sidecars into dist\ (static Go binaries, CGO off); Windows PowerShell 5.1 or later.
$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist' }
if (-not $env:CGO_ENABLED) { $env:CGO_ENABLED = '0' }
if (-not $env:GOTOOLCHAIN) { $env:GOTOOLCHAIN = 'local' }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

function Fail([string]$Message) {
    Write-Host "error: $Message"
    exit 1
}

function Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message"
}

Write-Host "== meta-mcp (cargo build --release)"
Push-Location $Root
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { Fail "cargo build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
Copy-Item -Force (Join-Path $TargetDir 'release\meta-mcp.exe') (Join-Path $Dist 'meta-mcp.exe')

foreach ($side in @('messenger', 'whatsapp')) {
    Write-Host "== sidecar: $side"
    $dir = Join-Path $Root "sidecars\$side"
    $exe = "meta-$side-sidecar.exe"
    Push-Location $dir
    try {
        Step "go vet"
        go vet ./...
        if ($LASTEXITCODE -ne 0) { Fail "go vet failed in $side (exit $LASTEXITCODE)" }

        Step "go test"
        go test -count=1 ./...
        if ($LASTEXITCODE -ne 0) { Fail "go test failed in $side (exit $LASTEXITCODE)" }

        Step "go build"
        go build -trimpath -ldflags=-buildid= -o "bin\$exe" .
        if ($LASTEXITCODE -ne 0) { Fail "go build failed in $side (exit $LASTEXITCODE)" }

        if (Get-Command govulncheck -ErrorAction SilentlyContinue) {
            Step "govulncheck source"
            govulncheck ./...
            if ($LASTEXITCODE -ne 0) { Fail "govulncheck failed in $side (exit $LASTEXITCODE)" }
            Step "govulncheck binary"
            govulncheck -mode=binary "bin\$exe"
            if ($LASTEXITCODE -ne 0) { Fail "govulncheck binary scan failed in $side (exit $LASTEXITCODE)" }
        } else {
            Write-Host "warning: govulncheck not found, skipping the vulnerability scan"
        }
    } finally {
        Pop-Location
    }
    Copy-Item -Force (Join-Path $dir "bin\$exe") (Join-Path $Dist $exe)
}

Write-Host ""
Write-Host "Binaries in ${Dist}:"
Get-ChildItem $Dist | Format-Table Mode, LastWriteTime, Length, Name -AutoSize | Out-String | Write-Host
Write-Host "Install: copy the three files to one directory on your PATH (meta-mcp finds the sidecars next to itself),"
Write-Host "or point META_MCP_SIDECAR_DIR at $Dist."
