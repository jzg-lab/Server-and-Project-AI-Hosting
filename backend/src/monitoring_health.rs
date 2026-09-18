//! H3c HOST health policy, deterministic per-run evaluation and health bands.
//!
//! This module deliberately consumes only the typed samples belonging to the
//! admitted run.  H3b rollups are a presentation/history tier and never feed
//! the evaluator.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header::ETAG},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, CpuBusyHealthRule, CpuBusySeriesKind,
        DataSourceDescriptor, DataSourceKind, DataSourceStatus, Freshness, HealthConditionStatus,
        HealthObservationState, HealthRequirement, HighHealthRule, HostCurrentHealth,
        HostHealthBandResolution, HostHealthCondition, HostHealthConditionCoverage, HostHealthData,
        HostHealthPolicyConfigurationState, HostHealthPolicyData, HostHealthPolicyPutRequest,
        HostHealthPolicyRecord, HostHealthPolicyResponse, HostHealthResponse, HostHealthStatus,
        HostHealthTimeBucket, HostHealthTimeWindow, HostHealthWindow, LowHealthRule,
        MonitorFreshness, MonitorRunTrigger,
    },
    monitoring_history,
};

const MAX_FILESYSTEM_RULES: usize = 64;
const MAX_STREAK: u32 = 10;
// metric_samples stores byte gauges as SQLite REAL. Keep configurable
// absolute boundaries inside f64's exact-integer range so an equality edge
// does not move when a u64 policy value is compared with a stored sample.
const MAX_EXACT_F64_INTEGER: u64 = 1_u64 << 53;
const SOURCE_KIND: &str = "user_confirmed";
const CREATED_BY: &str = "owner-local";

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("invalid health policy or window")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("host was not found")]
    NotFound { resource: &'static str, id: String },
    #[error("health policy precondition failed")]
    PreconditionFailed { expected_revision: i64 },
    #[error("health policy precondition is required")]
    PreconditionRequired,
    #[error("health policy conflicts with current state")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("health storage unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("health evaluation failed")]
    Internal,
}

impl IntoResponse for HealthError {
    fn into_response(self) -> Response {
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
                "请求的 HOST 不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::PreconditionFailed { expected_revision } => (
                StatusCode::PRECONDITION_FAILED,
                "REVISION_MISMATCH",
                "健康策略已被其他请求更新，请刷新后重试",
                json!({"expected_revision": expected_revision}),
            ),
            Self::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "IF_MATCH_REQUIRED",
                "更新健康策略需要 If-Match 修订号",
                json!({}),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "health storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地健康评价存储暂不可用",
                    json!({}),
                )
            }
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "健康评价内部处理失败",
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

#[derive(Debug, Clone)]
struct PolicyRow {
    policy_version_id: String,
    policy_id: String,
    host_id: String,
    revision: i64,
    enabled: bool,
    policy_json: String,
    policy_sha256: String,
    effective_from_at: String,
    created_at: String,
}

#[derive(Debug, Clone)]
struct SampleRow {
    sample_id: String,
    family: String,
    subject_kind: String,
    subject_id: String,
    metric_name: String,
    value_real: Option<f64>,
    window_seconds: Option<f64>,
    quality: String,
}

#[derive(Debug, Clone)]
struct PreviousCondition {
    status: HealthConditionStatus,
    candidate_status: HealthConditionStatus,
    value: Option<f64>,
}

#[derive(Debug, Clone)]
struct ConditionDraft {
    condition_key: String,
    condition_kind: String,
    requirement: HealthRequirement,
    subject_kind: String,
    subject_id: String,
    subject_label: String,
    candidate_status: HealthConditionStatus,
    status: HealthConditionStatus,
    reason_code: String,
    value: Option<f64>,
    unit: String,
    window_seconds: Option<f64>,
    streak_count: u32,
    streak_required: u32,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct EvaluationTiming {
    pub evaluated_at: DateTime<Utc>,
    pub observation_state: HealthObservationState,
    pub observed_at: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthWindowQuery {
    pub window: Option<String>,
}

#[derive(Debug, Clone)]
struct WindowSpec {
    name: HostHealthWindow,
    resolution: HostHealthBandResolution,
    width_seconds: i64,
    bucket_count: usize,
}

#[derive(Debug, Clone)]
struct BandEvaluation {
    status: HostHealthStatus,
    reason_code: String,
    policy_revision: Option<i64>,
    schedule_id: Option<String>,
    schedule_version_id: Option<String>,
    scheduled_for_epoch_ms: Option<i64>,
    evaluated_at_epoch_ms: i64,
    late: bool,
}

#[derive(Debug, Clone)]
struct ScheduleGrid {
    schedule_id: String,
    due_from_epoch_ms: i64,
    due_until_epoch_ms: Option<i64>,
    interval_ms: i64,
}

fn meta(request_id: &HeaderMap, revision: i64, freshness: Freshness) -> ApiMeta {
    ApiMeta {
        request_id: request_id
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        revision,
        generated_at: timestamp(Utc::now()),
        freshness,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status: DataSourceStatus::Fresh,
            label: "SQLite · HOST 健康策略与评价".to_owned(),
        },
    }
}

fn etag(revision: i64) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("\"revision-{revision}\"")) {
        headers.insert(ETAG, value);
    }
    headers
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/health-policy",
    tag = "monitoring",
    params(("host_id" = String, Path, description = "Registered Linux host identifier")),
    responses(
        (status = 200, body = HostHealthPolicyResponse),
        (status = 404, body = ApiErrorResponse)
    )
)]
pub async fn get_health_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<(HeaderMap, Json<HostHealthPolicyResponse>), HealthError> {
    ensure_host(&state.pool, &host_id).await?;
    let policy = load_policy(&state.pool, &host_id).await?;
    let revision = policy.as_ref().map_or(0, |row| row.revision);
    let data = policy_data(&host_id, policy.as_ref())?;
    Ok((
        etag(revision),
        Json(HostHealthPolicyResponse {
            data,
            meta: meta(&headers, revision, Freshness::Fresh),
        }),
    ))
}

#[utoipa::path(
    put,
    path = "/api/v1/hosts/{host_id}/health-policy",
    tag = "monitoring",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("If-Match" = String, Header, description = "Current revision-N ETag")
    ),
    request_body = HostHealthPolicyPutRequest,
    responses(
        (status = 200, body = HostHealthPolicyResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 412, body = ApiErrorResponse),
        (status = 428, body = ApiErrorResponse)
    )
)]
pub async fn put_health_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    payload: Result<Json<HostHealthPolicyPutRequest>, JsonRejection>,
) -> Result<(HeaderMap, Json<HostHealthPolicyResponse>), HealthError> {
    let Json(mut request) =
        payload.map_err(|_| bad_request("INVALID_JSON", "请求正文不是符合契约的 JSON"))?;
    normalize_and_validate_policy(&mut request)?;
    let expected_revision = if_match_revision(&headers)?;
    // HOST deletion takes the write side of this gate. A policy update must be
    // inside the same admission boundary as run creation and verified backup.
    let _admission = state.observation_admission.read().await;
    ensure_host(&state.pool, &host_id).await?;
    let request_json = serde_json::to_string(&request).map_err(|_| HealthError::Internal)?;
    let policy_sha256 = digest_text(&request_json);
    let mut tx = state.pool.begin().await.map_err(HealthError::Storage)?;
    let current = load_policy_tx(&mut tx, &host_id).await?;
    let actual_revision = current.as_ref().map_or(0, |row| row.revision);
    if actual_revision != expected_revision {
        tx.rollback().await.map_err(HealthError::Storage)?;
        return Err(HealthError::PreconditionFailed {
            expected_revision: actual_revision,
        });
    }
    if current
        .as_ref()
        .is_some_and(|row| row.policy_sha256 == policy_sha256)
    {
        tx.commit().await.map_err(HealthError::Storage)?;
        let policy = load_policy(&state.pool, &host_id).await?;
        let revision = policy.as_ref().map_or(0, |row| row.revision);
        return Ok((
            etag(revision),
            Json(HostHealthPolicyResponse {
                data: policy_data(&host_id, policy.as_ref())?,
                meta: meta(&headers, revision, Freshness::Fresh),
            }),
        ));
    }
    let now = Utc::now();
    let now_text = timestamp_ms(now);
    let now_epoch = now.timestamp_millis();
    if let Some(row) = current.as_ref() {
        sqlx::query(
            "UPDATE health_policy_versions
             SET lifecycle_state = 'superseded', effective_until_at = ?,
                 effective_until_at_epoch_ms = ?
             WHERE policy_version_id = ? AND lifecycle_state = 'current'",
        )
        .bind(&now_text)
        .bind(now_epoch)
        .bind(&row.policy_version_id)
        .execute(&mut *tx)
        .await
        .map_err(HealthError::Storage)?;
    }
    let policy_id = current
        .as_ref()
        .map_or_else(|| Uuid::new_v4().to_string(), |row| row.policy_id.clone());
    let revision = actual_revision + 1;
    let version_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO health_policy_versions(
             policy_version_id, policy_id, host_id, revision, lifecycle_state,
             enabled, source_kind, policy_json, policy_sha256,
             effective_from_at, effective_from_at_epoch_ms,
             created_by, created_at
         ) VALUES (?, ?, ?, ?, 'current', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&version_id)
    .bind(&policy_id)
    .bind(&host_id)
    .bind(revision)
    .bind(request.enabled)
    .bind(SOURCE_KIND)
    .bind(&request_json)
    .bind(&policy_sha256)
    .bind(&now_text)
    .bind(now_epoch)
    .bind(CREATED_BY)
    .bind(&now_text)
    .execute(&mut *tx)
    .await
    .map_err(HealthError::Storage)?;
    tx.commit().await.map_err(HealthError::Storage)?;
    let policy = load_policy(&state.pool, &host_id).await?;
    let data = policy_data(&host_id, policy.as_ref())?;
    Ok((
        etag(revision),
        Json(HostHealthPolicyResponse {
            data,
            meta: meta(&headers, revision, Freshness::Fresh),
        }),
    ))
}

fn bad_request(code: &'static str, message: &'static str) -> HealthError {
    HealthError::BadRequest {
        code,
        message,
        details: json!({}),
    }
}

