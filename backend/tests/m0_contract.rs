use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use network_atlas::{api, storage};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const WORKSPACE_ID: &str = "workspace-default";

async fn test_app() -> axum::Router {
    let pool = storage::connect("sqlite::memory:")
        .await
        .expect("test database");
    api::router(api::AppState::new(pool), "../frontend")
}

async fn json_response(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .expect("request succeeds");
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&body).expect("JSON response"),
    )
}

#[tokio::test]
async fn health_checks_sqlite() {
    let (status, body) = json_response(test_app().await, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "network-atlas");
    let revision = body["build_revision"]
        .as_str()
        .expect("health response exposes a build revision");
    assert!(revision == "unknown" || revision.len() >= 7);
    let executable_sha256 = body["executable_sha256"]
        .as_str()
        .expect("health response exposes an executable digest");
    assert!(executable_sha256 == "unavailable" || executable_sha256.len() == 64);
}

#[tokio::test]
async fn empty_database_is_a_real_empty_workspace_in_http_mode() {
    let (status, body) = json_response(test_app().await, "/api/v1/bootstrap").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["data_source"]["kind"], "real");
    assert_eq!(body["meta"]["data_source"]["status"], "fresh");
    assert_eq!(body["data"]["projects"].as_array().unwrap().len(), 0);
    assert_eq!(body["data"]["hosts"].as_array().unwrap().len(), 0);
    assert_eq!(body["data"]["features"]["global_world"], true);
    assert_eq!(body["data"]["features"]["project_resources"], true);
    assert_eq!(body["data"]["features"]["project_workflow"], false);
    assert_eq!(body["data"]["features"]["agent_assistance"], true);
}

#[tokio::test]
async fn global_hosts_view_keeps_connection_discovery_and_inventory_separate() {
    let (status, body) = json_response(test_app().await, "/api/v1/views/global/hosts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["data_source"]["kind"], "real");
    assert_eq!(body["data"]["host_count"], 0);
    assert_eq!(body["data"]["connection_ready_count"], 0);
    assert_eq!(body["data"]["discovery_partial_count"], 0);
    assert!(body["data"]["hosts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn global_hosts_view_normalizes_connection_without_hiding_discovery_state() {
    let pool = storage::connect("sqlite::memory:")
        .await
        .expect("test database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES (?, 'owner-local', '2026-08-14T00:00:00Z')",
    )
    .bind(WORKSPACE_ID)
    .execute(&pool)
    .await
    .expect("workspace");
    for (index, state) in [
        "discovery_complete",
        "discovery_partial",
        "discovery_unavailable",
    ]
    .iter()
    .enumerate()
    {
        let host_id = format!("host-state-{index}");
        let timestamp = format!("2026-08-14T00:00:0{index}Z");
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
                host_key_state, transport, os, status, created_at, last_checked_at
             ) VALUES (?, ?, ?, ?, 22, 'fixture', ?, 'verified', 'ssh', 'linux', ?, ?, ?)",
        )
        .bind(&host_id)
        .bind(WORKSPACE_ID)
        .bind(format!("HOST {index}"))
        .bind(format!("192.0.2.{}", index + 1))
        .bind(format!("secret://ssh/state-{index}"))
        .bind(state)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&pool)
        .await
        .expect("host");
        sqlx::query(
            "INSERT INTO discovery_runs(
                run_id, host_id, request_id, idempotency_key, protocol_version, state,
                response_json, submitted_at, finished_at, evidence_retention
             ) VALUES (?, ?, ?, ?, '1', ?, '{}', ?, ?, 'summary')",
        )
        .bind(format!("run-state-{index}"))
        .bind(&host_id)
        .bind(format!("request-state-{index}"))
        .bind(format!("key-state-{index}"))
        .bind(state)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&pool)
        .await
        .expect("discovery run");
    }

    let app = api::router(api::AppState::new(pool), "../frontend");
    let (status, body) = json_response(app, "/api/v1/views/global/hosts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["connection_ready_count"], 3);
    assert_eq!(body["data"]["connection_failed_count"], 0);
    assert_eq!(body["data"]["discovery_partial_count"], 1);
    for (index, expected) in [
        "discovery_complete",
        "discovery_partial",
        "discovery_unavailable",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            body["data"]["hosts"][index]["connection_state"],
            "connection_ready"
        );
        assert_eq!(body["data"]["hosts"][index]["discovery_state"], *expected);
        assert_eq!(body["data"]["hosts"][index]["host"]["status"], *expected);
    }
}

