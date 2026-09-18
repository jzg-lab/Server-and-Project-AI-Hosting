//! H3c black-box contract tests.
//!
//! These tests intentionally exercise the HTTP read/write surface and the
//! persisted receipts rather than private evaluator helpers.  Direct SQLite
//! fixtures represent terminal collector receipts that a remote SSH fixture
//! would otherwise have to produce.

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header::CONTENT_TYPE},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use http_body_util::BodyExt;
use network_atlas::{api, api::AppState, monitoring_api, storage};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const WORKSPACE_ID: &str = "workspace-default";
const NOW_TEXT: &str = "2026-08-15T00:00:00.000Z";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn seed_host(pool: &SqlitePool, host_id: &str) {
    sqlx::query(
        "INSERT OR IGNORE INTO workspaces(workspace_id, owner_id, created_at)
         VALUES (?, 'owner-local', ?)",
    )
    .bind(WORKSPACE_ID)
    .bind(NOW_TEXT)
    .execute(pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
             host_id, workspace_id, display_name, address, port, ssh_user,
             credential_ref, host_key_state, host_key_fingerprint, transport,
             os, status, created_at
         ) VALUES (?, ?, ?, 'fixture.invalid', 22, 'fixture',
             'secret-ref-fixture', 'verified', 'SHA256:fixture', 'ssh', 'linux',
             'connection_ready', ?)",
    )
    .bind(host_id)
    .bind(WORKSPACE_ID)
    .bind(host_id)
    .bind(NOW_TEXT)
    .execute(pool)
    .await
    .expect("host");
}

fn test_app(pool: SqlitePool, frontend: &TempDir) -> Router {
    let state = AppState::new(pool);
    api::router(state, frontend.path())
}

async fn request_json(
    app: Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    request_body(app, method, uri, headers, body.to_string()).await
}

async fn request_body(
    app: Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: String,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .oneshot(builder.body(Body::from(body)).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let payload = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    (status, headers, payload)
}

fn policy(enabled: bool, warning: f64) -> Value {
    json!({
        "enabled": enabled,
        "cpu_busy": {
            "requirement": "required",
            "series_kind": "collector_window",
            "minimum_window_seconds": 0.5,
            "warning_at_or_above": warning,
            "critical_at_or_above": 95.0,
            "recovery_below": 75.0,
            "enter_count": 2,
            "recover_count": 2
        },
        "memory_available_ratio": null,
        "normalized_load5": null,
        "filesystems": []
    })
}

async fn current_policy(pool: &SqlitePool, host_id: &str) -> (String, String, i64) {
    sqlx::query(
        "SELECT policy_version_id, policy_id, revision
         FROM health_policy_versions
         WHERE host_id = ? AND lifecycle_state = 'current'",
    )
    .bind(host_id)
    .fetch_one(pool)
    .await
    .map(|row| {
        (
            row.get("policy_version_id"),
            row.get("policy_id"),
            row.get("revision"),
        )
    })
    .expect("current health policy")
}

fn ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn seed_schedule(
    pool: &SqlitePool,
    host_id: &str,
    schedule_id: &str,
    due_from: DateTime<Utc>,
) -> String {
    let version_id = Uuid::new_v4().to_string();
    let due = ts(due_from);
    sqlx::query(
        "INSERT INTO monitor_schedules(
             schedule_id, host_id, profile, interval_seconds, jitter_seconds,
             jitter_offset_seconds, stale_after_seconds, state, next_due_at,
             revision, create_idempotency_key, create_request_sha256,
             created_response_json, created_at, updated_at
         ) VALUES (?, ?, 'host_resource_v1', 300, 0, 0, 900, 'enabled', ?, 1,
             ?, ?, '{}', ?, ?)",
    )
    .bind(schedule_id)
    .bind(host_id)
    .bind(&due)
    .bind(format!("create-{schedule_id}"))
    .bind(DIGEST)
    .bind(&due)
    .bind(&due)
    .execute(pool)
    .await
    .expect("schedule");
    sqlx::query(
        "INSERT INTO monitor_schedule_versions(
             schedule_version_id, schedule_id, host_id, revision, profile,
             interval_seconds, jitter_seconds, jitter_offset_seconds,
             stale_after_seconds, state, due_from_at, due_from_at_epoch_ms,
             effective_from_at, effective_from_at_epoch_ms, provenance_kind,
             activated_at, created_at
         ) VALUES (?, ?, ?, 1, 'host_resource_v1', 300, 0, 0, 900,
             'enabled', ?, ?, ?, ?, 'recorded', ?, ?)",
    )
    .bind(&version_id)
    .bind(schedule_id)
    .bind(host_id)
    .bind(&due)
    .bind(due_from.timestamp_millis())
    .bind(&due)
    .bind(due_from.timestamp_millis())
    .bind(&due)
    .bind(&due)
    .execute(pool)
    .await
    .expect("schedule version");
    version_id
}

