use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Request, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{CONTENT_SECURITY_POLICY, ORIGIN, SET_COOKIE},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, DataSourceDescriptor, DataSourceKind,
        DataSourceStatus, Freshness,
    },
    events,
};

pub const SESSION_COOKIE: &str = "network_atlas_session";
const DEFAULT_OWNER_ID: &str = "owner-local";
const DEFAULT_SESSION_HOURS: i64 = 12;
const MAX_USERNAME_CHARS: usize = 128;
const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Clone)]
pub struct AuthConfig {
    enabled: bool,
    owner_id: String,
    username: String,
    password_hash: Option<String>,
    allowed_origin: Option<String>,
    cookie_secure: bool,
    session_ttl: ChronoDuration,
    login_limit: u32,
    write_limit: u32,
}

impl AuthConfig {
    pub fn development_disabled() -> Self {
        Self {
            enabled: false,
            owner_id: DEFAULT_OWNER_ID.to_owned(),
            username: "owner".to_owned(),
            password_hash: None,
            allowed_origin: None,
            cookie_secure: false,
            session_ttl: ChronoDuration::hours(DEFAULT_SESSION_HOURS),
            login_limit: 5,
            write_limit: 120,
        }
    }

    pub fn required(
        username: impl Into<String>,
        password_hash: impl Into<String>,
        allowed_origin: impl Into<String>,
        cookie_secure: bool,
    ) -> Result<Self, AuthConfigError> {
        let username = username.into();
        let password_hash = password_hash.into();
        let allowed_origin = allowed_origin.into();
        validate_username(&username)?;
        let parsed_hash =
            PasswordHash::new(&password_hash).map_err(|_| AuthConfigError::InvalidPasswordHash)?;
        if parsed_hash.algorithm.as_str() != "argon2id" {
            return Err(AuthConfigError::InvalidPasswordHash);
        }
        let allowed_origin = validate_origin_config(&allowed_origin, cookie_secure)?;
        Ok(Self {
            enabled: true,
            owner_id: DEFAULT_OWNER_ID.to_owned(),
            username,
            password_hash: Some(password_hash),
            allowed_origin: Some(allowed_origin),
            cookie_secure,
            session_ttl: ChronoDuration::hours(DEFAULT_SESSION_HOURS),
            login_limit: 5,
            write_limit: 120,
        })
    }

    pub fn from_environment(bind: &str) -> Result<Self, AuthConfigError> {
        let mode = env::var("NETWORK_ATLAS_AUTH_MODE")
            .unwrap_or_else(|_| "auto".to_owned())
            .to_ascii_lowercase();
        if mode == "disabled" {
            if !is_loopback_bind(bind) {
                return Err(AuthConfigError::DisabledOnPublicBind);
            }
            return Ok(Self::development_disabled());
        }
        if !matches!(mode.as_str(), "auto" | "required") {
            return Err(AuthConfigError::InvalidMode);
        }

        let username = env::var("NETWORK_ATLAS_OWNER_USERNAME").ok();
        let password_hash = secret_setting(
            "NETWORK_ATLAS_OWNER_PASSWORD_HASH",
            "NETWORK_ATLAS_OWNER_PASSWORD_HASH_FILE",
        )?;
        let allowed_origin = env::var("NETWORK_ATLAS_ALLOWED_ORIGIN").ok();
        let supplied = username.is_some() || password_hash.is_some() || allowed_origin.is_some();
        if !supplied && mode == "auto" && is_loopback_bind(bind) {
            return Ok(Self::development_disabled());
        }
        let cookie_secure = env_bool("NETWORK_ATLAS_COOKIE_SECURE", true)?;
        let mut config = Self::required(
            username.ok_or(AuthConfigError::MissingRequired)?,
            password_hash.ok_or(AuthConfigError::MissingRequired)?,
            allowed_origin.ok_or(AuthConfigError::MissingRequired)?,
            cookie_secure,
        )?;
        let hours = env_i64("NETWORK_ATLAS_SESSION_HOURS", DEFAULT_SESSION_HOURS, 1, 168)?;
        config.session_ttl = ChronoDuration::hours(hours);
        config.login_limit = env_u32("NETWORK_ATLAS_LOGIN_LIMIT_PER_MINUTE", 5, 1, 100)?;
        config.write_limit = env_u32("NETWORK_ATLAS_WRITE_LIMIT_PER_MINUTE", 120, 1, 10_000)?;
        Ok(config)
    }

