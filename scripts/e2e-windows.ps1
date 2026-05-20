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
    # `Get-Random` returns an Int32, so `$((Get-Random))$((Get-Random))` can
    # produce a 13-20 digit run with no separator. The sensitivity classifier's
    # credit-card detector matches `\b\d(?:[ -]?\d){12,18}\b` and Luhn-validates
    # the hit; ~10% of uniform integers pass Luhn, which is what was making the
    # marker flake as `Secret` (the entry was redacted to `[REDACTED]` so the
    # captured-text comparison below never matched). Joining the two halves with
    # a non-`[ -]` character keeps each run under 13 digits, so neither half can
    # reach the detector's lower bound. The bash e2es escape this by accident
    # because `$RANDOM` is capped at 5 digits each.
    $marker = "nagori e2e marker $((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')) $((Get-Random))_$((Get-Random))"
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

    Step 'paste: nagori paste -> clipboard rewritten + SendInput exits cleanly'
    # `nagori paste` does two things in sequence (see daemon::runtime::paste_entry):
    # writes the entry to the OS clipboard, then synthesises Ctrl+V via the
    # platform PasteController. On Windows that is `WindowsPasteController` →
    # `SendInput`. CI does not have a text target whose contents we can read
    # back, so we verify the half we can observe: the clipboard is rewritten
    # to the marker, and `SendInput` returns Ok (the CLI exits 0).
    Set-Clipboard -Value 'sentinel-before-paste'
    Invoke-CliSilent @('paste', $entryId)

    $pasted = ''
    $deadline = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $deadline) {
        $pasted = Get-Clipboard -Raw
        if ($null -eq $pasted) { $pasted = '' }
        if ($pasted -eq $marker) { break }
        Start-Sleep -Milliseconds 100
    }
    if ($pasted -ne $marker) {
        throw "'nagori paste' did not rewrite the clipboard to the marker`n  expected: $marker`n  actual:   $pasted"
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
    # Same digit-run hazard as the capture marker above - separate the two
    # `Get-Random` halves so the credit-card detector cannot Luhn the suffix
    # into a `Secret` classification.
    $orderSuffix = "$((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ'))-$((Get-Random))_$((Get-Random))"
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

    Step 'file-list round-trip via STA SetFileDropList'
    # Include a space in one filename so the daemon's path ↔ URL conversion
    # has to percent-encode it on the way in and decode it on the way out.
    # The unit tests already prove the codec; exercising the daemon's
    # capture → store → republish loop with a non-ASCII-safe path catches
    # regressions where someone normalises paths and loses the encoding.
    $UriFileA = Join-Path $WorkDir 'cf hdrop-a.txt'
    $UriFileB = Join-Path $WorkDir 'cf-hdrop-b.txt'
    Set-Content -Path $UriFileA -Value 'first'  -NoNewline
    Set-Content -Path $UriFileB -Value 'second' -NoNewline
    # `[System.Windows.Forms.Clipboard]::SetFileDropList` writes CF_HDROP, the
    # same format Explorer uses for file copies. The Windows daemon surfaces
    # CF_HDROP captures as `FileList` with a `text/uri-list` representation.
    # We go through the .NET API in an STA child because pwsh 7's
    # `Set-Clipboard` dropped the `-Path` parameter that Windows PowerShell
    # 5.1 had, and `SetFileDropList` (like `SetImage`) requires STA while
    # pwsh 7 defaults to MTA. Retry `ExternalException` under the standard
    # 5s budget so a transient clipboard-busy collision (e.g. the previous
    # step's sentinel still committing) doesn't flake the test.
    $EscapedA = $UriFileA.Replace("'", "''")
    $EscapedB = $UriFileB.Replace("'", "''")
    $PushFileScript = @"
`$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
`$paths = New-Object System.Collections.Specialized.StringCollection
[void]`$paths.Add('$EscapedA')
[void]`$paths.Add('$EscapedB')
`$deadline = [DateTime]::UtcNow.AddSeconds(5)
while ([DateTime]::UtcNow -lt `$deadline) {
    try { [System.Windows.Forms.Clipboard]::SetFileDropList(`$paths); exit 0 }
    catch [System.Runtime.InteropServices.ExternalException] { Start-Sleep -Milliseconds 100 }
}
Write-Error 'SetFileDropList failed after retries'
exit 1
"@
    $pushFileEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($PushFileScript))
    & pwsh -Sta -NoProfile -EncodedCommand $pushFileEncoded | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to push file-list onto clipboard via STA child (exit $LASTEXITCODE)"
    }

    $UriEntryId = $null
    $UriListJson = $null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        try {
            $UriListJson = Invoke-Cli @('list', '--limit', '1', '--json') | ConvertFrom-Json
            if ($null -ne $UriListJson -and $UriListJson.Count -gt 0) {
                $top = $UriListJson[0]
                $topKind = if ($top.PSObject.Properties['kind']) { $top.kind } else { '' }
                $hasUri = if ($top.PSObject.Properties['representation_summary']) {
                    ($top.representation_summary | Where-Object { $_.mime_type -eq 'text/uri-list' } | Measure-Object).Count
                } else { 0 }
                if ($topKind -eq 'FileList' -and $hasUri -eq 1) {
                    $UriEntryId = $top.id
                    break
                }
            }
        } catch {}
        Start-Sleep -Milliseconds 200
    }
    if (-not $UriEntryId) {
        throw "file-list capture failed; latest entry: $($UriListJson | ConvertTo-Json -Depth 5 -Compress)"
    }
    Write-Host "captured file-list id=$UriEntryId"

    # Overwrite the selection with plain text so a no-op `copy` would be
    # visible — `GetFileDropList` on a text selection returns an empty
    # collection.
    Set-Clipboard -Value 'not-a-file-list'
    Invoke-CliSilent @('copy', $UriEntryId)

    # Read-back also goes through an STA child: pwsh 7's `Get-Clipboard`
    # has no `-Format` parameter (that was Windows PowerShell 5.1), so we
    # use `[System.Windows.Forms.Clipboard]::GetFileDropList`. The child
    # writes one path per line on success and exits 0; exits 1 if the
    # clipboard never holds a file drop list within the 5s budget.
    $ReadFileScript = @"
`$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
`$deadline = [DateTime]::UtcNow.AddSeconds(5)
while ([DateTime]::UtcNow -lt `$deadline) {
    try {
        `$drops = [System.Windows.Forms.Clipboard]::GetFileDropList()
        if (`$null -ne `$drops -and `$drops.Count -gt 0) {
            foreach (`$p in `$drops) { Write-Output `$p }
            exit 0
        }
    } catch [System.Runtime.InteropServices.ExternalException] { }
    Start-Sleep -Milliseconds 100
}
exit 1
"@
    $readFileEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($ReadFileScript))
    $readOutput = & pwsh -Sta -NoProfile -EncodedCommand $readFileEncoded
    if ($LASTEXITCODE -ne 0) {
        throw "file-list copy-back did not return a FileDropList within 5s"
    }
    # Sort both sides before comparing so a stable Compare-Object check
    # doesn't get tripped up by CF_HDROP order vs. the order we offered.
    $pastedPaths = @($readOutput | Where-Object { $_ -ne '' }) | Sort-Object
    $expectedPaths = @($UriFileA, $UriFileB) | Sort-Object
    $diff = Compare-Object $pastedPaths $expectedPaths -SyncWindow 0
    if ($diff -or $pastedPaths.Count -ne $expectedPaths.Count) {
        throw "file-list copy-back paths did not match`n  expected: $($expectedPaths -join ', ')`n  actual:   $($pastedPaths -join ', ')"
    }

    Step 'image round-trip via image/png'
    # Tiny 1x1 RGBA PNG fixture, base64-inlined so we don't keep a binary
    # artefact in the tree. Same bytes as the Linux e2e's fixture.
    $ImageFixture = Join-Path $WorkDir 'fixture.png'
    $ImageBytes = [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+P+/HgAFhAJ/wlseKgAAAABJRU5ErkJggg==')
    [IO.File]::WriteAllBytes($ImageFixture, $ImageBytes)

    # `[Windows.Forms.Clipboard]::SetImage` requires STA, and pwsh 7's default
    # runspace is MTA. Spawn a short STA child via `pwsh -Sta` to push the
    # bitmap; the OS owns the data after `SetImage` (which copies, not delays)
    # so the child can exit immediately without clearing the offer.
    # `SetImage` raises `System.Runtime.InteropServices.ExternalException` if
    # another process holds the clipboard at the moment of the OLE call.
    # Retry under the standard CI 5s budget so a transient Win32 collision
    # (e.g. the harness's own `Set-Clipboard 'not-an-image'` from the previous
    # step still being committed) doesn't flake the test.
    $PushScript = @"
`$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
`$bmp = [System.Drawing.Image]::FromFile('$ImageFixture')
try {
    `$deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ([DateTime]::UtcNow -lt `$deadline) {
        try { [System.Windows.Forms.Clipboard]::SetImage(`$bmp); exit 0 }
        catch [System.Runtime.InteropServices.ExternalException] { Start-Sleep -Milliseconds 100 }
    }
    Write-Error 'SetImage failed after retries'
    exit 1
} finally { `$bmp.Dispose() }
"@
    $pushEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($PushScript))
    & pwsh -Sta -NoProfile -EncodedCommand $pushEncoded | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to push image onto clipboard via STA child (exit $LASTEXITCODE)"
    }

    $ImageEntryId = $null
    $ImageListJson = $null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        try {
            $ImageListJson = Invoke-Cli @('list', '--limit', '1', '--json') | ConvertFrom-Json
            if ($null -ne $ImageListJson -and $ImageListJson.Count -gt 0) {
                $top = $ImageListJson[0]
                $topKind = if ($top.PSObject.Properties['kind']) { $top.kind } else { '' }
                $hasPng = if ($top.PSObject.Properties['representation_summary']) {
                    ($top.representation_summary | Where-Object { $_.mime_type -eq 'image/png' } | Measure-Object).Count
                } else { 0 }
                if ($topKind -eq 'Image' -and $hasPng -eq 1) {
                    $ImageEntryId = $top.id
                    break
                }
            }
        } catch {}
        Start-Sleep -Milliseconds 200
    }
    if (-not $ImageEntryId) {
        throw "image capture failed; latest entry: $($ImageListJson | ConvertTo-Json -Depth 5 -Compress)"
    }
    Write-Host "captured image id=$ImageEntryId"

    # Sentinel: overwrite with plain text so a no-op `copy` would be visible.
    Set-Clipboard -Value 'not-an-image'
    Invoke-CliSilent @('copy', $ImageEntryId)

    # Read-back also needs STA. We don't compare PNG bytes because the
    # daemon's capture re-encodes `CF_DIB(V5)` into PNG via the `image`
    # crate, so byte-identity with the fixture is broken by design. The
    # `CF_DIBV5` portion of the multi-rep publish is what the
    # `Get-Clipboard -Format Image` path renders, so a 1x1 bitmap on the
    # other side is enough to know the new transactional writer worked.
    # `GetImage` returns `$null` if no image is on the clipboard but raises
    # `ExternalException` when the clipboard is busy. Treat both as
    # "not ready yet" and retry until the deadline.
    $ReadScript = @"
`$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
`$deadline = [DateTime]::UtcNow.AddSeconds(5)
while ([DateTime]::UtcNow -lt `$deadline) {
    try {
        `$img = [System.Windows.Forms.Clipboard]::GetImage()
        if (`$null -ne `$img) {
            Write-Output ("{0}x{1}" -f `$img.Width, `$img.Height)
            `$img.Dispose()
            exit 0
        }
    } catch [System.Runtime.InteropServices.ExternalException] { }
    Start-Sleep -Milliseconds 100
}
exit 1
"@
    $readEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($ReadScript))
    $dim = (& pwsh -Sta -NoProfile -EncodedCommand $readEncoded | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $dim -ne '1x1') {
        throw "image copy-back did not return a 1x1 Bitmap`n  got: '$dim'"
    }

    Step 'multi-representation preserve round-trip (image + text)'
    # Push CF_BITMAP + CF_UNICODETEXT together in a single `SetDataObject`
    # call so the daemon's capture pass sees both reps on one snapshot. The
    # Windows capture path independently probes plain text (arboard
    # `get_text`) and image (`get_image` → PNG re-encode), so the resulting
    # entry should expose both `image/png` and `text/plain` in its
    # `representation_summary`. The copy-back assertion below proves
    # `write_representations` republishes the full set instead of collapsing
    # to a single rep on the way out.
    $multiText = "multi-rep marker $((Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')) $((Get-Random))_$((Get-Random))"
    $EscapedMultiText = $multiText.Replace("'", "''")
    $PushMultiScript = @"
