# Puts the Mini window above the other windows without activating it (keyboard focus stays where it is).
# Use before input.ps1 when the owner said the second monitor is free.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DrxRaise {
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint f);
}
"@
$p = Get-Process drxwnify-mini | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
# SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE. HWND_TOP alone is ignored for a window that is not in front, so it goes
# topmost and straight back to normal: it stays above the others, still inactive.
$f = 0x0001 -bor 0x0002 -bor 0x0010
[void][DrxRaise]::SetWindowPos($p.MainWindowHandle, [IntPtr](-1), 0, 0, 0, 0, $f)
[void][DrxRaise]::SetWindowPos($p.MainWindowHandle, [IntPtr](-2), 0, 0, 0, 0, $f)