#[allow(clippy::too_many_arguments)]
async fn seed_run(
    pool: &SqlitePool,
    host_id: &str,
    run_id: &str,
    state: &str,
    trigger: &str,
    policy_version_id: Option<&str>,
    schedule_id: Option<&str>,
    schedule_version_id: Option<&str>,
    scheduled_for: Option<DateTime<Utc>>,
    missed_due_count: i64,
    at: DateTime<Utc>,
) {
    let scheduled_text = scheduled_for.map(ts);
    sqlx::query(
        "INSERT INTO monitor_runs(
             run_id, host_id, request_id, idempotency_key, request_sha256,
             profile, trigger_kind, state, schedule_id, schedule_revision,
             schedule_version_id, health_policy_version_id, scheduled_for,
             stale_after_seconds, due_interval_seconds, missed_due_count,
             submitted_at, started_at, finished_at, accepted_response_json
         ) VALUES (?, ?, ?, ?, ?, 'host_resource_v1', ?, ?, ?,
             CASE WHEN ? IS NULL THEN NULL ELSE 1 END, ?, ?, ?, 900, 300, ?, ?, ?, ?, '{}')",
    )
    .bind(run_id)
    .bind(host_id)
    .bind(format!("request-{run_id}"))
    .bind(format!("key-{run_id}"))
    .bind(DIGEST)
    .bind(trigger)
    .bind(state)
    .bind(schedule_id)
    .bind(schedule_id)
    .bind(schedule_version_id)
    .bind(policy_version_id)
    .bind(scheduled_text)
    .bind(missed_due_count)
    .bind(ts(at))
    .bind(ts(at))
    .bind(ts(at))
    .execute(pool)
    .await
    .expect("monitor run");
}

