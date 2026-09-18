#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE=(docker compose --project-directory "${ROOT}" --env-file "${ROOT}/.env")
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BACKUP="${ROOT}/backups/pre-upgrade-${STAMP}.tar.gz"
"${ROOT}/scripts/backup.sh" "${BACKUP}"

CURRENT_ID="$("${COMPOSE[@]}" images -q app | head -n 1)"
[[ -n "${CURRENT_ID}" ]] || { echo "Current app image was not found." >&2; exit 1; }
ROLLBACK_IMAGE="network-atlas:rollback-${STAMP}"
docker tag "${CURRENT_ID}" "${ROLLBACK_IMAGE}"
printf 'ROLLBACK_IMAGE=%q\nROLLBACK_BACKUP=%q\n' "${ROLLBACK_IMAGE}" "${BACKUP}" > "${ROOT}/.release-state"

"${COMPOSE[@]}" pull caddy
"${COMPOSE[@]}" build --pull app
"${COMPOSE[@]}" up -d app caddy
if ! "${ROOT}/scripts/health-check.sh"; then
  SKIP_EMERGENCY_BACKUP=1 APP_IMAGE="${ROLLBACK_IMAGE}" "${ROOT}/scripts/rollback.sh" "${ROLLBACK_IMAGE}" "${BACKUP}"
  exit 1
fi
echo "UPGRADE_OK rollback_image=${ROLLBACK_IMAGE} rollback_backup=${BACKUP}"