fn normalize_and_validate_policy(
    request: &mut HostHealthPolicyPutRequest,
) -> Result<(), HealthError> {
    if request.filesystems.len() > MAX_FILESYSTEM_RULES {
        return Err(HealthError::BadRequest {
            code: "TOO_MANY_FILESYSTEM_RULES",
            message: "文件系统策略数量超出上限",
            details: json!({"maximum": MAX_FILESYSTEM_RULES}),
        });
    }
    request.filesystems.sort_by(|a, b| a.mount.cmp(&b.mount));
    let mut mounts = BTreeSet::new();
    for rule in &request.filesystems {
        if rule.mount.is_empty()
            || rule.mount.len() > 256
            || !rule.mount.starts_with('/')
            || rule.mount.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(bad_request(
                "INVALID_FILESYSTEM_MOUNT",
                "文件系统挂载点格式不受支持",
            ));
        }
        if !mounts.insert(rule.mount.clone()) {
            return Err(bad_request(
                "DUPLICATE_FILESYSTEM_RULE",
                "同一挂载点只能配置一条策略",
            ));
        }
        if rule
            .critical_available_bytes_below
            .is_some_and(|value| value == 0 || value > MAX_EXACT_F64_INTEGER)
        {
            return Err(bad_request(
                "INVALID_FILESYSTEM_AVAILABLE_THRESHOLD",
                "文件系统绝对可用空间阈值必须大于零",
            ));
        }
        validate_high_rule(
            rule.warning_at_or_above,
            rule.critical_at_or_above,
            rule.recovery_below,
            rule.enter_count,
            rule.recover_count,
            0.0,
            1.0,
            "INVALID_FILESYSTEM_THRESHOLD",
        )?;
    }
    if let Some(rule) = request.cpu_busy.as_ref() {
        if !matches!(rule.series_kind, CpuBusySeriesKind::CollectorWindow)
            || !finite_between(rule.minimum_window_seconds, 0.1, 60.0)
        {
            return Err(bad_request(
                "INVALID_CPU_SERIES",
                "CPU 策略只能评价采集窗口序列",
            ));
        }
        validate_high_rule(
            rule.warning_at_or_above,
            rule.critical_at_or_above,
            rule.recovery_below,
            rule.enter_count,
            rule.recover_count,
            0.0,
            100.0,
            "INVALID_CPU_THRESHOLD",
        )?;
    }
    if let Some(rule) = request.normalized_load5.as_ref() {
        validate_high_rule(
            rule.warning_at_or_above,
            rule.critical_at_or_above,
            rule.recovery_below,
            rule.enter_count,
            rule.recover_count,
            0.0,
            100.0,
            "INVALID_LOAD_THRESHOLD",
        )?;
    }
    if let Some(rule) = request.memory_available_ratio.as_ref() {
        validate_low_rule(
            rule.warning_below,
            rule.critical_below,
            rule.recovery_at_or_above,
            rule.enter_count,
            rule.recover_count,
            "INVALID_MEMORY_THRESHOLD",
        )?;
    }
    if request.enabled
        && request.cpu_busy.is_none()
        && request.memory_available_ratio.is_none()
        && request.normalized_load5.is_none()
        && request.filesystems.is_empty()
    {
        return Err(bad_request(
            "EMPTY_HEALTH_POLICY",
            "启用策略至少需要一个受控指标",
        ));
    }
    let has_required = request
        .cpu_busy
        .as_ref()
        .is_some_and(|rule| rule.requirement == HealthRequirement::Required)
        || request
            .memory_available_ratio
            .as_ref()
            .is_some_and(|rule| rule.requirement == HealthRequirement::Required)
        || request
            .normalized_load5
            .as_ref()
            .is_some_and(|rule| rule.requirement == HealthRequirement::Required)
        || request
            .filesystems
            .iter()
            .any(|rule| rule.requirement == HealthRequirement::Required);
    if request.enabled && !has_required {
        return Err(bad_request(
            "REQUIRED_HEALTH_CONDITION_MISSING",
            "启用策略至少需要一个 required 条件",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_high_rule(
    warning: f64,
    critical: f64,
    recovery: f64,
    enter_count: u32,
    recover_count: u32,
    minimum: f64,
    maximum: f64,
    code: &'static str,
) -> Result<(), HealthError> {
    if !finite_between(warning, minimum, maximum)
        || !finite_between(critical, minimum, maximum)
        || !finite_between(recovery, minimum, maximum)
        || recovery >= warning
        || warning >= critical
        || !(1..=MAX_STREAK).contains(&enter_count)
        || !(1..=MAX_STREAK).contains(&recover_count)
    {
        return Err(HealthError::BadRequest {
            code,
            message: "高阈值策略的范围或连续次数无效",
            details: json!({}),
        });
    }
    Ok(())
}

fn validate_low_rule(
    warning: f64,
    critical: f64,
    recovery: f64,
    enter_count: u32,
    recover_count: u32,
    code: &'static str,
) -> Result<(), HealthError> {
    if !finite_between(warning, 0.0, 1.0)
        || !finite_between(critical, 0.0, 1.0)
        || !finite_between(recovery, warning, 1.0)
        || critical >= warning
        || !(1..=MAX_STREAK).contains(&enter_count)
        || !(1..=MAX_STREAK).contains(&recover_count)
    {
        return Err(HealthError::BadRequest {
            code,
            message: "低阈值策略的范围或连续次数无效",
            details: json!({}),
        });
    }
    Ok(())
}

fn finite_between(value: f64, minimum: f64, maximum: f64) -> bool {
    value.is_finite() && value >= minimum && value <= maximum
}

fn if_match_revision(headers: &HeaderMap) -> Result<i64, HealthError> {
    let value = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .ok_or(HealthError::PreconditionRequired)?;
    value
        .trim_matches('"')
        .strip_prefix("revision-")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .ok_or_else(|| bad_request("INVALID_IF_MATCH", "If-Match 必须使用 revision-N 格式"))
}

async fn ensure_host(pool: &SqlitePool, host_id: &str) -> Result<(), HealthError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .fetch_optional(pool)
        .await
        .map_err(HealthError::Storage)?;
    if exists.is_none() {
        return Err(HealthError::NotFound {
            resource: "host",
            id: host_id.to_owned(),
        });
    }
    Ok(())
}

async fn load_policy(pool: &SqlitePool, host_id: &str) -> Result<Option<PolicyRow>, HealthError> {
    let row = sqlx::query(
        "SELECT policy_version_id, policy_id, host_id, revision, enabled,
                policy_json, policy_sha256, effective_from_at, created_at
         FROM health_policy_versions
         WHERE host_id = ? AND lifecycle_state = 'current'",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(HealthError::Storage)?;
    row.map(policy_from_row).transpose()
}

async fn load_policy_tx(
    tx: &mut Transaction<'_, Sqlite>,
    host_id: &str,
) -> Result<Option<PolicyRow>, HealthError> {
    let row = sqlx::query(
        "SELECT policy_version_id, policy_id, host_id, revision, enabled,
                policy_json, policy_sha256, effective_from_at, created_at
         FROM health_policy_versions
         WHERE host_id = ? AND lifecycle_state = 'current'",
    )
    .bind(host_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(HealthError::Storage)?;
    row.map(policy_from_row).transpose()
}

fn policy_from_row(row: sqlx::sqlite::SqliteRow) -> Result<PolicyRow, HealthError> {
    Ok(PolicyRow {
        policy_version_id: row
            .try_get("policy_version_id")
            .map_err(|_| HealthError::Internal)?,
        policy_id: row
            .try_get("policy_id")
            .map_err(|_| HealthError::Internal)?,
        host_id: row.try_get("host_id").map_err(|_| HealthError::Internal)?,
        revision: row.try_get("revision").map_err(|_| HealthError::Internal)?,
        enabled: row
            .try_get::<i64, _>("enabled")
            .map_err(|_| HealthError::Internal)?
            == 1,
        policy_json: row
            .try_get("policy_json")
            .map_err(|_| HealthError::Internal)?,
        policy_sha256: row
            .try_get("policy_sha256")
            .map_err(|_| HealthError::Internal)?,
        effective_from_at: row
            .try_get("effective_from_at")
            .map_err(|_| HealthError::Internal)?,
        created_at: row
            .try_get("created_at")
            .map_err(|_| HealthError::Internal)?,
    })
}

fn policy_config(row: &PolicyRow) -> HostHealthPolicyPutRequest {
    serde_json::from_str(&row.policy_json).unwrap_or(HostHealthPolicyPutRequest {
        enabled: row.enabled,
        cpu_busy: None,
        memory_available_ratio: None,
        normalized_load5: None,
        filesystems: Vec::new(),
    })
}

fn policy_data(
    host_id: &str,
    row: Option<&PolicyRow>,
) -> Result<HostHealthPolicyData, HealthError> {
    let Some(row) = row else {
        return Ok(HostHealthPolicyData {
            host_id: host_id.to_owned(),
            configured: false,
            state: HostHealthPolicyConfigurationState::NotConfigured,
            policy: None,
        });
    };
    let config = policy_config(row);
    Ok(HostHealthPolicyData {
        host_id: host_id.to_owned(),
        configured: true,
        state: if row.enabled {
            HostHealthPolicyConfigurationState::Enabled
        } else {
            HostHealthPolicyConfigurationState::Disabled
        },
        policy: Some(HostHealthPolicyRecord {
            policy_id: row.policy_id.clone(),
            policy_version_id: row.policy_version_id.clone(),
            host_id: row.host_id.clone(),
            revision: row.revision,
            enabled: row.enabled,
            source_kind: SOURCE_KIND.to_owned(),
            cpu_busy: config.cpu_busy,
            memory_available_ratio: config.memory_available_ratio,
            normalized_load5: config.normalized_load5,
            filesystems: config.filesystems,
            policy_sha256: row.policy_sha256.clone(),
            effective_from_at: row.effective_from_at.clone(),
            created_at: row.created_at.clone(),
        }),
    })
}

fn quality_is_observed(quality: &str, value: Option<f64>) -> bool {
    quality == "observed" && value.is_some_and(f64::is_finite)
}

async fn load_samples(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    host_id: &str,
) -> Result<Vec<SampleRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT sample_id, family, subject_kind, subject_id, metric_name,
                value_real, window_seconds, quality
         FROM metric_samples
         WHERE run_id = ? AND host_id = ?
         ORDER BY sample_id",
    )
    .bind(run_id)
    .bind(host_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(SampleRow {
                sample_id: row.try_get("sample_id")?,
                family: row.try_get("family")?,
                subject_kind: row.try_get("subject_kind")?,
                subject_id: row.try_get("subject_id")?,
                metric_name: row.try_get("metric_name")?,
                value_real: row.try_get("value_real")?,
                window_seconds: row.try_get("window_seconds")?,
                quality: row.try_get("quality")?,
            })
        })
        .collect()
}

fn find_sample<'a>(
    samples: &'a [SampleRow],
    family: &str,
    subject_kind: &str,
    subject_id: &str,
    metric_name: &str,
) -> Option<&'a SampleRow> {
    samples.iter().find(|sample| {
        sample.family == family
            && sample.subject_kind == subject_kind
            && sample.subject_id == subject_id
            && sample.metric_name == metric_name
    })
}

