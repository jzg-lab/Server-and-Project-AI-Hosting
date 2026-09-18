use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue},
};
use network_atlas::{
    api::{AppState, catalog_api},
    contracts::{
        DiscoveryEvidence, DiscoveryProviderCoverage, DiscoveryProviderStatus,
        EvidenceHostIdentity, Freshness, ProjectAgentBindRequest, ProjectAgentCapability,
        ProjectAgentToolName, ProjectAgentToolRequest, ProjectAgentToolResult,
        ProjectTargetCreateRequest, TechnicalProjectCreateRequest,
    },
    data_management,
    project_agent::{self, ProjectAgentError},
    storage,
};
use serde_json::json;
use sqlx::SqlitePool;

const HOST_ID: &str = "host-h6";
const DEPLOYMENT_A: &str = "11111111-1111-4111-8111-111111111111";
const DEPLOYMENT_B: &str = "22222222-2222-4222-8222-222222222222";
const OBSERVATION_A: &str = "33333333-3333-4333-8333-333333333333";

async fn database() -> SqlitePool {
    let pool = storage::connect("sqlite::memory:").await.expect("database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-h6', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(host_id, workspace_id, display_name, address, port, ssh_user,
            credential_ref, host_key_state, transport, os, status, created_at)
         VALUES (?, 'workspace-default', 'H6 host', '127.0.0.1', 22, 'fixture',
            'secret://fixture', 'verified', 'ssh', 'linux', 'connection_ready',
            '2026-08-15T00:00:00Z')",
    )
    .bind(HOST_ID)
    .execute(&pool)
    .await
    .expect("host");
    for (deployment_id, external_id, name, identity) in [
        (
            DEPLOYMENT_A,
            "compose:orders",
            "Orders",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            DEPLOYMENT_B,
            "compose:billing",
            "Billing",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ] {
        sqlx::query(
            "INSERT INTO deployments(deployment_id, workspace_id, host_id, provider_kind,
                external_id, identity_key, display_name, catalog_state, freshness,
                created_at, updated_at)
             VALUES (?, 'workspace-default', ?, 'compose', ?, ?, ?, 'observed', 'fresh',
                '2026-08-15T00:00:00Z', '2026-08-15T00:00:00Z')",
        )
        .bind(deployment_id)
        .bind(HOST_ID)
        .bind(external_id)
        .bind(identity)
        .bind(name)
        .execute(&pool)
        .await
        .expect("deployment");
    }
    pool
}

fn idempotency(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", HeaderValue::from_static(value));
    headers
}

async fn create_project(state: &AppState, key: &'static str, display_name: &str) -> String {
    catalog_api::create_technical_project(
        State(state.clone()),
        idempotency(key),
        Json(TechnicalProjectCreateRequest {
            display_name: display_name.to_owned(),
            summary: None,
        }),
    )
    .await
    .expect("project")
    .1
    .0
    .data
    .technical_project_id
}

async fn create_target(
    state: &AppState,
    key: &'static str,
    project_id: &str,
    deployment_id: &str,
) -> String {
    catalog_api::create_project_target(
        State(state.clone()),
        idempotency(key),
        Json(ProjectTargetCreateRequest {
            technical_project_id: project_id.to_owned(),
            deployment_id: deployment_id.to_owned(),
            display_name: None,
        }),
    )
    .await
    .expect("target")
    .1
    .0
    .data
    .project_target_id
}

async fn bind_agent(state: &AppState, project_id: &str, key: &'static str) -> String {
    project_agent::bind_project_agent(
        State(state.clone()),
        idempotency(key),
        Path(project_id.to_owned()),
        Ok(Json(ProjectAgentBindRequest { display_name: None })),
    )
    .await
    .expect("agent")
    .1
    .0
    .data
    .project_agent_id
}

