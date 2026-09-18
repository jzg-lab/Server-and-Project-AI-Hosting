use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use reqwest::Url;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, DataSourceDescriptor, DataSourceKind,
        DataSourceStatus, Freshness, ModelProviderData, ModelProviderPutRequest,
        ModelProviderResponse, ModelProviderTestData, ModelProviderTestResponse,
        ModelProviderTestState,
    },
    projection::ProjectionError,
    secrets::SecretStoreError,
};

const DEFAULT_WORKSPACE_ID: &str = "workspace-default";
const MAX_IDEMPOTENCY_KEY: usize = 160;
const MAX_MODEL_CHARS: usize = 160;
const MAX_MODEL_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ModelProviderConfig {
    pub base_url: String,
    pub model: String,
    pub credential_ref: String,
    pub revision: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ModelClient {
    client: reqwest::Client,
}

impl ModelClient {
    pub fn new(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .timeout(timeout)
            .build()
            .expect("model HTTP client configuration is valid");
        Self { client }
    }

    pub async fn chat(
        &self,
        config: &ModelProviderConfig,
        api_key: &str,
        messages: Vec<Value>,
    ) -> Result<String, ModelCallError> {
        let url =
            chat_completions_url(&config.base_url).map_err(|_| ModelCallError::InvalidConfig)?;
        let response = self
            .client
            .post(url)
            .bearer_auth(api_key)
            .json(&json!({
                "model": config.model,
                "messages": messages,
                "stream": false
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ModelCallError::Timeout
                } else {
                    ModelCallError::Transport
                }
            })?;
        if !response.status().is_success() {
            return Err(ModelCallError::HttpStatus);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MODEL_RESPONSE_BYTES)
        {
            return Err(ModelCallError::InvalidResponse);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| ModelCallError::Transport)?;
        if bytes.len() as u64 > MAX_MODEL_RESPONSE_BYTES {
            return Err(ModelCallError::InvalidResponse);
        }
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| ModelCallError::InvalidResponse)?;
        envelope
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
            .map(str::to_owned)
            .ok_or(ModelCallError::InvalidResponse)
    }
}

impl Default for ModelClient {
    fn default() -> Self {
        Self::new(Duration::from_secs(15))
    }
}

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum ModelCallError {
    #[error("model provider is not configured")]
    NotConfigured,
    #[error("model provider configuration is invalid")]
    InvalidConfig,
    #[error("model credential is unavailable")]
    KeyUnavailable,
    #[error("model request timed out")]
    Timeout,
    #[error("model transport failed")]
    Transport,
    #[error("model returned a non-success status")]
    HttpStatus,
    #[error("model returned an invalid response")]
    InvalidResponse,
}

impl ModelCallError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "MODEL_NOT_CONFIGURED",
            Self::InvalidConfig => "MODEL_CONFIG_INVALID",
            Self::KeyUnavailable => "MODEL_KEY_UNAVAILABLE",
            Self::Timeout => "MODEL_TIMEOUT",
            Self::Transport => "MODEL_TRANSPORT_ERROR",
            Self::HttpStatus => "MODEL_HTTP_ERROR",
            Self::InvalidResponse => "MODEL_INVALID_RESPONSE",
        }
    }
}

#[derive(Debug, Error)]
pub enum M3Error {
    #[error("invalid request")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
        status: StatusCode,
    },
    #[error("resource not found")]
    NotFound { resource: &'static str, id: String },
    #[error("request conflicts with current state")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("storage unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("secret store unavailable")]
    SecretStore,
    #[error("projection mutation failed")]
    Projection(#[source] ProjectionError),
    #[error("internal M3 error")]
    Internal,
}

impl M3Error {
    pub(crate) fn bad(code: &'static str, message: &'static str, details: Value) -> Self {
        Self::BadRequest {
            code,
            message,
            details,
            status: StatusCode::BAD_REQUEST,
        }
    }

    pub(crate) fn not_found(resource: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            resource,
            id: id.into(),
        }
    }
}

impl From<ProjectionError> for M3Error {
    fn from(value: ProjectionError) -> Self {
        Self::Projection(value)
    }
}

