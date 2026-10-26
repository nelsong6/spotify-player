#!/bin/sh
# Started by snapserver as one of the session's two slots (a or b): the session from the
# published spt release that current.json names, so a restart picks up a new release. Its audio
# goes to stdout. Each slot has its own cache and client port, both seeded from the shared ones.
set -eu
slot=$1
version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' /tools/spt/current.json)
mkdir -p "/cache/$slot"
[ -f "/cache/$slot/credentials.json" ] || cp /cache/*.json "/cache/$slot/"
case $slot in a) port=8081 ;; *) port=8082 ;; esac
exec "/tools/spt/$version/spotify_player" -c /tools/spt/config -C "/cache/$slot" \
    -o session_file=/session/last_session.json -o "client_port=$port" \
    session --slots /data/slots --release "$version"
