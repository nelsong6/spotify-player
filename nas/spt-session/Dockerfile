# Snapcast's server, which runs the session (run.sh) as its audio source and streams it to the PCs.
# The program comes from the published release mounted at /tools/spt, so this image rarely changes.
FROM debian:stable-slim
ARG SNAPCAST=0.35.0
ARG SNAPCAST_SHA256=6a82325cf9eae68b75fb062491639cb31ec58288fe8930f53e86689e22a095da
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libssl3 \
    && curl -fsSL -o /tmp/snapserver.deb \
        "https://github.com/badaix/snapcast/releases/download/v${SNAPCAST}/snapserver_${SNAPCAST}-1_amd64_trixie.deb" \
    && echo "${SNAPCAST_SHA256}  /tmp/snapserver.deb" | sha256sum -c - \
    && apt-get install -y --no-install-recommends /tmp/snapserver.deb \
    && rm /tmp/snapserver.deb \
    && rm -rf /var/lib/apt/lists/*
COPY snapserver.conf /etc/snapserver.conf
COPY run.sh /run.sh
ENTRYPOINT ["snapserver", "--config", "/etc/snapserver.conf"]
