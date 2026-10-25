# The Linux build links the system's TLS library and needs its root certificates; the program
# itself comes from the published release mounted at /tools/spt, so this image rarely changes.
FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY run.sh /run.sh
ENTRYPOINT ["/run.sh"]