    pub fn with_limits(mut self, login_limit: u32, write_limit: u32) -> Self {
        self.login_limit = login_limit;
        self.write_limit = write_limit;
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Debug, Error)]
pub enum AuthConfigError {
    #[error("NETWORK_ATLAS_AUTH_MODE must be auto, required, or disabled")]
    InvalidMode,
    #[error("authentication cannot be disabled on a non-loopback bind")]
    DisabledOnPublicBind,
    #[error("owner username, Argon2id password hash, and allowed origin are required")]
    MissingRequired,
    #[error("owner username is invalid")]
    InvalidUsername,
    #[error("owner password hash is not a valid Argon2 password hash")]
    InvalidPasswordHash,
    #[error("allowed origin must be an exact HTTPS origin, except loopback development")]
    InvalidOrigin,
    #[error("environment setting is invalid: {0}")]
    InvalidEnvironment(&'static str),
}

#[derive(Clone)]
pub struct AuthService {
    config: Arc<AuthConfig>,
    limiter: Arc<RateLimiter>,
}

impl AuthService {
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
            limiter: Arc::new(RateLimiter::default()),
        }
    }

    pub fn development_disabled() -> Self {
        Self::new(AuthConfig::development_disabled())
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[derive(Debug, Clone)]
pub struct AuthenticatedOwner {
    pub owner_id: String,
    pub username: String,
    pub session_id: Option<String>,
    pub expires_at: Option<String>,
    pub csrf_token: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AuthSessionData {
    pub enabled: bool,
    pub authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AuthSessionResponse {
    pub data: AuthSessionData,
    pub meta: ApiMeta,
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("authentication is required")]
    Unauthorized,
    #[error("credentials are invalid")]
    InvalidCredentials,
    #[error("request origin is invalid")]
    InvalidOrigin,
    #[error("CSRF token is invalid")]
    InvalidCsrf,
    #[error("request rate limit exceeded")]
    RateLimited { retry_after: u64 },
    #[error("request body is invalid")]
    InvalidJson,
    #[error("authentication storage is unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("password verification failed")]
    Verification,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details, retry_after) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "AUTH_REQUIRED",
                "请先登录所有者账户",
                json!({}),
                None,
            ),
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
                "用户名或密码不正确",
                json!({}),
                None,
            ),
            Self::InvalidOrigin => (
                StatusCode::FORBIDDEN,
                "ORIGIN_REJECTED",
                "请求来源不符合当前部署配置",
                json!({}),
                None,
            ),
            Self::InvalidCsrf => (
                StatusCode::FORBIDDEN,
                "CSRF_REJECTED",
                "请求缺少有效的会话保护令牌",
                json!({}),
                None,
            ),
            Self::RateLimited { retry_after } => (
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "请求过于频繁，请稍后重试",
                json!({"retry_after_seconds": retry_after}),
                Some(retry_after),
            ),
            Self::InvalidJson => (
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "请求正文不是符合契约的 JSON",
                json!({}),
                None,
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "authentication storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "AUTH_STORAGE_UNAVAILABLE",
                    "会话存储暂不可用",
                    json!({}),
                    None,
                )
            }
            Self::Verification => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "AUTH_VERIFICATION_FAILED",
                "所有者凭据校验失败",
                json!({}),
                None,
            ),
        };
        let mut response = (
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
            .into_response();
        if let Some(value) =
            retry_after.and_then(|value| HeaderValue::from_str(&value.to_string()).ok())
        {
            response.headers_mut().insert("retry-after", value);
        }
        response
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "m4",
    request_body = LoginRequest,
    responses(
        (status = 200, body = AuthSessionResponse),
        (status = 401, body = ApiErrorResponse),
        (status = 429, body = ApiErrorResponse)
    )
)]
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, AuthError> {
    if !state.auth.enabled() {
        return Ok(Json(disabled_session()).into_response());
    }
    validate_request_origin(&state.auth, &headers)?;
    let retry_after = state.auth.limiter.check(
        "login",
        state.auth.config.login_limit,
        Duration::from_secs(60),
    );
    if let Some(retry_after) = retry_after {
        record_audit(
            &state.pool,
            "anonymous",
            "auth.login.rate_limited",
            "owner",
            StatusCode::TOO_MANY_REQUESTS,
            request_id(&headers),
            json!({}),
        )
        .await;
        return Err(AuthError::RateLimited { retry_after });
    }
    let Json(request) = payload.map_err(|_| AuthError::InvalidJson)?;
    let username_matches = constant_time_eq(
        request.username.as_bytes(),
        state.auth.config.username.as_bytes(),
    );
    let password_valid = if request.password.len() <= MAX_PASSWORD_BYTES {
        let password = request.password;
        let encoded = state
            .auth
            .config
            .password_hash
            .clone()
            .ok_or(AuthError::Verification)?;
        tokio::task::spawn_blocking(move || {
            let parsed = PasswordHash::new(&encoded).map_err(|_| AuthError::Verification)?;
            Ok::<_, AuthError>(
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok(),
            )
        })
        .await
        .map_err(|_| AuthError::Verification)??
    } else {
        false
    };
    if !username_matches || !password_valid {
        record_audit(
            &state.pool,
            "anonymous",
            "auth.login.failed",
            "owner",
            StatusCode::UNAUTHORIZED,
            request_id(&headers),
            json!({}),
        )
        .await;
        return Err(AuthError::InvalidCredentials);
    }

    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let token_sha256 = sha256(&token);
    let csrf_token = csrf_token(&token);
    let session_id = Uuid::new_v4().to_string();
    let created_at = now();
    let expires_at =
        (Utc::now() + state.auth.config.session_ttl).to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut tx = state.pool.begin().await.map_err(AuthError::Storage)?;
    sqlx::query("DELETE FROM owner_sessions WHERE expires_at <= ? OR revoked_at IS NOT NULL")
        .bind(&created_at)
        .execute(&mut *tx)
        .await
        .map_err(AuthError::Storage)?;
    sqlx::query(
        "INSERT INTO owner_sessions(
            session_id, owner_id, token_sha256, created_at, expires_at, last_seen_at, revoked_at
         ) VALUES (?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&session_id)
    .bind(&state.auth.config.owner_id)
    .bind(&token_sha256)
    .bind(&created_at)
    .bind(&expires_at)
    .bind(&created_at)
    .execute(&mut *tx)
    .await
    .map_err(AuthError::Storage)?;
    insert_audit(
        &mut tx,
        &state.auth.config.owner_id,
        "auth.login.succeeded",
        "owner",
        StatusCode::OK,
        &request_id(&headers),
        json!({}),
    )
    .await?;
    tx.commit().await.map_err(AuthError::Storage)?;

    let response = AuthSessionResponse {
        data: AuthSessionData {
            enabled: true,
            authenticated: true,
            owner_id: Some(state.auth.config.owner_id.clone()),
            username: Some(state.auth.config.username.clone()),
            expires_at: Some(expires_at),
            csrf_token: Some(csrf_token),
        },
        meta: auth_meta(request_id(&headers)),
    };
    let mut response = Json(response).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session_cookie(
            &token,
            state.auth.config.cookie_secure,
            state.auth.config.session_ttl.num_seconds(),
        ))
        .expect("session cookie is valid"),
    );
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/api/v1/auth/session",
    tag = "m4",
    responses((status = 200, body = AuthSessionResponse), (status = 401, body = ApiErrorResponse))
)]
pub async fn get_session(
    Extension(owner): Extension<AuthenticatedOwner>,
) -> Json<AuthSessionResponse> {
    Json(AuthSessionResponse {
        data: AuthSessionData {
            enabled: owner.session_id.is_some(),
            authenticated: true,
            owner_id: Some(owner.owner_id),
            username: Some(owner.username),
            expires_at: owner.expires_at,
            csrf_token: owner.csrf_token,
        },
        meta: auth_meta(Uuid::new_v4().to_string()),
    })
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "m4",
    responses((status = 200, body = AuthSessionResponse), (status = 401, body = ApiErrorResponse))
)]
pub async fn logout(
    State(state): State<AppState>,
    Extension(owner): Extension<AuthenticatedOwner>,
) -> Result<Response, AuthError> {
    if let Some(session_id) = owner.session_id {
        let revoked_at = now();
        let mut tx = state.pool.begin().await.map_err(AuthError::Storage)?;
        sqlx::query("UPDATE owner_sessions SET revoked_at = ? WHERE session_id = ?")
            .bind(&revoked_at)
            .bind(&session_id)
            .execute(&mut *tx)
            .await
            .map_err(AuthError::Storage)?;
        insert_audit(
            &mut tx,
            &owner.owner_id,
            "auth.logout",
            "owner",
            StatusCode::OK,
            &Uuid::new_v4().to_string(),
            json!({}),
        )
        .await?;
        tx.commit().await.map_err(AuthError::Storage)?;
    }
    let mut response = Json(AuthSessionResponse {
        data: AuthSessionData {
            enabled: state.auth.enabled(),
            authenticated: !state.auth.enabled(),
            owner_id: (!state.auth.enabled()).then(|| DEFAULT_OWNER_ID.to_owned()),
            username: (!state.auth.enabled()).then(|| "owner".to_owned()),
            expires_at: None,
            csrf_token: None,
        },
        meta: auth_meta(Uuid::new_v4().to_string()),
    })
    .into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&expired_session_cookie(state.auth.config.cookie_secure))
            .expect("expired session cookie is valid"),
    );
    Ok(response)
}

