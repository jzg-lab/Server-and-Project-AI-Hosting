use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue},
};
use network_atlas::api::catalog_api::GlobalResourceQuery;
use network_atlas::{
    api::AppState,
    api::catalog_api,
    contracts::{
        BusinessCreateRequest, BusinessProjectLinkCreateRequest, BusinessProjectLinkState,
        BusinessState, GlobalResourceLens, TechnicalProjectCreateRequest,
    },
    storage,
};
use sqlx::SqlitePool;

async fn database() -> SqlitePool {
    let pool = storage::connect("sqlite::memory:").await.expect("database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-fixture', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    pool
}

fn idempotency(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", HeaderValue::from_static(value));
    headers
}

#[tokio::test]
async fn business_create_membership_and_archive_are_revisioned() {
    let pool = database().await;
    let state = AppState::new(pool.clone());
    let project = catalog_api::create_technical_project(
        State(state.clone()),
        idempotency("h5-project"),
        Json(TechnicalProjectCreateRequest {
            display_name: "Orders".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project")
    .1
    .0;
    let business = catalog_api::create_business(
        State(state.clone()),
        idempotency("h5-business"),
        Json(BusinessCreateRequest {
            display_name: "Commerce".to_owned(),
            summary: Some("customer orders".to_owned()),
            initial_project_ids: vec![project.data.technical_project_id.clone()],
        }),
    )
    .await
    .expect("business")
    .1
    .0;
    assert_eq!(business.data.state, BusinessState::Active);
    assert_eq!(
        business.data.origin,
        network_atlas::contracts::BusinessOrigin::UserDeclared
    );
    let links = catalog_api::list_business_project_links(
        State(state.clone()),
        Path(business.data.business_id.clone()),
    )
    .await
    .expect("links")
    .0;
    assert_eq!(links.data.links.len(), 1);
    assert_eq!(
        links.data.links[0].state,
        BusinessProjectLinkState::Confirmed
    );

    let duplicate = catalog_api::create_business_project_link(
        State(state.clone()),
        Path(business.data.business_id.clone()),
        idempotency("h5-relink"),
        Json(BusinessProjectLinkCreateRequest {
            technical_project_id: project.data.technical_project_id.clone(),
        }),
    )
    .await;
    assert!(duplicate.is_err());

    let mut delete_headers = idempotency("h5-unlink");
    delete_headers.insert("if-match", HeaderValue::from_static("revision-1"));
    let archived = catalog_api::delete_business_project_link(
        State(state.clone()),
        Path((
            business.data.business_id.clone(),
            project.data.technical_project_id.clone(),
        )),
        delete_headers,
    )
    .await
    .expect("archive link")
    .0;
    assert_eq!(archived.data.state, BusinessProjectLinkState::Archived);
}

#[tokio::test]
async fn resource_view_uses_explicit_targets_and_reports_shared_resources() {
    let pool = database().await;
    sqlx::query(
        "INSERT INTO hosts(host_id, workspace_id, display_name, address, port, ssh_user,
            credential_ref, host_key_state, transport, os, status, created_at)
         VALUES ('host-h5', 'workspace-default', 'Host H5', '127.0.0.1', 22, 'fixture',
            'secret://fixture', 'verified', 'ssh', 'linux', 'connection_ready',
            '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("host");
    let state = AppState::new(pool.clone());
    let project_a = catalog_api::create_technical_project(
        State(state.clone()),
        idempotency("h5-project-a"),
        Json(TechnicalProjectCreateRequest {
            display_name: "Project A".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project a")
    .1
    .0;
    let project_b = catalog_api::create_technical_project(
        State(state.clone()),
        idempotency("h5-project-b"),
        Json(TechnicalProjectCreateRequest {
            display_name: "Project B".to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project b")
    .1
    .0;
    sqlx::query(
        "INSERT INTO deployments(deployment_id, workspace_id, host_id, provider_kind,
            external_id, identity_key, display_name, catalog_state, freshness, created_at, updated_at)
         VALUES ('11111111-1111-4111-8111-111111111111', 'workspace-default', 'host-h5',
            'compose', 'compose:shared', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            'Shared gateway', 'observed', 'fresh', '2026-08-15T00:00:00Z', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("deployment");
    let mut target_ids = Vec::new();
    for (key, project_id) in [
        ("h5-target-a", &project_a.data.technical_project_id),
        ("h5-target-b", &project_b.data.technical_project_id),
    ] {
        let target = catalog_api::create_project_target(
            State(state.clone()),
            idempotency(key),
            Json(network_atlas::contracts::ProjectTargetCreateRequest {
                technical_project_id: project_id.clone(),
                deployment_id: "11111111-1111-4111-8111-111111111111".to_owned(),
                display_name: None,
            }),
        )
        .await
        .expect("target")
        .1
        .0;
        target_ids.push(target.data.project_target_id);
    }
    let resource_id = "22222222-2222-4222-8222-222222222222";
    sqlx::query(
        "INSERT INTO resource_entities(resource_entity_id, workspace_id, resource_kind, source,
            external_id, display_name, freshness, metadata_json, created_at, updated_at)
         VALUES (?, 'workspace-default', 'network', 'fixture', 'network:shared', 'Shared network',
            'fresh', '{}', '2026-08-15T00:00:00Z', '2026-08-15T00:00:00Z')",
    )
    .bind(resource_id)
    .execute(&pool)
    .await
    .expect("resource");
    sqlx::query(
        "INSERT INTO deployment_resource_links(deployment_resource_link_id, deployment_id,
            resource_entity_id, relation_kind, state, origin, source_refs_json, revision, created_at, updated_at)
         VALUES ('33333333-3333-4333-8333-333333333333', '11111111-1111-4111-8111-111111111111',
            ?, 'uses_network', 'confirmed', 'user_declared', '[\"fixture\"]', 1,
            '2026-08-15T00:00:00Z', '2026-08-15T00:00:00Z')",
    )
    .bind(resource_id)
    .execute(&pool)
    .await
    .expect("resource link");
    let topology = catalog_api::get_global_resources(
        State(state.clone()),
        Query(GlobalResourceQuery::default()),
    )
    .await
    .expect("topology")
    .0;
    assert_eq!(topology.data.lens, GlobalResourceLens::Topology);
    assert!(topology.data.summary.technical_project_count >= 2);
    let shared = catalog_api::get_global_resources(
        State(state.clone()),
        Query(GlobalResourceQuery {
            lens: Some("shared".to_owned()),
            ..GlobalResourceQuery::default()
        }),
    )
    .await
    .expect("shared")
    .0;
    assert_eq!(shared.data.lens, GlobalResourceLens::Shared);
    assert_eq!(shared.data.summary.shared_resource_count, 1);
    let impact = catalog_api::get_global_resources(
        State(state),
        Query(GlobalResourceQuery {
            lens: Some("impact".to_owned()),
            focus_kind: Some("resource".to_owned()),
            focus_id: Some(resource_id.to_owned()),
            ..GlobalResourceQuery::default()
        }),
    )
    .await
    .expect("impact")
    .0;
    let impact_refs = impact
        .data
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == network_atlas::contracts::GlobalResourceEdgeKind::ProjectImpactedByResource
        })
        .flat_map(|edge| edge.path_refs.iter())
        .collect::<Vec<_>>();
    assert!(target_ids.iter().all(|target_id| {
        let expected = format!("project_target:{target_id}");
        impact_refs.contains(&&expected)
    }));
}
