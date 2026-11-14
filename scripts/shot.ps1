# Captures a screen to a PNG (for checking UI layout during development).
# -Screen 2 captures the second monitor (where test windows open with DRX_BACKGROUND_AT, without taking focus).
param([string]$Out = "shot.png", [int]$Screen = 1)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$all = [System.Windows.Forms.Screen]::AllScreens | Sort-Object { $_.Bounds.X }
$s = if ($Screen -le $all.Count) { $all[$Screen - 1] } else { [System.Windows.Forms.Screen]::PrimaryScreen }
$b = $s.Bounds
$bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