#[allow(clippy::too_many_arguments)]
fn unknown_draft(
    key: impl Into<String>,
    kind: impl Into<String>,
    requirement: HealthRequirement,
    subject_kind: impl Into<String>,
    subject_id: impl Into<String>,
    label: impl Into<String>,
    reason: impl Into<String>,
    unit: impl Into<String>,
    streak_required: u32,
) -> ConditionDraft {
    ConditionDraft {
        condition_key: key.into(),
        condition_kind: kind.into(),
        requirement,
        subject_kind: subject_kind.into(),
        subject_id: subject_id.into(),
        subject_label: label.into(),
        candidate_status: HealthConditionStatus::Unknown,
        status: HealthConditionStatus::Unknown,
        reason_code: reason.into(),
        value: None,
        unit: unit.into(),
        window_seconds: None,
        streak_count: 0,
        streak_required,
        evidence_refs: Vec::new(),
    }
}

fn high_candidate(value: f64, rule: &HighHealthRule) -> (HealthConditionStatus, bool) {
    let candidate = if value >= rule.critical_at_or_above {
        HealthConditionStatus::Critical
    } else if value >= rule.warning_at_or_above {
        HealthConditionStatus::Warning
    } else {
        HealthConditionStatus::Ok
    };
    (candidate, value < rule.recovery_below)
}

fn cpu_candidate(value: f64, rule: &CpuBusyHealthRule) -> (HealthConditionStatus, bool) {
    let candidate = if value >= rule.critical_at_or_above {
        HealthConditionStatus::Critical
    } else if value >= rule.warning_at_or_above {
        HealthConditionStatus::Warning
    } else {
        HealthConditionStatus::Ok
    };
    (candidate, value < rule.recovery_below)
}

fn low_candidate(value: f64, rule: &LowHealthRule) -> (HealthConditionStatus, bool) {
    let candidate = if value < rule.critical_below {
        HealthConditionStatus::Critical
    } else if value < rule.warning_below {
        HealthConditionStatus::Warning
    } else {
        HealthConditionStatus::Ok
    };
    (candidate, value >= rule.recovery_at_or_above)
}

fn severity(status: &HealthConditionStatus) -> u8 {
    match status {
        HealthConditionStatus::Ok => 0,
        HealthConditionStatus::Warning => 1,
        HealthConditionStatus::Critical => 2,
        HealthConditionStatus::Unknown | HealthConditionStatus::Stale => 0,
    }
}

fn is_unknown(status: &HealthConditionStatus) -> bool {
    matches!(
        status,
        HealthConditionStatus::Unknown | HealthConditionStatus::Stale
    )
}

fn count_consecutive<F>(
    current: &HealthConditionStatus,
    history: &[PreviousCondition],
    predicate: F,
) -> u32
where
    F: Fn(&HealthConditionStatus) -> bool,
{
    let mut count = 0u32;
    if predicate(current) {
        count = 1;
        for previous in history {
            if is_unknown(&previous.candidate_status) || !predicate(&previous.candidate_status) {
                break;
            }
            count = count.saturating_add(1);
        }
    }
    count
}

fn count_recovery<F>(
    current_recovered: bool,
    current: &HealthConditionStatus,
    history: &[PreviousCondition],
    predicate: F,
) -> u32
where
    F: Fn(Option<f64>) -> bool,
{
    if !current_recovered {
        return 0;
    }
    let mut count = 1u32;
    for previous in history {
        if is_unknown(&previous.candidate_status) || !predicate(previous.value) {
            break;
        }
        count = count.saturating_add(1);
    }
    // Keep the parameter in the signature explicit: a recovery sample with a
    // still-critical candidate is valid only when the typed recovery rule
    // says so; callers already supplied that decision.
    let _ = current;
    count
}

#[allow(clippy::too_many_arguments)]
fn debounced_status(
    candidate: HealthConditionStatus,
    recovery_now: bool,
    previous: Option<&PreviousCondition>,
    history: &[PreviousCondition],
    enter_count: u32,
    recover_count: u32,
    recovery_predicate: impl Fn(Option<f64>) -> bool,
    missed_due: bool,
) -> (HealthConditionStatus, u32, String) {
    if is_unknown(&candidate) {
        return (
            HealthConditionStatus::Unknown,
            0,
            "metric_unavailable".to_owned(),
        );
    }
    if missed_due {
        // A catch-up run is a fresh observation, not continuity across the
        // unobserved due slots.
        return if severity(&candidate) == 0 {
            (HealthConditionStatus::Ok, 1, "ok_after_gap".to_owned())
        } else {
            (
                HealthConditionStatus::Unknown,
                1,
                "threshold_pending_after_gap".to_owned(),
            )
        };
    }
    if let Some(previous) = previous {
        match previous.status {
            HealthConditionStatus::Critical | HealthConditionStatus::Warning => {
                if recovery_now {
                    let count =
                        count_recovery(recovery_now, &candidate, history, recovery_predicate);
                    if count >= recover_count {
                        return (HealthConditionStatus::Ok, count, "recovered".to_owned());
                    }
                    return (
                        previous.status.clone(),
                        count,
                        "recovery_pending".to_owned(),
                    );
                }
                if previous.status == HealthConditionStatus::Warning
                    && candidate == HealthConditionStatus::Critical
                {
                    let count = count_consecutive(&candidate, history, |value| {
                        *value == HealthConditionStatus::Critical
                    });
                    if count >= enter_count {
                        return (
                            HealthConditionStatus::Critical,
                            count,
                            "threshold_critical".to_owned(),
                        );
                    }
                    return (
                        HealthConditionStatus::Warning,
                        count,
                        "threshold_pending".to_owned(),
                    );
                }
                return (previous.status.clone(), 1, "threshold_active".to_owned());
            }
            HealthConditionStatus::Ok
            | HealthConditionStatus::Unknown
            | HealthConditionStatus::Stale => {}
        }
    }
    if candidate == HealthConditionStatus::Ok {
        return (HealthConditionStatus::Ok, 1, "ok".to_owned());
    }
    let broad_count = count_consecutive(&candidate, history, |value| severity(value) >= 1);
    if candidate == HealthConditionStatus::Critical {
        let critical_count = count_consecutive(&candidate, history, |value| {
            *value == HealthConditionStatus::Critical
        });
        if critical_count >= enter_count {
            return (
                HealthConditionStatus::Critical,
                critical_count,
                "threshold_critical".to_owned(),
            );
        }
        if broad_count >= enter_count {
            return (
                HealthConditionStatus::Warning,
                broad_count,
                "threshold_warning".to_owned(),
            );
        }
        return (
            HealthConditionStatus::Unknown,
            broad_count,
            "threshold_pending".to_owned(),
        );
    }
    if broad_count >= enter_count {
        (
            HealthConditionStatus::Warning,
            broad_count,
            "threshold_warning".to_owned(),
        )
    } else {
        (
            HealthConditionStatus::Unknown,
            broad_count,
            "threshold_pending".to_owned(),
        )
    }
}

