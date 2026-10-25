#!/bin/bash
# Push nas/ to the NAS (over `ssh nas`) and (re)start the spt-keep container, which runs
# `spotify_player keep` from the published spt release.
#
# The container signs in to Spotify with spt's cached sign-in. The first deploy copies it from
# this machine's spt cache; it stays on the NAS (/volume1/docker/spt-keep/cache, owner-only)
# and never enters this repo.
set -euo pipefail
cd "$(dirname "$0")"

CACHE="${SPT_CACHE:-$HOME/.cache/spotify-player}"
tar cf - spt-keep | ssh nas "cat > /tmp/spt-keep-deploy.tar"
if ! ssh nas "sudo test -f /volume1/docker/spt-keep/cache/credentials.json"; then
  (cd "$CACHE" && tar cf - credentials.json ./*_token.json) | ssh nas "cat > /tmp/spt-keep-auth.tar"
  echo "copying spt's sign-in from $CACHE"
fi

ssh nas "sudo bash -s" <<'REMOTE'
set -euo pipefail
D=/usr/local/bin/docker
K=/volume1/docker/spt-keep
T=$(mktemp -d); tar xf /tmp/spt-keep-deploy.tar -C "$T"; rm /tmp/spt-keep-deploy.tar
mkdir -p $K/cache
if [ -f /tmp/spt-keep-auth.tar ]; then
  tar xf /tmp/spt-keep-auth.tar -C $K/cache; rm /tmp/spt-keep-auth.tar
fi
/usr/syno/bin/synoacltool -del $K >/dev/null 2>&1 || true
/usr/syno/bin/synoacltool -del $K/cache >/dev/null 2>&1 || true
cp $T/spt-keep/Dockerfile $T/spt-keep/docker-compose.yml $K/
install -m 755 $T/spt-keep/run.sh $K/
chown -R 1026:100 $K
chmod 700 $K/cache
chmod 600 $K/cache/*
cd $K && $D compose up -d --build --force-recreate 2>&1 | grep -v -E "Running|Up-to-date" || true
rm -rf "$T"
echo "deployed"
REMOTE
