//! M1 SSH fact-chain: secret references, host-key confirmation, read-only
//! connection checks, and versioned discovery evidence.
//!
//! This module deliberately keeps the first vertical slice small.  The
//! browser supplies structured fields only; command strings and the evidence
//! allow-list remain server-owned in `discovery.rs`.

use std::{net::IpAddr, sync::atomic::Ordering};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    api::AppState,
    catalog,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, ConnectionTestData, ConnectionTestResponse,
        ConnectionTestState, DataSourceDescriptor, DataSourceKind, DataSourceStatus,
        DiscoveryEvidence, DiscoveryEvidenceResponse, DiscoveryProviderCoverage,
        DiscoveryProviderStatus, DiscoveryRunAccepted, DiscoveryRunAcceptedResponse,
        DiscoveryRunRecord, DiscoveryRunResponse, DiscoveryRunState, EvidenceHostIdentity,
        EvidenceItem, EvidenceWarning, Freshness, GlobalHostsViewData, GlobalHostsViewResponse,
        GraphNodeKind, GraphSnapshot, HostAssetRecord, HostCreateRequest,
        HostKeyConfirmationRequest, HostKeyConfirmationResponse, HostKeyState, HostListResponse,
        HostRecord, HostResponse, HostStatus, HostUpdateRequest, MonitorFreshness,
        MonitorScheduleState, SecretKind, SecretRefCreateRequest, SecretRefDescriptor,
        SecretRefResponse, SshAuthTransport,
    },
    discovery::{
        CommandAudit, DiscoveryFailure, DiscoveryRunner, DiscoverySuccess, LINUX_IDENTITY_COMMAND,
        PROTOCOL_VERSION,
    },
    events::{self, ChangeEventKind},
    monitoring_api, projection,
    secrets::SecretStoreError,
    ssh::{
        SshAuthTransport as InternalSshAuthTransport, SshCredential, SshError, SshFailure,
        SshTarget,
    },
};

pub const DEFAULT_WORKSPACE_ID: &str = "workspace-default";
const MAX_IDEMPOTENCY_KEY: usize = 128;
const MAX_REQUEST_ID: usize = 128;

pub async fn recover_interrupted_discoveries(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let finished_at = now();
    let mut tx = pool.begin().await?;
    // A discovery process can be interrupted after SSH has already been
    // verified. Keep the host usable; only the discovery run itself timed out.
    sqlx::query(
        "UPDATE hosts SET status = 'connection_ready', last_checked_at = ?
         WHERE host_id IN (SELECT host_id FROM discovery_runs WHERE state IN ('accepted', 'running'))",
    )
    .bind(&finished_at)
    .execute(&mut *tx)
    .await?;
    let result = sqlx::query(
        "UPDATE discovery_runs SET state = 'discovery_timeout', failure_code = 'DISCOVERY_INTERRUPTED',
            failure_summary = 'Discovery was interrupted by process restart', finished_at = ?,
            evidence_retention = 'summary'
         WHERE state IN ('accepted', 'running')",
    )
    .bind(&finished_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone)]
struct HostDb {
    host_id: String,
    display_name: String,
    address: String,
    port: u16,
    ssh_user: String,
    credential_ref: String,
    host_key_fingerprint: Option<String>,
    pending_host_key_fingerprint: Option<String>,
    pending_host_key_line: Option<String>,
    host_key_state: HostKeyState,
    status: HostStatus,
    created_at: String,
    last_checked_at: Option<String>,
    last_error_code: Option<String>,
    last_error_summary: Option<String>,
}

#[derive(Debug, Error)]
pub enum M1Error {
    #[error("invalid request")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("resource not found")]
    NotFound { resource: &'static str, id: String },
    #[error("request conflicts with current state")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("external SSH operation failed")]
    External {
        code: &'static str,
        message: &'static str,
        summary: String,
        status: StatusCode,
    },
    #[error("storage unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("secret store unavailable")]
    SecretStore,
    #[error("secret reference unavailable")]
    SecretRefUnavailable,
    #[error("application is shutting down")]
    ShuttingDown,
    #[error("internal task error")]
    Internal,
}

impl M1Error {
    fn bad(code: &'static str, message: &'static str, details: Value) -> Self {
        Self::BadRequest {
            code,
            message,
            details,
        }
    }

    fn conflict(code: &'static str, message: &'static str, details: Value) -> Self {
        Self::Conflict {
            code,
            message,
            details,
        }
    }

    fn not_found(resource: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            resource,
            id: id.into(),
        }
    }
}

impl IntoResponse for M1Error {
    fn into_response(self) -> axum::response::Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details) = match self {
            Self::BadRequest {
                code,
                message,
                details,
            } => (StatusCode::BAD_REQUEST, code, message, details),
            Self::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的远程接入对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            Self::External {
                code,
                message,
                summary,
                status,
            } => (
                status,
                code,
                message,
                json!({"summary": compact_summary(&summary)}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "m1 storage operation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地数据存储暂不可用",
                    json!({}),
                )
            }
            Self::SecretStore => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SECRET_STORE_UNAVAILABLE",
                "服务端秘密存储暂不可用",
                json!({}),
            ),
            Self::SecretRefUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SECRET_REF_UNAVAILABLE",
                "SSH credential reference could not be resolved",
                json!({}),
            ),
            Self::ShuttingDown => (
                StatusCode::SERVICE_UNAVAILABLE,
                "APPLICATION_SHUTTING_DOWN",
                "The application is draining and is not accepting new observation jobs",
                json!({}),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "服务内部处理失败",
                json!({}),
            ),
        };
        (
            status,
            Json(ApiErrorResponse {
                error: ApiErrorBody {
                    code: code.to_owned(),
                    message: message.to_owned(),
                    details,
                    request_id,
                },
            }),
        )
            .into_response()
    }
}

use axum::response::IntoResponse;

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn compact_summary(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(512)
        .collect()
}

fn invalid_json(_error: JsonRejection) -> M1Error {
    M1Error::bad("INVALID_JSON", "请求正文不是符合契约的 JSON", json!({}))
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_REQUEST_ID
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, M1Error> {
    let value = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .ok_or_else(|| {
            M1Error::bad(
                "IDEMPOTENCY_KEY_REQUIRED",
                "该操作需要 Idempotency-Key",
                json!({}),
            )
        })?;
    Ok(value.to_owned())
}

fn real_meta(request_id: impl Into<String>, freshness: Freshness, revision: i64) -> ApiMeta {
    let status = match freshness {
        Freshness::Fresh => DataSourceStatus::Fresh,
        Freshness::Stale => DataSourceStatus::Stale,
        Freshness::Unavailable => DataSourceStatus::Unavailable,
    };
    ApiMeta {
        request_id: request_id.into(),
        revision,
        generated_at: now(),
        freshness,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status,
            label: "SSH · Linux · 只读发现".to_owned(),
        },
    }
}

fn enum_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn parse_host_key_state(value: &str) -> HostKeyState {
    match value {
        "verified" => HostKeyState::Verified,
        "changed" => HostKeyState::Changed,
        _ => HostKeyState::Unverified,
    }
}

pub(crate) fn host_status_from_storage(value: &str) -> HostStatus {
    match value {
        "fingerprint_fetching" => HostStatus::FingerprintFetching,
        "host_key_unverified" => HostStatus::HostKeyUnverified,
        "host_key_verified" => HostStatus::HostKeyVerified,
        "host_key_changed" => HostStatus::HostKeyChanged,
        "connection_checking" => HostStatus::ConnectionChecking,
        "connection_ready" => HostStatus::ConnectionReady,
        "docker_unavailable" => HostStatus::DockerUnavailable,
        "docker_permission_denied" => HostStatus::DockerPermissionDenied,
        "discovery_running" => HostStatus::DiscoveryRunning,
        "evidence_ready" => HostStatus::EvidenceReady,
        "discovery_complete" => HostStatus::DiscoveryComplete,
        "discovery_partial" => HostStatus::DiscoveryPartial,
        "discovery_unavailable" => HostStatus::DiscoveryUnavailable,
        "failed" => HostStatus::Failed,
        _ => HostStatus::HostRegistered,
    }
}

fn host_record(host: &HostDb) -> HostRecord {
    HostRecord {
        host_id: host.host_id.clone(),
        display_name: host.display_name.clone(),
        address: host.address.clone(),
        port: host.port,
        ssh_user: host.ssh_user.clone(),
        credential_kind: credential_kind(&host.credential_ref).unwrap_or(SecretKind::SshKey),
        host_key_fingerprint: host.host_key_fingerprint.clone(),
        host_key_state: host.host_key_state.clone(),
        transport: "ssh".to_owned(),
        os: "linux".to_owned(),
        status: host.status.clone(),
        created_at: host.created_at.clone(),
        last_checked_at: host.last_checked_at.clone(),
        last_error_code: host.last_error_code.clone(),
        last_error_summary: host.last_error_summary.clone(),
    }
}

async fn load_last_host_error(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Option<(String, String)>, M1Error> {
    if let Some(row) = sqlx::query(
        "SELECT error_code, error_summary FROM connection_tests
         WHERE host_id = ?
         ORDER BY finished_at DESC, test_id DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?
    {
        let code: Option<String> = row.try_get("error_code").map_err(M1Error::Storage)?;
        let summary: Option<String> = row.try_get("error_summary").map_err(M1Error::Storage)?;
        if let Some(code) = code
            && !is_discovery_only_error(&code, summary.as_deref())
        {
            return Ok(Some((
                code,
                summary.unwrap_or_else(|| "SSH 连接检查失败".to_owned()),
            )));
        }
        return Ok(None);
    }
    Ok(None)
}

fn is_discovery_only_error(code: &str, summary: Option<&str>) -> bool {
    matches!(
        code,
        "DOCKER_PERMISSION_DENIED" | "DOCKER_UNAVAILABLE" | "COMPOSE_UNAVAILABLE"
    ) || (code == "SSH_PROCESS_FAILED"
        && summary
            .unwrap_or_default()
            .to_ascii_lowercase()
            .starts_with("docker_"))
}

async fn ensure_workspace(pool: &SqlitePool) -> Result<(), M1Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO workspaces(workspace_id, owner_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .bind("owner-local")
    .bind(now())
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(M1Error::Storage)
}

async fn load_host(pool: &SqlitePool, host_id: &str) -> Result<HostDb, M1Error> {
    let row = sqlx::query(
        "SELECT host_id, display_name, address, port, ssh_user, credential_ref,
                host_key_fingerprint, pending_host_key_fingerprint, pending_host_key_line,
                host_key_state, status, created_at, last_checked_at
         FROM hosts WHERE host_id = ?",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?
    .ok_or_else(|| M1Error::not_found("host", host_id))?;
    let port: i64 = row.try_get("port").map_err(M1Error::Storage)?;
    let last_error = load_last_host_error(pool, host_id).await?;
    Ok(HostDb {
        host_id: row.try_get("host_id").map_err(M1Error::Storage)?,
        display_name: row.try_get("display_name").map_err(M1Error::Storage)?,
        address: row.try_get("address").map_err(M1Error::Storage)?,
        port: u16::try_from(port).map_err(|_| M1Error::Internal)?,
        ssh_user: row.try_get("ssh_user").map_err(M1Error::Storage)?,
        credential_ref: row.try_get("credential_ref").map_err(M1Error::Storage)?,
        host_key_fingerprint: row
            .try_get("host_key_fingerprint")
            .map_err(M1Error::Storage)?,
        pending_host_key_fingerprint: row
            .try_get("pending_host_key_fingerprint")
            .map_err(M1Error::Storage)?,
        pending_host_key_line: row
            .try_get("pending_host_key_line")
            .map_err(M1Error::Storage)?,
        host_key_state: parse_host_key_state(
            row.try_get::<String, _>("host_key_state")
                .map_err(M1Error::Storage)?
                .as_str(),
        ),
        status: host_status_from_storage(
            row.try_get::<String, _>("status")
                .map_err(M1Error::Storage)?
                .as_str(),
        ),
        created_at: row.try_get("created_at").map_err(M1Error::Storage)?,
        last_checked_at: row.try_get("last_checked_at").map_err(M1Error::Storage)?,
        last_error_code: last_error.as_ref().map(|value| value.0.clone()),
        last_error_summary: last_error.map(|value| value.1),
    })
}

fn validate_display_name(value: &str) -> bool {
    let length = value.chars().count();
    (1..=120).contains(&length) && value.bytes().all(|byte| !byte.is_ascii_control())
}

fn validate_address(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 253
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || value.contains(['/', '\\', '@'])
    {
        return false;
    }
    if value.parse::<IpAddr>().is_ok() {
        return true;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    })
}

fn validate_user(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_credential_ref(value: &str) -> bool {
    ["secret://ssh/", "secret://ssh-password/"]
        .into_iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .and_then(|id| Uuid::parse_str(id).ok())
        .is_some()
}

fn credential_kind(value: &str) -> Option<SecretKind> {
    if value.starts_with("secret://ssh/") {
        Some(SecretKind::SshKey)
    } else if value.starts_with("secret://ssh-password/") {
        Some(SecretKind::SshPassword)
    } else {
        None
    }
}

fn validate_fingerprint(value: &str) -> bool {
    let payload = value.strip_prefix("SHA256:").unwrap_or_default();
    !payload.is_empty()
        && payload.len() <= 128
        && payload.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
        })
}

fn payload_sha256(value: impl AsRef<[u8]>) -> String {
    hex_digest(&Sha256::digest(value.as_ref()))
}

async fn replay_secret_ref(
    pool: &SqlitePool,
    key: &str,
    secret_payload: &[u8],
) -> Result<Option<SecretRefResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT secret_sha256, response_json FROM secret_ref_descriptors WHERE idempotency_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let recorded_hash: String = row.try_get("secret_sha256").map_err(M1Error::Storage)?;
    if !secret_payload_matches(recorded_hash, secret_payload.to_vec()).await? {
        return Err(M1Error::conflict(
            "IDEMPOTENCY_KEY_REUSED",
            "Idempotency-Key 已用于不同的秘密载荷",
            json!({}),
        ));
    }
    let payload: String = row.try_get("response_json").map_err(M1Error::Storage)?;
    serde_json::from_str(&payload)
        .map(Some)
        .map_err(|_| M1Error::Internal)
}