async fn seed_observation_and_diff(pool: &SqlitePool) {
    let evidence = DiscoveryEvidence {
        protocol_version: "fixture/1".to_owned(),
        discovery_id: "discovery-h6".to_owned(),
        host: EvidenceHostIdentity {
            host_id: HOST_ID.to_owned(),
            address: "127.0.0.1".to_owned(),
            os: "linux".to_owned(),
        },
        host_facts: Vec::new(),
        docker_engines: Vec::new(),
        compose_projects: Vec::new(),
        systemd_units: Vec::new(),
        containers: Vec::new(),
        images: Vec::new(),
        networks: Vec::new(),
        volumes: Vec::new(),
        document_candidates: Vec::new(),
        health_checks: Vec::new(),
        warnings: Vec::new(),
        provider_results: vec![DiscoveryProviderCoverage {
            provider_kind: "compose".to_owned(),
            status: DiscoveryProviderStatus::Ready,
            observed_count: 1,
            evidence_refs: vec!["evidence:compose:orders".to_owned()],
            warnings: Vec::new(),
            observed_at: Some("2026-08-15T00:01:00Z".to_owned()),
        }],
        started_at: "2026-08-15T00:00:00Z".to_owned(),
        finished_at: "2026-08-15T00:01:00Z".to_owned(),
    };
    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version, state,
            response_json, submitted_at, started_at, finished_at, evidence_json,
            evidence_item_count, evidence_retention, diff_id
         ) VALUES ('run-h6', ?, 'request-h6', 'discovery-h6', 'fixture/1',
            'discovery_complete', '{}', '2026-08-15T00:00:00Z',
            '2026-08-15T00:00:00Z', '2026-08-15T00:01:00Z', ?, 1, 'complete', 'diff-h6')",
    )
    .bind(HOST_ID)
    .bind(serde_json::to_string(&evidence).expect("evidence json"))
    .execute(pool)
    .await
    .expect("discovery run");
    sqlx::query(
        "INSERT INTO deployment_observations(
            deployment_observation_id, deployment_id, discovery_run_id, provider_kind,
            external_id, observation_state, provider_status, observed_at,
            observed_at_epoch_ms, evidence_refs_json, metadata_json, created_at
         ) VALUES (?, ?, 'run-h6', 'compose', 'compose:orders', 'observed', 'ready',
            '2026-08-15T00:01:00Z', 1786752060000,
            '[\"evidence:compose:orders\"]', '{\"service_state\":\"running\"}',
            '2026-08-15T00:01:00Z')",
    )
    .bind(OBSERVATION_A)
    .bind(DEPLOYMENT_A)
    .execute(pool)
    .await
    .expect("observation");
    sqlx::query(
        "UPDATE deployments SET latest_observation_id = ?,
            last_observed_at = '2026-08-15T00:01:00Z',
            last_observed_at_epoch_ms = 1786752060000
         WHERE deployment_id = ?",
    )
    .bind(OBSERVATION_A)
    .bind(DEPLOYMENT_A)
    .execute(pool)
    .await
    .expect("deployment pointer");
    sqlx::query(
        "INSERT INTO discovery_diffs(
            diff_id, run_id, host_id, counts_json, items_json, created_at
         ) VALUES ('diff-h6', 'run-h6', ?,
            '{\"added\":2,\"changed\":0,\"missing\":0,\"conflict\":0,\"unchanged\":0}',
            '[{\"entity_key\":\"compose:orders\",\"evidence_kind\":\"compose_project\",\"external_id\":\"compose:orders\",\"change\":\"added\",\"summary\":\"new\",\"evidence_refs\":[\"evidence:compose:orders\"]},{\"entity_key\":\"compose:billing\",\"evidence_kind\":\"compose_project\",\"external_id\":\"compose:billing\",\"change\":\"added\",\"summary\":\"other project\",\"evidence_refs\":[\"evidence:compose:billing\"]}]',
            '2026-08-15T00:01:00Z')",
    )
    .bind(HOST_ID)
    .execute(pool)
    .await
    .expect("diff");
}

async fn invoke(
    state: &AppState,
    agent_id: &str,
    tool: &str,
    project_target_id: Option<String>,
) -> Result<network_atlas::contracts::ProjectAgentToolResponse, ProjectAgentError> {
    project_agent::invoke_tool(
        State(state.clone()),
        HeaderMap::new(),
        Path((agent_id.to_owned(), tool.to_owned())),
        Ok(Json(ProjectAgentToolRequest {
            project_target_id,
            limit: None,
        })),
    )
    .await
    .map(|response| response.0)
}

