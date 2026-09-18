use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration as StdDuration,
};

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use tokio::{
    sync::{RwLock, watch},
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, DataSourceDescriptor, DataSourceKind,
        DataSourceStatus, Freshness, HostMonitorProfile, MonitorRunAccepted,
        MonitorRunAcceptedResponse, MonitorRunState, MonitorRunTrigger,
        MonitorScheduleCreateRequest, MonitorScheduleListData, MonitorScheduleListResponse,
        MonitorScheduleRecord, MonitorScheduleResponse, MonitorScheduleState,
        MonitorScheduleUpdateRequest, MonitoringSchedulerLimits, MonitoringSchedulerState,
        MonitoringSchedulerStateReason, MonitoringSchedulerStatus,
        MonitoringSchedulerStatusResponse,
    },
    events::{self, ChangeEventKind},
    monitoring_api, monitoring_health, monitoring_rollup,
};

pub const MIN_INTERVAL_SECONDS: u32 = 300;
pub const MAX_INTERVAL_SECONDS: u32 = 86_400;
pub const MAX_STALE_AFTER_SECONDS: u32 = 604_800;
const MAX_IDEMPOTENCY_KEY: usize = 128;
const MAX_REQUEST_ID: usize = 128;
const DEFAULT_MAX_CONCURRENCY: u32 = 1;
const DEFAULT_TICK_SECONDS: u32 = 5;
const MAX_DUE_PER_TICK: i64 = 64;
const MIN_LATE_AFTER_SECONDS: u32 = 15;

#[derive(Debug, Clone, Copy)]
pub struct SchedulerSettings {
    pub max_concurrency: u32,
    pub tick_seconds: u32,
}