#[allow(clippy::too_many_arguments)]
async fn seed_evaluation(
    pool: &SqlitePool,
    host_id: &str,
    run_id: &str,
    evaluation_id: &str,
    policy: Option<(&str, &str, i64)>,
    status: &str,
    observation_state: &str,
    evaluated_at: DateTime<Utc>,
    valid_until: Option<DateTime<Utc>>,
) {
    let observed = valid_until.map(|_| ts(evaluated_at));
    let observed_epoch = valid_until.map(|_| evaluated_at.timestamp_millis());
    let valid_text = valid_until.map(ts);
    let valid_epoch = valid_until.map(|value| value.timestamp_millis());
    let has_condition = policy.is_some() && status == "healthy";
    let (ok_count, unknown_count, required_count) =
        if has_condition { (1, 0, 1) } else { (0, 0, 0) };
    sqlx::query(
        "INSERT INTO health_evaluations(
             evaluation_id, run_id, host_id, policy_version_id, policy_id,
             policy_revision, status, reason_code, required_condition_count,
             optional_condition_count, ok_count, warning_count, critical_count,
             unknown_count, observation_state, input_sha256, evaluated_at,
             evaluated_at_epoch_ms, observed_at, observed_at_epoch_ms,
             valid_until, valid_until_epoch_ms, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, 0, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(evaluation_id)
    .bind(run_id)
    .bind(host_id)
    .bind(policy.map(|value| value.0))
    .bind(policy.map(|value| value.1))
    .bind(policy.map(|value| value.2))
    .bind(status)
    .bind(if status == "healthy" {
        "all_conditions_ok"
    } else {
        "required_unknown"
    })
    .bind(required_count)
    .bind(ok_count)
    .bind(unknown_count)
    .bind(observation_state)
    .bind(DIGEST)
    .bind(ts(evaluated_at))
    .bind(evaluated_at.timestamp_millis())
    .bind(observed)
    .bind(observed_epoch)
    .bind(valid_text)
    .bind(valid_epoch)
    .bind(ts(evaluated_at))
    .execute(pool)
    .await
    .expect("health evaluation");
    if has_condition {
        sqlx::query(
            "INSERT INTO health_condition_evaluations(
                 condition_evaluation_id, evaluation_id, condition_key,
                 condition_kind, requirement, subject_kind, subject_id,
                 subject_label, status, candidate_status, reason_code,
                 value_real, unit, window_seconds, streak_count, streak_required,
                 evidence_refs_json, input_sha256, created_at
             ) VALUES (?, ?, 'cpu_busy', 'cpu_busy_instant_percent', 'required',
                 'host', ?, 'CPU busy', 'ok', 'ok', 'ok', 10.0, 'percent',
                 1.0, 1, 2, '[]', ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(evaluation_id)
        .bind(host_id)
        .bind(DIGEST)
        .bind(ts(evaluated_at))
        .execute(pool)
        .await
        .expect("health condition");
    }
}

async fn seed_metric_sample(pool: &SqlitePool, host_id: &str, run_id: &str, at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO metric_samples(
             sample_id, run_id, host_id, family, subject_kind, subject_id,
             metric_name, dimensions_json, dimensions_sha256, sample_kind,
             value_real, unit, window_seconds, quality, observed_at,
             observed_at_epoch_ms, source_kind, created_at
         ) VALUES (?, ?, ?, 'cpu', 'host', ?, 'busy_pct', '{}', ?, 'derived',
             50.0, 'percent', 1.0, 'observed', ?, ?,
             'ssh_host_resource_v1', ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(run_id)
    .bind(host_id)
    .bind(host_id)
    .bind(DIGEST)
    .bind(ts(at))
    .bind(at.timestamp_millis())
    .bind(ts(at))
    .execute(pool)
    .await
    .expect("metric sample");
}

#[tokio::test]
async fn health_policy_get_put_revision_and_typed_validation() {
    let pool = storage::connect("sqlite::memory:").await.expect("db");
    seed_host(&pool, "health-policy").await;
    let frontend = TempDir::new().expect("frontend");
    let app = test_app(pool.clone(), &frontend);

    let (status, headers, body) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-policy/health-policy",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["configured"], false);
    assert_eq!(body["data"]["state"], "not_configured");
    assert_eq!(
        headers.get("etag").and_then(|value| value.to_str().ok()),
        Some("\"revision-0\"")
    );
    let (status, _, health) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-policy/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["data"]["current"]["health"], "unknown");
    assert_eq!(health["data"]["current"]["reason_code"], "not_configured");

    let payload = policy(true, 80.0);
    let (status, headers, body) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-0\"")],
        payload.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["policy"]["revision"], 1);
    assert_eq!(
        headers.get("etag").and_then(|value| value.to_str().ok()),
        Some("\"revision-1\"")
    );

    let (status, _, replay) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-1\"")],
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"]["policy"]["revision"], 1);
    let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM health_policy_versions")
        .fetch_one(&pool)
        .await
        .expect("policy versions");
    assert_eq!(versions, 1, "same hash must be idempotent");

    let (status, _, stale) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-0\"")],
        policy(true, 81.0),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{stale}");
    assert_eq!(stale["error"]["code"], "REVISION_MISMATCH");

    let (status, _, unknown_field) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-1\"")],
        json!({"enabled": true, "filesystems": [], "command": "cat /proc"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown_field}");

    let mut all_optional = policy(true, 80.0);
    all_optional["cpu_busy"]["requirement"] = json!("optional");
    let (status, _, optional_only) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-1\"")],
        all_optional,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an enabled policy needs at least one required rule: {optional_only}"
    );

    let (status, _, empty) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-1\"")],
        json!({"enabled": true, "filesystems": []}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{empty}");

    let mut exact_bytes = policy(true, 80.0);
    exact_bytes["filesystems"] = json!([{
        "mount": "/",
        "requirement": "required",
        "warning_at_or_above": 0.8,
        "critical_at_or_above": 0.9,
        "recovery_below": 0.7,
        "critical_available_bytes_below": 9007199254740992u64,
        "enter_count": 1,
        "recover_count": 1
    }]);
    let (status, _, exact) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-1\"")],
        exact_bytes.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "2^53 is exactly representable: {exact}"
    );
    assert_eq!(exact["data"]["policy"]["revision"], 2);

    exact_bytes["filesystems"][0]["critical_available_bytes_below"] = json!(9007199254740993u64);
    let (status, _, rounded) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-2\"")],
        exact_bytes,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rounded}");

    let invalid_nan = r#"{
        "enabled": true,
        "cpu_busy": {
            "requirement": "required",
            "series_kind": "collector_window",
            "minimum_window_seconds": 0.5,
            "warning_at_or_above": NaN,
            "critical_at_or_above": 95.0,
            "recovery_below": 75.0,
            "enter_count": 2,
            "recover_count": 2
        },
        "filesystems": []
    }"#
    .to_owned();
    let (status, _, invalid) = request_body(
        app,
        Method::PUT,
        "/api/v1/hosts/health-policy/health-policy",
        &[("if-match", "\"revision-2\"")],
        invalid_nan,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
}

