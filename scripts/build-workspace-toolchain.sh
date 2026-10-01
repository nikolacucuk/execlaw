#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image_tag="execlaw/workspace-toolchain:1.0.0"

cd "$repo_root"
docker build --pull --file Dockerfile.workspace-toolchain --tag "$image_tag" .
image_id="$(docker image inspect --format '{{.Id}}' "$image_tag")"
if [[ ! "$image_id" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo "Docker did not return an immutable workspace-toolchain image ID" >&2
  exit 1
fi
printf 'Built %s\n' "$image_tag"
printf 'Local development reference: %s\n' "$image_id"
printf '%s\n' 'Use that local digest only after enabling the Controller local-artifact override; production must use a published, attested OCI digest.'