impl SchedulerSettings {
    pub fn from_environment() -> Self {
        Self {
            max_concurrency: env_u32(
                "NETWORK_ATLAS_MONITOR_MAX_CONCURRENCY",
                DEFAULT_MAX_CONCURRENCY,
                1,
                32,
            ),
            tick_seconds: env_u32(
                "NETWORK_ATLAS_MONITOR_TICK_SECONDS",
                DEFAULT_TICK_SECONDS,
                1,
                60,
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct SchedulerTickErrorRecord {
    code: String,
    occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
struct SchedulerTickHealth {
    last_successful_at: Option<DateTime<Utc>>,
    last_error: Option<SchedulerTickErrorRecord>,
    consecutive_failures: u32,
}

#[derive(Debug)]
pub struct MonitoringSchedulerRuntime {
    started: AtomicBool,
    tick_health: RwLock<SchedulerTickHealth>,
    settings: SchedulerSettings,
    instance_id: String,
}

impl MonitoringSchedulerRuntime {
    pub fn new(settings: SchedulerSettings) -> Self {
        Self {
            started: AtomicBool::new(false),
            tick_health: RwLock::new(SchedulerTickHealth::default()),
            settings,
            instance_id: Uuid::new_v4().to_string(),
        }
    }

    pub fn settings(&self) -> SchedulerSettings {
        self.settings
    }

    async fn record_tick_success(&self, completed_at: DateTime<Utc>) {
        let mut health = self.tick_health.write().await;
        health.last_successful_at = Some(completed_at);
        health.consecutive_failures = 0;
    }

    async fn record_tick_failure(&self, occurred_at: DateTime<Utc>, code: &str) {
        let mut health = self.tick_health.write().await;
        health.last_error = Some(SchedulerTickErrorRecord {
            code: code.to_owned(),
            occurred_at,
        });
        health.consecutive_failures = health.consecutive_failures.saturating_add(1);
    }
}

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("invalid monitoring schedule")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("monitoring schedule was not found")]
    NotFound { resource: &'static str, id: String },
    #[error("monitoring schedule conflicts with current state")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("If-Match is required")]
    PreconditionRequired,
    #[error("schedule revision does not match")]
    PreconditionFailed { expected_revision: i64 },
    #[error("storage unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("internal scheduler error")]
    Internal,
}

impl IntoResponse for SchedulerError {
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
                "请求的监控调度对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            Self::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "IF_MATCH_REQUIRED",
                "更新调度需要 If-Match 修订号",
                json!({}),
            ),
            Self::PreconditionFailed { expected_revision } => (
                StatusCode::PRECONDITION_FAILED,
                "REVISION_MISMATCH",
                "调度已被其他请求更新，请刷新后重试",
                json!({"expected_revision": expected_revision}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "monitor scheduler storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地监控调度存储暂不可用",
                    json!({}),
                )
            }
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "监控调度内部处理失败",
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

#[utoipa::path(
    get,
    path = "/api/v1/monitoring/status",
    tag = "monitoring",
    responses((status = 200, body = MonitoringSchedulerStatusResponse))
)]
pub async fn get_scheduler_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MonitoringSchedulerStatusResponse>, SchedulerError> {
    let evaluated_at = Utc::now();
    let evaluated_at_text = timestamp(evaluated_at);
    let active_schedule_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitor_schedules WHERE state = 'enabled'")
            .fetch_one(&state.pool)
            .await
            .map_err(SchedulerError::Storage)?;
    let due_schedule_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monitor_schedules
         WHERE state = 'enabled' AND next_due_at <= ?",
    )
    .bind(&evaluated_at_text)
    .fetch_one(&state.pool)
    .await
    .map_err(SchedulerError::Storage)?;
    let oldest_due_at: Option<String> = sqlx::query_scalar(
        "SELECT MIN(next_due_at) FROM monitor_schedules
         WHERE state = 'enabled' AND next_due_at <= ?",
    )
    .bind(&evaluated_at_text)
    .fetch_one(&state.pool)
    .await
    .map_err(SchedulerError::Storage)?;
    let queued_run_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs WHERE state = 'queued'")
            .fetch_one(&state.pool)
            .await
            .map_err(SchedulerError::Storage)?;
    let running_run_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs WHERE state = 'running'")
            .fetch_one(&state.pool)
            .await
            .map_err(SchedulerError::Storage)?;
    let settings = state.monitoring_scheduler.settings();
    let history_maintenance = monitoring_rollup::maintenance_status(&state)
        .await
        .map_err(|error| match error {
            monitoring_rollup::RollupError::Storage(error) => SchedulerError::Storage(error),
            _ => SchedulerError::Internal,
        })?;
    let late_after_seconds = scheduler_late_after_seconds(settings);
    let tick_health = state.monitoring_scheduler.tick_health.read().await.clone();
    let scheduler_started = state.monitoring_scheduler.started.load(Ordering::Acquire);
    let (scheduler_state, state_reason) = derive_scheduler_state(
        scheduler_started,
        evaluated_at,
        &tick_health,
        late_after_seconds,
    );
    Ok(Json(MonitoringSchedulerStatusResponse {
        data: MonitoringSchedulerStatus {
            state: scheduler_state,
            state_reason,
            active_schedule_count: count_u32(active_schedule_count),
            due_schedule_count: count_u32(due_schedule_count),
            queued_run_count: count_u32(queued_run_count),
            running_run_count: count_u32(running_run_count),
            oldest_due_at,
            last_tick_at: tick_health.last_successful_at.map(timestamp),
            last_tick_error_at: tick_health
                .last_error
                .as_ref()
                .map(|error| timestamp(error.occurred_at)),
            last_tick_error_code: tick_health.last_error.map(|error| error.code),
            consecutive_tick_failures: tick_health.consecutive_failures,
            limits: MonitoringSchedulerLimits {
                minimum_interval_seconds: MIN_INTERVAL_SECONDS,
                maximum_interval_seconds: MAX_INTERVAL_SECONDS,
                maximum_stale_after_seconds: MAX_STALE_AFTER_SECONDS,
                maximum_concurrency: settings.max_concurrency,
                tick_seconds: settings.tick_seconds,
                late_after_seconds,
            },
            history_maintenance,
        },
        meta: real_meta(request_id(&headers), 1),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/monitor-schedules",
    tag = "monitoring",
    params(("host_id" = String, Path, description = "Registered Linux host identifier")),
    responses(
        (status = 200, body = MonitorScheduleListResponse),
        (status = 404, body = ApiErrorResponse)
    )
)]
pub async fn list_monitor_schedules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<Json<MonitorScheduleListResponse>, SchedulerError> {
    ensure_host_exists(&state.pool, &host_id).await?;
    let rows = sqlx::query(
        "SELECT schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                last_due_at, revision, created_at, updated_at
         FROM monitor_schedules WHERE host_id = ? AND state != 'archived'
         ORDER BY created_at, schedule_id",
    )
    .bind(&host_id)
    .fetch_all(&state.pool)
    .await
    .map_err(SchedulerError::Storage)?;
    let schedules = rows
        .into_iter()
        .map(schedule_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(MonitorScheduleListResponse {
        data: MonitorScheduleListData { host_id, schedules },
        meta: real_meta(request_id(&headers), 1),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host_id}/monitor-schedules",
    tag = "monitoring",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe retries")
    ),
    request_body = MonitorScheduleCreateRequest,
    responses(
        (status = 201, body = MonitorScheduleResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 409, body = ApiErrorResponse)
    )
)]
pub async fn create_monitor_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    payload: Result<Json<MonitorScheduleCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MonitorScheduleResponse>), SchedulerError> {
    let Json(request) = payload.map_err(|_| invalid_json())?;
    validate_schedule_values(
        request.interval_seconds,
        request.jitter_seconds,
        request.stale_after_seconds,
    )?;
    let observation_admission = state.observation_admission.read().await;
    ensure_host_exists(&state.pool, &host_id).await?;
    let key = idempotency_key(&headers)?;
    let request_sha256 = digest_json(&request)?;
    if let Some(response) = replay_create(&state.pool, &host_id, &key, &request_sha256).await? {
        return Ok((StatusCode::CREATED, Json(response)));
    }

    let schedule_id = Uuid::new_v4().to_string();
    let revision = 1;
    let jitter_offset_seconds = stable_jitter(&schedule_id, revision, request.jitter_seconds);
    // Use one wall-clock instant for the schedule revision, its first due slot
    // and the provenance row.  Deriving these values from separate `Utc::now`
    // calls can otherwise put a due point on the wrong side of a revision's
    // half-open boundary when a PATCH races the scheduler.
    let created_at_instant = Utc::now();
    let created_at = timestamp(created_at_instant);
    let next_due_at = request.enabled.then(|| {
        timestamp(
            created_at_instant
                + Duration::seconds(i64::from(request.interval_seconds))
                + Duration::seconds(i64::from(jitter_offset_seconds)),
        )
    });
    let record = MonitorScheduleRecord {
        schedule_id: schedule_id.clone(),
        host_id: host_id.clone(),
        profile: request.profile,
        interval_seconds: request.interval_seconds,
        jitter_seconds: request.jitter_seconds,
        jitter_offset_seconds,
        stale_after_seconds: request.stale_after_seconds,
        state: if request.enabled {
            MonitorScheduleState::Enabled
        } else {
            MonitorScheduleState::Paused
        },
        next_due_at,
        last_due_at: None,
        revision,
        created_at: created_at.clone(),
        updated_at: created_at.clone(),
    };
    let response = MonitorScheduleResponse {
        data: record.clone(),
        meta: real_meta(request_id(&headers), revision),
    };
    let response_json = serde_json::to_string(&response).map_err(|_| SchedulerError::Internal)?;
    let mut tx = state.pool.begin().await.map_err(SchedulerError::Storage)?;
    let insert = sqlx::query(
        "INSERT INTO monitor_schedules(
            schedule_id, host_id, profile, interval_seconds, jitter_seconds,
            jitter_offset_seconds, stale_after_seconds, state, next_due_at,
            revision, create_idempotency_key, create_request_sha256,
            created_response_json, created_at, updated_at
         ) VALUES (?, ?, 'host_resource_v1', ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?)",
    )
    .bind(&record.schedule_id)
    .bind(&record.host_id)
    .bind(i64::from(record.interval_seconds))
    .bind(i64::from(record.jitter_seconds))
    .bind(i64::from(record.jitter_offset_seconds))
    .bind(i64::from(record.stale_after_seconds))
    .bind(schedule_state_name(&record.state))
    .bind(&record.next_due_at)
    .bind(&key)
    .bind(&request_sha256)
    .bind(&response_json)
    .bind(&record.created_at)
    .bind(&record.updated_at)
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert {
        tx.rollback().await.map_err(SchedulerError::Storage)?;
        let lower = error.to_string().to_ascii_lowercase();
        if lower.contains("unique") {
            if let Some(response) =
                replay_create(&state.pool, &host_id, &key, &request_sha256).await?
            {
                return Ok((StatusCode::CREATED, Json(response)));
            }
            return Err(SchedulerError::Conflict {
                code: "MONITOR_SCHEDULE_EXISTS",
                message: "该 HOST 已有一个有效的资源监控调度",
                details: json!({"host_id": host_id, "profile": "host_resource_v1"}),
            });
        }
        return Err(SchedulerError::Storage(error));
    }
    insert_schedule_version(
        &mut tx,
        &record,
        &record.created_at,
        &record.created_at,
        "recorded",
        None,
    )
    .await
    .map_err(SchedulerError::Storage)?;
    tx.commit().await.map_err(SchedulerError::Storage)?;
    publish_schedule_state(&state.pool, &record).await;
    drop(observation_admission);
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/monitor-schedules/{schedule_id}",
    tag = "monitoring",
    params(
        ("schedule_id" = String, Path, description = "Persistent monitor schedule identifier"),
        ("If-Match" = String, Header, description = "Current revision-N ETag")
    ),
    request_body = MonitorScheduleUpdateRequest,
    responses(
        (status = 200, body = MonitorScheduleResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 409, body = ApiErrorResponse),
        (status = 412, body = ApiErrorResponse),
        (status = 428, body = ApiErrorResponse)
    )
)]
pub async fn update_monitor_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(schedule_id): Path<String>,
    payload: Result<Json<MonitorScheduleUpdateRequest>, JsonRejection>,
) -> Result<Json<MonitorScheduleResponse>, SchedulerError> {
    let expected_revision = if_match_revision(&headers)?;
    let Json(request) = payload.map_err(|_| invalid_json())?;
    if request.interval_seconds.is_none()
        && request.jitter_seconds.is_none()
        && request.stale_after_seconds.is_none()
        && request.state.is_none()
    {
        return Err(SchedulerError::BadRequest {
            code: "EMPTY_PATCH",
            message: "调度更新至少需要一个字段",
            details: json!({}),
        });
    }
    let observation_admission = state.observation_admission.read().await;
    let current = load_schedule(&state.pool, &schedule_id)
        .await?
        .ok_or_else(|| SchedulerError::NotFound {
            resource: "monitor_schedule",
            id: schedule_id.clone(),
        })?;
    if current.revision != expected_revision {
        return Err(SchedulerError::PreconditionFailed {
            expected_revision: current.revision,
        });
    }
    if current.state == MonitorScheduleState::Archived {
        return Err(SchedulerError::Conflict {
            code: "MONITOR_SCHEDULE_ARCHIVED",
            message: "已归档的调度不可再次修改",
            details: json!({"schedule_id": schedule_id}),
        });
    }

    let interval_seconds = request.interval_seconds.unwrap_or(current.interval_seconds);
    let jitter_seconds = request.jitter_seconds.unwrap_or(current.jitter_seconds);
    let stale_after_seconds = request
        .stale_after_seconds
        .unwrap_or(current.stale_after_seconds);
    validate_schedule_values(interval_seconds, jitter_seconds, stale_after_seconds)?;
    let target_state = request.state.unwrap_or_else(|| current.state.clone());
    let revision = current.revision + 1;
    // A stale-after-only revision must retain the pending due slot.  Changing
    // the interval, jitter, or lifecycle starts a new grid anchored at the
    // update instant instead.
    let grid_changed = interval_seconds != current.interval_seconds
        || jitter_seconds != current.jitter_seconds
        || target_state != current.state;
    let mut tx = state.pool.begin().await.map_err(SchedulerError::Storage)?;
    // Upgrade the deferred SQLite transaction to a writer before reading the
    // old row. This serializes the PATCH with a scheduler lease advance, so
    // the due boundary below uses the actual pre-update slot.
    let lock = sqlx::query(
        "UPDATE monitor_schedules
         SET updated_at = updated_at
         WHERE schedule_id = ? AND revision = ?",
    )
    .bind(&schedule_id)
    .bind(expected_revision)
    .execute(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    if lock.rows_affected() != 1 {
        tx.rollback().await.map_err(SchedulerError::Storage)?;
        let latest = load_schedule(&state.pool, &schedule_id).await?;
        return Err(SchedulerError::PreconditionFailed {
            expected_revision: latest.map_or(expected_revision, |value| value.revision),
        });
    }
    let before = load_schedule_tx(&mut tx, &schedule_id)
        .await?
        .ok_or(SchedulerError::Internal)?;
    // Capture the revision's effective instant only after the writer lock is
    // held.  If the PATCH waited behind a scheduler claim, provenance and the
    // newly anchored grid must reflect the actual linearization point.
    let changed_at_instant = Utc::now();
    let jitter_offset_seconds = if grid_changed {
        stable_jitter(&schedule_id, revision, jitter_seconds)
    } else {
        before.jitter_offset_seconds
    };
    let next_due_at = match target_state {
        MonitorScheduleState::Enabled if grid_changed => Some(timestamp(
            changed_at_instant
                + Duration::seconds(i64::from(interval_seconds))
                + Duration::seconds(i64::from(jitter_offset_seconds)),
        )),
        MonitorScheduleState::Enabled => before.next_due_at.clone(),
        MonitorScheduleState::Paused | MonitorScheduleState::Archived => None,
    };
    let updated_at = timestamp(changed_at_instant);
    let rows_affected = persist_schedule_update(
        &mut tx,
        &schedule_id,
        interval_seconds,
        jitter_seconds,
        jitter_offset_seconds,
        stale_after_seconds,
        &target_state,
        grid_changed,
        next_due_at.as_deref(),
        revision,
        &updated_at,
        expected_revision,
    )
    .await?;
    if rows_affected != 1 {
        tx.rollback().await.map_err(SchedulerError::Storage)?;
        let latest = load_schedule(&state.pool, &schedule_id).await?;
        return Err(SchedulerError::PreconditionFailed {
            expected_revision: latest.map_or(expected_revision, |value| value.revision),
        });
    }
    // The CAS update may have waited behind a scheduler tick that advanced the
    // pending due slot.  Read back the row through this same transaction so
    // the provenance revision describes the value that was actually stored,
    // not the stale snapshot read before the PATCH.
    let persisted = load_schedule_tx(&mut tx, &schedule_id)
        .await?
        .ok_or(SchedulerError::Internal)?;
    let old_due_until_at = if grid_changed {
        // A future first due point produces an empty old interval when timing
        // or lifecycle changes before that point.  Represent it as [due,due]
        // because the schema intentionally forbids a due_until before
        // due_from; an overdue point still belongs to the old revision until
        // the PATCH instant.
        let old_due_is_future = before
            .next_due_at
            .as_deref()
            .and_then(parse_timestamp)
            .is_some_and(|due| due > changed_at_instant);
        if old_due_is_future {
            before.next_due_at.as_deref().unwrap_or(updated_at.as_str())
        } else {
            updated_at.as_str()
        }
    } else {
        // The pending slot belongs to the new revision even when it is
        // already overdue.  Using the PATCH instant here would make that slot
        // appear in both revisions' half-open due ranges.
        persisted
            .next_due_at
            .as_deref()
            .unwrap_or(updated_at.as_str())
    };
    close_schedule_version(&mut tx, &before, &updated_at, old_due_until_at)
        .await
        .map_err(SchedulerError::Storage)?;
    insert_schedule_version(
        &mut tx,
        &persisted,
        &updated_at,
        &updated_at,
        "recorded",
        Some(&before.state),
    )
    .await
    .map_err(SchedulerError::Storage)?;
    tx.commit().await.map_err(SchedulerError::Storage)?;
    let updated = load_schedule(&state.pool, &schedule_id)
        .await?
        .ok_or(SchedulerError::Internal)?;
    publish_schedule_state(&state.pool, &updated).await;
    drop(observation_admission);
    Ok(Json(MonitorScheduleResponse {
        meta: real_meta(request_id(&headers), revision),
        data: updated,
    }))
}

async fn publish_schedule_state(pool: &SqlitePool, schedule: &MonitorScheduleRecord) {
    if let Err(error) = events::publish(
        pool,
        ChangeEventKind::MonitorScheduleChanged,
        &format!("monitor-schedule:{}", schedule.schedule_id),
        schedule.revision,
        json!({
            "host_id": schedule.host_id,
            "profile": "host_resource_v1",
            "state": schedule_state_name(&schedule.state),
        }),
    )
    .await
    {
        tracing::warn!(
            schedule_id = %schedule.schedule_id,
            error = %error,
            "could not publish monitor schedule state"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_schedule_update(
    tx: &mut Transaction<'_, Sqlite>,
    schedule_id: &str,
    interval_seconds: u32,
    jitter_seconds: u32,
    jitter_offset_seconds: u32,
    stale_after_seconds: u32,
    target_state: &MonitorScheduleState,
    timing_changed: bool,
    next_due_at: Option<&str>,
    revision: i64,
    updated_at: &str,
    expected_revision: i64,
) -> Result<u64, SchedulerError> {
    sqlx::query(
        "UPDATE monitor_schedules
         SET interval_seconds = ?, jitter_seconds = ?, jitter_offset_seconds = ?,
             stale_after_seconds = ?, state = ?,
             next_due_at = CASE WHEN ? THEN ? ELSE next_due_at END,
             revision = ?,
             lease_owner = NULL, lease_token = NULL, lease_until = NULL, updated_at = ?
         WHERE schedule_id = ? AND revision = ?",
    )
    .bind(i64::from(interval_seconds))
    .bind(i64::from(jitter_seconds))
    .bind(i64::from(jitter_offset_seconds))
    .bind(i64::from(stale_after_seconds))
    .bind(schedule_state_name(target_state))
    .bind(timing_changed)
    .bind(next_due_at)
    .bind(revision)
    .bind(updated_at)
    .bind(schedule_id)
    .bind(expected_revision)
    .execute(&mut **tx)
    .await
    .map(|result| result.rows_affected())
    .map_err(SchedulerError::Storage)
}

/// Persist the immutable provenance row for a schedule revision.  The caller
/// must insert this in the same transaction as the schedule row so a revision
/// can never become visible without a matching due-grid description.
async fn insert_schedule_version(
    tx: &mut Transaction<'_, Sqlite>,
    schedule: &MonitorScheduleRecord,
    effective_from_at: &str,
    created_at: &str,
    provenance_kind: &str,
    previous_state: Option<&MonitorScheduleState>,
) -> Result<(), sqlx::Error> {
    let version_id = Uuid::new_v4().to_string();
    let effective_from_epoch_ms = timestamp_epoch_ms(effective_from_at)?;
    let due_from_at = schedule.next_due_at.as_deref();
    let due_from_epoch_ms = due_from_at.map(timestamp_epoch_ms).transpose()?;
    let (activated_at, paused_at, resumed_at, archived_at): (
        Option<&str>,
        Option<&str>,
        Option<&str>,
        Option<&str>,
    ) = match (&schedule.state, previous_state) {
        (MonitorScheduleState::Enabled, Some(MonitorScheduleState::Paused)) => {
            (None, None, Some(effective_from_at), None)
        }
        (MonitorScheduleState::Enabled, Some(MonitorScheduleState::Enabled)) => {
            (None, None, None, None)
        }
        (MonitorScheduleState::Enabled, _) => (Some(effective_from_at), None, None, None),
        (MonitorScheduleState::Paused, Some(MonitorScheduleState::Paused)) => {
            (None, None, None, None)
        }
        (MonitorScheduleState::Paused, _) => (None, Some(effective_from_at), None, None),
        (MonitorScheduleState::Archived, _) => (None, None, None, Some(effective_from_at)),
    };
    sqlx::query(
        "INSERT INTO monitor_schedule_versions(
            schedule_version_id, schedule_id, host_id, revision, profile,
            interval_seconds, jitter_seconds, jitter_offset_seconds,
            stale_after_seconds, state, due_from_at, due_from_at_epoch_ms,
            effective_from_at, effective_from_at_epoch_ms, provenance_kind,
            activated_at, paused_at, resumed_at, archived_at, created_at
         ) VALUES (?, ?, ?, ?, 'host_resource_v1', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(version_id)
    .bind(&schedule.schedule_id)
    .bind(&schedule.host_id)
    .bind(schedule.revision)
    .bind(i64::from(schedule.interval_seconds))
    .bind(i64::from(schedule.jitter_seconds))
    .bind(i64::from(schedule.jitter_offset_seconds))
    .bind(i64::from(schedule.stale_after_seconds))
    .bind(schedule_state_name(&schedule.state))
    .bind(due_from_at)
    .bind(due_from_epoch_ms)
    .bind(effective_from_at)
    .bind(effective_from_epoch_ms)
    .bind(provenance_kind)
    .bind(activated_at)
    .bind(paused_at)
    .bind(resumed_at)
    .bind(archived_at)
    .bind(created_at)
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

/// Close the old half-open due interval before inserting the next revision.
/// A schedule may have been created by a pre-H3c fixture; in that case create
/// one explicit legacy-compatible baseline rather than silently losing its
/// provenance.
async fn close_schedule_version(
    tx: &mut Transaction<'_, Sqlite>,
    current: &MonitorScheduleRecord,
    effective_until_at: &str,
    due_until_at: &str,
) -> Result<(), sqlx::Error> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT schedule_version_id FROM monitor_schedule_versions
         WHERE schedule_id = ? AND revision = ?",
    )
    .bind(&current.schedule_id)
    .bind(current.revision)
    .fetch_optional(&mut **tx)
    .await?;
    if existing.is_none() {
        let baseline_id = format!(
            "{}:revision:{}:compat",
            current.schedule_id, current.revision
        );
        let effective_from_epoch_ms = timestamp_epoch_ms(&current.created_at)?;
        let due_from_at = current.next_due_at.as_deref();
        let due_from_epoch_ms = due_from_at.map(timestamp_epoch_ms).transpose()?;
        sqlx::query(
            "INSERT INTO monitor_schedule_versions(
                schedule_version_id, schedule_id, host_id, revision, profile,
                interval_seconds, jitter_seconds, jitter_offset_seconds,
                stale_after_seconds, state, due_from_at, due_from_at_epoch_ms,
                effective_from_at, effective_from_at_epoch_ms, provenance_kind,
                activated_at, paused_at, archived_at, created_at
             ) VALUES (?, ?, ?, ?, 'host_resource_v1', ?, ?, ?, ?, ?, ?, ?, ?, ?,
                       'legacy_baseline', ?, ?, ?, ?)",
        )
        .bind(baseline_id)
        .bind(&current.schedule_id)
        .bind(&current.host_id)
        .bind(current.revision)
        .bind(i64::from(current.interval_seconds))
        .bind(i64::from(current.jitter_seconds))
        .bind(i64::from(current.jitter_offset_seconds))
        .bind(i64::from(current.stale_after_seconds))
        .bind(schedule_state_name(&current.state))
        .bind(due_from_at)
        .bind(due_from_epoch_ms)
        .bind(&current.created_at)
        .bind(effective_from_epoch_ms)
        .bind(matches!(current.state, MonitorScheduleState::Enabled).then_some(&current.created_at))
        .bind(matches!(current.state, MonitorScheduleState::Paused).then_some(&current.created_at))
        .bind(
            matches!(current.state, MonitorScheduleState::Archived).then_some(&current.created_at),
        )
        .bind(&current.created_at)
        .execute(&mut **tx)
        .await?;
    }
    let effective_until_epoch_ms = timestamp_epoch_ms(effective_until_at)?;
    let due_until_epoch_ms = timestamp_epoch_ms(due_until_at)?;
    let closed = sqlx::query(
        "UPDATE monitor_schedule_versions
         SET effective_until_at = ?, effective_until_at_epoch_ms = ?,
             due_until_at = CASE WHEN state = 'enabled' THEN ? ELSE NULL END,
             due_until_at_epoch_ms = CASE WHEN state = 'enabled' THEN ? ELSE NULL END
         WHERE schedule_id = ? AND revision = ? AND effective_until_at IS NULL",
    )
    .bind(effective_until_at)
    .bind(effective_until_epoch_ms)
    .bind(due_until_at)
    .bind(due_until_epoch_ms)
    .bind(&current.schedule_id)
    .bind(current.revision)
    .execute(&mut **tx)
    .await?;
    if closed.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

fn timestamp_epoch_ms(value: &str) -> Result<i64, sqlx::Error> {
    parse_timestamp(value)
        .map(|parsed| parsed.timestamp_millis())
        .ok_or_else(|| sqlx::Error::Protocol(format!("invalid RFC3339 timestamp: {value}")))
}

pub async fn clear_scheduler_leases(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE monitor_schedules
         SET lease_owner = NULL, lease_token = NULL, lease_until = NULL
         WHERE lease_token IS NOT NULL",
    )
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

pub fn spawn(state: AppState, mut shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    state
        .monitoring_scheduler
        .started
        .store(true, Ordering::Release);
    tokio::spawn(async move {
        let tick_seconds = state.monitoring_scheduler.settings().tick_seconds;
        let mut ticker = interval(StdDuration::from_secs(u64::from(tick_seconds)));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if state.shutdown_requested.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(error) = tick_once(&state, Utc::now()).await {
                        tracing::error!(error = %error, "monitor scheduler tick failed");
                    }
                }
            }
        }
        // Stop claiming first, then wait until every observation permit is
        // returned. This lets already accepted read-only jobs finish before
        // the Tokio runtime exits; a hard kill is still recovered on startup.
        let maximum = state.monitoring_scheduler.settings().max_concurrency;
        if let Ok(permits) = state
            .observation_permits
            .clone()
            .acquire_many_owned(maximum)
            .await
        {
            drop(permits);
        }
        state
            .monitoring_scheduler
            .started
            .store(false, Ordering::Release);
    })
}

#[derive(Debug)]
struct DueSchedule {
    schedule_id: String,
    schedule_version_id: String,
    host_id: String,
    interval_seconds: u32,
    stale_after_seconds: u32,
    revision: i64,
    next_due_at: String,
}

#[derive(Debug)]
struct ClaimedRun {
    run_id: String,
    host_id: String,
    state: MonitorRunState,
}

pub async fn tick_once(state: &AppState, tick_at: DateTime<Utc>) -> Result<u32, SchedulerError> {
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Ok(0);
    }
    // DELETE holds the write side while it drains observation permits, takes
    // a verified backup, and cascades the scope. The scheduler must join the
    // same admission fence even when no permit is available, because its
    // skipped_overlap path still writes a durable run and advances next_due_at.
    let observation_admission = state.observation_admission.read().await;
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Ok(0);
    }
    let result = tick_once_inner(state, tick_at).await;
    drop(observation_admission);
    let completed_at = Utc::now();
    match &result {
        Ok(_) => {
            state
                .monitoring_scheduler
                .record_tick_success(completed_at)
                .await;
        }
        Err(error) => {
            state
                .monitoring_scheduler
                .record_tick_failure(completed_at, scheduler_tick_error_code(error))
                .await;
        }
    }
    result
}

