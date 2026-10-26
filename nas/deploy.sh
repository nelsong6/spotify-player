#!/bin/bash
# Push nas/spt-session to the NAS (over `ssh nas`) and (re)start it: Snapcast's server running the
# home cast's session from the published spt release. The session holds the house's one Spotify
# sign-in, keeps the playlists, and relays the PCs' requests to Spotify.
#
# The first deploy copies spt's cached sign-in from this machine's spt cache; it stays on the NAS
# (/volume1/docker/spt-session/cache, owner-only) and never enters this repo.
set -euo pipefail
cd "$(dirname "$0")"

CACHE="${SPT_CACHE:-$HOME/.cache/spotify-player}"
tar cf - spt-session | ssh nas "cat > /tmp/spt-deploy.tar"
if ! ssh nas "sudo test -f /volume1/docker/spt-session/cache/credentials.json"; then
  (cd "$CACHE" && tar cf - credentials.json ./*_token.json) | ssh nas "cat > /tmp/spt-session-auth.tar"
  echo "copying spt's sign-in from $CACHE"
fi

ssh nas "sudo bash -s" <<'REMOTE'
set -euo pipefail
D=/usr/local/bin/docker
K=/volume1/docker/spt-session
T=$(mktemp -d); tar xf /tmp/spt-deploy.tar -C "$T"; rm /tmp/spt-deploy.tar
mkdir -p $K/cache $K/data
for d in $K $K/cache $K/data; do
  /usr/syno/bin/synoacltool -del $d >/dev/null 2>&1 || true
done
cp "$T"/spt-session/* $K/
chmod 755 $K/*.sh
if [ -f /tmp/spt-session-auth.tar ]; then
  tar xf /tmp/spt-session-auth.tar -C $K/cache; rm /tmp/spt-session-auth.tar
fi
# the session took over spt-keep's job; its state carries on beside the saved session
if [ -d /volume1/docker/spt-keep ]; then
  $D rm -f spt-keep >/dev/null 2>&1 || true
  if [ -f /volume1/docker/spt-keep/cache/keeper.json ] && [ ! -f /volume1/Files/spt/keeper.json ]; then
    cp /volume1/docker/spt-keep/cache/keeper.json /volume1/Files/spt/keeper.json
    chown 1026:100 /volume1/Files/spt/keeper.json
  fi
  rm -rf /volume1/docker/spt-keep
  echo "removed spt-keep"
fi
chown -R 1026:100 $K
# removing Synology's ACLs leaves no permission bits behind
chmod 755 $K $K/data
chmod 700 $K/cache
chmod 600 $K/cache/*.json
(cd $K && $D compose up -d --build --force-recreate 2>&1 | grep -v -E "Running|Up-to-date" || true)
rm -rf "$T"
echo "deployed"
REMOTE
