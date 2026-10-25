#!/bin/sh
# Run `spotify_player keep` from the published spt release that current.json names, so a
# restart picks up a new release.
set -eu
version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' /tools/spt/current.json)
exec "/tools/spt/$version/spotify_player" -c /tools/spt/config -C /cache keep
