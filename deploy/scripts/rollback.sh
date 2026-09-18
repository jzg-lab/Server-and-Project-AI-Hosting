#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -ge 2 ]]; then
  IMAGE="$1"
  BACKUP="$2"
elif [[ -f "${ROOT}/.release-state" ]]; then
  # The file is generated locally by upgrade.sh and contains shell-escaped values only.
  source "${ROOT}/.release-state"
  IMAGE="${ROLLBACK_IMAGE}"
  BACKUP="${ROLLBACK_BACKUP}"
else
  echo "usage: rollback.sh ROLLBACK_IMAGE BACKUP.tar.gz" >&2
  exit 1
fi

docker image inspect "${IMAGE}" >/dev/null
[[ -f "${BACKUP}" ]] || { echo "Rollback backup is missing: ${BACKUP}" >&2; exit 1; }
SKIP_EMERGENCY_BACKUP="${SKIP_EMERGENCY_BACKUP:-0}" APP_IMAGE="${IMAGE}" "${ROOT}/scripts/restore.sh" "${BACKUP}"
echo "ROLLBACK_OK image=${IMAGE} backup=${BACKUP}"