impl IntoResponse for M3Error {
    fn into_response(self) -> Response {
        if let Self::Projection(error) = self {
            return error.into_response();
        }
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details) = match self {
            Self::BadRequest {
                code,
                message,
                details,
                status,
            } => (status, code, message, details),
            Self::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的 Agent 辅助对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "M3 storage operation failed");
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
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Agent 辅助处理失败",
                json!({}),
            ),
            Self::Projection(_) => unreachable!("projection errors return above"),
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

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<String, M3Error> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            M3Error::bad(
                "IDEMPOTENCY_KEY_REQUIRED",
                "该 Agent 辅助操作需要 Idempotency-Key",
                json!({}),
            )
        })
}

pub(crate) fn payload_sha256(value: impl AsRef<[u8]>) -> String {
    Sha256::digest(value.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn m3_meta(request_id: String, revision: i64, freshness: Freshness) -> ApiMeta {
    let status = match freshness {
        Freshness::Fresh => DataSourceStatus::Fresh,
        Freshness::Stale => DataSourceStatus::Stale,
        Freshness::Unavailable => DataSourceStatus::Unavailable,
    };
    ApiMeta {
        request_id,
        revision,
        generated_at: now(),
        freshness,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status,
            label: "Agent · OpenAI 兼容 · 可选辅助".to_owned(),
        },
    }
}

pub(crate) async fn ensure_workspace(pool: &SqlitePool) -> Result<(), M3Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO workspaces(workspace_id, owner_id, created_at) VALUES (?, 'owner-local', ?)",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .bind(now())
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(M3Error::Storage)
}

