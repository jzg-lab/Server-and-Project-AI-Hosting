#!/usr/bin/env bash
set -euo pipefail

REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/network-atlas-m4-deploy.XXXXXX")"
FAKE_BIN="${TEMP_ROOT}/bin"
ORIGINAL_PATH="${PATH}"
trap 'rm -rf "${TEMP_ROOT}"' EXIT

fail() {
  echo "DEPLOY_VERIFY_FAILED: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local expected="$2"
  grep -F -- "${expected}" "${file}" >/dev/null || fail "${file} does not contain: ${expected}"
}

assert_mode() {
  local expected="$1"
  local path="$2"
  local actual
  actual="$(stat -c '%a' "${path}")"
  [[ "${actual}" == "${expected}" ]] || fail "mode for ${path}: expected ${expected}, got ${actual}"
}

assert_order() {
  local file="$1"
  shift
  local previous=0
  local pattern line
  for pattern in "$@"; do
    line="$(grep -nF -- "${pattern}" "${file}" | head -n 1 | cut -d: -f1 || true)"
    [[ -n "${line}" ]] || fail "${file} does not contain ordered step: ${pattern}"
    (( line > previous )) || fail "${file} has an invalid command order at: ${pattern}"
    previous="${line}"
  done
}

mkdir -p "${FAKE_BIN}"

cat > "${FAKE_BIN}/docker" <<'FAKE_DOCKER'
#!/usr/bin/env bash
set -euo pipefail

{
  printf 'docker'
  printf ' %q' "$@"
  printf '\n'
} >> "${FAKE_DOCKER_LOG}"

map_container_path() {
  case "$1" in
    /tmp/network-atlas-backup.db)
      printf '%s/tmp/network-atlas-backup.db' "${FAKE_CONTAINER_ROOT}"
      ;;
    /var/lib/network-atlas/*)
      printf '%s/%s' "${FAKE_CONTAINER_ROOT}" "${1#/var/lib/network-atlas/}"
      ;;
    *)
      return 1
      ;;
  esac
}

command_name="${1:-}"
[[ -n "${command_name}" ]] || exit 2
shift

case "${command_name}" in
  compose)
    while [[ "${1:-}" == --project-directory || "${1:-}" == --env-file ]]; do
      shift 2
    done
    subcommand="${1:-}"
    shift || true
    case "${subcommand}" in
      ps)
        echo "fake-app-cid"
        ;;
      exec)
        [[ "${1:-}" == "-T" ]] && shift
        shift || true
        joined=" $* "
        if [[ "${joined}" == *'.backup /tmp/network-atlas-backup.db'* ]]; then
          mkdir -p "${FAKE_CONTAINER_ROOT}/tmp"
          cp "${FAKE_CONTAINER_ROOT}/network-atlas.db" "${FAKE_CONTAINER_ROOT}/tmp/network-atlas-backup.db"
        elif [[ "${joined}" == *'rm -f /tmp/network-atlas-backup.db'* ]]; then
          rm -f "${FAKE_CONTAINER_ROOT}/tmp/network-atlas-backup.db"
        fi
        ;;
      images)
        echo "${FAKE_IMAGE_ID}"
        ;;
      stop|up|pull|build)
        ;;
      *)
        echo "Unsupported fake docker compose command: ${subcommand}" >&2
        exit 2
        ;;
    esac
    ;;
  cp)
    source_path="$1"
    destination="$2"
    container_path="${source_path#*:}"
    mapped="$(map_container_path "${container_path}")"
    if [[ -d "${mapped}" ]]; then
      cp -a "${mapped}" "${destination}"
    else
      cp "${mapped}" "${destination}"
    fi
    ;;
  exec)
    shift
    if [[ "${1:-}" == test && "${2:-}" == -d ]]; then
      mapped="$(map_container_path "$3")"
      [[ -d "${mapped}" ]]
    else
      echo "Unsupported fake docker exec command" >&2
      exit 2
    fi
    ;;
  inspect)
    echo "${FAKE_IMAGE_ID}"
    ;;
  image)
    [[ "${1:-}" == inspect ]] || exit 2
    ;;
  tag)
    ;;
  run)
    args=("$@")
    joined=" ${args[*]} "
    if [[ "${joined}" == *' --hash-password '* ]]; then
      cat >/dev/null
      printf '%s\n' '$argon2id$v=19$m=19456,t=2,p=1$fixture$fixturehash'
      exit 0
    fi

    backup_mount=""
    restore_mount=""
    has_data_volume=0
    for ((index = 0; index < ${#args[@]}; index++)); do
      if [[ "${args[index]}" == -v && $((index + 1)) -lt ${#args[@]} ]]; then
        mapping="${args[index + 1]}"
        case "${mapping}" in
          *:/backup:ro) backup_mount="${mapping%:/backup:ro}" ;;
          *:/restore:ro) restore_mount="${mapping%:/restore:ro}" ;;
          network-atlas-data:/data) has_data_volume=1 ;;
        esac
      fi
    done

    if [[ "${joined}" == *' --verify-backup '* ]]; then
      mount="${backup_mount:-${restore_mount}}"
      [[ -n "${mount}" && -s "${mount}/network-atlas.db" ]] || exit 3
      exit 0
    fi

    if [[ "${joined}" == *' --entrypoint sh '* ]]; then
      [[ "${has_data_volume}" == 1 ]] || exit 4
      [[ -n "${restore_mount}" ]] || exit 5
      [[ "${joined}" == *'rm -rf /data/network-atlas.db'* ]] || exit 6
      [[ "${joined}" == *'cp /restore/network-atlas.db /data/network-atlas.db'* ]] || exit 7
      rm -rf "${FAKE_DOCKER_VOLUME}"
      mkdir -p "${FAKE_DOCKER_VOLUME}"
      cp "${restore_mount}/network-atlas.db" "${FAKE_DOCKER_VOLUME}/network-atlas.db"
      [[ ! -d "${restore_mount}/secrets" ]] || cp -a "${restore_mount}/secrets" "${FAKE_DOCKER_VOLUME}/secrets"
      [[ ! -d "${restore_mount}/ssh" ]] || cp -a "${restore_mount}/ssh" "${FAKE_DOCKER_VOLUME}/ssh"
      exit 0
    fi
    ;;
  *)
    echo "Unsupported fake docker command: ${command_name}" >&2
    exit 2
    ;;
esac
FAKE_DOCKER

cat > "${FAKE_BIN}/curl" <<'FAKE_CURL'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'curl'
  printf ' %q' "$@"
  printf '\n'
} >> "${FAKE_CURL_LOG}"

url="${!#}"
case "${url}" in
  */healthz)
    count=0
    [[ ! -f "${FAKE_HEALTH_COUNT_FILE}" ]] || count="$(cat "${FAKE_HEALTH_COUNT_FILE}")"
    count=$((count + 1))
    printf '%s' "${count}" > "${FAKE_HEALTH_COUNT_FILE}"
    if (( count <= ${FAKE_HEALTH_FAILURES:-0} )); then
      echo "fixture health failure" >&2
      exit 22
    fi
    printf '%s\n' '{"status":"ok"}'
    ;;
  */api/v1/bootstrap)
    printf '%s' "${FAKE_ANON_STATUS:-401}"
    ;;
  *)
    echo "Unsupported fake curl URL: ${url}" >&2
    exit 2
    ;;