pub async fn protect_business_api(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let owner = match authenticate(&state, &headers).await {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    if state.auth.enabled() && is_unsafe(&method) {
        if let Err(error) = validate_request_origin(&state.auth, &headers) {
            return error.into_response();
        }
        let supplied = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let expected = owner.csrf_token.as_deref().unwrap_or_default();
        if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
            return AuthError::InvalidCsrf.into_response();
        }
        let key = format!("write:{}", owner.session_id.as_deref().unwrap_or("unknown"));
        if let Some(retry_after) =
            state
                .auth
                .limiter
                .check(&key, state.auth.config.write_limit, Duration::from_secs(60))
        {
            return AuthError::RateLimited { retry_after }.into_response();
        }
    }
    request.extensions_mut().insert(owner.clone());
    let request_identifier = request_id(&headers);
    let response = next.run(request).await;
    if is_unsafe(&method) && !path.ends_with("/auth/logout") {
        let status = response.status();
        let kind = audit_kind(&method, &path);
        record_audit(
            &state.pool,
            &owner.owner_id,
            kind,
            &sanitize_target(&path),
            status,
            request_identifier,
            json!({"method": method.as_str()}),
        )
        .await;
        if status.is_success()
            && let Some(kind) = events::event_kind_for_request(&method, &path)
            && let Err(error) = events::publish(
                &state.pool,
                kind,
                &sanitize_target(&path),
                0,
                json!({"method": method.as_str(), "status": status.as_u16()}),
            )
            .await
        {
            tracing::error!(error = %error, "failed to persist committed change event");
        }
    }
    response
}

