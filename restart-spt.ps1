# Run on a home PC by publish.ps1 (locally, or over ssh): restart every spt running in a tmux
# pane, in that pane, on the release now current. The pane then runs spt alone.
$ErrorActionPreference = 'Stop'

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