async fn tick_once_inner(state: &AppState, tick_at: DateTime<Utc>) -> Result<u32, SchedulerError> {
    let due_rows = sqlx::query(
        "SELECT schedules.schedule_id, schedules.host_id, schedules.interval_seconds,
                schedules.stale_after_seconds, schedules.revision, schedules.next_due_at,
                versions.schedule_version_id
         FROM monitor_schedules schedules
         LEFT JOIN monitor_schedule_versions versions
           ON versions.schedule_id = schedules.schedule_id
          AND versions.host_id = schedules.host_id
          AND versions.revision = schedules.revision
          AND versions.effective_until_at IS NULL
         WHERE schedules.state = 'enabled' AND schedules.next_due_at <= ?
         ORDER BY schedules.next_due_at, schedules.schedule_id LIMIT ?",
    )
    .bind(timestamp(tick_at))
    .bind(MAX_DUE_PER_TICK)
    .fetch_all(&state.pool)
    .await
    .map_err(SchedulerError::Storage)?;
    let mut claimed = 0u32;
    for row in due_rows {
        if state.shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        let due = DueSchedule {
            schedule_id: row
                .try_get("schedule_id")
                .map_err(SchedulerError::Storage)?,
            schedule_version_id: row
                .try_get::<Option<String>, _>("schedule_version_id")
                .map_err(SchedulerError::Storage)?
                .ok_or(SchedulerError::Internal)?,
            host_id: row.try_get("host_id").map_err(SchedulerError::Storage)?,
            interval_seconds: non_negative_u32(
                row.try_get("interval_seconds")
                    .map_err(SchedulerError::Storage)?,
            ),
            stale_after_seconds: non_negative_u32(
                row.try_get("stale_after_seconds")
                    .map_err(SchedulerError::Storage)?,
            ),
            revision: row.try_get("revision").map_err(SchedulerError::Storage)?,
            next_due_at: row
                .try_get("next_due_at")
                .map_err(SchedulerError::Storage)?,
        };
        let (permit, overlap_only) = match state.observation_permits.clone().try_acquire_owned() {
            Ok(permit) => (Some(permit), false),
            // A running manual/discovery observation already owns a permit.
            // Still materialize its due slot as skipped_overlap. If capacity is
            // occupied only by another HOST, leave this slot due instead of
            // creating an unbounded queued backlog.
            Err(_) => (None, true),
        };
        if state.shutdown_requested.load(Ordering::Acquire) {
            drop(permit);
            break;
        }
        match claim_due(
            &state.pool,
            &state.monitoring_scheduler,
            due,
            tick_at,
            overlap_only,
        )
        .await?
        {
            Some(run) => {
                claimed = claimed.saturating_add(1);
                if let Err(error) = events::publish(
                    &state.pool,
                    ChangeEventKind::MonitorRunChanged,
                    &format!("monitor-run:{}", run.run_id),
                    0,
                    json!({"state": run_state_name(&run.state)}),
                )
                .await
                {
                    tracing::warn!(run_id = %run.run_id, error = %error, "could not publish scheduled monitor run");
                }
                if run.state == MonitorRunState::Queued {
                    let permit = permit.ok_or(SchedulerError::Internal)?;
                    let background_state = state.clone();
                    tokio::spawn(async move {
                        monitoring_api::run_scheduled_monitor_background(
                            background_state,
                            run.run_id,
                            run.host_id,
                            permit,
                        )
                        .await;
                    });
                } else {
                    drop(permit);
                }
            }
            None => {
                drop(permit);
                // Capacity may be occupied by a later due HOST. Continue the
                // bounded scan so that HOST's overlapping slot can still be
                // recorded as skipped without queuing idle HOST work.
            }
        }
    }
    Ok(claimed)
}

