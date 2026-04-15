#!powershell
<#
.SYNOPSIS
    Dumps every SteelSeries Sonar preset on this Windows machine to JSON files
    that arctis-chatmix can import.

.DESCRIPTION
    Sonar exposes a local REST API on a dynamic port. This script:
      1. Reads C:\ProgramData\SteelSeries\GG\coreProps.json to find the GG
         base address and auth cert.
      2. Calls /subApps to discover the Sonar app's port.
      3. GETs /configs and writes one JSON file per preset to ./presets/.

    Produced files can be:
      - Committed to the arctis-chatmix repo as seed presets, OR
      - Copied to ~/.config/arctis-chatmix/eq_presets/ on Linux and loaded
        via the app's import flow (rename to *.json and use "Import from
        Sonar" → "Open file" if available, or manually convert).

.PARAMETER OutDir
    Directory to write preset JSON files. Defaults to ./presets.

.EXAMPLE
    .\dump-sonar-presets.ps1
    .\dump-sonar-presets.ps1 -OutDir C:\Users\me\Desktop\sonar-dump
#>

param(
    [string]$OutDir = "presets"
)

$ErrorActionPreference = "Stop"

$corePropsPath = Join-Path $env:ProgramData "SteelSeries\GG\coreProps.json"
if (-not (Test-Path $corePropsPath)) {
    Write-Error "coreProps.json not found at $corePropsPath. Is SteelSeries GG installed and running?"
}

$coreProps = Get-Content $corePropsPath -Raw | ConvertFrom-Json
$ggAddress = $coreProps.ggEncryptedAddress
if (-not $ggAddress) {
    Write-Error "coreProps.json is missing ggEncryptedAddress. Is GG running?"
}

Write-Host "GG address: https://$ggAddress"

# The GG REST API uses a self-signed cert — skip verification for localhost.
# SkipCertificateCheck requires PowerShell 7+.
$skipCert = @{}
if ($PSVersionTable.PSVersion.Major -ge 7) {
    $skipCert = @{ SkipCertificateCheck = $true }
} else {
    Add-Type -TypeDefinition @"
using System.Net;
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
public class TrustAllCerts {
    public static bool Validator(object sender, X509Certificate cert, X509Chain chain, SslPolicyErrors errors) { return true; }
}
"@
    [Net.ServicePointManager]::ServerCertificateValidationCallback = [TrustAllCerts]::Validator
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

# Discover the Sonar sub-app's webServerAddress (an http:// URL with the dynamic port).
$subApps = Invoke-RestMethod -Uri "https://$ggAddress/subApps" @skipCert
$sonar = $subApps.subApps.sonar
if (-not $sonar) {
    Write-Error "Sonar sub-app not found in /subApps response. Is Sonar installed inside GG?"
}
$sonarBase = $sonar.metadata.webServerAddress
if (-not $sonarBase) {
    Write-Error "Sonar webServerAddress is empty. Is Sonar running?"
}

Write-Host "Sonar API: $sonarBase"

$configs = Invoke-RestMethod -Uri "$sonarBase/configs"
if (-not $configs) {
    Write-Error "Sonar /configs returned empty response."
}

if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir | Out-Null
}

$saved = 0
foreach ($cfg in $configs) {
    # Sanitize name for filesystem: replace anything not alphanumeric/dash/underscore
    $safeName = ($cfg.name -replace '[^\w\-\. ]', '_').Trim()
    if (-not $safeName) { $safeName = $cfg.id }

    $vadMap = @{ 1 = "game"; 2 = "chat"; 3 = "mic"; 4 = "media"; 5 = "aux" }
    $category = $vadMap[[int]$cfg.vad]
    if (-not $category) { $category = "unknown" }

    $filename = "{0}__{1}.json" -f $category, $safeName
    $path = Join-Path $OutDir $filename

    $cfg | ConvertTo-Json -Depth 10 | Set-Content -Path $path -Encoding UTF8
    $saved++
}

Write-Host "Wrote $saved preset(s) to $OutDir"
Write-Host ""
Write-Host "Next steps:"
Write-Host "  - Commit the $OutDir directory to your arctis-chatmix fork and open a PR, OR"
Write-Host "  - Copy individual JSON files somewhere the Linux app can reach and import them"