async fn previous_conditions(
    tx: &mut Transaction<'_, Sqlite>,
    host_id: &str,
    policy_version_id: &str,
    condition_key: &str,
    continuity_at_epoch_ms: i64,
) -> Result<Vec<PreviousCondition>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT c.status, c.candidate_status, c.value_real,
                e.valid_until_epoch_ms, e.evaluated_at_epoch_ms
         FROM health_condition_evaluations c
         JOIN health_evaluations e ON e.evaluation_id = c.evaluation_id
         WHERE e.host_id = ? AND e.policy_version_id = ? AND c.condition_key = ?
         ORDER BY e.evaluated_at_epoch_ms DESC, e.evaluation_id DESC LIMIT 20",
    )
    .bind(host_id)
    .bind(policy_version_id)
    .bind(condition_key)
    .fetch_all(&mut **tx)
    .await?;
    let previous = rows
        .into_iter()
        .map(|row| {
            let valid_until: Option<i64> = row.try_get("valid_until_epoch_ms")?;
            Ok((
                PreviousCondition {
                    status: parse_condition_status(&row.try_get::<String, _>("status")?),
                    candidate_status: parse_condition_status(
                        &row.try_get::<String, _>("candidate_status")?,
                    ),
                    value: row.try_get("value_real")?,
                },
                valid_until,
                row.try_get::<i64, _>("evaluated_at_epoch_ms")?,
            ))
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    let mut contiguous = Vec::new();
    let mut newer_evaluation_at = continuity_at_epoch_ms;
    for (condition, valid_until, evaluated_at) in previous {
        if valid_until.is_none_or(|value| value <= newer_evaluation_at) {
            break;
        }
        contiguous.push(condition);
        newer_evaluation_at = evaluated_at;
    }
    Ok(contiguous)
}

/// Write one immutable HOST-scope health receipt and its condition rows. The
/// caller owns the surrounding terminal-run transaction, so samples/current
/// and health are committed or rolled back together.
pub(crate) async fn evaluate_run_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    host_id: &str,
    mut timing: EvaluationTiming,
) -> Result<(), sqlx::Error> {
    let already: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM health_evaluations WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await?;
    if already.is_some() {
        return Ok(());
    }
    let run = sqlx::query(
        "SELECT health_policy_version_id, missed_due_count, state
         FROM monitor_runs WHERE run_id = ? AND host_id = ?",
    )
    .bind(run_id)
    .bind(host_id)
    .fetch_one(&mut **tx)
    .await?;
    let policy_version_id: Option<String> = run.try_get("health_policy_version_id")?;
    let missed_due_count: i64 = run
        .try_get::<Option<i64>, _>("missed_due_count")?
        .unwrap_or(0);
    let run_state: String = run.try_get("state")?;
    let has_observation = matches!(run_state.as_str(), "succeeded" | "partial");
    if !has_observation {
        // Terminal failures, timeouts, overlaps and interruptions are explicit
        // unknown receipts even if a caller accidentally supplies a freshness
        // window. They must never reuse samples as a green result.
        timing.observation_state = HealthObservationState::None;
        timing.observed_at = None;
        timing.valid_until = None;
    }
    let policy = if let Some(version_id) = policy_version_id.as_deref() {
        sqlx::query(
            "SELECT policy_version_id, policy_id, host_id, revision, enabled,
                    policy_json, policy_sha256, effective_from_at, created_at
             FROM health_policy_versions
             WHERE policy_version_id = ? AND host_id = ?",
        )
        .bind(version_id)
        .bind(host_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(policy_from_row_for_db)
        .transpose()?
    } else {
        None
    };

    let samples = if has_observation && policy.as_ref().is_some_and(|row| row.enabled) {
        load_samples(tx, run_id, host_id).await?
    } else {
        Vec::new()
    };
    let mut drafts = if let Some(policy) = policy.as_ref() {
        let config = policy_config(policy);
        build_condition_drafts(
            tx,
            host_id,
            policy,
            &config,
            &samples,
            missed_due_count > 0,
            timing
                .observed_at
                .unwrap_or(timing.evaluated_at)
                .timestamp_millis(),
        )
        .await?
    } else {
        Vec::new()
    };
    if let Some(policy) = policy.as_ref().filter(|row| !row.enabled) {
        let config = policy_config(policy);
        drafts = build_disabled_drafts(host_id, &config);
    }

    let mut ok_count = 0i64;
    let mut warning_count = 0i64;
    let mut critical_count = 0i64;
    let mut unknown_count = 0i64;
    let mut required_count = 0i64;
    let mut optional_count = 0i64;
    for draft in &drafts {
        match draft.requirement {
            HealthRequirement::Required => required_count += 1,
            HealthRequirement::Optional => optional_count += 1,
        }
        match draft.status {
            HealthConditionStatus::Ok => ok_count += 1,
            HealthConditionStatus::Warning => warning_count += 1,
            HealthConditionStatus::Critical => critical_count += 1,
            HealthConditionStatus::Unknown | HealthConditionStatus::Stale => unknown_count += 1,
        }
    }
    let status = if !policy.as_ref().is_some_and(|row| row.enabled) {
        HostHealthStatus::Unknown
    } else if drafts.iter().any(|draft| {
        draft.requirement == HealthRequirement::Required
            && draft.status == HealthConditionStatus::Critical
    }) {
        HostHealthStatus::Unhealthy
    } else if drafts
        .iter()
        .any(|draft| draft.requirement == HealthRequirement::Required && is_unknown(&draft.status))
    {
        HostHealthStatus::Unknown
    } else if drafts.iter().any(|draft| {
        (draft.requirement == HealthRequirement::Required
            && draft.status == HealthConditionStatus::Warning)
            || (draft.requirement == HealthRequirement::Optional
                && matches!(
                    draft.status,
                    HealthConditionStatus::Warning | HealthConditionStatus::Critical
                ))
    }) {
        HostHealthStatus::Degraded
    } else {
        HostHealthStatus::Healthy
    };
    let reason_code = if policy.is_none() {
        "not_configured"
    } else if !policy.as_ref().is_some_and(|row| row.enabled) {
        "policy_disabled"
    } else if status == HostHealthStatus::Unhealthy {
        "required_critical"
    } else if status == HostHealthStatus::Unknown {
        "required_unknown"
    } else if status == HostHealthStatus::Degraded {
        "threshold_warning"
    } else {
        "all_conditions_ok"
    };
    let evaluated_text = timestamp_ms(timing.evaluated_at);
    let evaluated_epoch = timing.evaluated_at.timestamp_millis();
    let observed_text = timing.observed_at.map(timestamp_ms);
    let observed_epoch = timing.observed_at.map(|value| value.timestamp_millis());
    let valid_until_text = timing.valid_until.map(timestamp_ms);
    let valid_until_epoch = timing.valid_until.map(|value| value.timestamp_millis());
    let input_sha256 = evaluation_digest(
        policy.as_ref().map(|row| row.policy_sha256.as_str()),
        &samples,
        &drafts,
        missed_due_count,
    );
    let evaluation_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO health_evaluations(
             evaluation_id, run_id, host_id, policy_version_id, policy_id,
             policy_revision, status, reason_code, required_condition_count,
             optional_condition_count, ok_count, warning_count, critical_count,
             unknown_count, observation_state, input_sha256, evaluated_at,
             evaluated_at_epoch_ms, observed_at, observed_at_epoch_ms,
             valid_until, valid_until_epoch_ms, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&evaluation_id)
    .bind(run_id)
    .bind(host_id)
    .bind(policy.as_ref().map(|row| row.policy_version_id.as_str()))
    .bind(policy.as_ref().map(|row| row.policy_id.as_str()))
    .bind(policy.as_ref().map(|row| row.revision))
    .bind(host_status_name(&status))
    .bind(reason_code)
    .bind(required_count)
    .bind(optional_count)
    .bind(ok_count)
    .bind(warning_count)
    .bind(critical_count)
    .bind(unknown_count)
    .bind(observation_state_name(&timing.observation_state))
    .bind(&input_sha256)
    .bind(&evaluated_text)
    .bind(evaluated_epoch)
    .bind(observed_text.as_deref())
    .bind(observed_epoch)
    .bind(valid_until_text.as_deref())
    .bind(valid_until_epoch)
    .bind(&evaluated_text)
    .execute(&mut **tx)
    .await?;
    for draft in drafts.drain(..) {
        let condition_id = Uuid::new_v4().to_string();
        let evidence_json =
            serde_json::to_string(&draft.evidence_refs).unwrap_or_else(|_| "[]".to_owned());
        let condition_input = digest_text(&format!(
            "{}:{}:{}:{}",
            input_sha256,
            draft.condition_key,
            condition_status_name(&draft.candidate_status),
            evidence_json
        ));
        sqlx::query(
            "INSERT INTO health_condition_evaluations(
                 condition_evaluation_id, evaluation_id, condition_key,
                 condition_kind, requirement, subject_kind, subject_id,
                 subject_label, status, candidate_status, reason_code,
                 value_real, unit, window_seconds, streak_count, streak_required,
                 evidence_refs_json, input_sha256, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(condition_id)
        .bind(&evaluation_id)
        .bind(&draft.condition_key)
        .bind(&draft.condition_kind)
        .bind(requirement_name(&draft.requirement))
        .bind(&draft.subject_kind)
        .bind(&draft.subject_id)
        .bind(&draft.subject_label)
        .bind(condition_status_name(&draft.status))
        .bind(condition_status_name(&draft.candidate_status))
        .bind(&draft.reason_code)
        .bind(draft.value)
        .bind(&draft.unit)
        .bind(draft.window_seconds)
        .bind(i64::from(draft.streak_count))
        .bind(i64::from(draft.streak_required))
        .bind(evidence_json)
        .bind(condition_input)
        .bind(&evaluated_text)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn policy_from_row_for_db(row: sqlx::sqlite::SqliteRow) -> Result<PolicyRow, sqlx::Error> {
    Ok(PolicyRow {
        policy_version_id: row.try_get("policy_version_id")?,
        policy_id: row.try_get("policy_id")?,
        host_id: row.try_get("host_id")?,
        revision: row.try_get("revision")?,
        enabled: row.try_get::<i64, _>("enabled")? == 1,
        policy_json: row.try_get("policy_json")?,
        policy_sha256: row.try_get("policy_sha256")?,
        effective_from_at: row.try_get("effective_from_at")?,
        created_at: row.try_get("created_at")?,
    })
}

async fn build_condition_drafts(
    tx: &mut Transaction<'_, Sqlite>,
    host_id: &str,
    policy: &PolicyRow,
    config: &HostHealthPolicyPutRequest,
    samples: &[SampleRow],
    missed_due: bool,
    continuity_at_epoch_ms: i64,
) -> Result<Vec<ConditionDraft>, sqlx::Error> {
    let mut drafts = Vec::new();
    if let Some(rule) = config.cpu_busy.as_ref() {
        let key = "cpu_busy".to_owned();
        let requirement = rule.requirement.clone();
        let sample = find_sample(samples, "cpu", "host", host_id, "busy_pct");
        let mut draft = if let Some(sample) =
            sample.filter(|sample| quality_is_observed(&sample.quality, sample.value_real))
        {
            let value = sample.value_real.unwrap_or_default();
            if sample.window_seconds.unwrap_or(0.0) < rule.minimum_window_seconds {
                unknown_draft(
                    key.clone(),
                    "cpu_busy_instant_percent",
                    requirement.clone(),
                    "host",
                    host_id,
                    "CPU busy",
                    "window_too_short",
                    "percent",
                    rule.enter_count,
                )
            } else {
                let (candidate, recovered) = cpu_candidate(value, rule);
                let history = previous_conditions(
                    tx,
                    host_id,
                    &policy.policy_version_id,
                    &key,
                    continuity_at_epoch_ms,
                )
                .await?;
                let previous = history.first();
                let (status, streak, reason) = debounced_status(
                    candidate.clone(),
                    recovered,
                    previous,
                    &history,
                    rule.enter_count,
                    rule.recover_count,
                    |value| value.is_some_and(|value| value < rule.recovery_below),
                    missed_due,
                );
                let streak_required = if status == HealthConditionStatus::Ok {
                    rule.recover_count
                } else {
                    rule.enter_count
                };
                ConditionDraft {
                    condition_key: key.clone(),
                    condition_kind: "cpu_busy_instant_percent".to_owned(),
                    requirement: requirement.clone(),
                    subject_kind: "host".to_owned(),
                    subject_id: host_id.to_owned(),
                    subject_label: "CPU busy".to_owned(),
                    candidate_status: candidate,
                    status,
                    reason_code: reason,
                    value: Some(value),
                    unit: "percent".to_owned(),
                    window_seconds: sample.window_seconds,
                    streak_count: streak,
                    streak_required,
                    evidence_refs: vec![sample.sample_id.clone()],
                }
            }
        } else {
            unknown_draft(
                key.clone(),
                "cpu_busy_instant_percent",
                requirement.clone(),
                "host",
                host_id,
                "CPU busy",
                sample.map_or_else(
                    || "metric_unavailable".to_owned(),
                    |value| quality_reason(&value.quality),
                ),
                "percent",
                rule.enter_count,
            )
        };
        if let Some(sample) = sample {
            draft.evidence_refs.push(sample.sample_id.clone());
            draft.evidence_refs.sort();
            draft.evidence_refs.dedup();
            if draft.value.is_none() {
                draft.window_seconds = sample.window_seconds;
            }
        }
        drafts.push(draft);
    }
    if let Some(rule) = config.memory_available_ratio.as_ref() {
        let key = "memory_available_ratio".to_owned();
        let requirement = rule.requirement.clone();
        let available = find_sample(samples, "memory", "host", host_id, "available_bytes");
        let total = find_sample(samples, "memory", "host", host_id, "total_bytes");
        let valid = available
            .filter(|sample| quality_is_observed(&sample.quality, sample.value_real))
            .and_then(|available| {
                total
                    .filter(|sample| quality_is_observed(&sample.quality, sample.value_real))
                    .map(|total| (available, total))
            });
        let draft = if let Some((available, total)) = valid {
            let total_value = total.value_real.unwrap_or(0.0);
            let value = if total_value > 0.0 {
                available.value_real.unwrap_or(0.0) / total_value
            } else {
                f64::NAN
            };
            if value.is_finite() {
                let (candidate, recovered) = low_candidate(value, rule);
                let history = previous_conditions(
                    tx,
                    host_id,
                    &policy.policy_version_id,
                    &key,
                    continuity_at_epoch_ms,
                )
                .await?;
                let (status, streak, reason) = debounced_status(
                    candidate.clone(),
                    recovered,
                    history.first(),
                    &history,
                    rule.enter_count,
                    rule.recover_count,
                    |value| value.is_some_and(|value| value >= rule.recovery_at_or_above),
                    missed_due,
                );
                let streak_required = if status == HealthConditionStatus::Ok {
                    rule.recover_count
                } else {
                    rule.enter_count
                };
                ConditionDraft {
                    condition_key: key.clone(),
                    condition_kind: "memory_available_ratio".to_owned(),
                    requirement: requirement.clone(),
                    subject_kind: "host".to_owned(),
                    subject_id: host_id.to_owned(),
                    subject_label: "MemAvailable ratio".to_owned(),
                    candidate_status: candidate,
                    status,
                    reason_code: reason,
                    value: Some(value),
                    unit: "ratio".to_owned(),
                    window_seconds: None,
                    streak_count: streak,
                    streak_required,
                    evidence_refs: vec![available.sample_id.clone(), total.sample_id.clone()],
                }
            } else {
                unknown_draft(
                    key,
                    "memory_available_ratio",
                    requirement,
                    "host",
                    host_id,
                    "MemAvailable ratio",
                    "invalid_memory_total",
                    "ratio",
                    rule.enter_count,
                )
            }
        } else {
            unknown_draft(
                key,
                "memory_available_ratio",
                requirement,
                "host",
                host_id,
                "MemAvailable ratio",
                "memory_metric_unavailable",
                "ratio",
                rule.enter_count,
            )
        };
        drafts.push(draft);
    }
    if let Some(rule) = config.normalized_load5.as_ref() {
        let key = "normalized_load5".to_owned();
        let requirement = rule.requirement.clone();
        let sample = find_sample(samples, "load", "host", host_id, "normalized_load5");
        let draft = if let Some(sample) =
            sample.filter(|sample| quality_is_observed(&sample.quality, sample.value_real))
        {
            let value = sample.value_real.unwrap_or_default();
            let (candidate, recovered) = high_candidate(value, rule);
            let history = previous_conditions(
                tx,
                host_id,
                &policy.policy_version_id,
                &key,
                continuity_at_epoch_ms,
            )
            .await?;
            let (status, streak, reason) = debounced_status(
                candidate.clone(),
                recovered,
                history.first(),
                &history,
                rule.enter_count,
                rule.recover_count,
                |value| value.is_some_and(|value| value < rule.recovery_below),
                missed_due,
            );
            let streak_required = if status == HealthConditionStatus::Ok {
                rule.recover_count
            } else {
                rule.enter_count
            };
            ConditionDraft {
                condition_key: key.clone(),
                condition_kind: "normalized_load5".to_owned(),
                requirement: requirement.clone(),
                subject_kind: "host".to_owned(),
                subject_id: host_id.to_owned(),
                subject_label: "normalized load5".to_owned(),
                candidate_status: candidate,
                status,
                reason_code: reason,
                value: Some(value),
                unit: "ratio".to_owned(),
                window_seconds: sample.window_seconds,
                streak_count: streak,
                streak_required,
                evidence_refs: vec![sample.sample_id.clone()],
            }
        } else {
            unknown_draft(
                key,
                "normalized_load5",
                requirement,
                "host",
                host_id,
                "normalized load5",
                sample.map_or_else(
                    || "metric_unavailable".to_owned(),
                    |value| quality_reason(&value.quality),
                ),
                "ratio",
                rule.enter_count,
            )
        };
        drafts.push(draft);
    }
    for rule in &config.filesystems {
        let subject_id = monitoring_history::stable_subject_id("filesystem", &rule.mount);
        let key = format!("filesystem:{subject_id}");
        let requirement = rule.requirement.clone();
        let ratio = find_sample(
            samples,
            "disk_capacity",
            "filesystem",
            &subject_id,
            "allocatable_used_ratio",
        );
        let available = find_sample(
            samples,
            "disk_capacity",
            "filesystem",
            &subject_id,
            "available_bytes",
        );
        let valid_ratio =
            ratio.filter(|sample| quality_is_observed(&sample.quality, sample.value_real));
        let draft = if let Some(ratio) = valid_ratio {
            let value = ratio.value_real.unwrap_or_default();
            let available_value = available
                .filter(|sample| quality_is_observed(&sample.quality, sample.value_real))
                .and_then(|sample| sample.value_real);
            if rule.critical_available_bytes_below.is_some() && available_value.is_none() {
                unknown_draft(
                    key,
                    "filesystem_allocatable_used_ratio",
                    requirement,
                    "filesystem",
                    subject_id,
                    rule.mount.clone(),
                    "filesystem_available_unavailable",
                    "ratio",
                    rule.enter_count,
                )
            } else {
                let mut candidate = if value >= rule.critical_at_or_above {
                    HealthConditionStatus::Critical
                } else if value >= rule.warning_at_or_above {
                    HealthConditionStatus::Warning
                } else {
                    HealthConditionStatus::Ok
                };
                if rule.critical_available_bytes_below.is_some_and(|limit| {
                    available_value.is_some_and(|available| available < limit as f64)
                }) {
                    candidate = HealthConditionStatus::Critical;
                }
                let recovered = value < rule.recovery_below
                    && rule.critical_available_bytes_below.is_none_or(|limit| {
                        available_value.is_some_and(|available| available >= limit as f64)
                    });
                let history = previous_conditions(
                    tx,
                    host_id,
                    &policy.policy_version_id,
                    &key,
                    continuity_at_epoch_ms,
                )
                .await?;
                let (status, streak, reason) = debounced_status(
                    candidate.clone(),
                    recovered,
                    history.first(),
                    &history,
                    rule.enter_count,
                    rule.recover_count,
                    |value| value.is_some_and(|value| value < rule.recovery_below),
                    missed_due,
                );
                let mut evidence = vec![ratio.sample_id.clone()];
                if let Some(available) = available {
                    evidence.push(available.sample_id.clone());
                }
                evidence.sort();
                let streak_required = if status == HealthConditionStatus::Ok {
                    rule.recover_count
                } else {
                    rule.enter_count
                };
                ConditionDraft {
                    condition_key: key.clone(),
                    condition_kind: "filesystem_allocatable_used_ratio".to_owned(),
                    requirement: requirement.clone(),
                    subject_kind: "filesystem".to_owned(),
                    subject_id,
                    subject_label: rule.mount.clone(),
                    candidate_status: candidate,
                    status,
                    reason_code: reason,
                    value: Some(value),
                    unit: "ratio".to_owned(),
                    window_seconds: ratio.window_seconds,
                    streak_count: streak,
                    streak_required,
                    evidence_refs: evidence,
                }
            }
        } else {
            unknown_draft(
                key,
                "filesystem_allocatable_used_ratio",
                requirement,
                "filesystem",
                subject_id,
                rule.mount.clone(),
                ratio.map_or_else(
                    || "filesystem_metric_unavailable".to_owned(),
                    |value| quality_reason(&value.quality),
                ),
                "ratio",
                rule.enter_count,
            )
        };
        drafts.push(draft);
    }
    Ok(drafts)
}

fn build_disabled_drafts(
    host_id: &str,
    config: &HostHealthPolicyPutRequest,
) -> Vec<ConditionDraft> {
    let mut drafts = Vec::new();
    if let Some(rule) = config.cpu_busy.as_ref() {
        drafts.push(unknown_draft(
            "cpu_busy",
            "cpu_busy_instant_percent",
            rule.requirement.clone(),
            "host",
            host_id,
            "CPU busy",
            "policy_disabled",
            "percent",
            rule.enter_count,
        ));
    }
    if let Some(rule) = config.memory_available_ratio.as_ref() {
        drafts.push(unknown_draft(
            "memory_available_ratio",
            "memory_available_ratio",
            rule.requirement.clone(),
            "host",
            host_id,
            "MemAvailable ratio",
            "policy_disabled",
            "ratio",
            rule.enter_count,
        ));
    }
    if let Some(rule) = config.normalized_load5.as_ref() {
        drafts.push(unknown_draft(
            "normalized_load5",
            "normalized_load5",
            rule.requirement.clone(),
            "host",
            host_id,
            "normalized load5",
            "policy_disabled",
            "ratio",
            rule.enter_count,
        ));
    }
    for rule in &config.filesystems {
        let subject_id = monitoring_history::stable_subject_id("filesystem", &rule.mount);
        drafts.push(unknown_draft(
            format!("filesystem:{subject_id}"),
            "filesystem_allocatable_used_ratio",
            rule.requirement.clone(),
            "filesystem",
            subject_id,
            rule.mount.clone(),
            "policy_disabled",
            "ratio",
            rule.enter_count,
        ));
    }
    drafts
}

fn quality_reason(quality: &str) -> String {
    format!("metric_{quality}")
}

fn evaluation_digest(
    policy_sha256: Option<&str>,
    samples: &[SampleRow],
    drafts: &[ConditionDraft],
    missed_due_count: i64,
) -> String {
    let sample_ids: Vec<&str> = samples
        .iter()
        .map(|sample| sample.sample_id.as_str())
        .collect();
    let conditions: Vec<Value> = drafts
        .iter()
        .map(|draft| {
            json!({
                "key": draft.condition_key,
                "candidate": condition_status_name(&draft.candidate_status),
                "evidence": draft.evidence_refs,
            })
        })
        .collect();
    digest_text(
        &json!({
            "policy_sha256": policy_sha256,
            "sample_ids": sample_ids,
            "conditions": conditions,
            "missed_due_count": missed_due_count,
        })
        .to_string(),
    )
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/health",
    tag = "monitoring",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("window" = Option<String>, Query, description = "24h, 7d or 30d")
    ),
    responses(
        (status = 200, body = HostHealthResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse)
    )
)]
pub async fn get_host_health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    query: Result<Query<HealthWindowQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<HostHealthResponse>, HealthError> {
    let Query(query) =
        query.map_err(|_| bad_request("INVALID_HEALTH_WINDOW", "健康时间窗口参数无效"))?;
    ensure_host(&state.pool, &host_id).await?;
    let spec = window_spec(query.window.as_deref())?;
    // One captured instant drives policy/current freshness and every bucket.
    let evaluated_at = Utc::now();
    let policy = load_policy(&state.pool, &host_id).await?;
    let policy_view = policy_data(&host_id, policy.as_ref())?;
    let current = current_health(&state.pool, &host_id, policy.as_ref(), evaluated_at).await?;
    let window = health_window(&state.pool, &host_id, spec, evaluated_at).await?;
    let freshness = match current.freshness {
        MonitorFreshness::Fresh => Freshness::Fresh,
        MonitorFreshness::Stale => Freshness::Stale,
        MonitorFreshness::Unknown => Freshness::Unavailable,
    };
    let revision = policy.as_ref().map_or(0, |row| row.revision);
    Ok(Json(HostHealthResponse {
        data: HostHealthData {
            host_id,
            evaluated_at: timestamp_ms(evaluated_at),
            policy: policy_view,
            current,
            window,
        },
        meta: meta(&headers, revision, freshness),
    }))
}

async fn current_health(
    pool: &SqlitePool,
    host_id: &str,
    current_policy: Option<&PolicyRow>,
    evaluated_at: DateTime<Utc>,
) -> Result<HostCurrentHealth, HealthError> {
    let empty_coverage = || HostHealthConditionCoverage {
        required_total: 0,
        required_ok: 0,
        required_warning: 0,
        required_critical: 0,
        required_unknown: 0,
        optional_total: 0,
        optional_ok: 0,
        optional_warning: 0,
        optional_critical: 0,
        optional_unknown: 0,
    };
    let synthetic = |reason: &str| HostCurrentHealth {
        host_id: host_id.to_owned(),
        health: HostHealthStatus::Unknown,
        reason_code: reason.to_owned(),
        freshness: MonitorFreshness::Unknown,
        observation_state: HealthObservationState::None,
        evaluation_id: None,
        run_id: None,
        trigger_kind: None,
        policy_id: current_policy.map(|row| row.policy_id.clone()),
        policy_revision: current_policy.map(|row| row.revision),
        evaluated_at: timestamp_ms(evaluated_at),
        observed_at: None,
        valid_until: None,
        coverage: empty_coverage(),
        conditions: Vec::new(),
    };
    let Some(policy) = current_policy else {
        return Ok(synthetic("not_configured"));
    };
    if !policy.enabled {
        return Ok(synthetic("policy_disabled"));
    }
    let row = sqlx::query(
        "SELECT e.evaluation_id, e.run_id, e.policy_version_id, e.policy_id,
                e.policy_revision, e.status, e.reason_code, e.observation_state,
                e.evaluated_at, e.observed_at, e.valid_until,
                e.valid_until_epoch_ms, r.trigger_kind, r.state AS run_state
         FROM health_evaluations e
         JOIN monitor_runs r ON r.run_id = e.run_id
         WHERE e.host_id = ? AND e.policy_version_id = ?
         ORDER BY e.evaluated_at_epoch_ms DESC, e.evaluation_id DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(&policy.policy_version_id)
    .fetch_optional(pool)
    .await
    .map_err(HealthError::Storage)?;
    let Some(row) = row else {
        return Ok(synthetic("awaiting_policy_evaluation"));
    };
    let evaluated_policy_version: Option<String> = row
        .try_get("policy_version_id")
        .map_err(HealthError::Storage)?;
    if evaluated_policy_version.as_deref() != Some(policy.policy_version_id.as_str()) {
        return Ok(synthetic("awaiting_policy_evaluation"));
    }
    let evaluation_id: String = row.try_get("evaluation_id").map_err(HealthError::Storage)?;
    let condition_rows = sqlx::query(
        "SELECT condition_key, condition_kind, requirement, subject_kind,
                subject_id, subject_label, status, candidate_status,
                reason_code, value_real, unit, window_seconds, streak_count,
                streak_required, evidence_refs_json
         FROM health_condition_evaluations
         WHERE evaluation_id = ? ORDER BY condition_key",
    )
    .bind(&evaluation_id)
    .fetch_all(pool)
    .await
    .map_err(HealthError::Storage)?;
    let observation_state = parse_observation_state(
        &row.try_get::<String, _>("observation_state")
            .map_err(HealthError::Storage)?,
    );
    let valid_until_epoch: Option<i64> = row
        .try_get("valid_until_epoch_ms")
        .map_err(HealthError::Storage)?;
    let stale = valid_until_epoch.is_some_and(|value| value <= evaluated_at.timestamp_millis());
    let run_state: String = row.try_get("run_state").map_err(HealthError::Storage)?;
    let terminal_failure = matches!(
        run_state.as_str(),
        "failed" | "timed_out" | "skipped_overlap" | "interrupted"
    );
    let freshness = if stale {
        MonitorFreshness::Stale
    } else if observation_state == HealthObservationState::None || terminal_failure {
        MonitorFreshness::Unknown
    } else {
        MonitorFreshness::Fresh
    };
    let mut coverage = empty_coverage();
    let mut conditions = Vec::new();
    for condition in condition_rows {
        let requirement = parse_requirement(
            &condition
                .try_get::<String, _>("requirement")
                .map_err(HealthError::Storage)?,
        );
        let stored_status = parse_condition_status(
            &condition
                .try_get::<String, _>("status")
                .map_err(HealthError::Storage)?,
        );
        let status = if stale {
            HealthConditionStatus::Stale
        } else if terminal_failure {
            HealthConditionStatus::Unknown
        } else {
            stored_status.clone()
        };
        increment_condition_coverage(&mut coverage, &requirement, &status);
        let evidence_json: String = condition
            .try_get("evidence_refs_json")
            .map_err(HealthError::Storage)?;
        conditions.push(HostHealthCondition {
            condition_key: condition
                .try_get("condition_key")
                .map_err(HealthError::Storage)?,
            condition_kind: condition
                .try_get("condition_kind")
                .map_err(HealthError::Storage)?,
            requirement,
            subject_kind: condition
                .try_get("subject_kind")
                .map_err(HealthError::Storage)?,
            subject_id: condition
                .try_get("subject_id")
                .map_err(HealthError::Storage)?,
            subject_label: condition
                .try_get("subject_label")
                .map_err(HealthError::Storage)?,
            status,
            candidate_status: parse_condition_status(
                &condition
                    .try_get::<String, _>("candidate_status")
                    .map_err(HealthError::Storage)?,
            ),
            reason_code: if stale {
                "stale".to_owned()
            } else if terminal_failure {
                "terminal_run_unknown".to_owned()
            } else {
                condition
                    .try_get("reason_code")
                    .map_err(HealthError::Storage)?
            },
            value: condition
                .try_get("value_real")
                .map_err(HealthError::Storage)?,
            unit: condition.try_get("unit").map_err(HealthError::Storage)?,
            window_seconds: condition
                .try_get("window_seconds")
                .map_err(HealthError::Storage)?,
            streak_count: non_negative_u32(
                condition
                    .try_get("streak_count")
                    .map_err(HealthError::Storage)?,
            ),
            streak_required: non_negative_u32(
                condition
                    .try_get("streak_required")
                    .map_err(HealthError::Storage)?,
            ),
            evidence_refs: serde_json::from_str(&evidence_json).unwrap_or_default(),
        });
    }
    let stored_health = parse_host_status(
        &row.try_get::<String, _>("status")
            .map_err(HealthError::Storage)?,
    );
    let health = if stale || observation_state == HealthObservationState::None || terminal_failure {
        HostHealthStatus::Unknown
    } else {
        stored_health
    };
    Ok(HostCurrentHealth {
        host_id: host_id.to_owned(),
        health,
        reason_code: if stale {
            "stale".to_owned()
        } else if terminal_failure {
            "terminal_run_unknown".to_owned()
        } else {
            row.try_get("reason_code").map_err(HealthError::Storage)?
        },
        freshness,
        observation_state,
        evaluation_id: Some(evaluation_id),
        run_id: Some(row.try_get("run_id").map_err(HealthError::Storage)?),
        trigger_kind: Some(parse_trigger(
            &row.try_get::<String, _>("trigger_kind")
                .map_err(HealthError::Storage)?,
        )),
        policy_id: row.try_get("policy_id").map_err(HealthError::Storage)?,
        policy_revision: row
            .try_get("policy_revision")
            .map_err(HealthError::Storage)?,
        evaluated_at: row.try_get("evaluated_at").map_err(HealthError::Storage)?,
        observed_at: row.try_get("observed_at").map_err(HealthError::Storage)?,
        valid_until: row.try_get("valid_until").map_err(HealthError::Storage)?,
        coverage,
        conditions,
    })
}

fn increment_condition_coverage(
    coverage: &mut HostHealthConditionCoverage,
    requirement: &HealthRequirement,
    status: &HealthConditionStatus,
) {
    match requirement {
        HealthRequirement::Required => {
            coverage.required_total = coverage.required_total.saturating_add(1);
            match status {
                HealthConditionStatus::Ok => {
                    coverage.required_ok = coverage.required_ok.saturating_add(1)
                }
                HealthConditionStatus::Warning => {
                    coverage.required_warning = coverage.required_warning.saturating_add(1)
                }
                HealthConditionStatus::Critical => {
                    coverage.required_critical = coverage.required_critical.saturating_add(1)
                }
                HealthConditionStatus::Unknown | HealthConditionStatus::Stale => {
                    coverage.required_unknown = coverage.required_unknown.saturating_add(1)
                }
            }
        }
        HealthRequirement::Optional => {
            coverage.optional_total = coverage.optional_total.saturating_add(1);
            match status {
                HealthConditionStatus::Ok => {
                    coverage.optional_ok = coverage.optional_ok.saturating_add(1)
                }
                HealthConditionStatus::Warning => {
                    coverage.optional_warning = coverage.optional_warning.saturating_add(1)
                }
                HealthConditionStatus::Critical => {
                    coverage.optional_critical = coverage.optional_critical.saturating_add(1)
                }
                HealthConditionStatus::Unknown | HealthConditionStatus::Stale => {
                    coverage.optional_unknown = coverage.optional_unknown.saturating_add(1)
                }
            }
        }
    }
}

fn window_spec(value: Option<&str>) -> Result<WindowSpec, HealthError> {
    match value.unwrap_or("24h") {
        "24h" => Ok(WindowSpec {
            name: HostHealthWindow::Hours24,
            resolution: HostHealthBandResolution::Hour,
            width_seconds: 3_600,
            bucket_count: 24,
        }),
        "7d" => Ok(WindowSpec {
            name: HostHealthWindow::Days7,
            resolution: HostHealthBandResolution::SixHour,
            width_seconds: 21_600,
            bucket_count: 28,
        }),
        "30d" => Ok(WindowSpec {
            name: HostHealthWindow::Days30,
            resolution: HostHealthBandResolution::Day,
            width_seconds: 86_400,
            bucket_count: 30,
        }),
        _ => Err(HealthError::BadRequest {
            code: "INVALID_HEALTH_WINDOW",
            message: "健康时间窗口只支持 24h、7d 或 30d",
            details: json!({"allowed": ["24h", "7d", "30d"]}),
        }),
    }
}

async fn health_window(
    pool: &SqlitePool,
    host_id: &str,
    spec: WindowSpec,
    evaluated_at: DateTime<Utc>,
) -> Result<HostHealthTimeWindow, HealthError> {
    let metadata = sqlx::query(
        "SELECT provenance_started_at, provenance_started_at_epoch_ms
         FROM monitor_schedule_provenance_metadata WHERE singleton_id = 1",
    )
    .fetch_one(pool)
    .await
    .map_err(HealthError::Storage)?;
    let provenance_started_at: String = metadata
        .try_get("provenance_started_at")
        .map_err(HealthError::Storage)?;
    let provenance_started_epoch: i64 = metadata
        .try_get("provenance_started_at_epoch_ms")
        .map_err(HealthError::Storage)?;
    let width_ms = spec.width_seconds.saturating_mul(1_000);
    let evaluated_epoch = evaluated_at.timestamp_millis();
    let end_epoch = if evaluated_epoch.rem_euclid(width_ms) == 0 {
        evaluated_epoch
    } else {
        evaluated_epoch
            .div_euclid(width_ms)
            .saturating_add(1)
            .saturating_mul(width_ms)
    };
    let from_epoch = end_epoch.saturating_sub(
        width_ms.saturating_mul(i64::try_from(spec.bucket_count).unwrap_or(i64::MAX)),
    );
    let grid_rows = sqlx::query(
        "SELECT schedule_id, due_from_at_epoch_ms, due_until_at_epoch_ms,
                interval_seconds
         FROM monitor_schedule_versions
         WHERE host_id = ? AND state = 'enabled' AND due_from_at_epoch_ms < ?
           AND (due_until_at_epoch_ms IS NULL OR due_until_at_epoch_ms > ?)
         ORDER BY due_from_at_epoch_ms, schedule_id, revision",
    )
    .bind(end_epoch)
    .bind(from_epoch)
    .fetch_all(pool)
    .await
    .map_err(HealthError::Storage)?;
    let mut grids = Vec::new();
    for row in grid_rows {
        grids.push(ScheduleGrid {
            schedule_id: row.try_get("schedule_id").map_err(HealthError::Storage)?,
            due_from_epoch_ms: row
                .try_get("due_from_at_epoch_ms")
                .map_err(HealthError::Storage)?,
            due_until_epoch_ms: row
                .try_get("due_until_at_epoch_ms")
                .map_err(HealthError::Storage)?,
            interval_ms: row
                .try_get::<i64, _>("interval_seconds")
                .map_err(HealthError::Storage)?
                .saturating_mul(1_000),
        });
    }
    let evaluation_rows = sqlx::query(
        "SELECT e.status, e.reason_code, e.policy_revision, e.evaluated_at_epoch_ms,
                r.schedule_id, r.schedule_version_id, r.scheduled_for,
                r.trigger_kind, r.missed_due_count
         FROM health_evaluations e
         JOIN monitor_runs r ON r.run_id = e.run_id
         WHERE e.host_id = ? AND r.trigger_kind IN ('scheduled', 'catch_up')
           AND e.evaluated_at_epoch_ms >= ? AND e.evaluated_at_epoch_ms < ?
         ORDER BY e.evaluated_at_epoch_ms, e.evaluation_id",
    )
    .bind(host_id)
    .bind(from_epoch)
    .bind(end_epoch)
    .fetch_all(pool)
    .await
    .map_err(HealthError::Storage)?;
    let mut evaluations = Vec::new();
    for row in evaluation_rows {
        let trigger: String = row.try_get("trigger_kind").map_err(HealthError::Storage)?;
        let missed: i64 = row
            .try_get::<Option<i64>, _>("missed_due_count")
            .map_err(HealthError::Storage)?
            .unwrap_or(0);
        evaluations.push(BandEvaluation {
            status: parse_host_status(
                &row.try_get::<String, _>("status")
                    .map_err(HealthError::Storage)?,
            ),
            reason_code: row.try_get("reason_code").map_err(HealthError::Storage)?,
            policy_revision: row
                .try_get("policy_revision")
                .map_err(HealthError::Storage)?,
            schedule_id: row.try_get("schedule_id").map_err(HealthError::Storage)?,
            schedule_version_id: row
                .try_get("schedule_version_id")
                .map_err(HealthError::Storage)?,
            scheduled_for_epoch_ms: row
                .try_get::<Option<String>, _>("scheduled_for")
                .map_err(HealthError::Storage)?
                .as_deref()
                .and_then(parse_timestamp)
                .map(|value| value.timestamp_millis()),
            evaluated_at_epoch_ms: row
                .try_get("evaluated_at_epoch_ms")
                .map_err(HealthError::Storage)?,
            late: trigger == "catch_up" || missed > 0,
        });
    }

    let mut buckets = Vec::with_capacity(spec.bucket_count);
    for index in 0..spec.bucket_count {
        let bucket_start = from_epoch
            .saturating_add(width_ms.saturating_mul(i64::try_from(index).unwrap_or(i64::MAX)));
        let bucket_end = bucket_start.saturating_add(width_ms);
        let effective_end = bucket_end.min(evaluated_epoch);
        let mut provenance_complete = bucket_start >= provenance_started_epoch;
        let expected_slots = if provenance_complete {
            expected_slots(&grids, bucket_start, effective_end)
        } else {
            BTreeSet::new()
        };
        let mut slot_evaluations: HashMap<(String, i64), &BandEvaluation> = HashMap::new();
        let mut late_observation_count = 0u32;
        for evaluation in &evaluations {
            if evaluation.late {
                if evaluation.evaluated_at_epoch_ms >= bucket_start
                    && evaluation.evaluated_at_epoch_ms < effective_end
                {
                    late_observation_count = late_observation_count.saturating_add(1);
                }
                continue;
            }
            let Some(slot) = evaluation.scheduled_for_epoch_ms else {
                continue;
            };
            if slot < bucket_start || slot >= effective_end {
                continue;
            }
            let Some(schedule_id) = evaluation.schedule_id.as_ref() else {
                provenance_complete = false;
                continue;
            };
            if evaluation.schedule_version_id.is_none() {
                provenance_complete = false;
            }
            slot_evaluations.insert((schedule_id.clone(), slot), evaluation);
        }
        if provenance_complete
            && slot_evaluations
                .keys()
                .any(|key| !expected_slots.contains(key))
        {
            provenance_complete = false;
        }

        let mut ok_count = 0u32;
        let mut warning_count = 0u32;
        let mut critical_count = 0u32;
        let mut explicit_unknown_count = 0u32;
        let mut evaluated_count = 0u32;
        let mut policy_revisions = BTreeSet::new();
        let mut unknown_reasons = BTreeMap::new();
        if provenance_complete {
            for slot in &expected_slots {
                if let Some(evaluation) = slot_evaluations.get(slot) {
                    evaluated_count = evaluated_count.saturating_add(1);
                    if let Some(revision) = evaluation.policy_revision {
                        policy_revisions.insert(revision);
                    }
                    increment_band_status(
                        &evaluation.status,
                        &mut ok_count,
                        &mut warning_count,
                        &mut critical_count,
                        &mut explicit_unknown_count,
                    );
                    if evaluation.status == HostHealthStatus::Unknown {
                        increment_reason(&mut unknown_reasons, &evaluation.reason_code);
                    }
                }
            }
        } else {
            for evaluation in slot_evaluations.values() {
                evaluated_count = evaluated_count.saturating_add(1);
                if let Some(revision) = evaluation.policy_revision {
                    policy_revisions.insert(revision);
                }
                increment_band_status(
                    &evaluation.status,
                    &mut ok_count,
                    &mut warning_count,
                    &mut critical_count,
                    &mut explicit_unknown_count,
                );
                if evaluation.status == HostHealthStatus::Unknown {
                    increment_reason(&mut unknown_reasons, &evaluation.reason_code);
                }
            }
        }
        if late_observation_count > 0 {
            unknown_reasons.insert(
                "late_observation_excluded".to_owned(),
                late_observation_count,
            );
        }
        let expected_count =
            provenance_complete.then(|| u32::try_from(expected_slots.len()).unwrap_or(u32::MAX));
        let gap_count = expected_count.map(|expected| expected.saturating_sub(evaluated_count));
        if let Some(gaps) = gap_count.filter(|value| *value > 0) {
            unknown_reasons.insert("missing_evaluation".to_owned(), gaps);
        }
        let unknown_count = explicit_unknown_count.saturating_add(gap_count.unwrap_or(0));
        let observed_count = ok_count
            .saturating_add(warning_count)
            .saturating_add(critical_count);
        let coverage = expected_count.and_then(|expected| {
            (expected > 0).then_some(f64::from(observed_count) / f64::from(expected))
        });
        let worst_status = if critical_count > 0 {
            Some(HealthConditionStatus::Critical)
        } else if unknown_count > 0 {
            Some(HealthConditionStatus::Unknown)
        } else if warning_count > 0 {
            Some(HealthConditionStatus::Warning)
        } else if ok_count > 0 {
            Some(HealthConditionStatus::Ok)
        } else {
            None
        };
        let reason_code = if !provenance_complete {
            "provenance_incomplete"
        } else if expected_count == Some(0) {
            "no_schedule"
        } else if gap_count.is_some_and(|value| value > 0) {
            "coverage_gap"
        } else if explicit_unknown_count > 0 {
            "unknown_evaluation"
        } else if critical_count > 0 {
            "critical"
        } else if warning_count > 0 {
            "warning"
        } else {
            "ok"
        };
        buckets.push(HostHealthTimeBucket {
            bucket_start: epoch_timestamp(bucket_start)?,
            bucket_end: epoch_timestamp(bucket_end)?,
            effective_end: epoch_timestamp(effective_end)?,
            bucket_width_seconds: u32::try_from(spec.width_seconds).unwrap_or(u32::MAX),
            is_partial: effective_end < bucket_end,
            count_basis: "scope_evaluations".to_owned(),
            ok_count,
            warning_count,
            critical_count,
            unknown_count,
            expected_count,
            evaluated_count,
            observed_count,
            gap_count,
            late_observation_count,
            coverage,
            worst_status,
            reason_code: reason_code.to_owned(),
            unknown_reason_counts: unknown_reasons,
            provenance_complete,
            policy_revisions: policy_revisions.into_iter().collect(),
        });
    }
    let provenance_complete = buckets.iter().all(|bucket| bucket.provenance_complete);
    Ok(HostHealthTimeWindow {
        name: spec.name,
        actual_resolution: spec.resolution,
        from: epoch_timestamp(from_epoch)?,
        to: epoch_timestamp(end_epoch)?,
        bucket_width_seconds: u32::try_from(spec.width_seconds).unwrap_or(u32::MAX),
        bucket_count: u32::try_from(spec.bucket_count).unwrap_or(u32::MAX),
        count_basis: "scope_evaluations".to_owned(),
        provenance_started_at,
        provenance_complete,
        buckets,
    })
}

fn expected_slots(
    grids: &[ScheduleGrid],
    bucket_start: i64,
    effective_end: i64,
) -> BTreeSet<(String, i64)> {
    let mut slots = BTreeSet::new();
    if effective_end <= bucket_start {
        return slots;
    }
    for grid in grids {
        if grid.interval_ms <= 0 {
            continue;
        }
        let range_start = bucket_start.max(grid.due_from_epoch_ms);
        let range_end = effective_end.min(grid.due_until_epoch_ms.unwrap_or(effective_end));
        if range_end <= range_start {
            continue;
        }
        let delta = range_start.saturating_sub(grid.due_from_epoch_ms);
        let steps = if delta <= 0 {
            0
        } else {
            delta
                .saturating_add(grid.interval_ms.saturating_sub(1))
                .div_euclid(grid.interval_ms)
        };
        let mut slot = grid
            .due_from_epoch_ms
            .saturating_add(steps.saturating_mul(grid.interval_ms));
        while slot < range_end {
            slots.insert((grid.schedule_id.clone(), slot));
            let next = slot.saturating_add(grid.interval_ms);
            if next <= slot {
                break;
            }
            slot = next;
        }
    }
    slots
}

fn increment_band_status(
    status: &HostHealthStatus,
    ok: &mut u32,
    warning: &mut u32,
    critical: &mut u32,
    unknown: &mut u32,
) {
    match status {
        HostHealthStatus::Healthy => *ok = ok.saturating_add(1),
        HostHealthStatus::Degraded => *warning = warning.saturating_add(1),
        HostHealthStatus::Unhealthy => *critical = critical.saturating_add(1),
        HostHealthStatus::Unknown => *unknown = unknown.saturating_add(1),
    }
}

fn increment_reason(reasons: &mut BTreeMap<String, u32>, reason: &str) {
    let count = reasons.entry(reason.to_owned()).or_insert(0);
    *count = count.saturating_add(1);
}

fn requirement_name(value: &HealthRequirement) -> &'static str {
    match value {
        HealthRequirement::Required => "required",
        HealthRequirement::Optional => "optional",
    }
}

fn parse_requirement(value: &str) -> HealthRequirement {
    if value == "optional" {
        HealthRequirement::Optional
    } else {
        HealthRequirement::Required
    }
}

fn condition_status_name(value: &HealthConditionStatus) -> &'static str {
    match value {
        HealthConditionStatus::Ok => "ok",
        HealthConditionStatus::Warning => "warning",
        HealthConditionStatus::Critical => "critical",
        HealthConditionStatus::Unknown | HealthConditionStatus::Stale => "unknown",
    }
}

fn parse_condition_status(value: &str) -> HealthConditionStatus {
    match value {
        "ok" => HealthConditionStatus::Ok,
        "warning" => HealthConditionStatus::Warning,
        "critical" => HealthConditionStatus::Critical,
        "stale" => HealthConditionStatus::Stale,
        _ => HealthConditionStatus::Unknown,
    }
}

fn host_status_name(value: &HostHealthStatus) -> &'static str {
    match value {
        HostHealthStatus::Healthy => "healthy",
        HostHealthStatus::Degraded => "degraded",
        HostHealthStatus::Unhealthy => "unhealthy",
        HostHealthStatus::Unknown => "unknown",
    }
}

