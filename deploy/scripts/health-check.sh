#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -gt 0 ]]; then
  BASE_URL="${1%/}"
else
  BASE_URL="$(sed -n 's/^ALLOWED_ORIGIN=//p' "${ROOT}/.env" | tail -n 1)"
fi
if [[ -z "${BASE_URL}" || "${BASE_URL}" == *DOMAIN* ]]; then
  echo "Provide a concrete HTTPS URL or set ALLOWED_ORIGIN in deploy/.env." >&2
  exit 1
fi

CURL=(curl --silent --show-error --max-time 10)
if [[ "${CURL_INSECURE:-0}" == "1" ]]; then CURL+=(-k); fi

HEALTH="$("${CURL[@]}" --fail "${BASE_URL}/healthz")"
[[ "${HEALTH}" == *'"status":"ok"'* ]] || { echo "Unexpected health response: ${HEALTH}" >&2; exit 1; }
STATUS="$("${CURL[@]}" --output /dev/null --write-out '%{http_code}' "${BASE_URL}/api/v1/bootstrap")"
[[ "${STATUS}" == "401" ]] || { echo "Anonymous business API returned HTTP ${STATUS}, expected 401." >&2; exit 1; }
echo "HEALTH_OK url=${BASE_URL} anonymous_api=401"