#[tokio::test]
async fn binding_is_stable_idempotent_and_only_exposes_fixed_read_only_tools() {
    let pool = database().await;
    let state = AppState::new(pool.clone());
    let project_id = create_project(&state, "h6-project-bind", "Orders").await;
    let first = project_agent::bind_project_agent(
        State(state.clone()),
        idempotency("h6-agent-bind"),
        Path(project_id.clone()),
        Ok(Json(ProjectAgentBindRequest { display_name: None })),
    )
    .await
    .expect("bind")
    .1
    .0;
    let replay = project_agent::bind_project_agent(
        State(state.clone()),
        idempotency("h6-agent-bind"),
        Path(project_id.clone()),
        Ok(Json(ProjectAgentBindRequest { display_name: None })),
    )
    .await
    .expect("replay")
    .1
    .0;
    assert_eq!(first.data.project_agent_id, replay.data.project_agent_id);
    assert_eq!(first.data.capabilities, [ProjectAgentCapability::ReadOnly]);
    assert_eq!(
        first.data.tool_names,
        [
            ProjectAgentToolName::ListProjectTargets,
            ProjectAgentToolName::ReadDeploymentObservation,
            ProjectAgentToolName::ReadHostCapabilities,
            ProjectAgentToolName::ReadServiceStatus,
            ProjectAgentToolName::ReadRecentDiff,
        ]
    );
    let loaded = project_agent::get_project_agent(State(state), HeaderMap::new(), Path(project_id))
        .await
        .expect("get")
        .0;
    assert_eq!(loaded.data, first.data);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_agents")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 1);
    let exported = network_atlas::data_management::export_technical_project(
        State(AppState::new(pool)),
        Path(first.data.technical_project_id.clone()),
    )
    .await
    .expect("technical project export")
    .0;
    assert_eq!(
        exported.data.payload["project_agent"]["project_agent_id"],
        json!(first.data.project_agent_id)
    );
}

#[tokio::test]
async fn exports_scope_catalog_rows_and_deduplicate_host_resources() {
    let pool = database().await;
    sqlx::query(
        "INSERT INTO resource_entities(
            resource_entity_id, workspace_id, resource_kind, source, external_id,
            display_name, freshness, metadata_json, created_at, updated_at
         ) VALUES ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'workspace-default',
            'network', 'fixture', 'network:shared', 'Shared network', 'fresh',
            '{}', '2026-08-15T00:00:00Z', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("resource");
    for (link_id, deployment_id) in [
        ("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", DEPLOYMENT_A),
        ("cccccccc-cccc-4ccc-8ccc-cccccccccccc", DEPLOYMENT_B),
    ] {
        sqlx::query(
            "INSERT INTO deployment_resource_links(
                deployment_resource_link_id, deployment_id, resource_entity_id,
                relation_kind, state, origin, source_refs_json, revision,
                created_at, updated_at
             ) VALUES (?, ?, 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'attached',
                'observed', 'observed', '[]', 1, '2026-08-15T00:00:00Z',
                '2026-08-15T00:00:00Z')",
        )
        .bind(link_id)
        .bind(deployment_id)
        .execute(&pool)
        .await
        .expect("deployment resource link");
    }

    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-other', 'owner-other', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("other workspace");
    sqlx::query(
        "INSERT INTO hosts(host_id, workspace_id, display_name, address, port, ssh_user,
            credential_ref, host_key_state, transport, os, status, created_at)
         VALUES ('host-other', 'workspace-other', 'Other host', '192.0.2.10', 22,
            'fixture', 'secret://other', 'verified', 'ssh', 'linux',
            'connection_ready', '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("other host");
    sqlx::query(
        "INSERT INTO deployments(
            deployment_id, workspace_id, host_id, provider_kind, external_id,
            identity_key, display_name, catalog_state, freshness, created_at, updated_at
         ) VALUES ('dddddddd-dddd-4ddd-8ddd-dddddddddddd', 'workspace-other', 'host-other',
            'compose', 'compose:other',
            'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
            'Other deployment', 'observed', 'fresh', '2026-08-15T00:00:00Z',
            '2026-08-15T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("other deployment");

    let workspace = data_management::export_workspace(State(AppState::new(pool.clone())))
        .await
        .expect("workspace export")
        .0;
    let deployments = workspace.data.payload["deployments"]
        .as_array()
        .expect("deployment export");
    assert_eq!(deployments.len(), 2);
    assert!(
        deployments
            .iter()
            .all(|item| item["workspace_id"] == "workspace-default")
    );

    let host = data_management::export_host(State(AppState::new(pool)), Path(HOST_ID.to_owned()))
        .await
        .expect("host export")
        .0;
    let resources = host.data.payload["resource_entities"]
        .as_array()
        .expect("host resource export");
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0]["resource_entity_id"],
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    );
}

