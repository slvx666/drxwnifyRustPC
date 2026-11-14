<#
.SYNOPSIS
  Runs a build command (cargo, npm, ...) with limits that depend on what else is running.

.DESCRIPTION
  The owner plays games and runs an iOS VM on this PC while builds run. Limits:
    - Apex Legends (r5apex*) running  -> Idle priority, 3 compiler jobs. The game keeps the CPU.
    - iOS VM running (vmmemWSL > 3 GB) -> BelowNormal priority, 6 jobs.
    - neither                          -> Normal priority, all cores.
    - less than 4 GB RAM free          -> at most 2 jobs on top of the above.
  The check repeats every 5 s during the build: if Apex starts mid-build, the running compilers drop to Idle.
  Cargo and npm caches go to D: (C: is nearly full).

.EXAMPLE
  powershell -NoProfile -File scripts\build.ps1 cargo build -p drx-core
#>
# No param() block on purpose: with one, PowerShell treats cargo flags like -D as its own common parameters.
if ($args.Count -lt 1) { Write-Error 'usage: build.ps1 <command> [args...]'; exit 2 }
$Command = $args[0]
$Rest = @($args | Select-Object -Skip 1)

$ErrorActionPreference = 'Stop'

$env:CARGO_HOME = 'D:\dev\cargo-home'
$env:npm_config_cache = 'D:\dev\npm-cache'

# CMake (needed to build libopus): use the copy bundled with Visual Studio when none is on PATH.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $vsCmake = Get-ChildItem 'C:\Program Files\Microsoft Visual Studio\*\*\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($vsCmake) { $env:PATH = "$($vsCmake.FullName);$env:PATH" }
}
# Portable tools (NSIS etc.) live here.
if (Test-Path 'D:\dev\tools\bin') { $env:PATH = "D:\dev\tools\bin;$env:PATH" }

function Get-Mode {
    $apex = Get-Process -Name 'r5apex*' -ErrorAction SilentlyContinue
    $vm = Get-Process -Name 'vmmemWSL', 'vmmem' -ErrorAction SilentlyContinue |
        Where-Object { $_.WorkingSet64 -gt 3GB }
    $freeMb = [int]((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1KB)
    $cores = [Environment]::ProcessorCount

    if ($apex) { $mode = @{ Name = 'apex'; Priority = 'Idle'; Jobs = 3 } }
    elseif ($vm) { $mode = @{ Name = 'vm'; Priority = 'BelowNormal'; Jobs = 6 } }
    else { $mode = @{ Name = 'free'; Priority = 'Normal'; Jobs = $cores } }

    if ($freeMb -lt 4096 -and $mode.Jobs -gt 2) { $mode.Jobs = 2; $mode.Name += '+lowmem' }
    $mode.FreeMb = $freeMb
    return $mode
}

$mode = Get-Mode
$env:CARGO_BUILD_JOBS = "$($mode.Jobs)"
# Child processes inherit the priority class, so lowering this shell lowers cargo, rustc and the linker.
(Get-Process -Id $PID).PriorityClass = $mode.Priority
Write-Host ("[build] mode={0} priority={1} jobs={2} freeRAM={3}MB" -f $mode.Name, $mode.Priority, $mode.Jobs, $mode.FreeMb)

$toolNames = 'cargo', 'rustc', 'link', 'lld-link', 'cl', 'cc1', 'node', 'esbuild'

# Watch in the background: lower the compilers if a game or the VM starts during a long build.
$watcher = Start-Job -ArgumentList (,$toolNames) -ScriptBlock {
    param($names)
    while ($true) {
        Start-Sleep -Seconds 5
        $prio = $null
        if (Get-Process -Name 'r5apex*' -ErrorAction SilentlyContinue) { $prio = 'Idle' }
        elseif (Get-Process -Name 'vmmemWSL', 'vmmem' -ErrorAction SilentlyContinue | Where-Object { $_.WorkingSet64 -gt 3GB }) { $prio = 'BelowNormal' }
        if ($prio) {
            Get-Process -Name $names -ErrorAction SilentlyContinue | ForEach-Object {
                try { if ($_.PriorityClass -ne $prio) { $_.PriorityClass = $prio } } catch {}
            }
        }
    }
}

try {
    & $Command @Rest
    $code = $LASTEXITCODE
}
finally {
    Stop-Job $watcher -ErrorAction SilentlyContinue
    Remove-Job $watcher -Force -ErrorAction SilentlyContinue
}
exit $code
