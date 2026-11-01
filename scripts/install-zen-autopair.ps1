param([switch]$SkipStartup)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$install = Join-Path $env:LOCALAPPDATA 'Programs\QDM'
$app = Join-Path $install 'QDM.exe'
$hostSource = Join-Path $root 'target\release\QDMHost.exe'
$hostExe = Join-Path $install 'QDMHost.exe'
$extensionSource = Join-Path $root 'release\QDM-Zen-AutoCapture-0.3.1.xpi'
$extensionInstalled = Join-Path $install 'extensions\firefox\QDM-Zen-AutoCapture-0.3.1.xpi'
$profile = Join-Path $env:APPDATA 'zen\Profiles\tvegrpxk.Default (release)'
$profileExtension = Join-Path $profile 'extensions\qdm@queen.local.xpi'
$reportDir = Join-Path $root 'docs\evidence\zen-autopair-20261027'
$backup = Join-Path (Split-Path -Parent $root) ('backups\zen-autopair-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
$regPath = 'HKCU:\Software\Mozilla\NativeMessagingHosts\com.queen.qdm'
$runPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$manifestPath = Join-Path $install 'qdm-native-host.json'
$pairingFile = Join-Path $env:APPDATA 'QDM\browser-pairing-v2.bin'
foreach($p in @($app,$hostSource,$extensionSource,$profileExtension,$pairingFile)){if(!(Test-Path -LiteralPath $p)){throw ('Missing required file: '+$p)}}
$expectedApp = '4e0f518043cb73a4205da215716c3da7529cb605dbae5cb0ea8ca66a672fd0cc'
if((Get-FileHash -LiteralPath $app -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expectedApp){throw 'The installed QDM executable has changed; do not modify this installation blindly.'}
$oldPairHash = (Get-FileHash $pairingFile -Algorithm SHA256).Hash
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($extensionSource)
try {
  $reader = [IO.StreamReader]::new($zip.GetEntry('manifest.json').Open())
  try {$meta = $reader.ReadToEnd() | ConvertFrom-Json} finally {$reader.Dispose()}
} finally {$zip.Dispose()}
if($meta.version -cne '0.3.1' -or $meta.browser_specific_settings.gecko.id -cne 'qdm@queen.local' -or $meta.permissions -notcontains 'nativeMessaging') {throw 'Extension payload validation failed.'}
New-Item -ItemType Directory -Path $reportDir,$backup,(Split-Path $extensionInstalled -Parent) -Force | Out-Null
Copy-Item -LiteralPath $profileExtension -Destination (Join-Path $backup 'zen-qdm-extension-previous.xpi') -Force
if(Test-Path $hostExe){Copy-Item -LiteralPath $hostExe -Destination (Join-Path $backup 'QDMHost-previous.exe') -Force}
if(Test-Path $manifestPath){Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $backup 'qdm-native-host-previous.json') -Force}
$oldHostReg = if(Test-Path $regPath){(Get-Item $regPath).GetValue('')}else{$null}
$oldStartup = (Get-ItemProperty -Path $runPath -Name 'QDM' -ErrorAction SilentlyContinue).QDM
Copy-Item -LiteralPath $hostSource -Destination $hostExe -Force
Copy-Item -LiteralPath $extensionSource -Destination $extensionInstalled -Force
$manifest = [ordered]@{
  name = 'com.queen.qdm'
  description = 'QDM Zen secure automatic pairing host'
  path = $hostExe
  type = 'stdio'
  allowed_extensions = @('qdm@queen.local')
}
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText($manifestPath,($manifest | ConvertTo-Json -Depth 5),$utf8)
New-Item -Path $regPath -Force | Out-Null
Set-Item -Path $regPath -Value $manifestPath
if(!$SkipStartup){Set-ItemProperty -Path $runPath -Name 'QDM' -Value ('"' + $app + '"')}
$actualReg = (Get-Item $regPath).GetValue('')
$actualManifest = Get-Content -LiteralPath $actualReg -Raw -Encoding UTF8 | ConvertFrom-Json
if($actualReg -cne $manifestPath -or $actualManifest.path -cne $hostExe -or $actualManifest.allowed_extensions[0] -cne 'qdm@queen.local'){throw 'Native-host registration verification failed.'}
if((Get-FileHash $pairingFile -Algorithm SHA256).Hash -cne $oldPairHash){throw 'Pairing secret file unexpectedly changed.'}
$record=[ordered]@{
  status='passed'
  installedAt=[DateTime]::UtcNow.ToString('o')
  hostPath=$hostExe
  hostSha256=(Get-FileHash $hostExe -Algorithm SHA256).Hash.ToLowerInvariant()
  manifestPath=$manifestPath
  nativeHostRegistry=$regPath
  nativeHostRegistered=$true
  startupEnabled=(!$SkipStartup)
  startupRegistryValue=if(!$SkipStartup){(Get-ItemProperty $runPath -Name 'QDM').QDM}else{$oldStartup}
  priorStartupValue=$oldStartup
  priorNativeHostRegistryValue=$oldHostReg
  extensionStaged=$extensionInstalled
  extensionSha256=(Get-FileHash $extensionInstalled -Algorithm SHA256).Hash.ToLowerInvariant()
  activeZenProfileUnmodified=$true
  profileExtensionVersion=$null
  profileExtensionBackup=(Join-Path $backup 'zen-qdm-extension-previous.xpi')
  backupDirectory=$backup
  installedQdmExeUnchanged=$true
  pairingDataUnchanged=$true
}
($record|ConvertTo-Json -Depth 5)|Set-Content -LiteralPath (Join-Path $reportDir 'installation.json') -Encoding UTF8
$record|ConvertTo-Json -Depth 5
