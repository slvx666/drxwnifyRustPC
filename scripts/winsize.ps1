# Developer checks: moves and sizes the Mini window without activating it (e.g. to the owner's 1920x1080 layout on
# the second monitor). Usage: scripts\winsize.ps1 -X 1920 -Y 0 -W 1920 -H 1040
param([int]$X = 1920, [int]$Y = 0, [int]$W = 1920, [int]$H = 1040, [int]$WinPid = [int]("0" + $env:DRX_WIN_PID))
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DrxSize {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string c, string t);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    // The window with this title; with pid > 0 only that process's (two Minis with different profiles share the title).
    public static IntPtr Find(string title, uint pid) {
        IntPtr w = IntPtr.Zero;
        while (true) {
            w = FindWindowEx(IntPtr.Zero, w, null, title);
            if (w == IntPtr.Zero || pid == 0) return w;
            uint p; GetWindowThreadProcessId(w, out p);
            if (p == pid) return w;
        }
    }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
}
"@
# By title (not $h: PowerShell names ignore case, it would overwrite -H): the process also owns winit's invisible helper window, which MainWindowHandle may pick.
$hwnd = [DrxSize]::Find("Drxwnify Mini", [uint32]$WinPid)
if ($hwnd -eq [IntPtr]::Zero) { Write-Error "Mini window not found"; exit 1 }
# SWP_NOZORDER | SWP_NOACTIVATE
[void][DrxSize]::SetWindowPos($hwnd, [IntPtr]::Zero, $X, $Y, $W, $H, 0x0004 -bor 0x0010)
