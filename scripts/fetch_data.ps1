# Download one Nasdaq TotalView-ITCH 5.0 sample day into data/ (3.5 GB).
# The file is never committed: the directory states no licence.
$ErrorActionPreference = 'Stop'

$Day = '12302019.NASDAQ_ITCH50'
$Url = "https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/$Day.gz"
$Size = 3524013057
# Nasdaq's own .md5sum for this file returns 404, so this is the MD5 of the
# copy downloaded on 2026-11-23. It guards against a changed or damaged file;
# it is not the publisher's checksum.
$Md5 = '23514ef98821448a3f8f4c5b4095f3f5'

Set-Location (Join-Path $PSScriptRoot '..')
New-Item -ItemType Directory -Force data | Out-Null
$Target = "data/$Day.gz"
curl.exe --fail --location --retry 5 --continue-at - --output $Target $Url
if ($LASTEXITCODE -ne 0) { throw "download failed with exit code $LASTEXITCODE" }

$Got = (Get-Item $Target).Length
if ($Got -ne $Size) { throw "$Target is $Got bytes, expected $Size" }
$Sum = (Get-FileHash -Algorithm MD5 $Target).Hash.ToLower()
if ($Sum -ne $Md5) { throw "$Target has MD5 $Sum, expected $Md5" }
Write-Output "$Target is in place. Next: scripts/reproduce.ps1"
