#!/bin/bash
# Stages the spt release k8s/values.yaml pins (/pin/release.json) and has the session's waiting
# slot start on it: Snapcast starts the ended slot again, on the new release, which takes the
# session over between songs (spotify_player/src/handover.rs). Then waits for the next pin:
# Kubernetes updates /pin in place, so a new pin never restarts the pod.
#
# A release is staged only as CI published it: a stable release of the pinned tag and commit,
# and a binary matching session-release.json's size and SHA-256. Staged releases never change.
# Any failure exits, and Kubernetes starts the stager again after its back-off.
set -euo pipefail
FORGEJO=https://forgejo.romaine.life
REPO=nelsong6/spotify-player
R=/state/releases
SLOTS=/state/data/slots

get() {
	printf 'header = "Authorization: token %s"\n' "$(cat /secrets/release-read-token)" |
		curl -fsS --max-time 300 -K - "$@"
}

stage() {
	local tag=$1 commit=$2
	if [ -f "$R/$tag/session-release.json" ]; then
		return
	fi
	local release manifest size sha stage
	release=$(get "$FORGEJO/api/v1/repos/$REPO/releases/tags/$tag")
	jq -e --arg tag "$tag" --arg commit "$commit" '.tag_name == $tag and .target_commitish == $commit
		and .body == "Built from \($commit)." and (.draft | not) and (.prerelease | not)' <<<"$release" >/dev/null ||
		{ echo "stager: $tag is not a published CI release of $commit" >&2; exit 1; }
	stage=$(mktemp -d "$R/.stage-XXXXXX")
	trap 'rm -rf "$stage"' EXIT
	get "$FORGEJO/$REPO/releases/download/$tag/session-release.json" >"$stage/session-release.json"
	manifest=$(cat "$stage/session-release.json")
	jq -e --arg tag "$tag" --arg commit "$commit" '.tag == $tag and .commit == $commit' <<<"$manifest" >/dev/null ||
		{ echo "stager: $tag's session-release.json names another release" >&2; exit 1; }
	size=$(jq -er '.files.spotify_player.size' <<<"$manifest")
	sha=$(jq -er '.files.spotify_player.sha256' <<<"$manifest")
	get --max-filesize "$size" "$FORGEJO/$REPO/releases/download/$tag/spotify_player" >"$stage/spotify_player"
	if [ "$(stat -c %s "$stage/spotify_player")" != "$size" ] ||
		! echo "$sha  $stage/spotify_player" | sha256sum -c --quiet -; then
		echo "stager: $tag's spotify_player doesn't match session-release.json" >&2
		exit 1
	fi
	chmod 755 "$stage" "$stage/spotify_player"
	sync
	mv -T "$stage" "$R/$tag"
	trap - EXIT
	echo "stager: staged $tag ($commit)"
}

point() {
	local tag=$1 commit=$2
	if [ "$(jq -r .version "$R/current.json" 2>/dev/null)" = "$tag" ]; then
		return
	fi
	jq -n --arg version "$tag" --arg commit "$commit" --arg switched "$(date -u +%FT%TZ)" \
		'{version: $version, commit: $commit, switched: $switched}' >"$R/.current.json"
	sync
	mv -f "$R/.current.json" "$R/current.json"
	echo "stager: the session's slots start on $tag"
}

# The slot that isn't the session ends, unless it already runs this release; Snapcast starts it
# again on it.
restart_waiting() {
	local tag=$1 pid
	pid=$(cat "$SLOTS/waiting.pid" 2>/dev/null || true)
	if [ -z "$pid" ] || ! [ -r "/proc/$pid/cmdline" ]; then
		echo "stager: no waiting slot yet"
	elif tr '\0' ' ' <"/proc/$pid/cmdline" | grep -qF "$R/$tag/spotify_player"; then
		:
	elif kill "$pid"; then
		echo "stager: restarted the waiting slot on $tag"
	fi
}

# Every staged release but the pinned one and the session's own.
prune() {
	local tag=$1 active dir
	active=$(jq -r '.release // empty' "$SLOTS/active.json" 2>/dev/null || true)
	for dir in "$R"/v*; do
		case ${dir##*/} in "$tag" | "$active") ;; *) rm -rf "$dir" && echo "stager: removed ${dir##*/}" ;; esac
	done
}

# The pin last applied: Kubernetes' swap of /pin is several events.
applied=
apply() {
	local tag commit
	tag=$(jq -er .tag /pin/release.json)
	commit=$(jq -er .commit /pin/release.json)
	if [ "$tag $commit" = "$applied" ]; then
		return
	fi
	if ! [[ $tag =~ ^v[0-9]+(\.[0-9]+)+$ && $commit =~ ^[0-9a-f]{40}$ ]]; then
		echo "stager: invalid pin $tag $commit" >&2
		exit 1
	fi
	stage "$tag" "$commit"
	point "$tag" "$commit"
	restart_waiting "$tag"
	prune "$tag"
	applied="$tag $commit"
}

mkdir -p "$R"
# Kubernetes swaps the pin's folder in with a rename. The watch is in place before the first
# apply, so a pin that lands while one is staged isn't missed.
exec 3< <(inotifywait -m -e moved_to -e create -e delete --format x /pin 2>&1)
while read -r line <&3 && [ "$line" != "Watches established." ]; do :; done
[ "${line:-}" = "Watches established." ] || { echo "stager: can't watch /pin: ${line:-}" >&2; exit 1; }
apply
while read -r _ <&3; do apply; done
echo "stager: the watch on /pin ended" >&2
exit 1
