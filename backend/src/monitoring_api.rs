use std::sync::atomic::Ordering;

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorResponse, ApiMeta, DataSourceDescriptor, DataSourceKind, DataSourceStatus,
        Freshness, HealthObservationState, HostCpuCoreMetric, HostCpuMetric, HostDiskIoMetric,
        HostFilesystemMetric, HostLoadMetric, HostMemoryMetric, HostMonitorProfile,
        HostMonitoringData, HostMonitoringResponse, HostNetworkMetric, HostProcessMetric,
        HostResourceSnapshot, HostUptimeMetric, MonitorFamilyCoverage, MonitorFreshness,
        MonitorMetricQuality, MonitorRunAccepted, MonitorRunAcceptedResponse,
        MonitorRunCreateRequest, MonitorRunRecord, MonitorRunResponse, MonitorRunState,
        MonitorRunTrigger,
    },
    events::{self, ChangeEventKind},
    m1::{self, M1Error},
    monitoring::{
        FamilyObservation, HostResourceObservation, MetricQuality, RunCompleteness,
        host_resource_v1_batch_command, parse_host_resource_v1, parse_host_resource_v1_batch,
    },
    monitoring_health::{self, EvaluationTiming},
    monitoring_history,
    ssh::{SshError, SshFailure},
};

const MAX_REQUEST_ID: usize = 128;
const MAX_IDEMPOTENCY_KEY: usize = 128;
const COLLECTOR_VERSION: &str = "host_resource_v1/1";
const DEFAULT_SNAPSHOT_STALE_AFTER_SECONDS: i64 = 900;
const MAX_BATCH_STDOUT_BYTES: usize = 1024 * 1024;

#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host_id}/monitor-runs",
    tag = "monitoring",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = MonitorRunCreateRequest,
    responses(
        (status = 202, body = MonitorRunAcceptedResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 409, body = ApiErrorResponse),
        (status = 503, body = ApiErrorResponse)
    )
)]
pub async fn create_monitor_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    payload: Result<Json<MonitorRunCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MonitorRunAcceptedResponse>), M1Error> {
    let Json(request) = payload.map_err(|_| M1Error::BadRequest {
        code: "INVALID_JSON",
        message: "请求正文不是符合契约的 JSON",
        details: json!({}),
    })?;
    let request_id = request_id(&headers);
    let idempotency_key = idempotency_key(&headers)?;
    let request_sha256 = request_digest(&request)?;

    if let Some(response) =
        replay_request(&state.pool, &host_id, &idempotency_key, &request_sha256).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(response)));
    }
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Err(M1Error::ShuttingDown);
    }
    // A deletion takes the write side of this short admission gate before it
    // drains observation permits. Therefore every run inserted before the
    // backup is either completed by the permit fence or is already present in
    // that backup; requests arriving later re-check the HOST after deletion.
    let observation_admission = state.observation_admission.read().await;

    // Resolving here proves the fixed target is connection-ready and the
    // SecretRef is usable before an accepted receipt is persisted. The secret
    // value remains memory-only and is moved directly into the background run.
    let target = m1::resolve_monitoring_target(&state, &host_id).await?;
    let submitted_at = now();
    let run_id = Uuid::new_v4().to_string();
    let accepted = MonitorRunAcceptedResponse {
        data: MonitorRunAccepted {
            request_id: request_id.clone(),
            run_id: run_id.clone(),
            host_id: host_id.clone(),
            profile: request.profile,
            state: MonitorRunState::Queued,
            submitted_at: submitted_at.clone(),
        },
        meta: real_meta(request_id, Freshness::Stale),
    };
    let response_json = serde_json::to_string(&accepted).map_err(|_| M1Error::Internal)?;
    let insert = sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds,
            health_policy_version_id, submitted_at, accepted_response_json
         ) VALUES (?, ?, ?, ?, ?, 'host_resource_v1', 'manual', 'queued', ?,
            (SELECT policy_version_id FROM health_policy_versions
             WHERE host_id = ? AND lifecycle_state = 'current'), ?, ?)",
    )
    .bind(&run_id)
    .bind(&host_id)
    .bind(&accepted.data.request_id)
    .bind(&idempotency_key)
    .bind(&request_sha256)
    .bind(DEFAULT_SNAPSHOT_STALE_AFTER_SECONDS)
    .bind(&host_id)
    .bind(&submitted_at)
    .bind(&response_json)
    .execute(&state.pool)
    .await;
    if let Err(error) = insert {
        let lower = error.to_string().to_ascii_lowercase();
        if lower.contains("unique") || lower.contains("host observation already active") {
            if let Some(response) =
                replay_request(&state.pool, &host_id, &idempotency_key, &request_sha256).await?
            {
                return Ok((StatusCode::ACCEPTED, Json(response)));
            }
            return Err(M1Error::Conflict {
                code: "MONITOR_ALREADY_RUNNING",
                message: "该 HOST 已有一个资源采集任务正在运行",
                details: json!({"host_id": host_id}),
            });
        }
        return Err(M1Error::Storage(error));
    }
    publish_run_state(&state.pool, &run_id).await;
    drop(observation_admission);

    let background_state = state.clone();
    tokio::spawn(async move {
        run_manual_monitor_when_permitted(background_state, run_id, host_id, target).await;
    });
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn run_manual_monitor_when_permitted(
    state: AppState,
    run_id: String,
    host_id: String,
    target: crate::ssh::SshTarget,
) {
    match state.observation_permits.clone().acquire_owned().await {
        Ok(permit) => {
            if state.shutdown_requested.load(Ordering::Acquire) {
                drop(permit);
                match interrupt_queued_monitor_run(&state.pool, &run_id).await {
                    Ok(true) => publish_run_state(&state.pool, &run_id).await,
                    Ok(false) => {}
                    Err(error) => {
                        tracing::error!(%run_id, error = %error, "could not interrupt queued monitor during shutdown");
                    }
                }
                return;
            }
            run_monitor_background(state, run_id, host_id, Some(target), permit).await;
        }
        Err(error) => {
            tracing::error!(%run_id, error = %error, "monitor concurrency gate closed");
        }
    }
}

