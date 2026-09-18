use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header::CONTENT_TYPE},
};
use chrono::{Duration, SecondsFormat, Utc};
use http_body_util::BodyExt;
use network_atlas::{
    api::{self, AppState},
    monitoring_api, monitoring_scheduler, storage,
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use tempfile::TempDir;
use tower::ServiceExt;

async fn seed_host(pool: &SqlitePool, host_id: &str) {
    sqlx::query(
        "INSERT OR IGNORE INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user,
            credential_ref, host_key_state, host_key_fingerprint, transport,
            os, status, created_at
         ) VALUES (?, 'workspace-default', ?, 'fixture.invalid', 22, 'fixture',
            'secret-ref-fixture', 'verified', 'SHA256:fixture', 'ssh', 'linux',
            'connection_ready', '2026-08-15T00:00:00Z')",
    )
    .bind(host_id)
    .bind(host_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_schedule_version(pool: &SqlitePool, schedule_id: &str) -> String {
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

fn test_app(pool: SqlitePool, frontend: &TempDir) -> (AppState, Router) {
    let state = AppState::new(pool);
    let app = api::router(state.clone(), frontend.path());
    (state, app)
}

async fn request_json(
    app: Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let payload = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    (status, payload)
}

#[tokio::test]
async fn schedule_api_is_explicit_idempotent_and_revision_guarded() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    seed_host(&pool, "host-schedule-api").await;
    let frontend = TempDir::new().unwrap();
    let (_state, app) = test_app(pool.clone(), &frontend);
    let request = json!({
        "profile": "host_resource_v1",
        "interval_seconds": 300,
        "jitter_seconds": 30,
        "stale_after_seconds": 900,
        "enabled": true
    });

    let (status, created) = request_json(
        app.clone(),
        Method::POST,
        "/api/v1/hosts/host-schedule-api/monitor-schedules",
        &[("idempotency-key", "schedule-create-1")],
        request.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let schedule_id = created["data"]["schedule_id"].as_str().unwrap();
    assert_eq!(created["data"]["revision"], 1);
    assert_eq!(created["data"]["state"], "enabled");
    assert!(created["data"]["next_due_at"].is_string());
    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(run_count, 0, "creating a schedule must not run SSH");

    let (status, replayed) = request_json(
        app.clone(),
        Method::POST,
        "/api/v1/hosts/host-schedule-api/monitor-schedules",
        &[("idempotency-key", "schedule-create-1")],
        request.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replayed}");
    assert_eq!(replayed["data"]["schedule_id"], schedule_id);

    let (status, conflict) = request_json(
        app.clone(),
        Method::POST,
        "/api/v1/hosts/host-schedule-api/monitor-schedules",
        &[("idempotency-key", "schedule-create-1")],
        json!({
            "profile": "host_resource_v1",
            "interval_seconds": 600,
            "jitter_seconds": 30,
            "stale_after_seconds": 900,
            "enabled": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "IDEMPOTENCY_KEY_REUSED");

    let (status, unknown) = request_json(
        app.clone(),
        Method::POST,
        "/api/v1/hosts/host-schedule-api/monitor-schedules",
        &[("idempotency-key", "schedule-create-unknown")],
        json!({
            "profile": "host_resource_v1",
            "interval_seconds": 300,
            "jitter_seconds": 30,
            "stale_after_seconds": 900,
            "enabled": true,
            "command": "fixture"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown}");
    assert_eq!(unknown["error"]["code"], "INVALID_JSON");

    let (status, exported) = request_json(
        app.clone(),
        Method::GET,
        "/api/v1/hosts/host-schedule-api/export",
        &[],
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    assert_eq!(
        exported["data"]["payload"]["monitor_schedules"][0]["schedule_id"],
        schedule_id
    );
    let serialized = exported.to_string();
    assert!(!serialized.contains("lease_token"));
    assert!(!serialized.contains("create_request_sha256"));

    let patch_uri = format!("/api/v1/monitor-schedules/{schedule_id}");
    let (status, missing_match) = request_json(
        app.clone(),
        Method::PATCH,
        &patch_uri,
        &[],
        json!({"state": "paused"}),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{missing_match}");

    let (status, paused) = request_json(
        app.clone(),
        Method::PATCH,
        &patch_uri,
        &[("if-match", "\"revision-1\"")],
        json!({"state": "paused"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    assert_eq!(paused["data"]["revision"], 2);
    assert_eq!(paused["data"]["state"], "paused");
    assert!(paused["data"]["next_due_at"].is_null());
    let revisions = sqlx::query(
        "SELECT revision, state, due_from_at, due_until_at
         FROM monitor_schedule_versions
         WHERE schedule_id = ? ORDER BY revision",
    )
    .bind(schedule_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].get::<String, _>("state"), "enabled");
    assert!(
        revisions[0]
            .get::<Option<String>, _>("due_from_at")
            .is_some()
    );
    assert!(
        revisions[0]
            .get::<Option<String>, _>("due_until_at")
            .is_some()
    );
    assert_eq!(revisions[1].get::<String, _>("state"), "paused");
    assert!(
        revisions[1]
            .get::<Option<String>, _>("due_from_at")
            .is_none()
    );

    let (status, stale) = request_json(
        app,
        Method::PATCH,
        &patch_uri,
        &[("if-match", "revision-1")],
        json!({"state": "enabled"}),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{stale}");
    assert_eq!(stale["error"]["code"], "REVISION_MISMATCH");

    let schedule_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM change_events WHERE kind = 'monitor.schedule.changed'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        schedule_events, 2,
        "only successful create and patch emit events"
    );
}

#[tokio::test]
async fn unresolved_secret_ref_is_not_reported_as_remote_ssh_authentication() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    seed_host(&pool, "host-missing-secret").await;
    let frontend = TempDir::new().unwrap();
    let (state, app) = test_app(pool.clone(), &frontend);

    let (status, failure) = request_json(
        app,
        Method::POST,
        "/api/v1/hosts/host-missing-secret/monitor-runs",
        &[("idempotency-key", "manual-missing-secret")],
        json!({"profile": "host_resource_v1"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{failure}");
    assert_eq!(failure["error"]["code"], "SECRET_REF_UNAVAILABLE");
    let manual_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        manual_runs, 0,
        "manual preflight must not persist a false receipt"
    );

    let tick_at = Utc::now();
    sqlx::query(
        "INSERT INTO monitor_schedules(
            schedule_id, host_id, profile, interval_seconds, jitter_seconds,
            jitter_offset_seconds, stale_after_seconds, state, next_due_at,
            revision, create_idempotency_key, create_request_sha256,
            created_response_json, created_at, updated_at
         ) VALUES ('schedule-missing-secret', 'host-missing-secret',
            'host_resource_v1', 300, 0, 0, 900, 'enabled', ?, 1,
            'create-missing-secret', 'digest', '{}', ?, ?)",
    )
    .bind((tick_at - Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind(tick_at.to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind(tick_at.to_rfc3339_opts(SecondsFormat::Secs, true))
    .execute(&pool)
    .await
    .unwrap();
    seed_schedule_version(&pool, "schedule-missing-secret").await;

    assert_eq!(
        monitoring_scheduler::tick_once(&state, tick_at)
            .await
            .unwrap(),
        1
    );
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let row = sqlx::query(
                "SELECT state, failure_code, ssh_session_count
                 FROM monitor_runs WHERE schedule_id = 'schedule-missing-secret'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if row.get::<String, _>("state") == "failed" {
                break row;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduled failure should become terminal");
    assert_eq!(
        terminal.get::<Option<String>, _>("failure_code").as_deref(),
        Some("SECRET_REF_UNAVAILABLE")
    );
    assert_eq!(terminal.get::<i64, _>("ssh_session_count"), 0);
}

#[tokio::test]
async fn application_shutdown_closes_the_sse_stream_instead_of_blocking_drain() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    let frontend = TempDir::new().unwrap();
    let (state, app) = test_app(pool, &frontend);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/events/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    state.begin_shutdown();
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        response.into_body().collect(),
    )
    .await
    .expect("SSE body must end promptly after application shutdown")
    .unwrap()
    .to_bytes();
    assert!(body.is_empty());
}

#[tokio::test]
async fn application_shutdown_rejects_new_observations_and_stops_due_claims() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    seed_host(&pool, "host-shutdown").await;
    let tick_at = Utc::now();
    sqlx::query(
        "INSERT INTO monitor_schedules(
            schedule_id, host_id, profile, interval_seconds, jitter_seconds,
            jitter_offset_seconds, stale_after_seconds, state, next_due_at,
            revision, create_idempotency_key, create_request_sha256,
            created_response_json, created_at, updated_at
         ) VALUES ('schedule-shutdown', 'host-shutdown', 'host_resource_v1',
            300, 0, 0, 900, 'enabled', ?, 1, 'create-shutdown', 'digest',
            '{}', ?, ?)",
    )
    .bind((tick_at - Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind(tick_at.to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind(tick_at.to_rfc3339_opts(SecondsFormat::Secs, true))
    .execute(&pool)
    .await
    .unwrap();
    seed_schedule_version(&pool, "schedule-shutdown").await;
    let frontend = TempDir::new().unwrap();
    let (state, app) = test_app(pool.clone(), &frontend);
    state.begin_shutdown();

    assert_eq!(
        monitoring_scheduler::tick_once(&state, tick_at)
            .await
            .unwrap(),
        0
    );
    let (monitor_status, monitor_error) = request_json(
        app.clone(),
        Method::POST,
        "/api/v1/hosts/host-shutdown/monitor-runs",
        &[("idempotency-key", "monitor-during-shutdown")],
        json!({"profile": "host_resource_v1"}),
    )
    .await;
    assert_eq!(monitor_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(monitor_error["error"]["code"], "APPLICATION_SHUTTING_DOWN");
    let (discovery_status, discovery_error) = request_json(
        app,
        Method::POST,
        "/api/v1/hosts/host-shutdown/discovery-runs",
        &[("idempotency-key", "discovery-during-shutdown")],
        json!({}),
    )
    .await;
    assert_eq!(discovery_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        discovery_error["error"]["code"],
        "APPLICATION_SHUTTING_DOWN"
    );
    let monitor_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    let discovery_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM discovery_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((monitor_runs, discovery_runs), (0, 0));
}

#[tokio::test]
async fn due_schedule_catches_up_once_and_skips_an_active_manual_observation() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    seed_host(&pool, "host-catch-up").await;
    let now = Utc::now();
    let oldest_due = now - Duration::minutes(16);
    sqlx::query(
        "INSERT INTO monitor_schedules(
            schedule_id, host_id, profile, interval_seconds, jitter_seconds,
            jitter_offset_seconds, stale_after_seconds, state, next_due_at,
            revision, create_idempotency_key, create_request_sha256,
            created_response_json, created_at, updated_at
         ) VALUES ('schedule-catch-up', 'host-catch-up', 'host_resource_v1',
            300, 0, 0, 900, 'enabled', ?, 1, 'create-catch-up', 'digest',
            '{}', ?, ?)",
    )
    .bind(oldest_due.to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind((now - Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind((now - Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true))
    .execute(&pool)
    .await
    .unwrap();
    let _schedule_version_id = seed_schedule_version(&pool, "schedule-catch-up").await;
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds, submitted_at,
            accepted_response_json
         ) VALUES ('manual-active', 'host-catch-up', 'request-manual', 'manual-key',
            'digest', 'host_resource_v1', 'manual', 'queued', 900, ?, '{}')",
    )
    .bind(now.to_rfc3339_opts(SecondsFormat::Secs, true))
    .execute(&pool)
    .await
    .unwrap();

    let frontend = TempDir::new().unwrap();
    let (state, _app) = test_app(pool.clone(), &frontend);
    assert_eq!(
        monitoring_scheduler::tick_once(&state, now).await.unwrap(),
        1
    );
    assert_eq!(
        monitoring_scheduler::tick_once(&state, now).await.unwrap(),
        0
    );

    let row = sqlx::query(
        "SELECT trigger_kind, state, ssh_session_count, scheduled_for,
                due_interval_seconds, missed_due_count
         FROM monitor_runs WHERE schedule_id = 'schedule-catch-up'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("trigger_kind"), "catch_up");
    assert_eq!(row.get::<String, _>("state"), "skipped_overlap");
    assert_eq!(row.get::<i64, _>("ssh_session_count"), 0);
    assert_eq!(row.get::<i64, _>("due_interval_seconds"), 300);
    assert_eq!(row.get::<i64, _>("missed_due_count"), 3);
    let scheduled_for: String = row.get("scheduled_for");
    let next_due: String = sqlx::query_scalar(
        "SELECT next_due_at FROM monitor_schedules WHERE schedule_id = 'schedule-catch-up'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let scheduled = chrono::DateTime::parse_from_rfc3339(&scheduled_for).unwrap();
    let next = chrono::DateTime::parse_from_rfc3339(&next_due).unwrap();
    assert_eq!((next - scheduled).num_seconds(), 300);
    assert!(scheduled <= now);
    assert!(scheduled > now - Duration::minutes(5));
    let run_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM change_events WHERE kind = 'monitor.run.changed'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        run_events, 1,
        "the materialized skipped slot emits one event"
    );
}

#[tokio::test]
async fn concurrent_ticks_materialize_one_due_slot_and_restart_recovery_is_terminal() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    seed_host(&pool, "host-race").await;
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO monitor_schedules(
            schedule_id, host_id, profile, interval_seconds, jitter_seconds,
            jitter_offset_seconds, stale_after_seconds, state, next_due_at,
            revision, create_idempotency_key, create_request_sha256,
            created_response_json, created_at, updated_at
         ) VALUES ('schedule-race', 'host-race', 'host_resource_v1', 300, 0, 0,
            900, 'enabled', ?, 1, 'create-race', 'digest', '{}', ?, ?)",
    )
    .bind((now - Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind(now.to_rfc3339_opts(SecondsFormat::Secs, true))
    .bind(now.to_rfc3339_opts(SecondsFormat::Secs, true))
    .execute(&pool)
    .await
    .unwrap();
    let _schedule_version_id = seed_schedule_version(&pool, "schedule-race").await;
    let frontend = TempDir::new().unwrap();
    let (state, _app) = test_app(pool.clone(), &frontend);
    let state = Arc::new(state);
    let (left, right) = tokio::join!(
        monitoring_scheduler::tick_once(&state, now),
        monitoring_scheduler::tick_once(&state, now)
    );
    assert_eq!(left.unwrap() + right.unwrap(), 1);
    tokio::task::yield_now().await;

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitor_runs WHERE schedule_id = 'schedule-race'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let duplicate_slots: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM (
            SELECT schedule_id, scheduled_for, COUNT(*) AS amount
            FROM monitor_runs WHERE schedule_id = 'schedule-race'
            GROUP BY schedule_id, scheduled_for HAVING amount > 1
         )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(duplicate_slots, 0);

    let recovered = monitoring_api::recover_interrupted_monitor_runs(&pool)
        .await
        .unwrap();
    assert!(recovered <= 1);
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monitor_runs WHERE state IN ('queued', 'running')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 0);
}

#[tokio::test]
async fn startup_recovery_commits_a_resumable_monitor_change_event() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    seed_host(&pool, "host-recovery-event").await;
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds, submitted_at,
            accepted_response_json
         ) VALUES ('run-recovery-event', 'host-recovery-event', 'request-recovery',
            'key-recovery', 'digest', 'host_resource_v1', 'manual', 'running',
            900, '2026-08-15T00:00:00Z', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO change_events(
            workspace_id, kind, subject_ref, revision, summary_json, committed_at
         ) VALUES ('workspace-default', 'host.connection.changed', 'host:fixture',
            0, '{}', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let old_cursor: i64 = sqlx::query_scalar("SELECT MAX(cursor) FROM change_events")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        monitoring_api::recover_interrupted_monitor_runs(&pool)
            .await
            .unwrap(),
        1
    );
    let recovered_state: String =
        sqlx::query_scalar("SELECT state FROM monitor_runs WHERE run_id = 'run-recovery-event'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recovered_state, "interrupted");
    let event = sqlx::query(
        "SELECT cursor, kind, subject_ref, summary_json FROM change_events
         WHERE cursor > ? ORDER BY cursor LIMIT 1",
    )
    .bind(old_cursor)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event.get::<String, _>("kind"), "monitor.run.changed");
    assert_eq!(
        event.get::<String, _>("subject_ref"),
        "monitor-runs:startup-recovery"
    );
    let summary: Value = serde_json::from_str(&event.get::<String, _>("summary_json")).unwrap();
    assert_eq!(summary["state"], "interrupted");
    assert_eq!(summary["recovered_count"], 1);
    assert_eq!(summary["snapshot_required"], true);
}

#[tokio::test]
async fn migration_preserves_h1_receipts_and_enforces_cross_observation_exclusion() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for migration in [
        include_str!("../migrations/0001_m0_projection.sql"),
        include_str!("../migrations/0002_m1_ssh_discovery.sql"),
        include_str!("../migrations/0003_m2_projection.sql"),
        include_str!("../migrations/0004_m3_agent_rescan.sql"),
        include_str!("../migrations/0005_m4_remote_release.sql"),
        include_str!("../migrations/0006_ssh_password_credentials.sql"),
        include_str!("../migrations/0007_normalize_host_connection_status.sql"),
        include_str!("../migrations/0008_discovery_request_idempotency.sql"),
        include_str!("../migrations/0009_host_resource_monitoring.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }
    seed_host(&pool, "host-migration").await;
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, submitted_at, accepted_response_json
         ) VALUES ('h1-run', 'host-migration', 'h1-request', 'h1-key', 'digest',
            'host_resource_v1', 'manual', 'succeeded', '2026-08-15T00:00:00Z', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO change_events(
            workspace_id, kind, subject_ref, revision, summary_json, committed_at
         ) VALUES ('workspace-default', 'host.connection.changed', 'host:host-migration',
            7, '{}', '2026-08-15T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let old_cursor: i64 = sqlx::query_scalar("SELECT MAX(cursor) FROM change_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/0010_monitoring_interval_scheduler.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    let row = sqlx::query(
        "SELECT trigger_kind, stale_after_seconds, schedule_id
         FROM monitor_runs WHERE run_id = 'h1-run'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("trigger_kind"), "manual");
    assert_eq!(row.get::<i64, _>("stale_after_seconds"), 900);
    assert!(row.get::<Option<String>, _>("schedule_id").is_none());
    let migrated_event =
        sqlx::query("SELECT cursor, kind, revision FROM change_events WHERE cursor = ?")
            .bind(old_cursor)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(migrated_event.get::<i64, _>("cursor"), old_cursor);
    assert_eq!(
        migrated_event.get::<String, _>("kind"),
        "host.connection.changed"
    );
    assert_eq!(migrated_event.get::<i64, _>("revision"), 7);
    sqlx::query(
        "INSERT INTO change_events(
            workspace_id, kind, subject_ref, revision, summary_json, committed_at
         ) VALUES ('workspace-default', 'monitor.schedule.changed', 'schedule:fixture',
            1, '{}', '2026-08-15T00:00:02Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let new_cursor: i64 = sqlx::query_scalar(
        "SELECT cursor FROM change_events WHERE kind = 'monitor.schedule.changed'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        new_cursor > old_cursor,
        "migration must preserve cursor monotonicity"
    );

    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version,
            state, response_json, submitted_at
         ) VALUES ('discovery-active', 'host-migration', 'request-discovery',
            'key-discovery', '1', 'accepted', '{}', '2026-08-15T00:01:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let blocked = sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds, submitted_at,
            accepted_response_json
         ) VALUES ('blocked-monitor', 'host-migration', 'request-monitor', 'key-monitor',
            'digest', 'host_resource_v1', 'manual', 'queued', 900,
            '2026-08-15T00:01:01Z', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        blocked
            .to_string()
            .contains("host observation already active")
    );

    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");
    let foreign_keys = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(foreign_keys.is_empty());
}
