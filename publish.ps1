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

# CI's windows-build and linux-build jobs make the published binaries. A squash merge of a PR
# whose branch was up to date has the PR's last commit's tree, so that PR run already built
# this commit and there's no need to wait for the run on master.
$run = $null
$head = gh api "repos/$repo/commits/$commit/pulls" --jq '.[0].head.sha'
if ($head -and (gh api "repos/$repo/git/commits/$head" --jq .tree.sha) -eq (git rev-parse "$commit^{tree}")) {
    $run = gh run list -R $repo -w CI -c $head -e pull_request --json databaseId --jq '.[0].databaseId'
}
foreach ($attempt in 1..30) {
    if ($run) { break }
    $run = gh run list -R $repo -w CI -c $commit -e push --json databaseId --jq '.[0].databaseId'
    if ($run) { break }
    Start-Sleep -Seconds 10
}
if (-not $run) { throw "no CI run for $commit on $repo" }
gh run watch $run -R $repo --exit-status --interval 30
if ($LASTEXITCODE -ne 0) { throw "CI failed: https://github.com/$repo/actions/runs/$run" }

$dir = Join-Path ([IO.Path]::GetTempPath()) "spt-$version"
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
# the Windows build is what the PCs run; the Linux one is `spotify_player keep` on the NAS
foreach ($artifact in 'spotify_player-windows', 'spotify_player-linux') {
    gh run download $run -R $repo -n $artifact -D $dir
    if ($LASTEXITCODE -ne 0) { throw "downloading $artifact failed" }
}

publish-tool -Name spt -Version $version -Path $dir `
    -Entry spotify_player.exe -Repo https://github.com/nelsong6/spotify-player -Commit $commit

# spt-keep on the NAS runs the current release from its start; restart it onto this one
# (before the first `nas/deploy.sh` there is none to restart)
$restarted = ssh nas 'sudo /usr/local/bin/docker inspect spt-keep >/dev/null 2>&1 && sudo /usr/local/bin/docker restart spt-keep >/dev/null && echo yes || true'
if ($restarted -eq 'yes') { Write-Host 'restarted spt-keep on the NAS' }
