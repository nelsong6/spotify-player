# Shared CI

The repository selects the Rust, release and cluster-service suites with Forgejo
repository topics. `.ci.json` declares platform features, the locked fzt dependency,
joined release targets and the Helm render. The default suite checks workflow rules
and dependency fetches.

The Linux session tests use `rustls-tls,streaming` without default features. Windows
tests additionally exercise streaming, cast, media-control, image, notify and fzf.
Linux and Windows checks run in separate jobs. Reused checks post skipped statuses
that identify the commit where identical inputs passed.
The Windows release keeps the application's default features; the Linux release
uses the session's feature set. The fzt dependency belongs only to the Windows part.

The published names remain `spotify_player.exe`, `fzt.dll`, `spotify_player`,
`session-sources.txt` and `session-release.json`. The asset map restores the executable
names, Cargo depfiles supply the source list, and the shared publisher writes the
existing tag/commit/files checksum manifest after signing. Home-sync's
`machine/state.json` consumes the two Windows files without a configuration change.

The root Dockerfile packages the assembled Linux session binary. Its terminating
`--self-test` executes the binary in the final Debian image to check dynamic linking,
and verifies the Snapcast and staging tools without connecting to Spotify or the
live session.

`k8s/values.yaml` retains `release.tag` and `release.commit`: the chart writes them to
`/pin/release.json`, and `k8s/image/stager.sh` validates the published release and its
checksum manifest against both fields before staging it. The declared `pinFields`
advance both release fields alongside the image in one automatically merged pin PR.

The stager accepts the currently pinned publisher's body and the shared publisher's
header; both paths still require the pinned tag, source commit, stable release and
matching binary checksum manifest.
