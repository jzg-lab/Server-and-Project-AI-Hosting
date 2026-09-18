use std::{path::PathBuf, time::Duration};

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode, header::SET_COOKIE},
};
use http_body_util::BodyExt;
use network_atlas::{
    api,
    auth::{AuthConfig, AuthService},
    events::{self, ChangeEventKind},
    fixtures,
    model_provider::ModelClient,
    secrets::FileSecretStore,
    ssh::SystemSsh,
    storage,
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "https://atlas.test";
const USERNAME: &str = "owner";
const PASSWORD: &str = "correct horse battery staple";

struct TestApp {
    app: Router,
    pool: SqlitePool,
    directory: TempDir,
    state: api::AppState,
}

async fn test_app(login_limit: u32) -> TestApp {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("network-atlas.db");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        database_path.to_string_lossy().replace('\\', "/")
    );
    let pool = storage::connect(&database_url).await.unwrap();
    let salt = SaltString::encode_b64(b"network-atlas-m4-test").unwrap();
    let password_hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    let auth = AuthConfig::required(USERNAME, password_hash, ORIGIN, true)
        .unwrap()
        .with_limits(login_limit, 120);
    let secret_root = directory.path().join("secrets");
    let ssh = SystemSsh::system_default(directory.path().join("ssh"));
    let state = api::AppState::with_services_model_auth(
        pool.clone(),
        FileSecretStore::new(secret_root),
        ssh,
        ModelClient::default(),
        AuthService::new(auth),
        directory.path().to_path_buf(),
    );
    let app = api::router(state.clone(), "../frontend");
    TestApp {
        app,
        pool,
        directory,
        state,
    }
}

