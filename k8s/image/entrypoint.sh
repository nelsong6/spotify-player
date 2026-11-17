#!/bin/sh
# The image's entry: Snapcast, which runs the session (run.sh) as its audio source. With
# --self-test it is the check CI runs on the image (pipeline-templates' suites): Snapcast and
# the stager's tools are here and the scripts parse, and spt's Linux release runs here. spt
# isn't in the image (the stager stages it), so CI puts the release it assembled at /ci/dist,
# read-only, and the loader runs it from there.
set -eu
if [ "${1:-}" = --self-test ]; then
	snapserver --version
	for p in bash jq curl inotifywait sqlite3 sha256sum; do command -v "$p"; done
	bash -n /stager.sh
	sh -n /run.sh
	/lib64/ld-linux-x86-64.so.2 /ci/dist/spotify_player --version
	exit 0
fi
exec snapserver --config /etc/snapserver.conf "$@"
