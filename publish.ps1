# Publish this fork's CI build as `spt` to \\NelsonNAS\tools (Nelson's home
# setup; see shell-config-profile-home claude\topics\home-tools.md).
# The version is `git describe` (e.g. v0.25.1-13-g9daae90: 13 commits past
# upstream v0.25.1), so only committed, pushed code is published.
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

if (git status --porcelain) { throw 'commit first: a published version has to match a commit' }
git fetch -q origin
if (git log --oneline '@{u}..HEAD') { throw 'push first: the published commit has to be on GitHub' }

$version = git describe --tags --always
$commit = git rev-parse HEAD
$repo = 'nelsong6/spotify-player'

# CI's windows-build job makes the published build; wait for this commit's run
$run = $null
foreach ($attempt in 1..30) {
    $run = gh run list -R $repo -w CI -c $commit -e push --json databaseId --jq '.[0].databaseId'
    if ($run) { break }
    Start-Sleep -Seconds 10
}
if (-not $run) { throw "no CI run for $commit on $repo" }
gh run watch $run -R $repo --exit-status --interval 30
if ($LASTEXITCODE -ne 0) { throw "CI failed: https://github.com/$repo/actions/runs/$run" }

$dir = Join-Path ([IO.Path]::GetTempPath()) "spt-$version"
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
gh run download $run -R $repo -n spotify_player-windows -D $dir
if ($LASTEXITCODE -ne 0) { throw 'downloading the build failed' }

publish-tool -Name spt -Version $version -Path (Join-Path $dir 'spotify_player.exe') `
    -Entry spotify_player.exe -Repo https://github.com/nelsong6/spotify-player -Commit $commit
