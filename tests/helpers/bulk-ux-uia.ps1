param([Parameter(Mandatory=$true)][int]$TargetProcessId)
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class QdmMotionWindow {
 [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left,Top,Right,Bottom; }
 [StructLayout(LayoutKind.Sequential)] public struct MonitorInfo { public uint Size; public Rect Monitor,Work; public uint Flags; }
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out Rect r);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h,out Rect r);
 [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr h,uint flags);
 [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern bool GetMonitorInfo(IntPtr monitor,ref MonitorInfo info);
 [DllImport("user32.dll", SetLastError=true)] public static extern bool SetWindowPos(IntPtr h,IntPtr after,int x,int y,int width,int height,uint flags);
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
 [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h,uint m,IntPtr w,IntPtr l);
}
'@
[void][QdmMotionWindow]::SetThreadDpiAwarenessContext([IntPtr](-4))
$qdmMotionHandle = [IntPtr]::Zero

function Get-QdmMotionWindow {
  $watch = [Diagnostics.Stopwatch]::StartNew()
  do {
    $ownedProcess = Get-Process -Id $TargetProcessId -ErrorAction Stop
    $ownedProcess.Refresh()
    if ($ownedProcess.MainWindowHandle -ne [IntPtr]::Zero -and $ownedProcess.MainWindowTitle -like 'QDM*QUEEN Download Manager*') {
      $script:qdmMotionHandle = $ownedProcess.MainWindowHandle
      return $ownedProcess
    }
    Start-Sleep -Milliseconds 60
  } while ($watch.ElapsedMilliseconds -lt 12000)
  throw 'The owned QDM product window was not found.'
}

function Assert-QdmMotionOwner {
  if ($qdmMotionHandle -eq [IntPtr]::Zero) { [void](Get-QdmMotionWindow) }
  $windowProcessId = [uint32]0
  [void][QdmMotionWindow]::GetWindowThreadProcessId($qdmMotionHandle,[ref]$windowProcessId)
  if ($windowProcessId -ne $TargetProcessId) { throw 'The capture HWND no longer belongs to the owned QDM process.' }
}

function Get-QdmMotionWorkArea {
  Assert-QdmMotionOwner
  $monitor = [QdmMotionWindow]::MonitorFromWindow($qdmMotionHandle,2)
  if ($monitor -eq [IntPtr]::Zero) { throw 'No monitor was found for the owned window.' }
  $info = New-Object QdmMotionWindow+MonitorInfo
  $info.Size = [uint32][Runtime.InteropServices.Marshal]::SizeOf($info)
  if (![QdmMotionWindow]::GetMonitorInfo($monitor,[ref]$info)) { throw 'The owned monitor work area is unavailable.' }
  return $info
}

function Convert-QdmMotionRect($Rect) {
  return [pscustomobject]@{x=$Rect.Left;y=$Rect.Top;left=$Rect.Left;top=$Rect.Top;right=$Rect.Right;bottom=$Rect.Bottom;width=($Rect.Right - $Rect.Left);height=($Rect.Bottom - $Rect.Top)}
}

function Find-QdmMotionControl([string]$Name,[string]$Type,[bool]$RequireEnabled = $true) {
  Assert-QdmMotionOwner
  $root = [Windows.Automation.AutomationElement]::FromHandle($qdmMotionHandle)
  $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$Name)
  $items = $root.FindAll([Windows.Automation.TreeScope]::Descendants,$condition)
  foreach ($item in $items) {
    try {
      if (!$item.Current.IsOffscreen -and ($Type -eq '' -or $item.Current.ControlType.ProgrammaticName -eq ('ControlType.'+$Type)) -and (!$RequireEnabled -or $item.Current.IsEnabled)) { return $item }
    } catch { }
  }
  return $null
}

function Wait-QdmMotionControl([string]$Name,[string]$Type) {
  $watch = [Diagnostics.Stopwatch]::StartNew()
  do {
    $item = Find-QdmMotionControl $Name $Type
    if ($null -ne $item) { return $item }
    Start-Sleep -Milliseconds 50
  } while ($watch.ElapsedMilliseconds -lt 4500)
  throw ('An enabled, visible owned-window control was not found: '+$Name)
}