#[tokio::test]
async fn graph_contract_carries_source_state_time_and_layout() {
    let (status, world) = json_response(test_app().await, "/api/v1/views/global/world").await;
    assert_eq!(status, StatusCode::OK);
    let nodes = world["data"]["nodes"].as_array().unwrap();
    let edges = world["data"]["edges"].as_array().unwrap();
    assert!(nodes.is_empty());
    assert!(edges.is_empty());
    assert_eq!(world["meta"]["data_source"]["kind"], "real");
    assert_eq!(world["data"]["layout"]["revision"], 0);
}

#[tokio::test]
async fn missing_project_uses_unified_error_envelope() {
    let (status, body) = json_response(
        test_app().await,
        "/api/v1/projects/not-found/views/resources",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert!(body["error"]["request_id"].is_string());
}

#[tokio::test]
async fn unknown_api_route_never_falls_through_to_the_frontend() {
    let (status, body) = json_response(test_app().await, "/api/v1/not-a-route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert_eq!(body["error"]["details"]["resource"], "api_route");
}

#[tokio::test]
async fn openapi_is_generated_from_rust_routes_and_contains_m0_schemas() {
    let document = api::openapi();
    let value = serde_json::to_value(document).unwrap();
    assert!(value["paths"]["/api/v1/bootstrap"].is_object());
    assert!(value["paths"]["/api/v1/views/global/world"].is_object());
    assert!(value["paths"]["/api/v1/views/global/hosts"].is_object());
    assert!(value["paths"]["/api/v1/projects/{project_id}/views/resources"].is_object());
    assert!(value["components"]["schemas"]["GraphSnapshot"].is_object());
    assert!(value["components"]["schemas"]["TaskSummary"].is_object());
    assert!(value["components"]["schemas"]["ProjectionDraftSummary"].is_object());
    assert!(value["paths"]["/api/v1/secret-refs"].is_object());
    assert!(value["paths"]["/api/v1/hosts/{host_id}/connection-tests"].is_object());
    assert!(value["paths"]["/api/v1/hosts/{host_id}"]["patch"].is_object());
    assert!(value["paths"]["/api/v1/hosts/{host_id}/host-key-confirmations"].is_object());
    assert!(value["paths"]["/api/v1/hosts/{host_id}/discovery-runs"].is_object());
    assert!(value["paths"]["/api/v1/hosts/{host_id}/monitoring"].is_object());
    assert!(value["paths"]["/api/v1/hosts/{host_id}/monitor-runs"]["post"].is_object());
    assert!(value["paths"]["/api/v1/monitor-runs/{run_id}"].is_object());
    assert!(
        value["paths"]["/api/v1/hosts/{host_id}/discovery-runs"]["post"]["responses"]["400"]
            .is_object()
    );
    assert!(value["paths"]["/api/v1/discovery-runs/{run_id}/evidence"].is_object());
    assert!(value["components"]["schemas"]["DiscoveryEvidence"].is_object());
    assert!(value["components"]["schemas"]["GlobalHostsViewResponse"].is_object());
    assert!(value["components"]["schemas"]["HostResourceSnapshot"].is_object());
    assert!(value["components"]["schemas"]["MonitorRunRecord"].is_object());
    assert!(value["components"]["schemas"]["MonitorFreshness"].is_object());
    assert!(value["components"]["schemas"]["MetricHistoryRollupStatistics"].is_object());
    assert!(value["components"]["schemas"]["MonitoringHistoryRetentionPolicy"].is_object());
    assert!(value["components"]["schemas"]["MonitoringHistoryMaintenanceStatus"].is_object());
    assert!(value["paths"]["/api/v1/businesses"].is_object());
    assert!(value["paths"]["/api/v1/businesses/{business_id}/project-links"].is_object());
    assert!(value["paths"]["/api/v1/views/global/resources"].is_object());
    assert!(value["components"]["schemas"]["BusinessRecord"].is_object());
    assert!(value["components"]["schemas"]["GlobalResourceViewResponse"].is_object());
    for field in [
        "latest_discovery_run_id",
        "latest_evidence_run_id",
        "latest_projection_draft_id",
        "latest_monitor_run",
        "current_snapshot_run_id",
        "monitor_observed_at",
        "monitor_freshness",
        "monitor_unknown_count",
    ] {
        assert!(value["components"]["schemas"]["HostAssetRecord"]["properties"][field].is_object());
    }
}

#[tokio::test]
async fn registered_hosts_without_scans_are_real_and_never_expand_fixture_projects() {
    let pool = storage::connect("sqlite::memory:")
        .await
        .expect("test database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES (?, 'owner-local', '2026-08-12T00:00:00Z')",
    )
    .bind(WORKSPACE_ID)
    .execute(&pool)
    .await
    .expect("workspace");
    for (index, address) in ["192.0.2.10", "192.0.2.11", "192.0.2.12"]
        .iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
                host_key_state, transport, os, status, created_at, last_checked_at
             ) VALUES (?, ?, ?, ?, 22, 'root', ?, 'unverified', 'ssh', 'linux', 'host_registered', ?, ?)",
        )
        .bind(format!("host-{}", index + 1))
        .bind(WORKSPACE_ID)
        .bind(format!("HOST_{}", index + 1))
        .bind(address)
        .bind(format!("secret://ssh/host-{}", index + 1))
        .bind(format!("2026-08-12T00:00:0{}Z", index + 1))
        .bind(format!("2026-08-12T01:00:0{}Z", index + 1))
        .execute(&pool)
        .await
        .expect("host");
    }
    let app = api::router(api::AppState::new(pool), "../frontend");
    let (status, bootstrap) = json_response(app.clone(), "/api/v1/bootstrap").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bootstrap["meta"]["data_source"]["kind"], "real");
    assert_eq!(bootstrap["data"]["hosts"].as_array().unwrap().len(), 3);
    assert_eq!(bootstrap["data"]["projects"].as_array().unwrap().len(), 0);
    assert_eq!(bootstrap["data"]["navigation"]["projects"], 0);

    let (status, hosts_view) = json_response(app.clone(), "/api/v1/views/global/hosts").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hosts_view["data"]["host_count"], 3);
    assert_eq!(hosts_view["data"]["connection_ready_count"], 0);
    assert_eq!(hosts_view["data"]["unknown_count"], 3);
    assert_eq!(hosts_view["data"]["hosts"][0]["deployment_count"], 0);
    assert_eq!(hosts_view["data"]["hosts"][0]["project_count"], 0);
    assert_eq!(
        hosts_view["data"]["hosts"][0]["host"]["last_checked_at"],
        "2026-08-12T01:00:01Z"
    );
    assert!(hosts_view["data"]["hosts"][0]["last_observed_at"].is_null());
    assert_eq!(hosts_view["data"]["hosts"][0]["freshness"], "unavailable");
    let unscanned_host = hosts_view["data"]["hosts"][0]
        .as_object()
        .expect("host asset record");
    for field in [
        "latest_discovery_run_id",
        "latest_evidence_run_id",
        "latest_projection_draft_id",
    ] {
        assert!(unscanned_host.contains_key(field), "missing {field}");
        assert!(unscanned_host[field].is_null(), "{field} must be null");
    }
    assert!(
        hosts_view["data"]["hosts"][0]["provider_coverage"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let (status, world) = json_response(app, "/api/v1/views/global/world").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(world["meta"]["data_source"]["kind"], "real");
    let nodes = world["data"]["nodes"].as_array().unwrap();
    let edges = world["data"]["edges"].as_array().unwrap();
    assert!(nodes.is_empty());
    assert!(edges.is_empty());
}

#[tokio::test]
async fn global_hosts_keeps_last_evidence_inventory_while_a_new_run_is_running_or_failed() {
    let pool = storage::connect("sqlite::memory:")
        .await
        .expect("test database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES (?, 'owner-local', '2026-08-14T00:00:00Z')",
    )
    .bind(WORKSPACE_ID)
    .execute(&pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at, last_checked_at
         ) VALUES (
            'host-evidence-history', ?, 'Evidence history', '192.0.2.20', 22, 'fixture',
            'secret://ssh/evidence-history', 'verified', 'ssh', 'linux', 'discovery_running',
            '2026-08-14T00:00:00Z', '2026-08-14T00:02:00Z'
         )",
    )
    .bind(WORKSPACE_ID)
    .execute(&pool)
    .await
    .expect("host");

    let evidence = serde_json::json!({
        "protocol_version": "1",
        "discovery_id": "run-with-evidence",
        "host": {"host_id": "host-evidence-history", "address": "192.0.2.20", "os": "linux"},
        "host_facts": [],
        "docker_engines": [],
        "compose_projects": [],
        "systemd_units": [{
            "external_id": "systemd:fixture.service",
            "kind": "systemd_unit",
            "source": "ssh:host-evidence-history:systemd_units",
            "observed_at": "2026-08-14T00:01:00Z",
            "freshness": "fresh",
            "redaction_state": "metadata_only",
            "metadata": {"unit": "fixture.service", "active": "active", "sub": "running"}
        }],
        "containers": [],
        "images": [],
        "networks": [],
        "volumes": [],
        "document_candidates": [],
        "health_checks": [],
        "warnings": [],
        "provider_results": [{
            "provider_kind": "systemd",
            "status": "ready",
            "observed_count": 1,
            "evidence_refs": ["systemd:fixture.service"],
            "warnings": [],
            "observed_at": "2026-08-14T00:01:00Z"
        }],
        "started_at": "2026-08-14T00:00:59Z",
        "finished_at": "2026-08-14T00:01:00Z"
    });
    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version, state, response_json,
            submitted_at, finished_at, evidence_json, evidence_retention
         ) VALUES (
            'run-with-evidence', 'host-evidence-history', 'request-evidence', 'key-evidence', '1',
            'discovery_complete', '{}', '2026-08-14T00:00:59Z', '2026-08-14T00:01:00Z', ?, 'complete'
         )",
    )
    .bind(evidence.to_string())
    .execute(&pool)
    .await
    .expect("evidence run");
    let snapshot = serde_json::json!({
        "focus": {"kind": "global", "id": WORKSPACE_ID},
        "nodes": [],
        "edges": [],
        "layout": {"layout_id": "layout-pointer", "scope": "global", "revision": 1}
    });
    sqlx::query(
        "INSERT INTO projection_drafts(
            draft_id, workspace_id, discovery_run_id, host_id, base_revision, revision, state,
            pending_changes, snapshot_json, created_at, updated_at
         ) VALUES (
            'draft-with-evidence', ?, 'run-with-evidence', 'host-evidence-history', 0, 1, 'draft',
            0, ?, '2026-08-14T00:01:01Z', '2026-08-14T00:01:01Z'
         )",
    )
    .bind(WORKSPACE_ID)
    .bind(snapshot.to_string())
    .execute(&pool)
    .await
    .expect("projection draft");
    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version, state, response_json,
            submitted_at, started_at, evidence_retention
         ) VALUES (
            'run-newer', 'host-evidence-history', 'request-newer', 'key-newer', '1', 'running',
            '{}', '2026-08-14T00:02:00Z', '2026-08-14T00:02:00Z', 'complete'
         )",
    )
    .execute(&pool)
    .await
    .expect("running run");
    sqlx::query(
        "INSERT INTO projection_drafts(
            draft_id, workspace_id, discovery_run_id, host_id, base_revision, revision, state,
            pending_changes, snapshot_json, created_at, updated_at
         ) VALUES (
            'draft-malformed-newer', ?, 'run-newer', 'host-evidence-history', 0, 1, 'draft',
            0, '{}', '2026-08-14T00:02:01Z', '2026-08-14T00:02:01Z'
         )",
    )
    .bind(WORKSPACE_ID)
    .execute(&pool)
    .await
    .expect("malformed newer projection draft");

    let app = api::router(api::AppState::new(pool.clone()), "../frontend");
    let (status, running_view) = json_response(app.clone(), "/api/v1/views/global/hosts").await;
    assert_eq!(status, StatusCode::OK);
    let running = &running_view["data"]["hosts"][0];
    assert_eq!(running["discovery_state"], "running");
    assert_eq!(running["latest_discovery_run_id"], "run-newer");
    assert_eq!(running["latest_evidence_run_id"], "run-with-evidence");
    assert_eq!(running["latest_projection_draft_id"], "draft-with-evidence");
    assert_eq!(running["deployment_count"], 1);
    assert_eq!(running["provider_coverage"][0]["provider_kind"], "systemd");
    assert_eq!(running["last_observed_at"], "2026-08-14T00:01:00Z");
    assert_eq!(running["freshness"], "fresh");

    sqlx::query(
        "UPDATE discovery_runs
         SET state = 'docker_unavailable', failure_code = 'DOCKER_UNAVAILABLE',
             failure_summary = 'Docker is unavailable', finished_at = '2026-08-14T00:02:01Z',
             evidence_retention = 'summary', evidence_json = ?
         WHERE run_id = 'run-newer'",
    )
    .bind("{malformed")
    .execute(&pool)
    .await
    .expect("failed run");
    let (status, failed_view) = json_response(app, "/api/v1/views/global/hosts").await;
    assert_eq!(status, StatusCode::OK);
    let failed = &failed_view["data"]["hosts"][0];
    assert_eq!(failed["discovery_state"], "docker_unavailable");
    assert_eq!(failed["latest_discovery_run_id"], "run-newer");
    assert_eq!(failed["latest_evidence_run_id"], "run-with-evidence");
    assert_eq!(failed["latest_projection_draft_id"], "draft-with-evidence");
    assert_eq!(failed["deployment_count"], 1);
    assert_eq!(failed["provider_coverage"][0]["provider_kind"], "systemd");
    assert_eq!(failed["last_observed_at"], "2026-08-14T00:01:00Z");
    assert_eq!(failed["freshness"], "fresh");
}