async fn secret_payload_matches(
    recorded_hash: String,
    secret_payload: Vec<u8>,
) -> Result<bool, M1Error> {
    if !recorded_hash.starts_with("$argon2id$") {
        return Ok(recorded_hash == payload_sha256(secret_payload));
    }
    tokio::task::spawn_blocking(move || {
        let parsed = PasswordHash::new(&recorded_hash).map_err(|_| M1Error::Internal)?;
        Ok(Argon2::default()
            .verify_password(&secret_payload, &parsed)
            .is_ok())
    })
    .await
    .map_err(|_| M1Error::Internal)?
}

async fn password_payload_hash(secret_payload: Vec<u8>) -> Result<String, M1Error> {
    tokio::task::spawn_blocking(move || {
        let salt =
            SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| M1Error::Internal)?;
        Argon2::default()
            .hash_password(&secret_payload, &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| M1Error::Internal)
    })
    .await
    .map_err(|_| M1Error::Internal)?
}

async fn replay_host_registration(
    pool: &SqlitePool,
    key: &str,
    request_sha256: &str,
) -> Result<Option<HostResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT request_sha256, response_json FROM host_registration_requests WHERE idempotency_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let recorded_hash: String = row.try_get("request_sha256").map_err(M1Error::Storage)?;
    if recorded_hash != request_sha256 {
        return Err(M1Error::conflict(
            "IDEMPOTENCY_KEY_REUSED",
            "Idempotency-Key 已用于不同的 HOST 登记载荷",
            json!({}),
        ));
    }
    let payload: String = row.try_get("response_json").map_err(M1Error::Storage)?;
    serde_json::from_str(&payload)
        .map(Some)
        .map_err(|_| M1Error::Internal)
}

#[utoipa::path(
    post,
    path = "/api/v1/secret-refs",
    tag = "m1",
    params(("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")),
    request_body = SecretRefCreateRequest,
    responses((status = 201, body = SecretRefResponse), (status = 400, body = ApiErrorResponse))
)]
pub async fn create_secret_ref(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<SecretRefCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<SecretRefResponse>), M1Error> {
    let request_id = request_id(&headers);
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let secret_payload = serde_json::to_vec(&request).map_err(|_| M1Error::Internal)?;
    if let Some(response) = replay_secret_ref(&state.pool, &key, &secret_payload).await? {
        return Ok((StatusCode::OK, Json(response)));
    }
    let secret_sha256 = if matches!(&request, SecretRefCreateRequest::SshPassword { .. }) {
        password_payload_hash(secret_payload).await?
    } else {
        payload_sha256(secret_payload)
    };
    let (stored, kind) = match request {
        SecretRefCreateRequest::SshKey { private_key } => (
            state
                .secrets
                .store_ssh_key(&private_key)
                .await
                .map_err(|error| match error {
                    SecretStoreError::InvalidKey | SecretStoreError::InvalidReference => {
                        M1Error::bad("INVALID_SSH_KEY", "请提供受支持格式的 SSH 私钥", json!({}))
                    }
                    SecretStoreError::InvalidModelKey
                    | SecretStoreError::InvalidPassword
                    | SecretStoreError::NotFound
                    | SecretStoreError::Io(_) => M1Error::SecretStore,
                })?,
            SecretKind::SshKey,
        ),
        SecretRefCreateRequest::SshPassword { password } => (
            state
                .secrets
                .store_ssh_password(&password)
                .await
                .map_err(|error| match error {
                    SecretStoreError::InvalidPassword | SecretStoreError::InvalidReference => {
                        M1Error::bad(
                            "INVALID_SSH_PASSWORD",
                            "SSH 密码不能为空、过长或包含换行",
                            json!({}),
                        )
                    }
                    SecretStoreError::InvalidKey
                    | SecretStoreError::InvalidModelKey
                    | SecretStoreError::NotFound
                    | SecretStoreError::Io(_) => M1Error::SecretStore,
                })?,
            SecretKind::SshPassword,
        ),
        SecretRefCreateRequest::ModelKey { api_key } => (
            state
                .secrets
                .store_model_key(&api_key)
                .await
                .map_err(|error| match error {
                    SecretStoreError::InvalidModelKey | SecretStoreError::InvalidReference => {
                        M1Error::bad(
                            "INVALID_MODEL_KEY",
                            "模型 Key 不能为空或包含控制字符",
                            json!({}),
                        )
                    }
                    SecretStoreError::InvalidKey
                    | SecretStoreError::InvalidPassword
                    | SecretStoreError::NotFound
                    | SecretStoreError::Io(_) => M1Error::SecretStore,
                })?,
            SecretKind::ModelKey,
        ),
    };
    let response = SecretRefResponse {
        data: SecretRefDescriptor {
            credential_ref: stored.credential_ref,
            kind: kind.clone(),
            created_at: stored.created_at,
        },
        meta: real_meta(request_id, Freshness::Fresh, 1),
    };
    let response_json = match serde_json::to_string(&response) {
        Ok(value) => value,
        Err(_) => {
            let _ = state.secrets.delete(&response.data.credential_ref).await;
            return Err(M1Error::Internal);
        }
    };
    let persisted = sqlx::query(
        "INSERT INTO secret_ref_descriptors(
            credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&response.data.credential_ref)
    .bind(enum_string(&kind))
    .bind(&key)
    .bind(&secret_sha256)
    .bind(response_json)
    .bind(&response.data.created_at)
    .execute(&state.pool)
    .await;
    if let Err(error) = persisted {
        let _ = state.secrets.delete(&response.data.credential_ref).await;
        return Err(M1Error::Storage(error));
    }
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/hosts",
    tag = "m1",
    params(("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")),
    request_body = HostCreateRequest,
    responses((status = 201, body = HostResponse), (status = 400, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn create_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<HostCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<HostResponse>), M1Error> {
    let request_id = request_id(&headers);
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let request_sha256 =
        payload_sha256(serde_json::to_vec(&request).map_err(|_| M1Error::Internal)?);
    if let Some(response) = replay_host_registration(&state.pool, &key, &request_sha256).await? {
        return Ok((StatusCode::OK, Json(response)));
    }
    if !validate_display_name(&request.display_name)
        || !validate_address(&request.address)
        || !validate_user(&request.ssh_user)
        || !validate_credential_ref(&request.credential_ref)
    {
        return Err(M1Error::bad(
            "INVALID_HOST",
            "HOST 字段不符合 Linux SSH 登记规则",
            json!({}),
        ));
    }
    if !state.secrets.contains(&request.credential_ref).await {
        return Err(M1Error::bad(
            "CREDENTIAL_NOT_FOUND",
            "credential_ref 未指向服务端已保存的 SSH 凭据",
            json!({"credential_ref": request.credential_ref}),
        ));
    }
    ensure_workspace(&state.pool).await?;
    if let Some(row) = sqlx::query(
        "SELECT host_id FROM hosts WHERE workspace_id = ? AND address = ? AND port = ? AND ssh_user = ?",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .bind(&request.address)
    .bind(i64::from(request.port))
    .bind(&request.ssh_user)
    .fetch_optional(&state.pool)
    .await
    .map_err(M1Error::Storage)?
    {
        let existing_id: String = row.try_get("host_id").map_err(M1Error::Storage)?;
        let existing = load_host(&state.pool, &existing_id).await?;
        if existing.display_name != request.display_name
            || existing.credential_ref != request.credential_ref
        {
            return Err(M1Error::conflict(
                "HOST_ALREADY_EXISTS",
                "相同地址、端口和用户的 HOST 已登记；修改需使用后续编辑入口",
                json!({"host_id": existing.host_id}),
            ));
        }
        let response = HostResponse {
                data: host_record(&existing),
                meta: real_meta(request_id, Freshness::Fresh, 1),
            };
        sqlx::query(
            "INSERT INTO host_registration_requests(
                idempotency_key, request_sha256, host_id, response_json, created_at
             ) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&key)
        .bind(&request_sha256)
        .bind(&existing.host_id)
        .bind(serde_json::to_string(&response).map_err(|_| M1Error::Internal)?)
        .bind(now())
        .execute(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
        return Ok((StatusCode::OK, Json(response)));
    }
    let host_id = Uuid::new_v4().to_string();
    let created_at = now();
    let response = HostResponse {
        data: HostRecord {
            host_id: host_id.clone(),
            display_name: request.display_name.clone(),
            address: request.address.clone(),
            port: request.port,
            ssh_user: request.ssh_user.clone(),
            credential_kind: credential_kind(&request.credential_ref).ok_or_else(|| {
                M1Error::bad("INVALID_HOST", "HOST 的 SSH 凭据类型无效", json!({}))
            })?,
            host_key_fingerprint: None,
            host_key_state: HostKeyState::Unverified,
            transport: "ssh".to_owned(),
            os: "linux".to_owned(),
            status: HostStatus::HostRegistered,
            created_at: created_at.clone(),
            last_checked_at: None,
            last_error_code: None,
            last_error_summary: None,
        },
        meta: real_meta(request_id, Freshness::Fresh, 1),
    };
    let response_json = serde_json::to_string(&response).map_err(|_| M1Error::Internal)?;
    let mut tx = state.pool.begin().await.map_err(M1Error::Storage)?;
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, 'unverified', 'ssh', 'linux', 'host_registered', ?)",
    )
    .bind(&host_id)
    .bind(DEFAULT_WORKSPACE_ID)
    .bind(&request.display_name)
    .bind(&request.address)
    .bind(i64::from(request.port))
    .bind(&request.ssh_user)
    .bind(&request.credential_ref)
    .bind(&created_at)
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    sqlx::query(
        "INSERT INTO host_registration_requests(
            idempotency_key, request_sha256, host_id, response_json, created_at
         ) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&key)
    .bind(&request_sha256)
    .bind(&host_id)
    .bind(response_json)
    .bind(&created_at)
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    tx.commit().await.map_err(M1Error::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts",
    tag = "m1",
    responses((status = 200, body = HostListResponse))
)]
pub async fn list_hosts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<HostListResponse>, M1Error> {
    let request_id = request_id(&headers);
    ensure_workspace(&state.pool).await?;
    let rows = sqlx::query(
        "SELECT host_id FROM hosts WHERE workspace_id = ? ORDER BY created_at, host_id",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(M1Error::Storage)?;
    let mut hosts = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("host_id").map_err(M1Error::Storage)?;
        hosts.push(host_record(&load_host(&state.pool, &id).await?));
    }
    Ok(Json(HostListResponse {
        data: hosts,
        meta: real_meta(request_id, Freshness::Fresh, 1),
    }))
}

