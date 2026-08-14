param(
    [switch]$Release
)

$releaseArg = if ($Release) { @("--release") } else { @() }

& cargo build -p e-agent-basic-tools @releaseArg
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
