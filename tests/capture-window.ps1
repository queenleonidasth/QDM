param([Parameter(Mandatory=$true)][int]$TargetProcessId,[Parameter(Mandatory=$true)][string]$OutputPath)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public static class QdmOwnedWindowCapture {
 [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left,Top,Right,Bottom; }
 [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hWnd,out RECT rect);
 [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hWnd,IntPtr hdc,uint flags);
 [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
 public static string Save(IntPtr h,string path){SetThreadDpiAwarenessContext(new IntPtr(-4));RECT r;if(!GetWindowRect(h,out r))throw new Exception("Window rectangle unavailable");using(var b=new Bitmap(r.Right-r.Left,r.Bottom-r.Top)){using(var g=Graphics.FromImage(b)){var dc=g.GetHdc();bool ok=PrintWindow(h,dc,2);g.ReleaseHdc(dc);if(!ok)throw new Exception("PrintWindow failed");}b.Save(path,ImageFormat.Png);return b.Width+"x"+b.Height;}}
}
'@ -ReferencedAssemblies System.Drawing
$qdmCaptureProcess=Get-Process -Id $TargetProcessId -ErrorAction Stop
if($qdmCaptureProcess.MainWindowHandle -eq [IntPtr]::Zero){throw 'Owned process has no main window.'}
$OutputPath=[IO.Path]::GetFullPath($OutputPath)
New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($OutputPath))|Out-Null
$qdmCaptureSize=[QdmOwnedWindowCapture]::Save($qdmCaptureProcess.MainWindowHandle,$OutputPath)
[pscustomobject]@{processId=$TargetProcessId;title=$qdmCaptureProcess.MainWindowTitle;path=$OutputPath;size=$qdmCaptureSize;method='PrintWindow(PW_RENDERFULLCONTENT) of the exact owned window; no foreground change'}|ConvertTo-Json -Compress
