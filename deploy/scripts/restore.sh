#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE=(docker compose --project-directory "${ROOT}" --env-file "${ROOT}/.env")
ARCHIVE="${1:?usage: restore.sh BACKUP.tar.gz}"
[[ -f "${ARCHIVE}" ]] || { echo "Backup archive does not exist: ${ARCHIVE}" >&2; exit 1; }
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
tar -xzf "${ARCHIVE}" -C "${TMP}"
(cd "${TMP}" && sha256sum -c SHA256SUMS)

IMAGE="${APP_IMAGE:-network-atlas:local}"
docker run --rm -v "${TMP}:/restore:ro" "${IMAGE}" --verify-backup /restore/network-atlas.db >/dev/null
EMERGENCY="skipped"
if [[ "${SKIP_EMERGENCY_BACKUP:-0}" != "1" ]]; then
  EMERGENCY="${ROOT}/backups/pre-restore-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"
  "${ROOT}/scripts/backup.sh" "${EMERGENCY}"
fi

"${COMPOSE[@]}" stop app
docker run --rm --user 0 \
  -v network-atlas-data:/data \
  -v "${TMP}:/restore:ro" \
  --entrypoint sh "${IMAGE}" -c '
    rm -rf /data/network-atlas.db /data/network-atlas.db-shm /data/network-atlas.db-wal /data/secrets /data/ssh
    cp /restore/network-atlas.db /data/network-atlas.db
    if [ -d /restore/secrets ]; then cp -a /restore/secrets /data/secrets; fi
    if [ -d /restore/ssh ]; then cp -a /restore/ssh /data/ssh; fi
    chown -R 11001:11001 /data
  '
"${COMPOSE[@]}" up -d app caddy
"${ROOT}/scripts/health-check.sh"
echo "RESTORE_OK archive=${ARCHIVE} emergency_backup=${EMERGENCY}"
