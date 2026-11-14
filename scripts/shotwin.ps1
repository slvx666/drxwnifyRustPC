# Captures one app window to a PNG, even when other windows cover it (PrintWindow), without activating it.
# Only that window is in the picture: nothing else on the owner's screen gets captured.
# Usage: scripts\shotwin.ps1 -Process drxwnify-mini -Out shot.png
param([string]$Process = "drxwnify-mini", [string]$Out = "shot.png", [switch]$Client)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DrxWin {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
}
"@
# By title: Mini also owns winit's invisible helper window, which MainWindowHandle may pick.
$h = [DrxWin]::FindWindow([NullString]::Value, "Drxwnify Mini")
if ($h -eq [IntPtr]::Zero) { Write-Error "no Mini window"; exit 1 }
$r = New-Object DrxWin+RECT
if ($Client) { [void][DrxWin]::GetClientRect($h, [ref]$r) } else { [void][DrxWin]::GetWindowRect($h, [ref]$r) }
$w = $r.R - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
# 1 = client area only, 2 = PW_RENDERFULLCONTENT (needed for windows drawn with DirectComposition).
$flags = if ($Client) { 3 } else { 2 }
[void][DrxWin]::PrintWindow($h, $dc, $flags)
$g.ReleaseHdc($dc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
"$Out ${w}x$hh"
