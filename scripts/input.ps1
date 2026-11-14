# Developer checks with real input: mouse into the Mini window at client coordinates (the window may sit on the
# second monitor; the cursor moves there). Pair with shotwin.ps1 -Client. Run Mini with DRX_MUTE=1 so clicks stay silent.
param([string]$Do = "wheel", [int]$X, [int]$Y, [int]$Amount = -3, [string]$Text = "", [int]$Key = 0, [switch]$Ctrl)
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
  # Keyboard only into Mini when it is the active window (after a click into it): never into the owner's apps.
  "type"   { if ([DrxIn]::GetForegroundWindow() -ne $p.MainWindowHandle) { Write-Error "Mini is not active: no keys sent"; exit 3 }
             foreach ($c in $Text.ToCharArray()) { [DrxIn]::Key(0, [uint16]$c); Start-Sleep -Milliseconds 40 } }
  "key"    { if ([DrxIn]::GetForegroundWindow() -ne $p.MainWindowHandle) { Write-Error "Mini is not active: no keys sent"; exit 3 }
             if ($Ctrl) { [DrxIn]::Down(0x11) }; [DrxIn]::Key([uint16]$Key, 0); if ($Ctrl) { [DrxIn]::Up(0x11) } }
}
Start-Sleep -Milliseconds 400
