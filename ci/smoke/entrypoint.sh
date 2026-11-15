#!/bin/sh
set -eu
if [ "${1:-}" = --self-test ]; then
    snapserver --version
    /opt/spt/spotify_player --version
    ldd /opt/spt/spotify_player
    for p in bash jq curl inotifywait sqlite3 sha256sum; do command -v "$p"; done
    bash -n /stager.sh
    sh -n /run.sh
    exit 0
fi
exec snapserver --config /etc/snapserver.conf "$@"
