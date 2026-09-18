use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
};
use network_atlas::{
    api::AppState,
    api::catalog_api,
    auth, catalog,
    contracts::{
        DiscoveryEvidence, DiscoveryProviderCoverage, DiscoveryProviderStatus,
        EvidenceHostIdentity, EvidenceItem, EvidenceKind, Freshness, ProjectTargetCreateRequest,
        ProjectTargetState, ProjectTargetUpdateRequest, RedactionState,
        TechnicalProjectCreateRequest,
    },
    data_management,
    events::{self, ChangeEventKind},
    storage,
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};

async fn database() -> SqlitePool {
    let pool = storage::connect("sqlite::memory:").await.expect("database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-fixture', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at
         ) VALUES ('host-h4', 'workspace-default', 'H4 host', '127.0.0.1', 22,
                   'fixture', 'secret://fixture', 'verified', 'ssh', 'linux',
                   'connection_ready', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("host");
    pool
}

async fn run(pool: &SqlitePool, run_id: &str) {
    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version, state,
            response_json, submitted_at
         ) VALUES (?, 'host-h4', ?, ?, '1', 'discovery_complete', '{}',
                   '2026-08-15T00:00:00Z')",
    )
    .bind(run_id)
    .bind(format!("request-{run_id}"))
    .bind(format!("key-{run_id}"))
    .execute(pool)
    .await
    .expect("discovery run");
}

fn evidence(run_id: &str, provider_status: DiscoveryProviderStatus) -> DiscoveryEvidence {
    let observed_at = "2026-08-15T00:01:00Z".to_owned();
    let compose = item(
        "compose:orders",
        EvidenceKind::ComposeProject,
        json!({"name": "orders", "status": "running(1)"}),
        &observed_at,
        "ssh:compose_ls",
    );
    let compose_container = item(
        "container:orders-api",
        EvidenceKind::Container,
        json!({"id": "container:orders-api", "name": "orders-api", "compose_project": "orders", "compose_service": "api"}),
        &observed_at,
        "ssh:containers",
    );
    let standalone = item(
        "container:sidecar",
        EvidenceKind::Container,
        json!({"id": "container:sidecar", "name": "sidecar", "state": "running"}),
        &observed_at,
        "ssh:containers",
    );
    DiscoveryEvidence {
        protocol_version: "1".to_owned(),
        discovery_id: run_id.to_owned(),
        host: EvidenceHostIdentity {
            host_id: "host-h4".to_owned(),
            address: "127.0.0.1".to_owned(),
            os: "linux".to_owned(),
        },
        host_facts: Vec::new(),
        docker_engines: Vec::new(),
        compose_projects: vec![compose],
        systemd_units: Vec::new(),
        containers: vec![compose_container, standalone],
        images: Vec::new(),
        networks: Vec::new(),
        volumes: Vec::new(),
        document_candidates: Vec::new(),
        health_checks: Vec::new(),
        warnings: Vec::new(),
        provider_results: vec![
            coverage("compose", provider_status.clone(), 1, &observed_at),
            coverage("docker", provider_status, 1, &observed_at),
        ],
        started_at: "2026-08-15T00:00:30Z".to_owned(),
        finished_at: observed_at,
    }
}

fn set_observed_at(evidence: &mut DiscoveryEvidence, observed_at: &str) {
    for item in evidence
        .compose_projects
        .iter_mut()
        .chain(evidence.systemd_units.iter_mut())
        .chain(evidence.containers.iter_mut())
        .chain(evidence.images.iter_mut())
        .chain(evidence.networks.iter_mut())
        .chain(evidence.volumes.iter_mut())
        .chain(evidence.document_candidates.iter_mut())
    {
        item.observed_at = observed_at.to_owned();
    }
    evidence.finished_at = observed_at.to_owned();
    for provider in &mut evidence.provider_results {
        provider.observed_at = Some(observed_at.to_owned());
    }
}

