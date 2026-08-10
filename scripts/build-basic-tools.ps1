param(
    [switch]$Release
)

$profile = if ($Release) { "release" } else { "debug" }
$releaseArg = if ($Release) { @("--release") } else { @() }

& cargo build -p e-agent-basic-tools @releaseArg
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$target = Join-Path $PSScriptRoot "..\target\$profile"
Copy-Item (Join-Path $target "basic_tools.dll") (Join-Path $target "basic_tools.pyd") -Force
