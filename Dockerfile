FROM artifacts.home.romaine.life/dockerhub/library/debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
# apt reaches the cache over https before ca-certificates is installed, so its first two runs
# name this bundle, which apt doesn't find on its own
COPY --from=artifacts.home.romaine.life/dockerhub/library/alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
ARG SNAPCAST=0.35.0
ARG SNAPCAST_SHA256=6a82325cf9eae68b75fb062491639cb31ec58288fe8930f53e86689e22a095da
# jq, curl and inotify-tools for the stager; sqlite3 for nas-backup's copy of history.db
RUN sed -i 's#https\?://deb.debian.org/#https://artifacts.home.romaine.life/debian/#' /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::https::CAInfo=/etc/ssl/certs/ca-certificates.crt update \
    && apt-get -o Acquire::https::CAInfo=/etc/ssl/certs/ca-certificates.crt install -y --no-install-recommends ca-certificates curl jq inotify-tools sqlite3 libssl3 \
    && curl -fsSL -o /tmp/snapserver.deb \
        "https://artifacts.home.romaine.life/general/github/badaix/snapcast/releases/download/v${SNAPCAST}/snapserver_${SNAPCAST}-1_amd64_trixie.deb" \
    && echo "${SNAPCAST_SHA256}  /tmp/snapserver.deb" | sha256sum -c - \
    && apt-get install -y --no-install-recommends /tmp/snapserver.deb \
    && rm /tmp/snapserver.deb \
    && rm -rf /var/lib/apt/lists/*
COPY k8s/image/snapserver.conf /etc/snapserver.conf
COPY k8s/image/run.sh k8s/image/stager.sh /
COPY dist/spotify_player-x86_64-unknown-linux-gnu /opt/spt/spotify_player
COPY ci/smoke/entrypoint.sh /entrypoint.sh
RUN chmod 755 /opt/spt/spotify_player /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
