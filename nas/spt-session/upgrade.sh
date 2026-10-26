#!/bin/sh
# Run by publish.ps1: end the slot that isn't the session. Snapcast starts it again on the new
# release, and it takes the session over at the next song change or pause.
set -eu
pid=$(cat /data/slots/waiting.pid 2>/dev/null || true)
if [ -n "$pid" ] && kill "$pid" 2>/dev/null; then
    echo "restarted the waiting slot"
else
    echo "no waiting slot"
fi
