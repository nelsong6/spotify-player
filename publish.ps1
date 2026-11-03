# Publish this fork's Forgejo release as `spt` to \\NelsonNAS\tools (Nelson's home
# setup; see shell-config-profile-home claude\topics\home-tools.md).
# CI releases every master commit that passed in full, named by `git describe`
# (e.g. v0.25.1-13-g9daae90: 13 commits past upstream v0.25.1, release tags left
# out), so only committed, pushed code on master is published.
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

if (git status --porcelain) { throw 'commit first: a published version has to match a commit' }
git fetch -q origin
if ($LASTEXITCODE) { throw 'fetching Forgejo failed' }
if (git log --oneline '@{u}..HEAD') { throw 'push first: the published commit has to be on Forgejo' }
if ((git rev-parse HEAD) -ne (git rev-parse origin/master)) { throw 'publish from the current master commit' }

$version = git describe --tags --always --abbrev=7 --exclude '*-g*'
$commit = git rev-parse HEAD
$repo = 'nelsong6/spotify-player'
$api = "https://forgejo.romaine.life/api/v1/repos/$repo"
$token = ("protocol=https`nhost=forgejo.romaine.life`n`n" | git credential fill | Select-String '^password=(.*)').Matches[0].Groups[1].Value
$headers = @{ Authorization = "token $token" }

# CI's windows-build and linux-build jobs make the binaries, and its release job attaches them
# to the release once the whole run passed. Wait for that, and stop when the run failed.
$assets = 'spotify_player.exe', 'fzt.dll', 'spotify_player', 'session-sources.txt'
$release = $null
foreach ($attempt in 1..180) {
    $r = Invoke-RestMethod "$api/releases/tags/$version" -Headers $headers -SkipHttpErrorCheck -StatusCodeVariable code
    if ($code -eq 200 -and -not $r.draft -and -not ($assets | Where-Object { $_ -notin $r.assets.name })) { $release = $r; break }
    if ($code -notin 200, 404) { throw "release lookup for $version returned $code" }
    $state = (Invoke-RestMethod "$api/commits/$commit/status" -Headers $headers).state
    if ($state -in 'failure', 'error') { throw "CI failed for $commit`: https://forgejo.romaine.life/$repo/actions" }
    if ($attempt -eq 1) { Write-Host "waiting for CI to release $version" }
    Start-Sleep -Seconds 30
}
if (-not $release) { throw "no release $version after 90 minutes: https://forgejo.romaine.life/$repo/actions" }

$dir = Join-Path ([IO.Path]::GetTempPath()) ("spt-" + [guid]::NewGuid())
New-Item -ItemType Directory $dir | Out-Null
# the Windows build is what the PCs run; the Linux one is the session on the NAS
foreach ($asset in $release.assets | Where-Object name -in $assets) {
    Invoke-WebRequest $asset.browser_download_url -Headers $headers -OutFile (Join-Path $dir $asset.name)
}

# -Repo stays GitHub, which the push mirror keeps in step: publish-tool's check for an older
# commit overtaking a newer one asks GitHub (HOMECFG-110 replaces publish-tool)
publish-tool -Name spt -Version $version -Path $dir `
    -Entry spotify_player.exe -Repo https://github.com/nelsong6/spotify-player -Commit $commit

# The session moves to this release only when its program changed: a file compiled into it (CI's
# session-sources.txt, for its release and this one) or the dependencies. Without a list (a
# release from before CI kept one) it moves.
$active = (ssh nas 'sudo cat /volume1/docker/spt-session/data/slots/active.json' | ConvertFrom-Json).release
$activeDir = "\\NelsonNAS\tools\spt\$active"
$moveSession = $true
if ($active -and (Test-Path "$activeDir\session-sources.txt")) {
    $activeCommit = (Get-Content "$activeDir\release.json" -Raw | ConvertFrom-Json).commit
    # this release's list as published: publish-tool has moved the downloaded files by now
    $compiled = @(Get-Content "$activeDir\session-sources.txt") +
        @(Get-Content "\\NelsonNAS\tools\spt\$version\session-sources.txt") +
        'Cargo.toml', 'Cargo.lock', 'spotify_player/Cargo.toml'
    $changed = @(git diff --name-only $activeCommit $commit | Where-Object { $_ -in $compiled })
    $moveSession = $changed.Count -gt 0
}
if ($moveSession) {
    # the session isn't restarted: its waiting slot starts again on this release and takes the
    # session over at the next song change or pause, so the music doesn't stop
    ssh nas 'sudo /usr/local/bin/docker exec spt-session /upgrade.sh 2>/dev/null || true'
} else {
    Write-Host "the session's program is unchanged since $active; it stays"
}

# a running spt keeps its release until restarted, so restart each one on the PCs; a PC that is
# asleep or off gets this release the next time spt opens
$script = "`$Version = '$version'`n" + (Get-Content (Join-Path $PSScriptRoot 'restart-spt.ps1') -Raw)
$restart = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
foreach ($pc in 'nelsonpc', 'upstairspc', 'treadmill') {
    if ($pc -eq $env:COMPUTERNAME) {
        pwsh -NoProfile -EncodedCommand $restart
    } else {
        ssh -o BatchMode=yes -o ConnectTimeout=5 $pc "pwsh -NoProfile -EncodedCommand $restart"
        if ($LASTEXITCODE -ne 0) { Write-Host "$pc`: out of reach; spt there updates when it next opens" }
    }
}