pub async fn add_security_headers(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        ),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if path == "/api/v1/events/stream" {
        headers.insert(
            "cache-control",
            HeaderValue::from_static("no-cache, no-transform"),
        );
    } else if path.starts_with("/api/") || path == "/openapi.json" {
        headers.insert("cache-control", HeaderValue::from_static("no-store"));
    }
    response
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedOwner, AuthError> {
    if !state.auth.enabled() {
        return Ok(AuthenticatedOwner {
            owner_id: DEFAULT_OWNER_ID.to_owned(),
            username: "owner".to_owned(),
            session_id: None,
            expires_at: None,
            csrf_token: None,
        });
    }
    let token = cookie_value(headers, SESSION_COOKIE).ok_or(AuthError::Unauthorized)?;
    let token_sha256 = sha256(&token);
    let now = now();
    let row = sqlx::query(
        "SELECT session_id, owner_id, expires_at FROM owner_sessions
         WHERE token_sha256 = ? AND revoked_at IS NULL AND expires_at > ?",
    )
    .bind(token_sha256)
    .bind(&now)
    .fetch_optional(&state.pool)
    .await
    .map_err(AuthError::Storage)?
    .ok_or(AuthError::Unauthorized)?;
    let session_id: String = row.try_get("session_id").map_err(AuthError::Storage)?;
    let owner_id: String = row.try_get("owner_id").map_err(AuthError::Storage)?;
    let expires_at: String = row.try_get("expires_at").map_err(AuthError::Storage)?;
    sqlx::query("UPDATE owner_sessions SET last_seen_at = ? WHERE session_id = ?")
        .bind(&now)
        .bind(&session_id)
        .execute(&state.pool)
        .await
        .map_err(AuthError::Storage)?;
    Ok(AuthenticatedOwner {
        owner_id,
        username: state.auth.config.username.clone(),
        session_id: Some(session_id),
        expires_at: Some(expires_at),
        csrf_token: Some(csrf_token(&token)),
    })
}

