#!/bin/sh
# Started by snapserver as its audio source: the session from the published spt release that
# current.json names, so a restart picks up a new release. Its audio goes to stdout.
set -eu
version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' /tools/spt/current.json)
exec "/tools/spt/$version/spotify_player" -c /tools/spt/config -C /cache \
    -o session_file=/session/last_session.json session
