#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${APP_IMAGE:-network-atlas:local}"

read -r -s -p "Owner password: " PASSWORD
printf '\n'
read -r -s -p "Repeat password: " REPEAT
printf '\n'
if [[ -z "${PASSWORD}" || "${PASSWORD}" != "${REPEAT}" ]]; then
  echo "Passwords are empty or do not match." >&2
  exit 1
fi

if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
  docker compose --project-directory "${ROOT}" --env-file "${ROOT}/.env" build app
fi

HASH="$(printf '%s' "${PASSWORD}" | docker run --rm -i "${IMAGE}" --hash-password)"
unset PASSWORD REPEAT
if [[ "${HASH}" != \$argon2id\$* ]]; then
  echo "Generated value is not an Argon2id hash." >&2
  exit 1
fi

install -d -m 700 "${ROOT}/secrets"
umask 077
printf '%s\n' "${HASH}" > "${ROOT}/secrets/owner_password_hash.txt"
echo "WROTE ${ROOT}/secrets/owner_password_hash.txt"
