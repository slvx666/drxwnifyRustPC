# Developer checks with real input: mouse into the Mini window at client coordinates (the window may sit on the
# second monitor; the cursor moves there). Pair with shotwin.ps1 -Client. Run Mini with DRX_MUTE=1 so clicks stay silent.
param([string]$Do = "wheel", [int]$X, [int]$Y, [int]$Amount = -3, [string]$Text = "", [int]$Key = 0, [switch]$Ctrl, [int]$Dx = 0, [int]$Dy = 0, [int]$WinPid = [int]("0" + $env:DRX_WIN_PID))
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DrxIn {
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
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort Vk; public ushort Scan; public uint Flags; public uint Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Explicit, Size = 40)] public struct INPUT { [FieldOffset(0)] public uint Type; [FieldOffset(8)] public KEYBDINPUT Ki; }
    [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
    // Text as Unicode key events (independent of the keyboard layout); vk > 0 sends that virtual key instead.
    public static void Key(ushort vk, ushort ch) {
        var a = new INPUT[2];
        for (int i = 0; i < 2; i++) {
            a[i].Type = 1;
            a[i].Ki.Vk = vk; a[i].Ki.Scan = ch;
            a[i].Ki.Flags = (vk == 0 ? 4u : 0u) | (i == 1 ? 2u : 0u);
        }
        SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
    }
    static void One(ushort vk, uint flags) {
        var a = new INPUT[1]; a[0].Type = 1; a[0].Ki.Vk = vk; a[0].Ki.Flags = flags;
        SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void Down(ushort vk) { One(vk, 0); }
    public static void Up(ushort vk) { One(vk, 2); }
}
"@
# The owner is playing (Apex: r5apex, r5apex_dx12): no input at all — the cursor belongs to the game.
if (Get-Process -Name 'r5apex*' -ErrorAction SilentlyContinue) { Write-Error "Apex is running: no input sent"; exit 3 }
# By title: the process also owns winit's invisible helper window, which MainWindowHandle may pick.
$h = [DrxIn]::Find("Drxwnify Mini", [uint32]$WinPid)
if ($h -eq [IntPtr]::Zero) { Write-Error "no Mini window"; exit 1 }
$pt = New-Object DrxIn+POINT; $pt.X = $X; $pt.Y = $Y
[void][DrxIn]::ClientToScreen($h, [ref]$pt)
# Never send input into the owner's windows: if anything covers Mini at that point, do nothing.
$top = [DrxIn]::GetAncestor([DrxIn]::WindowFromPoint($pt), 2)
if ($top -ne $h) { Write-Error "Mini is covered at ($X,$Y) by another window: no input sent"; exit 2 }
[void][DrxIn]::SetCursorPos($pt.X, $pt.Y)
Start-Sleep -Milliseconds 150
switch ($Do) {
  "wheel"  { for ($i = 0; $i -lt [Math]::Abs($Amount); $i++) { [DrxIn]::mouse_event(0x0800, 0, 0, 120 * [Math]::Sign($Amount), [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 } }
  "hwheel" { for ($i = 0; $i -lt [Math]::Abs($Amount); $i++) { [DrxIn]::mouse_event(0x1000, 0, 0, 120 * [Math]::Sign($Amount), [UIntPtr]::Zero); Start-Sleep -Milliseconds 60 } }
  "shiftwheel" { Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait("+"); for ($i = 0; $i -lt [Math]::Abs($Amount); $i++) { [DrxIn]::mouse_event(0x0800, 0, 0, 120 * [Math]::Sign($Amount), [UIntPtr]::Zero) } }
  "click"  { [DrxIn]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero); [DrxIn]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero) }
  "rclick" { [DrxIn]::mouse_event(0x08, 0, 0, 0, [UIntPtr]::Zero); [DrxIn]::mouse_event(0x10, 0, 0, 0, [UIntPtr]::Zero) }
  "move"   { }
  # Press at (X,Y), move by (Dx,Dy) in steps, release (window drags from the top bar).
  "drag"   { Add-Type -AssemblyName System.Windows.Forms
             $vs = [System.Windows.Forms.SystemInformation]::VirtualScreen
             [DrxIn]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 250
             # Real (absolute, virtual desktop) moves: the system's window move loop follows input, not SetCursorPos.
             for ($k = 1; $k -le 15; $k++) {
               $ax = [int](($pt.X + $Dx * $k / 15 - $vs.Left) * 65535 / ($vs.Width - 1))
               $ay = [int](($pt.Y + $Dy * $k / 15 - $vs.Top) * 65535 / ($vs.Height - 1))
               [DrxIn]::mouse_event(0x0001 -bor 0x8000 -bor 0x4000, $ax, $ay, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
             Start-Sleep -Milliseconds 100
             [DrxIn]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero) }
  "dblclick" { 1..2 | ForEach-Object { [DrxIn]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero); [DrxIn]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 90 } }
  # Keyboard only into Mini when it is the active window (after a click into it): never into the owner's apps.
  "type"   { if ([DrxIn]::GetForegroundWindow() -ne $h) { Write-Error "Mini is not active: no keys sent"; exit 3 }
             foreach ($c in $Text.ToCharArray()) { [DrxIn]::Key(0, [uint16]$c); Start-Sleep -Milliseconds 40 } }
  "key"    { if ([DrxIn]::GetForegroundWindow() -ne $h) { Write-Error "Mini is not active: no keys sent"; exit 3 }
             if ($Ctrl) { [DrxIn]::Down(0x11) }; [DrxIn]::Key([uint16]$Key, 0); if ($Ctrl) { [DrxIn]::Up(0x11) } }
}
Start-Sleep -Milliseconds 400