esac
FAKE_CURL

chmod +x "${FAKE_BIN}/docker" "${FAKE_BIN}/curl"

setup_fixture() {
  local name="$1"
  SCENARIO_ROOT="${TEMP_ROOT}/${name}/deploy"
  mkdir -p "${SCENARIO_ROOT}/scripts" "${SCENARIO_ROOT}/backups"
  cp "${REPOSITORY_ROOT}"/deploy/scripts/*.sh "${SCENARIO_ROOT}/scripts/"
  chmod +x "${SCENARIO_ROOT}"/scripts/*.sh
  cat > "${SCENARIO_ROOT}/.env" <<'ENVIRONMENT'
DOMAIN=fixture.example
ALLOWED_ORIGIN=https://fixture.example
APP_IMAGE=network-atlas:local
ENVIRONMENT

  export FAKE_CONTAINER_ROOT="${TEMP_ROOT}/${name}/container"
  export FAKE_DOCKER_VOLUME="${TEMP_ROOT}/${name}/volume"
  export FAKE_DOCKER_LOG="${TEMP_ROOT}/${name}/docker.log"
  export FAKE_CURL_LOG="${TEMP_ROOT}/${name}/curl.log"
  export FAKE_HEALTH_COUNT_FILE="${TEMP_ROOT}/${name}/health-count"
  export FAKE_HEALTH_FAILURES=0
  export FAKE_ANON_STATUS=401
  export FAKE_IMAGE_ID="sha256:fixture-image"
  export PATH="${FAKE_BIN}:${ORIGINAL_PATH}"
  mkdir -p "${FAKE_CONTAINER_ROOT}/secrets" "${FAKE_CONTAINER_ROOT}/ssh" "${FAKE_DOCKER_VOLUME}"
  printf '%s\n' "sqlite-fixture-${name}" > "${FAKE_CONTAINER_ROOT}/network-atlas.db"
  printf '%s\n' "secret-fixture-${name}" > "${FAKE_CONTAINER_ROOT}/secrets/model-key"
  printf '%s\n' "known-host-fixture-${name}" > "${FAKE_CONTAINER_ROOT}/ssh/known_hosts"
  printf '%s\n' "old-volume-${name}" > "${FAKE_DOCKER_VOLUME}/network-atlas.db"
  : > "${FAKE_DOCKER_LOG}"
  : > "${FAKE_CURL_LOG}"
}

setup_fixture backup
BACKUP_ARCHIVE="${SCENARIO_ROOT}/backups/manual.tar.gz"
BACKUP_OUTPUT="${TEMP_ROOT}/backup.stdout"
"${SCENARIO_ROOT}/scripts/backup.sh" "${BACKUP_ARCHIVE}" > "${BACKUP_OUTPUT}"
assert_contains "${BACKUP_OUTPUT}" "BACKUP_OK path=${BACKUP_ARCHIVE}"
assert_mode 600 "${BACKUP_ARCHIVE}"
BACKUP_EXTRACT="${TEMP_ROOT}/backup-extract"
mkdir -p "${BACKUP_EXTRACT}"
tar -xzf "${BACKUP_ARCHIVE}" -C "${BACKUP_EXTRACT}"
(cd "${BACKUP_EXTRACT}" && sha256sum -c SHA256SUMS >/dev/null)
cmp "${FAKE_CONTAINER_ROOT}/network-atlas.db" "${BACKUP_EXTRACT}/network-atlas.db"
cmp "${FAKE_CONTAINER_ROOT}/secrets/model-key" "${BACKUP_EXTRACT}/secrets/model-key"
cmp "${FAKE_CONTAINER_ROOT}/ssh/known_hosts" "${BACKUP_EXTRACT}/ssh/known_hosts"
assert_contains "${BACKUP_EXTRACT}/manifest.env" "schema=network-atlas-backup.v1"
assert_order "${FAKE_DOCKER_LOG}" " ps -q app" " cp fake-app-cid:/tmp/network-atlas-backup.db" " inspect --format" " --verify-backup /backup/network-atlas.db"
echo "PASS deploy-backup archive=verified sha256=verified mode=600 command_order=verified"

setup_fixture restore
RESTORE_ARCHIVE="${SCENARIO_ROOT}/backups/restore-source.tar.gz"
"${SCENARIO_ROOT}/scripts/backup.sh" "${RESTORE_ARCHIVE}" >/dev/null
: > "${FAKE_DOCKER_LOG}"
: > "${FAKE_CURL_LOG}"
RESTORE_OUTPUT="${TEMP_ROOT}/restore.stdout"
SKIP_EMERGENCY_BACKUP=1 "${SCENARIO_ROOT}/scripts/restore.sh" "${RESTORE_ARCHIVE}" > "${RESTORE_OUTPUT}"
assert_contains "${RESTORE_OUTPUT}" "RESTORE_OK archive=${RESTORE_ARCHIVE} emergency_backup=skipped"
cmp "${FAKE_CONTAINER_ROOT}/network-atlas.db" "${FAKE_DOCKER_VOLUME}/network-atlas.db"
cmp "${FAKE_CONTAINER_ROOT}/secrets/model-key" "${FAKE_DOCKER_VOLUME}/secrets/model-key"
cmp "${FAKE_CONTAINER_ROOT}/ssh/known_hosts" "${FAKE_DOCKER_VOLUME}/ssh/known_hosts"
assert_order "${FAKE_DOCKER_LOG}" " --verify-backup /restore/network-atlas.db" " stop app" " --entrypoint sh" " up -d app caddy"
assert_order "${FAKE_CURL_LOG}" "/healthz" "/api/v1/bootstrap"
echo "PASS deploy-restore checksum=verified volume_replacement=verified health=ok anonymous=401"

setup_fixture release
UPGRADE_OUTPUT="${TEMP_ROOT}/upgrade.stdout"
"${SCENARIO_ROOT}/scripts/upgrade.sh" > "${UPGRADE_OUTPUT}"
assert_contains "${UPGRADE_OUTPUT}" "UPGRADE_OK rollback_image=network-atlas:rollback-"
assert_contains "${SCENARIO_ROOT}/.release-state" "ROLLBACK_IMAGE=network-atlas:rollback-"
assert_contains "${SCENARIO_ROOT}/.release-state" "ROLLBACK_BACKUP="
assert_order "${FAKE_DOCKER_LOG}" " tag sha256:fixture-image network-atlas:rollback-" " pull caddy" " build --pull app" " up -d app caddy"
printf '%s\n' corrupted > "${FAKE_DOCKER_VOLUME}/network-atlas.db"
: > "${FAKE_DOCKER_LOG}"
ROLLBACK_OUTPUT="${TEMP_ROOT}/rollback.stdout"
"${SCENARIO_ROOT}/scripts/rollback.sh" > "${ROLLBACK_OUTPUT}"
assert_contains "${ROLLBACK_OUTPUT}" "ROLLBACK_OK image=network-atlas:rollback-"
cmp "${FAKE_CONTAINER_ROOT}/network-atlas.db" "${FAKE_DOCKER_VOLUME}/network-atlas.db"
assert_order "${FAKE_DOCKER_LOG}" " image inspect network-atlas:rollback-" " --verify-backup /restore/network-atlas.db" " stop app" " --entrypoint sh" " up -d app caddy"
echo "PASS deploy-upgrade backup=verified image_tag=verified release_state=verified health=ok"
echo "PASS deploy-explicit-rollback release_state=loaded backup=verified volume_restored=verified"

setup_fixture auto-rollback
export FAKE_HEALTH_FAILURES=1
AUTO_OUTPUT="${TEMP_ROOT}/auto-rollback.stdout"
set +e
"${SCENARIO_ROOT}/scripts/upgrade.sh" > "${AUTO_OUTPUT}" 2>&1
AUTO_STATUS=$?
set -e
[[ "${AUTO_STATUS}" == 1 ]] || fail "failed upgrade must exit 1, got ${AUTO_STATUS}"
assert_contains "${AUTO_OUTPUT}" "fixture health failure"
assert_contains "${AUTO_OUTPUT}" "ROLLBACK_OK image=network-atlas:rollback-"
cmp "${FAKE_CONTAINER_ROOT}/network-atlas.db" "${FAKE_DOCKER_VOLUME}/network-atlas.db"
assert_order "${FAKE_DOCKER_LOG}" " tag sha256:fixture-image network-atlas:rollback-" " build --pull app" " image inspect network-atlas:rollback-" " --verify-backup /restore/network-atlas.db" " --entrypoint sh"
[[ "$(cat "${FAKE_HEALTH_COUNT_FILE}")" == 2 ]] || fail "auto rollback must perform failed and recovered health checks"
echo "PASS deploy-auto-rollback failed_health=detected rollback=executed recovered_health=ok exit=1"

setup_fixture health
HEALTH_OUTPUT="${TEMP_ROOT}/health.stdout"
"${SCENARIO_ROOT}/scripts/health-check.sh" > "${HEALTH_OUTPUT}"
assert_contains "${HEALTH_OUTPUT}" "HEALTH_OK url=https://fixture.example anonymous_api=401"
export FAKE_ANON_STATUS=200
HEALTH_FAILURE_OUTPUT="${TEMP_ROOT}/health-failure.stdout"
set +e
"${SCENARIO_ROOT}/scripts/health-check.sh" > "${HEALTH_FAILURE_OUTPUT}" 2>&1
HEALTH_FAILURE_STATUS=$?
set -e
[[ "${HEALTH_FAILURE_STATUS}" == 1 ]] || fail "anonymous HTTP 200 must fail the health gate"
assert_contains "${HEALTH_FAILURE_OUTPUT}" "Anonymous business API returned HTTP 200, expected 401."
echo "PASS deploy-health-gate health_json=checked anonymous_401=required anonymous_200=rejected"

setup_fixture password
PASSWORD_OUTPUT="${TEMP_ROOT}/password.stdout"
printf 'fixture-password\nfixture-password\n' | "${SCENARIO_ROOT}/scripts/generate-owner-password.sh" > "${PASSWORD_OUTPUT}"
assert_contains "${PASSWORD_OUTPUT}" "WROTE ${SCENARIO_ROOT}/secrets/owner_password_hash.txt"
assert_contains "${SCENARIO_ROOT}/secrets/owner_password_hash.txt" '$argon2id$'
assert_mode 700 "${SCENARIO_ROOT}/secrets"
assert_mode 600 "${SCENARIO_ROOT}/secrets/owner_password_hash.txt"
if grep -R -F "fixture-password" "${SCENARIO_ROOT}" "${FAKE_DOCKER_LOG}" "${PASSWORD_OUTPUT}" >/dev/null; then
  fail "password input leaked into an artifact or command log"
fi
echo "PASS deploy-password hash=argon2id directory_mode=700 file_mode=600 input_leak=absent"

echo "VERIFY_M4_DEPLOY_OK"
