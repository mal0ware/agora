# Rebuild results/ from the sample day in data/.
#   scripts/reproduce.ps1          full day (run scripts/fetch_data.ps1 first)
#   scripts/reproduce.ps1 --fast   committed synthetic stream, a few seconds
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
cargo run --release --locked -- reproduce @args
exit $LASTEXITCODE