#[tokio::test]
async fn health_receipts_survive_metric_sample_retention() {
    let pool = storage::connect("sqlite::memory:").await.expect("db");
    seed_host(&pool, "health-retention").await;
    let frontend = TempDir::new().expect("frontend");
    let app = test_app(pool.clone(), &frontend);
    let (_, _, put) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-retention/health-policy",
        &[("if-match", "\"revision-0\"")],
        policy(true, 80.0),
    )
    .await;
    assert_eq!(put["data"]["policy"]["revision"], 1, "{put}");
    let (version_id, policy_id, revision) = current_policy(&pool, "health-retention").await;
    let at = Utc::now();
    seed_run(
        &pool,
        "health-retention",
        "run-retention",
        "succeeded",
        "manual",
        Some(&version_id),
        None,
        None,
        None,
        0,
        at,
    )
    .await;
    seed_metric_sample(&pool, "health-retention", "run-retention", at).await;
    seed_evaluation(
        &pool,
        "health-retention",
        "run-retention",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "healthy",
        "complete",
        at,
        Some(at + Duration::minutes(10)),
    )
    .await;
    let (_, _, before) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-retention/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    let evaluation_id = before["data"]["current"]["evaluation_id"].clone();
    let conditions = before["data"]["current"]["conditions"].clone();
    assert_eq!(before["data"]["current"]["health"], "healthy", "{before}");

    sqlx::query("DELETE FROM metric_samples WHERE run_id = 'run-retention'")
        .execute(&pool)
        .await
        .expect("retention delete");
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples WHERE run_id = 'run-retention'")
            .fetch_one(&pool)
            .await
            .expect("remaining samples");
    assert_eq!(remaining, 0);
    let (_, _, after) = request_json(
        app,
        Method::GET,
        "/api/v1/hosts/health-retention/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(after["data"]["current"]["evaluation_id"], evaluation_id);
    assert_eq!(after["data"]["current"]["conditions"], conditions);
    assert_eq!(
        after["data"]["window"]["buckets"].as_array().map(Vec::len),
        Some(24)
    );
}

#[tokio::test]
async fn policy_switch_awaits_new_evaluation_and_disabled_is_unknown() {
    let pool = storage::connect("sqlite::memory:").await.expect("db");
    seed_host(&pool, "health-switch").await;
    let frontend = TempDir::new().expect("frontend");
    let app = test_app(pool.clone(), &frontend);
    let (status, _, put) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-switch/health-policy",
        &[("if-match", "\"revision-0\"")],
        policy(true, 80.0),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put}");
    let (version_id, policy_id, revision) = current_policy(&pool, "health-switch").await;
    let evaluated_at = Utc::now();
    seed_run(
        &pool,
        "health-switch",
        "run-switch-healthy",
        "succeeded",
        "manual",
        Some(&version_id),
        None,
        None,
        None,
        0,
        evaluated_at,
    )
    .await;
    seed_evaluation(
        &pool,
        "health-switch",
        "run-switch-healthy",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "healthy",
        "complete",
        evaluated_at,
        Some(evaluated_at + Duration::minutes(10)),
    )
    .await;
    let (_, _, healthy) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-switch/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(healthy["data"]["current"]["health"], "healthy", "{healthy}");

    let (status, _, switched) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-switch/health-policy",
        &[("if-match", "\"revision-1\"")],
        policy(true, 81.0),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    let (_, _, awaiting) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-switch/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(awaiting["data"]["current"]["health"], "unknown");
    assert_eq!(
        awaiting["data"]["current"]["reason_code"],
        "awaiting_policy_evaluation"
    );

    let (status, _, disabled) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-switch/health-policy",
        &[("if-match", "\"revision-2\"")],
        policy(false, 81.0),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let (_, _, disabled_health) = request_json(
        app,
        Method::GET,
        "/api/v1/hosts/health-switch/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(disabled_health["data"]["current"]["health"], "unknown");
    assert_eq!(
        disabled_health["data"]["current"]["reason_code"],
        "policy_disabled"
    );
}

