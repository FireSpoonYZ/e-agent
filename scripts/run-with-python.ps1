param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Arguments
)

$envFile = Join-Path $PSScriptRoot "..\.env"
$line = Get-Content $envFile | Where-Object { $_ -match '^PYTHONHOME=' } | Select-Object -First 1
if (-not $line) {
    Write-Error "PYTHONHOME is not set in $envFile"
    exit 1
}

$pythonHome = $line.Substring('PYTHONHOME='.Length).Trim()
$env:PYTHONHOME = $pythonHome
$env:PATH = "$pythonHome;$env:PATH"

& $Executable @Arguments
exit $LASTEXITCODE
