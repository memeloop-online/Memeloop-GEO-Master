#!/usr/bin/env bash
set -euo pipefail

# Run from the repository root on a Linux host with a working Docker daemon.
# No real account, external network, inherited runner token or host port.
# --prebuilt is only for a trusted caller that just built and loaded this tag.
# Never rebuild between this smoke and publication/export of the tested image.
if [[ $# -eq 0 ]]; then
  docker build \
    --file packages/browser-runner/Dockerfile.interactive \
    --tag geo-interactive-smoke:local \
    packages/browser-runner
elif [[ $# -eq 1 && "$1" == "--prebuilt" ]]; then
  docker image inspect geo-interactive-smoke:local >/dev/null
else
  printf 'Usage: %s [--prebuilt]\n' "$0" >&2
  exit 2
fi
docker run --rm \
  --pull never \
  --network none \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --pids-limit 256 \
  --memory 2g \
  --shm-size 1g \
  --tmpfs /tmp:rw,nosuid,nodev,mode=1777,size=268435456 \
  geo-interactive-smoke:local \
  node smoke/interactive-linux.mjs
