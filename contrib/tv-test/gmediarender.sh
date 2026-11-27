#!/bin/sh
# luchd against a real UPnP renderer, gmediarender (gmrender-resurrect), in Docker. Its audio and video sinks are
# fakesinks that keep time like a real one: nothing is played aloud. The search goes out with TTL 0, so only this container hears it.
#
#   contrib/tv-test/gmediarender.sh            (from the repository root)
set -eu
cd "$(dirname "$0")/../.."
docker build -q -t luch-gmr-test -f contrib/tv-test/Dockerfile.gmediarender contrib/tv-test >/dev/null
docker run --rm -v "$PWD":/src -v luch-cargo:/root/.cargo/registry -v "${LUCH_TARGET:-luch-target}":/target \
    -e CARGO_TARGET_DIR=/target -w /src luch-gmr-test sh -c '
    gmediarender -f "Docker renderer" --gstout-audiopipe="fakesink sync=true" --gstout-videopipe="fakesink sync=true" \
        --logfile=/tmp/gmr.log &
    sleep 1
    LUCH_REAL_RENDERER="Docker renderer" cargo test -p luchd -- --ignored --nocapture a_real_renderer'