async fn claim_due(
    pool: &SqlitePool,
    runtime: &Arc<MonitoringSchedulerRuntime>,
    due: DueSchedule,
    tick_at: DateTime<Utc>,
    overlap_only: bool,
) -> Result<Option<ClaimedRun>, SchedulerError> {
    let parsed_due = parse_timestamp(&due.next_due_at).ok_or(SchedulerError::Internal)?;
    if parsed_due > tick_at {
        return Ok(None);
    }
    let interval = Duration::seconds(i64::from(due.interval_seconds));
    let missed = (tick_at - parsed_due).num_seconds() / i64::from(due.interval_seconds);
    let scheduled_for = parsed_due + interval * i32::try_from(missed.max(0)).unwrap_or(i32::MAX);
    let trigger = if missed > 0 {
        MonitorRunTrigger::CatchUp
    } else {
        MonitorRunTrigger::Scheduled
    };
    let next_due_at = scheduled_for + interval;
    let lease_token = Uuid::new_v4().to_string();
    let lease_until = tick_at + Duration::seconds(120);
    let mut tx = pool.begin().await.map_err(SchedulerError::Storage)?;
    let lease = sqlx::query(
        "UPDATE monitor_schedules
         SET lease_owner = ?, lease_token = ?, lease_until = ?
         WHERE schedule_id = ? AND state = 'enabled' AND revision = ?
           AND next_due_at = ? AND (lease_until IS NULL OR lease_until < ?)",
    )
    .bind(&runtime.instance_id)
    .bind(&lease_token)
    .bind(timestamp(lease_until))
    .bind(&due.schedule_id)
    .bind(due.revision)
    .bind(&due.next_due_at)
    .bind(timestamp(tick_at))
    .execute(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    if lease.rows_affected() != 1 {
        tx.rollback().await.map_err(SchedulerError::Storage)?;
        return Ok(None);
    }

    let monitor_active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monitor_runs
         WHERE host_id = ? AND state IN ('queued', 'running')",
    )
    .bind(&due.host_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    let discovery_active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM discovery_runs
         WHERE host_id = ? AND state IN ('accepted', 'running')",
    )
    .bind(&due.host_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    if overlap_only && monitor_active == 0 && discovery_active == 0 {
        tx.rollback().await.map_err(SchedulerError::Storage)?;
        return Ok(None);
    }
    let run_state = if monitor_active > 0 || discovery_active > 0 {
        MonitorRunState::SkippedOverlap
    } else {
        MonitorRunState::Queued
    };
    // Pin both provenance dimensions at admission.  The evaluator must never
    // resolve the mutable current policy after a run has started.
    let health_policy_version_id: Option<String> = sqlx::query_scalar(
        "SELECT policy_version_id FROM health_policy_versions
         WHERE host_id = ? AND lifecycle_state = 'current'
         ORDER BY revision DESC LIMIT 1",
    )
    .bind(&due.host_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    let run_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();
    let scheduled_for_text = timestamp(scheduled_for);
    let idempotency_key = format!("schedule:{}:{}", due.schedule_id, scheduled_for_text);
    let request_sha256 = hex_digest(&Sha256::digest(idempotency_key.as_bytes()));
    let submitted_at = timestamp(tick_at);
    let accepted = MonitorRunAcceptedResponse {
        data: MonitorRunAccepted {
            request_id: request_id.clone(),
            run_id: run_id.clone(),
            host_id: due.host_id.clone(),
            profile: HostMonitorProfile::HostResourceV1,
            state: run_state.clone(),
            submitted_at: submitted_at.clone(),
        },
        meta: real_meta(request_id.clone(), due.revision),
    };
    let accepted_json = serde_json::to_string(&accepted).map_err(|_| SchedulerError::Internal)?;
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, schedule_id, schedule_revision,
            schedule_version_id, health_policy_version_id,
            scheduled_for, stale_after_seconds, due_interval_seconds,
            missed_due_count, failure_code, failure_summary, submitted_at,
            finished_at, accepted_response_json
         ) VALUES (?, ?, ?, ?, ?, 'host_resource_v1', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(&due.host_id)
    .bind(&request_id)
    .bind(&idempotency_key)
    .bind(&request_sha256)
    .bind(trigger_name(&trigger))
    .bind(run_state_name(&run_state))
    .bind(&due.schedule_id)
    .bind(due.revision)
    .bind(&due.schedule_version_id)
    .bind(&health_policy_version_id)
    .bind(&scheduled_for_text)
    .bind(i64::from(due.stale_after_seconds))
    .bind(i64::from(due.interval_seconds))
    .bind(missed.max(0))
    .bind((run_state == MonitorRunState::SkippedOverlap).then_some("MONITOR_OVERLAP_SKIPPED"))
    .bind((run_state == MonitorRunState::SkippedOverlap).then_some(
        "Scheduled observation was skipped because this HOST already had an active observation",
    ))
    .bind(&submitted_at)
    .bind((run_state == MonitorRunState::SkippedOverlap).then_some(&submitted_at))
    .bind(&accepted_json)
    .execute(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    sqlx::query(
        "UPDATE monitor_schedules
         SET last_due_at = ?, next_due_at = ?, lease_owner = NULL,
             lease_token = NULL, lease_until = NULL, updated_at = ?
         WHERE schedule_id = ? AND lease_token = ?",
    )
    .bind(&scheduled_for_text)
    .bind(timestamp(next_due_at))
    .bind(&submitted_at)
    .bind(&due.schedule_id)
    .bind(&lease_token)
    .execute(&mut *tx)
    .await
    .map_err(SchedulerError::Storage)?;
    if run_state == MonitorRunState::SkippedOverlap {
        // A skipped due slot is a terminal observation receipt. Persist an
        // explicit unknown evaluation in this transaction so health bands
        // distinguish overlap from an unaccounted schedule gap.
        monitoring_health::evaluate_run_tx(
            &mut tx,
            &run_id,
            &due.host_id,
            monitoring_health::EvaluationTiming {
                evaluated_at: tick_at,
                observation_state: crate::contracts::HealthObservationState::None,
                observed_at: None,
                valid_until: None,
            },
        )
        .await
        .map_err(SchedulerError::Storage)?;
    }
    tx.commit().await.map_err(SchedulerError::Storage)?;
    Ok(Some(ClaimedRun {
        run_id,
        host_id: due.host_id,
        state: run_state,
    }))
}

async fn load_schedule(
    pool: &SqlitePool,
    schedule_id: &str,
) -> Result<Option<MonitorScheduleRecord>, SchedulerError> {
    sqlx::query(
        "SELECT schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                last_due_at, revision, created_at, updated_at
         FROM monitor_schedules WHERE schedule_id = ?",
    )
    .bind(schedule_id)
    .fetch_optional(pool)
    .await
    .map_err(SchedulerError::Storage)?
    .map(schedule_from_row)
    .transpose()
}

async fn load_schedule_tx(
    tx: &mut Transaction<'_, Sqlite>,
    schedule_id: &str,
) -> Result<Option<MonitorScheduleRecord>, SchedulerError> {
    sqlx::query(
        "SELECT schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                last_due_at, revision, created_at, updated_at
         FROM monitor_schedules WHERE schedule_id = ?",
    )
    .bind(schedule_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(SchedulerError::Storage)?
    .map(schedule_from_row)
    .transpose()
}

fn schedule_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> Result<MonitorScheduleRecord, SchedulerError> {
    let state: String = row.try_get("state").map_err(SchedulerError::Storage)?;
    Ok(MonitorScheduleRecord {
        schedule_id: row
            .try_get("schedule_id")
            .map_err(SchedulerError::Storage)?,
        host_id: row.try_get("host_id").map_err(SchedulerError::Storage)?,
        profile: HostMonitorProfile::HostResourceV1,
        interval_seconds: non_negative_u32(
            row.try_get("interval_seconds")
                .map_err(SchedulerError::Storage)?,
        ),
        jitter_seconds: non_negative_u32(
            row.try_get("jitter_seconds")
                .map_err(SchedulerError::Storage)?,
        ),
        jitter_offset_seconds: non_negative_u32(
            row.try_get("jitter_offset_seconds")
                .map_err(SchedulerError::Storage)?,
        ),
        stale_after_seconds: non_negative_u32(
            row.try_get("stale_after_seconds")
                .map_err(SchedulerError::Storage)?,
        ),
        state: parse_schedule_state(&state),
        next_due_at: row
            .try_get("next_due_at")
            .map_err(SchedulerError::Storage)?,
        last_due_at: row
            .try_get("last_due_at")
            .map_err(SchedulerError::Storage)?,
        revision: row.try_get("revision").map_err(SchedulerError::Storage)?,
        created_at: row.try_get("created_at").map_err(SchedulerError::Storage)?,
        updated_at: row.try_get("updated_at").map_err(SchedulerError::Storage)?,
    })
}

async fn replay_create(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
    request_sha256: &str,
) -> Result<Option<MonitorScheduleResponse>, SchedulerError> {
    let row = sqlx::query(
        "SELECT create_request_sha256, created_response_json
         FROM monitor_schedules WHERE host_id = ? AND create_idempotency_key = ?",
    )
    .bind(host_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(SchedulerError::Storage)?;
    row.map(|row| {
        let recorded: String = row
            .try_get("create_request_sha256")
            .map_err(SchedulerError::Storage)?;
        if recorded != request_sha256 {
            return Err(SchedulerError::Conflict {
                code: "IDEMPOTENCY_KEY_REUSED",
                message: "Idempotency-Key 已用于不同的监控调度请求",
                details: json!({"host_id": host_id}),
            });
        }
        let payload: String = row
            .try_get("created_response_json")
            .map_err(SchedulerError::Storage)?;
        serde_json::from_str(&payload).map_err(|_| SchedulerError::Internal)
    })
    .transpose()
}

fn validate_schedule_values(
    interval_seconds: u32,
    jitter_seconds: u32,
    stale_after_seconds: u32,
) -> Result<(), SchedulerError> {
    if !(MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&interval_seconds) {
        return Err(SchedulerError::BadRequest {
            code: "INVALID_MONITOR_INTERVAL",
            message: "资源监控周期超出允许范围",
            details: json!({
                "minimum_seconds": MIN_INTERVAL_SECONDS,
                "maximum_seconds": MAX_INTERVAL_SECONDS
            }),
        });
    }
    if jitter_seconds >= interval_seconds {
        return Err(SchedulerError::BadRequest {
            code: "INVALID_MONITOR_JITTER",
            message: "jitter 必须小于采集周期",
            details: json!({"interval_seconds": interval_seconds}),
        });
    }
    if stale_after_seconds < interval_seconds || stale_after_seconds > MAX_STALE_AFTER_SECONDS {
        return Err(SchedulerError::BadRequest {
            code: "INVALID_MONITOR_STALE_AFTER",
            message: "数据过期窗口必须不短于采集周期",
            details: json!({
                "minimum_seconds": interval_seconds,
                "maximum_seconds": MAX_STALE_AFTER_SECONDS
            }),
        });
    }
    Ok(())
}

async fn ensure_host_exists(pool: &SqlitePool, host_id: &str) -> Result<(), SchedulerError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .fetch_optional(pool)
        .await
        .map_err(SchedulerError::Storage)?;
    if exists.is_none() {
        return Err(SchedulerError::NotFound {
            resource: "host",
            id: host_id.to_owned(),
        });
    }
    Ok(())
}

fn stable_jitter(schedule_id: &str, revision: i64, jitter_seconds: u32) -> u32 {
    if jitter_seconds == 0 {
        return 0;
    }
    let digest = Sha256::digest(format!("{schedule_id}:{revision}").as_bytes());
    let value = u64::from_be_bytes(digest[..8].try_into().expect("eight-byte digest prefix"));
    u32::try_from(value % (u64::from(jitter_seconds) + 1)).unwrap_or(0)
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, SchedulerError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .ok_or_else(|| SchedulerError::BadRequest {
            code: "IDEMPOTENCY_KEY_REQUIRED",
            message: "该操作需要 Idempotency-Key",
            details: json!({}),
        })
}

fn if_match_revision(headers: &HeaderMap) -> Result<i64, SchedulerError> {
    let value = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .ok_or(SchedulerError::PreconditionRequired)?;
    let normalized = value.trim_matches('"');
    normalized
        .strip_prefix("revision-")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 1)
        .ok_or_else(|| SchedulerError::BadRequest {
            code: "INVALID_IF_MATCH",
            message: "If-Match 必须使用 revision-N 格式",
            details: json!({}),
        })
}

fn digest_json<T: serde::Serialize>(value: &T) -> Result<String, SchedulerError> {
    let payload = serde_json::to_vec(value).map_err(|_| SchedulerError::Internal)?;
    Ok(hex_digest(&Sha256::digest(payload)))
}

fn invalid_json() -> SchedulerError {
    SchedulerError::BadRequest {
        code: "INVALID_JSON",
        message: "请求正文不是符合契约的 JSON",
        details: json!({}),
    }
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

fn real_meta(request_id: impl Into<String>, revision: i64) -> ApiMeta {
    ApiMeta {
        request_id: request_id.into(),
        revision,
        generated_at: now(),
        freshness: Freshness::Fresh,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status: DataSourceStatus::Fresh,
            label: "SQLite · HOST 周期监控调度".to_owned(),
        },
    }
}

fn parse_schedule_state(value: &str) -> MonitorScheduleState {
    match value {
        "enabled" => MonitorScheduleState::Enabled,
        "archived" => MonitorScheduleState::Archived,
        _ => MonitorScheduleState::Paused,
    }
}

fn schedule_state_name(value: &MonitorScheduleState) -> &'static str {
    match value {
        MonitorScheduleState::Enabled => "enabled",
        MonitorScheduleState::Paused => "paused",
        MonitorScheduleState::Archived => "archived",
    }
}

fn trigger_name(value: &MonitorRunTrigger) -> &'static str {
    match value {
        MonitorRunTrigger::Manual => "manual",
        MonitorRunTrigger::Scheduled => "scheduled",
        MonitorRunTrigger::CatchUp => "catch_up",
    }
}

fn run_state_name(value: &MonitorRunState) -> &'static str {
    match value {
        MonitorRunState::Queued => "queued",
        MonitorRunState::Running => "running",
        MonitorRunState::Succeeded => "succeeded",
        MonitorRunState::Partial => "partial",
        MonitorRunState::Failed => "failed",
        MonitorRunState::TimedOut => "timed_out",
        MonitorRunState::SkippedOverlap => "skipped_overlap",
        MonitorRunState::Interrupted => "interrupted",
    }
}

