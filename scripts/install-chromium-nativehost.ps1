# Register the QDM native-messaging host for a specific Chromium extension ID.
#
# Firefox/Zen auto-pairing is handled by install-zen-autopair.ps1. Chromium and
# Edge, however, require an OS-level registry entry that points the fixed host name
# "com.queen.qdm" at a manifest whose allowed_origins list the exact extension ID.
#
# Chrome refuses to trust a native host unless the manifest names the requesting
# extension, so a wildcard is never written. The caller must already know the loaded
# (packed or unpacked) extension ID.
#
# This script:
#   - validates the extension ID format and the host binary,
#   - backs up any existing registry values and manifest,
#   - writes qdm-chromium-native-host.json (allowed_origins = the one ID),
#   - registers it under both Google Chrome and Microsoft Edge per-user hive,
#   - reads the keys back to verify,
#   - emits an evidence record.
#
# It only touches HKCU and the per-user install folder; it never edits the running
# QDM executable, the download database, or the pairing secret.
param(
    [Parameter(Mandatory = $true)][string]$ExtensionId,
    [ValidateSet('both', 'chrome', 'edge')][string]$Browser = 'both',
    [string]$HostExe,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# Chrome and Edge extension IDs are exactly 32 characters drawn from a-p.
if ($ExtensionId -notmatch '^[a-p]{32}$') {
    throw 'ExtensionId must be 32 characters using only a-p (the Chrome/Edge extension ID alphabet).'
}

$install = Join-Path $env:LOCALAPPDATA 'Programs\QDM'
$hostTarget = if ($HostExe) { $HostExe } else { (Join-Path $install 'QDMHost.exe') }
if (-not (Test-Path -LiteralPath $hostTarget)) {
    $built = Join-Path $root 'target\release\QDMHost.exe'
    if (Test-Path -LiteralPath $built) { $hostTarget = $built }
    else { throw "QDMHost.exe not found at $hostTarget. Build it or pass -HostExe." }
}
# The host exe will be copied to the install folder so the registry points at a stable path.
$hostPath = Join-Path $install 'QDMHost.exe'
$manifestPath = Join-Path $install 'qdm-chromium-native-host.json'
$manifestName = 'chrome-extension://' + $ExtensionId + '/'

$origins = @($manifestName)

$manifest = [ordered]@{
    name           = 'com.queen.qdm'
    description    = 'QDM QUEEN Download Manager native messaging host'
    path           = $hostPath
    type           = 'stdio'
    allowed_origins = $origins
}

# Resolve which per-user registries to write. Both Chrome and Edge honour these keys.
$chromeReg = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.queen.qdm'
$edgeReg = 'HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\com.queen.qdm'
$targets = switch ($Browser) {
    'chrome' { @($chromeReg) }
    'edge'   { @($edgeReg) }
    default  { @($chromeReg, $edgeReg) }
}

if ($DryRun) {
    $preview = [ordered]@{
        extensionId = $ExtensionId
        manifestPath = $manifestPath
        manifest = $manifest
        registryKeys = $targets
        dryRun = $true
    }
    return $preview | ConvertTo-Json -Depth 6
}

New-Item -ItemType Directory -Path $install -Force | Out-Null

# Preserve what was there before so this is reversible.
$backupStamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$backupDir = Join-Path (Split-Path -Parent $root) ('backups\chromium-nativehost-' + $backupStamp)
New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
$priorHost = if (Test-Path -LiteralPath $hostPath) { Copy-Item -LiteralPath $hostPath -Destination (Join-Path $backupDir 'QDMHost-previous.exe') -Force; $true } else { $false }
$priorManifest = if (Test-Path -LiteralPath $manifestPath) { Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $backupDir 'qdm-chromium-native-host-previous.json') -Force; $true } else { $false }
$priorReg = foreach ($key in $targets) { if (Test-Path $key) { [pscustomobject]@{ key = $key; value = (Get-Item $key).GetValue('') } } }

# Install a fresh host binary at the stable path the manifest references.
Copy-Item -LiteralPath $hostTarget -Destination $hostPath -Force

# Write the manifest as UTF-8 without a BOM (browsers parse it byte-for-byte).
$utf8NoBom = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 6), $utf8NoBom)

foreach ($key in $targets) {
    New-Item -Path $key -Force | Out-Null
    Set-Item -Path $key -Value $manifestPath
}

# Verify every registry entry resolves to the manifest we just wrote with the right origin.
$verify = foreach ($key in $targets) {
    $regValue = (Get-Item $key).GetValue('')
    $loaded = Get-Content -LiteralPath $regValue -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($regValue -ne $manifestPath -or $loaded.path -ne $hostPath -or $loaded.allowed_origins[0] -ne $manifestName) {
        throw "Native-host registration verification failed for $key."
    }
    [pscustomobject]@{ key = $key; manifest = $regValue; origin = $loaded.allowed_origins[0] }
}

$record = [ordered]@{
    status        = 'passed'
    registeredAt  = ([DateTime]::UtcNow).ToString('o')
    extensionId   = $ExtensionId
    browser       = $Browser
    hostPath      = $hostPath
    hostSha256    = (Get-FileHash -LiteralPath $hostPath -Algorithm SHA256).Hash.ToLowerInvariant()
    manifestPath  = $manifestPath
    allowedOrigin = $manifestName
    registryKeys  = @($verify | ForEach-Object { $_.key })
    priorHostPresent = $priorHost
    priorManifestPresent = $priorManifest
    priorRegistry = @($priorReg | ForEach-Object { [pscustomobject]@{ key = $_.key; value = $_.value } })
    backupDirectory = $backupDir
    note          = 'Load the unpacked/packed extension in the browser, then in QDM use Browser connection to confirm the pairing token, or rely on native discovery from the extension.'
}
$evidenceDir = Join-Path $root 'docs\evidence\chromium-nativehost-' + $backupStamp
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null
($record | ConvertTo-Json -Depth 8) | Set-Content -LiteralPath (Join-Path $evidenceDir 'registration.json') -Encoding UTF8
$record | ConvertTo-Json -Depth 8
