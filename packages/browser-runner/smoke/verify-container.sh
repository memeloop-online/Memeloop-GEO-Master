#!/usr/bin/env bash
set -euo pipefail

# Run from the repository root on a Linux host with a working Docker daemon.
# No real account, external network, inherited runner token or host port.
docker build \
  --file packages/browser-runner/Dockerfile.interactive \
  --tag geo-interactive-smoke:local \
  packages/browser-runner
docker run --rm \
  --network none \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --pids-limit 256 \
  --memory 2g \
  --shm-size 1g \
  --tmpfs /tmp:rw,nosuid,nodev,mode=1777,size=268435456 \
  --tmpfs /home/pwuser:rw,nosuid,nodev,uid=1000,gid=1000,mode=700,size=134217728 \
  geo-interactive-smoke:local \
  node smoke/interactive-linux.mjs