fn scheduler_late_after_seconds(settings: SchedulerSettings) -> u32 {
    settings
        .tick_seconds
        .saturating_mul(3)
        .max(MIN_LATE_AFTER_SECONDS)
}

fn derive_scheduler_state(
    started: bool,
    evaluated_at: DateTime<Utc>,
    health: &SchedulerTickHealth,
    late_after_seconds: u32,
) -> (MonitoringSchedulerState, MonitoringSchedulerStateReason) {
    if !started {
        return (
            MonitoringSchedulerState::Stopped,
            MonitoringSchedulerStateReason::SchedulerNotStarted,
        );
    }
    if health.consecutive_failures > 0 {
        return (
            MonitoringSchedulerState::Unknown,
            MonitoringSchedulerStateReason::LatestTickFailed,
        );
    }
    let Some(last_successful_at) = health.last_successful_at else {
        return (
            MonitoringSchedulerState::Unknown,
            MonitoringSchedulerStateReason::AwaitingFirstSuccessfulTick,
        );
    };
    if evaluated_at.signed_duration_since(last_successful_at)
        > Duration::seconds(i64::from(late_after_seconds))
    {
        (
            MonitoringSchedulerState::Late,
            MonitoringSchedulerStateReason::SuccessfulTickLate,
        )
    } else {
        (
            MonitoringSchedulerState::Running,
            MonitoringSchedulerStateReason::SuccessfulTickRecent,
        )
    }
}