pub async fn record_audit(
    pool: &SqlitePool,
    actor_id: &str,
    kind: &str,
    target_ref: &str,
    status: StatusCode,
    request_id: String,
    summary: Value,
) {
    if let Err(error) = sqlx::query(
        "INSERT INTO audit_events(
            workspace_id, actor_id, kind, target_ref, status_code, request_id, summary_json, occurred_at
         ) VALUES ('workspace-default', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(actor_id)
    .bind(kind)
    .bind(target_ref)
    .bind(i64::from(status.as_u16()))
    .bind(request_id)
    .bind(summary.to_string())
    .bind(now())
    .execute(pool)
    .await
    {
        tracing::error!(error = %error, kind, "failed to persist audit event");
    }
}

async fn insert_audit(
    connection: &mut sqlx::SqliteConnection,
    actor_id: &str,
    kind: &str,
    target_ref: &str,
    status: StatusCode,
    request_id: &str,
    summary: Value,
) -> Result<(), AuthError> {
    sqlx::query(
        "INSERT INTO audit_events(
            workspace_id, actor_id, kind, target_ref, status_code, request_id, summary_json, occurred_at
         ) VALUES ('workspace-default', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(actor_id)
    .bind(kind)
    .bind(target_ref)
    .bind(i64::from(status.as_u16()))
    .bind(request_id)
    .bind(summary.to_string())
    .bind(now())
    .execute(connection)
    .await
    .map(|_| ())
    .map_err(AuthError::Storage)
}

fn disabled_session() -> AuthSessionResponse {
    AuthSessionResponse {
        data: AuthSessionData {
            enabled: false,
            authenticated: true,
            owner_id: Some(DEFAULT_OWNER_ID.to_owned()),
            username: Some("owner".to_owned()),
            expires_at: None,
            csrf_token: None,
        },
        meta: auth_meta(Uuid::new_v4().to_string()),
    }
}

fn auth_meta(request_id: String) -> ApiMeta {
    ApiMeta {
        request_id,
        revision: 1,
        generated_at: now(),
        freshness: Freshness::Fresh,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status: DataSourceStatus::Fresh,
            label: "所有者会话".to_owned(),
        },
    }
}

fn validate_request_origin(auth: &AuthService, headers: &HeaderMap) -> Result<(), AuthError> {
    let supplied = headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(AuthError::InvalidOrigin)?;
    let expected = auth
        .config
        .allowed_origin
        .as_deref()
        .ok_or(AuthError::InvalidOrigin)?;
    constant_time_eq(supplied.as_bytes(), expected.as_bytes())
        .then_some(())
        .ok_or(AuthError::InvalidOrigin)
}

fn validate_username(username: &str) -> Result<(), AuthConfigError> {
    let chars = username.chars().count();
    if !(1..=MAX_USERNAME_CHARS).contains(&chars)
        || username.trim() != username
        || username.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(AuthConfigError::InvalidUsername);
    }
    Ok(())
}

fn validate_origin_config(origin: &str, cookie_secure: bool) -> Result<String, AuthConfigError> {
    let parsed = Url::parse(origin).map_err(|_| AuthConfigError::InvalidOrigin)?;
    let loopback = parsed.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
        || (!loopback && parsed.scheme() != "https")
        || (cookie_secure && parsed.scheme() != "https")
    {
        return Err(AuthConfigError::InvalidOrigin);
    }
    Ok(parsed.origin().ascii_serialization())
}

fn is_loopback_bind(bind: &str) -> bool {
    bind.parse::<SocketAddr>()
        .is_ok_and(|address| address.ip().is_loopback())
        || bind.starts_with("localhost:")
}

fn env_bool(name: &'static str, default: bool) -> Result<bool, AuthConfigError> {
    match env::var(name) {
        Ok(value) if value.eq_ignore_ascii_case("true") || value == "1" => Ok(true),
        Ok(value) if value.eq_ignore_ascii_case("false") || value == "0" => Ok(false),
        Ok(_) => Err(AuthConfigError::InvalidEnvironment(name)),
        Err(_) => Ok(default),
    }
}

fn secret_setting(
    value_name: &'static str,
    file_name: &'static str,
) -> Result<Option<String>, AuthConfigError> {
    let direct = env::var(value_name).ok();
    let path = env::var(file_name).ok();
    if direct.is_some() && path.is_some() {
        return Err(AuthConfigError::InvalidEnvironment(value_name));
    }
    if let Some(value) = direct {
        return Ok(Some(value));
    }
    let Some(path) = path else {
        return Ok(None);
    };
    let value = std::fs::read_to_string(path)
        .map_err(|_| AuthConfigError::InvalidEnvironment(file_name))?;
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > 1024 {
        return Err(AuthConfigError::InvalidEnvironment(file_name));
    }
    Ok(Some(value))
}

