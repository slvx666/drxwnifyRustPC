# Captures one app window to a PNG, even when other windows cover it (PrintWindow), without activating it.
# Only that window is in the picture: nothing else on the owner's screen gets captured.
# Usage: scripts\shotwin.ps1 -Process drxwnify-mini -Out shot.png
param([string]$Process = "drxwnify-mini", [string]$Out = "shot.png", [switch]$Client)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DrxWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
}
"@
$p = Get-Process -Name $Process -ErrorAction Stop | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Error "no window for $Process"; exit 1 }
$h = $p.MainWindowHandle
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
