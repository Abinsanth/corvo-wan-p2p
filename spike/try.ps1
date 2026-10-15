# Temporary M0 connection test: downloads iroh-spike for Windows and runs it.
#   irm https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/main/spike/try.ps1 | iex
# Nothing is installed: it runs from a temporary folder that is deleted afterwards.
$ErrorActionPreference = 'Stop'
# The progress bar makes Invoke-WebRequest crawl on Windows PowerShell.
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 may not offer TLS 1.2 by default, and GitHub requires it.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$name = 'iroh-spike-x86_64-pc-windows-msvc'
$url = "https://github.com/Abinsanth/corvo-wan-p2p/releases/download/spike/$name.zip"
$dir = Join-Path ([IO.Path]::GetTempPath()) ('iroh-spike-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $dir | Out-Null
try {
    Write-Host 'Downloading the connection test for Windows...'
    $zip = Join-Path $dir "$name.zip"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip
    Expand-Archive -Path $zip -DestinationPath $dir
    $exe = Join-Path $dir 'iroh-spike.exe'
    Write-Host 'Paste the key you were sent and press Enter.'
    $key = (Read-Host 'If nobody sent you one, just press Enter to wait for them').Trim()
    if ($key) { & $exe dial $key } else { & $exe listen }
} finally {
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