#[tokio::test]
async fn failed_interrupted_and_stale_receipts_never_keep_green_health() {
    let pool = storage::connect("sqlite::memory:").await.expect("db");
    seed_host(&pool, "health-terminal").await;
    let frontend = TempDir::new().expect("frontend");
    let app = test_app(pool.clone(), &frontend);
    let (_, _, _) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-terminal/health-policy",
        &[("if-match", "\"revision-0\"")],
        policy(true, 80.0),
    )
    .await;
    let (version_id, policy_id, revision) = current_policy(&pool, "health-terminal").await;
    let at = Utc::now();
    seed_run(
        &pool,
        "health-terminal",
        "run-terminal-failed",
        "failed",
        "manual",
        Some(&version_id),
        None,
        None,
        None,
        0,
        at,
    )
    .await;
    seed_evaluation(
        &pool,
        "health-terminal",
        "run-terminal-failed",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "healthy",
        "complete",
        at,
        Some(at + Duration::minutes(10)),
    )
    .await;
    let (_, _, failed) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-terminal/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(failed["data"]["current"]["health"], "unknown", "{failed}");

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let skipped_at = Utc::now();
    let schedule_version_id = seed_schedule(
        &pool,
        "health-terminal",
        "schedule-terminal-skip",
        skipped_at - Duration::minutes(5),
    )
    .await;
    seed_run(
        &pool,
        "health-terminal",
        "run-terminal-skipped",
        "skipped_overlap",
        "scheduled",
        Some(&version_id),
        Some("schedule-terminal-skip"),
        Some(&schedule_version_id),
        Some(skipped_at),
        0,
        skipped_at,
    )
    .await;
    seed_evaluation(
        &pool,
        "health-terminal",
        "run-terminal-skipped",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "unknown",
        "none",
        skipped_at,
        None,
    )
    .await;
    let (_, _, skipped) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/health-terminal/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(skipped["data"]["current"]["health"], "unknown", "{skipped}");

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let stale_at = Utc::now();
    seed_run(
        &pool,
        "health-terminal",
        "run-terminal-stale",
        "succeeded",
        "manual",
        Some(&version_id),
        None,
        None,
        None,
        0,
        stale_at,
    )
    .await;
    seed_evaluation(
        &pool,
        "health-terminal",
        "run-terminal-stale",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "healthy",
        "complete",
        stale_at,
        Some(stale_at + Duration::milliseconds(1)),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let (_, _, stale) = request_json(
        app,
        Method::GET,
        "/api/v1/hosts/health-terminal/health?window=24h",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(stale["data"]["current"]["health"], "unknown", "{stale}");
    assert_eq!(stale["data"]["current"]["freshness"], "stale");
}

#[tokio::test]
async fn unknown_receipt_breaks_streak_and_missed_due_is_not_healthy() {
    let pool = storage::connect("sqlite::memory:").await.expect("db");
    seed_host(&pool, "health-gap").await;
    let frontend = TempDir::new().expect("frontend");
    let app = test_app(pool.clone(), &frontend);
    let (_, _, _) = request_json(
        app,
        Method::PUT,
        "/api/v1/hosts/health-gap/health-policy",
        &[("if-match", "\"revision-0\"")],
        policy(true, 80.0),
    )
    .await;
    let (version_id, policy_id, revision) = current_policy(&pool, "health-gap").await;
    let old = Utc::now();
    seed_run(
        &pool,
        "health-gap",
        "run-gap-old",
        "succeeded",
        "manual",
        Some(&version_id),
        None,
        None,
        None,
        0,
        old,
    )
    .await;
    seed_evaluation(
        &pool,
        "health-gap",
        "run-gap-old",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "healthy",
        "complete",
        old,
        Some(old + Duration::minutes(10)),
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let now = Utc::now();
    let schedule_version_id = seed_schedule(
        &pool,
        "health-gap",
        "schedule-gap",
        now - Duration::minutes(10),
    )
    .await;
    seed_run(
        &pool,
        "health-gap",
        "run-gap-interrupted",
        "running",
        "catch_up",
        Some(&version_id),
        Some("schedule-gap"),
        Some(&schedule_version_id),
        Some(now - Duration::minutes(5)),
        2,
        now,
    )
    .await;
    let recovered = monitoring_api::recover_interrupted_monitor_runs(&pool)
        .await
        .expect("recover interrupted");
    assert_eq!(recovered, 1);
    let current = sqlx::query(
        "SELECT e.status, e.reason_code AS evaluation_reason,
                c.reason_code AS condition_reason, c.streak_count
         FROM health_evaluations e
         JOIN health_condition_evaluations c ON c.evaluation_id = e.evaluation_id
         WHERE e.run_id = 'run-gap-interrupted'",
    )
    .fetch_one(&pool)
    .await
    .expect("interrupted health evaluation");
    assert_eq!(current.get::<String, _>("status"), "unknown");
    assert_eq!(current.get::<i64, _>("streak_count"), 0);
    assert_eq!(
        current.get::<String, _>("evaluation_reason"),
        "required_unknown"
    );
    assert_eq!(
        current.get::<String, _>("condition_reason"),
        "metric_unavailable"
    );
}

#[tokio::test]
async fn health_windows_have_fixed_sizes_provenance_and_catch_up_visibility() {
    let pool = storage::connect("sqlite::memory:").await.expect("db");
    seed_host(&pool, "health-window").await;
    let frontend = TempDir::new().expect("frontend");
    let app = test_app(pool.clone(), &frontend);
    let (_, _, _) = request_json(
        app.clone(),
        Method::PUT,
        "/api/v1/hosts/health-window/health-policy",
        &[("if-match", "\"revision-0\"")],
        policy(true, 80.0),
    )
    .await;
    let (version_id, policy_id, revision) = current_policy(&pool, "health-window").await;
    let due_from = Utc::now() - Duration::minutes(20);
    let schedule_version_id =
        seed_schedule(&pool, "health-window", "schedule-window", due_from).await;
    let scheduled_for = Utc::now() - Duration::minutes(5);
    let evaluated_at = Utc::now();
    seed_run(
        &pool,
        "health-window",
        "run-window-catchup",
        "succeeded",
        "catch_up",
        Some(&version_id),
        Some("schedule-window"),
        Some(&schedule_version_id),
        Some(scheduled_for),
        2,
        evaluated_at,
    )
    .await;
    seed_evaluation(
        &pool,
        "health-window",
        "run-window-catchup",
        &Uuid::new_v4().to_string(),
        Some((&version_id, &policy_id, revision)),
        "healthy",
        "complete",
        evaluated_at,
        Some(evaluated_at + Duration::minutes(5)),
    )
    .await;

    for (window, expected_count, resolution) in [
        ("24h", 24, "hour"),
        ("7d", 28, "six_hour"),
        ("30d", 30, "day"),
    ] {
        let (_, _, body) = request_json(
            app.clone(),
            Method::GET,
            &format!("/api/v1/hosts/health-window/health?window={window}"),
            &[],
            json!({}),
        )
        .await;
        assert_eq!(
            body["data"]["window"]["actual_resolution"], resolution,
            "{body}"
        );
        assert_eq!(
            body["data"]["window"]["buckets"].as_array().map(Vec::len),
            Some(expected_count)
        );
        assert_eq!(body["data"]["window"]["count_basis"], "scope_evaluations");
        assert_eq!(body["data"]["window"]["provenance_complete"], false);
        let late: u64 = body["data"]["window"]["buckets"]
            .as_array()
            .expect("buckets")
            .iter()
            .map(|bucket| bucket["late_observation_count"].as_u64().unwrap_or(0))
            .sum();
        assert!(
            late >= 1,
            "catch-up evidence must remain visible as late: {body}"
        );
    }
}