#[tokio::test]
async fn file_database_uses_wal_and_migrations_are_repeatable() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("m0.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = storage::connect(&url).await.unwrap();
    storage::migrate(&pool).await.unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('workspaces','projects','entities','entity_relations','projection_drafts','projection_versions','canvas_layouts')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(table_count, 7);
    let migration_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    // H6 adds the Project Agent binding and typed-tool audit tables.
    assert_eq!(migration_version, 16);
    let nullable_request_hash: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('discovery_runs')
         WHERE name = 'request_sha256' AND [notnull] = 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nullable_request_hash, 1);
}

#[tokio::test]
async fn monitoring_current_rejects_cross_host_runs_and_survives_receipt_cleanup() {
    let pool = storage::connect("sqlite::memory:")
        .await
        .expect("test database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    for host_id in ["host-a", "host-b"] {
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, transport, os, status, created_at
             ) VALUES (?, 'workspace-default', ?, ?, 22, 'fixture', ?, 'verified',
                'ssh', 'linux', 'connection_ready', '2026-08-15T00:00:00Z')",
        )
        .bind(host_id)
        .bind(host_id)
        .bind(format!("{host_id}.invalid"))
        .bind(format!("secret://ssh/{host_id}"))
        .execute(&pool)
        .await
        .expect("host");
    }
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, submitted_at, accepted_response_json
         ) VALUES ('run-b', 'host-b', 'request-b', 'key-b', 'sha-b',
            'host_resource_v1', 'manual', 'succeeded', '2026-08-15T00:00:00Z', '{}')",
    )
    .execute(&pool)
    .await
    .expect("monitor run");
    let cross_host = sqlx::query(
        "INSERT INTO monitoring_current(
            host_id, run_id, profile, collector_version, snapshot_json, coverage_json,
            metric_count, unknown_count, observed_at, valid_until, snapshot_sha256, updated_at
         ) VALUES ('host-a', 'run-b', 'host_resource_v1', 'v1', '{}', '[]', 0, 8,
            '2026-08-15T00:00:00Z', '2026-08-15T00:15:00Z', 'sha', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    assert!(cross_host.is_err(), "cross-HOST current pointer must fail");

    sqlx::query(
        "INSERT INTO monitoring_current(
            host_id, run_id, profile, collector_version, snapshot_json, coverage_json,
            metric_count, unknown_count, observed_at, valid_until, snapshot_sha256, updated_at
         ) VALUES ('host-b', 'run-b', 'host_resource_v1', 'v1', '{}', '[]', 0, 8,
            '2026-08-15T00:00:00Z', '2026-08-15T00:15:00Z', 'sha', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("matching current pointer");
    sqlx::query("DELETE FROM monitor_runs WHERE run_id = 'run-b'")
        .execute(&pool)
        .await
        .expect("receipt cleanup");
    let current_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitoring_current WHERE host_id = 'host-b'")
            .fetch_one(&pool)
            .await
            .expect("current survives");
    assert_eq!(current_count, 1);
}