/// Read model for the dedicated global server asset page. This intentionally
/// derives deployment and project counts from the latest retained evidence or
/// projection snapshot; it does not create a second ownership model for HOST.
#[utoipa::path(
    get,
    path = "/api/v1/views/global/hosts",
    tag = "m1",
    responses((status = 200, body = GlobalHostsViewResponse))
)]
pub async fn get_global_hosts_view(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<GlobalHostsViewResponse>, M1Error> {
    ensure_workspace(&state.pool).await?;
    let rows = sqlx::query(
        "SELECT host_id FROM hosts WHERE workspace_id = ? ORDER BY created_at, host_id",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(M1Error::Storage)?;

    let mut hosts = Vec::with_capacity(rows.len());
    for row in rows {
        let host_id: String = row.try_get("host_id").map_err(M1Error::Storage)?;
        let host = load_host(&state.pool, &host_id).await?;
        let latest_run = sqlx::query(
            "SELECT run_id, state, failure_code, failure_summary
             FROM discovery_runs WHERE host_id = ? ORDER BY submitted_at DESC, rowid DESC LIMIT 1",
        )
        .bind(&host_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
        let evidence_rows = sqlx::query(
            "SELECT run_id, state, evidence_retention, evidence_json
             FROM discovery_runs
             WHERE host_id = ? AND evidence_json IS NOT NULL
             ORDER BY submitted_at DESC, rowid DESC",
        )
        .bind(&host_id)
        .fetch_all(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
        let latest_projection_draft = latest_valid_projection_draft(&state.pool, &host_id).await?;
        let latest_projection_draft_id = latest_projection_draft
            .as_ref()
            .map(|(draft_id, _)| draft_id.clone());
        let (latest_monitor_run, current_snapshot) =
            monitoring_api::latest_for_host(&state.pool, &host_id).await?;
        let monitor_freshness = current_snapshot
            .as_ref()
            .map(|snapshot| snapshot.freshness.clone())
            .unwrap_or(MonitorFreshness::Unknown);
        let monitor_unknown_count = current_snapshot
            .as_ref()
            .map(|snapshot| snapshot.unknown_count)
            .unwrap_or(8);
        let current_snapshot_run_id = current_snapshot
            .as_ref()
            .map(|snapshot| snapshot.run_id.clone());
        let monitor_observed_at = current_snapshot
            .as_ref()
            .map(|snapshot| snapshot.observed_at.clone());
        let monitor_schedule = sqlx::query(
            "SELECT state, next_due_at, last_due_at FROM monitor_schedules
             WHERE host_id = ? AND state != 'archived'
             ORDER BY created_at DESC, schedule_id DESC LIMIT 1",
        )
        .bind(&host_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
        let monitor_schedule_state = monitor_schedule
            .as_ref()
            .map(|row| row.try_get::<String, _>("state"))
            .transpose()
            .map_err(M1Error::Storage)?
            .map(|value| match value.as_str() {
                "enabled" => MonitorScheduleState::Enabled,
                "archived" => MonitorScheduleState::Archived,
                _ => MonitorScheduleState::Paused,
            });
        let monitor_schedule_next_due_at = monitor_schedule
            .as_ref()
            .map(|row| row.try_get::<Option<String>, _>("next_due_at"))
            .transpose()
            .map_err(M1Error::Storage)?
            .flatten();
        let monitor_schedule_last_due_at = monitor_schedule
            .as_ref()
            .map(|row| row.try_get::<Option<String>, _>("last_due_at"))
            .transpose()
            .map_err(M1Error::Storage)?
            .flatten();

        let mut discovery_state = None;
        let mut latest_discovery_run_id = None;
        let mut latest_evidence_run_id = None;
        let mut provider_coverage = Vec::new();
        let mut deployment_count = 0u32;
        let project_count = latest_project_count(
            &state.pool,
            &host_id,
            latest_projection_draft
                .as_ref()
                .map(|(_, snapshot)| snapshot),
        )
        .await?;
        let mut last_observed_at = None;
        let mut freshness = Freshness::Unavailable;
        let mut attention_count = u32::from(matches!(
            host.status,
            HostStatus::Failed | HostStatus::HostKeyChanged
        ));

        if let Some(row) = latest_run {
            latest_discovery_run_id = Some(row.try_get("run_id").map_err(M1Error::Storage)?);
            let state_value: String = row.try_get("state").map_err(M1Error::Storage)?;
            discovery_state = Some(parse_discovery_state(&state_value));
            let failure_code: Option<String> =
                row.try_get("failure_code").map_err(M1Error::Storage)?;
            let failure_summary: Option<String> =
                row.try_get("failure_summary").map_err(M1Error::Storage)?;
            if failure_code.is_some() || failure_summary.is_some() {
                attention_count = attention_count.saturating_add(1);
            }
        }
        let mut invalid_evidence_seen = false;
        for row in evidence_rows {
            let payload: String = row.try_get("evidence_json").map_err(M1Error::Storage)?;
            let Ok(evidence) = serde_json::from_str::<DiscoveryEvidence>(&payload) else {
                invalid_evidence_seen = true;
                continue;
            };
            latest_evidence_run_id = Some(row.try_get("run_id").map_err(M1Error::Storage)?);
            let state_value: String = row.try_get("state").map_err(M1Error::Storage)?;
            let evidence_state = parse_discovery_state(&state_value);
            let retention: String = row
                .try_get("evidence_retention")
                .map_err(M1Error::Storage)?;
            deployment_count = u32::try_from(
                evidence
                    .compose_projects
                    .len()
                    .saturating_add(evidence.systemd_units.len()),
            )
            .unwrap_or(u32::MAX);
            provider_coverage = provider_coverage_from_evidence(&evidence);
            last_observed_at = Some(evidence.finished_at.clone());
            freshness = discovery_freshness(&evidence_state, &retention);
            attention_count = attention_count
                .saturating_add(u32::try_from(evidence.warnings.len()).unwrap_or(u32::MAX));
            break;
        }
        if invalid_evidence_seen {
            attention_count = attention_count.saturating_add(1);
        }
        if latest_monitor_run.as_ref().is_some_and(|run| {
            matches!(
                run.state,
                crate::contracts::MonitorRunState::Failed
                    | crate::contracts::MonitorRunState::TimedOut
                    | crate::contracts::MonitorRunState::Interrupted
            )
        }) {
            attention_count = attention_count.saturating_add(1);
        }

        hosts.push(HostAssetRecord {
            host: host_record(&host),
            connection_state: host_connection_state(&host.status),
            discovery_state,
            latest_discovery_run_id,
            latest_evidence_run_id,
            latest_projection_draft_id,
            latest_monitor_run,
            current_snapshot_run_id,
            monitor_observed_at,
            monitor_freshness,
            monitor_unknown_count,
            monitor_schedule_state,
            monitor_schedule_next_due_at,
            monitor_schedule_last_due_at,
            provider_coverage,
            deployment_count,
            project_count,
            last_observed_at,
            freshness,
            attention_count,
        });
    }

    let host_count = u32::try_from(hosts.len()).unwrap_or(u32::MAX);
    let connection_ready_count = u32::try_from(
        hosts
            .iter()
            .filter(|asset| asset.connection_state == HostStatus::ConnectionReady)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let connection_failed_count = u32::try_from(
        hosts
            .iter()
            .filter(|asset| {
                matches!(
                    asset.connection_state,
                    HostStatus::Failed | HostStatus::HostKeyChanged
                )
            })
            .count(),
    )
    .unwrap_or(u32::MAX);
    let discovery_partial_count = u32::try_from(
        hosts
            .iter()
            .filter(|asset| asset.discovery_state == Some(DiscoveryRunState::DiscoveryPartial))
            .count(),
    )
    .unwrap_or(u32::MAX);
    let stale_evidence_count = u32::try_from(
        hosts
            .iter()
            .filter(|asset| asset.freshness == Freshness::Stale)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let unknown_count = u32::try_from(
        hosts
            .iter()
            .filter(|asset| asset.freshness == Freshness::Unavailable)
            .count(),
    )
    .unwrap_or(u32::MAX);

    Ok(Json(GlobalHostsViewResponse {
        data: GlobalHostsViewData {
            host_count,
            connection_ready_count,
            connection_failed_count,
            discovery_partial_count,
            stale_evidence_count,
            unknown_count,
            hosts,
        },
        meta: real_meta(request_id(&headers), Freshness::Fresh, 1),
    }))
}

fn host_connection_state(status: &HostStatus) -> HostStatus {
    match status {
        HostStatus::ConnectionReady
        | HostStatus::DockerUnavailable
        | HostStatus::DockerPermissionDenied
        | HostStatus::DiscoveryRunning
        | HostStatus::EvidenceReady
        | HostStatus::DiscoveryComplete
        | HostStatus::DiscoveryPartial
        | HostStatus::DiscoveryUnavailable => HostStatus::ConnectionReady,
        other => other.clone(),
    }
}

fn provider_coverage_from_evidence(evidence: &DiscoveryEvidence) -> Vec<DiscoveryProviderCoverage> {
    if !evidence.provider_results.is_empty() {
        return evidence.provider_results.clone();
    }
    let observed_at = Some(evidence.finished_at.clone());
    let warnings = evidence.warnings.clone();
    let docker_refs = evidence
        .docker_engines
        .iter()
        .chain(&evidence.containers)
        .chain(&evidence.images)
        .chain(&evidence.networks)
        .chain(&evidence.volumes)
        .map(|item| item.external_id.clone())
        .collect::<Vec<_>>();
    let compose_refs = evidence
        .compose_projects
        .iter()
        .map(|item| item.external_id.clone())
        .collect::<Vec<_>>();
    let document_refs = evidence
        .document_candidates
        .iter()
        .map(|item| item.external_id.clone())
        .collect::<Vec<_>>();
    let systemd_refs = evidence
        .systemd_units
        .iter()
        .map(|item| item.external_id.clone())
        .collect::<Vec<_>>();
    let mut coverage = vec![
        DiscoveryProviderCoverage {
            provider_kind: "docker".to_owned(),
            status: provider_status(
                &warnings,
                &["DOCKER_PERMISSION_DENIED"],
                &["DOCKER_UNAVAILABLE"],
            ),
            observed_count: u32::try_from(docker_refs.len()).unwrap_or(u32::MAX),
            evidence_refs: docker_refs,
            warnings: provider_warnings(&warnings, &["DOCKER_"]),
            observed_at: observed_at.clone(),
        },
        DiscoveryProviderCoverage {
            provider_kind: "compose".to_owned(),
            status: provider_status(
                &warnings,
                &["DOCKER_PERMISSION_DENIED"],
                &["COMPOSE_UNAVAILABLE"],
            ),
            observed_count: u32::try_from(compose_refs.len()).unwrap_or(u32::MAX),
            evidence_refs: compose_refs,
            warnings: provider_warnings(&warnings, &["COMPOSE_"]),
            observed_at: observed_at.clone(),
        },
    ];
    let systemd_warnings = provider_warnings(&warnings, &["SYSTEMD_"]);
    if !systemd_refs.is_empty() || !systemd_warnings.is_empty() {
        coverage.push(DiscoveryProviderCoverage {
            provider_kind: "systemd".to_owned(),
            status: provider_status(
                &warnings,
                &["SYSTEMD_PERMISSION_DENIED"],
                &["SYSTEMD_UNAVAILABLE"],
            ),
            observed_count: u32::try_from(systemd_refs.len()).unwrap_or(u32::MAX),
            evidence_refs: systemd_refs,
            warnings: systemd_warnings,
            observed_at: observed_at.clone(),
        });
    }
    let document_warnings = provider_warnings(&warnings, &["DOCUMENT_"]);
    if !document_refs.is_empty() || !document_warnings.is_empty() {
        coverage.push(DiscoveryProviderCoverage {
            provider_kind: "documents".to_owned(),
            status: provider_status(&warnings, &[], &["DOCUMENT_UNAVAILABLE"]),
            observed_count: u32::try_from(document_refs.len()).unwrap_or(u32::MAX),
            evidence_refs: document_refs,
            warnings: document_warnings,
            observed_at,
        });
    }
    coverage
}

fn provider_status(
    warnings: &[EvidenceWarning],
    permission_codes: &[&str],
    unavailable_codes: &[&str],
) -> DiscoveryProviderStatus {
    if warnings
        .iter()
        .any(|warning| permission_codes.contains(&warning.code.as_str()))
    {
        DiscoveryProviderStatus::PermissionDenied
    } else if warnings
        .iter()
        .any(|warning| unavailable_codes.contains(&warning.code.as_str()))
    {
        DiscoveryProviderStatus::Unavailable
    } else {
        DiscoveryProviderStatus::Ready
    }
}

fn provider_warnings(warnings: &[EvidenceWarning], prefixes: &[&str]) -> Vec<EvidenceWarning> {
    warnings
        .iter()
        .filter(|warning| {
            prefixes
                .iter()
                .any(|prefix| warning.code.starts_with(prefix))
        })
        .cloned()
        .collect()
}

async fn latest_valid_projection_draft(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Option<(String, GraphSnapshot)>, M1Error> {
    let rows = sqlx::query(
        "SELECT draft.draft_id, draft.snapshot_json
         FROM projection_drafts draft
         JOIN discovery_runs run
           ON run.run_id = draft.discovery_run_id AND run.host_id = draft.host_id
         WHERE draft.host_id = ? AND draft.state IN ('draft', 'confirmed')
         ORDER BY draft.updated_at DESC, draft.rowid DESC",
    )
    .bind(host_id)
    .fetch_all(pool)
    .await
    .map_err(M1Error::Storage)?;
    for row in rows {
        let payload: String = row.try_get("snapshot_json").map_err(M1Error::Storage)?;
        if let Ok(snapshot) = serde_json::from_str::<GraphSnapshot>(&payload) {
            let draft_id = row.try_get("draft_id").map_err(M1Error::Storage)?;
            return Ok(Some((draft_id, snapshot)));
        }
    }
    Ok(None)
}

async fn latest_project_count(
    pool: &SqlitePool,
    host_id: &str,
    latest_draft: Option<&GraphSnapshot>,
) -> Result<u32, M1Error> {
    let row = sqlx::query(
        "SELECT snapshot_json FROM projection_versions WHERE host_id = ?
         ORDER BY confirmed_at DESC, rowid DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    if let Some(row) = row {
        let payload: String = row.try_get("snapshot_json").map_err(M1Error::Storage)?;
        let snapshot =
            serde_json::from_str::<GraphSnapshot>(&payload).map_err(|_| M1Error::Internal)?;
        return Ok(project_count_from_snapshot(&snapshot));
    }
    Ok(latest_draft.map(project_count_from_snapshot).unwrap_or(0))
}

fn project_count_from_snapshot(snapshot: &GraphSnapshot) -> u32 {
    let mut projects = std::collections::BTreeSet::new();
    for node in &snapshot.nodes {
        if node.kind == GraphNodeKind::Project {
            projects.insert(node.project_id.as_ref().unwrap_or(&node.id));
        }
    }
    u32::try_from(projects.len()).unwrap_or(u32::MAX)
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}",
    tag = "m1",
    params(("host_id" = String, Path, description = "Registered Linux host identifier")),
    responses((status = 200, body = HostResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<Json<HostResponse>, M1Error> {
    let request_id = request_id(&headers);
    let host = load_host(&state.pool, &host_id).await?;
    Ok(Json(HostResponse {
        data: host_record(&host),
        meta: real_meta(request_id, Freshness::Fresh, 1),
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/hosts/{host_id}",
    tag = "m1",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = HostUpdateRequest,
    responses(
        (status = 200, body = HostResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse)
    )
)]
pub async fn update_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    payload: Result<Json<HostUpdateRequest>, JsonRejection>,
) -> Result<Json<HostResponse>, M1Error> {
    let request_id = request_id(&headers);
    let key = idempotency_key(&headers)?;
    let Json(request) = payload.map_err(invalid_json)?;
    let host = load_host(&state.pool, &host_id).await?;
    let display_name = request
        .display_name
        .as_deref()
        .unwrap_or(&host.display_name)
        .trim()
        .to_owned();
    let address = request
        .address
        .as_deref()
        .unwrap_or(&host.address)
        .trim()
        .to_owned();
    let port = request.port.unwrap_or(host.port);
    let ssh_user = request
        .ssh_user
        .as_deref()
        .unwrap_or(&host.ssh_user)
        .trim()
        .to_owned();
    let credential_ref = request
        .credential_ref
        .as_deref()
        .unwrap_or(&host.credential_ref)
        .to_owned();
    if !validate_display_name(&display_name)
        || !validate_address(&address)
        || !validate_user(&ssh_user)
        || request
            .credential_ref
            .as_deref()
            .is_some_and(|value| !validate_credential_ref(value))
    {
        return Err(M1Error::bad(
            "INVALID_HOST",
            "服务器连接信息不符合 Linux SSH 登记规则",
            json!({"host_id": host_id}),
        ));
    }
    if request.credential_ref.is_some() && !state.secrets.contains(&credential_ref).await {
        return Err(M1Error::bad(
            "CREDENTIAL_NOT_FOUND",
            "credential_ref 未指向服务端已保存的 SSH 凭据",
            json!({"host_id": host_id}),
        ));
    }
    let connection_changed = host.address != address
        || host.port != port
        || host.ssh_user != ssh_user
        || host.credential_ref != credential_ref;
    let request_sha256 =
        payload_sha256(serde_json::to_vec(&request).map_err(|_| M1Error::Internal)?);
    if let Some(row) = sqlx::query(
        "SELECT request_sha256, response_json FROM projection_mutation_requests
         WHERE resource_kind = 'host_connection' AND resource_id = ? AND idempotency_key = ?",
    )
    .bind(&host_id)
    .bind(&key)
    .fetch_optional(&state.pool)
    .await
    .map_err(M1Error::Storage)?
    {
        let recorded: String = row.try_get("request_sha256").map_err(M1Error::Storage)?;
        if recorded != request_sha256 {
            return Err(M1Error::conflict(
                "IDEMPOTENCY_KEY_REUSED",
                "Idempotency-Key 已用于不同的服务器连接修改",
                json!({"host_id": host_id}),
            ));
        }
        let payload: String = row.try_get("response_json").map_err(M1Error::Storage)?;
        let response = serde_json::from_str(&payload).map_err(|_| M1Error::Internal)?;
        return Ok(Json(response));
    }
    let mut updated = host;
    updated.display_name = display_name.clone();
    updated.address = address.clone();
    updated.port = port;
    updated.ssh_user = ssh_user.clone();
    updated.credential_ref = credential_ref.clone();
    if connection_changed {
        updated.host_key_fingerprint = None;
        updated.pending_host_key_fingerprint = None;
        updated.pending_host_key_line = None;
        updated.host_key_state = HostKeyState::Unverified;
        updated.status = HostStatus::HostRegistered;
        updated.last_checked_at = None;
        updated.last_error_code = None;
        updated.last_error_summary = None;
    }
    let response = HostResponse {
        data: host_record(&updated),
        meta: real_meta(request_id, Freshness::Fresh, 1),
    };
    let mut tx = state.pool.begin().await.map_err(M1Error::Storage)?;
    let update = sqlx::query(
        "UPDATE hosts SET display_name = ?, address = ?, port = ?, ssh_user = ?, credential_ref = ?,
            host_key_fingerprint = CASE WHEN ? THEN NULL ELSE host_key_fingerprint END,
            pending_host_key_fingerprint = CASE WHEN ? THEN NULL ELSE pending_host_key_fingerprint END,
            pending_host_key_line = CASE WHEN ? THEN NULL ELSE pending_host_key_line END,
            host_key_state = CASE WHEN ? THEN 'unverified' ELSE host_key_state END,
            status = CASE WHEN ? THEN 'host_registered' ELSE status END,
            last_checked_at = CASE WHEN ? THEN NULL ELSE last_checked_at END
         WHERE host_id = ?",
    )
        .bind(&display_name)
        .bind(&address)
        .bind(i64::from(port))
        .bind(&ssh_user)
        .bind(&credential_ref)
        .bind(connection_changed)
        .bind(connection_changed)
        .bind(connection_changed)
        .bind(connection_changed)
        .bind(connection_changed)
        .bind(connection_changed)
        .bind(&host_id)
        .execute(&mut *tx)
        .await;
    if let Err(error) = update {
        if error.to_string().to_ascii_lowercase().contains("unique") {
            return Err(M1Error::conflict(
                "HOST_ALREADY_EXISTS",
                "相同地址、端口和 SSH 用户的服务器已经登记",
                json!({"host_id": host_id}),
            ));
        }
        return Err(M1Error::Storage(error));
    }
    sqlx::query(
        "INSERT INTO projection_mutation_requests(
            request_id, resource_kind, resource_id, idempotency_key,
            request_sha256, response_json, created_at
         ) VALUES (?, 'host_connection', ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&host_id)
    .bind(&key)
    .bind(&request_sha256)
    .bind(serde_json::to_string(&response).map_err(|_| M1Error::Internal)?)
    .bind(now())
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    tx.commit().await.map_err(M1Error::Storage)?;
    if connection_changed {
        let known_hosts = state.ssh.known_hosts_path(&host_id);
        match tokio::fs::remove_file(&known_hosts).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(host_id = %host_id, error = %error, "could not clear stale known_hosts entry")
            }
        }
    }
    if let Err(error) = events::publish(
        &state.pool,
        ChangeEventKind::HostConnectionChanged,
        &format!("host:{host_id}"),
        0,
        json!({"state": if connection_changed { "host_registered" } else { "metadata_updated" }}),
    )
    .await
    {
        tracing::warn!(host_id = %host_id, error = %error, "could not publish host update event");
    }
    Ok(Json(response))
}

fn connection_state_name(state: &ConnectionTestState) -> &'static str {
    match state {
        ConnectionTestState::HostKeyUnverified => "host_key_unverified",
        ConnectionTestState::HostKeyVerified => "host_key_verified",
        ConnectionTestState::HostKeyChanged => "host_key_changed",
        ConnectionTestState::ConnectionReady => "connection_ready",
        ConnectionTestState::DockerUnavailable => "docker_unavailable",
        ConnectionTestState::DockerPermissionDenied => "docker_permission_denied",
        ConnectionTestState::Failed => "failed",
    }
}

async fn replay_connection_test(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
) -> Result<Option<ConnectionTestResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT connection_tests.response_json, hosts.port, hosts.ssh_user, hosts.credential_ref
         FROM connection_tests
         JOIN hosts ON hosts.host_id = connection_tests.host_id
         WHERE connection_tests.host_id = ? AND connection_tests.idempotency_key = ?",
    )
    .bind(host_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    row.map(|row| {
        let payload: String = row.try_get("response_json").map_err(M1Error::Storage)?;
        let mut payload: Value = serde_json::from_str(&payload).map_err(|_| M1Error::Internal)?;
        let data = payload
            .get_mut("data")
            .and_then(Value::as_object_mut)
            .ok_or(M1Error::Internal)?;

        // Connection test responses persisted before these audit fields existed
        // must remain replayable under the same idempotency key.
        if !data.contains_key("port") {
            let port: i64 = row.try_get("port").map_err(M1Error::Storage)?;
            data.insert("port".to_owned(), json!(port));
        }
        if !data.contains_key("ssh_user") {
            let ssh_user: String = row.try_get("ssh_user").map_err(M1Error::Storage)?;
            data.insert("ssh_user".to_owned(), json!(ssh_user));
        }
        if !data.contains_key("credential_kind") {
            let credential_ref: String = row.try_get("credential_ref").map_err(M1Error::Storage)?;
            let kind = credential_kind(&credential_ref).ok_or(M1Error::Internal)?;
            data.insert(
                "credential_kind".to_owned(),
                serde_json::to_value(kind).map_err(|_| M1Error::Internal)?,
            );
        }
        if !data.contains_key("auth_transport") {
            data.insert("auth_transport".to_owned(), Value::Null);
        }
        serde_json::from_value(payload).map_err(|_| M1Error::Internal)
    })
    .transpose()
}

async fn persist_connection_test(
    pool: &SqlitePool,
    key: &str,
    response: &ConnectionTestResponse,
    started_at: &str,
    finished_at: &str,
) -> Result<(), M1Error> {
    let data = &response.data;
    sqlx::query(
        "INSERT INTO connection_tests(
            test_id, host_id, request_id, idempotency_key, state, candidate_fingerprint,
            capabilities_json, error_code, error_summary, response_json, started_at, finished_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&data.test_id)
    .bind(&data.host_id)
    .bind(&data.request_id)
    .bind(key)
    .bind(connection_state_name(&data.state))
    .bind(&data.candidate_fingerprint)
    .bind(serde_json::to_string(&data.capabilities).map_err(|_| M1Error::Internal)?)
    .bind(&data.error_code)
    .bind(&data.error_summary)
    .bind(serde_json::to_string(response).map_err(|_| M1Error::Internal)?)
    .bind(started_at)
    .bind(finished_at)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(M1Error::Storage)
}

struct ConnectionOutcome {
    state: ConnectionTestState,
    candidate_fingerprint: Option<String>,
    capabilities: Vec<String>,
    error_code: Option<String>,
    error_summary: Option<String>,
    auth_transport: Option<SshAuthTransport>,
}

fn contract_auth_transport(value: InternalSshAuthTransport) -> SshAuthTransport {
    match value {
        InternalSshAuthTransport::RusshClient => SshAuthTransport::RusshClient,
        InternalSshAuthTransport::OpensshAskpass => SshAuthTransport::OpensshAskpass,
        InternalSshAuthTransport::OpensshKey => SshAuthTransport::OpensshKey,
    }
}

fn connection_response(
    request_id: &str,
    host: &HostDb,
    started_at: String,
    finished_at: String,
    outcome: ConnectionOutcome,
) -> ConnectionTestResponse {
    let freshness = if matches!(
        outcome.state,
        ConnectionTestState::Failed
            | ConnectionTestState::DockerUnavailable
            | ConnectionTestState::DockerPermissionDenied
    ) {
        Freshness::Unavailable
    } else {
        Freshness::Fresh
    };
    ConnectionTestResponse {
        data: ConnectionTestData {
            request_id: request_id.to_owned(),
            test_id: Uuid::new_v4().to_string(),
            host_id: host.host_id.clone(),
            port: host.port,
            ssh_user: host.ssh_user.clone(),
            credential_kind: credential_kind(&host.credential_ref).unwrap_or(SecretKind::SshKey),
            auth_transport: outcome.auth_transport,
            state: outcome.state,
            candidate_fingerprint: outcome.candidate_fingerprint,
            capabilities: outcome.capabilities,
            error_code: outcome.error_code,
            error_summary: outcome.error_summary,
            started_at,
            finished_at,
        },
        meta: real_meta(request_id.to_owned(), freshness, 1),
    }
}

fn ssh_failure_code(failure: SshFailure) -> (&'static str, &'static str, StatusCode) {
    match failure {
        SshFailure::Unreachable => (
            "SSH_UNREACHABLE",
            "无法连接到目标 HOST",
            StatusCode::BAD_GATEWAY,
        ),
        SshFailure::Authentication => (
            "SSH_AUTH_FAILED",
            "SSH 用户、密码或私钥认证失败",
            StatusCode::BAD_GATEWAY,
        ),
        SshFailure::HostKey => (
            "HOST_KEY_CONFLICT",
            "HOST 主机指纹校验未通过",
            StatusCode::CONFLICT,
        ),
        SshFailure::Timeout => ("SSH_TIMEOUT", "SSH 操作超时", StatusCode::GATEWAY_TIMEOUT),
        SshFailure::OutputLimit => (
            "SSH_OUTPUT_LIMIT",
            "SSH 输出超过上限",
            StatusCode::BAD_GATEWAY,
        ),
        SshFailure::Process => (
            "SSH_PROCESS_FAILED",
            "SSH 只读检查进程失败",
            StatusCode::BAD_GATEWAY,
        ),
    }
}

fn synthetic_ssh_error(failure: SshFailure, summary: &str) -> SshError {
    SshError {
        failure,
        summary: summary.to_owned(),
        exit_code: None,
        output_bytes: 0,
        auth_transport: None,
    }
}

struct ConnectionProbeSuccess {
    capabilities: Vec<String>,
    auth_transport: SshAuthTransport,
}

async fn connection_probe(
    state: &AppState,
    host: &HostDb,
) -> Result<ConnectionProbeSuccess, SshError> {
    let credential = resolve_ssh_credential(state, &host.credential_ref).await?;
    let target = SshTarget {
        host_id: host.host_id.clone(),
        address: host.address.clone(),
        port: host.port,
        user: host.ssh_user.clone(),
        credential,
    };
    let identity = state
        .ssh
        .execute(&target, LINUX_IDENTITY_COMMAND, 64 * 1024)
        .await?;
    if !identity
        .stdout
        .lines()
        .next()
        .is_some_and(|line| line.trim().eq_ignore_ascii_case("linux"))
    {
        return Err(SshError {
            auth_transport: Some(identity.auth_transport),
            ..synthetic_ssh_error(SshFailure::Process, "target did not report Linux")
        });
    }
    Ok(ConnectionProbeSuccess {
        capabilities: vec!["ssh".to_owned(), "linux".to_owned()],
        auth_transport: contract_auth_transport(identity.auth_transport),
    })
}

async fn resolve_ssh_credential(
    state: &AppState,
    credential_ref: &str,
) -> Result<SshCredential, SshError> {
    if credential_ref.starts_with("secret://ssh-password/") {
        return state
            .secrets
            .resolve_ssh_password(credential_ref)
            .await
            .map(SshCredential::Password)
            .map_err(|_| {
                synthetic_ssh_error(
                    SshFailure::Authentication,
                    "SSH password credential is unavailable",
                )
            });
    }
    state
        .secrets
        .resolve_ssh_key(credential_ref)
        .await
        .map(SshCredential::PrivateKey)
        .map_err(|_| {
            synthetic_ssh_error(
                SshFailure::Authentication,
                "SSH key credential is unavailable",
            )
        })
}

/// Resolves the already verified fixed SSH target for typed read-only
/// monitoring. This is gated by Linux connection readiness only; Docker,
/// Compose and discovery-provider availability remain independent axes.
pub(crate) async fn resolve_monitoring_target(
    state: &AppState,
    host_id: &str,
) -> Result<SshTarget, M1Error> {
    let host = load_host(&state.pool, host_id).await?;
    if host.host_key_state != HostKeyState::Verified
        || host_connection_state(&host.status) != HostStatus::ConnectionReady
    {
        return Err(M1Error::conflict(
            "CONNECTION_NOT_READY",
            "必须先确认主机指纹并完成 SSH/Linux 连接检查",
            json!({"host_id": host_id, "status": enum_string(&host.status)}),
        ));
    }
    // Resolving a SecretRef is a local prerequisite. A missing or unreadable
    // reference means no SSH session was attempted, so it must not be reported
    // as remote authentication failure.
    let credential = resolve_ssh_credential(state, &host.credential_ref)
        .await
        .map_err(|_| M1Error::SecretRefUnavailable)?;
    Ok(SshTarget {
        host_id: host.host_id,
        address: host.address,
        port: host.port,
        user: host.ssh_user,
        credential,
    })
}

#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host_id}/connection-tests",
    tag = "m1",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    responses((status = 200, body = ConnectionTestResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn create_connection_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<Json<ConnectionTestResponse>, M1Error> {
    let request_id = request_id(&headers);
    let key = idempotency_key(&headers)?;
    if let Some(response) = replay_connection_test(&state.pool, &host_id, &key).await? {
        return Ok(Json(response));
    }
    let host = load_host(&state.pool, &host_id).await?;
    let started_at = now();
    sqlx::query("UPDATE hosts SET status = 'fingerprint_fetching' WHERE host_id = ?")
        .bind(&host_id)
        .execute(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
    let scanned = match state.ssh.scan_host_key(&host.address, host.port).await {
        Ok(scanned) => scanned,
        Err(error) => {
            let finished_at = now();
            let (code, _message, _status) = ssh_failure_code(error.failure);
            let response = connection_response(
                &request_id,
                &host,
                started_at,
                finished_at.clone(),
                ConnectionOutcome {
                    state: ConnectionTestState::Failed,
                    candidate_fingerprint: None,
                    capabilities: Vec::new(),
                    error_code: Some(code.to_owned()),
                    error_summary: Some(compact_summary(&error.summary)),
                    auth_transport: None,
                },
            );
            sqlx::query(
                "UPDATE hosts SET status = 'failed', last_checked_at = ? WHERE host_id = ?",
            )
            .bind(&finished_at)
            .bind(&host_id)
            .execute(&state.pool)
            .await
            .map_err(M1Error::Storage)?;
            persist_connection_test(
                &state.pool,
                &key,
                &response,
                &response.data.started_at,
                &finished_at,
            )
            .await?;
            return Ok(Json(response));
        }
    };

    let same_verified_key = host.host_key_state == HostKeyState::Verified
        && host.host_key_fingerprint.as_deref() == Some(scanned.fingerprint.as_str());
    if !same_verified_key {
        let changed = host.host_key_fingerprint.is_some()
            && host.host_key_fingerprint.as_deref() != Some(scanned.fingerprint.as_str());
        let state_value = if changed {
            HostKeyState::Changed
        } else {
            HostKeyState::Unverified
        };
        let host_status = if changed {
            HostStatus::HostKeyChanged
        } else {
            HostStatus::HostKeyUnverified
        };
        let finished_at = now();
        sqlx::query(
            "UPDATE hosts SET pending_host_key_fingerprint = ?, pending_host_key_line = ?, host_key_state = ?, status = ?, last_checked_at = ? WHERE host_id = ?",
        )
        .bind(&scanned.fingerprint)
        .bind(&scanned.known_hosts_line)
        .bind(enum_string(&state_value))
        .bind(enum_string(&host_status))
        .bind(&finished_at)
        .bind(&host_id)
        .execute(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
        let response = connection_response(
            &request_id,
            &host,
            started_at,
            finished_at.clone(),
            ConnectionOutcome {
                state: if changed {
                    ConnectionTestState::HostKeyChanged
                } else {
                    ConnectionTestState::HostKeyUnverified
                },
                candidate_fingerprint: Some(scanned.fingerprint),
                capabilities: vec!["host_key_candidate".to_owned()],
                error_code: if changed {
                    Some("HOST_KEY_CHANGED".to_owned())
                } else {
                    None
                },
                error_summary: if changed {
                    Some("检测到与已确认指纹不同的新主机密钥，需要重新确认".to_owned())
                } else {
                    Some("首次连接需要用户确认主机指纹".to_owned())
                },
                auth_transport: None,
            },
        );
        persist_connection_test(
            &state.pool,
            &key,
            &response,
            &response.data.started_at,
            &finished_at,
        )
        .await?;
        return Ok(Json(response));
    }

    sqlx::query("UPDATE hosts SET status = 'connection_checking' WHERE host_id = ?")
        .bind(&host_id)
        .execute(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
    let probe = connection_probe(&state, &host).await;
    let finished_at = now();
    let response = match probe {
        Ok(probe) => {
            sqlx::query(
                "UPDATE hosts SET status = 'connection_ready', last_checked_at = ? WHERE host_id = ?",
            )
            .bind(&finished_at)
            .bind(&host_id)
            .execute(&state.pool)
            .await
            .map_err(M1Error::Storage)?;
            connection_response(
                &request_id,
                &host,
                started_at,
                finished_at.clone(),
                ConnectionOutcome {
                    state: ConnectionTestState::ConnectionReady,
                    candidate_fingerprint: Some(scanned.fingerprint),
                    capabilities: probe.capabilities,
                    error_code: None,
                    error_summary: None,
                    auth_transport: Some(probe.auth_transport),
                },
            )
        }
        Err(error) => {
            let (code, _message, _status) = ssh_failure_code(error.failure);
            sqlx::query(
                "UPDATE hosts SET status = 'failed', last_checked_at = ? WHERE host_id = ?",
            )
            .bind(&finished_at)
            .bind(&host_id)
            .execute(&state.pool)
            .await
            .map_err(M1Error::Storage)?;
            connection_response(
                &request_id,
                &host,
                started_at,
                finished_at.clone(),
                ConnectionOutcome {
                    state: ConnectionTestState::Failed,
                    candidate_fingerprint: Some(scanned.fingerprint),
                    capabilities: Vec::new(),
                    error_code: Some(code.to_owned()),
                    error_summary: Some(compact_summary(&error.summary)),
                    auth_transport: error.auth_transport.map(contract_auth_transport),
                },
            )
        }
    };
    persist_connection_test(
        &state.pool,
        &key,
        &response,
        &response.data.started_at,
        &finished_at,
    )
    .await?;
    Ok(Json(response))
}

async fn replay_host_confirmation(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<HostKeyConfirmationResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT fingerprint, response_json FROM host_key_confirmations WHERE host_id = ? AND idempotency_key = ?",
    )
    .bind(host_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let recorded_fingerprint: String = row.try_get("fingerprint").map_err(M1Error::Storage)?;
    if recorded_fingerprint != fingerprint {
        return Err(M1Error::conflict(
            "IDEMPOTENCY_KEY_REUSED",
            "Idempotency-Key 已用于不同的主机指纹",
            json!({}),
        ));
    }
    Ok(Some({
        let payload: String = row.try_get("response_json").map_err(M1Error::Storage)?;
        serde_json::from_str(&payload).map_err(|_| M1Error::Internal)
    }?))
}

#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host_id}/host-key-confirmations",
    tag = "m1",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = HostKeyConfirmationRequest,
    responses((status = 200, body = HostKeyConfirmationResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn confirm_host_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    payload: Result<Json<HostKeyConfirmationRequest>, JsonRejection>,
) -> Result<Json<HostKeyConfirmationResponse>, M1Error> {
    let request_id = request_id(&headers);
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    if let Some(response) =
        replay_host_confirmation(&state.pool, &host_id, &key, &request.fingerprint).await?
    {
        return Ok(Json(response));
    }
    if !validate_fingerprint(&request.fingerprint) {
        return Err(M1Error::bad(
            "INVALID_HOST_FINGERPRINT",
            "主机指纹格式无效",
            json!({}),
        ));
    }
    let host = load_host(&state.pool, &host_id).await?;
    let pending_matches =
        host.pending_host_key_fingerprint.as_deref() == Some(request.fingerprint.as_str());
    let already_verified = host.host_key_state == HostKeyState::Verified
        && host.host_key_fingerprint.as_deref() == Some(request.fingerprint.as_str());
    if !pending_matches && !already_verified {
        return Err(M1Error::conflict(
            "HOST_KEY_CONFIRMATION_REQUIRED",
            "只能确认最近一次服务端取得的候选主机指纹",
            json!({"host_id": host_id, "candidate_fingerprint": host.pending_host_key_fingerprint}),
        ));
    }
    if pending_matches {
        let line = host.pending_host_key_line.as_deref().ok_or_else(|| {
            M1Error::conflict(
                "HOST_KEY_CANDIDATE_MISSING",
                "候选主机密钥材料已过期，请重新获取",
                json!({"host_id": host_id}),
            )
        })?;
        state
            .ssh
            .confirm_host_key(&host_id, line)
            .await
            .map_err(|error| external_from_ssh(error, "HOST_KEY_WRITE_FAILED"))?;
        sqlx::query(
            "UPDATE hosts SET host_key_fingerprint = ?, pending_host_key_fingerprint = NULL,
                pending_host_key_line = NULL, host_key_state = 'verified',
                status = 'host_key_verified', last_checked_at = ? WHERE host_id = ?",
        )
        .bind(&request.fingerprint)
        .bind(now())
        .bind(&host_id)
        .execute(&state.pool)
        .await
        .map_err(M1Error::Storage)?;
    }
    let updated = load_host(&state.pool, &host_id).await?;
    let response = HostKeyConfirmationResponse {
        data: host_record(&updated),
        meta: real_meta(request_id, Freshness::Fresh, 1),
    };
    sqlx::query(
        "INSERT INTO host_key_confirmations(
            confirmation_id, host_id, request_id, idempotency_key, fingerprint, response_json, confirmed_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&host_id)
    .bind(&response.meta.request_id)
    .bind(&key)
    .bind(&request.fingerprint)
    .bind(serde_json::to_string(&response).map_err(|_| M1Error::Internal)?)
    .bind(now())
    .execute(&state.pool)
    .await
    .map_err(M1Error::Storage)?;
    Ok(Json(response))
}

fn discovery_state_name(state: &DiscoveryRunState) -> String {
    enum_string(state)
}

fn discovery_freshness(state: &DiscoveryRunState, retention: &str) -> Freshness {
    if matches!(
        state,
        DiscoveryRunState::EvidenceReady
            | DiscoveryRunState::DiscoveryComplete
            | DiscoveryRunState::DiscoveryPartial
    ) {
        if retention == "summary" {
            Freshness::Stale
        } else {
            Freshness::Fresh
        }
    } else if state.is_terminal() {
        Freshness::Unavailable
    } else {
        Freshness::Stale
    }
}

async fn replay_discovery_request(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
    request_sha256: &str,
) -> Result<Option<DiscoveryRunAcceptedResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT request_sha256, response_json FROM discovery_runs
         WHERE host_id = ? AND idempotency_key = ?",
    )
    .bind(host_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    row.map(|row| {
        let recorded: Option<String> = row.try_get("request_sha256").map_err(M1Error::Storage)?;
        if recorded
            .as_deref()
            .is_some_and(|recorded| recorded != request_sha256)
        {
            return Err(M1Error::conflict(
                "IDEMPOTENCY_KEY_REUSED",
                "Idempotency-Key 已用于不同的发现请求",
                json!({"host_id": host_id}),
            ));
        }
        let payload: String = row.try_get("response_json").map_err(M1Error::Storage)?;
        serde_json::from_str(&payload).map_err(|_| M1Error::Internal)
    })
    .transpose()
}

fn normalized_discovery_request_sha256(
    request: &crate::contracts::DiscoveryRunCreateRequest,
) -> Result<String, M1Error> {
    fn normalized(values: &[String]) -> Vec<String> {
        let mut values = values.to_vec();
        values.sort();
        values.dedup();
        values
    }

    let payload = serde_json::to_vec(&json!({
        "provider_kinds": normalized(&request.provider_kinds),
        "root_refs": normalized(&request.root_refs),
        "requested_capabilities": normalized(&request.requested_capabilities),
    }))
    .map_err(|_| M1Error::Internal)?;
    Ok(hex_digest(&Sha256::digest(payload)))
}

fn validate_discovery_request(
    request: &crate::contracts::DiscoveryRunCreateRequest,
) -> Result<(), M1Error> {
    if request.provider_kinds.len() > 16
        || request.root_refs.len() > 8
        || request.requested_capabilities.len() > 16
    {
        return Err(M1Error::bad(
            "DISCOVERY_SCOPE_TOO_LARGE",
            "发现范围超过单次只读任务限制",
            json!({
                "provider_count": request.provider_kinds.len(),
                "root_count": request.root_refs.len(),
                "capability_count": request.requested_capabilities.len(),
            }),
        ));
    }
    let unsupported = request
        .provider_kinds
        .iter()
        .filter(|kind| !matches!(kind.as_str(), "docker" | "compose" | "systemd"))
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        return Err(M1Error::bad(
            "DISCOVERY_PROVIDER_UNSUPPORTED",
            "发现请求包含尚未支持的 Provider",
            json!({"provider_kinds": unsupported}),
        ));
    }
    let requests_docker = request.provider_kinds.iter().any(|kind| kind == "docker");
    let requests_compose = request.provider_kinds.iter().any(|kind| kind == "compose");
    if requests_docker != requests_compose {
        return Err(M1Error::bad(
            "DISCOVERY_PROVIDER_COMBINATION_UNSUPPORTED",
            "Docker 与 Compose 当前由同一发现切片采集，显式请求必须同时包含二者",
            json!({"required_provider_kinds": ["docker", "compose"]}),
        ));
    }
    let unsupported_capabilities = request
        .requested_capabilities
        .iter()
        .filter(|capability| capability.as_str() != "read_only")
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported_capabilities.is_empty() {
        return Err(M1Error::bad(
            "DISCOVERY_CAPABILITY_UNSUPPORTED",
            "发现请求包含尚未开放的能力",
            json!({"requested_capabilities": unsupported_capabilities}),
        ));
    }
    if let Some(root) = request.root_refs.iter().find(|root| {
        !root.starts_with('/')
            || root.as_str() == "/"
            || root.len() > 4096
            || root.contains(['\0', '\n', '\r'])
            || root.split('/').any(|segment| segment == "..")
    }) {
        return Err(M1Error::bad(
            "DISCOVERY_ROOT_INVALID",
            "用户确认根目录必须是受限的 Linux 绝对路径",
            json!({"root_ref": root}),
        ));
    }
    if !request.root_refs.is_empty() {
        return Err(M1Error::bad(
            "DISCOVERY_ROOT_PROVIDER_UNAVAILABLE",
            "用户确认根目录 Provider 尚未进入当前后端切片",
            json!({"root_count": request.root_refs.len()}),
        ));
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host_id}/discovery-runs",
    tag = "m1",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = crate::contracts::DiscoveryRunCreateRequest,
    responses(
        (status = 202, body = DiscoveryRunAcceptedResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 409, body = ApiErrorResponse),
        (status = 503, body = ApiErrorResponse)
    )
)]
pub async fn create_discovery_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    payload: Result<Json<crate::contracts::DiscoveryRunCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<DiscoveryRunAcceptedResponse>), M1Error> {
    let request_id = request_id(&headers);
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let request_sha256 = normalized_discovery_request_sha256(&request)?;
    if let Some(response) =
        replay_discovery_request(&state.pool, &host_id, &key, &request_sha256).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(response)));
    }
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Err(M1Error::ShuttingDown);
    }
    // Serialize only admission with backup/delete. The remote worker itself is
    // fenced by observation_permits, so this read guard can be released as soon
    // as the accepted run is durably present.
    let observation_admission = state.observation_admission.read().await;
    validate_discovery_request(&request)?;
    let host = load_host(&state.pool, &host_id).await?;
    if host.host_key_state != HostKeyState::Verified
        || !matches!(
            host.status,
            HostStatus::ConnectionReady
                | HostStatus::EvidenceReady
                | HostStatus::DiscoveryComplete
                | HostStatus::DiscoveryPartial
                | HostStatus::DiscoveryUnavailable
        )
    {
        return Err(M1Error::conflict(
            "CONNECTION_NOT_READY",
            "必须先确认主机指纹并完成 SSH/Linux/Docker 只读检查",
            json!({"host_id": host_id, "status": enum_string(&host.status)}),
        ));
    }
    let submitted_at = now();
    let run_id = Uuid::new_v4().to_string();
    let accepted = DiscoveryRunAcceptedResponse {
        data: DiscoveryRunAccepted {
            request_id: request_id.clone(),
            run_id: run_id.clone(),
            state: DiscoveryRunState::Accepted,
            submitted_at: submitted_at.clone(),
        },
        meta: real_meta(request_id, Freshness::Stale, 1),
    };
    let response_json = serde_json::to_string(&accepted).map_err(|_| M1Error::Internal)?;
    let mut tx = state.pool.begin().await.map_err(M1Error::Storage)?;
    let insert = sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256, protocol_version, state,
            submitted_at, response_json
         ) VALUES (?, ?, ?, ?, ?, ?, 'accepted', ?, ?)",
    )
    .bind(&run_id)
    .bind(&host_id)
    .bind(&accepted.data.request_id)
    .bind(&key)
    .bind(&request_sha256)
    .bind(PROTOCOL_VERSION)
    .bind(&submitted_at)
    .bind(response_json)
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert {
        let message = error.to_string().to_ascii_lowercase();
        if message.contains("one_active")
            || message.contains("unique")
            || message.contains("host observation already active")
        {
            tx.rollback().await.map_err(M1Error::Storage)?;
            if let Some(response) =
                replay_discovery_request(&state.pool, &host_id, &key, &request_sha256).await?
            {
                return Ok((StatusCode::ACCEPTED, Json(response)));
            }
            return Err(M1Error::conflict(
                "DISCOVERY_ALREADY_RUNNING",
                "该 HOST 已有一个扫描任务正在运行",
                json!({"host_id": host_id}),
            ));
        }
        return Err(M1Error::Storage(error));
    }
    sqlx::query("UPDATE hosts SET status = 'discovery_running' WHERE host_id = ?")
        .bind(&host_id)
        .execute(&mut *tx)
        .await
        .map_err(M1Error::Storage)?;
    tx.commit().await.map_err(M1Error::Storage)?;
    drop(observation_admission);
    let background_state = state.clone();
    let background_host = host_id.clone();
    tokio::spawn(async move {
        run_discovery_when_permitted(background_state, run_id, background_host, request).await;
    });
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn run_discovery_when_permitted(
    state: AppState,
    run_id: String,
    host_id: String,
    request: crate::contracts::DiscoveryRunCreateRequest,
) {
    match state.observation_permits.clone().acquire_owned().await {
        Ok(permit) => {
            if state.shutdown_requested.load(Ordering::Acquire) {
                drop(permit);
                match interrupt_accepted_discovery(&state.pool, &run_id, &host_id).await {
                    Ok(true) => {
                        if let Err(error) = events::publish(
                            &state.pool,
                            ChangeEventKind::DiscoveryRunChanged,
                            &format!("discovery-run:{run_id}"),
                            0,
                            json!({"state": "discovery_timeout", "reason": "application_shutdown"}),
                        )
                        .await
                        {
                            tracing::error!(%run_id, error = %error, "could not publish interrupted discovery event");
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::error!(%run_id, error = %error, "could not interrupt queued discovery during shutdown");
                    }
                }
                return;
            }
            run_discovery_background(state, run_id, host_id, request).await;
            drop(permit);
        }
        Err(error) => {
            tracing::error!(%run_id, error = %error, "observation concurrency gate closed");
        }
    }
}

async fn interrupt_accepted_discovery(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
) -> Result<bool, sqlx::Error> {
    let finished_at = now();
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "UPDATE discovery_runs
         SET state = 'discovery_timeout', failure_code = 'DISCOVERY_INTERRUPTED',
             failure_summary = 'Discovery was interrupted during application shutdown',
             finished_at = ?, evidence_retention = 'summary'
         WHERE run_id = ? AND state = 'accepted'",
    )
    .bind(&finished_at)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 1 {
        sqlx::query(
            "UPDATE hosts SET status = 'connection_ready', last_checked_at = ?
             WHERE host_id = ? AND status = 'discovery_running'",
        )
        .bind(&finished_at)
        .bind(host_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(result.rows_affected() == 1)
}

fn external_from_ssh(error: SshError, fallback_code: &'static str) -> M1Error {
    let (code, message, status) = ssh_failure_code(error.failure);
    M1Error::External {
        code: if code == "SSH_PROCESS_FAILED" {
            fallback_code
        } else {
            code
        },
        message,
        summary: error.summary,
        status,
    }
}

fn parse_discovery_state(value: &str) -> DiscoveryRunState {
    match value {
        "accepted" => DiscoveryRunState::Accepted,
        "running" => DiscoveryRunState::Running,
        "evidence_ready" => DiscoveryRunState::EvidenceReady,
        "discovery_complete" => DiscoveryRunState::DiscoveryComplete,
        "discovery_partial" => DiscoveryRunState::DiscoveryPartial,
        "discovery_unavailable" => DiscoveryRunState::DiscoveryUnavailable,
        "ssh_unreachable" => DiscoveryRunState::SshUnreachable,
        "ssh_auth_failed" => DiscoveryRunState::SshAuthFailed,
        "permission_denied" => DiscoveryRunState::PermissionDenied,
        "docker_permission_denied" => DiscoveryRunState::DockerPermissionDenied,
        "docker_unavailable" => DiscoveryRunState::DockerUnavailable,
        "compose_unavailable" => DiscoveryRunState::ComposeUnavailable,
        "discovery_timeout" => DiscoveryRunState::DiscoveryTimeout,
        "document_read_failed" => DiscoveryRunState::DocumentReadFailed,
        _ => DiscoveryRunState::EvidenceConflict,
    }
}

fn discovery_failure_for_secret() -> DiscoveryFailure {
    DiscoveryFailure {
        state: DiscoveryRunState::SshAuthFailed,
        code: "SSH_AUTH_FAILED",
        summary: "SSH credential is unavailable".to_owned(),
        audits: Vec::new(),
    }
}

async fn set_run_running(
    pool: &SqlitePool,
    run_id: &str,
    started_at: &str,
) -> Result<bool, M1Error> {
    sqlx::query(
        "UPDATE discovery_runs SET state = 'running', started_at = ? WHERE run_id = ? AND state = 'accepted'",
    )
    .bind(started_at)
    .bind(run_id)
    .execute(pool)
    .await
    .map(|result| result.rows_affected() == 1)
    .map_err(M1Error::Storage)
}

async fn persist_audits(
    executor: &mut sqlx::sqlite::SqliteConnection,
    run_id: &str,
    audits: &[CommandAudit],
) -> Result<(), M1Error> {
    for audit in audits {
        sqlx::query(
            "INSERT INTO discovery_command_audits(
                audit_id, run_id, action, exit_code, output_bytes, stderr_summary, started_at, finished_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(run_id)
        .bind(&audit.action)
        .bind(audit.exit_code)
        .bind(i64::try_from(audit.output_bytes).unwrap_or(i64::MAX))
        .bind(audit.stderr_summary.as_deref().map(compact_summary))
        .bind(&audit.started_at)
        .bind(&audit.finished_at)
        .execute(&mut *executor)
        .await
        .map_err(M1Error::Storage)?;
    }
    Ok(())
}

async fn persist_discovery_failure(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    failure: &DiscoveryFailure,
) -> Result<(), M1Error> {
    let finished_at = now();
    let mut tx = pool.begin().await.map_err(M1Error::Storage)?;
    sqlx::query(
        "UPDATE discovery_runs SET state = ?, failure_code = ?, failure_summary = ?, finished_at = ?, evidence_retention = 'summary' WHERE run_id = ?",
    )
    .bind(discovery_state_name(&failure.state))
    .bind(failure.code)
    .bind(compact_summary(&failure.summary))
    .bind(&finished_at)
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    persist_audits(&mut tx, run_id, &failure.audits).await?;
    let host_status = match failure.code {
        "SSH_UNREACHABLE" | "SSH_AUTH_FAILED" | "HOST_KEY_UNVERIFIED" | "PERMISSION_DENIED" => {
            "failed"
        }
        _ => "connection_ready",
    };
    sqlx::query("UPDATE hosts SET status = ?, last_checked_at = ? WHERE host_id = ?")
        .bind(host_status)
        .bind(&finished_at)
        .bind(host_id)
        .execute(&mut *tx)
        .await
        .map_err(M1Error::Storage)?;
    tx.commit().await.map_err(M1Error::Storage)
}

async fn prune_complete_runs(pool: &SqlitePool, host_id: &str) -> Result<(), M1Error> {
    let rows = sqlx::query(
        "SELECT run_id FROM discovery_runs
         WHERE host_id = ? AND state IN (
             'evidence_ready', 'discovery_complete', 'discovery_partial', 'discovery_unavailable'
         )
         ORDER BY submitted_at DESC, rowid DESC",
    )
    .bind(host_id)
    .fetch_all(pool)
    .await
    .map_err(M1Error::Storage)?;
    for row in rows.into_iter().skip(3) {
        let run_id: String = row.try_get("run_id").map_err(M1Error::Storage)?;
        sqlx::query(
            "UPDATE discovery_runs SET evidence_json = NULL, evidence_retention = 'summary' WHERE run_id = ?",
        )
        .bind(&run_id)
        .execute(pool)
        .await
        .map_err(M1Error::Storage)?;
        sqlx::query("DELETE FROM evidence_items WHERE run_id = ?")
            .bind(&run_id)
            .execute(pool)
            .await
            .map_err(M1Error::Storage)?;
    }
    Ok(())
}

async fn persist_discovery_success(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    success: &DiscoverySuccess,
) -> Result<(), M1Error> {
    let finished_at = success.evidence.finished_at.clone();
    let evidence_json = serde_json::to_string(&success.evidence).map_err(|_| M1Error::Internal)?;
    let evidence_sha256 = hex_digest(&Sha256::digest(evidence_json.as_bytes()));
    let item_count = success.evidence.items().count();
    let success_state = discovery_state_name(&success.state);
    let mut tx = pool.begin().await.map_err(M1Error::Storage)?;
    sqlx::query(
        "UPDATE discovery_runs SET state = ?, failure_code = NULL, failure_summary = NULL,
            finished_at = ?, evidence_json = ?, evidence_sha256 = ?, evidence_item_count = ?, evidence_retention = 'complete'
         WHERE run_id = ?",
    )
    .bind(&success_state)
    .bind(&finished_at)
    .bind(&evidence_json)
    .bind(&evidence_sha256)
    .bind(i64::try_from(item_count).unwrap_or(i64::MAX))
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    for item in success.evidence.items() {
        insert_evidence_item(&mut tx, run_id, item).await?;
    }
    persist_audits(&mut tx, run_id, &success.audits).await?;
    catalog::persist_discovery_success_in(&mut tx, run_id, host_id, &success.evidence)
        .await
        .map_err(|error| {
            tracing::error!(%run_id, %host_id, error = %error, "could not persist deployment catalog");
            M1Error::Internal
        })?;
    let host_status = match success.state {
        DiscoveryRunState::DiscoveryComplete => "discovery_complete",
        DiscoveryRunState::DiscoveryPartial => "discovery_partial",
        DiscoveryRunState::DiscoveryUnavailable => "discovery_unavailable",
        _ => "evidence_ready",
    };
    sqlx::query("UPDATE hosts SET status = ?, last_checked_at = ? WHERE host_id = ?")
        .bind(host_status)
        .bind(&finished_at)
        .bind(host_id)
        .execute(&mut *tx)
        .await
        .map_err(M1Error::Storage)?;
    if !matches!(success.state, DiscoveryRunState::DiscoveryUnavailable) {
        projection::create_draft_from_evidence_in(&mut tx, run_id, host_id, &success.evidence)
            .await
            .map_err(|error| {
                tracing::error!(%run_id, %host_id, error = %error, "could not create deterministic projection draft");
                M1Error::Internal
            })?;
    }
    tx.commit().await.map_err(M1Error::Storage)?;
    prune_complete_runs(pool, host_id).await
}

async fn insert_evidence_item(
    executor: &mut sqlx::sqlite::SqliteConnection,
    run_id: &str,
    item: &EvidenceItem,
) -> Result<(), M1Error> {
    sqlx::query(
        "INSERT INTO evidence_items(
            evidence_id, run_id, external_id, kind, source, observed_at, freshness, sha256,
            redaction_state, metadata_json
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(run_id)
    .bind(&item.external_id)
    .bind(enum_string(&item.kind))
    .bind(&item.source)
    .bind(&item.observed_at)
    .bind(enum_string(&item.freshness))
    .bind(&item.sha256)
    .bind(enum_string(&item.redaction_state))
    .bind(serde_json::to_string(&item.metadata).map_err(|_| M1Error::Internal)?)
    .execute(&mut *executor)
    .await
    .map(|_| ())
    .map_err(M1Error::Storage)
}

fn discovery_baseline_failed(failure: &DiscoveryFailure) -> bool {
    failure
        .audits
        .last()
        .is_none_or(|audit| audit.action == "linux_identity")
}

fn failure_provider_kind(failure: &DiscoveryFailure) -> &'static str {
    let action = failure
        .audits
        .last()
        .map(|audit| audit.action.as_str())
        .unwrap_or_default();
    if action == "compose_ls" || failure.code.starts_with("COMPOSE_") {
        "compose"
    } else if action.starts_with("document_") || failure.code.starts_with("DOCUMENT_") {
        "documents"
    } else if action == "systemd_units" || failure.code.starts_with("SYSTEMD_") {
        "systemd"
    } else {
        "docker"
    }
}

fn failure_provider_status(failure: &DiscoveryFailure) -> DiscoveryProviderStatus {
    if failure.code.contains("PERMISSION_DENIED") {
        DiscoveryProviderStatus::PermissionDenied
    } else if matches!(failure.state, DiscoveryRunState::DiscoveryTimeout) {
        DiscoveryProviderStatus::TimedOut
    } else if failure.code.ends_with("_UNAVAILABLE") {
        DiscoveryProviderStatus::Unavailable
    } else {
        DiscoveryProviderStatus::Failed
    }
}

fn coverage_for_failure(
    failure: &DiscoveryFailure,
    forced_provider_kind: Option<&'static str>,
) -> Vec<DiscoveryProviderCoverage> {
    let failed_kind = forced_provider_kind.unwrap_or_else(|| failure_provider_kind(failure));
    let observed_at = failure.audits.last().map(|audit| audit.finished_at.clone());
    let mut coverage = Vec::new();

    if forced_provider_kind.is_none() {
        for (action, provider_kind) in [("docker_version", "docker"), ("compose_ls", "compose")] {
            if provider_kind != failed_kind
                && failure
                    .audits
                    .iter()
                    .any(|audit| audit.action == action && audit.exit_code == Some(0))
            {
                coverage.push(DiscoveryProviderCoverage {
                    provider_kind: provider_kind.to_owned(),
                    status: DiscoveryProviderStatus::Ready,
                    observed_count: 0,
                    evidence_refs: Vec::new(),
                    warnings: Vec::new(),
                    observed_at: observed_at.clone(),
                });
            }
        }
    }

    coverage.push(DiscoveryProviderCoverage {
        provider_kind: failed_kind.to_owned(),
        status: failure_provider_status(failure),
        observed_count: 0,
        evidence_refs: Vec::new(),
        warnings: vec![EvidenceWarning {
            code: failure.code.to_owned(),
            summary: compact_summary(&failure.summary),
        }],
        observed_at,
    });
    coverage
}

fn aggregate_provider_state(provider_results: &[DiscoveryProviderCoverage]) -> DiscoveryRunState {
    let ready = provider_results
        .iter()
        .filter(|provider| provider.status == DiscoveryProviderStatus::Ready)
        .count();
    if !provider_results.is_empty() && ready == provider_results.len() {
        DiscoveryRunState::DiscoveryComplete
    } else if ready > 0 {
        DiscoveryRunState::DiscoveryPartial
    } else {
        DiscoveryRunState::DiscoveryUnavailable
    }
}

fn ensure_provider_results(success: &mut DiscoverySuccess) {
    if success.evidence.provider_results.is_empty() {
        success.evidence.provider_results = provider_coverage_from_evidence(&success.evidence);
    }
}

fn provider_failure_as_success(
    run_id: &str,
    target: &SshTarget,
    failure: DiscoveryFailure,
) -> DiscoverySuccess {
    let provider_results = coverage_for_failure(&failure, None);
    let warnings = provider_results
        .iter()
        .flat_map(|provider| provider.warnings.clone())
        .collect::<Vec<_>>();
    let started_at = failure
        .audits
        .first()
        .map(|audit| audit.started_at.clone())
        .unwrap_or_else(now);
    let finished_at = failure
        .audits
        .last()
        .map(|audit| audit.finished_at.clone())
        .unwrap_or_else(now);
    let state = aggregate_provider_state(&provider_results);
    DiscoverySuccess {
        evidence: DiscoveryEvidence {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            discovery_id: run_id.to_owned(),
            host: EvidenceHostIdentity {
                host_id: target.host_id.clone(),
                address: target.address.clone(),
                os: "linux".to_owned(),
            },
            host_facts: Vec::new(),
            docker_engines: Vec::new(),
            compose_projects: Vec::new(),
            systemd_units: Vec::new(),
            containers: Vec::new(),
            images: Vec::new(),
            networks: Vec::new(),
            volumes: Vec::new(),
            document_candidates: Vec::new(),
            health_checks: Vec::new(),
            warnings,
            provider_results,
            started_at,
            finished_at,
        },
        audits: failure.audits,
        state,
    }
}

fn finalize_requested_success(
    mut success: DiscoverySuccess,
    requested_provider_kinds: &[String],
) -> DiscoverySuccess {
    ensure_provider_results(&mut success);
    success.evidence.provider_results.retain(|provider| {
        requested_provider_kinds
            .iter()
            .any(|requested| requested == &provider.provider_kind)
    });

    let mut requested = requested_provider_kinds.to_vec();
    requested.sort();
    requested.dedup();
    let observed_at = Some(success.evidence.finished_at.clone());
    for provider_kind in requested {
        if success
            .evidence
            .provider_results
            .iter()
            .any(|provider| provider.provider_kind == provider_kind)
        {
            continue;
        }
        let warning = EvidenceWarning {
            code: "PROVIDER_NOT_EXECUTED".to_owned(),
            summary: format!(
                "{provider_kind} was not executed because an earlier provider step failed"
            ),
        };
        success.evidence.warnings.push(warning.clone());
        success
            .evidence
            .provider_results
            .push(DiscoveryProviderCoverage {
                provider_kind,
                status: DiscoveryProviderStatus::Unavailable,
                observed_count: 0,
                evidence_refs: Vec::new(),
                warnings: vec![warning],
                observed_at: observed_at.clone(),
            });
    }
    success.state = aggregate_provider_state(&success.evidence.provider_results);
    success
}

fn merge_discovery_successes(
    mut primary: DiscoverySuccess,
    mut additional: DiscoverySuccess,
) -> DiscoverySuccess {
    ensure_provider_results(&mut primary);
    ensure_provider_results(&mut additional);
    primary
        .evidence
        .systemd_units
        .append(&mut additional.evidence.systemd_units);
    primary
        .evidence
        .provider_results
        .append(&mut additional.evidence.provider_results);
    primary
        .evidence
        .warnings
        .append(&mut additional.evidence.warnings);
    if additional.evidence.started_at < primary.evidence.started_at {
        primary.evidence.started_at = additional.evidence.started_at;
    }
    if additional.evidence.finished_at > primary.evidence.finished_at {
        primary.evidence.finished_at = additional.evidence.finished_at;
    }
    primary.audits.append(&mut additional.audits);
    primary.state = aggregate_provider_state(&primary.evidence.provider_results);
    primary
}

fn merge_failure_into_success(
    mut success: DiscoverySuccess,
    failure: DiscoveryFailure,
    forced_provider_kind: Option<&'static str>,
    failure_happened_first: bool,
) -> DiscoverySuccess {
    ensure_provider_results(&mut success);
    let mut failure_coverage = coverage_for_failure(&failure, forced_provider_kind);
    for provider in &failure_coverage {
        success.evidence.warnings.extend(provider.warnings.clone());
    }
    success
        .evidence
        .provider_results
        .append(&mut failure_coverage);
    if failure_happened_first {
        let mut audits = failure.audits;
        audits.append(&mut success.audits);
        success.audits = audits;
    } else {
        success.audits.extend(failure.audits);
    }
    success.state = aggregate_provider_state(&success.evidence.provider_results);
    success
}

async fn run_requested_discovery(
    runner: &DiscoveryRunner,
    run_id: &str,
    target: &SshTarget,
    request: &crate::contracts::DiscoveryRunCreateRequest,
) -> Result<DiscoverySuccess, DiscoveryFailure> {
    if request.provider_kinds.is_empty() {
        return runner.run(run_id, target).await;
    }
    let requests_systemd = request.provider_kinds.iter().any(|kind| kind == "systemd");
    let requests_docker = request
        .provider_kinds
        .iter()
        .any(|kind| matches!(kind.as_str(), "docker" | "compose"));

    if requests_systemd && requests_docker {
        let docker = runner.run(run_id, target).await;
        if docker.as_ref().is_err_and(discovery_baseline_failed) {
            return docker;
        }
        let systemd = runner.run_systemd(run_id, target).await;
        let combined = match (docker, systemd) {
            (Ok(docker), Ok(systemd)) => Ok(merge_discovery_successes(docker, systemd)),
            (Err(docker), Ok(systemd)) => {
                Ok(merge_failure_into_success(systemd, docker, None, true))
            }
            (Ok(docker), Err(systemd)) => Ok(merge_failure_into_success(
                docker,
                systemd,
                Some("systemd"),
                false,
            )),
            (Err(_docker), Err(systemd)) => Err(systemd),
        };
        return combined
            .map(|success| finalize_requested_success(success, request.provider_kinds.as_slice()));
    }
    if requests_systemd {
        runner
            .run_systemd(run_id, target)
            .await
            .map(|success| finalize_requested_success(success, request.provider_kinds.as_slice()))
    } else {
        match runner.run(run_id, target).await {
            Ok(success) => Ok(finalize_requested_success(
                success,
                request.provider_kinds.as_slice(),
            )),
            Err(failure) if discovery_baseline_failed(&failure) => Err(failure),
            Err(failure) => Ok(finalize_requested_success(
                provider_failure_as_success(run_id, target, failure),
                request.provider_kinds.as_slice(),
            )),
        }
    }
}

async fn run_discovery_background(
    state: AppState,
    run_id: String,
    host_id: String,
    request: crate::contracts::DiscoveryRunCreateRequest,
) {
    let started_at = now();
    match set_run_running(&state.pool, &run_id, &started_at).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::error!(run_id = %run_id, error = %error, "could not mark discovery running");
            return;
        }
    }
    if let Err(error) = events::publish(
        &state.pool,
        ChangeEventKind::DiscoveryRunChanged,
        &format!("discovery-run:{run_id}"),
        0,
        json!({"state": "running"}),
    )
    .await
    {
        tracing::error!(run_id = %run_id, error = %error, "could not publish running discovery event");
    }
    let host = match load_host(&state.pool, &host_id).await {
        Ok(host) => host,
        Err(error) => {
            tracing::error!(run_id = %run_id, error = %error, "could not load discovery host");
            return;
        }
    };
    let credential = match resolve_ssh_credential(&state, &host.credential_ref).await {
        Ok(credential) => credential,
        Err(_) => {
            let persisted = persist_discovery_failure(
                &state.pool,
                &run_id,
                &host_id,
                &discovery_failure_for_secret(),
            )
            .await;
            if persisted.is_ok() {
                let _ = events::publish(
                    &state.pool,
                    ChangeEventKind::DiscoveryRunChanged,
                    &format!("discovery-run:{run_id}"),
                    0,
                    json!({"state": "ssh_auth_failed"}),
                )
                .await;
            }
            return;
        }
    };
    let target = SshTarget {
        host_id: host.host_id.clone(),
        address: host.address.clone(),
        port: host.port,
        user: host.ssh_user.clone(),
        credential,
    };
    let result = run_requested_discovery(&state.discovery, &run_id, &target, &request).await;
    match result {
        Ok(success) => {
            match persist_discovery_success(&state.pool, &run_id, &host_id, &success).await {
                Ok(()) => {
                    if let Err(error) = events::publish(
                        &state.pool,
                        ChangeEventKind::DiscoveryRunChanged,
                        &format!("discovery-run:{run_id}"),
                        0,
                        json!({"state": discovery_state_name(&success.state)}),
                    )
                    .await
                    {
                        tracing::error!(run_id = %run_id, error = %error, "could not publish completed discovery event");
                    }
                }
                Err(error) => {
                    tracing::error!(run_id = %run_id, error = %error, "could not persist discovery evidence");
                }
            }
        }
        Err(failure) => {
            match persist_discovery_failure(&state.pool, &run_id, &host_id, &failure).await {
                Ok(()) => {
                    if let Err(error) = events::publish(
                        &state.pool,
                        ChangeEventKind::DiscoveryRunChanged,
                        &format!("discovery-run:{run_id}"),
                        0,
                        json!({"state": discovery_state_name(&failure.state)}),
                    )
                    .await
                    {
                        tracing::error!(run_id = %run_id, error = %error, "could not publish failed discovery event");
                    }
                }
                Err(error) => {
                    tracing::error!(run_id = %run_id, error = %error, "could not persist discovery failure");
                }
            }
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/discovery-runs/{run_id}",
    tag = "m1",
    params(("run_id" = String, Path, description = "Discovery run identifier")),
    responses((status = 200, body = DiscoveryRunResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_discovery_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<DiscoveryRunResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT request_id, run_id, host_id, protocol_version, state, failure_code, failure_summary,
                submitted_at, started_at, finished_at, draft_id, diff_id, evidence_item_count, evidence_sha256, evidence_retention
         FROM discovery_runs WHERE run_id = ?",
    )
    .bind(&run_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(M1Error::Storage)?
    .ok_or_else(|| M1Error::not_found("discovery_run", &run_id))?;
    let state_value = parse_discovery_state(
        row.try_get::<String, _>("state")
            .map_err(M1Error::Storage)?
            .as_str(),
    );
    let retention: String = row
        .try_get("evidence_retention")
        .map_err(M1Error::Storage)?;
    let count: i64 = row
        .try_get("evidence_item_count")
        .map_err(M1Error::Storage)?;
    let request_id_value: String = row.try_get("request_id").map_err(M1Error::Storage)?;
    let record = DiscoveryRunRecord {
        request_id: request_id_value.clone(),
        run_id: row.try_get("run_id").map_err(M1Error::Storage)?,
        host_id: row.try_get("host_id").map_err(M1Error::Storage)?,
        protocol_version: row.try_get("protocol_version").map_err(M1Error::Storage)?,
        state: state_value.clone(),
        failure_code: row.try_get("failure_code").map_err(M1Error::Storage)?,
        failure_summary: row.try_get("failure_summary").map_err(M1Error::Storage)?,
        submitted_at: row.try_get("submitted_at").map_err(M1Error::Storage)?,
        started_at: row.try_get("started_at").map_err(M1Error::Storage)?,
        finished_at: row.try_get("finished_at").map_err(M1Error::Storage)?,
        draft_id: row.try_get("draft_id").map_err(M1Error::Storage)?,
        diff_id: row.try_get("diff_id").map_err(M1Error::Storage)?,
        evidence_item_count: u32::try_from(count.max(0)).unwrap_or(u32::MAX),
        evidence_sha256: row.try_get("evidence_sha256").map_err(M1Error::Storage)?,
        evidence_retention: retention.clone(),
    };
    let meta_request_id = request_id(&headers);
    Ok(Json(DiscoveryRunResponse {
        data: record,
        meta: real_meta(
            meta_request_id,
            discovery_freshness(&state_value, &retention),
            1,
        ),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/discovery-runs/{run_id}/evidence",
    tag = "m1",
    params(("run_id" = String, Path, description = "Discovery run identifier")),
    responses((status = 200, body = DiscoveryEvidenceResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn get_discovery_evidence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<DiscoveryEvidenceResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT state, evidence_retention, evidence_json FROM discovery_runs WHERE run_id = ?",
    )
    .bind(&run_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(M1Error::Storage)?
    .ok_or_else(|| M1Error::not_found("discovery_run", &run_id))?;
    let state_value: String = row.try_get("state").map_err(M1Error::Storage)?;
    let retention: String = row
        .try_get("evidence_retention")
        .map_err(M1Error::Storage)?;
    let payload: Option<String> = row.try_get("evidence_json").map_err(M1Error::Storage)?;
    let payload = payload.ok_or_else(|| {
        if retention == "summary"
            && matches!(
                state_value.as_str(),
                "evidence_ready"
                    | "discovery_complete"
                    | "discovery_partial"
                    | "discovery_unavailable"
            )
        {
            M1Error::conflict(
                "EVIDENCE_SUMMARY_ONLY",
                "该扫描已按保留策略仅保存摘要与哈希",
                json!({"run_id": &run_id, "state": &state_value, "retention": &retention}),
            )
        } else {
            M1Error::conflict(
                "EVIDENCE_NOT_READY",
                "该扫描尚未产生可读取的完整证据",
                json!({"run_id": &run_id, "state": &state_value}),
            )
        }
    })?;
    let evidence: DiscoveryEvidence =
        serde_json::from_str(&payload).map_err(|_| M1Error::Internal)?;
    Ok(Json(DiscoveryEvidenceResponse {
        data: evidence,
        meta: real_meta(
            request_id(&headers),
            discovery_freshness(&parse_discovery_state(&state_value), &retention),
            1,
        ),
    }))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::DiscoveryRunCreateRequest;
    use crate::storage;

    #[test]
    fn host_connection_state_does_not_repeat_discovery_status() {
        for status in [
            HostStatus::DockerUnavailable,
            HostStatus::DockerPermissionDenied,
            HostStatus::DiscoveryRunning,
            HostStatus::EvidenceReady,
            HostStatus::DiscoveryComplete,
            HostStatus::DiscoveryPartial,
            HostStatus::DiscoveryUnavailable,
        ] {
            assert_eq!(host_connection_state(&status), HostStatus::ConnectionReady);
        }
        assert_eq!(
            host_connection_state(&HostStatus::HostKeyChanged),
            HostStatus::HostKeyChanged
        );
        assert_eq!(
            host_connection_state(&HostStatus::Failed),
            HostStatus::Failed
        );
    }

    #[test]
    fn discovery_request_accepts_explicit_mixed_read_only_providers_but_not_ignored_inputs() {
        validate_discovery_request(&DiscoveryRunCreateRequest {
            provider_kinds: vec![
                "docker".to_owned(),
                "compose".to_owned(),
                "systemd".to_owned(),
            ],
            root_refs: Vec::new(),
            requested_capabilities: vec!["read_only".to_owned()],
        })
        .expect("implemented provider combination");

        for lone_provider in ["docker", "compose"] {
            let unsupported_combination = validate_discovery_request(&DiscoveryRunCreateRequest {
                provider_kinds: vec![lone_provider.to_owned()],
                root_refs: Vec::new(),
                requested_capabilities: vec!["read_only".to_owned()],
            })
            .expect_err("legacy Docker and Compose discovery must be requested together");
            assert!(matches!(
                unsupported_combination,
                M1Error::BadRequest {
                    code: "DISCOVERY_PROVIDER_COMBINATION_UNSUPPORTED",
                    ..
                }
            ));
        }

        let unsupported_root = validate_discovery_request(&DiscoveryRunCreateRequest {
            provider_kinds: Vec::new(),
            root_refs: vec!["/srv/app".to_owned()],
            requested_capabilities: Vec::new(),
        })
        .expect_err("root provider is not implemented in this slice");
        assert!(matches!(
            unsupported_root,
            M1Error::BadRequest {
                code: "DISCOVERY_ROOT_PROVIDER_UNAVAILABLE",
                ..
            }
        ));

        let unsupported_capability = validate_discovery_request(&DiscoveryRunCreateRequest {
            provider_kinds: vec!["systemd".to_owned()],
            root_refs: Vec::new(),
            requested_capabilities: vec!["restart".to_owned()],
        })
        .expect_err("write capability is not opened");
        assert!(matches!(
            unsupported_capability,
            M1Error::BadRequest {
                code: "DISCOVERY_CAPABILITY_UNSUPPORTED",
                ..
            }
        ));
    }

    #[test]
    fn discovery_request_hash_normalizes_collection_order_and_duplicates() {
        let first = DiscoveryRunCreateRequest {
            provider_kinds: vec![
                "systemd".to_owned(),
                "compose".to_owned(),
                "docker".to_owned(),
                "systemd".to_owned(),
            ],
            root_refs: vec!["/srv/b".to_owned(), "/srv/a".to_owned()],
            requested_capabilities: vec!["read_only".to_owned(), "read_only".to_owned()],
        };
        let reordered = DiscoveryRunCreateRequest {
            provider_kinds: vec![
                "docker".to_owned(),
                "compose".to_owned(),
                "systemd".to_owned(),
            ],
            root_refs: vec!["/srv/a".to_owned(), "/srv/b".to_owned()],
            requested_capabilities: vec!["read_only".to_owned()],
        };
        assert_eq!(
            normalized_discovery_request_sha256(&first).unwrap(),
            normalized_discovery_request_sha256(&reordered).unwrap()
        );

        let changed = DiscoveryRunCreateRequest {
            provider_kinds: vec!["systemd".to_owned()],
            root_refs: Vec::new(),
            requested_capabilities: vec!["read_only".to_owned()],
        };
        assert_ne!(
            normalized_discovery_request_sha256(&first).unwrap(),
            normalized_discovery_request_sha256(&changed).unwrap()
        );
    }

    #[test]
    fn provider_aggregation_distinguishes_complete_partial_and_unavailable() {
        let provider = |status| DiscoveryProviderCoverage {
            provider_kind: "fixture".to_owned(),
            status,
            observed_count: 0,
            evidence_refs: Vec::new(),
            warnings: Vec::new(),
            observed_at: None,
        };
        assert_eq!(
            aggregate_provider_state(&[provider(DiscoveryProviderStatus::Ready)]),
            DiscoveryRunState::DiscoveryComplete
        );
        assert_eq!(
            aggregate_provider_state(&[
                provider(DiscoveryProviderStatus::Ready),
                provider(DiscoveryProviderStatus::Unavailable),
            ]),
            DiscoveryRunState::DiscoveryPartial
        );
        assert_eq!(
            aggregate_provider_state(&[
                provider(DiscoveryProviderStatus::Unavailable),
                provider(DiscoveryProviderStatus::Failed),
            ]),
            DiscoveryRunState::DiscoveryUnavailable
        );
    }

    #[tokio::test]
    async fn retention_keeps_only_three_evidence_runs_per_host() {
        let pool = storage::connect("sqlite::memory:")
            .await
            .expect("test database");
        ensure_workspace(&pool).await.expect("default workspace");
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
                host_key_state, transport, os, status, created_at
             ) VALUES ('host-retention', ?, 'Retention', '127.0.0.1', 22, 'fixture',
                'secret://ssh/00000000-0000-0000-0000-000000000000', 'verified', 'ssh',
                'linux', 'evidence_ready', '2026-08-11T00:00:00Z')",
        )
        .bind(DEFAULT_WORKSPACE_ID)
        .execute(&pool)
        .await
        .expect("host row");

        for index in 1..=4 {
            let run_id = format!("run-{index}");
            let submitted_at = format!("2026-08-11T00:00:0{index}Z");
            let state = if index == 1 {
                "discovery_unavailable"
            } else {
                "evidence_ready"
            };
            sqlx::query(
                "INSERT INTO discovery_runs(
                    run_id, host_id, request_id, idempotency_key, protocol_version, state,
                    response_json, submitted_at, finished_at, evidence_json, evidence_sha256,
                    evidence_item_count, evidence_retention
                 ) VALUES (?, 'host-retention', ?, ?, '1', ?, '{}', ?, ?, '{}',
                    'digest', 1, 'complete')",
            )
            .bind(&run_id)
            .bind(format!("request-{index}"))
            .bind(format!("key-{index}"))
            .bind(state)
            .bind(&submitted_at)
            .bind(&submitted_at)
            .execute(&pool)
            .await
            .expect("discovery row");
            sqlx::query(
                "INSERT INTO evidence_items(
                    evidence_id, run_id, external_id, kind, source, observed_at, freshness,
                    redaction_state, metadata_json
                 ) VALUES (?, ?, ?, 'container', 'fixture', ?, 'fresh', 'not_required', '{}')",
            )
            .bind(format!("evidence-{index}"))
            .bind(&run_id)
            .bind(format!("external-{index}"))
            .bind(&submitted_at)
            .execute(&pool)
            .await
            .expect("evidence row");
        }

        prune_complete_runs(&pool, "host-retention")
            .await
            .expect("retention pruning");
        let rows = sqlx::query(
            "SELECT run_id, evidence_retention, evidence_json FROM discovery_runs
             WHERE host_id = 'host-retention' ORDER BY submitted_at DESC",
        )
        .fetch_all(&pool)
        .await
        .expect("retained rows");
        assert_eq!(rows.len(), 4);
        for row in rows.iter().take(3) {
            assert_eq!(row.get::<String, _>("evidence_retention"), "complete");
            assert!(row.get::<Option<String>, _>("evidence_json").is_some());
        }
        assert_eq!(rows[3].get::<String, _>("evidence_retention"), "summary");
        assert!(rows[3].get::<Option<String>, _>("evidence_json").is_none());
        let old_items: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM evidence_items WHERE run_id = 'run-1'")
                .fetch_one(&pool)
                .await
                .expect("old evidence count");
        assert_eq!(old_items, 0);
    }

    #[tokio::test]
    async fn startup_recovery_closes_interrupted_runs() {
        let pool = storage::connect("sqlite::memory:")
            .await
            .expect("test database");
        ensure_workspace(&pool).await.expect("default workspace");
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
                host_key_state, transport, os, status, created_at
             ) VALUES ('host-recovery', ?, 'Recovery', '127.0.0.1', 22, 'fixture',
                'secret://ssh/00000000-0000-0000-0000-000000000000', 'verified', 'ssh',
                'linux', 'discovery_running', '2026-08-11T00:00:00Z')",
        )
        .bind(DEFAULT_WORKSPACE_ID)
        .execute(&pool)
        .await
        .expect("host row");
        sqlx::query(
            "INSERT INTO discovery_runs(
                run_id, host_id, request_id, idempotency_key, protocol_version, state,
                response_json, submitted_at, evidence_retention
             ) VALUES ('run-recovery', 'host-recovery', 'request-recovery', 'key-recovery',
                '1', 'running', '{}', '2026-08-11T00:00:00Z', 'complete')",
        )
        .execute(&pool)
        .await
        .expect("running discovery");

        assert_eq!(recover_interrupted_discoveries(&pool).await.unwrap(), 1);
        let state: String =
            sqlx::query_scalar("SELECT state FROM discovery_runs WHERE run_id = 'run-recovery'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let host_status: String =
            sqlx::query_scalar("SELECT status FROM hosts WHERE host_id = 'host-recovery'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, "discovery_timeout");
        assert_eq!(host_status, "connection_ready");
    }

    #[tokio::test]
    async fn queued_discovery_does_not_start_ssh_after_shutdown() {
        let pool = storage::connect("sqlite::memory:")
            .await
            .expect("test database");
        ensure_workspace(&pool).await.expect("default workspace");
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
                host_key_state, transport, os, status, created_at
             ) VALUES ('host-queued-shutdown', ?, 'Shutdown', '127.0.0.2', 22, 'fixture',
                'secret://ssh/00000000-0000-0000-0000-000000000000', 'verified', 'ssh',
                'linux', 'discovery_running', '2026-08-15T00:00:00Z')",
        )
        .bind(DEFAULT_WORKSPACE_ID)
        .execute(&pool)
        .await
        .expect("host row");
        sqlx::query(
            "INSERT INTO discovery_runs(
                run_id, host_id, request_id, idempotency_key, protocol_version, state,
                response_json, submitted_at, evidence_retention
             ) VALUES ('run-queued-shutdown', 'host-queued-shutdown', 'request-shutdown',
                'key-shutdown', '1', 'accepted', '{}', '2026-08-15T00:00:00Z', 'complete')",
        )
        .execute(&pool)
        .await
        .expect("accepted discovery");
        let state = AppState::new(pool.clone());
        let maximum = state.monitoring_scheduler.settings().max_concurrency;
        let held = state
            .observation_permits
            .clone()
            .acquire_many_owned(maximum)
            .await
            .unwrap();
        let worker = tokio::spawn(run_discovery_when_permitted(
            state.clone(),
            "run-queued-shutdown".to_owned(),
            "host-queued-shutdown".to_owned(),
            DiscoveryRunCreateRequest::default(),
        ));
        state.begin_shutdown();
        drop(held);
        worker.await.unwrap();

        let run = sqlx::query(
            "SELECT state, failure_code FROM discovery_runs
             WHERE run_id = 'run-queued-shutdown'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let host_status: String =
            sqlx::query_scalar("SELECT status FROM hosts WHERE host_id = 'host-queued-shutdown'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(run.get::<String, _>("state"), "discovery_timeout");
        assert_eq!(
            run.get::<Option<String>, _>("failure_code").as_deref(),
            Some("DISCOVERY_INTERRUPTED")
        );
        assert_eq!(host_status, "connection_ready");
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM change_events
             WHERE kind = 'discovery.run.changed'
               AND subject_ref = 'discovery-run:run-queued-shutdown'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
    }

    #[tokio::test]
    async fn deleted_queued_discovery_stops_before_running_event_or_ssh() {
        let pool = storage::connect("sqlite::memory:")
            .await
            .expect("test database");
        ensure_workspace(&pool).await.expect("default workspace");
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
                host_key_state, transport, os, status, created_at
             ) VALUES ('host-queued-delete', ?, 'Delete', '127.0.0.2', 22, 'fixture',
                'secret://ssh/00000000-0000-0000-0000-000000000000', 'verified', 'ssh',
                'linux', 'discovery_running', '2026-08-15T00:00:00Z')",
        )
        .bind(DEFAULT_WORKSPACE_ID)
        .execute(&pool)
        .await
        .expect("host row");
        sqlx::query(
            "INSERT INTO discovery_runs(
                run_id, host_id, request_id, idempotency_key, protocol_version, state,
                response_json, submitted_at, evidence_retention
             ) VALUES ('run-queued-delete', 'host-queued-delete', 'request-delete',
                'key-delete', '1', 'accepted', '{}', '2026-08-15T00:00:00Z', 'complete')",
        )
        .execute(&pool)
        .await
        .expect("accepted discovery");
        let state = AppState::new(pool.clone());
        let maximum = state.monitoring_scheduler.settings().max_concurrency;
        let held = state
            .observation_permits
            .clone()
            .acquire_many_owned(maximum)
            .await
            .unwrap();
        let worker = tokio::spawn(run_discovery_when_permitted(
            state,
            "run-queued-delete".to_owned(),
            "host-queued-delete".to_owned(),
            DiscoveryRunCreateRequest::default(),
        ));
        sqlx::query("DELETE FROM hosts WHERE host_id = 'host-queued-delete'")
            .execute(&pool)
            .await
            .expect("HOST cascade after backup fence");
        drop(held);
        worker.await.unwrap();

        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM change_events
             WHERE kind = 'discovery.run.changed'
               AND subject_ref = 'discovery-run:run-queued-delete'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, 0, "a deleted queued run never reached running");
        let runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM discovery_runs WHERE run_id = 'run-queued-delete'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(runs, 0);
    }
}