function Get-QdmMotionInventory {
  Assert-QdmMotionOwner
  $root = [Windows.Automation.AutomationElement]::FromHandle($qdmMotionHandle)
  $items = $root.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)
  $result = [Collections.Generic.List[object]]::new()
  foreach ($item in $items) {
    try {
      if (!$item.Current.IsOffscreen -and $item.Current.Name) {
        $result.Add([pscustomobject]@{name=$item.Current.Name;type=$item.Current.ControlType.ProgrammaticName;enabled=$item.Current.IsEnabled})
      }
    } catch { }
  }
  return $result.ToArray()
}

while ($null -ne ($line = [Console]::ReadLine())) {
  if ([string]::IsNullOrWhiteSpace($line)) { continue }
  $request = $null
  $receivedAt = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  try {
    $request = $line | ConvertFrom-Json
    $result = $null
    switch ([string]$request.action) {
      'window' {
        $ownedProcess = Get-QdmMotionWindow
        Assert-QdmMotionOwner
        $rect = New-Object QdmMotionWindow+Rect
        $client = New-Object QdmMotionWindow+Rect
        if (![QdmMotionWindow]::GetWindowRect($qdmMotionHandle,[ref]$rect) -or ![QdmMotionWindow]::GetClientRect($qdmMotionHandle,[ref]$client)) { throw 'Owned-window dimensions are unavailable.' }
        $monitorInfo = Get-QdmMotionWorkArea
        $result = [pscustomobject]@{
          processId=$TargetProcessId;title=$ownedProcess.MainWindowTitle
          hwnd=$qdmMotionHandle.ToInt64().ToString();hwndHex=('0x{0:X}' -f $qdmMotionHandle.ToInt64())
          visible=[QdmMotionWindow]::IsWindowVisible($qdmMotionHandle)
          windowWidthPixels=($rect.Right - $rect.Left);windowHeightPixels=($rect.Bottom - $rect.Top)
          clientWidthPixels=($client.Right - $client.Left);clientHeightPixels=($client.Bottom - $client.Top)
          dpi=[QdmMotionWindow]::GetDpiForWindow($qdmMotionHandle)
          x=$rect.Left;y=$rect.Top;windowBounds=(Convert-QdmMotionRect $rect)
          monitorBounds=(Convert-QdmMotionRect $monitorInfo.Monitor);workArea=(Convert-QdmMotionRect $monitorInfo.Work)
          fullyInsideWorkArea=($rect.Left -ge $monitorInfo.Work.Left -and $rect.Top -ge $monitorInfo.Work.Top -and $rect.Right -le $monitorInfo.Work.Right -and $rect.Bottom -le $monitorInfo.Work.Bottom)
        }
      }
      'place_within_work_area' {
        Assert-QdmMotionOwner
        $before = New-Object QdmMotionWindow+Rect
        if (![QdmMotionWindow]::GetWindowRect($qdmMotionHandle,[ref]$before)) { throw 'Owned-window rectangle is unavailable before positioning.' }
        $monitorInfo = Get-QdmMotionWorkArea
        $area = $monitorInfo.Work
        $availableWidth = $area.Right - $area.Left
        $availableHeight = $area.Bottom - $area.Top
        if ($availableWidth -le 0 -or $availableHeight -le 0) { throw 'The monitor work area is empty.' }
        # Preserve the current size whenever it fits. A requested shrink is
        # limited to this owned test window; its actual minimum is verified.
        $width = [Math]::Min(($before.Right - $before.Left),$availableWidth)
        $height = [Math]::Min(($before.Bottom - $before.Top),$availableHeight)
        $x = [Math]::Max($area.Left,[Math]::Min($before.Left,($area.Right - $width)))
        $y = [Math]::Max($area.Top,[Math]::Min($before.Top,($area.Bottom - $height)))
        # SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER. This operation
        # neither activates nor changes the stacking order of any window.
        if (![QdmMotionWindow]::SetWindowPos($qdmMotionHandle,[IntPtr]::Zero,$x,$y,$width,$height,0x214)) { throw ('Owned-window positioning failed: '+[Runtime.InteropServices.Marshal]::GetLastWin32Error()) }
        Start-Sleep -Milliseconds 200
        Assert-QdmMotionOwner
        $after = New-Object QdmMotionWindow+Rect
        if (![QdmMotionWindow]::GetWindowRect($qdmMotionHandle,[ref]$after)) { throw 'Owned-window rectangle is unavailable after positioning.' }
        $inside = $after.Left -ge $area.Left -and $after.Top -ge $area.Top -and $after.Right -le $area.Right -and $after.Bottom -le $area.Bottom
        $result = [pscustomobject]@{
          before=(Convert-QdmMotionRect $before);after=(Convert-QdmMotionRect $after)
          workArea=(Convert-QdmMotionRect $area);monitorBounds=(Convert-QdmMotionRect $monitorInfo.Monitor)
          requested=[pscustomobject]@{x=$x;y=$y;width=$width;height=$height}
          method='SetWindowPos: NOZORDER | NOACTIVATE | NOOWNERZORDER';fullyInsideWorkArea=$inside
        }
        if (!$inside) { throw ('The owned window minimum size cannot fit inside this monitor work area. Geometry: '+($result|ConvertTo-Json -Depth 4 -Compress)) }
      }
      'invoke' {
        $item = Wait-QdmMotionControl ([string]$request.name) 'Button'
        $pattern = $null
        if (!$item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) { throw ('InvokePattern is unavailable: '+$request.name) }
        $at = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $pattern.Invoke()
        $result = [pscustomobject]@{name=$request.name;pattern='InvokePattern';invokedAtUnixMs=$at}
      }
      'toggle' {
        $item = Wait-QdmMotionControl ([string]$request.name) 'CheckBox'
        $pattern = $null
        if ($item.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)) { $pattern.Toggle(); $used='TogglePattern' } elseif ($item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) { $pattern.Invoke(); $used='InvokePattern' } else { throw ('No checkbox action available: '+$request.name) }
        $result = [pscustomobject]@{name=$request.name;pattern=$used}
      }
      'select' {
        $item = Wait-QdmMotionControl ([string]$request.name) 'ListItem'
        $pattern = $null
        $at = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        if ($item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) {
          $pattern.Invoke(); $used = 'InvokePattern'
        } elseif ($item.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern,[ref]$pattern)) {
          $pattern.Select(); $used = 'SelectionItemPattern'
        } else { throw ('No accessible selection action: '+$request.name) }
        $result = [pscustomobject]@{name=$request.name;pattern=$used;invokedAtUnixMs=$at}
      }
      'exists' {
        $type = if ($request.type) { [string]$request.type } else { 'Button' }
        $result = [pscustomobject]@{name=$request.name;exists=($null -ne (Find-QdmMotionControl ([string]$request.name) $type))}
      }
      'inventory' { $result = @(Get-QdmMotionInventory) }
      'close' {
        Assert-QdmMotionOwner
        if (![QdmMotionWindow]::PostMessageW($qdmMotionHandle,0x10,[IntPtr]::Zero,[IntPtr]::Zero)) { throw 'Could not close the owned GUI.' }
        $result = [pscustomobject]@{posted='WM_CLOSE';processId=$TargetProcessId}
      }
      default { throw ('Unsupported motion action: '+$request.action) }
    }
    [pscustomobject]@{id=$request.id;ok=$true;receivedAtUnixMs=$receivedAt;completedAtUnixMs=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds();result=$result} | ConvertTo-Json -Depth 8 -Compress | ForEach-Object { [Console]::WriteLine($_) }
  } catch {
    [pscustomobject]@{id=$request.id;ok=$false;receivedAtUnixMs=$receivedAt;completedAtUnixMs=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds();error=$_.Exception.Message} | ConvertTo-Json -Compress | ForEach-Object { [Console]::WriteLine($_) }
  }
}
