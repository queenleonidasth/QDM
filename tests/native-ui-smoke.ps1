param(
  [Parameter(Mandatory=$true)][string]$Binary,
  [Parameter(Mandatory=$true)][string]$NodeBinary,
  [Parameter(Mandatory=$true)][string]$OutputDirectory,
  [switch]$KeepWorkspace
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Drawing;
using System.Drawing.Imaging;
public static class QdmUiSmokeNative {
  public const string ExpectedMainTitle = "QDM \u2014 QUEEN Download Manager";
  public const string ExpectedSettingsTitle = "\u0e15\u0e31\u0e49\u0e07\u0e04\u0e48\u0e32 QDM";
  public delegate bool EnumWindowProc(IntPtr hwnd, IntPtr parameter);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowProc callback, IntPtr parameter);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr hwnd, StringBuilder text, int capacity);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int capacity);
  [DllImport("user32.dll")] public static extern int GetWindowTextLengthW(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr parent, int id);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
  [DllImport("user32.dll", EntryPoint="SendMessageW")] public static extern IntPtr Send(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
  [DllImport("user32.dll", EntryPoint="SendMessageW", CharSet=CharSet.Unicode)] public static extern IntPtr SendText(IntPtr hwnd, uint message, IntPtr wparam, string text);
  [DllImport("user32.dll", EntryPoint="SendMessageW", CharSet=CharSet.Unicode)] public static extern IntPtr ReceiveText(IntPtr hwnd, uint message, IntPtr wparam, StringBuilder text);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hwnd, int x, int y, int width, int height, bool repaint);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr deviceContext, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  public static IntPtr Find(int processId, string className) {
    IntPtr found = IntPtr.Zero;
    EnumWindows(delegate(IntPtr hwnd, IntPtr parameter) {
      uint actual; GetWindowThreadProcessId(hwnd, out actual);
      if (actual != (uint)processId) return true;
      StringBuilder text = new StringBuilder(256); GetClassNameW(hwnd, text, text.Capacity);
      if (text.ToString() == className) { found = hwnd; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
  public static string Text(IntPtr hwnd) {
    int length = Math.Min(32768, Send(hwnd, 0x000e, IntPtr.Zero, IntPtr.Zero).ToInt32());
    StringBuilder text = new StringBuilder(Math.Max(1, length + 1));
    ReceiveText(hwnd, 0x000d, new IntPtr(text.Capacity), text);
    return text.ToString();
  }
  public static string Caption(IntPtr hwnd) {
    int length = Math.Min(32768, GetWindowTextLengthW(hwnd));
    StringBuilder text = new StringBuilder(Math.Max(1, length + 1));
    GetWindowTextW(hwnd, text, text.Capacity);
    return text.ToString();
  }
  public static string ComboItem(IntPtr hwnd, int index) {
    int length = Send(hwnd, 0x0149, new IntPtr(index), IntPtr.Zero).ToInt32();
    if (length < 0) return null;
    StringBuilder text = new StringBuilder(length + 1);
    ReceiveText(hwnd, 0x0148, new IntPtr(index), text);
    return text.ToString();
  }
  public static bool Capture(IntPtr hwnd, string path) {
    Rect rect; if (!GetWindowRect(hwnd, out rect)) return false;
    using (Bitmap bitmap = new Bitmap(Math.Max(1, rect.Right - rect.Left), Math.Max(1, rect.Bottom - rect.Top))) {
      using (Graphics graphics = Graphics.FromImage(bitmap)) {
        IntPtr dc = graphics.GetHdc(); bool result;
        try { result = PrintWindow(hwnd, dc, 2); }
        finally { graphics.ReleaseHdc(dc); }
        if (!result) return false;
      }
      bitmap.Save(path, ImageFormat.Png);
    }
    return true;
  }
}
'@ -ReferencedAssemblies System.Drawing

# Match the native application's physical-pixel coordinates. Without this,
# an unaware PowerShell host can receive virtualized rectangles at 125% DPI
# and crop PrintWindow captures even though the application is laid out well.
[void][QdmUiSmokeNative]::SetThreadDpiAwarenessContext([IntPtr](-4))

$Binary = [IO.Path]::GetFullPath($Binary)
$NodeBinary = [IO.Path]::GetFullPath($NodeBinary)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
$workspace = Join-Path ([IO.Path]::GetTempPath()) ('qdm-native-ui-' + [Guid]::NewGuid().ToString('N'))
$dataDirectory = Join-Path $workspace 'state'
$downloadDirectory = Join-Path $workspace 'downloads'
$statePath = Join-Path $dataDirectory 'state-v2.json'
[IO.Directory]::CreateDirectory($downloadDirectory) | Out-Null
$steps = [System.Collections.Generic.List[object]]::new()
$launches = [System.Collections.Generic.List[object]]::new()
$ownedProcesses = [System.Collections.Generic.List[Diagnostics.Process]]::new()
$fixtureProcess = $null
$appProcess = $null
$mainWindow = [IntPtr]::Zero
$report = [ordered]@{
  schema = 1
  suite = 'QDM native Windows UI smoke'
  binary = $Binary
  binarySha256 = (Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.ToLowerInvariant()
  startedAt = [DateTime]::UtcNow.ToString('o')
  isolatedWorkspace = $workspace
  dataDirectory = $dataDirectory
  downloadDirectory = $downloadDirectory
  launches = $launches
  steps = $steps
}

function Wait-Until([scriptblock]$Read, [string]$Description, [int]$TimeoutMilliseconds = 20000) {
  $watch = [Diagnostics.Stopwatch]::StartNew()
  while ($watch.ElapsedMilliseconds -lt $TimeoutMilliseconds) {
    $value = & $Read
    if ($value) { return $value }
    if ($null -ne $appProcess -and $appProcess.HasExited) { throw "QDM exited while waiting for $Description (code $($appProcess.ExitCode))." }
    Start-Sleep -Milliseconds 80
  }
  throw "Timed out waiting for $Description."
}
function Assert-True([bool]$Condition, [string]$Message) { if (!$Condition) { throw $Message } }
function Assert-OwnedWindow([IntPtr]$Handle) {
  [uint32]$actualProcessId = 0
  [void][QdmUiSmokeNative]::GetWindowThreadProcessId($Handle, [ref]$actualProcessId)
  Assert-True ($actualProcessId -eq $appProcess.Id) "Window $Handle belongs to process $actualProcessId instead of the owned QDM process $($appProcess.Id)."
}
function Same-Path([string]$First, [string]$Second) {
  return [StringComparer]::OrdinalIgnoreCase.Equals([IO.Path]::GetFullPath($First).TrimEnd('\'), [IO.Path]::GetFullPath($Second).TrimEnd('\'))
}
function Read-IsolatedState {
  if (!(Test-Path -LiteralPath $statePath)) { return $null }
  try { return Get-Content -LiteralPath $statePath -Raw -Encoding UTF8 | ConvertFrom-Json } catch { return $null }
}
function Step-Passed([string]$Name, $Detail = $null) {
  $steps.Add([pscustomobject]@{ name = $Name; status = 'passed'; utc = [DateTime]::UtcNow.ToString('o'); detail = $Detail })
  Write-Output "PASS $Name"
}
function Start-Owned([string]$Executable, [string[]]$Arguments, [bool]$InputPipe = $false) {
  $process = [Diagnostics.Process]::new()
  $process.StartInfo.FileName = $Executable
  # All arguments here are Windows paths or numeric options. Quotes are not
  # valid filename characters; reject them rather than invoking a shell.
  foreach ($argument in $Arguments) { if ($argument.Contains('"')) { throw 'Unexpected quote inside a process argument.' } }
  $process.StartInfo.Arguments = (($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' ')
  $process.StartInfo.UseShellExecute = $false
  $process.StartInfo.RedirectStandardInput = $InputPipe
  $process.StartInfo.RedirectStandardOutput = $true
  $process.StartInfo.RedirectStandardError = $true
  $process.StartInfo.CreateNoWindow = $true
  [void]$process.Start()
  $ownedProcesses.Add($process)
  $launches.Add([pscustomobject]@{ processId = $process.Id; executable = $Executable; arguments = @($Arguments); windowsArguments = $process.StartInfo.Arguments })
  return $process
}
function Start-Qdm {
  $script:appProcess = Start-Owned $Binary @('--data-dir', $dataDirectory, '--download-dir', $downloadDirectory)
  $script:mainWindow = Wait-Until {
    $found = [QdmUiSmokeNative]::Find($appProcess.Id, 'QDM.Native.Main.v2')
    if ($found -ne [IntPtr]::Zero -and [QdmUiSmokeNative]::IsWindowVisible($found)) { $found }
  } 'owned QDM main window'
  Assert-OwnedWindow $mainWindow
  $caption = [QdmUiSmokeNative]::Caption($mainWindow)
  Assert-True ($caption -ceq [QdmUiSmokeNative]::ExpectedMainTitle) "Main caption is '$caption' instead of the QDM product title."
  $appProcess.Refresh()
  Assert-True ($appProcess.MainWindowTitle -ceq [QdmUiSmokeNative]::ExpectedMainTitle) 'The process MainWindowTitle does not expose the QDM product title.'
  [void](Wait-Until { Test-Path -LiteralPath (Join-Path $dataDirectory 'parts-v2') -PathType Container } 'engine-created isolated profile directory')
  [void][QdmUiSmokeNative]::SetForegroundWindow($mainWindow)
}
function Control([int]$Id, [IntPtr]$Parent = $mainWindow) {
  Assert-OwnedWindow $Parent
  $handle = [QdmUiSmokeNative]::GetDlgItem($Parent, $Id)
  if ($handle -eq [IntPtr]::Zero) { throw "Missing native control $Id." }
  Assert-OwnedWindow $handle
  return $handle
}
function Click-Control([int]$Id, [IntPtr]$Parent = $mainWindow) {
  $handle = Control $Id $Parent
  Assert-True ([QdmUiSmokeNative]::IsWindowEnabled($handle)) "Control $Id is disabled."
  [void][QdmUiSmokeNative]::PostMessageW($handle, 0x00f5, [IntPtr]::Zero, [IntPtr]::Zero)
}
function Open-Settings {
  [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0111, [IntPtr]1101, [IntPtr]::Zero)
  $settingsHandle = Wait-Until {
    $handle = [QdmUiSmokeNative]::Find($appProcess.Id, 'QDM.Native.Dialog.v2')
    if ($handle -ne [IntPtr]::Zero -and [QdmUiSmokeNative]::IsWindowVisible($handle)) { $handle }
  } 'Settings window'
  Assert-OwnedWindow $settingsHandle
  $caption = [QdmUiSmokeNative]::Caption($settingsHandle)
  Assert-True ($caption -ceq [QdmUiSmokeNative]::ExpectedSettingsTitle) "Settings caption is '$caption' instead of its expected title."
  $script:lastSettingsCaption = $caption
  return $settingsHandle
}
function Close-Settings([IntPtr]$SettingsWindow, [bool]$Save = $false) {
  if ($Save) { Click-Control 1201 $SettingsWindow } else { Click-Control 1202 $SettingsWindow }
  [void](Wait-Until { ![QdmUiSmokeNative]::IsWindow($SettingsWindow) } 'Settings close')
}
function Select-FirstRow {
  $list = Control 1004
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($list)
  # Common-control accessibility differs between Windows/provider versions.
  # Discover the actual selection pattern rather than requiring one row type.
  $elements = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
  foreach ($item in $elements) {
    try {
      $pattern = $null
      if ($item.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) {
        $pattern.Select()
        if ([QdmUiSmokeNative]::Send($list, 0x100c, [IntPtr](-1), [IntPtr]2).ToInt32() -ge 0) {
          return ('UIA SelectionItem: ' + $item.Current.Name)
        }
      }
    } catch {}
  }
  # Send navigation to this owned ListView only, without global SendKeys or
  # remote-process memory writes. Verify actual native selection afterwards.
  [void][QdmUiSmokeNative]::SetForegroundWindow($mainWindow)
  try { $root.SetFocus() } catch {}
  [void][QdmUiSmokeNative]::PostMessageW($list, 0x0100, [IntPtr]0x24, [IntPtr]1)
  [void][QdmUiSmokeNative]::PostMessageW($list, 0x0101, [IntPtr]0x24, [IntPtr]1)
  [void](Wait-Until {
    [QdmUiSmokeNative]::Send($list, 0x100c, [IntPtr](-1), [IntPtr]2).ToInt32() -ge 0
  } 'first native ListView row selection' 3000)
  $selected = [QdmUiSmokeNative]::Send($list, 0x100c, [IntPtr](-1), [IntPtr]2).ToInt32()
  return ('native ListView Home; selected row ' + $selected)
}
function Progress-Value { return [QdmUiSmokeNative]::Send((Control 1020), 0x0408, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32() }
function Capture-Window([string]$Name, [IntPtr]$Handle = $mainWindow) {
  $target = Join-Path $OutputDirectory ($Name + '.png')
  if ([QdmUiSmokeNative]::Capture($Handle, $target)) { return $target }
  return $null
}
function Exit-Qdm {
  [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0111, [IntPtr]1106, [IntPtr]::Zero)
  if (!$appProcess.WaitForExit(15000)) { throw 'Exit did not terminate QDM within 15 seconds.' }
  Assert-True ($appProcess.ExitCode -eq 0) "QDM exited with code $($appProcess.ExitCode)."
  $script:mainWindow = [IntPtr]::Zero
}

try {
  $fixtureInfoPath = Join-Path $OutputDirectory 'ui-fixture-info.json'
  if (Test-Path -LiteralPath $fixtureInfoPath) { Remove-Item -LiteralPath $fixtureInfoPath }
  $fixtureProcess = Start-Owned $NodeBinary @((Join-Path $PSScriptRoot 'serve-fixture.cjs'), '--size-mib', '64', '--delay-ms', '150', '--chunk-kib', '16', '--info', $fixtureInfoPath, '--report', (Join-Path $OutputDirectory 'ui-fixture-report.json')) $true
  $fixtureInfo = Wait-Until {
    if ($fixtureProcess.HasExited) { throw "HTTP fixture exited: $($fixtureProcess.StandardError.ReadToEnd())" }
    if (Test-Path -LiteralPath $fixtureInfoPath) { try { Get-Content -LiteralPath $fixtureInfoPath -Raw -Encoding UTF8 | ConvertFrom-Json } catch {} }
  } 'local HTTP fixture'

  Start-Qdm
  $controls = @(1001,1002,1003,1004,1005,1006,1007,1008,1009,1010) | ForEach-Object {
    $handle = Control $_
    [pscustomobject]@{ id = $_; label = [QdmUiSmokeNative]::Text($handle); visible = [QdmUiSmokeNative]::IsWindowVisible($handle) }
  }
  Assert-True (@($controls | Where-Object { !$_.visible }).Count -eq 0) 'A main control is hidden at launch.'
  Step-Passed 'native main window has product caption and compact controls' @{ processId = $appProcess.Id; caption = [QdmUiSmokeNative]::Caption($mainWindow); processMainWindowTitle = $appProcess.MainWindowTitle; controls = $controls; screenshot = (Capture-Window '01-main') }

  $dpi = [QdmUiSmokeNative]::GetDpiForWindow($mainWindow)
  $resizeWidth = [int][Math]::Round(900 * $dpi / 96)
  $resizeHeight = [int][Math]::Round(540 * $dpi / 96)
  Assert-True ([QdmUiSmokeNative]::MoveWindow($mainWindow, 100, 100, $resizeWidth, $resizeHeight, $true)) 'The main window did not resize.'
  Start-Sleep -Milliseconds 150
  $outer = [QdmUiSmokeNative+Rect]::new()
  [void][QdmUiSmokeNative]::GetWindowRect($mainWindow, [ref]$outer)
  foreach ($controlId in @(1001,1002,1003,1004,1005,1006,1007,1008,1009,1010,1020,1021,1022)) {
    $childRect = [QdmUiSmokeNative+Rect]::new()
    [void][QdmUiSmokeNative]::GetWindowRect((Control $controlId), [ref]$childRect)
    Assert-True ($childRect.Left -ge $outer.Left -and $childRect.Top -ge $outer.Top -and $childRect.Right -le $outer.Right -and $childRect.Bottom -le $outer.Bottom) "Native control $controlId extends outside the resized window."
  }
  Step-Passed 'window resizes and controls remain within its bounds' @{ dpi = $dpi; width = $resizeWidth; height = $resizeHeight; screenshot = (Capture-Window '02-resized') }

  # Exercise Windows' own modal sizing loop, the same path used by the title
  # bar/System menu. Nested WM_SIZE notifications must still reach the app.
  $beforeSystem = [QdmUiSmokeNative+Rect]::new()
  $listBeforeSystem = [QdmUiSmokeNative+Rect]::new()
  [void][QdmUiSmokeNative]::GetWindowRect($mainWindow, [ref]$beforeSystem)
  [void][QdmUiSmokeNative]::GetWindowRect((Control 1004), [ref]$listBeforeSystem)
  [void][QdmUiSmokeNative]::SetForegroundWindow($mainWindow)
  [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0112, [IntPtr]0xf008, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 150
  for ($index = 0; $index -lt 6; $index++) {
    [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0100, [IntPtr]0x27, [IntPtr]1)
    [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0101, [IntPtr]0x27, [IntPtr]1)
  }
  for ($index = 0; $index -lt 4; $index++) {
    [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0100, [IntPtr]0x28, [IntPtr]1)
    [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0101, [IntPtr]0x28, [IntPtr]1)
  }
  [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0100, [IntPtr]0x0d, [IntPtr]1)
  [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0101, [IntPtr]0x0d, [IntPtr]1)
  Start-Sleep -Milliseconds 200
  [void](Wait-Until {
    $rect = [QdmUiSmokeNative+Rect]::new()
    [void][QdmUiSmokeNative]::GetWindowRect($mainWindow, [ref]$rect)
    ($rect.Right - $rect.Left) -gt ($beforeSystem.Right - $beforeSystem.Left)
  } 'system sizing changes the window width' 3000)
  $afterSystem = [QdmUiSmokeNative+Rect]::new()
  $listAfterSystem = [QdmUiSmokeNative+Rect]::new()
  [void][QdmUiSmokeNative]::GetWindowRect($mainWindow, [ref]$afterSystem)
  [void][QdmUiSmokeNative]::GetWindowRect((Control 1004), [ref]$listAfterSystem)
  $outerWidthChange = ($afterSystem.Right - $afterSystem.Left) - ($beforeSystem.Right - $beforeSystem.Left)
  $listWidthChange = ($listAfterSystem.Right - $listAfterSystem.Left) - ($listBeforeSystem.Right - $listBeforeSystem.Left)
  Assert-True ([Math]::Abs($outerWidthChange - $listWidthChange) -le 3) 'System resizing did not update the download list layout.'
  Step-Passed 'system sizing loop updates native control layout' @{ outerWidthChange = $outerWidthChange; listWidthChange = $listWidthChange; screenshot = (Capture-Window '02b-system-resized') }

  $settingsWindow = Open-Settings
  $combo = Control 1302 $settingsWindow
  Assert-True ([QdmUiSmokeNative]::Text($combo).StartsWith('32 ')) 'Fresh settings did not default to 32 connections.'
  Assert-True (Same-Path ([QdmUiSmokeNative]::Text((Control 1309 $settingsWindow))) $downloadDirectory) 'The GUI does not point at the isolated download directory; refusing to change its settings.'
  $count = [QdmUiSmokeNative]::Send($combo, 0x0146, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32()
  $selectedIndex = -1
  $choices = @()
  for ($index = 0; $index -lt $count; $index++) {
    $text = [QdmUiSmokeNative]::ComboItem($combo, $index)
    $choices += $text
    if ($text.StartsWith('64 ')) { $selectedIndex = $index }
  }
  Assert-True ($selectedIndex -ge 0) 'Settings do not offer 64 connections.'
  [void][QdmUiSmokeNative]::Send($combo, 0x014e, [IntPtr]$selectedIndex, [IntPtr]::Zero)
  [void][QdmUiSmokeNative]::SendText((Control 1304 $settingsWindow), 0x000c, [IntPtr]::Zero, '1')
  [void][QdmUiSmokeNative]::Send((Control 1310 $settingsWindow), 0x00f1, [IntPtr]1, [IntPtr]::Zero)
  $settingsImage = Capture-Window '03-settings-64' $settingsWindow
  Close-Settings $settingsWindow $true
  [void](Wait-Until { [QdmUiSmokeNative]::Text((Control 1022)).Contains('64') } '64-connection setting reaches the main snapshot')
  $settingsWindow = Open-Settings
  Assert-True ([QdmUiSmokeNative]::Text((Control 1302 $settingsWindow)).StartsWith('64 ')) 'Saved connection selection did not reach the UI.'
  Close-Settings $settingsWindow
  $savedSettings = Wait-Until {
    $state = Read-IsolatedState
    if ($null -ne $state -and $state.settings.connections -eq 64 -and (Same-Path $state.settings.download_dir $downloadDirectory)) { $state.settings }
  } '64-connection settings persisted in the isolated profile'
  $report.profileIsolation = [ordered]@{ statePath = $statePath; settingsConnections = $savedSettings.connections; settingsDownloadDirectory = $savedSettings.download_dir; ownedProcessId = $appProcess.Id }
  Step-Passed 'select and save 64 connections in the isolated profile' @{ caption = $lastSettingsCaption; choices = $choices; statePath = $statePath; screenshot = $settingsImage }

  [void][QdmUiSmokeNative]::SendText((Control 1001), 0x000c, [IntPtr]::Zero, $fixtureInfo.url)
  Click-Control 1002
  [void](Wait-Until { [QdmUiSmokeNative]::Send((Control 1004), 0x1004, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32() -eq 1 } 'download row')
  $rowName = Select-FirstRow
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowEnabled((Control 1005)) -and (Progress-Value) -gt 100 } 'visible transfer progress')
  $savedTask = Wait-Until {
    $state = Read-IsolatedState
    if ($null -ne $state) {
      foreach ($task in $state.tasks) {
        if ($task.view.url -eq $fixtureInfo.url -and (Same-Path $task.view.directory $downloadDirectory)) { return $task.view }
      }
    }
  } 'GUI-created job stored in the isolated state file'
  $report.profileIsolation.savedTaskId = $savedTask.id
  $report.profileIsolation.savedTaskDirectory = $savedTask.directory
  Step-Passed 'add through URL field and store the job in the isolated profile' @{ row = $rowName; savedTaskId = $savedTask.id; statePath = $statePath; progress = (Progress-Value); screenshot = (Capture-Window '04-downloading') }

  $progressBeforeDialog = Progress-Value
  $settingsWindow = Open-Settings
  Start-Sleep -Milliseconds 400
  [void][QdmUiSmokeNative]::PostMessageW($settingsWindow, 0x0112, [IntPtr]0xf060, [IntPtr]::Zero)
  [void](Wait-Until { ![QdmUiSmokeNative]::IsWindow($settingsWindow) } 'Settings system Close command')
  Assert-True ([QdmUiSmokeNative]::IsWindowEnabled($mainWindow)) 'Closing Settings through its title bar left the main window disabled.'
  [void](Wait-Until { (Progress-Value) -gt $progressBeforeDialog } 'progress after closing Settings during download')
  Step-Passed 'Settings system Close preserves progress and parent interaction' @{ before = $progressBeforeDialog; after = (Progress-Value) }

  Click-Control 1005
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowEnabled((Control 1006)) -and ![QdmUiSmokeNative]::IsWindowEnabled((Control 1005)) } 'paused UI state')
  Start-Sleep -Milliseconds 300
  $pausedProgress = Progress-Value
  Start-Sleep -Milliseconds 500
  Assert-True ([Math]::Abs((Progress-Value) - $pausedProgress) -le 50) 'Progress kept advancing after Pause.'
  Step-Passed 'pause stops progress and enables Resume' @{ progress = $pausedProgress; screenshot = (Capture-Window '05-paused') }

  [void][QdmUiSmokeNative]::PostMessageW($mainWindow, 0x0112, [IntPtr]0xf060, [IntPtr]::Zero)
  [void](Wait-Until { ![QdmUiSmokeNative]::IsWindowVisible($mainWindow) } 'close-to-tray')
  Assert-True (!$appProcess.HasExited) 'Close-to-tray exited the process.'
  $sameProfile = Start-Owned $Binary @('--data-dir', $dataDirectory, '--download-dir', $downloadDirectory)
  Assert-True ($sameProfile.WaitForExit(10000)) 'Second launch did not exit after restoring the first instance.'
  Assert-True ($sameProfile.ExitCode -eq 0) 'Second launch failed.'
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowVisible($mainWindow) } 'second launch restores the owned window')
  Assert-True ([QdmUiSmokeNative]::IsWindowEnabled((Control 1006))) 'Restoring the window lost the paused row selection.'
  Step-Passed 'close hides to tray and second launch restores same instance' @{ originalProcessId = $appProcess.Id; secondProcessId = $sameProfile.Id }

  $fixtureProcess.StandardInput.WriteLine('{"op":"behavior","values":{"delayMs":1}}')
  $fixtureProcess.StandardInput.Flush()
  Click-Control 1006
  $mergeStage = Wait-Until {
    $state = Read-IsolatedState
    if ($null -eq $state) { return $null }
    $activeJob = @($state.tasks | Where-Object { $_.view.id -eq $savedTask.id } | Select-Object -First 1)
    if ($activeJob.Count -eq 0) { return $null }
    $view = $activeJob[0].view
    if ($view.status -ne 'assembling' -or $view.assembly_total_bytes -le 0) { return $null }
    $progress = Progress-Value
    if ($view.assembly_bytes -gt 0 -and $view.assembly_bytes -lt $view.assembly_total_bytes -and $progress -gt 0 -and $progress -lt 10000) {
      return @{ bytes = $view.assembly_bytes; total = $view.assembly_total_bytes; progress = $progress }
    }
  } 'visible merge progress separate from completed download' 45000
  Step-Passed 'physical file assembly has its own progress bar' @{
      bytes = $mergeStage.bytes; total = $mergeStage.total;
      progress = $mergeStage.progress; screenshot = (Capture-Window '06a-assembling')
  }
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowEnabled((Control 1007)) } 'completed download Open action' 60000)
  $downloadPath = Join-Path $downloadDirectory $fixtureInfo.filename
  Assert-True (Test-Path -LiteralPath $downloadPath) 'Completed UI did not publish the expected file.'
  $actualHash = (Get-FileHash -LiteralPath $downloadPath -Algorithm SHA256).Hash.ToLowerInvariant()
  Assert-True ($actualHash -eq $fixtureInfo.sha256) 'The GUI download differs from independent fixture bytes.'
  Assert-True ((Get-Item -LiteralPath $downloadPath).Length -eq $fixtureInfo.sizeBytes) 'The GUI download has the wrong length.'
  Step-Passed 'resume completes exact file and enables Open' @{ bytes = $fixtureInfo.sizeBytes; sha256 = $actualHash; screenshot = (Capture-Window '06-completed') }

  Exit-Qdm
  $completedState = Read-IsolatedState
  $persistedTask = @($completedState.tasks | Where-Object { $_.view.id -eq $savedTask.id })
  Assert-True ($persistedTask.Count -eq 1 -and $persistedTask[0].view.status -eq 'completed') 'The completed GUI task was not persisted in the isolated profile.'
  Assert-True ($persistedTask[0].view.sha256 -eq $actualHash) 'The isolated completed record has the wrong hash.'
  $report.profileIsolation.completedTaskStatus = $persistedTask[0].view.status
  $report.profileIsolation.completedTaskSha256 = $persistedTask[0].view.sha256
  $report.profileIsolation.completedStateSha256 = (Get-FileHash -LiteralPath $statePath -Algorithm SHA256).Hash.ToLowerInvariant()
  Step-Passed 'Exit terminates cleanly'
  Start-Qdm
  $settingsWindow = Open-Settings
  Assert-True ([QdmUiSmokeNative]::Text((Control 1302 $settingsWindow)).StartsWith('64 ')) '64-connection setting did not survive a real process restart.'
  Close-Settings $settingsWindow
  Assert-True ([QdmUiSmokeNative]::Send((Control 1004), 0x1004, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32() -eq 1) 'Completed task disappeared after restart.'
  [void](Select-FirstRow)
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowEnabled((Control 1007)) } 'restored completed row')
  Step-Passed 'settings and completed row persist after process restart' @{ screenshot = (Capture-Window '07-restarted') }

  Click-Control 1010
  [void](Wait-Until { [QdmUiSmokeNative]::Send((Control 1004), 0x1004, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32() -eq 0 } 'Remove clears the row')
  Assert-True (Test-Path -LiteralPath $downloadPath) 'Remove deleted the completed user file.'
  Step-Passed 'Remove clears history while preserving downloaded file'
  Exit-Qdm
  $report.status = 'passed'
} catch {
  $report.status = 'failed'
  $report.error = $_.Exception.Message
  $report.stack = $_.ScriptStackTrace
  if ($mainWindow -ne [IntPtr]::Zero -and [QdmUiSmokeNative]::IsWindow($mainWindow)) {
    $report.failureScreenshot = Capture-Window 'failure'
    try {
      $root = [System.Windows.Automation.AutomationElement]::FromHandle($mainWindow)
      $elements = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
      $report.accessibility = @($elements | ForEach-Object { [pscustomobject]@{
        name = $_.Current.Name; class = $_.Current.ClassName; controlType = $_.Current.ControlType.ProgrammaticName
        enabled = $_.Current.IsEnabled; patterns = @($_.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName })
      } })
    } catch {}
  }
  Write-Output "FAIL $($report.error)"
} finally {
  foreach ($process in $ownedProcesses) {
    if ($null -ne $fixtureProcess -and $process.Id -eq $fixtureProcess.Id) { continue }
    if (!$process.HasExited) {
      $window = [QdmUiSmokeNative]::Find($process.Id, 'QDM.Native.Main.v2')
      if ($window -ne [IntPtr]::Zero) { [void][QdmUiSmokeNative]::PostMessageW($window, 0x0111, [IntPtr]1106, [IntPtr]::Zero) }
      if (!$process.WaitForExit(5000)) { $process.Kill(); [void]$process.WaitForExit(5000) }
    }
  }
  if ($null -ne $fixtureProcess -and !$fixtureProcess.HasExited) {
    try { $fixtureProcess.StandardInput.WriteLine('{"op":"shutdown"}'); $fixtureProcess.StandardInput.Close() } catch {}
    if (!$fixtureProcess.WaitForExit(5000)) { $fixtureProcess.Kill(); [void]$fixtureProcess.WaitForExit(5000) }
  }
  $report.processErrors = @($ownedProcesses | ForEach-Object {
    if ($_.HasExited) {
      $stderr = $_.StandardError.ReadToEnd()
      if ($stderr) { [pscustomobject]@{ processId = $_.Id; stderr = $stderr } }
    }
  })
  $report.finishedAt = [DateTime]::UtcNow.ToString('o')
  $report.passedSteps = $steps.Count
  $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'native-ui-smoke.json') -Encoding utf8
  if (!$KeepWorkspace -and $report.status -eq 'passed') { Remove-Item -LiteralPath $workspace -Recurse -Force }
}

if ($report.status -ne 'passed') { exit 1 }
Write-Output "RESULT $($steps.Count) UI checks passed"
