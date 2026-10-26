#!/bin/bash
# Push nas/ to the NAS (over `ssh nas`) and (re)start its containers, which run the published spt
# release: spt-keep runs `spotify_player keep`; spt-session runs the home cast's session under
# Snapcast's server.
#
# Both sign in to Spotify with spt's cached sign-in. The first deploy copies it from this
# machine's spt cache into spt-keep, and spt-session starts from spt-keep's copy; it stays on the
# NAS (/volume1/docker/<container>/cache, owner-only) and never enters this repo.
set -euo pipefail
cd "$(dirname "$0")"

CACHE="${SPT_CACHE:-$HOME/.cache/spotify-player}"
tar cf - spt-keep spt-session | ssh nas "cat > /tmp/spt-deploy.tar"
if ! ssh nas "sudo test -f /volume1/docker/spt-keep/cache/credentials.json"; then
  (cd "$CACHE" && tar cf - credentials.json ./*_token.json) | ssh nas "cat > /tmp/spt-keep-auth.tar"
  echo "copying spt's sign-in from $CACHE"
fi

ssh nas "sudo bash -s" <<'REMOTE'
set -euo pipefail
D=/usr/local/bin/docker
T=$(mktemp -d); tar xf /tmp/spt-deploy.tar -C "$T"; rm /tmp/spt-deploy.tar
mkdir -p /volume1/docker/spt-session/data
for name in spt-keep spt-session; do
  K=/volume1/docker/$name
  mkdir -p $K/cache
  for d in $K $K/cache $K/data; do
    if [ -d $d ]; then /usr/syno/bin/synoacltool -del $d >/dev/null 2>&1 || true; fi
  done
  cp "$T/$name"/* $K/
  chmod 755 $K/*.sh
done
if [ -f /tmp/spt-keep-auth.tar ]; then
  tar xf /tmp/spt-keep-auth.tar -C /volume1/docker/spt-keep/cache; rm /tmp/spt-keep-auth.tar
fi
if [ ! -f /volume1/docker/spt-session/cache/credentials.json ]; then
  cp /volume1/docker/spt-keep/cache/credentials.json /volume1/docker/spt-keep/cache/*_token.json \
    /volume1/docker/spt-session/cache/
fi
for name in spt-keep spt-session; do
  K=/volume1/docker/$name
  chown -R 1026:100 $K
  # removing Synology's ACLs leaves no permission bits behind
  chmod 755 $K
  if [ -d $K/data ]; then chmod 755 $K/data; fi
  chmod 700 $K/cache
  chmod 600 $K/cache/*.json
  (cd $K && $D compose up -d --build --force-recreate 2>&1 | grep -v -E "Running|Up-to-date" || true)
done
rm -rf "$T"
echo "deployed"
REMOTE