fn env_i64(name: &'static str, default: i64, min: i64, max: i64) -> Result<i64, AuthConfigError> {
    let value = match env::var(name) {
        Ok(value) => value
            .parse::<i64>()
            .map_err(|_| AuthConfigError::InvalidEnvironment(name))?,
        Err(_) => default,
    };
    (min..=max)
        .contains(&value)
        .then_some(value)
        .ok_or(AuthConfigError::InvalidEnvironment(name))
}

fn env_u32(name: &'static str, default: u32, min: u32, max: u32) -> Result<u32, AuthConfigError> {
    let value = match env::var(name) {
        Ok(value) => value
            .parse::<u32>()
            .map_err(|_| AuthConfigError::InvalidEnvironment(name))?,
        Err(_) => default,
    };
    (min..=max)
        .contains(&value)
        .then_some(value)
        .ok_or(AuthConfigError::InvalidEnvironment(name))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn session_cookie(token: &str, secure: bool, max_age: i64) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

fn expired_session_cookie(secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

fn csrf_token(session_token: &str) -> String {
    sha256(format!("network-atlas-csrf:{session_token}"))
}

fn sha256(value: impl AsRef<[u8]>) -> String {
    Sha256::digest(value.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn is_unsafe(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn audit_kind(method: &Method, path: &str) -> &'static str {
    if path.ends_with("/auth/logout") {
        return "auth.logout.request";
    }
    if method == Method::DELETE {
        return "data.deleted";
    }
    if path.contains("discovery-runs") {
        return "discovery.requested";
    }
    if path.contains("project-agents") || path.ends_with("/agent") {
        return "project_agent.changed";
    }
    if path.contains("technical-projects")
        || path.contains("project-targets")
        || path.contains("deployment-candidates")
        || path.contains("/deployments/")
    {
        return "catalog.changed";
    }
    if path.contains("connection-tests") || path.contains("host-key-confirmations") {
        return "host.connection.changed";
    }
    if path.contains("projection-drafts")
        || path.contains("layouts")
        || path.contains("ignore-rules")
    {
        return "projection.changed";
    }
    if path.contains("onboarding") || path.contains("model-provider") {
        return "onboarding.changed";
    }
    if path.contains("hosts") {
        return "host.changed";
    }
    if path.contains("secret-refs") {
        return "secret.reference.changed";
    }
    "api.write"
}

fn sanitize_target(path: &str) -> String {
    path.chars().take(512).collect()
}

fn request_id(headers: &HeaderMap) -> String {
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

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Default)]
struct RateLimiter {
    buckets: Mutex<HashMap<String, RateBucket>>,
}

struct RateBucket {
    window_started: Instant,
    count: u32,
}

impl RateLimiter {
    fn check(&self, key: &str, limit: u32, window: Duration) -> Option<u64> {
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        let now = Instant::now();
        let bucket = buckets.entry(key.to_owned()).or_insert(RateBucket {
            window_started: now,
            count: 0,
        });
        if now.duration_since(bucket.window_started) >= window {
            bucket.window_started = now;
            bucket.count = 0;
        }
        if bucket.count >= limit {
            let elapsed = now.duration_since(bucket.window_started);
            return Some(window.saturating_sub(elapsed).as_secs().max(1));
        }
        bucket.count += 1;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_bind_never_accepts_disabled_auth() {
        assert!(!is_loopback_bind("0.0.0.0:8787"));
        assert!(is_loopback_bind("127.0.0.1:8787"));
        assert!(is_loopback_bind("[::1]:8787"));
    }

    #[test]
    fn cookie_parser_uses_an_exact_name() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_static("other=one; network_atlas_session=token-value"),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("token-value")
        );
    }

    #[test]
    fn strict_origin_rejects_paths_and_plaintext_public_hosts() {
        assert_eq!(
            validate_origin_config("https://app.example/", true).unwrap(),
            "https://app.example"
        );
        assert!(validate_origin_config("https://app.example/path", true).is_err());
        assert!(validate_origin_config("http://app.example", false).is_err());
        assert!(validate_origin_config("http://127.0.0.1:8787", false).is_ok());
    }

    #[test]
    fn catalog_writes_have_audit_kind_and_projection_change_event() {
        assert_eq!(
            audit_kind(&Method::POST, "/api/v1/project-targets"),
            "catalog.changed"
        );
        assert_eq!(
            events::event_kind_for_request(&Method::POST, "/api/v1/project-targets"),
            Some(events::ChangeEventKind::ProjectionChanged)
        );
    }
}
