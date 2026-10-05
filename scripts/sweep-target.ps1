<#
.SYNOPSIS
Prunes stale cargo build output from target/ so it doesn't fill the disk.

.DESCRIPTION
Cargo never deletes old build output. Each build of snapr with different
settings (tests, env overrides, several sessions at once) leaves another
snapr-<hash>.exe and .pdb behind, and builds pointed at a side folder
(CARGO_TARGET_DIR=target/xyz, --target cross-checks) stay after they're done.

This keeps the newest few snapr builds per profile and removes:
  - older snapr-<hash> binaries and debug symbols in debug/ and release/
  - incremental caches not written for -StaleDays days
  - side folders in target/ (anything but debug/ and release/) not written
    for -StaleDays days

Files in use by a running build or program are skipped. Everything removed
is rebuilt by cargo when needed.

.EXAMPLE
scripts\sweep-target.ps1 -DryRun
#>
param(
    # Newest snapr builds kept per profile.
    [int]$Keep = 4,
    # Caches and side folders untouched this long are removed.
    [int]$StaleDays = 2,
    # Only report what would be removed.
    [switch]$DryRun,
    # Report as a Claude Code hook message (and say nothing if nothing went).
    [switch]$Hook
)

$target = Join-Path (Split-Path $PSScriptRoot -Parent) 'target'
if (-not (Test-Path $target)) { return }
$cutoff = (Get-Date).AddDays(-$StaleDays)
$script:freed = 0L
$script:count = 0

function Get-Size($item) {
    if ($item.PSIsContainer) {
        $sum = (Get-ChildItem -LiteralPath $item.FullName -Recurse -File -Force -ErrorAction SilentlyContinue |
            Measure-Object Length -Sum).Sum
        if ($sum) { [long]$sum } else { 0L }
    } else {
        $item.Length
    }
}

# Removes the items (one build's files, or one folder) and reports them as
# one line under `$name`.
function Remove-Stale($name, $items) {
    $size = 0L
    foreach ($item in $items) {
        $itemSize = Get-Size $item
        if (-not $DryRun) {
            try {
                Remove-Item -LiteralPath $item.FullName -Recurse -Force -ErrorAction Stop
            } catch {
                continue # in use, e.g. a test that's running
            }
        }
        $size += $itemSize
    }
    if ($size -eq 0) { return }
    $script:freed += $size
    $script:count++
    if (-not $Hook -and $size -ge 1MB) {
        '{0,8:N1} MB  {1}' -f ($size / 1MB), $name
    }
}

# When a folder was last written to: its own time, or that of the folders
# inside it (a build adds and removes files in deps/, .fingerprint/, ...).
function Get-LastWrite($dir) {
    $times = @($dir.LastWriteTime) + @(Get-ChildItem -LiteralPath $dir.FullName -Directory -Recurse -Depth 2 -Force -ErrorAction SilentlyContinue |
        ForEach-Object { $_.LastWriteTime })
    ($times | Sort-Object -Descending)[0]
}

foreach ($kind in 'debug', 'release') {
    $deps = Join-Path $target "$kind\deps"
    if (Test-Path $deps) {
        # snapr-<hash>.exe, .pdb, .d: one group per build.
        $builds = Get-ChildItem -LiteralPath $deps -File -Filter 'snapr-*' |
            Where-Object { $_.Name -match '^snapr-[0-9a-f]{16}\.' } |
            Group-Object { ($_.Name -split '[-.]')[1] } |
            Sort-Object { ($_.Group | Sort-Object LastWriteTime -Descending)[0].LastWriteTime } -Descending
        $builds | Select-Object -Skip $Keep | ForEach-Object {
            Remove-Stale "$kind\deps\snapr-$($_.Name) ($($_.Count) files)" $_.Group
        }
    }
    $incremental = Join-Path $target "$kind\incremental"
    if (Test-Path $incremental) {
        Get-ChildItem -LiteralPath $incremental -Directory |
            Where-Object { (Get-LastWrite $_) -lt $cutoff } |
            ForEach-Object { Remove-Stale "$kind\incremental\$($_.Name)" @($_) }
    }
}

Get-ChildItem -LiteralPath $target -Directory |
    Where-Object { $_.Name -notin 'debug', 'release' -and (Get-LastWrite $_) -lt $cutoff } |
    ForEach-Object { Remove-Stale "$($_.Name)\ (side build folder)" @($_) }

$verb = if ($DryRun) { 'would free' } else { 'freed' }
$summary = 'sweep-target: {0} {1:N1} GB of stale builds in target\ ({2} removed)' -f $verb, ($script:freed / 1GB), $script:count
if ($Hook) {
    if ($script:count -gt 0) {
        @{ systemMessage = $summary } | ConvertTo-Json -Compress
    }
} else {
    $summary
}