fn json_request(
    method: &str,
    uri: &str,
    body: Value,
    origin: Option<&str>,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn login(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/login",
            json!({"username": USERNAME, "password": PASSWORD}),
            Some(ORIGIN),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(
        response
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Secure")
    );
    let payload = json_body(response).await;
    let csrf = payload["data"]["csrf_token"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

async fn seed_h3c_export_fixture(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO monitor_schedules(
            schedule_id, host_id, profile, interval_seconds, jitter_seconds,
            jitter_offset_seconds, stale_after_seconds, state, next_due_at,
            revision, create_idempotency_key, create_request_sha256,
            created_response_json, created_at, updated_at
         ) VALUES (
            'schedule-host-m4-health', 'host-m4', 'host_resource_v1', 300, 0,
            0, 900, 'enabled', '2026-08-11T00:10:00Z', 1,
            'create-host-m4-health', 'schedule-request-hash', '{}',
            '2026-08-11T00:00:00Z', '2026-08-11T00:00:00Z'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO monitor_schedule_versions(
            schedule_version_id, schedule_id, host_id, revision, profile,
            interval_seconds, jitter_seconds, jitter_offset_seconds,
            stale_after_seconds, state, due_from_at, due_from_at_epoch_ms,
            effective_from_at, effective_from_at_epoch_ms, provenance_kind,
            activated_at, created_at
         ) VALUES (
            '10000000-0000-0000-0000-000000000001', 'schedule-host-m4-health',
            'host-m4', 1, 'host_resource_v1', 300, 0, 0, 900, 'enabled',
            '2026-08-11T00:10:00Z', 1786407000000,
            '2026-08-11T00:00:00Z', 1786406400000, 'recorded',
            '2026-08-11T00:00:00Z', '2026-08-11T00:00:00Z'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO health_policy_versions(
            policy_version_id, policy_id, host_id, revision, lifecycle_state,
            enabled, source_kind, policy_json, policy_sha256,
            effective_from_at, effective_from_at_epoch_ms, created_by, created_at
         ) VALUES (
            '20000000-0000-0000-0000-000000000001',
            '20000000-0000-0000-0000-000000000000', 'host-m4', 1, 'current',
            1, 'user_confirmed', '{\"enabled\":true,\"cpu_busy\":{}}',
            'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
            '2026-08-11T00:00:00Z', 1786406400000, 'owner-local',
            '2026-08-11T00:00:00Z'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, schedule_id, schedule_revision,
            schedule_version_id, health_policy_version_id, scheduled_for,
            stale_after_seconds, due_interval_seconds, missed_due_count,
            collector_version, coverage_json, output_bytes, ssh_session_count,
            submitted_at, started_at, finished_at, accepted_response_json
         ) VALUES (
            'run-host-m4-health', 'host-m4', 'request-host-m4-health',
            'idempotency-host-m4-health', 'run-request-hash',
            'host_resource_v1', 'scheduled', 'succeeded',
            'schedule-host-m4-health', 1,
            '10000000-0000-0000-0000-000000000001',
            '20000000-0000-0000-0000-000000000001',
            '2026-08-11T00:10:00Z', 900, 300, 0,
            'host-resource-v1', '[]', 1024, 1,
            '2026-08-11T00:10:00Z', '2026-08-11T00:10:00Z',
            '2026-08-11T00:10:01Z', '{}'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO health_evaluations(
            evaluation_id, run_id, host_id, policy_version_id, policy_id,
            policy_revision, status, reason_code, required_condition_count,
            optional_condition_count, ok_count, warning_count, critical_count,
            unknown_count, observation_state, input_sha256, evaluated_at,
            evaluated_at_epoch_ms, observed_at, observed_at_epoch_ms,
            valid_until, valid_until_epoch_ms, created_at
         ) VALUES (
            '30000000-0000-0000-0000-000000000001', 'run-host-m4-health',
            'host-m4', '20000000-0000-0000-0000-000000000001',
            '20000000-0000-0000-0000-000000000000', 1, 'healthy',
            'all_required_conditions_ok', 1, 0, 1, 0, 0, 0, 'complete',
            'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
            '2026-08-11T00:10:01Z', 1786407001000,
            '2026-08-11T00:10:01Z', 1786407001000,
            '2026-08-11T00:25:01Z', 1786407901000,
            '2026-08-11T00:10:01Z'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO health_condition_evaluations(
            condition_evaluation_id, evaluation_id, condition_key,
            condition_kind, requirement, subject_kind, subject_id, subject_label,
            status, candidate_status, reason_code, value_real, unit,
            window_seconds, streak_count, streak_required, evidence_refs_json,
            input_sha256, created_at
         ) VALUES (
            '40000000-0000-0000-0000-000000000002',
            '30000000-0000-0000-0000-000000000001', 'cpu_busy',
            'cpu_busy_instant_percent', 'required', 'host', 'host-m4', 'CPU busy',
            'ok', 'ok', 'within_threshold', 42.0, 'percent', 1.0, 1, 1,
            '[\"sample-host-m4-raw-only\"]',
            'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
            '2026-08-11T00:10:01Z'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn owner_session_enforces_origin_csrf_rate_limit_and_logout() {
    let fixture = test_app(3).await;

    let anonymous = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/bootstrap")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous.headers().get("cache-control").unwrap(),
        "no-store"
    );
    assert!(anonymous.headers().contains_key("content-security-policy"));
    assert_eq!(json_body(anonymous).await["error"]["code"], "AUTH_REQUIRED");

    let wrong_origin = fixture
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/login",
            json!({"username": USERNAME, "password": PASSWORD}),
            Some("https://wrong.test"),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(wrong_origin.status(), StatusCode::FORBIDDEN);

    let wrong_password = fixture
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/login",
            json!({"username": USERNAME, "password": "wrong"}),
            Some(ORIGIN),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(wrong_password.status(), StatusCode::UNAUTHORIZED);
    let (cookie, csrf) = login(&fixture.app).await;

    let session = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/session")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    assert_eq!(json_body(session).await["data"]["username"], USERNAME);

    let missing_csrf = fixture
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/logout",
            json!({}),
            Some(ORIGIN),
            Some(&cookie),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);

    let rate_limited = fixture
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/login",
            json!({"username": USERNAME, "password": PASSWORD}),
            Some(ORIGIN),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(rate_limited.status(), StatusCode::OK);
    let blocked = fixture
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/login",
            json!({"username": USERNAME, "password": PASSWORD}),
            Some(ORIGIN),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);

    let logout = fixture
        .app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/logout",
            json!({}),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert!(
        logout
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );

    let revoked = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/bootstrap")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(audit_count >= 4);
}

#[tokio::test]
async fn every_documented_business_route_is_behind_the_owner_session() {
    let fixture = test_app(10).await;
    let document: Value = serde_json::from_str(&api::openapi().to_json().unwrap()).unwrap();
    assert_eq!(
        document.pointer("/components/securitySchemes/owner_session/type"),
        Some(&json!("apiKey"))
    );
    assert_eq!(
        document.pointer("/components/securitySchemes/owner_session/in"),
        Some(&json!("cookie"))
    );
    assert_eq!(
        document.pointer("/components/securitySchemes/owner_session/name"),
        Some(&json!("network_atlas_session"))
    );
    let paths = document["paths"].as_object().unwrap();
    let mut checked = 0_u32;
    for (template, operations) in paths {
        if !template.starts_with("/api/v1/") || template == "/api/v1/auth/login" {
            continue;
        }
        let uri = template
            .replace("{project_id}", "missing")
            .replace("{host_id}", "missing")
            .replace("{run_id}", "missing")
            .replace("{draft_id}", "missing")
            .replace("{layout_id}", "missing")
            .replace("{session_id}", "missing");
        for method in operations.as_object().unwrap().keys() {
            if !["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
                continue;
            }
            assert_eq!(
                operations[method]["security"][0]["owner_session"],
                json!([]),
                "{method} {template} must declare owner_session"
            );
            let response = fixture
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.to_ascii_uppercase().as_str())
                        .uri(&uri)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {template} must reject anonymous access"
            );
            checked += 1;
        }
    }
    assert!(checked >= 25, "expected the full M0-M4 business surface");
}

#[tokio::test]
async fn model_test_without_a_workspace_returns_model_not_configured() {
    let fixture = test_app(10).await;
    let (cookie, csrf) = login(&fixture.app).await;

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/model-provider/test")
                .header("origin", ORIGIN)
                .header("cookie", &cookie)
                .header("x-csrf-token", &csrf)
                .header("idempotency-key", "model-test-no-workspace")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = json_body(response).await;
    assert_eq!(payload["data"]["state"], "failed");
    assert_eq!(payload["data"]["error_code"], "MODEL_NOT_CONFIGURED");
    let workspaces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        workspaces, 1,
        "the first model test must initialize its workspace"
    );
}

#[tokio::test]
async fn sse_requires_a_session_and_resumes_from_last_committed_cursor() {
    let fixture = test_app(10).await;
    let (cookie, _csrf) = login(&fixture.app).await;
    let first = events::publish(
        &fixture.pool,
        ChangeEventKind::ProjectionChanged,
        "projection-draft:first",
        3,
        json!({"state": "draft"}),
    )
    .await
    .unwrap();
    let second = events::publish(
        &fixture.pool,
        ChangeEventKind::ProjectionChanged,
        "projection-draft:second",
        4,
        json!({"state": "confirmed"}),
    )
    .await
    .unwrap();

    let anonymous = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events/stream")
                .header("cookie", &cookie)
                .header("last-event-id", first.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-accel-buffering").unwrap(), "no");
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let bytes = frame.into_data().unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains(&format!("id: {second}")));
    assert!(text.contains("event: projection.changed"));
    assert!(text.contains("projection-draft:second"));
    assert!(!text.contains("projection-draft:first"));
}

#[tokio::test]
async fn sse_expired_cursor_requires_a_snapshot_reset() {
    let fixture = test_app(10).await;
    let (cookie, _csrf) = login(&fixture.app).await;
    let first = events::publish(
        &fixture.pool,
        ChangeEventKind::ProjectionChanged,
        "projection-draft:expired",
        1,
        json!({"state": "draft"}),
    )
    .await
    .unwrap();
    let second = events::publish(
        &fixture.pool,
        ChangeEventKind::ProjectionChanged,
        "projection-draft:removed",
        2,
        json!({"state": "draft"}),
    )
    .await
    .unwrap();
    let latest = events::publish(
        &fixture.pool,
        ChangeEventKind::ProjectionChanged,
        "projection-draft:latest",
        3,
        json!({"state": "confirmed"}),
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM change_events WHERE cursor <= ?")
        .bind(second)
        .execute(&fixture.pool)
        .await
        .unwrap();

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events/stream")
                .header("cookie", &cookie)
                .header("last-event-id", first.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(text.contains(&format!("id: {latest}")));
    assert!(text.contains("event: stream.reset"));
    assert!(text.contains("\"reason\":\"cursor_expired\""));
    assert!(text.contains("\"snapshot_required\":true"));
}

#[tokio::test]
async fn export_and_host_delete_create_a_verified_backup_before_cleanup() {
    let fixture = test_app(10).await;
    let (cookie, csrf) = login(&fixture.app).await;
    let credential_id = Uuid::new_v4();
    let credential_ref = format!("secret://ssh/{credential_id}");
    let secret_root = fixture.directory.path().join("secrets");
    std::fs::create_dir_all(&secret_root).unwrap();
    let secret_path = secret_root.join(format!("{credential_id}.key"));
    std::fs::write(
        &secret_path,
        "-----BEGIN OPENSSH PRIVATE KEY-----\nfixture\n-----END OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-11T00:00:00Z')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO secret_ref_descriptors(
            credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
         ) VALUES (?, 'ssh_key', 'm4-seed', 'hash', '{}', '2026-08-11T00:00:00Z')",
    )
    .bind(&credential_ref)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at
         ) VALUES (
            'host-m4', 'workspace-default', 'M4 HOST', '127.0.0.1', 22, 'fixture', ?,
            'unverified', 'ssh', 'linux', 'registered', '2026-08-11T00:00:00Z'
         )",
    )
    .bind(&credential_ref)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE monitoring_history_metadata
         SET history_started_at = '2026-08-11T00:00:00.000Z',
             updated_at = '2026-08-11T00:00:00.000Z'
         WHERE singleton_id = 1",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds,
            collector_version, coverage_json, submitted_at, started_at,
            finished_at, accepted_response_json
         ) VALUES (
            'run-host-m4-history', 'host-m4', 'request-host-m4-history',
            'idempotency-host-m4-history', 'request-hash-host-m4-history',
            'host_resource_v1', 'manual', 'succeeded', 900,
            'host-resource-v1', '[]', '2026-08-11T00:05:00.000Z',
            '2026-08-11T00:05:00.000Z', '2026-08-11T00:05:01.000Z', '{}'
         )",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO metric_samples(
            sample_id, run_id, host_id, family, subject_kind, subject_id,
            metric_name, dimensions_json, dimensions_sha256, sample_kind,
            value_real, unit, window_seconds, quality, observed_at,
            observed_at_epoch_ms, source_kind, created_at
         ) VALUES (
            'sample-host-m4-raw-only', 'run-host-m4-history', 'host-m4',
            'cpu', 'host', 'host-m4', 'busy_percent', '{}',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            'derived', 73.25, 'percent', 1.0, 'observed',
            '2026-08-11T00:05:01.000Z', 1786406701000,
            'ssh_host_resource_v1', '2026-08-11T00:05:01.000Z'
         )",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO monitoring_compaction_runs(
             compaction_run_id, state, started_at, finished_at, settings_json
         ) VALUES (
             '40000000-0000-0000-0000-000000000001', 'succeeded',
             '2026-08-11T00:06:00.000Z', '2026-08-11T00:06:01.000Z', '{}'
         )",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE monitoring_history_maintenance
         SET retention_enabled = 1
         WHERE singleton_id = 1",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    seed_h3c_export_fixture(&fixture.pool).await;

    let export = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/hosts/host-m4/export")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let export_status = export.status();
    let exported = json_body(export).await;
    assert_eq!(export_status, StatusCode::OK, "{exported}");
    assert_eq!(exported["data"]["scope_id"], "host-m4");
    assert_eq!(
        exported["data"]["payload"]["host"]["credential_ref"],
        credential_ref
    );
    assert!(
        exported["data"]["payload"]["monitor_runs"][0]
            .get("due_interval_seconds")
            .is_some()
    );
    assert!(
        exported["data"]["payload"]["monitor_runs"][0]
            .get("missed_due_count")
            .is_some()
    );
    let payload = &exported["data"]["payload"];
    assert_eq!(
        payload["monitor_schedule_provenance_metadata"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(payload["monitor_schedule_versions"][0]["revision"], 1);
    assert_eq!(
        payload["health_policy_versions"][0]["policy"]["enabled"],
        true
    );
    assert_eq!(payload["health_evaluations"][0]["status"], "healthy");
    assert_eq!(
        payload["health_condition_evaluations"][0]["condition_key"],
        "cpu_busy"
    );
    assert_eq!(
        payload["health_condition_evaluations"][0]["evidence_ref_count"],
        1
    );
    assert_eq!(
        payload["health_condition_evaluations"][0]["evidence_refs"],
        json!([])
    );
    let health_run = payload["monitor_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run_id"] == "run-host-m4-health")
        .unwrap();
    assert_eq!(
        health_run["schedule_version_id"],
        "10000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        health_run["health_policy_version_id"],
        "20000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]["included"],
        false
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]["selection"],
        "excluded_by_default"
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["retention_enforcement"]
            ["mode"],
        "implemented_opt_in"
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["retention_enforcement"]
            ["automatic_cleanup"],
        false
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["retention_enforcement"]
            ["last_persisted_run_setting"],
        true
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["retention_enforcement"]
            ["periodic_compactor"],
        "available"
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["raw_query_max_span_seconds"],
        604_800
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["capabilities"]["hour_resolution"],
        "available"
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["capabilities"]["day_resolution"],
        "available"
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_rollups"]["hour_omitted_row_count"],
        0
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_rollups"]["retrieval"]
            ["lossless"],
        false
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["maintenance_ledgers"]
            ["included"],
        false
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["maintenance_ledgers"]
            ["tables"]["monitoring_compaction_runs"]["omitted_row_count"],
        1
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]["omitted_row_count"],
        1
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]["retrieval"]
            ["scoped_host_id"],
        "host-m4"
    );
    let retrieval = &exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]
        ["retrieval"];
    assert_eq!(retrieval["mode"], "partitioned_keyset_query");
    assert_eq!(retrieval["representation"], "semantic_metric_points");
    assert_eq!(retrieval["snapshot_consistency"], "not_provided");
    assert_eq!(retrieval["lossless"], false);
    assert_eq!(retrieval["complete_export"], false);
    assert_eq!(
        retrieval["complete_recovery_artifact"],
        "verified_pre_delete_sqlite_backup"
    );
    assert_eq!(retrieval["pagination"]["maximum_limit"], 5_000);
    assert_eq!(
        retrieval["pagination"]["cursor_query_parameters"],
        json!(["after_epoch_ms", "after_sample_id"])
    );
    assert_eq!(
        retrieval["subject_kind_partitions"],
        json!([
            "host",
            "cpu",
            "filesystem",
            "block_device",
            "interface",
            "process"
        ])
    );
    assert!(retrieval["verification"]["sum_of_returned_points_must_equal"].is_null());
    assert_eq!(
        retrieval["verification"]["row_count_equality_supported"],
        false
    );
    assert_eq!(
        exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]["omitted_row_count_semantics"],
        "informational_at_manifest_generation"
    );
    assert!(!exported.to_string().contains("sample-host-m4-raw-only"));

    let workspace_export = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/exports/workspace")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let workspace_export_status = workspace_export.status();
    let workspace_exported = json_body(workspace_export).await;
    assert_eq!(
        workspace_export_status,
        StatusCode::OK,
        "{workspace_exported}"
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["monitor_schedule_versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["health_policy_versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["health_evaluations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["health_condition_evaluations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["health_condition_evaluations"][0]["evidence_ref_count"],
        1
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["health_condition_evaluations"][0]["evidence_refs"],
        json!([])
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]
            ["omitted_row_count"],
        1
    );
    assert!(
        workspace_exported["data"]["payload"]["_content_manifest"]
            ["monitoring_history"]["metric_samples"]["retrieval"]["scoped_host_id"]
            .is_null()
    );
    assert_eq!(
        workspace_exported["data"]["payload"]["_content_manifest"]["monitoring_history"]["metric_samples"]
            ["retrieval"]["workspace_host_ids_from"],
        "payload.hosts[].host_id"
    );
    assert!(
        !workspace_exported
            .to_string()
            .contains("sample-host-m4-raw-only")
    );

    // Model a worker that was already admitted before deletion. Its terminal
    // run/sample/current transaction commits only after DELETE has started.
    // The backup fence must wait and include that complete transaction before
    // the HOST cascade is allowed to run.
    let observation_gate = fixture.state.observation_gate();
    let maximum = observation_gate.available_permits();
    assert!(maximum > 0);
    let worker_permits = observation_gate
        .clone()
        .acquire_many_owned(u32::try_from(maximum).unwrap())
        .await
        .unwrap();
    let delete_app = fixture.app.clone();
    let delete_cookie = cookie.clone();
    let delete_csrf = csrf.clone();
    let mut delete_task = tokio::spawn(async move {
        delete_app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/hosts/host-m4")
                    .header("origin", ORIGIN)
                    .header("cookie", delete_cookie)
                    .header("x-csrf-token", delete_csrf)
                    .header("idempotency-key", "delete-host-m4")
                    .header("x-confirm-delete", "host:host-m4")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut delete_task)
            .await
            .is_err(),
        "DELETE must wait while an admitted observation can still commit"
    );

    let mut worker_tx = fixture.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds,
            collector_version, boot_id, coverage_json, submitted_at, started_at,
            finished_at, accepted_response_json
         ) VALUES (
            'run-host-m4-fenced', 'host-m4', 'request-host-m4-fenced',
            'idempotency-host-m4-fenced', 'request-hash-host-m4-fenced',
            'host_resource_v1', 'manual', 'succeeded', 900,
            'host-resource-v1', 'boot-fenced', '[]', '2026-08-11T00:06:00.000Z',
            '2026-08-11T00:06:00.000Z', '2026-08-11T00:06:01.000Z', '{}'
         )",
    )
    .execute(&mut *worker_tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO metric_samples(
            sample_id, run_id, host_id, family, subject_kind, subject_id,
            metric_name, dimensions_json, dimensions_sha256, sample_kind,
            value_integer, unit, quality, observed_at, observed_at_epoch_ms,
            source_kind, created_at
         ) VALUES (
            'sample-host-m4-fenced', 'run-host-m4-fenced', 'host-m4',
            'cpu', 'host', 'host-m4', 'user_ticks', '{}',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            'counter', 123, 'ticks', 'observed', '2026-08-11T00:06:01.000Z',
            1786406761000, 'ssh_host_resource_v1', '2026-08-11T00:06:01.000Z'
         )",
    )
    .execute(&mut *worker_tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO monitoring_current(
            host_id, run_id, profile, collector_version, boot_id, snapshot_json,
            coverage_json, metric_count, unknown_count, observed_at, valid_until,
            snapshot_sha256, updated_at
         ) VALUES (
            'host-m4', 'run-host-m4-fenced', 'host_resource_v1',
            'host-resource-v1', 'boot-fenced', '{}', '[]', 1, 0,
            '2026-08-11T00:06:01.000Z', '2026-08-11T00:21:01.000Z',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            '2026-08-11T00:06:01.000Z'
         )",
    )
    .execute(&mut *worker_tx)
    .await
    .unwrap();
    worker_tx.commit().await.unwrap();
    drop(worker_permits);

    let delete = tokio::time::timeout(Duration::from_secs(5), delete_task)
        .await
        .expect("fenced DELETE completion")
        .expect("DELETE task");
    assert_eq!(delete.status(), StatusCode::OK);
    let deleted = json_body(delete).await;
    assert_eq!(deleted["data"]["secret_cleanup"], "completed");
    assert!(!secret_path.exists());
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hosts WHERE host_id = 'host-m4'")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    let live_samples: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples WHERE host_id = 'host-m4'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(live_samples, 0);
    for table in [
        "monitor_schedules",
        "monitor_schedule_versions",
        "health_policy_versions",
        "health_evaluations",
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE host_id = 'host-m4'");
        let count: i64 = sqlx::query_scalar(&sql)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table} must cascade with HOST deletion");
    }
    let live_conditions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM health_condition_evaluations
         WHERE evaluation_id = '30000000-0000-0000-0000-000000000001'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(live_conditions, 0);
    let live_provenance_metadata: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM monitor_schedule_provenance_metadata WHERE singleton_id = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        live_provenance_metadata, 1,
        "schedule provenance metadata belongs to the database lifetime"
    );

    let backup_path: String = sqlx::query_scalar(
        "SELECT storage_path FROM backup_records WHERE scope_kind = 'host' AND scope_id = 'host-m4'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let backup = PathBuf::from(backup_path);
    let backup_database = backup.join("network-atlas.db");
    let backup_manifest = backup.join("manifest.json");
    assert!(backup_database.is_file());
    assert!(backup_manifest.is_file());
    assert!(
        backup
            .join("secrets")
            .join(format!("{credential_id}.key"))
            .is_file()
    );
    storage::verify_database_file(&backup_database)
        .await
        .unwrap();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&backup_manifest).unwrap()).unwrap();
    assert_eq!(
        manifest["database_contents"]["database_scope"],
        "full_workspace_snapshot"
    );
    assert_eq!(
        manifest["database_contents"]["monitoring_history"]["included"],
        true
    );
    assert_eq!(
        manifest["database_contents"]["monitoring_history"]["metric_samples"]["row_count"],
        2
    );
    assert_eq!(
        manifest["database_contents"]["monitoring_history"]["monitoring_history_metadata"]["row_count"],
        1
    );
    assert_eq!(
        manifest["database_contents"]["monitoring_history"]["metric_rollups"]["hour_row_count"],
        0
    );
    assert_eq!(
        manifest["database_contents"]["monitoring_history"]["metric_rollup_partitions"]["row_count"],
        0
    );
    assert_eq!(
        manifest["database_contents"]["monitoring_history"]["monitoring_compaction_runs"]["row_count"],
        1
    );
    assert_eq!(
        manifest["database_sha256"],
        deleted["data"]["backup_sha256"]
    );
    let backup_url = format!(
        "sqlite://{}?mode=ro",
        backup_database.to_string_lossy().replace('\\', "/")
    );
    let backup_pool = SqlitePool::connect(&backup_url).await.unwrap();
    let backed_up_sample: String = sqlx::query_scalar(
        "SELECT sample_id FROM metric_samples WHERE sample_id = 'sample-host-m4-raw-only'",
    )
    .fetch_one(&backup_pool)
    .await
    .unwrap();
    assert_eq!(backed_up_sample, "sample-host-m4-raw-only");
    let fenced_sample: i64 = sqlx::query_scalar(
        "SELECT value_integer FROM metric_samples WHERE sample_id = 'sample-host-m4-fenced'",
    )
    .fetch_one(&backup_pool)
    .await
    .unwrap();
    assert_eq!(fenced_sample, 123);
    let fenced_current: String =
        sqlx::query_scalar("SELECT run_id FROM monitoring_current WHERE host_id = 'host-m4'")
            .fetch_one(&backup_pool)
            .await
            .unwrap();
    assert_eq!(fenced_current, "run-host-m4-fenced");
    let backed_up_history_started_at: String = sqlx::query_scalar(
        "SELECT history_started_at FROM monitoring_history_metadata WHERE singleton_id = 1",
    )
    .fetch_one(&backup_pool)
    .await
    .unwrap();
    assert_eq!(backed_up_history_started_at, "2026-08-11T00:00:00.000Z");
    for (table, predicate) in [
        ("monitor_schedule_provenance_metadata", "singleton_id = 1"),
        ("monitor_schedule_versions", "host_id = 'host-m4'"),
        ("health_policy_versions", "host_id = 'host-m4'"),
        ("health_evaluations", "host_id = 'host-m4'"),
        (
            "health_condition_evaluations",
            "evaluation_id = '30000000-0000-0000-0000-000000000001'",
        ),
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {predicate}");
        let count: i64 = sqlx::query_scalar(&sql)
            .fetch_one(&backup_pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "backup must retain the H3c row from {table}");
    }
    let backed_up_evidence_refs: String = sqlx::query_scalar(
        "SELECT evidence_refs_json FROM health_condition_evaluations
         WHERE condition_evaluation_id = '40000000-0000-0000-0000-000000000002'",
    )
    .fetch_one(&backup_pool)
    .await
    .unwrap();
    assert_eq!(backed_up_evidence_refs, "[\"sample-host-m4-raw-only\"]");
    backup_pool.close().await;

    let replay = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/hosts/host-m4")
                .header("origin", ORIGIN)
                .header("cookie", &cookie)
                .header("x-csrf-token", &csrf)
                .header("idempotency-key", "delete-host-m4")
                .header("x-confirm-delete", "host:host-m4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await["data"], deleted["data"]);
}

