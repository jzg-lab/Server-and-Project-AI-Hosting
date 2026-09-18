use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, header::CONTENT_TYPE},
    routing::post,
};
use http_body_util::BodyExt;
use network_atlas::{
    api,
    contracts::{
        DiscoveryEvidence, EvidenceHostIdentity, EvidenceItem, EvidenceKind, Freshness,
        RedactionState,
    },
    model_provider::ModelClient,
    projection,
    secrets::FileSecretStore,
    ssh::SystemSsh,
    storage,
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle, time::sleep};
use tower::ServiceExt;

fn item(kind: EvidenceKind, external_id: &str, source: &str, metadata: Value) -> EvidenceItem {
    EvidenceItem {
        external_id: external_id.to_owned(),
        kind,
        source: source.to_owned(),
        observed_at: "2026-08-11T00:00:00Z".to_owned(),
        freshness: Freshness::Fresh,
        sha256: None,
        redaction_state: RedactionState::MetadataOnly,
        metadata,
    }
}

fn fixture_evidence(run_id: &str) -> DiscoveryEvidence {
    DiscoveryEvidence {
        protocol_version: "1".to_owned(),
        discovery_id: run_id.to_owned(),
        host: EvidenceHostIdentity {
            host_id: "host-m3".to_owned(),
            address: "fixture.local".to_owned(),
            os: "linux".to_owned(),
        },
        host_facts: vec![item(
            EvidenceKind::HostIdentity,
            "host-fact",
            "ssh:host",
            json!({"kernel": "Linux", "distribution": "fixture"}),
        )],
        docker_engines: vec![item(
            EvidenceKind::DockerEngine,
            "docker-engine",
            "ssh:docker_version",
            json!({"version": "26.1"}),
        )],
        compose_projects: vec![item(
            EvidenceKind::ComposeProject,
            "compose-stack",
            "ssh:compose_ls",
            json!({"name": "fixture-stack", "status": "running"}),
        )],
        systemd_units: Vec::new(),
        containers: vec![item(
            EvidenceKind::Container,
            "container-api",
            "ssh:containers",
            json!({
                "id": "container-api",
                "name": "fixture-api",
                "image": "fixture/api:1",
                "networks": "fixture-net",
                "mounts": "fixture-data",
                "ports": "0.0.0.0:8080->8080/tcp",
                "compose_project": "fixture-stack",
                "compose_service": "api",
                "state": "running"
            }),
        )],
        images: vec![item(
            EvidenceKind::Image,
            "image-api",
            "ssh:images",
            json!({"id": "image-api", "repository": "fixture/api", "tag": "1"}),
        )],
        networks: vec![item(
            EvidenceKind::Network,
            "network-fixture",
            "ssh:networks",
            json!({"id": "network-fixture", "name": "fixture-net"}),
        )],
        volumes: vec![item(
            EvidenceKind::Volume,
            "volume-fixture",
            "ssh:volumes",
            json!({"name": "fixture-data"}),
        )],
        document_candidates: vec![item(
            EvidenceKind::Document,
            "README.md",
            "ssh:document",
            json!({"relative_path": "README.md", "summary_excerpt": "Fixture service"}),
        )],
        health_checks: Vec::new(),
        warnings: Vec::new(),
        provider_results: Vec::new(),
        started_at: "2026-08-11T00:00:00Z".to_owned(),
        finished_at: "2026-08-11T00:00:01Z".to_owned(),
    }
}

async fn insert_run(pool: &SqlitePool, evidence: &DiscoveryEvidence, sequence: u32) {
    let submitted_at = format!("2026-08-11T00:00:{sequence:02}Z");
    let finished_at = format!("2026-08-11T00:00:{:02}Z", sequence + 1);
    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version, state, response_json,
            submitted_at, finished_at, evidence_json, evidence_sha256, evidence_item_count,
            evidence_retention
         ) VALUES (?, 'host-m3', ?, ?, '1', 'evidence_ready', '{}', ?, ?, ?, 'digest', ?, 'complete')",
    )
    .bind(&evidence.discovery_id)
    .bind(format!("request-{}", evidence.discovery_id))
    .bind(format!("key-{}", evidence.discovery_id))
    .bind(submitted_at)
    .bind(finished_at)
    .bind(serde_json::to_string(evidence).expect("evidence JSON"))
    .bind(i64::try_from(evidence.items().count()).unwrap())
    .execute(pool)
    .await
    .expect("discovery run");
}

