$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class QdmPreview {
 [StructLayout(LayoutKind.Sequential)]
 public struct RECT { public int L,T,R,B; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h,IntPtr dc,uint flags);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p,IntPtr after,string cls,string text);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
}
'@ -ReferencedAssemblies System.Drawing
[void][QdmPreview]::SetThreadDpiAwarenessContext([IntPtr](-4))
$proc=Get-CimInstance Win32_Process -Filter "Name = 'QDM.exe'"|
 Where-Object {$_.ExecutablePath -ieq (Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe')}|
 Select-Object -First 1
if(!$proc){throw 'Live installed QDM not running'}
$window=(Get-Process -Id ([int]$proc.ProcessId)).MainWindowHandle
if(!$window){throw 'No main window'}
$lanes=[QdmPreview]::FindWindowEx([IntPtr]$window,[IntPtr]::Zero,'QDM.Native.LaneGrid.v1',$null)
if($lanes -eq [IntPtr]::Zero){throw 'Main window connection panel is missing'}
$rc=New-Object QdmPreview+RECT
[void][QdmPreview]::GetClientRect($lanes,[ref]$rc)
if(($rc.R - $rc.L) -lt 500 -or ($rc.B-$rc.T) -lt 70){throw 'Lane panel is too small or hidden'}
'LANE_PANEL_VISIBLE='+[QdmPreview]::IsWindowVisible($lanes)
'LANE_PANEL_SIZE='+($rc.R-$rc.L)+'x'+($rc.B-$rc.T)
$bounds=New-Object QdmPreview+RECT
[void][QdmPreview]::GetWindowRect($window,[ref]$bounds)
$width=($bounds.R-$bounds.L)
$height=($bounds.B-$bounds.T)
if($width -lt 650 -or $height -lt 400){throw 'Invalid window dimensions'}
$out='C:\Users\QUEEN\Projects\QDM\native\docs\evidence\main-lanes-live.png'
$bitmap=New-Object System.Drawing.Bitmap($width,$height)
try{
 $g=[System.Drawing.Graphics]::FromImage($bitmap)
 try{
   $dc=$g.GetHdc()
   try{$ok=[QdmPreview]::PrintWindow([IntPtr]$window,$dc,2)}
   finally{$g.ReleaseHdc($dc)}
 }finally{$g.Dispose()}
 if(!$ok){throw 'PrintWindow failed'}
 $bitmap.Save($out,[System.Drawing.Imaging.ImageFormat]::Png)
 'LIVE_PREVIEW='+$out
}finally{$bitmap.Dispose()}
