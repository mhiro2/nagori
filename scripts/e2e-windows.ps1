#!/usr/bin/env pwsh
# Smoke-test the Windows clipboard pipeline end-to-end against a freshly built
# `nagori` daemon. Drives the real Win32 clipboard via `Set-Clipboard` /
# `Get-Clipboard` so the `WindowsClipboard` capture path, named-pipe IPC,
# storage, search, and copy-back all run the same code the desktop app uses.
#
# Usage:
#   pwsh -File scripts/e2e-windows.ps1
#   $env:NAGORI_E2E_BIN = 'X:\path\to\nagori.exe'; pwsh -File scripts/e2e-windows.ps1

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($PSVersionTable.Platform -and $PSVersionTable.Platform -ne 'Win32NT') {
    Write-Error 'e2e-windows.ps1: this script is Windows-only.'
    exit 2
}

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$BinDefault = Join-Path $RepoRoot 'target\release\nagori.exe'
$Bin = if ($env:NAGORI_E2E_BIN) { $env:NAGORI_E2E_BIN } else { $BinDefault }
if (-not (Test-Path $Bin -PathType Leaf)) {
    Write-Error "e2e-windows.ps1: nagori binary not found at $Bin`n  hint: cargo build --release -p nagori-cli"
    exit 2
}

$RunSuffix = [guid]::NewGuid().ToString('N').Substring(0, 12)
$WorkDir = (New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "nagori-e2e-$RunSuffix")).FullName
$PipeName = "\\.\pipe\nagori-e2e-$RunSuffix"
$Db = Join-Path $WorkDir 'nagori.sqlite'
$DaemonLog = Join-Path $WorkDir 'daemon.out.log'
$DaemonErrLog = Join-Path $WorkDir 'daemon.err.log'
$CliErr = Join-Path $WorkDir 'cli.err'
$RestoreClipboardFile = Join-Path $WorkDir 'clipboard.bak'

# The Windows daemon stores its IPC auth token under
# %LOCALAPPDATA%\nagori\<sanitised-pipe-name>-<hash>.token (see
# `token_path_for_endpoint`). The `dirs` crate resolves LocalAppData via
# `SHGetKnownFolderPath` and ignores `$env:LOCALAPPDATA`, so we cannot
# reroute the daemon's token directory the way the macOS harness reroutes
# `$HOME`. Instead the e2e uses a per-run unique pipe name (above) so the
# derived token filename never collides with any concurrent daemon, and
# cleanup deletes exactly that file. We resolve LocalAppData here the same
# way the daemon does to make sure cleanup looks in the same place.
$LocalAppData = [Environment]::GetFolderPath('LocalApplicationData')
$TokenDir = Join-Path $LocalAppData 'nagori'
$TokenPattern = "nagori-e2e-$RunSuffix-*.token"

$DaemonProc = $null
$ClipboardSaved = $false

function Step([string]$Message) {
    Write-Host ''
    Write-Host "--- $Message ---"
}

function Invoke-Cli {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)
    $stdout = & $Bin --ipc $PipeName @Arguments 2> $CliErr
    if ($LASTEXITCODE -ne 0) {
        throw "nagori $($Arguments -join ' ') exited with $LASTEXITCODE"
    }
    return $stdout
}

function Invoke-CliSilent {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)
    & $Bin --ipc $PipeName @Arguments 2> $CliErr | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "nagori $($Arguments -join ' ') exited with $LASTEXITCODE"
    }
}

function Wait-For {
    param(
        [Parameter(Mandatory = $true)][string]$Description,
        [Parameter(Mandatory = $true)][scriptblock]$Predicate,
        [int]$TimeoutSec = 30
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        try {
            if (& $Predicate) { return }
        } catch {
            # Predicate raised; treat as still waiting.
        }
        Start-Sleep -Milliseconds 200
    }
    throw "timeout waiting for $Description"
}

function Get-EntryText($entry) {
    if ($entry.PSObject.Properties['text'] -and $null -ne $entry.text) { return $entry.text }
    if ($entry.PSObject.Properties['preview']) { return $entry.preview }
    return ''
}