struct TestContext {
    _directory: TempDir,
    pool: SqlitePool,
    app: Router,
    draft_id: String,
}

async fn prepare_context(model_timeout: Duration) -> TestContext {
    let directory = TempDir::new().expect("test directory");
    let path = directory.path().join("m3.db");
    let pool = storage::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .expect("test database");
    sqlx::query(
        "INSERT INTO workspaces(workspace_id, owner_id, created_at)
         VALUES ('workspace-default', 'owner-local', '2026-08-11T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at
         ) VALUES (
            'host-m3', 'workspace-default', 'Fixture host', 'fixture.local', 22, 'fixture',
            'secret://ssh/fixture', 'verified', 'ssh', 'linux', 'evidence_ready',
            '2026-08-11T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .expect("host");
    let evidence = fixture_evidence("run-m3-1");
    insert_run(&pool, &evidence, 1).await;
    let draft_id = projection::create_draft_from_evidence(&pool, "run-m3-1", "host-m3", &evidence)
        .await
        .expect("first draft");
    let secrets = FileSecretStore::new(directory.path().join("secrets"));
    let ssh = SystemSsh::system_default(directory.path().join("ssh"));
    let state = api::AppState::with_services_and_model(
        pool.clone(),
        secrets,
        ssh,
        ModelClient::new(model_timeout),
    );
    let app = api::router(state, "../frontend");
    TestContext {
        _directory: directory,
        pool,
        app,
        draft_id,
    }
}

async fn json_request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = if let Some(body) = body {
        builder = builder.header(CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).expect("request JSON"))
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("router response");
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).expect("UTF-8 response");
    let json = serde_json::from_str(&text).expect("JSON response");
    (status, response_headers, json, text)
}

fn node_id(snapshot: &Value, kind: &str) -> String {
    snapshot["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["kind"] == kind)
        .and_then(|node| node["id"].as_str())
        .expect("node kind")
        .to_owned()
}

fn node<'a>(snapshot: &'a Value, id: &str) -> &'a Value {
    snapshot["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["id"] == id)
        .expect("node id")
}

#[derive(Clone)]
struct ModelFixture {
    content: String,
    delay: Duration,
    requests: Arc<Mutex<Vec<Value>>>,
}

struct MockModel {
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl Drop for MockModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn model_handler(
    State(state): State<ModelFixture>,
    Json(request): Json<Value>,
) -> Json<Value> {
    state.requests.lock().await.push(request);
    if !state.delay.is_zero() {
        sleep(state.delay).await;
    }
    Json(json!({"choices": [{"message": {"content": state.content}}]}))
}

async fn start_model(content: String, delay: Duration) -> MockModel {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("model listener");
    let address = listener.local_addr().expect("model address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let fixture = ModelFixture {
        content,
        delay,
        requests: requests.clone(),
    };
    let router = Router::new()
        .route("/v1/chat/completions", post(model_handler))
        .with_state(fixture);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("model server");
    });
    MockModel {
        base_url: format!("http://{address}/v1"),
        requests,
        task,
    }
}