pub(crate) async fn replay_mutation<T: DeserializeOwned>(
    pool: &SqlitePool,
    resource_kind: &str,
    resource_id: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<T>, M3Error> {
    let row = sqlx::query(
        "SELECT request_sha256, response_json FROM m3_mutation_requests
         WHERE resource_kind = ? AND resource_id = ? AND idempotency_key = ?",
    )
    .bind(resource_kind)
    .bind(resource_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M3Error::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_hash: String = row.try_get("request_sha256").map_err(M3Error::Storage)?;
    if stored_hash != request_hash {
        return Err(M3Error::Conflict {
            code: "IDEMPOTENCY_KEY_REUSED",
            message: "Idempotency-Key 已用于不同请求",
            details: json!({"resource_kind": resource_kind, "resource_id": resource_id}),
        });
    }
    let payload: String = row.try_get("response_json").map_err(M3Error::Storage)?;
    serde_json::from_str(&payload)
        .map(Some)
        .map_err(|_| M3Error::Internal)
}

pub(crate) async fn store_mutation<T: Serialize>(
    connection: &mut SqliteConnection,
    resource_kind: &str,
    resource_id: &str,
    key: &str,
    request_hash: &str,
    response: &T,
) -> Result<(), M3Error> {
    let response_json = serde_json::to_string(response).map_err(|_| M3Error::Internal)?;
    sqlx::query(
        "INSERT INTO m3_mutation_requests(
            request_id, resource_kind, resource_id, idempotency_key,
            request_sha256, response_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(resource_kind)
    .bind(resource_id)
    .bind(key)
    .bind(request_hash)
    .bind(response_json)
    .bind(now())
    .execute(connection)
    .await
    .map(|_| ())
    .map_err(M3Error::Storage)
}

fn invalid_json(_error: JsonRejection) -> M3Error {
    M3Error::bad("INVALID_JSON", "请求正文不是符合契约的 JSON", json!({}))
}

fn chat_completions_url(base_url: &str) -> Result<Url, ()> {
    let parsed = Url::parse(base_url).map_err(|_| ())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(());
    }
    if parsed
        .path()
        .trim_end_matches('/')
        .ends_with("/chat/completions")
    {
        return Ok(parsed);
    }
    Url::parse(&format!(
        "{}/chat/completions",
        base_url.trim_end_matches('/')
    ))
    .map_err(|_| ())
}

fn validate_config(request: &ModelProviderPutRequest) -> Result<(), M3Error> {
    chat_completions_url(&request.base_url).map_err(|_| {
        M3Error::bad(
            "INVALID_MODEL_URL",
            "模型 URL 必须是无凭据、查询或片段的 HTTP(S) 地址",
            json!({}),
        )
    })?;
    let model_length = request.model.trim().chars().count();
    if !(1..=MAX_MODEL_CHARS).contains(&model_length)
        || request.model.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(M3Error::bad(
            "INVALID_MODEL",
            "模型名称必须为 1 到 160 个可见字符",
            json!({}),
        ));
    }
    Ok(())
}

pub(crate) async fn load_config(pool: &SqlitePool) -> Result<Option<ModelProviderConfig>, M3Error> {
    let row = sqlx::query(
        "SELECT base_url, model, credential_ref, revision, updated_at
         FROM model_provider_configs WHERE workspace_id = ?",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(M3Error::Storage)?;
    row.map(|row| {
        Ok(ModelProviderConfig {
            base_url: row.try_get("base_url").map_err(M3Error::Storage)?,
            model: row.try_get("model").map_err(M3Error::Storage)?,
            credential_ref: row.try_get("credential_ref").map_err(M3Error::Storage)?,
            revision: row.try_get("revision").map_err(M3Error::Storage)?,
            updated_at: row.try_get("updated_at").map_err(M3Error::Storage)?,
        })
    })
    .transpose()
}

fn config_response(config: &ModelProviderConfig, request_id: String) -> ModelProviderResponse {
    ModelProviderResponse {
        data: ModelProviderData {
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            key_state: "stored".to_owned(),
            revision: config.revision,
            updated_at: config.updated_at.clone(),
        },
        meta: m3_meta(request_id, config.revision, Freshness::Fresh),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/model-provider",
    tag = "m3",
    responses(
        (status = 200, body = ModelProviderResponse),
        (status = 404, body = ApiErrorResponse)
    )
)]
pub async fn get_model_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ModelProviderResponse>, M3Error> {
    let config = load_config(&state.pool)
        .await?
        .ok_or_else(|| M3Error::not_found("model_provider", DEFAULT_WORKSPACE_ID))?;
    Ok(Json(config_response(&config, request_id(&headers))))
}

#[utoipa::path(
    put,
    path = "/api/v1/model-provider",
    tag = "m3",
    params(("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")),
    request_body = ModelProviderPutRequest,
    responses(
        (status = 200, body = ModelProviderResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 409, body = ApiErrorResponse)
    )
)]
pub async fn put_model_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<ModelProviderPutRequest>, JsonRejection>,
) -> Result<Json<ModelProviderResponse>, M3Error> {
    let Json(mut request) = payload.map_err(invalid_json)?;
    request.base_url = request.base_url.trim().trim_end_matches('/').to_owned();
    request.model = request.model.trim().to_owned();
    validate_config(&request)?;
    state
        .secrets
        .resolve_model_key(&request.credential_ref)
        .await
        .map_err(|error| match error {
            SecretStoreError::InvalidReference | SecretStoreError::NotFound => {
                M3Error::bad("MODEL_KEY_NOT_FOUND", "模型 Key 引用不存在", json!({}))
            }
            _ => M3Error::SecretStore,
        })?;
    ensure_workspace(&state.pool).await?;
    let key = idempotency_key(&headers)?;
    let request_hash = payload_sha256(serde_json::to_vec(&request).map_err(|_| M3Error::Internal)?);
    if let Some(response) = replay_mutation(
        &state.pool,
        "model_provider",
        DEFAULT_WORKSPACE_ID,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let current_revision = load_config(&state.pool)
        .await?
        .map_or(0, |config| config.revision);
    let config = ModelProviderConfig {
        base_url: request.base_url,
        model: request.model,
        credential_ref: request.credential_ref,
        revision: current_revision + 1,
        updated_at: now(),
    };
    let response = config_response(&config, request_id(&headers));
    let mut tx = state.pool.begin().await.map_err(M3Error::Storage)?;
    sqlx::query(
        "INSERT INTO model_provider_configs(
            workspace_id, base_url, model, credential_ref, revision, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(workspace_id) DO UPDATE SET
            base_url = excluded.base_url,
            model = excluded.model,
            credential_ref = excluded.credential_ref,
            revision = excluded.revision,
            updated_at = excluded.updated_at",
    )
    .bind(DEFAULT_WORKSPACE_ID)
    .bind(&config.base_url)
    .bind(&config.model)
    .bind(&config.credential_ref)
    .bind(config.revision)
    .bind(&config.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(M3Error::Storage)?;
    store_mutation(
        &mut tx,
        "model_provider",
        DEFAULT_WORKSPACE_ID,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(M3Error::Storage)?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/api/v1/model-provider/test",
    tag = "m3",
    params(("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")),
    responses((status = 200, body = ModelProviderTestResponse))
)]
pub async fn test_model_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ModelProviderTestResponse>, M3Error> {
    let key = idempotency_key(&headers)?;
    ensure_workspace(&state.pool).await?;
    let config = load_config(&state.pool).await?;
    let config_revision = config.as_ref().map_or(0, |value| value.revision);
    let request_hash = payload_sha256(format!("model-test:{config_revision}"));
    if let Some(response) = replay_mutation(
        &state.pool,
        "model_provider_test",
        DEFAULT_WORKSPACE_ID,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let test_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let result =
        if let Some(config) = config {
            match state
                .secrets
                .resolve_model_key(&config.credential_ref)
                .await
            {
                Ok(api_key) => state
                    .model_client
                    .chat(
                        &config,
                        &api_key,
                        vec![json!({
                            "role": "user",
                            "content": "Reply with a short JSON object containing {\"ok\":true}."
                        })],
                    )
                    .await,
                Err(_) => Err(ModelCallError::KeyUnavailable),
            }
        } else {
            Err(ModelCallError::NotConfigured)
        };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (state_value, error_code) = match result {
        Ok(_) => (ModelProviderTestState::Reachable, None),
        Err(error) => (
            ModelProviderTestState::Failed,
            Some(error.code().to_owned()),
        ),
    };
    let response = ModelProviderTestResponse {
        data: ModelProviderTestData {
            test_id: test_id.clone(),
            state: state_value.clone(),
            latency_ms,
            error_code: error_code.clone(),
        },
        meta: m3_meta(request_id(&headers), config_revision, Freshness::Fresh),
    };
    let mut tx = state.pool.begin().await.map_err(M3Error::Storage)?;
    sqlx::query(
        "INSERT INTO model_provider_test_runs(
            test_id, workspace_id, state, error_code, latency_ms, created_at
         ) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&test_id)
    .bind(DEFAULT_WORKSPACE_ID)
    .bind(match state_value {
        ModelProviderTestState::Reachable => "reachable",
        ModelProviderTestState::Failed => "failed",
    })
    .bind(&error_code)
    .bind(i64::try_from(latency_ms).unwrap_or(i64::MAX))
    .bind(now())
    .execute(&mut *tx)
    .await
    .map_err(M3Error::Storage)?;
    store_mutation(
        &mut tx,
        "model_provider_test",
        DEFAULT_WORKSPACE_ID,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(M3Error::Storage)?;
    Ok(Json(response))
}

pub(crate) async fn call_configured_model(
    state: &AppState,
    messages: Vec<Value>,
) -> Result<(ModelProviderConfig, String), ModelCallError> {
    let config = load_config(&state.pool)
        .await
        .map_err(|_| ModelCallError::Transport)?
        .ok_or(ModelCallError::NotConfigured)?;
    let api_key = state
        .secrets
        .resolve_model_key(&config.credential_ref)
        .await
        .map_err(|_| ModelCallError::KeyUnavailable)?;
    let content = state.model_client.chat(&config, &api_key, messages).await?;
    Ok((config, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_chat_completions_without_losing_the_v1_path() {
        assert_eq!(
            chat_completions_url("https://fixture.invalid/v1")
                .unwrap()
                .as_str(),
            "https://fixture.invalid/v1/chat/completions"
        );
        assert!(chat_completions_url("file:///tmp/model").is_err());
        assert!(chat_completions_url("https://user:pass@fixture.invalid/v1").is_err());
    }
}