#[tokio::test]
async fn typed_tools_enforce_project_target_scope_and_record_evidence() {
    let pool = database().await;
    seed_observation_and_diff(&pool).await;
    let state = AppState::new(pool.clone());
    let project_a = create_project(&state, "h6-project-a", "Orders").await;
    let project_b = create_project(&state, "h6-project-b", "Billing").await;
    let target_a = create_target(&state, "h6-target-a", &project_a, DEPLOYMENT_A).await;
    let target_b = create_target(&state, "h6-target-b", &project_b, DEPLOYMENT_B).await;
    let agent_id = bind_agent(&state, &project_a, "h6-agent-a").await;

    let targets = invoke(&state, &agent_id, "list_project_targets", None)
        .await
        .expect("list targets");
    let ProjectAgentToolResult::ProjectTargets(targets_data) = targets.data.result else {
        panic!("unexpected result")
    };
    assert_eq!(targets_data.targets.len(), 1);
    assert_eq!(targets_data.targets[0].project_target_id, target_a);

    let observation = invoke(
        &state,
        &agent_id,
        "read_deployment_observation",
        Some(target_a.clone()),
    )
    .await
    .expect("observation");
    assert_eq!(observation.meta.freshness, Freshness::Fresh);
    assert!(
        observation
            .data
            .evidence_refs
            .contains(&"evidence:compose:orders".to_owned())
    );

    let host = invoke(
        &state,
        &agent_id,
        "read_host_capabilities",
        Some(target_a.clone()),
    )
    .await
    .expect("host capabilities");
    let ProjectAgentToolResult::HostCapabilities(host_data) = host.data.result else {
        panic!("unexpected host result")
    };
    assert_eq!(host_data.host_id, HOST_ID);
    assert_eq!(
        host_data.provider_coverage[0].status,
        DiscoveryProviderStatus::Ready
    );

    let service = invoke(
        &state,
        &agent_id,
        "read_service_status",
        Some(target_a.clone()),
    )
    .await
    .expect("service");
    let ProjectAgentToolResult::ServiceStatus(service_data) = service.data.result else {
        panic!("unexpected service result")
    };
    assert_eq!(service_data.metadata["service_state"], json!("running"));

    let diff = invoke(
        &state,
        &agent_id,
        "read_recent_diff",
        Some(target_a.clone()),
    )
    .await
    .expect("diff");
    let ProjectAgentToolResult::RecentDiff(diff_data) = diff.data.result else {
        panic!("unexpected diff result")
    };
    let scoped_diff = diff_data.diff.expect("stored diff");
    assert_eq!(scoped_diff.diff_id, "diff-h6");
    assert_eq!(scoped_diff.items.len(), 1);
    assert_eq!(scoped_diff.items[0].external_id, "compose:orders");

    let missing_target = invoke(&state, &agent_id, "read_service_status", None).await;
    assert!(matches!(
        missing_target,
        Err(ProjectAgentError::BadRequest {
            code: "PROJECT_TARGET_REQUIRED",
            ..
        })
    ));
    let cross_project = invoke(&state, &agent_id, "read_service_status", Some(target_b)).await;
    assert!(matches!(
        cross_project,
        Err(ProjectAgentError::NotFound { .. })
    ));
    let arbitrary_tool = invoke(&state, &agent_id, "execute_shell", Some(target_a)).await;
    assert!(matches!(
        arbitrary_tool,
        Err(ProjectAgentError::BadRequest {
            code: "PROJECT_AGENT_TOOL_NOT_ALLOWED",
            ..
        })
    ));
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_agent_tool_calls")
        .fetch_one(&pool)
        .await
        .expect("tool calls");
    assert_eq!(calls, 5, "only successful typed reads are audited");
}

#[tokio::test]
async fn deleting_project_cascades_agent_and_tool_audit_without_touching_other_projects() {
    let pool = database().await;
    let state = AppState::new(pool.clone());
    let project_a = create_project(&state, "h6-delete-a", "A").await;
    let project_b = create_project(&state, "h6-delete-b", "B").await;
    let target_a = create_target(&state, "h6-delete-target", &project_a, DEPLOYMENT_A).await;
    let agent_a = bind_agent(&state, &project_a, "h6-delete-agent-a").await;
    let agent_b = bind_agent(&state, &project_b, "h6-delete-agent-b").await;
    invoke(&state, &agent_a, "read_service_status", Some(target_a))
        .await
        .expect("read before delete");
    sqlx::query("DELETE FROM technical_projects WHERE technical_project_id = ?")
        .bind(&project_a)
        .execute(&pool)
        .await
        .expect("delete project");
    let remaining_agents: Vec<String> =
        sqlx::query_scalar("SELECT project_agent_id FROM project_agents ORDER BY project_agent_id")
            .fetch_all(&pool)
            .await
            .expect("remaining agents");
    assert_eq!(remaining_agents, [agent_b]);
    let remaining_calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_agent_tool_calls")
        .fetch_one(&pool)
        .await
        .expect("remaining calls");
    assert_eq!(remaining_calls, 0);
}
