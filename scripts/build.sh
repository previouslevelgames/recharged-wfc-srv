#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

revision="$(git rev-parse --verify HEAD 2>/dev/null || printf unknown)"
image="${SERVER_IMAGE:-ghcr.io/previouslevelgames/recharged-wfc-srv:latest}"

printf 'Building %s (revision %s)\n' "$image" "$revision"
docker build --build-arg "GIT_COMMIT=$revision" --tag "$image" .
