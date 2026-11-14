<#
.SYNOPSIS
  Measures an exe: file size, LZMA-compressed size (close to an NSIS installer), idle memory and idle CPU.
  Opens a window, so it refuses to run while Apex is running.

.EXAMPLE
  powershell -NoProfile -File scripts\measure.ps1 D:\dev\cargo-target-spikes\release\spike-slint.exe
#>
if ($args.Count -lt 1) { Write-Error 'usage: measure.ps1 <exe> [warmup-seconds] [sample-seconds]'; exit 2 }
$exe = $args[0]
$warm = if ($args.Count -ge 2) { [int]$args[1] } else { 8 }
$sample = if ($args.Count -ge 3) { [int]$args[2] } else { 20 }

if (Get-Process -Name 'r5apex*' -ErrorAction SilentlyContinue) { Write-Error 'Apex is running: not opening windows now.'; exit 3 }

$size = (Get-Item $exe).Length
$xz = python -I -c "import lzma,sys;print(len(lzma.compress(open(sys.argv[1],'rb').read(),preset=9|lzma.PRESET_EXTREME)))" $exe

$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds $warm
$p.Refresh()
# Sum over the process tree (helpers, WebView2 children) - Mini should have none.
function Tree($id) { $kids = Get-CimInstance Win32_Process -Filter "ParentProcessId=$id" | ForEach-Object { Tree $_.ProcessId }; @($id) + $kids }
$ids = Tree $p.Id
$procs = $ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
$cpu0 = ($procs | Measure-Object TotalProcessorTime -Sum).Sum
if ($cpu0 -is [timespan]) { $cpu0 = $cpu0.TotalMilliseconds } else { $cpu0 = ($procs | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum }
Start-Sleep -Seconds $sample
$procs = $ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
$cpu1 = ($procs | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
$priv = ($procs | Measure-Object PrivateMemorySize64 -Sum).Sum
$ws = ($procs | Measure-Object WorkingSet64 -Sum).Sum
$cores = [Environment]::ProcessorCount
$cpuPct = [math]::Round(100 * ($cpu1 - $cpu0) / ($sample * 1000 * $cores), 3)
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue

"{0}`n  exe {1:N2} MB, lzma {2:N2} MB`n  idle private {3:N1} MB, working set {4:N1} MB, CPU {5}% of all cores ({6} processes)" -f `
    (Split-Path $exe -Leaf), ($size / 1MB), ([double]$xz / 1MB), ($priv / 1MB), ($ws / 1MB), $cpuPct, $procs.Count