async fn configure_model(app: &Router, base_url: &str, suffix: &str) {
    let (status, _headers, secret, text) = json_request(
        app,
        Method::POST,
        "/api/v1/secret-refs",
        Some(json!({"kind": "model_key", "api_key": "fixture-model-key"})),
        &[("idempotency-key", &format!("model-secret-{suffix}"))],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let credential_ref = secret["data"]["credential_ref"]
        .as_str()
        .expect("model secret ref");
    let (status, _headers, _config, text) = json_request(
        app,
        Method::PUT,
        "/api/v1/model-provider",
        Some(json!({
            "base_url": base_url,
            "model": "fixture-model",
            "credential_ref": credential_ref
        })),
        &[("idempotency-key", &format!("model-config-{suffix}"))],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn configured_model_provider_is_projected_as_redacted_business_coordinator_agent() {
    let context = prepare_context(Duration::from_secs(2)).await;

    let (status, _headers, before, text) = json_request(
        &context.app,
        Method::GET,
        "/api/v1/views/global/world",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        !before["data"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["id"] == "business-coordinator-agent")
    );

    let model = start_model("fixture response".to_owned(), Duration::ZERO).await;
    configure_model(&context.app, &model.base_url, "world-node").await;
    let (status, _headers, tested, text) = json_request(
        &context.app,
        Method::POST,
        "/api/v1/model-provider/test",
        None,
        &[("idempotency-key", "model-test-world-node")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(tested["data"]["state"], "reachable");

    let (status, _headers, world, text) = json_request(
        &context.app,
        Method::GET,
        "/api/v1/views/global/world",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let agent = node(&world["data"], "business-coordinator-agent");
    assert_eq!(agent["kind"], "workspace");
    assert_eq!(agent["label"], "业务统筹 Agent");
    assert_eq!(agent["subtitle"], "fixture-model");
    assert_eq!(agent["state"], "confirmed");
    assert_eq!(agent["health"]["label"], "可用");
    assert_eq!(agent["source_refs"], json!(["registry:model-provider"]));
    assert!(
        agent["facts"].as_array().unwrap().iter().any(|fact| {
            fact["label"] == "OpenAI 兼容 URL" && fact["value"] == model.base_url
        })
    );
    assert!(
        agent["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|fact| { fact["label"] == "模型" && fact["value"] == "fixture-model" })
    );
    assert!(
        agent["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|fact| { fact["label"] == "Key 状态" && fact["value"] == "已保存" })
    );
    assert!(
        !world["data"]["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| {
                edge["from"] == "business-coordinator-agent" && edge["to"] == "host-m3"
            })
    );
    assert!(
        !world["data"]["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["from"] == "business-coordinator-agent")
    );
    assert!(!text.contains("fixture-model-key"));
    assert!(!text.contains("credential_ref"));
    assert!(!text.contains("secret://"));
}

#[tokio::test]
async fn agent_proposals_support_adopt_modify_reject_answer_and_guarded_undo() {
    let context = prepare_context(Duration::from_secs(2)).await;
    sqlx::query("UPDATE discovery_runs SET state = 'discovery_partial' WHERE run_id = 'run-m3-1'")
        .execute(&context.pool)
        .await
        .expect("partial discovery state");
    let draft_uri = format!("/api/v1/projection-drafts/{}", context.draft_id);
    let (status, _headers, draft, text) =
        json_request(&context.app, Method::GET, &draft_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let container_id = node_id(&draft["data"], "container");
    let image_id = node_id(&draft["data"], "image");
    let project_id = node_id(&draft["data"], "project");
    let content = serde_json::to_string(&json!({
        "facts_used": ["evidence:container-api"],
        "proposals": [
            {
                "title": "Rename the API container",
                "reason": "The container evidence supplies a stable service name.",
                "confidence": "high",
                "evidence_refs": ["evidence:container-api"],
                "patch": [{"op": "rename", "node_id": container_id, "label": "Agent API"}]
            },
            {
                "title": "Clarify the image label",
                "reason": "The image repository and tag identify this artifact.",
                "confidence": "medium",
                "evidence_refs": ["evidence:image-api"],
                "patch": [{"op": "rename", "node_id": image_id, "label": "Fixture Image"}]
            },
            {
                "title": "Clarify the project label",
                "reason": "Compose evidence identifies the project boundary.",
                "confidence": "medium",
                "evidence_refs": ["evidence:compose-stack"],
                "patch": [{"op": "rename", "node_id": project_id, "label": "Fixture Project"}]
            }
        ],
        "questions": [{
            "question_id": "project-owner",
            "type": "choice",
            "prompt": "Which project boundary should own this API?",
            "options": ["fixture-stack", "manual"],
            "evidence_refs": ["evidence:container-api"],
            "blocking": false
        }],
        "projection_patch": [],
        "warnings": []
    }))
    .expect("model content");
    let model = start_model(content, Duration::ZERO).await;
    configure_model(&context.app, &model.base_url, "normal").await;
    let (status, _headers, tested, text) = json_request(
        &context.app,
        Method::POST,
        "/api/v1/model-provider/test",
        None,
        &[("idempotency-key", "model-test-normal")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(tested["data"]["state"], "reachable");

    let (status, _headers, session, text) = json_request(
        &context.app,
        Method::POST,
        "/api/v1/onboarding-sessions",
        Some(json!({"draft_id": context.draft_id})),
        &[("idempotency-key", "session-normal")],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(session["data"]["state"], "ready");
    assert_eq!(session["data"]["proposals"].as_array().unwrap().len(), 3);
    assert_eq!(session["data"]["questions"].as_array().unwrap().len(), 1);
    let session_id = session["data"]["session_id"].as_str().unwrap();
    let proposal_ids = session["data"]["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|proposal| proposal["proposal_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let question_id = session["data"]["questions"][0]["question_id"]
        .as_str()
        .unwrap();
    let messages_uri = format!("/api/v1/onboarding-sessions/{session_id}/messages");

    let (status, _headers, adopted, text) = json_request(
        &context.app,
        Method::POST,
        &messages_uri,
        Some(json!({
            "action": "adopt",
            "proposal_id": proposal_ids[0],
            "base_revision": 1
        })),
        &[("idempotency-key", "adopt-first")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(adopted["data"]["proposals"][0]["state"], "adopted");
    assert_eq!(adopted["data"]["proposals"][0]["applied_revision"], 2);
    let (status, _headers, after_adopt, text) =
        json_request(&context.app, Method::GET, &draft_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        node(&after_adopt["data"], &container_id)["label"],
        "Agent API"
    );

    let (status, _headers, undone, text) = json_request(
        &context.app,
        Method::POST,
        &messages_uri,
        Some(json!({
            "action": "undo",
            "proposal_id": proposal_ids[0],
            "base_revision": 2
        })),
        &[("idempotency-key", "undo-first")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(undone["data"]["proposals"][0]["state"], "undone");
    assert_eq!(undone["meta"]["revision"], 3);

    let (status, _headers, modified, text) = json_request(
        &context.app,
        Method::POST,
        &messages_uri,
        Some(json!({
            "action": "modify",
            "proposal_id": proposal_ids[1],
            "base_revision": 3,
            "operations": [{"op": "rename", "node_id": container_id, "label": "User API"}]
        })),
        &[("idempotency-key", "modify-second")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(modified["data"]["proposals"][1]["state"], "modified");
    assert_eq!(modified["meta"]["revision"], 4);

    let (status, _headers, rejected, text) = json_request(
        &context.app,
        Method::POST,
        &messages_uri,
        Some(json!({"action": "reject", "proposal_id": proposal_ids[2]})),
        &[("idempotency-key", "reject-third")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(rejected["data"]["proposals"][2]["state"], "rejected");

    let (status, _headers, answered, text) = json_request(
        &context.app,
        Method::POST,
        &messages_uri,
        Some(json!({
            "action": "answer",
            "question_id": question_id,
            "answer": {"type": "choice", "value": "fixture-stack"}
        })),
        &[("idempotency-key", "answer-question")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(answered["data"]["questions"][0]["state"], "answered");
    assert_eq!(
        answered["data"]["questions"][0]["answer"]["value"],
        "fixture-stack"
    );

    let (status, _headers, blocked_undo, text) = json_request(
        &context.app,
        Method::POST,
        &messages_uri,
        Some(json!({
            "action": "undo",
            "proposal_id": proposal_ids[1],
            "base_revision": 3
        })),
        &[("idempotency-key", "undo-stale")],
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{text}");
    assert_eq!(
        blocked_undo["error"]["code"],
        "UNDO_HAS_INTERVENING_CHANGES"
    );
    assert!(model.requests.lock().await.len() >= 2);
}

async fn assert_manual_path_survives(app: &Router, draft_id: &str, revision: i64, key: &str) {
    let uri = format!("/api/v1/projection-drafts/{draft_id}");
    let (status, _headers, draft, text) = json_request(
        app,
        Method::PATCH,
        &uri,
        Some(json!({
            "base_revision": revision,
            "operations": [{"op": "rename", "node_id": node_id_from_response(app, draft_id, "container").await, "label": "Manual API"}]
        })),
        &[("if-match", &format!("revision-{revision}")), ("idempotency-key", key)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(draft["data"]["revision"], revision + 1);
}

async fn node_id_from_response(app: &Router, draft_id: &str, kind: &str) -> String {
    let uri = format!("/api/v1/projection-drafts/{draft_id}");
    let (status, _headers, draft, text) = json_request(app, Method::GET, &uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    node_id(&draft["data"], kind)
}

#[tokio::test]
async fn model_unconfigured_invalid_and_timeout_states_do_not_break_m2() {
    let unconfigured = prepare_context(Duration::from_millis(50)).await;
    let (status, _headers, session, text) = json_request(
        &unconfigured.app,
        Method::POST,
        "/api/v1/onboarding-sessions",
        Some(json!({"draft_id": unconfigured.draft_id})),
        &[("idempotency-key", "session-unconfigured")],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(session["data"]["state"], "unavailable");
    assert_eq!(session["data"]["error_code"], "MODEL_NOT_CONFIGURED");
    assert_manual_path_survives(
        &unconfigured.app,
        &unconfigured.draft_id,
        1,
        "manual-unconfigured",
    )
    .await;

    let invalid = prepare_context(Duration::from_secs(1)).await;
    let invalid_content = serde_json::to_string(&json!({
        "facts_used": ["evidence:not-present"],
        "proposals": [],
        "questions": [],
        "projection_patch": [],
        "warnings": []
    }))
    .unwrap();
    let invalid_model = start_model(invalid_content, Duration::ZERO).await;
    configure_model(&invalid.app, &invalid_model.base_url, "invalid").await;
    let (status, _headers, session, text) = json_request(
        &invalid.app,
        Method::POST,
        "/api/v1/onboarding-sessions",
        Some(json!({"draft_id": invalid.draft_id})),
        &[("idempotency-key", "session-invalid")],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(session["data"]["state"], "degraded");
    assert_eq!(session["data"]["error_code"], "PROPOSAL_INVALID");
    assert_eq!(session["data"]["proposals"].as_array().unwrap().len(), 0);
    assert_manual_path_survives(&invalid.app, &invalid.draft_id, 1, "manual-invalid").await;

    let timed_out = prepare_context(Duration::from_millis(25)).await;
    let valid_empty = serde_json::to_string(&json!({
        "facts_used": [], "proposals": [], "questions": [], "projection_patch": [], "warnings": []
    }))
    .unwrap();
    let slow_model = start_model(valid_empty, Duration::from_millis(150)).await;
    configure_model(&timed_out.app, &slow_model.base_url, "timeout").await;
    let (status, _headers, session, text) = json_request(
        &timed_out.app,
        Method::POST,
        "/api/v1/onboarding-sessions",
        Some(json!({"draft_id": timed_out.draft_id})),
        &[("idempotency-key", "session-timeout")],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(session["data"]["state"], "unavailable");
    assert_eq!(session["data"]["error_code"], "MODEL_TIMEOUT");
    assert_manual_path_survives(&timed_out.app, &timed_out.draft_id, 1, "manual-timeout").await;
}

#[tokio::test]
async fn rescan_reports_all_change_kinds_and_preserves_confirmed_user_decisions() {
    let context = prepare_context(Duration::from_secs(1)).await;
    let draft_uri = format!("/api/v1/projection-drafts/{}", context.draft_id);
    let (status, _headers, first, text) =
        json_request(&context.app, Method::GET, &draft_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let container_id = node_id(&first["data"], "container");
    let image_id = node_id(&first["data"], "image");
    let network_id = node_id(&first["data"], "network");
    let volume_id = node_id(&first["data"], "volume");
    let manual_project = "project-user-owned";
    let (status, _headers, edited, text) = json_request(
        &context.app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 1,
            "operations": [
                {"op": "rename", "node_id": container_id, "label": "Confirmed API"},
                {"op": "move", "node_id": container_id, "position": {"x": 777.0, "y": 333.0}},
                {"op": "create_project", "project_id": manual_project, "label": "Manual Project", "subtitle": "User boundary"},
                {"op": "assign_project", "node_id": image_id, "project_id": manual_project},
                {"op": "add_relation", "from": manual_project, "to": container_id, "kind": "contains", "label": "User relation"}
            ]
        })),
        &[("if-match", "revision-1"), ("idempotency-key", "first-decisions")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(edited["data"]["revision"], 2);
    let (status, _headers, ignored, text) = json_request(
        &context.app,
        Method::POST,
        "/api/v1/ignore-rules",
        Some(json!({
            "draft_id": context.draft_id,
            "node_id": network_id,
            "action": "ignore",
            "base_revision": 2
        })),
        &[
            ("if-match", "revision-2"),
            ("idempotency-key", "ignore-network"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(ignored["data"]["revision"], 3);
    let (status, _headers, _confirmed, text) = json_request(
        &context.app,
        Method::POST,
        &format!("{draft_uri}/confirm"),
        Some(json!({"base_revision": 3})),
        &[
            ("if-match", "revision-3"),
            ("idempotency-key", "confirm-first"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    let mut second = fixture_evidence("run-m3-2");
    second.finished_at = "2026-08-11T00:00:11Z".to_owned();
    second.docker_engines[0].metadata = json!({"version": "27.0"});
    second.volumes.clear();
    second.images.push(item(
        EvidenceKind::Image,
        "image-worker",
        "ssh:images",
        json!({"id": "image-worker", "repository": "fixture/worker", "tag": "1"}),
    ));
    sqlx::query("UPDATE discovery_runs SET state = 'discovery_complete' WHERE run_id = 'run-m3-1'")
        .execute(&context.pool)
        .await
        .expect("complete discovery baseline");
    insert_run(&context.pool, &second, 10).await;
    let second_draft =
        projection::create_draft_from_evidence(&context.pool, "run-m3-2", "host-m3", &second)
            .await
            .expect("second draft");

    let (status, _headers, run, text) = json_request(
        &context.app,
        Method::GET,
        "/api/v1/discovery-runs/run-m3-2",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(run["data"]["draft_id"], second_draft);
    assert!(
        run["data"]["diff_id"]
            .as_str()
            .unwrap()
            .starts_with("diff-")
    );
    let (status, _headers, diff, text) = json_request(
        &context.app,
        Method::GET,
        "/api/v1/discovery-runs/run-m3-2/diff",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    for kind in ["added", "changed", "missing", "conflict", "unchanged"] {
        assert!(
            diff["data"]["counts"][kind].as_u64().unwrap() > 0,
            "missing {kind}: {text}"
        );
    }

    let second_uri = format!("/api/v1/projection-drafts/{second_draft}");
    let (status, _headers, draft, text) =
        json_request(&context.app, Method::GET, &second_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(draft["data"]["base_revision"], 3);
    assert_eq!(draft["data"]["revision"], 4);
    assert_eq!(
        node(&draft["data"], &container_id)["label"],
        "Confirmed API"
    );
    assert_eq!(
        node(&draft["data"], &container_id)["position"],
        json!({"x": 777.0, "y": 333.0})
    );
    assert_eq!(
        node(&draft["data"], &image_id)["project_id"],
        manual_project
    );
    assert_eq!(node(&draft["data"], &network_id)["state"], "archived");
    assert_eq!(node(&draft["data"], &volume_id)["state"], "stale");
    assert_eq!(
        node(&draft["data"], manual_project)["label"],
        "Manual Project"
    );
    assert!(
        draft["data"]["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| {
                edge["from"] == manual_project
                    && edge["to"] == container_id
                    && edge["label"] == "User relation"
            })
    );

    let (status, _headers, world, text) = json_request(
        &context.app,
        Method::GET,
        "/api/v1/views/global/world",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        node(&world["data"], &container_id)["label"],
        "Confirmed API"
    );
    assert!(
        !world["data"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| {
                node["source_refs"].as_array().is_some_and(|refs| {
                    refs.iter()
                        .any(|reference| reference == "evidence:image-worker")
                })
            })
    );

    let ignore_state: String = sqlx::query("SELECT state FROM ignore_rules WHERE fingerprint = ?")
        .bind(&network_id)
        .fetch_one(&context.pool)
        .await
        .expect("ignore rule")
        .try_get("state")
        .expect("ignore state");
    assert_eq!(ignore_state, "ignored");
}