`$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
`$bmp = [System.Drawing.Image]::FromFile('$ImageFixture')
try {
    `$do = New-Object System.Windows.Forms.DataObject
    `$do.SetImage(`$bmp)
    `$do.SetText('$EscapedMultiText')
    `$deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ([DateTime]::UtcNow -lt `$deadline) {
        try { [System.Windows.Forms.Clipboard]::SetDataObject(`$do, `$true); exit 0 }
        catch [System.Runtime.InteropServices.ExternalException] { Start-Sleep -Milliseconds 100 }
    }
    Write-Error 'SetDataObject failed after retries'
    exit 1
} finally { `$bmp.Dispose() }
"@
    $pushMultiEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($PushMultiScript))
    & pwsh -Sta -NoProfile -EncodedCommand $pushMultiEncoded | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to push multi-rep DataObject via STA child (exit $LASTEXITCODE)"
    }

    # Wait for an entry whose representation_summary carries BOTH
    # `image/png` and `text/plain`. We can't pivot on `kind` alone because
    # the capture path may pick either content kind as the primary depending
    # on rep ordering, but the summary is the authoritative inventory.
    $MultiEntryId = $null
    $MultiListJson = $null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        try {
            $MultiListJson = Invoke-Cli @('list', '--limit', '1', '--json') | ConvertFrom-Json
            if ($null -ne $MultiListJson -and $MultiListJson.Count -gt 0) {
                $top = $MultiListJson[0]
                if ($top.PSObject.Properties['representation_summary']) {
                    $hasPng = ($top.representation_summary |
                        Where-Object { $_.mime_type -eq 'image/png' } |
                        Measure-Object).Count
                    $hasText = ($top.representation_summary |
                        Where-Object { $_.mime_type -eq 'text/plain' } |
                        Measure-Object).Count
                    if ($hasPng -ge 1 -and $hasText -ge 1) {
                        $MultiEntryId = $top.id
                        break
                    }
                }
            }
        } catch {}
        Start-Sleep -Milliseconds 200
    }
    if (-not $MultiEntryId) {
        throw "multi-rep capture failed; latest entry: $($MultiListJson | ConvertTo-Json -Depth 5 -Compress)"
    }
    Write-Host "captured multi-rep id=$MultiEntryId"

    # Sentinel: overwrite with plain text so a no-op `copy` would surface as
    # both Get-Clipboard returning the sentinel and GetImage returning $null.
    Set-Clipboard -Value 'sentinel-multi-rep'
    Invoke-CliSilent @('copy', $MultiEntryId)

    # Plain-text rep should be reachable via Get-Clipboard (CF_UNICODETEXT).
    $pasted = ''
    $deadline = (Get-Date).AddSeconds(5)
    while ((Get-Date) -lt $deadline) {
        $pasted = Get-Clipboard -Raw
        if ($null -eq $pasted) { $pasted = '' }
        if ($pasted -eq $multiText) { break }
        Start-Sleep -Milliseconds 100
    }
    if ($pasted -ne $multiText) {
        throw "multi-rep copy-back did not republish CF_UNICODETEXT`n  expected: $multiText`n  actual:   $pasted"
    }

    # Image rep should be reachable via STA GetImage (CF_DIBV5 → CF_BITMAP
    # synthesized). Same 1x1 fixture as the image roundtrip above; dimension
    # is the cheapest proof the bitmap landed.
    $readMultiEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($ReadScript))
    $dim = (& pwsh -Sta -NoProfile -EncodedCommand $readMultiEncoded | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $dim -ne '1x1') {
        throw "multi-rep copy-back did not republish the image rep`n  got: '$dim'"
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
