#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE=(docker compose --project-directory "${ROOT}" --env-file "${ROOT}/.env")
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUTPUT="${1:-${ROOT}/backups/network-atlas-${STAMP}.tar.gz}"
mkdir -p "$(dirname "${OUTPUT}")"
OUTPUT="$(cd "$(dirname "${OUTPUT}")" && pwd)/$(basename "${OUTPUT}")"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

CID="$("${COMPOSE[@]}" ps -q app)"
[[ -n "${CID}" ]] || { echo "App container is not running." >&2; exit 1; }
"${COMPOSE[@]}" exec -T app sh -c 'rm -f /tmp/network-atlas-backup.db && sqlite3 /var/lib/network-atlas/network-atlas.db ".backup /tmp/network-atlas-backup.db"'
docker cp "${CID}:/tmp/network-atlas-backup.db" "${TMP}/network-atlas.db" >/dev/null
"${COMPOSE[@]}" exec -T app rm -f /tmp/network-atlas-backup.db
for directory in secrets ssh; do
  if docker exec "${CID}" test -d "/var/lib/network-atlas/${directory}"; then
    docker cp "${CID}:/var/lib/network-atlas/${directory}" "${TMP}/${directory}" >/dev/null
  fi
done

IMAGE_ID="$(docker inspect --format '{{.Image}}' "${CID}")"
docker run --rm -v "${TMP}:/backup:ro" "${IMAGE_ID}" --verify-backup /backup/network-atlas.db >/dev/null
(
  cd "${TMP}"
  sha256sum network-atlas.db > SHA256SUMS
  printf 'schema=network-atlas-backup.v1\ncreated_at=%s\nimage_id=%s\n' "${STAMP}" "${IMAGE_ID}" > manifest.env
  tar -czf "${OUTPUT}" .
)
chmod 600 "${OUTPUT}"
tar -tzf "${OUTPUT}" >/dev/null
echo "BACKUP_OK path=${OUTPUT}"
