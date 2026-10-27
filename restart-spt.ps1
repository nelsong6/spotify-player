# Run on a home PC by publish.ps1 (locally, or over ssh), with $Version set to the release just
# published: restart every spt running in a tmux pane, in that pane, on that release. The pane
# then runs spt alone.
$ErrorActionPreference = 'Stop'

# spt runs from this PC's synced copy of the tools share once it has one, which lags the NAS: a
# restart before the release reaches it reopens the old one. Wait for it, woken by the sync's
# own file changes.
$local = Join-Path $env:USERPROFILE 'tools\spt'
function Test-Synced {
    $current = Join-Path $local 'current.json'
    (Test-Path $current) -and
    ((Get-Content $current -Raw | ConvertFrom-Json).version -eq $Version) -and
    (Test-Path (Join-Path $local "$Version\spotify_player.exe"))
}
if ((Test-Path $local) -and -not (Test-Synced)) {
    $watcher = [IO.FileSystemWatcher]::new($local)
    $watcher.IncludeSubdirectories = $true
    $giveUp = (Get-Date).AddMinutes(5)
    while (-not (Test-Synced) -and (Get-Date) -lt $giveUp) {
        $null = $watcher.WaitForChanged([IO.WatcherChangeTypes]::All, 10000)
    }
    $watcher.Dispose()
    if (-not (Test-Synced)) {
        "$env:COMPUTERNAME`: $Version hasn't synced here; spt gets it when next opened"
        return
    }
}

# A tab keeps the tmux release it opened with, and before win32.7-7 (tmux-windows-fork b3938b48)
# respawn-pane -k took the whole server down, with every pane in the tab. Those tabs are left
# alone; their spt gets the new release when reopened.
function Test-RespawnSafe([string]$path) {
    if ($path -notmatch '\\v[^\\]*-win32\.(\d+)-(\d+)-g[0-9a-f]+\\tmux\.exe$') { return $false }
    $tag, $commits = [int]$Matches[1], [int]$Matches[2]
    $tag -gt 7 -or ($tag -eq 7 -and $commits -ge 7)
}

$servers = Get-CimInstance Win32_Process -Filter "Name='tmux.exe'" |
    Where-Object { $_.CommandLine -match '\s-D\s+-S\s+"([^"]+)"' } |
    ForEach-Object { [pscustomobject]@{ Tmux = $_.ExecutablePath; Label = $Matches[1] } }
$restarted = 0
$skipped = 0
foreach ($server in $servers) {
    $panes = & $server.Tmux -S $server.Label list-panes -a -F '#{pane_id} #{pane_current_command}' 2>$null
    foreach ($pane in $panes) {
        $id, $command = $pane -split ' ', 2
        if ($command -ne 'spotify_player') { continue }
        if (-not (Test-RespawnSafe $server.Tmux)) {
            $skipped++
            continue
        }
        & $server.Tmux -S $server.Label respawn-pane -k -t $id spt
        $restarted++
    }
}
"$env:COMPUTERNAME`: restarted $restarted spt" + $(if ($skipped) { ", left $skipped in tabs on an older tmux" })
