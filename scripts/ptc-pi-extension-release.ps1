param(
    [Parameter(Mandatory = $true)]
    [string]$OfficialPiRoot
)

$ErrorActionPreference = "Stop"
$commit = "e429d90b800f9a37c8a5812f4c9c10a8cdcc85a7"
if ((git -C $OfficialPiRoot rev-parse HEAD).Trim() -ne $commit) {
    throw "Official Pi checkout must be at $commit"
}
if (-not (Get-Command rg -ErrorAction SilentlyContinue)) {
    throw "rg must be available in PATH"
}

$sep = [IO.Path]::PathSeparator
$env:E_AGENT_EXTENSION_PATHS = @(
    (Join-Path $OfficialPiRoot "packages/coding-agent/examples/extensions/todo.ts"),
    (Join-Path $OfficialPiRoot "packages/coding-agent/examples/extensions/truncated-tool.ts")
) -join $sep

cargo run --release -- 'Use the node tool exactly once. In that one program, import { todo } from "todo"; call await todo({ action: "add", text: "PTC Pi extension acceptance" }); then call await todo({ action: "list" }); print the list result text. Do not use any other tool.'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo run --release -- 'Use the node tool exactly once. In that one program, import { rg } from "truncated_tool"; call await rg({ pattern: "registerTool", path: "extensions.md" }); print result.content[0].text. Do not use any other tool.'
exit $LASTEXITCODE