function Cleanup {
    param([int]$ExitCode)
    if ($null -ne $DaemonProc -and -not $DaemonProc.HasExited) {
        try {
            Stop-Process -Id $DaemonProc.Id -Force -ErrorAction SilentlyContinue
            # Wait for the daemon process to actually exit so the redirect
            # log files are flushed and no zombie holds the named pipe.
            Wait-Process -Id $DaemonProc.Id -Timeout 5 -ErrorAction SilentlyContinue
        } catch {}
    }
    if ($env:CI -ne 'true' -and $ClipboardSaved -and (Test-Path $RestoreClipboardFile)) {
        try {
            $restored = Get-Content -Path $RestoreClipboardFile -Raw
            if ($null -eq $restored) { $restored = '' }
            Set-Clipboard -Value $restored -ErrorAction SilentlyContinue
        } catch {}
    }
    if ($ExitCode -ne 0) {
        if (Test-Path $DaemonLog) {
            Write-Host "::group::daemon stdout ($DaemonLog)"
            Get-Content -Path $DaemonLog -Raw | Write-Host
            Write-Host '::endgroup::'
        }
        if (Test-Path $DaemonErrLog) {
            Write-Host "::group::daemon stderr ($DaemonErrLog)"
            Get-Content -Path $DaemonErrLog -Raw | Write-Host
            Write-Host '::endgroup::'
        }
        if ((Test-Path $CliErr) -and ((Get-Item $CliErr).Length -gt 0)) {
            Write-Host "::group::last cli stderr ($CliErr)"
            Get-Content -Path $CliErr -Raw | Write-Host
            Write-Host '::endgroup::'
        }
    }
    if (Test-Path $TokenDir) {
        Get-ChildItem -Path $TokenDir -Filter $TokenPattern -ErrorAction SilentlyContinue |
            Remove-Item -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Path $WorkDir -Recurse -Force -ErrorAction SilentlyContinue
    exit $ExitCode
}

# Save the user's current clipboard so a local run does not nuke it.
try {
    $existing = Get-Clipboard -Raw -ErrorAction Stop
    if ($null -eq $existing) { $existing = '' }
    Set-Content -Path $RestoreClipboardFile -Value $existing -NoNewline -ErrorAction Stop
    $ClipboardSaved = $true
} catch {
    $ClipboardSaved = $false
}

try {
    Step 'start daemon'
    $daemonArgs = @(
        '--ipc', $PipeName,
        '--db', $Db,
        'daemon', 'run',
        '--capture-interval-ms', '200',
        '--maintenance-interval-min', '60'
    )
    $DaemonProc = Start-Process -FilePath $Bin -ArgumentList $daemonArgs `
        -RedirectStandardOutput $DaemonLog -RedirectStandardError $DaemonErrLog `
        -NoNewWindow -PassThru

    Wait-For 'daemon health' { Invoke-CliSilent @('daemon', 'status'); $true }

    Step 'capture: Set-Clipboard -> daemon -> nagori list'
    $marker = "nagori e2e marker $((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')) $((Get-Random))$((Get-Random))"
    Set-Clipboard -Value $marker

    # Capture loop polls every 200ms; give it a generous budget under CI load.
    $entry = $null
    $entryId = $null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        try {
            $list = Invoke-Cli @('list', '--limit', '1', '--json') | ConvertFrom-Json
            if ($null -ne $list -and $list.Count -gt 0) {
                $top = $list[0]
                if ((Get-EntryText $top) -eq $marker) {
                    $entry = $top
                    $entryId = $top.id
                    break
                }
            }
        } catch {}
        Start-Sleep -Milliseconds 200
    }
    if (-not $entryId) {
        throw 'capture failed; latest entry did not match marker'
    }
    Write-Host "captured id=$entryId"

    if ($entry.sensitivity -ne 'Public') {
        throw "expected Public sensitivity, got $($entry.sensitivity)"
    }

    Step 'search: full-text hits the captured entry'
    $searchHits = (Invoke-Cli @('search', 'nagori e2e marker', '--limit', '5', '--json') |
        ConvertFrom-Json |
        Where-Object { $_.id -eq $entryId } |
        Measure-Object).Count
    if ($searchHits -ne 1) {
        throw "search did not return the captured entry (hits=$searchHits)"
    }

    Step 'copy: nagori copy -> Get-Clipboard returns the original text'
    # Overwrite the clipboard with a sentinel so a no-op `copy` would be visible.
    Set-Clipboard -Value 'sentinel-not-the-marker'
    Invoke-CliSilent @('copy', $entryId)

    $pasted = ''
    $deadline = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $deadline) {
        $pasted = Get-Clipboard -Raw
        if ($null -eq $pasted) { $pasted = '' }
        if ($pasted -eq $marker) { break }
        Start-Sleep -Milliseconds 100
    }
    if ($pasted -ne $marker) {
        throw "Get-Clipboard did not return the marker after copy`n  expected: $marker`n  actual:   $pasted"
    }

    Step 'pin / unpin round-trip'
    Invoke-CliSilent @('pin', $entryId)
    $pinnedCount = (Invoke-Cli @('list', '--pinned', '--json') |
        ConvertFrom-Json |
        Where-Object { $_.id -eq $entryId } |
        Measure-Object).Count
    if ($pinnedCount -ne 1) {
        throw 'pinned list did not contain the entry'
    }
    Invoke-CliSilent @('unpin', $entryId)
    $pinnedAfter = (Invoke-Cli @('list', '--pinned', '--json') |
        ConvertFrom-Json |
        Where-Object { $_.id -eq $entryId } |
        Measure-Object).Count
    if ($pinnedAfter -ne 0) {
        throw 'unpin did not remove the entry from pinned list'
    }

    Step 'delete tombstones the entry'
    Invoke-CliSilent @('delete', $entryId)
    $remaining = (Invoke-Cli @('list', '--limit', '50', '--json') |
        ConvertFrom-Json |
        Where-Object { $_.id -eq $entryId } |
        Measure-Object).Count
    if ($remaining -ne 0) {
        throw 'deleted entry still present in list'
    }

    # Each Set-Clipboard bumps `GetClipboardSequenceNumber`, so the daemon
    # stores even repeated text as distinct entries. The capture loop only
    # sees whichever value happens to be on the clipboard at poll time, so
    # push markers one at a time and confirm each one has landed before
    # pushing the next; otherwise CI scheduling jitter could silently drop
    # intermediate markers.
    function Push-AndWait([string]$Text) {
        Set-Clipboard -Value $Text
        $deadline = (Get-Date).AddSeconds(10)
        while ((Get-Date) -lt $deadline) {
            try {
                $top = (Invoke-Cli @('list', '--limit', '1', '--json') | ConvertFrom-Json)[0]
                if ((Get-EntryText $top) -eq $Text) { return }
            } catch {}
            Start-Sleep -Milliseconds 100
        }
        throw "marker did not land at top of list: $Text"
    }

    Step 'multi-copy ordering newest-first'
    $orderSuffix = "$((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ'))-$((Get-Random))$((Get-Random))"
    $markerA = "nagori e2e order A $orderSuffix"
    $markerB = "nagori e2e order B $orderSuffix"
    $markerC = "nagori e2e order C $orderSuffix"
    Push-AndWait $markerA
    Push-AndWait $markerB
    Push-AndWait $markerC

    $expected = "$markerC`t$markerB`t$markerA"
    $orderTop = $null
    $top3Tsv = ''
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        try {
            $orderTop = (Invoke-Cli @('list', '--limit', '5', '--json') | ConvertFrom-Json) | Select-Object -First 3
            $top3Tsv = ($orderTop | ForEach-Object { Get-EntryText $_ }) -join "`t"
            if ($top3Tsv -eq $expected) { break }
        } catch {}
        Start-Sleep -Milliseconds 200
    }
    if ($top3Tsv -ne $expected) {
        throw "ordering check failed; top 3 were: $top3Tsv"
    }

    Step 'copy back the oldest of the three'
    $entryA = $orderTop | Where-Object { (Get-EntryText $_) -eq $markerA } | Select-Object -First 1
    if (-not $entryA) {
        throw 'could not resolve id for marker A'
    }
    Set-Clipboard -Value 'sentinel-not-marker-A'
    Invoke-CliSilent @('copy', $entryA.id)

    $pasted = ''
    $deadline = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $deadline) {
        $pasted = Get-Clipboard -Raw
        if ($null -eq $pasted) { $pasted = '' }
        if ($pasted -eq $markerA) { break }
        Start-Sleep -Milliseconds 100
    }
    if ($pasted -ne $markerA) {
        throw "older-entry copy-back did not return marker A`n  expected: $markerA`n  actual:   $pasted"
    }

    Step 'graceful shutdown via daemon stop'
    Invoke-CliSilent @('daemon', 'stop')
    $deadline = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $deadline) {
        if ($DaemonProc.HasExited) { break }
        Start-Sleep -Milliseconds 100
    }
    if (-not $DaemonProc.HasExited) {
        throw "daemon did not exit after 'nagori daemon stop'"
    }
    $DaemonProc = $null

    Write-Host 'e2e ok'
    Cleanup -ExitCode 0
}
catch {
    Write-Host "::error::e2e-windows.ps1 failed: $($_.Exception.Message)"
    Write-Host $_.ScriptStackTrace
    Cleanup -ExitCode 1
}