fn scheduler_tick_error_code(error: &SchedulerError) -> &'static str {
    match error {
        SchedulerError::Storage(_) => "SCHEDULER_STORAGE_UNAVAILABLE",
        SchedulerError::Internal => "SCHEDULER_INTERNAL_ERROR",
        SchedulerError::BadRequest { .. }
        | SchedulerError::NotFound { .. }
        | SchedulerError::Conflict { .. }
        | SchedulerError::PreconditionRequired
        | SchedulerError::PreconditionFailed { .. } => "SCHEDULER_TICK_FAILED",
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn now() -> String {
    timestamp(Utc::now())
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn non_negative_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn count_u32(value: i64) -> u32 {
    non_negative_u32(value)
}

fn env_u32(name: &str, default: u32, minimum: u32, maximum: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (*value >= minimum) && (*value <= maximum))
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::AppState, storage};

    async fn insert_enabled_schedule_version_fixture(
        pool: &SqlitePool,
        schedule_id: &str,
    ) -> String {
        let version_id = format!("{schedule_id}:revision:1:test");
        let inserted = sqlx::query(
            "INSERT INTO monitor_schedule_versions(
                schedule_version_id, schedule_id, host_id, revision, profile,
                interval_seconds, jitter_seconds, jitter_offset_seconds,
                stale_after_seconds, state, due_from_at, due_from_at_epoch_ms,
                effective_from_at, effective_from_at_epoch_ms, provenance_kind,
                activated_at, created_at
             )
             SELECT ?, schedule_id, host_id, revision, profile,
                    interval_seconds, jitter_seconds, jitter_offset_seconds,
                    stale_after_seconds, state, next_due_at,
                    CAST(strftime('%s', next_due_at) AS INTEGER) * 1000,
                    created_at, CAST(strftime('%s', created_at) AS INTEGER) * 1000,
                    'recorded', created_at, created_at
             FROM monitor_schedules
             WHERE schedule_id = ? AND state = 'enabled' AND revision = 1",
        )
        .bind(&version_id)
        .bind(schedule_id)
        .execute(pool)
        .await
        .unwrap();
        assert_eq!(inserted.rows_affected(), 1);
        version_id
    }

    #[test]
    fn stable_jitter_is_bounded_and_repeatable() {
        let first = stable_jitter("schedule-a", 3, 30);
        assert_eq!(first, stable_jitter("schedule-a", 3, 30));
        assert!(first <= 30);
        assert_eq!(stable_jitter("schedule-a", 3, 0), 0);
    }

    #[test]
    fn schedule_validation_rejects_ambiguous_time_windows() {
        assert!(validate_schedule_values(300, 30, 900).is_ok());
        assert!(validate_schedule_values(299, 0, 900).is_err());
        assert!(validate_schedule_values(300, 300, 900).is_err());
        assert!(validate_schedule_values(300, 30, 299).is_err());
    }

    #[test]
    fn scheduler_state_distinguishes_startup_failure_late_and_recent_success() {
        let evaluated_at = Utc::now();
        let late_after_seconds = 15;
        let mut health = SchedulerTickHealth::default();
        assert_eq!(
            derive_scheduler_state(false, evaluated_at, &health, late_after_seconds),
            (
                MonitoringSchedulerState::Stopped,
                MonitoringSchedulerStateReason::SchedulerNotStarted,
            )
        );
        assert_eq!(
            derive_scheduler_state(true, evaluated_at, &health, late_after_seconds),
            (
                MonitoringSchedulerState::Unknown,
                MonitoringSchedulerStateReason::AwaitingFirstSuccessfulTick,
            )
        );

        health.last_successful_at = Some(evaluated_at - Duration::seconds(16));
        assert_eq!(
            derive_scheduler_state(true, evaluated_at, &health, late_after_seconds),
            (
                MonitoringSchedulerState::Late,
                MonitoringSchedulerStateReason::SuccessfulTickLate,
            )
        );
        health.last_successful_at = Some(evaluated_at - Duration::seconds(1));
        assert_eq!(
            derive_scheduler_state(true, evaluated_at, &health, late_after_seconds),
            (
                MonitoringSchedulerState::Running,
                MonitoringSchedulerStateReason::SuccessfulTickRecent,
            )
        );
        health.consecutive_failures = 1;
        assert_eq!(
            derive_scheduler_state(true, evaluated_at, &health, late_after_seconds),
            (
                MonitoringSchedulerState::Unknown,
                MonitoringSchedulerStateReason::LatestTickFailed,
            )
        );
    }

    #[tokio::test]
    async fn scheduler_status_separates_queued_running_and_oldest_due() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, owner_id, created_at)
             VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for host_id in [
            "host-status-due",
            "host-status-queued",
            "host-status-running",
        ] {
            sqlx::query(
                "INSERT INTO hosts(
                    host_id, workspace_id, display_name, address, port, ssh_user,
                    credential_ref, host_key_state, transport, os, status, created_at
                 ) VALUES (?, 'workspace-default', ?, ?, 22, 'fixture',
                    'secret-ref-fixture', 'verified', 'ssh', 'linux', 'connection_ready',
                    '2026-08-15T00:00:00Z')",
            )
            .bind(host_id)
            .bind(host_id)
            .bind(format!("{host_id}.invalid"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let oldest_due_at = timestamp(Utc::now() - Duration::minutes(2));
        sqlx::query(
            "INSERT INTO monitor_schedules(
                schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                revision, create_idempotency_key, create_request_sha256,
                created_response_json, created_at, updated_at
             ) VALUES ('schedule-status', 'host-status-due', 'host_resource_v1',
                300, 0, 0, 900, 'enabled', ?, 1, 'create-status', 'digest', '{}',
                '2026-08-15T00:00:00Z', '2026-08-15T00:00:00Z')",
        )
        .bind(&oldest_due_at)
        .execute(&pool)
        .await
        .unwrap();
        for (run_id, host_id, run_state) in [
            ("run-status-queued", "host-status-queued", "queued"),
            ("run-status-running", "host-status-running", "running"),
        ] {
            sqlx::query(
                "INSERT INTO monitor_runs(
                    run_id, host_id, request_id, idempotency_key, request_sha256,
                    profile, trigger_kind, state, stale_after_seconds, submitted_at,
                    accepted_response_json
                 ) VALUES (?, ?, ?, ?, 'digest', 'host_resource_v1', 'manual', ?, 900,
                    '2026-08-15T00:00:00Z', '{}')",
            )
            .bind(run_id)
            .bind(host_id)
            .bind(format!("request-{run_id}"))
            .bind(format!("key-{run_id}"))
            .bind(run_state)
            .execute(&pool)
            .await
            .unwrap();
        }

        let state = AppState::new(pool);
        let response = get_scheduler_status(State(state), HeaderMap::new())
            .await
            .unwrap()
            .0;
        assert_eq!(response.data.state, MonitoringSchedulerState::Stopped);
        assert_eq!(response.data.queued_run_count, 1);
        assert_eq!(response.data.running_run_count, 1);
        assert_eq!(response.data.due_schedule_count, 1);
        assert_eq!(
            response.data.oldest_due_at.as_deref(),
            Some(oldest_due_at.as_str())
        );
    }

    #[tokio::test]
    async fn consecutive_tick_storage_failures_are_unknown_until_a_success() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let state = AppState::new(pool.clone());
        state
            .monitoring_scheduler
            .started
            .store(true, Ordering::Release);
        tick_once(&state, Utc::now()).await.unwrap();
        let successful_at = state
            .monitoring_scheduler
            .tick_health
            .read()
            .await
            .last_successful_at;

        sqlx::query("ALTER TABLE monitor_schedules RENAME TO monitor_schedules_unavailable")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            tick_once(&state, Utc::now()).await,
            Err(SchedulerError::Storage(_))
        ));
        assert!(matches!(
            tick_once(&state, Utc::now()).await,
            Err(SchedulerError::Storage(_))
        ));
        sqlx::query("ALTER TABLE monitor_schedules_unavailable RENAME TO monitor_schedules")
            .execute(&pool)
            .await
            .unwrap();

        let failed = get_scheduler_status(State(state.clone()), HeaderMap::new())
            .await
            .unwrap()
            .0;
        assert_eq!(failed.data.state, MonitoringSchedulerState::Unknown);
        assert_eq!(
            failed.data.state_reason,
            MonitoringSchedulerStateReason::LatestTickFailed
        );
        assert_eq!(failed.data.consecutive_tick_failures, 2);
        assert_eq!(
            failed.data.last_tick_error_code.as_deref(),
            Some("SCHEDULER_STORAGE_UNAVAILABLE")
        );
        assert_eq!(
            failed.data.last_tick_at,
            successful_at.map(timestamp),
            "failed ticks must not advance the last successful tick"
        );

        tick_once(&state, Utc::now()).await.unwrap();
        let recovered = get_scheduler_status(State(state), HeaderMap::new())
            .await
            .unwrap()
            .0;
        assert_eq!(recovered.data.state, MonitoringSchedulerState::Running);
        assert_eq!(
            recovered.data.state_reason,
            MonitoringSchedulerStateReason::SuccessfulTickRecent
        );
        assert_eq!(recovered.data.consecutive_tick_failures, 0);
        assert!(recovered.data.last_tick_error_at.is_some());
    }

    #[tokio::test]
    async fn due_slot_is_skipped_when_same_host_is_active_and_all_permits_are_busy() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, owner_id, created_at)
             VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, host_key_fingerprint, transport,
                os, status, created_at
             ) VALUES ('host-permit-overlap', 'workspace-default', 'fixture',
                'fixture.invalid', 22, 'fixture', 'secret-ref-fixture', 'verified',
                'SHA256:fixture', 'ssh', 'linux', 'connection_ready',
                '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let tick_at = Utc::now();
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, host_key_fingerprint, transport,
                os, status, created_at
             ) VALUES ('host-idle-before-overlap', 'workspace-default', 'idle fixture',
                'idle-fixture.invalid', 22, 'fixture', 'secret-ref-fixture', 'verified',
                'SHA256:fixture-idle', 'ssh', 'linux', 'connection_ready',
                '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO monitor_schedules(
                schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                revision, create_idempotency_key, create_request_sha256,
                created_response_json, created_at, updated_at
             ) VALUES ('schedule-idle-before-overlap', 'host-idle-before-overlap',
                'host_resource_v1', 300, 0, 0, 900, 'enabled', ?, 1,
                'create-idle-before-overlap', 'digest', '{}', ?, ?)",
        )
        .bind(timestamp(tick_at - Duration::seconds(2)))
        .bind(timestamp(tick_at))
        .bind(timestamp(tick_at))
        .execute(&pool)
        .await
        .unwrap();
        insert_enabled_schedule_version_fixture(&pool, "schedule-idle-before-overlap").await;
        sqlx::query(
            "INSERT INTO monitor_schedules(
                schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                revision, create_idempotency_key, create_request_sha256,
                created_response_json, created_at, updated_at
             ) VALUES ('schedule-permit-overlap', 'host-permit-overlap',
                'host_resource_v1', 300, 0, 0, 900, 'enabled', ?, 1,
                'create-permit-overlap', 'digest', '{}', ?, ?)",
        )
        .bind(timestamp(tick_at - Duration::seconds(1)))
        .bind(timestamp(tick_at))
        .bind(timestamp(tick_at))
        .execute(&pool)
        .await
        .unwrap();
        let overlap_version_id =
            insert_enabled_schedule_version_fixture(&pool, "schedule-permit-overlap").await;
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, submitted_at,
                accepted_response_json
             ) VALUES ('manual-permit-overlap', 'host-permit-overlap', 'request',
                'manual-permit-overlap', 'digest', 'host_resource_v1', 'manual',
                'running', 900, ?, '{}')",
        )
        .bind(timestamp(tick_at))
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool.clone());
        let maximum = state.monitoring_scheduler.settings().max_concurrency;
        let held = state
            .observation_permits
            .clone()
            .acquire_many_owned(maximum)
            .await
            .unwrap();
        assert_eq!(tick_once(&state, tick_at).await.unwrap(), 1);
        drop(held);

        let idle_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM monitor_runs
             WHERE schedule_id = 'schedule-idle-before-overlap'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            idle_runs, 0,
            "an idle HOST must remain due without a permit"
        );

        let row = sqlx::query(
            "SELECT state, failure_code, ssh_session_count, schedule_version_id
             FROM monitor_runs WHERE schedule_id = 'schedule-permit-overlap'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("state"), "skipped_overlap");
        assert_eq!(
            row.get::<Option<String>, _>("schedule_version_id")
                .as_deref(),
            Some(overlap_version_id.as_str())
        );
        assert_eq!(
            row.get::<Option<String>, _>("failure_code").as_deref(),
            Some("MONITOR_OVERLAP_SKIPPED")
        );
        assert_eq!(row.get::<i64, _>("ssh_session_count"), 0);
    }

    #[tokio::test]
    async fn scheduler_writes_wait_for_the_backup_delete_admission_fence() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, owner_id, created_at)
             VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, host_key_fingerprint, transport,
                os, status, created_at
             ) VALUES ('host-delete-fence', 'workspace-default', 'fixture',
                'fixture.invalid', 22, 'fixture', 'secret-ref-fixture', 'verified',
                'SHA256:fixture', 'ssh', 'linux', 'connection_ready',
                '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let tick_at = Utc::now();
        sqlx::query(
            "INSERT INTO monitor_schedules(
                schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                revision, create_idempotency_key, create_request_sha256,
                created_response_json, created_at, updated_at
             ) VALUES ('schedule-delete-fence', 'host-delete-fence',
                'host_resource_v1', 300, 0, 0, 900, 'enabled', ?, 1,
                'create-delete-fence', 'digest', '{}', ?, ?)",
        )
        .bind(timestamp(tick_at - Duration::seconds(1)))
        .bind(timestamp(tick_at))
        .bind(timestamp(tick_at))
        .execute(&pool)
        .await
        .unwrap();
        let schedule_version_id =
            insert_enabled_schedule_version_fixture(&pool, "schedule-delete-fence").await;
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, submitted_at,
                accepted_response_json
             ) VALUES ('manual-delete-fence', 'host-delete-fence', 'request',
                'manual-delete-fence', 'digest', 'host_resource_v1', 'manual',
                'running', 900, ?, '{}')",
        )
        .bind(timestamp(tick_at))
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool.clone());
        let maximum = state.monitoring_scheduler.settings().max_concurrency;
        let permits = state
            .observation_permits
            .clone()
            .acquire_many_owned(maximum)
            .await
            .unwrap();
        let deletion_fence = state.observation_admission.write().await;
        let tick_state = state.clone();
        let mut tick = tokio::spawn(async move { tick_once(&tick_state, tick_at).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut tick)
                .await
                .is_err(),
            "scheduler must not materialize even a skipped run during backup/delete"
        );
        let while_fenced: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM monitor_runs WHERE schedule_id = 'schedule-delete-fence'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(while_fenced, 0);

        drop(deletion_fence);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), tick)
                .await
                .expect("scheduler resumed after deletion fence")
                .expect("scheduler task")
                .expect("scheduler tick"),
            1
        );
        drop(permits);
        let row = sqlx::query(
            "SELECT state, failure_code, schedule_version_id FROM monitor_runs
             WHERE schedule_id = 'schedule-delete-fence'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("state"), "skipped_overlap");
        assert_eq!(
            row.get::<Option<String>, _>("schedule_version_id")
                .as_deref(),
            Some(schedule_version_id.as_str())
        );
        assert_eq!(
            row.get::<Option<String>, _>("failure_code").as_deref(),
            Some("MONITOR_OVERLAP_SKIPPED")
        );
    }

    #[tokio::test]
    async fn stale_only_patch_does_not_rewind_a_due_advanced_by_the_scheduler() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, owner_id, created_at)
             VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, transport, os, status, created_at
             ) VALUES ('host-patch-race', 'workspace-default', 'fixture',
                'fixture.invalid', 22, 'fixture', 'secret-ref-fixture',
                'verified', 'ssh', 'linux', 'connection_ready', '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let original_due = Utc::now();
        let advanced_due = original_due + Duration::seconds(300);
        sqlx::query(
            "INSERT INTO monitor_schedules(
                schedule_id, host_id, profile, interval_seconds, jitter_seconds,
                jitter_offset_seconds, stale_after_seconds, state, next_due_at,
                revision, create_idempotency_key, create_request_sha256,
                created_response_json, created_at, updated_at
             ) VALUES ('schedule-patch-race', 'host-patch-race',
                'host_resource_v1', 300, 0, 0, 900, 'enabled', ?, 1,
                'create-patch-race', 'digest', '{}', ?, ?)",
        )
        .bind(timestamp(original_due))
        .bind(timestamp(original_due))
        .bind(timestamp(original_due))
        .execute(&pool)
        .await
        .unwrap();

        // This is the interleaving that previously lost the scheduler write:
        // PATCH read original_due, then the scheduler advanced next_due_at
        // without changing the configuration revision.
        sqlx::query(
            "UPDATE monitor_schedules SET next_due_at = ?, last_due_at = ?
             WHERE schedule_id = 'schedule-patch-race'",
        )
        .bind(timestamp(advanced_due))
        .bind(timestamp(original_due))
        .execute(&pool)
        .await
        .unwrap();
        let stale_due_text = timestamp(original_due);
        let updated_at = timestamp(Utc::now());
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            persist_schedule_update(
                &mut tx,
                "schedule-patch-race",
                300,
                0,
                0,
                1200,
                &MonitorScheduleState::Enabled,
                false,
                Some(&stale_due_text),
                2,
                &updated_at,
                1,
            )
            .await
            .unwrap(),
            1
        );
        tx.commit().await.unwrap();

        let row = sqlx::query(
            "SELECT next_due_at, stale_after_seconds, revision
             FROM monitor_schedules WHERE schedule_id = 'schedule-patch-race'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("next_due_at"), timestamp(advanced_due));
        assert_eq!(row.get::<i64, _>("stale_after_seconds"), 1200);
        assert_eq!(row.get::<i64, _>("revision"), 2);
    }
}
