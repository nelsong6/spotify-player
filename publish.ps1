# Build this fork and publish it as `spt` to \\NelsonNAS\tools (Nelson's home
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
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot 'target' }

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

publish-tool -Name spt -Version $version -Path (Join-Path $target 'release\spotify_player.exe') `
    -Entry spotify_player.exe -Repo https://github.com/nelsong6/spotify-player -Commit $commit