#[tokio::test]
async fn a_closed_observation_gate_rejects_delete_before_backup_or_data_loss() {
    let fixture = test_app(10).await;
    let (cookie, csrf) = login(&fixture.app).await;
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-11T00:00:00Z')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture.state.observation_gate().close();

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/workspace")
                .header("origin", ORIGIN)
                .header("cookie", cookie)
                .header("x-csrf-token", csrf)
                .header("idempotency-key", "delete-workspace-closed-gate")
                .header("x-confirm-delete", "workspace:workspace-default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "OBSERVATION_GATE_CLOSED");
    let workspace_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let backup_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM backup_records")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(workspace_count, 1);
    assert_eq!(backup_count, 0);
}

#[tokio::test]
async fn project_delete_updates_persisted_snapshots_and_replays_its_receipt() {
    let fixture = test_app(10).await;
    let (cookie, csrf) = login(&fixture.app).await;
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-11T00:00:00Z')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO projects(
            project_id, workspace_id, label, projection_state, source_refs_json, created_at, updated_at
         ) VALUES (
            'hermes', 'workspace-default', 'Hermes', 'draft', '[\"fixture:m4\"]',
            '2026-08-11T00:00:00Z', '2026-08-11T00:00:00Z'
         )",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let snapshot = serde_json::to_string(&fixtures::global_world().data).unwrap();
    sqlx::query(
        "INSERT INTO projection_drafts(
            draft_id, workspace_id, base_revision, state, snapshot_json, updated_at,
            revision, pending_changes, created_at
         ) VALUES (
            'draft-project-delete', 'workspace-default', 0, 'draft', ?,
            '2026-08-11T00:00:00Z', 1, 0, '2026-08-11T00:00:00Z'
         )",
    )
    .bind(&snapshot)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let export = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects/hermes/export")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(export.status(), StatusCode::OK);
    assert_eq!(json_body(export).await["data"]["scope_id"], "hermes");

    let delete = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/projects/hermes")
                .header("origin", ORIGIN)
                .header("cookie", &cookie)
                .header("x-csrf-token", &csrf)
                .header("idempotency-key", "delete-project-hermes")
                .header("x-confirm-delete", "project:hermes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);
    let deleted = json_body(delete).await;
    assert_eq!(deleted["data"]["secret_cleanup"], "not_applicable");
    let projects: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE project_id = 'hermes'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(projects, 0);
    let updated_snapshot: String = sqlx::query_scalar(
        "SELECT snapshot_json FROM projection_drafts WHERE draft_id = 'draft-project-delete'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let updated_snapshot: Value = serde_json::from_str(&updated_snapshot).unwrap();
    assert!(
        updated_snapshot["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["id"] != "hermes" && node["project_id"] != "hermes")
    );
    let backup_path: String = sqlx::query_scalar(
        "SELECT storage_path FROM backup_records WHERE scope_kind = 'project' AND scope_id = 'hermes'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    storage::verify_database_file(&PathBuf::from(backup_path).join("network-atlas.db"))
        .await
        .unwrap();

    let replay = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/projects/hermes")
                .header("origin", ORIGIN)
                .header("cookie", &cookie)
                .header("x-csrf-token", &csrf)
                .header("idempotency-key", "delete-project-hermes")
                .header("x-confirm-delete", "project:hermes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await["data"], deleted["data"]);
}

#[tokio::test]
async fn workspace_delete_revokes_sessions_cleans_secrets_and_survives_relogin_replay() {
    let fixture = test_app(10).await;
    let (cookie, csrf) = login(&fixture.app).await;
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-11T00:00:00Z')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let credential_id = Uuid::new_v4();
    let credential_ref = format!("secret://ssh/{credential_id}");
    let secret_root = fixture.directory.path().join("secrets");
    std::fs::create_dir_all(&secret_root).unwrap();
    let secret_path = secret_root.join(format!("{credential_id}.key"));
    std::fs::write(&secret_path, "workspace secret fixture").unwrap();
    sqlx::query(
        "INSERT INTO secret_ref_descriptors(
            credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
         ) VALUES (?, 'ssh_key', 'workspace-secret', 'hash', '{}', '2026-08-11T00:00:00Z')",
    )
    .bind(&credential_ref)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let delete = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/workspace")
                .header("origin", ORIGIN)
                .header("cookie", &cookie)
                .header("x-csrf-token", &csrf)
                .header("idempotency-key", "delete-workspace-default")
                .header("x-confirm-delete", "workspace:workspace-default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);
    let deleted = json_body(delete).await;
    assert_eq!(deleted["data"]["secret_cleanup"], "completed");
    assert!(!secret_path.exists());
    let workspaces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(workspaces, 0);
    let active_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM owner_sessions WHERE revoked_at IS NULL")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(active_sessions, 0);
    let revoked = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/bootstrap")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    let backup_path: String = sqlx::query_scalar(
        "SELECT storage_path FROM backup_records WHERE scope_kind = 'workspace' AND scope_id = 'workspace-default'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let backup_path = PathBuf::from(backup_path);
    assert!(
        backup_path
            .join("secrets")
            .join(format!("{credential_id}.key"))
            .is_file()
    );
    storage::verify_database_file(&backup_path.join("network-atlas.db"))
        .await
        .unwrap();

    let (relogin_cookie, relogin_csrf) = login(&fixture.app).await;
    let replay = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/workspace")
                .header("origin", ORIGIN)
                .header("cookie", &relogin_cookie)
                .header("x-csrf-token", &relogin_csrf)
                .header("idempotency-key", "delete-workspace-default")
                .header("x-confirm-delete", "workspace:workspace-default")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await["data"], deleted["data"]);
}