fn coverage(
    provider_kind: &str,
    status: DiscoveryProviderStatus,
    observed_count: u32,
    observed_at: &str,
) -> DiscoveryProviderCoverage {
    DiscoveryProviderCoverage {
        provider_kind: provider_kind.to_owned(),
        status,
        observed_count,
        evidence_refs: Vec::new(),
        warnings: Vec::new(),
        observed_at: Some(observed_at.to_owned()),
    }
}

fn item(
    external_id: &str,
    kind: EvidenceKind,
    metadata: Value,
    observed_at: &str,
    source: &str,
) -> EvidenceItem {
    EvidenceItem {
        external_id: external_id.to_owned(),
        kind,
        source: source.to_owned(),
        observed_at: observed_at.to_owned(),
        freshness: Freshness::Fresh,
        sha256: None,
        redaction_state: RedactionState::MetadataOnly,
        metadata,
    }
}

async fn persist(pool: &SqlitePool, run_id: &str, evidence: &DiscoveryEvidence) {
    let mut tx = pool.begin().await.expect("transaction");
    catalog::persist_discovery_success_in(&mut tx, run_id, "host-h4", evidence)
        .await
        .expect("catalog persistence");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn discovery_materializes_stable_deployments_without_compose_duplicates() {
    let pool = database().await;
    run(&pool, "run-h4-1").await;
    persist(
        &pool,
        "run-h4-1",
        &evidence("run-h4-1", DiscoveryProviderStatus::Ready),
    )
    .await;

    let rows = sqlx::query(
        "SELECT provider_kind, external_id, display_name FROM deployments
         ORDER BY provider_kind, external_id",
    )
    .fetch_all(&pool)
    .await
    .expect("deployments");
    let values = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("provider_kind"),
                row.get::<String, _>("external_id"),
                row.get::<String, _>("display_name"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert!(
        values
            .iter()
            .any(|(provider, id, name)| provider == "compose"
                && id == "compose:orders"
                && name == "orders")
    );
    assert!(
        values
            .iter()
            .any(|(provider, id, name)| provider == "docker"
                && id == "container:sidecar"
                && name == "sidecar")
    );
    assert!(!values.iter().any(|(_, id, _)| id == "container:orders-api"));
    let observations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deployment_observations")
        .fetch_one(&pool)
        .await
        .expect("observations");
    assert_eq!(observations, 2);
}

#[tokio::test]
async fn discovery_materializes_only_explicit_deployment_resource_references() {
    let pool = database().await;
    run(&pool, "run-h4-resources").await;
    let mut observed = evidence("run-h4-resources", DiscoveryProviderStatus::Ready);
    observed.containers[0].metadata["networks"] = json!(["orders-net"]);
    observed.networks.push(item(
        "network:orders-net",
        EvidenceKind::Network,
        json!({"name": "orders-net", "driver": "bridge"}),
        "2026-08-15T00:01:00Z",
        "ssh:networks",
    ));
    observed.volumes.push(item(
        "volume:unreferenced",
        EvidenceKind::Volume,
        json!({"name": "not-mounted"}),
        "2026-08-15T00:01:00Z",
        "ssh:volumes",
    ));
    persist(&pool, "run-h4-resources", &observed).await;

    let resources: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM resource_entities")
        .fetch_one(&pool)
        .await
        .expect("resource count");
    assert_eq!(resources, 2);
    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deployment_resource_links")
        .fetch_one(&pool)
        .await
        .expect("resource link count");
    assert_eq!(links, 1, "co-location must not create a resource edge");
    let provider: String = sqlx::query_scalar(
        "SELECT deployments.provider_kind FROM deployment_resource_links links
         JOIN deployments ON deployments.deployment_id = links.deployment_id",
    )
    .fetch_one(&pool)
    .await
    .expect("linked deployment provider");
    assert_eq!(provider, "compose");
    let source_refs: String =
        sqlx::query_scalar("SELECT source_refs_json FROM deployment_resource_links LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("resource provenance");
    let source_refs: Vec<String> = serde_json::from_str(&source_refs).expect("source refs");
    assert!(
        source_refs
            .iter()
            .any(|value| value == "discovery:run-h4-resources")
    );

    run(&pool, "run-h4-resources-missing").await;
    persist(
        &pool,
        "run-h4-resources-missing",
        &evidence("run-h4-resources-missing", DiscoveryProviderStatus::Ready),
    )
    .await;
    let stale_state: String =
        sqlx::query_scalar("SELECT state FROM deployment_resource_links LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("stale relationship");
    assert_eq!(stale_state, "stale");

    run(&pool, "run-h4-resources-recovered").await;
    let mut recovered = evidence("run-h4-resources-recovered", DiscoveryProviderStatus::Ready);
    recovered.containers[0].metadata["networks"] = json!(["orders-net"]);
    recovered.networks.push(item(
        "network:orders-net",
        EvidenceKind::Network,
        json!({"name": "orders-net", "driver": "bridge"}),
        "2026-08-15T00:02:00Z",
        "ssh:networks",
    ));
    persist(&pool, "run-h4-resources-recovered", &recovered).await;
    let recovered_state: String =
        sqlx::query_scalar("SELECT state FROM deployment_resource_links LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("recovered relationship");
    assert_eq!(recovered_state, "observed");

    sqlx::query(
        "UPDATE deployment_resource_links
         SET state = 'confirmed', origin = 'user_declared',
             source_refs_json = '[\"user:fixture\"]'",
    )
    .execute(&pool)
    .await
    .expect("confirm resource link");
    run(&pool, "run-h4-resources-rescan").await;
    observed.discovery_id = "run-h4-resources-rescan".to_owned();
    persist(&pool, "run-h4-resources-rescan", &observed).await;
    let confirmed = sqlx::query(
        "SELECT state, origin, source_refs_json FROM deployment_resource_links LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("confirmed link after rescan");
    assert_eq!(confirmed.get::<String, _>("state"), "confirmed");
    assert_eq!(confirmed.get::<String, _>("origin"), "user_declared");
    assert_eq!(
        confirmed.get::<String, _>("source_refs_json"),
        "[\"user:fixture\"]"
    );
}

#[tokio::test]
async fn unavailable_provider_records_unknown_without_marking_deployment_missing() {
    let pool = database().await;
    run(&pool, "run-h4-ready").await;
    persist(
        &pool,
        "run-h4-ready",
        &evidence("run-h4-ready", DiscoveryProviderStatus::Ready),
    )
    .await;
    run(&pool, "run-h4-unavailable").await;
    let mut next = evidence("run-h4-unavailable", DiscoveryProviderStatus::Unavailable);
    next.compose_projects.clear();
    next.containers.clear();
    persist(&pool, "run-h4-unavailable", &next).await;

    let row = sqlx::query(
        "SELECT freshness FROM deployments WHERE provider_kind = 'compose' AND external_id = 'compose:orders'",
    )
    .fetch_one(&pool)
    .await
    .expect("deployment");
    assert_eq!(row.get::<String, _>("freshness"), "unavailable");
    let state: String = sqlx::query_scalar(
        "SELECT observation_state FROM deployment_observations
         WHERE discovery_run_id = 'run-h4-unavailable' AND external_id = 'compose:orders'",
    )
    .fetch_one(&pool)
    .await
    .expect("unknown observation");
    assert_eq!(state, "unknown");
}

#[tokio::test]
async fn observations_are_append_only_and_identity_is_enforced() {
    let pool = database().await;
    run(&pool, "run-h4-immutable").await;
    persist(
        &pool,
        "run-h4-immutable",
        &evidence("run-h4-immutable", DiscoveryProviderStatus::Ready),
    )
    .await;
    let result = sqlx::query(
        "UPDATE deployment_observations SET metadata_json = '{}' WHERE discovery_run_id = 'run-h4-immutable'",
    )
    .execute(&pool)
    .await;
    assert!(result.is_err());
    let deployment_id: String = sqlx::query_scalar("SELECT deployment_id FROM deployments LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("deployment id");
    let bad = sqlx::query(
        "INSERT INTO deployment_observations(
            deployment_observation_id, deployment_id, discovery_run_id, provider_kind, external_id,
            observation_state, metadata_json, created_at
         ) VALUES ('11111111-1111-4111-8111-111111111111', ?, 'run-h4-immutable', 'systemd',
                   'wrong', 'observed', '{}', '2026-08-15T00:00:00Z')",
    )
    .bind(deployment_id)
    .execute(&pool)
    .await;
    assert!(bad.is_err());
}

#[tokio::test]
async fn catalog_audit_and_change_event_kinds_satisfy_storage_constraints() {
    let pool = database().await;
    auth::record_audit(
        &pool,
        "owner-fixture",
        "catalog.changed",
        "/api/v1/project-targets",
        StatusCode::CREATED,
        "request-h4-catalog-audit".to_owned(),
        json!({"method": "POST"}),
    )
    .await;
    let audit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE kind = 'catalog.changed'")
            .fetch_one(&pool)
            .await
            .expect("catalog audit");
    assert_eq!(audit_count, 1);

    events::publish(
        &pool,
        ChangeEventKind::ProjectionChanged,
        "catalog:project-target:fixture",
        1,
        json!({"catalog_kind": "project-target"}),
    )
    .await
    .expect("catalog change event");
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM change_events WHERE kind = 'projection.changed'")
            .fetch_one(&pool)
            .await
            .expect("projection change event");
    assert_eq!(event_count, 1);
}

#[tokio::test]
async fn technical_project_and_target_commands_are_idempotent_and_revision_checked() {
    let pool = database().await;
    run(&pool, "run-h4-api").await;
    persist(
        &pool,
        "run-h4-api",
        &evidence("run-h4-api", DiscoveryProviderStatus::Ready),
    )
    .await;
    let state = AppState::new(pool.clone());
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", HeaderValue::from_static("tp-create-1"));
    let request = TechnicalProjectCreateRequest {
        display_name: "Orders API".to_owned(),
        summary: Some("read-only fixture".to_owned()),
    };
    let first = catalog_api::create_technical_project(
        State(state.clone()),
        headers.clone(),
        Json(request.clone()),
    )
    .await
    .expect("create technical project")
    .1
    .0;
    let replay =
        catalog_api::create_technical_project(State(state.clone()), headers, Json(request))
            .await
            .expect("idempotent replay")
            .1
            .0;
    assert_eq!(
        first.data.technical_project_id,
        replay.data.technical_project_id
    );
    let deployment_id: String = sqlx::query_scalar(
        "SELECT deployment_id FROM deployments WHERE provider_kind = 'compose' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("deployment");
    let mut target_headers = HeaderMap::new();
    target_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("target-create-1"),
    );
    let target = catalog_api::create_project_target(
        State(state.clone()),
        target_headers,
        Json(ProjectTargetCreateRequest {
            technical_project_id: first.data.technical_project_id.clone(),
            deployment_id,
            display_name: None,
        }),
    )
    .await
    .expect("create target")
    .1
    .0;
    assert_eq!(target.data.state, ProjectTargetState::Confirmed);
    assert_eq!(
        target.data.last_observed_at.as_deref(),
        Some("2026-08-15T00:01:00Z")
    );
    let mut bad_headers = HeaderMap::new();
    bad_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("target-update-bad"),
    );
    bad_headers.insert("if-match", HeaderValue::from_static("revision-2"));
    let update = catalog_api::update_project_target(
        State(state),
        Path(target.data.project_target_id),
        bad_headers,
        Json(ProjectTargetUpdateRequest {
            display_name: Some("renamed".to_owned()),
            state: None,
            base_revision: None,
        }),
    )
    .await;
    assert!(update.is_err());
}

#[tokio::test]
async fn target_freshness_tracks_missing_and_recovered_deployment_observations() {
    let pool = database().await;
    run(&pool, "run-h4-target-state-1").await;
    persist(
        &pool,
        "run-h4-target-state-1",
        &evidence("run-h4-target-state-1", DiscoveryProviderStatus::Ready),
    )
    .await;
    let state = AppState::new(pool.clone());
    let mut project_headers = HeaderMap::new();
    project_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("target-state-project"),
    );
    let project = catalog_api::create_technical_project(
        State(state.clone()),
        project_headers,
        Json(TechnicalProjectCreateRequest {
            display_name: "Target state fixture".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project")
    .1
    .0;
    let deployment_id: String = sqlx::query_scalar(
        "SELECT deployment_id FROM deployments
         WHERE provider_kind = 'compose' AND external_id = 'compose:orders'",
    )
    .fetch_one(&pool)
    .await
    .expect("deployment");
    let mut target_headers = HeaderMap::new();
    target_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("target-state-create"),
    );
    let target = catalog_api::create_project_target(
        State(state.clone()),
        target_headers,
        Json(ProjectTargetCreateRequest {
            technical_project_id: project.data.technical_project_id.clone(),
            deployment_id: deployment_id.clone(),
            display_name: None,
        }),
    )
    .await
    .expect("target")
    .1
    .0;
    assert_eq!(target.data.state, ProjectTargetState::Confirmed);
    assert_eq!(
        target.data.last_observed_at.as_deref(),
        Some("2026-08-15T00:01:00Z")
    );

    run(&pool, "run-h4-target-state-missing").await;
    let mut missing = evidence(
        "run-h4-target-state-missing",
        DiscoveryProviderStatus::Ready,
    );
    missing.compose_projects.clear();
    missing.containers.clear();
    persist(&pool, "run-h4-target-state-missing", &missing).await;
    let state_after_missing: String =
        sqlx::query_scalar("SELECT state FROM project_targets WHERE project_target_id = ?")
            .bind(&target.data.project_target_id)
            .fetch_one(&pool)
            .await
            .expect("stale target");
    assert_eq!(state_after_missing, "stale");
    let last_observed_after_missing: Option<String> = sqlx::query_scalar(
        "SELECT last_observed_at FROM project_targets WHERE project_target_id = ?",
    )
    .bind(&target.data.project_target_id)
    .fetch_one(&pool)
    .await
    .expect("last observed");
    assert_eq!(
        last_observed_after_missing.as_deref(),
        Some("2026-08-15T00:01:00Z")
    );

    let mut stale_project_headers = HeaderMap::new();
    stale_project_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("target-state-stale-project"),
    );
    let stale_project = catalog_api::create_technical_project(
        State(state.clone()),
        stale_project_headers,
        Json(TechnicalProjectCreateRequest {
            display_name: "Target state stale fixture".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("stale project")
    .1
    .0;
    let mut stale_target_headers = HeaderMap::new();
    stale_target_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("target-state-stale-create"),
    );
    let stale_target = catalog_api::create_project_target(
        State(state.clone()),
        stale_target_headers,
        Json(ProjectTargetCreateRequest {
            technical_project_id: stale_project.data.technical_project_id,
            deployment_id: deployment_id.clone(),
            display_name: None,
        }),
    )
    .await
    .expect("stale target")
    .1
    .0;
    assert_eq!(stale_target.data.state, ProjectTargetState::Stale);
    assert_eq!(stale_target.meta.freshness, Freshness::Stale);
    assert_eq!(
        stale_target.data.last_observed_at.as_deref(),
        Some("2026-08-15T00:01:00Z")
    );

    run(&pool, "run-h4-target-state-recovered").await;
    let mut recovered = evidence(
        "run-h4-target-state-recovered",
        DiscoveryProviderStatus::Ready,
    );
    set_observed_at(&mut recovered, "2026-08-15T00:02:00Z");
    persist(&pool, "run-h4-target-state-recovered", &recovered).await;
    let recovered_state: String =
        sqlx::query_scalar("SELECT state FROM project_targets WHERE project_target_id = ?")
            .bind(&target.data.project_target_id)
            .fetch_one(&pool)
            .await
            .expect("recovered target");
    assert_eq!(recovered_state, "confirmed");
    let recovered_last_observed: Option<String> = sqlx::query_scalar(
        "SELECT last_observed_at FROM project_targets WHERE project_target_id = ?",
    )
    .bind(&target.data.project_target_id)
    .fetch_one(&pool)
    .await
    .expect("recovered last observed");
    assert_eq!(
        recovered_last_observed.as_deref(),
        Some("2026-08-15T00:02:00Z")
    );
}

#[tokio::test]
async fn host_and_workspace_deletes_remove_catalog_dependents() {
    let pool = database().await;
    run(&pool, "run-h4-delete").await;
    persist(
        &pool,
        "run-h4-delete",
        &evidence("run-h4-delete", DiscoveryProviderStatus::Ready),
    )
    .await;
    let state = AppState::new(pool.clone());
    let mut project_headers = HeaderMap::new();
    project_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("delete-catalog-project"),
    );
    let project = catalog_api::create_technical_project(
        State(state),
        project_headers,
        Json(TechnicalProjectCreateRequest {
            display_name: "Delete cascade fixture".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project")
    .1
    .0;
    let deployment_id: String = sqlx::query_scalar("SELECT deployment_id FROM deployments LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("deployment");
    let mut target_headers = HeaderMap::new();
    target_headers.insert(
        "idempotency-key",
        HeaderValue::from_static("delete-catalog-target"),
    );
    let _target_data = catalog_api::create_project_target(
        State(AppState::new(pool.clone())),
        target_headers,
        Json(ProjectTargetCreateRequest {
            technical_project_id: project.data.technical_project_id,
            deployment_id,
            display_name: None,
        }),
    )
    .await
    .expect("target")
    .1
    .0;
    let mutation_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM catalog_mutation_requests
         WHERE resource_kind = 'project_target.create'",
    )
    .fetch_one(&pool)
    .await
    .expect("target mutation");
    assert_eq!(mutation_count, 1);

    // Exercise SQLite's real foreign-key cascade directly. The data-management
    // endpoint intentionally creates a VACUUM backup first, which is not
    // portable against Windows temporary-directory ACLs in this unit test.
    sqlx::query("DELETE FROM hosts WHERE host_id = 'host-h4'")
        .execute(&pool)
        .await
        .expect("host deletion");

    for table in ["deployments", "deployment_observations", "project_targets"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("catalog count");
        assert_eq!(count, 0, "{table} should cascade with HOST");
    }
    let projects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM technical_projects")
        .fetch_one(&pool)
        .await
        .expect("project remains");
    assert_eq!(projects, 1);
    sqlx::query("DELETE FROM workspaces WHERE workspace_id = 'workspace-default'")
        .execute(&pool)
        .await
        .expect("workspace deletion");
    for table in [
        "technical_projects",
        "deployments",
        "deployment_observations",
        "project_targets",
        "catalog_mutation_requests",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("workspace catalog count");
        assert_eq!(count, 0, "{table} should cascade with workspace");
    }
}

#[tokio::test]
async fn catalog_exports_are_separate_from_legacy_project_exports() {
    let pool = database().await;
    run(&pool, "run-h4-export").await;
    persist(
        &pool,
        "run-h4-export",
        &evidence("run-h4-export", DiscoveryProviderStatus::Ready),
    )
    .await;
    let state = AppState::new(pool.clone());
    let mut headers = HeaderMap::new();
    headers.insert(
        "idempotency-key",
        HeaderValue::from_static("export-tp-create"),
    );
    let created = catalog_api::create_technical_project(
        State(state.clone()),
        headers,
        Json(TechnicalProjectCreateRequest {
            display_name: "Exported project".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project")
    .1
    .0;
    let exported = data_management::export_technical_project(
        State(state),
        Path(created.data.technical_project_id),
    )
    .await
    .expect("technical project export")
    .0;
    assert_eq!(exported.data.scope_kind, "technical_project");
    assert!(exported.data.payload["technical_project"].is_object());
    assert!(exported.data.payload["deployments"].is_array());
    let workspace = data_management::export_workspace(State(AppState::new(pool)))
        .await
        .expect("workspace export")
        .0;
    assert!(workspace.data.payload["technical_projects"].is_array());
    assert!(workspace.data.payload["deployment_observations"].is_array());
}
