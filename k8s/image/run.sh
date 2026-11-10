#!/bin/sh
# Started by snapserver as one of the session's two slots (a or b): the session from the
# release the stager last staged (current.json), so a restart picks up a new release. Its audio
# goes to stdout. Each slot has its own cache and client port, both seeded from the shared ones.
set -eu
slot=$1
version=$(jq -er .version /state/releases/current.json)
mkdir -p "/state/cache/$slot"
[ -f "/state/cache/$slot/credentials.json" ] || cp /state/cache/*.json "/state/cache/$slot/"
case $slot in a) port=8081 ;; *) port=8082 ;; esac
exec "/state/releases/$version/spotify_player" -c /config -C "/state/cache/$slot" \
    -o session_file=/session/last_session.json -o "client_port=$port" \
    session --slots /state/data/slots --release "$version"