async fn interrupt_queued_monitor_run(
    pool: &SqlitePool,
    run_id: &str,
) -> Result<bool, sqlx::Error> {
    let finished = Utc::now();
    let mut tx = pool.begin().await?;
    let host_id: Option<String> = sqlx::query_scalar(
        "SELECT host_id FROM monitor_runs WHERE run_id = ? AND state = 'queued'",
    )
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let result = sqlx::query(
        "UPDATE monitor_runs
         SET state = 'interrupted', failure_code = 'MONITOR_INTERRUPTED',
             failure_summary = 'Monitor run was interrupted during application shutdown',
             finished_at = ?
         WHERE run_id = ? AND state = 'queued'",
    )
    .bind(finished.to_rfc3339_opts(SecondsFormat::Millis, true))
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 1
        && let Some(host_id) = host_id
    {
        monitoring_health::evaluate_run_tx(
            &mut tx,
            run_id,
            &host_id,
            EvaluationTiming {
                evaluated_at: finished,
                observation_state: HealthObservationState::None,
                observed_at: None,
                valid_until: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(result.rows_affected() == 1)
}

#[utoipa::path(
    get,
    path = "/api/v1/monitor-runs/{run_id}",
    tag = "monitoring",
    params(("run_id" = String, Path, description = "Manual host monitor run identifier")),
    responses((status = 200, body = MonitorRunResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_monitor_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<MonitorRunResponse>, M1Error> {
    let record = load_monitor_run(&state.pool, &run_id)
        .await?
        .ok_or_else(|| M1Error::NotFound {
            resource: "monitor_run",
            id: run_id,
        })?;
    let freshness = run_freshness(&record.state);
    Ok(Json(MonitorRunResponse {
        data: record,
        meta: real_meta(request_id(&headers), freshness),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/monitoring",
    tag = "monitoring",
    params(("host_id" = String, Path, description = "Registered Linux host identifier")),
    responses((status = 200, body = HostMonitoringResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_host_monitoring(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<Json<HostMonitoringResponse>, M1Error> {
    ensure_host_exists(&state.pool, &host_id).await?;
    let (latest_run, current_snapshot) = latest_for_host(&state.pool, &host_id).await?;
    let monitor_freshness = current_snapshot
        .as_ref()
        .map(|snapshot| snapshot.freshness.clone())
        .unwrap_or(MonitorFreshness::Unknown);
    Ok(Json(HostMonitoringResponse {
        data: HostMonitoringData {
            host_id,
            latest_run,
            current_snapshot,
            monitor_freshness: monitor_freshness.clone(),
        },
        meta: real_meta(request_id(&headers), api_freshness(&monitor_freshness)),
    }))
}

pub async fn recover_interrupted_monitor_runs(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let finished = Utc::now();
    let finished_at = finished.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut tx = pool.begin().await?;
    let interrupted = sqlx::query(
        "SELECT run_id, host_id FROM monitor_runs WHERE state IN ('queued', 'running')",
    )
    .fetch_all(&mut *tx)
    .await?;
    let result = sqlx::query(
        "UPDATE monitor_runs
         SET state = 'interrupted', failure_code = 'MONITOR_INTERRUPTED',
             failure_summary = 'Monitor run was interrupted by process restart', finished_at = ?
         WHERE state IN ('queued', 'running')",
    )
    .bind(finished_at)
    .execute(&mut *tx)
    .await?;
    let recovered = result.rows_affected();
    for row in interrupted {
        let run_id: String = row.try_get("run_id")?;
        let host_id: String = row.try_get("host_id")?;
        monitoring_health::evaluate_run_tx(
            &mut tx,
            &run_id,
            &host_id,
            EvaluationTiming {
                evaluated_at: finished,
                observation_state: HealthObservationState::None,
                observed_at: None,
                valid_until: None,
            },
        )
        .await?;
    }
    if recovered > 0 {
        events::publish_in_transaction(
            &mut tx,
            ChangeEventKind::MonitorRunChanged,
            "monitor-runs:startup-recovery",
            0,
            json!({
                "state": "interrupted",
                "recovered_count": recovered,
                "snapshot_required": true,
            }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(recovered)
}

pub(crate) async fn latest_for_host(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<(Option<MonitorRunRecord>, Option<HostResourceSnapshot>), M1Error> {
    let latest_run_id: Option<String> = sqlx::query_scalar(
        "SELECT run_id FROM monitor_runs WHERE host_id = ?
         ORDER BY submitted_at DESC, rowid DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    let latest_run = if let Some(run_id) = latest_run_id {
        load_monitor_run(pool, &run_id).await?
    } else {
        None
    };
    let current_snapshot = load_current_snapshot(pool, host_id).await?;
    Ok((latest_run, current_snapshot))
}

pub(crate) async fn run_scheduled_monitor_background(
    state: AppState,
    run_id: String,
    host_id: String,
    permit: OwnedSemaphorePermit,
) {
    run_monitor_background(state, run_id, host_id, None, permit).await;
}

async fn run_monitor_background(
    state: AppState,
    run_id: String,
    host_id: String,
    target: Option<crate::ssh::SshTarget>,
    _permit: OwnedSemaphorePermit,
) {
    let started_at = now();
    let running = sqlx::query(
        "UPDATE monitor_runs SET state = 'running', started_at = ?
         WHERE run_id = ? AND state = 'queued'",
    )
    .bind(&started_at)
    .bind(&run_id)
    .execute(&state.pool)
    .await;
    match running {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => return,
        Err(error) => {
            tracing::error!(%run_id, error = %error, "could not mark monitor run running");
            return;
        }
    }
    publish_run_state(&state.pool, &run_id).await;

    let target = match target {
        Some(target) => target,
        None => match m1::resolve_monitoring_target(&state, &host_id).await {
            Ok(target) => target,
            Err(error) => {
                let failure = classify_target_resolution_failure(&error);
                if let Err(persist_error) = persist_failure(&state.pool, &run_id, failure).await {
                    tracing::error!(%run_id, error = %persist_error, "could not persist monitor target failure");
                }
                publish_run_state(&state.pool, &run_id).await;
                return;
            }
        },
    };

    let command = host_resource_v1_batch_command();
    let result = state
        .ssh
        .execute(&target, &command, MAX_BATCH_STDOUT_BYTES)
        .await;
    match result {
        Ok(output) => {
            let parsed = parse_host_resource_v1_batch(&output.stdout)
                .map(|capture| parse_host_resource_v1(&capture));
            match parsed {
                Ok(observation) => {
                    if let Err(error) = persist_observation(
                        &state.pool,
                        &run_id,
                        &host_id,
                        output.output_bytes,
                        observation,
                    )
                    .await
                    {
                        tracing::error!(%run_id, error = %error, "could not persist monitor snapshot");
                    }
                }
                Err(summary) => {
                    let failure = MonitorFailure {
                        state: MonitorRunState::Failed,
                        code: "COLLECTOR_PARSE_FAILED",
                        summary,
                        output_bytes: output.output_bytes,
                        ssh_session_count: 1,
                    };
                    if let Err(error) = persist_failure(&state.pool, &run_id, failure).await {
                        tracing::error!(%run_id, error = %error, "could not persist monitor parser failure");
                    }
                }
            }
        }
        Err(error) => {
            let failure = classify_ssh_failure(error);
            if let Err(error) = persist_failure(&state.pool, &run_id, failure).await {
                tracing::error!(%run_id, error = %error, "could not persist monitor SSH failure");
            }
        }
    }
    publish_run_state(&state.pool, &run_id).await;
}

async fn publish_run_state(pool: &SqlitePool, run_id: &str) {
    let state: Result<String, sqlx::Error> =
        sqlx::query_scalar("SELECT state FROM monitor_runs WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(pool)
            .await;
    let Ok(state) = state else {
        return;
    };
    if let Err(error) = events::publish(
        pool,
        ChangeEventKind::MonitorRunChanged,
        &format!("monitor-run:{run_id}"),
        0,
        json!({"state": state}),
    )
    .await
    {
        tracing::warn!(%run_id, error = %error, "could not publish monitor run state");
    }
}

pub(crate) async fn persist_observation(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    output_bytes: usize,
    observation: HostResourceObservation,
) -> Result<(), M1Error> {
    let finished = Utc::now();
    let finished_at = finished.to_rfc3339_opts(SecondsFormat::Millis, true);
    let observed_at = finished_at.clone();
    let stale_after_seconds: i64 =
        sqlx::query_scalar("SELECT stale_after_seconds FROM monitor_runs WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(pool)
            .await
            .map_err(M1Error::Storage)?;
    let valid_until = (finished + Duration::seconds(stale_after_seconds.max(60)))
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let observation_id = Uuid::new_v4().to_string();
    let snapshot = snapshot_from_observation(
        observation_id,
        run_id.to_owned(),
        host_id.to_owned(),
        observed_at.clone(),
        valid_until.clone(),
        &observation,
    );
    let snapshot_json = serde_json::to_string(&snapshot).map_err(|_| M1Error::Internal)?;
    let coverage_json = serde_json::to_string(&snapshot.coverage).map_err(|_| M1Error::Internal)?;
    let snapshot_sha256 = hex_digest(&Sha256::digest(snapshot_json.as_bytes()));
    let history_samples = monitoring_history::samples_from_observation(host_id, &observation);
    let state = match observation.coverage.completeness {
        RunCompleteness::Succeeded => MonitorRunState::Succeeded,
        RunCompleteness::Partial => MonitorRunState::Partial,
        RunCompleteness::Failed => MonitorRunState::Failed,
    };
    let boot_id = snapshot.uptime.boot_id.clone();
    let mut tx = pool.begin().await.map_err(M1Error::Storage)?;
    let run_update = sqlx::query(
        "UPDATE monitor_runs SET state = ?, collector_version = ?, boot_id = ?,
            coverage_json = ?, output_bytes = ?, ssh_session_count = 1,
            failure_code = ?, failure_summary = ?, finished_at = ?
         WHERE run_id = ? AND state = 'running'",
    )
    .bind(monitor_state_name(&state))
    .bind(COLLECTOR_VERSION)
    .bind(&boot_id)
    .bind(&coverage_json)
    .bind(i64::try_from(output_bytes).unwrap_or(i64::MAX))
    .bind(if state == MonitorRunState::Failed {
        Some("COLLECTOR_REQUIRED_FAMILIES_UNAVAILABLE")
    } else {
        None
    })
    .bind(if state == MonitorRunState::Failed {
        Some("No required host resource family produced a valid observation")
    } else {
        None
    })
    .bind(&finished_at)
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    if run_update.rows_affected() != 1 {
        tx.rollback().await.map_err(M1Error::Storage)?;
        return Err(M1Error::Internal);
    }

    monitoring_history::insert_samples(
        &mut tx,
        run_id,
        host_id,
        &observed_at,
        finished.timestamp_millis(),
        &history_samples,
    )
    .await
    .map_err(M1Error::Storage)?;

    if state != MonitorRunState::Failed {
        sqlx::query(
            "INSERT INTO monitoring_current(
                host_id, run_id, profile, collector_version, boot_id,
                snapshot_json, coverage_json, metric_count, unknown_count,
                observed_at, valid_until, retention_tier, snapshot_sha256, updated_at
             ) VALUES (?, ?, 'host_resource_v1', ?, ?, ?, ?, ?, ?, ?, ?, 'full', ?, ?)
             ON CONFLICT(host_id) DO UPDATE SET
                run_id = excluded.run_id,
                profile = excluded.profile,
                collector_version = excluded.collector_version,
                boot_id = excluded.boot_id,
                snapshot_json = excluded.snapshot_json,
                coverage_json = excluded.coverage_json,
                metric_count = excluded.metric_count,
                unknown_count = excluded.unknown_count,
                observed_at = excluded.observed_at,
                valid_until = excluded.valid_until,
                retention_tier = excluded.retention_tier,
                snapshot_sha256 = excluded.snapshot_sha256,
                updated_at = excluded.updated_at",
        )
        .bind(host_id)
        .bind(run_id)
        .bind(COLLECTOR_VERSION)
        .bind(&boot_id)
        .bind(&snapshot_json)
        .bind(&coverage_json)
        .bind(i64::from(snapshot.metric_count))
        .bind(i64::from(snapshot.unknown_count))
        .bind(&observed_at)
        .bind(&valid_until)
        .bind(&snapshot_sha256)
        .bind(&finished_at)
        .execute(&mut *tx)
        .await
        .map_err(M1Error::Storage)?;
    }
    monitoring_health::evaluate_run_tx(
        &mut tx,
        run_id,
        host_id,
        EvaluationTiming {
            evaluated_at: finished,
            observation_state: if state == MonitorRunState::Succeeded {
                HealthObservationState::Complete
            } else {
                HealthObservationState::Partial
            },
            observed_at: Some(finished),
            valid_until: Some(finished + Duration::seconds(stale_after_seconds.max(60))),
        },
    )
    .await
    .map_err(M1Error::Storage)?;
    tx.commit().await.map_err(M1Error::Storage)
}

struct MonitorFailure {
    state: MonitorRunState,
    code: &'static str,
    summary: &'static str,
    output_bytes: usize,
    ssh_session_count: u32,
}

fn classify_ssh_failure(error: SshError) -> MonitorFailure {
    let (state, code) = match error.failure {
        SshFailure::Unreachable => (MonitorRunState::Failed, "SSH_UNREACHABLE"),
        SshFailure::Authentication => (MonitorRunState::Failed, "SSH_AUTH_FAILED"),
        SshFailure::HostKey => (MonitorRunState::Failed, "HOST_KEY_CHANGED"),
        SshFailure::Timeout => (MonitorRunState::TimedOut, "COLLECTOR_TIMED_OUT"),
        SshFailure::OutputLimit => (MonitorRunState::Failed, "COLLECTOR_OUTPUT_LIMIT"),
        SshFailure::Process => (MonitorRunState::Failed, "COLLECTOR_PROCESS_FAILED"),
    };
    let summary = match error.failure {
        SshFailure::Unreachable => "SSH target was unreachable",
        SshFailure::Authentication => "SSH authentication failed",
        SshFailure::HostKey => "SSH host identity is no longer verified",
        SshFailure::Timeout => "Fixed host resource collection timed out",
        SshFailure::OutputLimit => "Fixed host resource output exceeded its byte limit",
        SshFailure::Process => "Fixed host resource command failed",
    };
    MonitorFailure {
        state,
        code,
        summary,
        output_bytes: error.output_bytes,
        ssh_session_count: 1,
    }
}

fn classify_target_resolution_failure(error: &M1Error) -> MonitorFailure {
    let (code, summary) = match error {
        M1Error::NotFound { .. } => ("MONITOR_HOST_NOT_FOUND", "Registered HOST no longer exists"),
        M1Error::Conflict { .. } => (
            "CONNECTION_NOT_READY",
            "HOST connection or verified SSH identity is not ready",
        ),
        M1Error::SecretStore | M1Error::SecretRefUnavailable => (
            "SECRET_REF_UNAVAILABLE",
            "SSH credential reference could not be resolved",
        ),
        M1Error::Storage(_) => (
            "STORAGE_UNAVAILABLE",
            "Local monitor storage was unavailable",
        ),
        _ => (
            "MONITOR_TARGET_UNAVAILABLE",
            "Fixed monitoring target could not be resolved",
        ),
    };
    MonitorFailure {
        state: MonitorRunState::Failed,
        code,
        summary,
        output_bytes: 0,
        ssh_session_count: 0,
    }
}

async fn persist_failure(
    pool: &SqlitePool,
    run_id: &str,
    failure: MonitorFailure,
) -> Result<(), M1Error> {
    let finished = Utc::now();
    let mut tx = pool.begin().await.map_err(M1Error::Storage)?;
    let host_id: String = sqlx::query_scalar("SELECT host_id FROM monitor_runs WHERE run_id = ?")
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(M1Error::Storage)?;
    let result = sqlx::query(
        "UPDATE monitor_runs SET state = ?, failure_code = ?, failure_summary = ?,
            output_bytes = ?, ssh_session_count = ?, finished_at = ?
         WHERE run_id = ? AND state = 'running'",
    )
    .bind(monitor_state_name(&failure.state))
    .bind(failure.code)
    .bind(failure.summary)
    .bind(i64::try_from(failure.output_bytes).unwrap_or(i64::MAX))
    .bind(i64::from(failure.ssh_session_count))
    .bind(finished.to_rfc3339_opts(SecondsFormat::Millis, true))
    .bind(run_id)
    .execute(&mut *tx)
    .await
    .map_err(M1Error::Storage)?;
    if result.rows_affected() == 1 {
        monitoring_health::evaluate_run_tx(
            &mut tx,
            run_id,
            &host_id,
            EvaluationTiming {
                evaluated_at: finished,
                observation_state: HealthObservationState::None,
                observed_at: None,
                valid_until: None,
            },
        )
        .await
        .map_err(M1Error::Storage)?;
    }
    tx.commit().await.map_err(M1Error::Storage)
}

async fn replay_request(
    pool: &SqlitePool,
    host_id: &str,
    key: &str,
    request_sha256: &str,
) -> Result<Option<MonitorRunAcceptedResponse>, M1Error> {
    let row = sqlx::query(
        "SELECT request_sha256, accepted_response_json FROM monitor_runs
         WHERE host_id = ? AND idempotency_key = ?",
    )
    .bind(host_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    row.map(|row| {
        let recorded: String = row.try_get("request_sha256").map_err(M1Error::Storage)?;
        if recorded != request_sha256 {
            return Err(M1Error::Conflict {
                code: "IDEMPOTENCY_KEY_REUSED",
                message: "Idempotency-Key 已用于不同的资源采集请求",
                details: json!({"host_id": host_id}),
            });
        }
        let payload: String = row
            .try_get("accepted_response_json")
            .map_err(M1Error::Storage)?;
        serde_json::from_str(&payload).map_err(|_| M1Error::Internal)
    })
    .transpose()
}

async fn load_monitor_run(
    pool: &SqlitePool,
    run_id: &str,
) -> Result<Option<MonitorRunRecord>, M1Error> {
    let row = sqlx::query(
        "SELECT request_id, run_id, host_id, profile, trigger_kind, state,
                schedule_id, schedule_revision, scheduled_for, stale_after_seconds,
                due_interval_seconds, missed_due_count, collector_version,
                boot_id, coverage_json, output_bytes,
                ssh_session_count, failure_code, failure_summary,
                submitted_at, started_at, finished_at
         FROM monitor_runs WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(M1Error::Storage)?;
    row.map(monitor_record_from_row).transpose()
}

fn monitor_record_from_row(row: sqlx::sqlite::SqliteRow) -> Result<MonitorRunRecord, M1Error> {
    let coverage_json: String = row.try_get("coverage_json").map_err(M1Error::Storage)?;
    let coverage = serde_json::from_str(&coverage_json).unwrap_or_default();
    let output_bytes: i64 = row.try_get("output_bytes").map_err(M1Error::Storage)?;
    let ssh_session_count: i64 = row.try_get("ssh_session_count").map_err(M1Error::Storage)?;
    let state_value: String = row.try_get("state").map_err(M1Error::Storage)?;
    Ok(MonitorRunRecord {
        request_id: row.try_get("request_id").map_err(M1Error::Storage)?,
        run_id: row.try_get("run_id").map_err(M1Error::Storage)?,
        host_id: row.try_get("host_id").map_err(M1Error::Storage)?,
        profile: HostMonitorProfile::HostResourceV1,
        trigger_kind: parse_monitor_trigger(
            &row.try_get::<String, _>("trigger_kind")
                .map_err(M1Error::Storage)?,
        ),
        state: parse_monitor_state(&state_value),
        schedule_id: row.try_get("schedule_id").map_err(M1Error::Storage)?,
        schedule_revision: row.try_get("schedule_revision").map_err(M1Error::Storage)?,
        scheduled_for: row.try_get("scheduled_for").map_err(M1Error::Storage)?,
        stale_after_seconds: u32::try_from(
            row.try_get::<i64, _>("stale_after_seconds")
                .map_err(M1Error::Storage)?
                .max(0),
        )
        .unwrap_or(u32::MAX),
        due_interval_seconds: row
            .try_get::<Option<i64>, _>("due_interval_seconds")
            .map_err(M1Error::Storage)?
            .map(|value| u32::try_from(value.max(0)).unwrap_or(u32::MAX)),
        missed_due_count: row
            .try_get::<Option<i64>, _>("missed_due_count")
            .map_err(M1Error::Storage)?
            .map(|value| u32::try_from(value.max(0)).unwrap_or(u32::MAX)),
        collector_version: row.try_get("collector_version").map_err(M1Error::Storage)?,
        boot_id: row.try_get("boot_id").map_err(M1Error::Storage)?,
        coverage,
        output_bytes: u64::try_from(output_bytes.max(0)).unwrap_or(u64::MAX),
        ssh_session_count: u32::try_from(ssh_session_count.max(0)).unwrap_or(u32::MAX),
        failure_code: row.try_get("failure_code").map_err(M1Error::Storage)?,
        failure_summary: row.try_get("failure_summary").map_err(M1Error::Storage)?,
        submitted_at: row.try_get("submitted_at").map_err(M1Error::Storage)?,
        started_at: row.try_get("started_at").map_err(M1Error::Storage)?,
        finished_at: row.try_get("finished_at").map_err(M1Error::Storage)?,
    })
}

async fn load_current_snapshot(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Option<HostResourceSnapshot>, M1Error> {
    let row =
        sqlx::query("SELECT snapshot_json, valid_until FROM monitoring_current WHERE host_id = ?")
            .bind(host_id)
            .fetch_optional(pool)
            .await
            .map_err(M1Error::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payload: String = row.try_get("snapshot_json").map_err(M1Error::Storage)?;
    let valid_until: String = row.try_get("valid_until").map_err(M1Error::Storage)?;
    let Ok(mut snapshot) = serde_json::from_str::<HostResourceSnapshot>(&payload) else {
        tracing::warn!(%host_id, "ignored invalid current monitor snapshot");
        return Ok(None);
    };
    snapshot.freshness = freshness_from_valid_until(&valid_until);
    Ok(Some(snapshot))
}

async fn ensure_host_exists(pool: &SqlitePool, host_id: &str) -> Result<(), M1Error> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .fetch_optional(pool)
        .await
        .map_err(M1Error::Storage)?;
    if exists.is_none() {
        return Err(M1Error::NotFound {
            resource: "host",
            id: host_id.to_owned(),
        });
    }
    Ok(())
}

fn snapshot_from_observation(
    observation_id: String,
    run_id: String,
    host_id: String,
    observed_at: String,
    valid_until: String,
    observation: &HostResourceObservation,
) -> HostResourceSnapshot {
    let coverage = vec![
        family_coverage("cpu", true, &observation.cpu, 1),
        family_coverage("memory", true, &observation.memory, 1),
        family_coverage("load", true, &observation.load, 1),
        family_coverage(
            "disk_capacity",
            true,
            &observation.disk_capacity,
            observation
                .disk_capacity
                .value
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
        ),
        family_coverage(
            "disk_io",
            false,
            &observation.disk_io,
            observation
                .disk_io
                .value
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
        ),
        family_coverage(
            "network",
            false,
            &observation.network,
            observation
                .network
                .value
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
        ),
        family_coverage("uptime", true, &observation.uptime, 1),
        family_coverage("process", false, &observation.process, 1),
    ];
    let metric_count = coverage
        .iter()
        .map(|family| family.observed_item_count)
        .fold(0u32, u32::saturating_add);
    let unknown_count = u32::try_from(
        coverage
            .iter()
            .filter(|family| family.quality != MonitorMetricQuality::Observed)
            .count(),
    )
    .unwrap_or(u32::MAX);

    let cpu = observation.cpu.value.as_ref();
    let memory = observation.memory.value.as_ref();
    let load = observation.load.value.as_ref();
    let uptime = observation.uptime.value.as_ref();
    let process = observation.process.value.as_ref();
    HostResourceSnapshot {
        observation_id,
        run_id,
        host_id,
        profile: HostMonitorProfile::HostResourceV1,
        collector_version: COLLECTOR_VERSION.to_owned(),
        observed_at,
        valid_until,
        freshness: MonitorFreshness::Fresh,
        retention_tier: "full".to_owned(),
        ssh_session_count: 1,
        coverage,
        metric_count,
        unknown_count,
        cpu: HostCpuMetric {
            quality: quality(observation.cpu.quality),
            busy_percent: cpu.map(|value| value.aggregate.busy_pct),
            iowait_percent: cpu.map(|value| value.aggregate.iowait_pct),
            steal_percent: cpu.map(|value| value.aggregate.steal_pct),
            online_cpu_count: cpu.and_then(|value| value.online_cpu_count),
            window_seconds: cpu.map(|value| value.window_seconds),
            per_cpu: cpu
                .map(|value| {
                    value
                        .per_cpu
                        .iter()
                        .map(|core| HostCpuCoreMetric {
                            cpu: core.cpu.clone(),
                            quality: quality(core.quality),
                            busy_percent: core.rate.map(|rate| rate.busy_pct),
                            iowait_percent: core.rate.map(|rate| rate.iowait_pct),
                            steal_percent: core.rate.map(|rate| rate.steal_pct),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        memory: HostMemoryMetric {
            quality: quality(observation.memory.quality),
            total_bytes: memory.map(|value| kib_to_bytes(value.total_kib)),
            available_bytes: memory.and_then(|value| value.available_kib.map(kib_to_bytes)),
            used_bytes: memory.and_then(|value| value.used_kib.map(kib_to_bytes)),
            swap_total_bytes: memory.and_then(|value| value.swap_total_kib.map(kib_to_bytes)),
            swap_free_bytes: memory.and_then(|value| value.swap_free_kib.map(kib_to_bytes)),
            cached_bytes: memory.and_then(|value| value.cached_kib.map(kib_to_bytes)),
            buffers_bytes: memory.and_then(|value| value.buffers_kib.map(kib_to_bytes)),
            slab_bytes: memory.and_then(|value| value.slab_kib.map(kib_to_bytes)),
        },
        load: HostLoadMetric {
            quality: quality(observation.load.quality),
            load1: load.map(|value| value.load1),
            load5: load.map(|value| value.load5),
            load15: load.map(|value| value.load15),
            normalized_load1: load.and_then(|value| value.normalized_load1),
            normalized_load5: load.and_then(|value| value.normalized_load5),
            normalized_load15: load.and_then(|value| value.normalized_load15),
            runnable_entities: load.map(|value| value.runnable_entities),
            total_scheduling_entities: load.map(|value| value.total_scheduling_entities),
            online_cpu_count: load.and_then(|value| value.online_cpu_count),
        },
        filesystems: observation
            .disk_capacity
            .value
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .map(|item| HostFilesystemMetric {
                        mount: item.mount.clone(),
                        size_bytes: kib_to_bytes(item.size_kib),
                        used_bytes: kib_to_bytes(item.used_kib),
                        available_bytes: kib_to_bytes(item.available_kib),
                        allocatable_used_ratio: item.allocatable_used_ratio,
                        inode_total: item.inode_total,
                        inode_used: item.inode_used,
                        inode_available: item.inode_available,
                        inode_used_ratio: item.inode_used_ratio,
                        inode_quality: quality(item.inode_quality),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        disk_io: observation
            .disk_io
            .value
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .map(|item| HostDiskIoMetric {
                        identity: item.identity.clone(),
                        name: item.name.clone(),
                        major: item.major,
                        minor: item.minor,
                        quality: quality(item.quality),
                        window_seconds: item.window_seconds,
                        read_bytes_per_second: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.read_bytes_per_second),
                        write_bytes_per_second: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.write_bytes_per_second),
                        iops: item.metrics.as_ref().map(|metric| metric.iops),
                        read_await_ms: item
                            .metrics
                            .as_ref()
                            .and_then(|metric| metric.read_await_ms),
                        write_await_ms: item
                            .metrics
                            .as_ref()
                            .and_then(|metric| metric.write_await_ms),
                        util_percent: item.metrics.as_ref().map(|metric| metric.util_pct),
                        average_queue_depth: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.average_queue_depth),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        network: observation
            .network
            .value
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .map(|item| HostNetworkMetric {
                        identity: item.identity.clone(),
                        name: item.name.clone(),
                        ifindex: item.ifindex,
                        iflink: item.iflink,
                        operstate: item.operstate.clone(),
                        speed_mbps: item.speed_mbps,
                        quality: quality(item.quality),
                        window_seconds: item.window_seconds,
                        rx_bytes_per_second: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.rx_bytes_per_second),
                        tx_bytes_per_second: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.tx_bytes_per_second),
                        rx_packets_per_second: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.rx_packets_per_second),
                        tx_packets_per_second: item
                            .metrics
                            .as_ref()
                            .map(|metric| metric.tx_packets_per_second),
                        rx_error_drop_percent: item
                            .metrics
                            .as_ref()
                            .and_then(|metric| metric.rx_error_drop_pct),
                        tx_error_drop_percent: item
                            .metrics
                            .as_ref()
                            .and_then(|metric| metric.tx_error_drop_pct),
                        rx_util_percent: item
                            .metrics
                            .as_ref()
                            .and_then(|metric| metric.rx_util_pct),
                        tx_util_percent: item
                            .metrics
                            .as_ref()
                            .and_then(|metric| metric.tx_util_pct),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        uptime: HostUptimeMetric {
            quality: quality(observation.uptime.quality),
            boot_id: uptime.map(|value| value.boot_id.clone()),
            uptime_seconds: uptime.map(|value| value.uptime_seconds),
            idle_seconds: None,
            rebooted_during_sample: uptime.and_then(|value| value.rebooted_since_frame_a),
            window_seconds: cpu.map(|value| value.window_seconds),
        },
        process: HostProcessMetric {
            quality: quality(observation.process.quality),
            scanned: process.map(|value| value.scanned),
            running: process.map(|value| value.running),
            blocked: process.map(|value| value.blocked),
            zombie: process.map(|value| value.zombie),
            raced: process.map(|value| value.raced),
            truncated: process.map(|value| value.truncated),
        },
    }
}

fn family_coverage<T>(
    family: &str,
    required: bool,
    observation: &FamilyObservation<T>,
    item_count: usize,
) -> MonitorFamilyCoverage {
    MonitorFamilyCoverage {
        family: family.to_owned(),
        required,
        quality: quality(observation.quality),
        observed_item_count: if observation.value.is_some() {
            u32::try_from(item_count).unwrap_or(u32::MAX)
        } else {
            0
        },
    }
}

fn quality(value: MetricQuality) -> MonitorMetricQuality {
    match value {
        MetricQuality::Observed => MonitorMetricQuality::Observed,
        MetricQuality::Unsupported => MonitorMetricQuality::Unsupported,
        MetricQuality::ParseFailed => MonitorMetricQuality::ParseFailed,
        MetricQuality::CounterReset => MonitorMetricQuality::CounterReset,
        MetricQuality::CounterUnreliable => MonitorMetricQuality::CounterUnreliable,
        MetricQuality::InsufficientInterval => MonitorMetricQuality::InsufficientInterval,
        MetricQuality::PermissionDenied => MonitorMetricQuality::PermissionDenied,
        MetricQuality::TimedOut => MonitorMetricQuality::TimedOut,
    }
}

fn kib_to_bytes(value: u64) -> u64 {
    value.saturating_mul(1024)
}

fn request_digest(request: &MonitorRunCreateRequest) -> Result<String, M1Error> {
    let payload = serde_json::to_vec(request).map_err(|_| M1Error::Internal)?;
    Ok(hex_digest(&Sha256::digest(payload)))
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
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .ok_or_else(|| M1Error::BadRequest {
            code: "IDEMPOTENCY_KEY_REQUIRED",
            message: "该操作需要 Idempotency-Key",
            details: json!({}),
        })
}

fn real_meta(request_id: impl Into<String>, freshness: Freshness) -> ApiMeta {
    let status = match freshness {
        Freshness::Fresh => DataSourceStatus::Fresh,
        Freshness::Stale => DataSourceStatus::Stale,
        Freshness::Unavailable => DataSourceStatus::Unavailable,
    };
    ApiMeta {
        request_id: request_id.into(),
        revision: 1,
        generated_at: now(),
        freshness,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status,
            label: "SSH · Linux · 当前资源快照".to_owned(),
        },
    }
}

fn freshness_from_valid_until(valid_until: &str) -> MonitorFreshness {
    DateTime::parse_from_rfc3339(valid_until)
        .ok()
        .map(|value| {
            if value.with_timezone(&Utc) >= Utc::now() {
                MonitorFreshness::Fresh
            } else {
                MonitorFreshness::Stale
            }
        })
        .unwrap_or(MonitorFreshness::Unknown)
}

fn api_freshness(freshness: &MonitorFreshness) -> Freshness {
    match freshness {
        MonitorFreshness::Fresh => Freshness::Fresh,
        MonitorFreshness::Stale => Freshness::Stale,
        MonitorFreshness::Unknown => Freshness::Unavailable,
    }
}

fn run_freshness(state: &MonitorRunState) -> Freshness {
    match state {
        MonitorRunState::Succeeded | MonitorRunState::Partial => Freshness::Fresh,
        MonitorRunState::Queued | MonitorRunState::Running => Freshness::Stale,
        _ => Freshness::Unavailable,
    }
}

fn parse_monitor_state(value: &str) -> MonitorRunState {
    match value {
        "queued" => MonitorRunState::Queued,
        "running" => MonitorRunState::Running,
        "succeeded" => MonitorRunState::Succeeded,
        "partial" => MonitorRunState::Partial,
        "timed_out" => MonitorRunState::TimedOut,
        "skipped_overlap" => MonitorRunState::SkippedOverlap,
        "interrupted" => MonitorRunState::Interrupted,
        _ => MonitorRunState::Failed,
    }
}

fn parse_monitor_trigger(value: &str) -> MonitorRunTrigger {
    match value {
        "scheduled" => MonitorRunTrigger::Scheduled,
        "catch_up" => MonitorRunTrigger::CatchUp,
        _ => MonitorRunTrigger::Manual,
    }
}

fn monitor_state_name(value: &MonitorRunState) -> &'static str {
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

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        api::AppState,
        ssh::{SshCredential, SshTarget},
        storage,
    };

    #[tokio::test]
    async fn queued_manual_monitor_does_not_start_ssh_after_shutdown() {
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
             ) VALUES ('host-queued-shutdown', 'workspace-default', 'fixture',
                'fixture.invalid', 22, 'fixture', 'secret-ref-fixture', 'verified',
                'ssh', 'linux', 'connection_ready', '2026-08-15T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, submitted_at,
                accepted_response_json
             ) VALUES ('run-queued-shutdown', 'host-queued-shutdown', 'request',
                'key', 'digest', 'host_resource_v1', 'manual', 'queued', 900,
                '2026-08-15T00:00:00Z', '{}')",
        )
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
        let worker = tokio::spawn(run_manual_monitor_when_permitted(
            state.clone(),
            "run-queued-shutdown".to_owned(),
            "host-queued-shutdown".to_owned(),
            SshTarget {
                host_id: "host-queued-shutdown".to_owned(),
                address: "fixture.invalid".to_owned(),
                port: 22,
                user: "fixture".to_owned(),
                credential: SshCredential::PrivateKey(PathBuf::from("unused")),
            },
        ));
        state.begin_shutdown();
        drop(held);
        worker.await.unwrap();

        let row = sqlx::query(
            "SELECT state, failure_code, ssh_session_count
             FROM monitor_runs WHERE run_id = 'run-queued-shutdown'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("state"), "interrupted");
        assert_eq!(
            row.get::<Option<String>, _>("failure_code").as_deref(),
            Some("MONITOR_INTERRUPTED")
        );
        assert_eq!(row.get::<i64, _>("ssh_session_count"), 0);
    }
}
