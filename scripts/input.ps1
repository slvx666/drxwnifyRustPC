# Developer checks with real input: mouse into the Mini window at client coordinates (the window may sit on the
# second monitor; the cursor moves there). Pair with shotwin.ps1 -Client. Run Mini with DRX_MUTE=1 so clicks stay silent.
param([string]$Do = "wheel", [int]$X, [int]$Y, [int]$Amount = -3)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DrxIn {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
}
"@
$p = Get-Process drxwnify-mini | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
$pt = New-Object DrxIn+POINT; $pt.X = $X; $pt.Y = $Y
[void][DrxIn]::ClientToScreen($p.MainWindowHandle, [ref]$pt)
# Never send input into the owner's windows: if anything covers Mini at that point, do nothing.
$top = [DrxIn]::GetAncestor([DrxIn]::WindowFromPoint($pt), 2)
if ($top -ne $p.MainWindowHandle) { Write-Error "Mini is covered at ($X,$Y) by another window: no input sent"; exit 2 }
[void][DrxIn]::SetCursorPos($pt.X, $pt.Y)
Start-Sleep -Milliseconds 150
switch ($Do) {
  "wheel"  { for ($i = 0; $i -lt [Math]::Abs($Amount); $i++) { [DrxIn]::mouse_event(0x0800, 0, 0, 120 * [Math]::Sign($Amount), [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 } }
  "hwheel" { for ($i = 0; $i -lt [Math]::Abs($Amount); $i++) { [DrxIn]::mouse_event(0x1000, 0, 0, 120 * [Math]::Sign($Amount), [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 } }
  "shiftwheel" { Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait("+"); for ($i = 0; $i -lt [Math]::Abs($Amount); $i++) { [DrxIn]::mouse_event(0x0800, 0, 0, 120 * [Math]::Sign($Amount), [UIntPtr]::Zero) } }
  "click"  { [DrxIn]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero); [DrxIn]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero) }
  "move"   { }
}
Start-Sleep -Milliseconds 400