fn parse_host_status(value: &str) -> HostHealthStatus {
    match value {
        "healthy" => HostHealthStatus::Healthy,
        "degraded" => HostHealthStatus::Degraded,
        "unhealthy" => HostHealthStatus::Unhealthy,
        _ => HostHealthStatus::Unknown,
    }
}

fn observation_state_name(value: &HealthObservationState) -> &'static str {
    match value {
        HealthObservationState::Complete => "complete",
        HealthObservationState::Partial => "partial",
        HealthObservationState::None => "none",
    }
}

fn parse_observation_state(value: &str) -> HealthObservationState {
    match value {
        "complete" => HealthObservationState::Complete,
        "partial" => HealthObservationState::Partial,
        _ => HealthObservationState::None,
    }
}

fn parse_trigger(value: &str) -> MonitorRunTrigger {
    match value {
        "scheduled" => MonitorRunTrigger::Scheduled,
        "catch_up" => MonitorRunTrigger::CatchUp,
        _ => MonitorRunTrigger::Manual,
    }
}

fn digest_text(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn epoch_timestamp(epoch_ms: i64) -> Result<String, HealthError> {
    Utc.timestamp_millis_opt(epoch_ms)
        .single()
        .map(timestamp_ms)
        .ok_or(HealthError::Internal)
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn timestamp_ms(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn non_negative_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_typed_thresholds_and_rejects_period_average_ambiguity() {
        let mut request = HostHealthPolicyPutRequest {
            enabled: true,
            cpu_busy: Some(CpuBusyHealthRule {
                requirement: HealthRequirement::Required,
                series_kind: CpuBusySeriesKind::CollectorWindow,
                minimum_window_seconds: 0.5,
                warning_at_or_above: 80.0,
                critical_at_or_above: 95.0,
                recovery_below: 75.0,
                enter_count: 2,
                recover_count: 2,
            }),
            memory_available_ratio: None,
            normalized_load5: None,
            filesystems: Vec::new(),
        };
        assert!(normalize_and_validate_policy(&mut request).is_ok());
        request.cpu_busy.as_mut().unwrap().recovery_below = 80.0;
        assert!(normalize_and_validate_policy(&mut request).is_err());
    }

    #[test]
    fn expected_slots_use_half_open_due_intervals() {
        let grids = vec![ScheduleGrid {
            schedule_id: "schedule-a".to_owned(),
            due_from_epoch_ms: 0,
            due_until_epoch_ms: Some(10_000),
            interval_ms: 5_000,
        }];
        assert_eq!(
            expected_slots(&grids, 0, 10_000),
            BTreeSet::from([
                ("schedule-a".to_owned(), 0),
                ("schedule-a".to_owned(), 5_000),
            ])
        );
        assert_eq!(
            expected_slots(&grids, 5_000, 10_000),
            BTreeSet::from([("schedule-a".to_owned(), 5_000)])
        );
    }

    #[test]
    fn bucket_worst_priority_is_critical_unknown_warning_ok() {
        let mut ok = 0;
        let mut warning = 0;
        let mut critical = 0;
        let mut unknown = 0;
        for status in [
            HostHealthStatus::Healthy,
            HostHealthStatus::Degraded,
            HostHealthStatus::Unknown,
            HostHealthStatus::Unhealthy,
        ] {
            increment_band_status(&status, &mut ok, &mut warning, &mut critical, &mut unknown);
        }
        assert_eq!((ok, warning, critical, unknown), (1, 1, 1, 1));
    }
}
