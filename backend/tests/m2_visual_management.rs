use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header::CONTENT_TYPE},
};
use http_body_util::BodyExt;
use network_atlas::{
    api,
    contracts::{
        DiscoveryEvidence, EvidenceHostIdentity, EvidenceItem, EvidenceKind, Freshness,
        RedactionState,
    },
    projection, storage,
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;
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

fn fixture_evidence() -> DiscoveryEvidence {
    DiscoveryEvidence {
        protocol_version: "1".to_owned(),
        discovery_id: "run-m2".to_owned(),
        host: EvidenceHostIdentity {
            host_id: "host-m2".to_owned(),
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

async fn prepare_pool(directory: &TempDir, evidence: &DiscoveryEvidence) -> SqlitePool {
    let path = directory.path().join("m2.db");
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
            'host-m2', 'workspace-default', 'Fixture host', 'fixture.local', 22, 'fixture',
            'secret://ssh/fixture', 'verified', 'ssh', 'linux', 'evidence_ready', '2026-08-11T00:00:00Z'
         )",
    )
    .execute(&pool)
    .await
    .expect("host");
    sqlx::query(
        "INSERT INTO discovery_runs(
            run_id, host_id, request_id, idempotency_key, protocol_version, state, response_json,
            submitted_at, finished_at, evidence_json, evidence_sha256, evidence_item_count, evidence_retention
         ) VALUES (
            'run-m2', 'host-m2', 'request-m2', 'discovery-m2', '1', 'evidence_ready', '{}',
            '2026-08-11T00:00:00Z', '2026-08-11T00:00:01Z', ?, 'fixture-digest', 8, 'complete'
         )",
    )
    .bind(serde_json::to_string(evidence).expect("evidence JSON"))
    .execute(&pool)
    .await
    .expect("discovery run");
    pool
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

#[tokio::test]
async fn deterministic_visual_management_keeps_facts_immutable_and_persists_confirmed_projection() {
    let directory = TempDir::new().expect("test directory");
    let evidence = fixture_evidence();
    let evidence_before = serde_json::to_string(&evidence).expect("evidence baseline");
    let pool = prepare_pool(&directory, &evidence).await;
    let draft_id = projection::create_draft_from_evidence(&pool, "run-m2", "host-m2", &evidence)
        .await
        .expect("deterministic draft");
    assert_eq!(
        projection::create_draft_from_evidence(&pool, "run-m2", "host-m2", &evidence)
            .await
            .expect("idempotent draft"),
        draft_id
    );

    let app = api::router(api::AppState::new(pool.clone()), "../frontend");
    let (status, _headers, run, text) = json_request(
        &app,
        Method::GET,
        "/api/v1/discovery-runs/run-m2",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(run["data"]["draft_id"], draft_id);

    let (status, headers, world, text) =
        json_request(&app, Method::GET, "/api/v1/views/global/world", None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(world["meta"]["data_source"]["kind"], "real");
    assert_eq!(
        headers.get("etag").and_then(|value| value.to_str().ok()),
        Some("\"revision-1\"")
    );
    for kind in [
        "project",
        "compose_project",
        "service",
        "container",
        "image",
        "network",
        "volume",
        "port",
        "document",
    ] {
        assert!(
            world["data"]["nodes"]
                .as_array()
                .expect("world nodes")
                .iter()
                .any(|node| node["kind"] == kind),
            "missing {kind} node"
        );
    }
    assert!(
        world["data"]["nodes"]
            .as_array()
            .expect("world nodes")
            .iter()
            .all(|node| node["kind"] != "host")
    );
    assert!(
        world["data"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["source_refs"]
                .as_array()
                .is_some_and(|refs| !refs.is_empty())
                && node["observed_at"].is_string())
    );
    let nodes = world["data"]["nodes"].as_array().expect("world nodes");
    let node_ids = nodes
        .iter()
        .filter_map(|node| node["id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(node_ids.len(), nodes.len(), "world node IDs must be unique");
    for edge in world["data"]["edges"].as_array().expect("world edges") {
        let from = edge["from"].as_str().expect("edge from");
        let to = edge["to"].as_str().expect("edge to");
        assert!(node_ids.contains(from), "dangling edge.from: {from}");
        assert!(node_ids.contains(to), "dangling edge.to: {to}");
        assert_ne!(from, "host-m2", "HOST edge leaked into business world");
        assert_ne!(to, "host-m2", "HOST edge leaked into business world");
    }
    for (index, left) in nodes.iter().enumerate() {
        for right in nodes.iter().skip(index + 1) {
            let left_x = left["position"]["x"].as_f64().expect("left x");
            let left_y = left["position"]["y"].as_f64().expect("left y");
            let right_x = right["position"]["x"].as_f64().expect("right x");
            let right_y = right["position"]["y"].as_f64().expect("right y");
            let separated = left_x + left["width"].as_f64().expect("left width") <= right_x
                || right_x + right["width"].as_f64().expect("right width") <= left_x
                || left_y + left["height"].as_f64().expect("left height") <= right_y
                || right_y + right["height"].as_f64().expect("right height") <= left_y;
            assert!(
                separated,
                "initial nodes {} and {} overlap",
                left["id"], right["id"]
            );
        }
    }
    let project_id = node_id(&world["data"], "project");
    assert!(
        world["data"]["edges"]
            .as_array()
            .expect("world edges")
            .iter()
            .any(|edge| edge["kind"] == "uses_image")
    );
    assert!(
        world["data"]["nodes"]
            .as_array()
            .expect("world nodes")
            .iter()
            .any(|node| node["kind"] == "image" && node["project_id"] == project_id)
    );

    let project_uri = format!("/api/v1/projects/{project_id}/views/resources");
    let (status, _headers, project_resources, text) =
        json_request(&app, Method::GET, &project_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        project_resources["data"]["nodes"]
            .as_array()
            .expect("project resource nodes")
            .iter()
            .any(|node| node["kind"] == "image")
    );

    let draft_uri = format!("/api/v1/projection-drafts/{draft_id}");
    let (status, _headers, draft, text) =
        json_request(&app, Method::GET, &draft_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let project_id = node_id(&draft["data"], "project");
    let container_id = node_id(&draft["data"], "container");
    assert_eq!(draft["data"]["revision"], 1);

    let rename = json!({
        "base_revision": 1,
        "operations": [{"op": "rename", "node_id": container_id, "label": "API Gateway"}]
    });
    let (status, _headers, renamed, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(rename.clone()),
        &[
            ("if-match", "revision-1"),
            ("idempotency-key", "rename-container"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(renamed["data"]["revision"], 2);
    let renamed_container = renamed["data"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == container_id)
        .expect("renamed container");
    assert_eq!(renamed_container["label"], "API Gateway");
    assert!(
        renamed_container["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|fact| fact["label"] == "原始名称" && fact["value"] == "fixture-api")
    );
    assert_eq!(
        renamed["data"]["snapshot"],
        Value::Null,
        "flattened snapshot must stay at the documented response level"
    );

    let (status, _headers, replay, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(rename),
        &[
            ("if-match", "revision-1"),
            ("idempotency-key", "rename-container"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(replay["data"]["revision"], 2);

    let (status, _headers, reused, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 2,
            "operations": [{"op": "rename", "node_id": container_id, "label": "API Gateway"}]
        })),
        &[
            ("if-match", "revision-2"),
            ("idempotency-key", "rename-container"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED");

    let (status, _headers, stale, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 1,
            "operations": [{"op": "move", "node_id": container_id, "position": {"x": 300.0, "y": 180.0}}]
        })),
        &[("if-match", "revision-1"), ("idempotency-key", "stale-write")],
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{text}");
    assert_eq!(stale["error"]["code"], "PRECONDITION_FAILED");

    let (status, _headers, created, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 2,
            "operations": [{
                "op": "create_project",
                "project_id": "user-split",
                "label": "User split",
                "subtitle": "Local project boundary"
            }]
        })),
        &[
            ("if-match", "revision-2"),
            ("idempotency-key", "create-project"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(created["data"]["revision"], 3);

    let (status, _headers, assigned, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 3,
            "operations": [{"op": "assign_project", "node_id": container_id, "project_id": "user-split"}]
        })),
        &[("if-match", "revision-3"), ("idempotency-key", "assign-project")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(assigned["data"]["revision"], 4);

    let (status, _headers, related, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 4,
            "operations": [{
                "op": "add_relation",
                "from": "user-split",
                "to": container_id,
                "kind": "contains",
                "label": "Local assignment"
            }]
        })),
        &[
            ("if-match", "revision-4"),
            ("idempotency-key", "add-relation"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let relation_id = related["data"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["from"] == "user-split" && edge["to"] == container_id)
        .and_then(|edge| edge["id"].as_str())
        .expect("user relation")
        .to_owned();

    let (status, _headers, removed, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 5,
            "operations": [{"op": "remove_relation", "edge_id": relation_id}]
        })),
        &[
            ("if-match", "revision-5"),
            ("idempotency-key", "remove-relation"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(removed["data"]["revision"], 6);

    let layout_uri = format!("/api/v1/layouts/layout-{draft_id}");
    let (status, _headers, layout, text) = json_request(
        &app,
        Method::PATCH,
        &layout_uri,
        Some(json!({
            "base_revision": 6,
            "positions": [{"node_id": container_id, "position": {"x": 321.0, "y": 222.0}}]
        })),
        &[
            ("if-match", "revision-6"),
            ("idempotency-key", "save-layout"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(layout["data"]["revision"], 7);

    let ignore_uri = "/api/v1/ignore-rules";
    let (status, _headers, archived, text) = json_request(
        &app,
        Method::POST,
        ignore_uri,
        Some(json!({
            "draft_id": draft_id,
            "node_id": container_id,
            "action": "archive",
            "base_revision": 7
        })),
        &[
            ("if-match", "revision-7"),
            ("idempotency-key", "archive-container"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(archived["data"]["revision"], 8);

    let (status, _headers, restored, text) = json_request(
        &app,
        Method::POST,
        ignore_uri,
        Some(json!({
            "draft_id": draft_id,
            "node_id": container_id,
            "action": "restore",
            "base_revision": 8
        })),
        &[
            ("if-match", "revision-8"),
            ("idempotency-key", "restore-container"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(restored["data"]["revision"], 9);

    let confirm_uri = format!("/api/v1/projection-drafts/{draft_id}/confirm");
    let (status, _headers, confirmed, text) = json_request(
        &app,
        Method::POST,
        &confirm_uri,
        Some(json!({"base_revision": 9})),
        &[
            ("if-match", "revision-9"),
            ("idempotency-key", "confirm-draft"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(confirmed["meta"]["data_source"]["kind"], "real");
    assert_eq!(confirmed["data"]["revision"], 9);

    let (status, _headers, immutable, text) = json_request(
        &app,
        Method::PATCH,
        &draft_uri,
        Some(json!({
            "base_revision": 9,
            "operations": [{"op": "rename", "node_id": project_id, "label": "Changed"}]
        })),
        &[
            ("if-match", "revision-9"),
            ("idempotency-key", "after-confirm"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(immutable["error"]["code"], "DRAFT_ALREADY_CONFIRMED");

    let (status, _headers, immutable_layout, text) = json_request(
        &app,
        Method::PATCH,
        &layout_uri,
        Some(json!({
            "base_revision": 9,
            "positions": [{"node_id": container_id, "position": {"x": 400.0, "y": 300.0}}]
        })),
        &[
            ("if-match", "revision-9"),
            ("idempotency-key", "layout-after-confirm"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(immutable_layout["error"]["code"], "DRAFT_ALREADY_CONFIRMED");

    let (status, _headers, immutable_ignore, text) = json_request(
        &app,
        Method::POST,
        ignore_uri,
        Some(json!({
            "draft_id": draft_id,
            "node_id": container_id,
            "action": "archive",
            "base_revision": 9
        })),
        &[
            ("if-match", "revision-9"),
            ("idempotency-key", "archive-after-confirm"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(immutable_ignore["error"]["code"], "DRAFT_ALREADY_CONFIRMED");

    let refreshed = api::router(api::AppState::new(pool.clone()), "../frontend");
    let (status, _headers, after_restart, text) =
        json_request(&refreshed, Method::GET, &draft_uri, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(after_restart["data"]["state"], "confirmed");
    let moved = after_restart["data"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == container_id)
        .expect("container after restart");
    assert_eq!(moved["position"], json!({"x": 321.0, "y": 222.0}));

    let (status, _headers, bootstrap, text) =
        json_request(&refreshed, Method::GET, "/api/v1/bootstrap", None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        bootstrap["data"]["projects"]
            .as_array()
            .expect("bootstrap projects")
            .iter()
            .all(|project| project["state"] == "confirmed" && project["health"] == "已确认")
    );

    let persisted_evidence: String =
        sqlx::query("SELECT evidence_json FROM discovery_runs WHERE run_id = 'run-m2'")
            .fetch_one(&pool)
            .await
            .expect("evidence row")
            .try_get("evidence_json")
            .expect("evidence payload");
    assert_eq!(persisted_evidence, evidence_before);
    let versions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM projection_versions WHERE draft_id = ?")
            .bind(&draft_id)
            .fetch_one(&pool)
            .await
            .expect("version count");
    assert_eq!(versions, 1);
}
