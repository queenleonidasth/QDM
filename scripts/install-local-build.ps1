$ErrorActionPreference = 'Stop'
$root = 'C:\Users\QUEEN\Projects\QDM'
$install = Join-Path $env:LOCALAPPDATA 'Programs\QDM'
$release = Join-Path $root 'target\release'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$backup = Join-Path (Split-Path -Parent $root) ("backups\before-notify-session-" + $stamp)
foreach ($required in @("$release\QDM.exe", "$release\QDMHost.exe", "$root\release\QDM-Zen-AutoCapture-0.4.1.xpi", "$root\release\QDM-Chromium-Extension-0.4.1.zip")) {
  if (!(Test-Path -LiteralPath $required)) { throw "Missing required file: $required" }
}
New-Item -ItemType Directory -Path $backup -Force | Out-Null
Copy-Item -LiteralPath "$install\QDM.exe" -Destination "$backup\QDM-previous.exe" -Force
Copy-Item -LiteralPath "$install\QDMHost.exe" -Destination "$backup\QDMHost-previous.exe" -Force
Copy-Item -LiteralPath "$install\extensions" -Destination "$backup\extensions" -Recurse -Force

Get-Process QDM -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 1000
if (Get-Process QDM -ErrorAction SilentlyContinue) { throw 'The old QDM process did not exit.' }

Copy-Item -LiteralPath "$release\QDM.exe" -Destination "$install\QDM.exe" -Force
Copy-Item -LiteralPath "$release\QDMHost.exe" -Destination "$install\QDMHost.exe" -Force
New-Item -ItemType Directory -Path "$install\extensions\chromium", "$install\extensions\firefox" -Force | Out-Null
Copy-Item -Path "$root\extensions\chromium\*" -Destination "$install\extensions\chromium" -Recurse -Force
Copy-Item -Path "$root\extensions\firefox\*" -Destination "$install\extensions\firefox" -Recurse -Force
Copy-Item -LiteralPath "$root\release\QDM-Zen-AutoCapture-0.4.1.xpi" -Destination "$install\extensions\firefox\QDM-Zen-AutoCapture-0.4.1.xpi" -Force
Copy-Item -LiteralPath "$root\release\QDM-Chromium-Extension-0.4.1.zip" -Destination "$install\extensions\QDM-Chromium-Extension-0.4.1.zip" -Force
Remove-Item -LiteralPath "$install\extensions\firefox\QDM-Zen-AutoCapture-0.3.1.xpi" -Force -ErrorAction SilentlyContinue

$installedHash = (Get-FileHash -LiteralPath "$install\QDM.exe" -Algorithm SHA256).Hash.ToLowerInvariant()
$releaseHash = (Get-FileHash -LiteralPath "$release\QDM.exe" -Algorithm SHA256).Hash.ToLowerInvariant()
if ($installedHash -ne $releaseHash) { throw 'Installed executable does not match the new release build.' }

$new = Start-Process -FilePath "$install\QDM.exe" -PassThru
$deadline = (Get-Date).AddSeconds(20)
do { Start-Sleep -Milliseconds 250; $new.Refresh() } while ($new.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline)
if ($new.MainWindowHandle -eq 0) { throw 'QDM started but did not present a window.' }

[ordered]@{
  status = 'passed'
  installedAt = [DateTime]::UtcNow.ToString('o')
  executable = "$install\QDM.exe"
  executableSha256 = $installedHash
  previousBuildBackup = $backup
  processId = $new.Id
  windowTitle = $new.MainWindowTitle
  versions = (& "$install\QDM.exe" --version | Out-String).Trim()
  firefoxExtension = "$install\extensions\firefox\QDM-Zen-AutoCapture-0.4.1.xpi"
  chromiumExtension = "$install\extensions\QDM-Chromium-Extension-0.4.1.zip"
} | ConvertTo-Json -Depth 4
