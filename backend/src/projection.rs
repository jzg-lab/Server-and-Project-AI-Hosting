//! M2 deterministic projection, local draft editing, confirmation, and read models.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, BootstrapData, BootstrapResponse, CanvasLayout,
        DataSourceDescriptor, DataSourceKind, DataSourceStatus, DiscoveryEvidence,
        FeatureAvailability, Freshness, GraphEdge, GraphFact, GraphFocus, GraphHealth, GraphNode,
        GraphNodeKind, GraphPosition, GraphRelationKind, GraphScopeKind, GraphSnapshot,
        GraphSnapshotResponse, HostStatus, HostSummary, IgnoreRuleAction, IgnoreRuleCreateRequest,
        LayoutPosition, LayoutResponse, LayoutUpdate, LayoutUpdateRequest, NavigationCounts,
        ProjectSummary, ProjectionConfirmRequest, ProjectionDraftData, ProjectionDraftResponse,
        ProjectionDraftUpdateRequest, ProjectionPatchOperation, ProjectionState,
        ProjectionVersionData, ProjectionVersionResponse,
    },
    discovery_diff::create_discovery_diff_in,
    model_provider::M3Error,
};

const MAX_IDEMPOTENCY_KEY: usize = 128;
const MAX_LABEL_CHARS: usize = 120;
const MAX_OPERATIONS: usize = 128;

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("invalid projection request")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
        status: StatusCode,
    },
    #[error("projection resource not found")]
    NotFound { resource: &'static str, id: String },
    #[error("projection request conflicts with current state")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
        status: StatusCode,
    },
    #[error("projection storage unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("projection payload is invalid")]
    Internal,
}

impl ProjectionError {
    fn bad(code: &'static str, message: &'static str, details: Value) -> Self {
        Self::BadRequest {
            code,
            message,
            details,
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn precondition(code: &'static str, message: &'static str, details: Value) -> Self {
        Self::Conflict {
            code,
            message,
            details,
            status: StatusCode::PRECONDITION_FAILED,
        }
    }

    fn conflict(code: &'static str, message: &'static str, details: Value) -> Self {
        Self::Conflict {
            code,
            message,
            details,
            status: StatusCode::CONFLICT,
        }
    }

    fn not_found(resource: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            resource,
            id: id.into(),
        }
    }
}

impl IntoResponse for ProjectionError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details) = match self {
            Self::BadRequest {
                code,
                message,
                details,
                status,
            }
            | Self::Conflict {
                code,
                message,
                details,
                status,
            } => (status, code, message, details),
            Self::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的投影对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "projection storage operation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地投影存储暂不可用",
                    json!({}),
                )
            }
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "投影载荷处理失败",
                json!({}),
            ),
        };
        (
            status,
            Json(ApiErrorResponse {
                error: ApiErrorBody {
                    code: code.to_owned(),
                    message: message.to_owned(),
                    details,
                    request_id,
                },
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Clone)]
struct DraftRow {
    draft_id: String,
    discovery_run_id: String,
    host_id: String,
    base_revision: i64,
    revision: i64,
    state: ProjectionState,
    pending_changes: u32,
    snapshot: GraphSnapshot,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct ConfirmedBaseline {
    snapshot: GraphSnapshot,
    deterministic: Option<GraphSnapshot>,
    revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProjectionUndoState {
    pub snapshot: GraphSnapshot,
    pub pending_changes: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct AppliedProjectionPatch {
    pub data: ProjectionDraftData,
    pub undo: ProjectionUndoState,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ProjectionError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            ProjectionError::bad(
                "IDEMPOTENCY_KEY_REQUIRED",
                "该投影修改需要 Idempotency-Key",
                json!({}),
            )
        })
}

fn if_match_revision(headers: &HeaderMap) -> Result<i64, ProjectionError> {
    let value = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ProjectionError::BadRequest {
            code: "IF_MATCH_REQUIRED",
            message: "该投影修改需要 If-Match 修订",
            details: json!({}),
            status: StatusCode::PRECONDITION_REQUIRED,
        })?;
    let normalized = value.trim().trim_matches('"');
    let revision = normalized
        .strip_prefix("revision-")
        .unwrap_or(normalized)
        .parse::<i64>()
        .ok()
        .filter(|revision| *revision >= 0)
        .ok_or_else(|| {
            ProjectionError::bad("INVALID_IF_MATCH", "If-Match 必须是 revision-N", json!({}))
        })?;
    Ok(revision)
}

fn invalid_json(_error: JsonRejection) -> ProjectionError {
    ProjectionError::bad("INVALID_JSON", "请求正文不是符合契约的 JSON", json!({}))
}

fn payload_sha256(value: impl AsRef<[u8]>) -> String {
    Sha256::digest(value.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn enum_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn projection_meta(request_id: String, revision: i64, freshness: Freshness) -> ApiMeta {
    let status = match freshness {
        Freshness::Fresh => DataSourceStatus::Fresh,
        Freshness::Stale => DataSourceStatus::Stale,
        Freshness::Unavailable => DataSourceStatus::Unavailable,
    };
    ApiMeta {
        request_id,
        revision,
        generated_at: now(),
        freshness,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status,
            label: "SSH · Linux · 确定性投影".to_owned(),
        },
    }
}

fn stable_id(prefix: &str, host_id: &str, external_id: &str) -> String {
    let digest = payload_sha256(format!("{prefix}\0{host_id}\0{external_id}"));
    format!("{prefix}-{}", &digest[..20])
}

fn valid_local_id(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_label(value: &str) -> bool {
    let length = value.trim().chars().count();
    (1..=MAX_LABEL_CHARS).contains(&length) && value.bytes().all(|byte| !byte.is_ascii_control())
}

fn metadata_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn list_values(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn facts_from_metadata(metadata: &Value) -> Vec<GraphFact> {
    let Some(object) = metadata.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort();
    keys.into_iter()
        .filter_map(|key| {
            let value = &object[key];
            let display = match value {
                Value::String(value) => value.clone(),
                Value::Number(value) => value.to_string(),
                Value::Bool(value) => value.to_string(),
                Value::Array(values) if values.len() <= 6 => values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => return None,
            };
            (!display.is_empty()).then(|| GraphFact {
                label: key.replace('_', " "),
                value: display.chars().take(160).collect(),
            })
        })
        .take(6)
        .collect()
}

fn node_size(kind: &GraphNodeKind) -> (f64, f64) {
    match kind {
        GraphNodeKind::Host | GraphNodeKind::Project => (184.0, 84.0),
        GraphNodeKind::ComposeProject | GraphNodeKind::Service => (164.0, 76.0),
        _ => (154.0, 72.0),
    }
}

fn node_position(index: usize, kind: &GraphNodeKind) -> GraphPosition {
    if *kind == GraphNodeKind::Host {
        return GraphPosition { x: 48.0, y: 246.0 };
    }
    let column = index % 3;
    let row = index / 3;
    GraphPosition {
        x: 260.0 + column as f64 * 220.0,
        y: 74.0 + row as f64 * 116.0,
    }
}

fn evidence_refs(run_id: &str, external_id: &str, source: &str) -> Vec<String> {
    vec![
        format!("discovery:{run_id}"),
        format!("evidence:{external_id}"),
        source.to_owned(),
    ]
}

struct NodeProvenance<'a> {
    run_id: &'a str,
    external_id: &'a str,
    source: &'a str,
    observed_at: &'a str,
    metadata: &'a Value,
    index: usize,
}

fn make_evidence_node(
    id: String,
    kind: GraphNodeKind,
    label: String,
    subtitle: String,
    project_id: Option<String>,
    provenance: NodeProvenance<'_>,
) -> GraphNode {
    let NodeProvenance {
        run_id,
        external_id,
        source,
        observed_at,
        metadata,
        index,
    } = provenance;
    let (width, height) = node_size(&kind);
    let mut facts = facts_from_metadata(metadata);
    if !facts.iter().any(|fact| fact.label == "原始名称") {
        facts.insert(
            0,
            GraphFact {
                label: "原始名称".to_owned(),
                value: label.clone(),
            },
        );
    }
    GraphNode {
        id,
        kind: kind.clone(),
        label,
        subtitle: Some(subtitle),
        state: ProjectionState::Draft,
        source_refs: evidence_refs(run_id, external_id, source),
        observed_at: Some(observed_at.to_owned()),
        position: node_position(index, &kind),
        width,
        height,
        summary: Some("由确定性规则从只读证据映射；用户确认前仅为本地草稿。".to_owned()),
        facts,
        project_id,
        health: None,
    }
}

fn add_node(nodes: &mut Vec<GraphNode>, known: &mut BTreeSet<String>, node: GraphNode) {
    if known.insert(node.id.clone()) {
        nodes.push(node);
    }
}

fn add_edge(
    edges: &mut Vec<GraphEdge>,
    known: &mut BTreeSet<String>,
    from: &str,
    to: &str,
    kind: GraphRelationKind,
    label: &str,
    source_refs: Vec<String>,
) {
    let kind_name = enum_string(&kind);
    let id = stable_id("edge", from, &format!("{to}\0{kind_name}"));
    if known.insert(id.clone()) {
        edges.push(GraphEdge {
            id,
            from: from.to_owned(),
            to: to.to_owned(),
            kind,
            label: label.to_owned(),
            state: ProjectionState::Draft,
            source_refs,
        });
    }
}

fn project_for_name(projects: &BTreeMap<String, String>, name: Option<&str>) -> Option<String> {
    name.and_then(|name| projects.get(name).cloned())
}

fn map_evidence(
    draft_id: &str,
    run_id: &str,
    host_id: &str,
    evidence: &DiscoveryEvidence,
    ignored: &BTreeSet<String>,
) -> GraphSnapshot {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_ids = BTreeSet::new();
    let mut edge_ids = BTreeSet::new();
    let mut usage: HashMap<String, BTreeSet<String>> = HashMap::new();

    let host_metadata = evidence
        .host_facts
        .first()
        .map(|item| item.metadata.clone())
        .unwrap_or_else(|| json!({"os": evidence.host.os}));
    add_node(
        &mut nodes,
        &mut node_ids,
        make_evidence_node(
            host_id.to_owned(),
            GraphNodeKind::Host,
            evidence.host.address.clone(),
            "Linux HOST".to_owned(),
            None,
            NodeProvenance {
                run_id,
                external_id: host_id,
                source: "ssh:host",
                observed_at: &evidence.started_at,
                metadata: &host_metadata,
                index: 0,
            },
        ),
    );

    for item in &evidence.systemd_units {
        let label =
            metadata_string(&item.metadata, "unit").unwrap_or_else(|| item.external_id.clone());
        let id = stable_id("systemd-unit", host_id, &item.external_id);
        let index = nodes.len();
        let mut node = make_evidence_node(
            id.clone(),
            GraphNodeKind::Service,
            label,
            "systemd service".to_owned(),
            None,
            NodeProvenance {
                run_id,
                external_id: &item.external_id,
                source: &item.source,
                observed_at: &item.observed_at,
                metadata: &item.metadata,
                index,
            },
        );
        let active = metadata_string(&item.metadata, "active").unwrap_or_default();
        let sub = metadata_string(&item.metadata, "sub").unwrap_or_default();
        node.health = Some(GraphHealth {
            label: if sub.is_empty() {
                active.clone()
            } else {
                format!("{active}/{sub}")
            },
            tone: if active == "active" {
                "green".to_owned()
            } else {
                "amber".to_owned()
            },
            activity: u32::from(active == "active"),
            alerts: u32::from(active != "active"),
            updated: item.observed_at.clone(),
        });
        add_node(&mut nodes, &mut node_ids, node);
        add_edge(
            &mut edges,
            &mut edge_ids,
            host_id,
            &id,
            GraphRelationKind::Contains,
            "发现 systemd 服务",
            evidence_refs(run_id, &item.external_id, &item.source),
        );
    }

    let mut project_names = BTreeSet::new();
    for item in &evidence.compose_projects {
        if let Some(name) = metadata_string(&item.metadata, "name") {
            project_names.insert(name);
        }
    }
    for item in &evidence.containers {
        if let Some(name) = metadata_string(&item.metadata, "compose_project")
            && !name.is_empty()
        {
            project_names.insert(name);
        }
    }
    let mut projects = BTreeMap::new();
    for name in project_names {
        let project_id = stable_id("project", host_id, &name);
        projects.insert(name.clone(), project_id.clone());
        let index = nodes.len();
        let metadata = json!({"compose_project": name, "mapping": "deterministic"});
        add_node(
            &mut nodes,
            &mut node_ids,
            make_evidence_node(
                project_id.clone(),
                GraphNodeKind::Project,
                name,
                "Compose 项目候选".to_owned(),
                Some(project_id.clone()),
                NodeProvenance {
                    run_id,
                    external_id: &project_id,
                    source: "deterministic:compose-project",
                    observed_at: &evidence.started_at,
                    metadata: &metadata,
                    index,
                },
            ),
        );
        add_edge(
            &mut edges,
            &mut edge_ids,
            host_id,
            &project_id,
            GraphRelationKind::Contains,
            "发现项目",
            vec![format!("discovery:{run_id}")],
        );
    }

    let mut image_by_name = BTreeMap::new();
    for item in &evidence.images {
        let label = metadata_string(&item.metadata, "repository")
            .filter(|value| value != "<none>")
            .unwrap_or_else(|| item.external_id.clone());
        let tag = metadata_string(&item.metadata, "tag").unwrap_or_default();
        let display = if tag.is_empty() || tag == "<none>" {
            label.clone()
        } else {
            format!("{label}:{tag}")
        };
        let id = stable_id("image", host_id, &item.external_id);
        image_by_name.insert(label, id.clone());
        image_by_name.insert(display.clone(), id.clone());
        if let Some(raw_id) = metadata_string(&item.metadata, "id") {
            image_by_name.insert(raw_id, id.clone());
        }
        let index = nodes.len();
        add_node(
            &mut nodes,
            &mut node_ids,
            make_evidence_node(
                id,
                GraphNodeKind::Image,
                display,
                "Docker image".to_owned(),
                None,
                NodeProvenance {
                    run_id,
                    external_id: &item.external_id,
                    source: &item.source,
                    observed_at: &item.observed_at,
                    metadata: &item.metadata,
                    index,
                },
            ),
        );
    }

    let mut network_by_name = BTreeMap::new();
    for item in &evidence.networks {
        let label =
            metadata_string(&item.metadata, "name").unwrap_or_else(|| item.external_id.clone());
        let id = stable_id("network", host_id, &item.external_id);
        network_by_name.insert(label.clone(), id.clone());
        let index = nodes.len();
        add_node(
            &mut nodes,
            &mut node_ids,
            make_evidence_node(
                id,
                GraphNodeKind::Network,
                label,
                "Docker network".to_owned(),
                None,
                NodeProvenance {
                    run_id,
                    external_id: &item.external_id,
                    source: &item.source,
                    observed_at: &item.observed_at,
                    metadata: &item.metadata,
                    index,
                },
            ),
        );
    }

    let mut volume_by_name = BTreeMap::new();
    for item in &evidence.volumes {
        let label =
            metadata_string(&item.metadata, "name").unwrap_or_else(|| item.external_id.clone());
        let id = stable_id("volume", host_id, &item.external_id);
        volume_by_name.insert(label.clone(), id.clone());
        let index = nodes.len();
        add_node(
            &mut nodes,
            &mut node_ids,
            make_evidence_node(
                id,
                GraphNodeKind::Volume,
                label,
                "Docker volume".to_owned(),
                None,
                NodeProvenance {
                    run_id,
                    external_id: &item.external_id,
                    source: &item.source,
                    observed_at: &item.observed_at,
                    metadata: &item.metadata,
                    index,
                },
            ),
        );
    }

    let mut compose_nodes = BTreeMap::new();
    for item in &evidence.compose_projects {
        let name =
            metadata_string(&item.metadata, "name").unwrap_or_else(|| item.external_id.clone());
        let Some(project_id) = projects.get(&name).cloned() else {
            continue;
        };
        let id = stable_id("compose", host_id, &item.external_id);
        compose_nodes.insert(name.clone(), id.clone());
        usage
            .entry(id.clone())
            .or_default()
            .insert(project_id.clone());
        let index = nodes.len();
        add_node(
            &mut nodes,
            &mut node_ids,
            make_evidence_node(
                id.clone(),
                GraphNodeKind::ComposeProject,
                name,
                "Docker Compose".to_owned(),
                Some(project_id.clone()),
                NodeProvenance {
                    run_id,
                    external_id: &item.external_id,
                    source: &item.source,
                    observed_at: &item.observed_at,
                    metadata: &item.metadata,
                    index,
                },
            ),
        );
        add_edge(
            &mut edges,
            &mut edge_ids,
            &project_id,
            &id,
            GraphRelationKind::Contains,
            "包含 Compose",
            evidence_refs(run_id, &item.external_id, &item.source),
        );
    }

    let health_by_container = evidence
        .health_checks
        .iter()
        .filter_map(|item| {
            Some((
                metadata_string(&item.metadata, "container_id")?,
                metadata_string(&item.metadata, "status")?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let mut service_nodes = BTreeMap::new();

    for item in &evidence.containers {
        let container_external_id =
            metadata_string(&item.metadata, "id").unwrap_or_else(|| item.external_id.clone());
        let container_name = metadata_string(&item.metadata, "name")
            .unwrap_or_else(|| container_external_id.clone());
        let project_name = metadata_string(&item.metadata, "compose_project");
        let project_id = project_for_name(&projects, project_name.as_deref());
        let service_name =
            metadata_string(&item.metadata, "compose_service").filter(|value| !value.is_empty());
        let container_id = stable_id("container", host_id, &item.external_id);
        if let Some(project_id) = &project_id {
            usage
                .entry(container_id.clone())
                .or_default()
                .insert(project_id.clone());
        }
        let index = nodes.len();
        let mut container_node = make_evidence_node(
            container_id.clone(),
            GraphNodeKind::Container,
            container_name,
            "Docker container".to_owned(),
            project_id.clone(),
            NodeProvenance {
                run_id,
                external_id: &item.external_id,
                source: &item.source,
                observed_at: &item.observed_at,
                metadata: &item.metadata,
                index,
            },
        );
        if let Some(status) = health_by_container.get(&container_external_id) {
            container_node.health = Some(GraphHealth {
                label: status.clone(),
                tone: if status == "healthy" {
                    "green"
                } else {
                    "amber"
                }
                .to_owned(),
                activity: 0,
                alerts: u32::from(status != "healthy"),
                updated: item.observed_at.clone(),
            });
        }
        add_node(&mut nodes, &mut node_ids, container_node);

        let parent = if let (Some(project_id), Some(service_name)) =
            (project_id.as_ref(), service_name.as_ref())
        {
            let key = format!("{project_id}\0{service_name}");
            let service_id = service_nodes
                .entry(key)
                .or_insert_with(|| {
                    stable_id("service", host_id, &format!("{project_id}:{service_name}"))
                })
                .clone();
            usage
                .entry(service_id.clone())
                .or_default()
                .insert(project_id.clone());
            if !node_ids.contains(&service_id) {
                let service_index = nodes.len();
                let metadata = json!({"compose_service": service_name});
                add_node(
                    &mut nodes,
                    &mut node_ids,
                    make_evidence_node(
                        service_id.clone(),
                        GraphNodeKind::Service,
                        service_name.clone(),
                        "Compose service".to_owned(),
                        Some(project_id.clone()),
                        NodeProvenance {
                            run_id,
                            external_id: service_name,
                            source: "deterministic:compose-service",
                            observed_at: &item.observed_at,
                            metadata: &metadata,
                            index: service_index,
                        },
                    ),
                );
                let compose_id = project_name
                    .as_ref()
                    .and_then(|name| compose_nodes.get(name))
                    .unwrap_or(project_id);
                add_edge(
                    &mut edges,
                    &mut edge_ids,
                    compose_id,
                    &service_id,
                    GraphRelationKind::Contains,
                    "包含服务",
                    evidence_refs(run_id, &item.external_id, &item.source),
                );
            }
            Some(service_id)
        } else {
            project_id.clone()
        };
        if let Some(parent) = parent {
            add_edge(
                &mut edges,
                &mut edge_ids,
                &parent,
                &container_id,
                GraphRelationKind::Deploys,
                "部署容器",
                evidence_refs(run_id, &item.external_id, &item.source),
            );
        } else {
            add_edge(
                &mut edges,
                &mut edge_ids,
                host_id,
                &container_id,
                GraphRelationKind::Contains,
                "待归类容器",
                evidence_refs(run_id, &item.external_id, &item.source),
            );
        }

        if let Some(image) = metadata_string(&item.metadata, "image")
            && let Some(image_id) = image_by_name.get(&image)
        {
            if let Some(project_id) = &project_id {
                usage
                    .entry(image_id.clone())
                    .or_default()
                    .insert(project_id.clone());
            }
            add_edge(
                &mut edges,
                &mut edge_ids,
                &container_id,
                image_id,
                GraphRelationKind::UsesImage,
                "使用镜像",
                evidence_refs(run_id, &item.external_id, &item.source),
            );
        }
        if let Some(networks) = metadata_string(&item.metadata, "networks") {
            for network in list_values(&networks) {
                if let Some(network_id) = network_by_name.get(network) {
                    if let Some(project_id) = &project_id {
                        usage
                            .entry(network_id.clone())
                            .or_default()
                            .insert(project_id.clone());
                    }
                    add_edge(
                        &mut edges,
                        &mut edge_ids,
                        &container_id,
                        network_id,
                        GraphRelationKind::ConnectsTo,
                        "连接网络",
                        evidence_refs(run_id, &item.external_id, &item.source),
                    );
                }
            }
        }
        if let Some(mounts) = metadata_string(&item.metadata, "mounts") {
            for mount in list_values(&mounts) {
                if let Some(volume_id) = volume_by_name.get(mount) {
                    if let Some(project_id) = &project_id {
                        usage
                            .entry(volume_id.clone())
                            .or_default()
                            .insert(project_id.clone());
                    }
                    add_edge(
                        &mut edges,
                        &mut edge_ids,
                        &container_id,
                        volume_id,
                        GraphRelationKind::Mounts,
                        "挂载卷",
                        evidence_refs(run_id, &item.external_id, &item.source),
                    );
                }
            }
        }
        if let Some(ports) = metadata_string(&item.metadata, "ports") {
            for port in list_values(&ports) {
                let port_id =
                    stable_id("port", host_id, &format!("{container_external_id}:{port}"));
                if let Some(project_id) = &project_id {
                    usage
                        .entry(port_id.clone())
                        .or_default()
                        .insert(project_id.clone());
                }
                if !node_ids.contains(&port_id) {
                    let port_index = nodes.len();
                    let metadata = json!({"binding": port, "container_id": container_external_id});
                    add_node(
                        &mut nodes,
                        &mut node_ids,
                        make_evidence_node(
                            port_id.clone(),
                            GraphNodeKind::Port,
                            port.to_owned(),
                            "Published port".to_owned(),
                            project_id.clone(),
                            NodeProvenance {
                                run_id,
                                external_id: port,
                                source: &item.source,
                                observed_at: &item.observed_at,
                                metadata: &metadata,
                                index: port_index,
                            },
                        ),
                    );
                }
                add_edge(
                    &mut edges,
                    &mut edge_ids,
                    &container_id,
                    &port_id,
                    GraphRelationKind::Exposes,
                    "暴露端口",
                    evidence_refs(run_id, &item.external_id, &item.source),
                );
            }
        }
    }

    let sole_project = (projects.len() == 1)
        .then(|| projects.values().next().cloned())
        .flatten();
    for item in &evidence.document_candidates {
        let label = metadata_string(&item.metadata, "relative_path")
            .unwrap_or_else(|| item.external_id.clone());
        let id = stable_id("document", host_id, &item.external_id);
        if let Some(project_id) = &sole_project {
            usage
                .entry(id.clone())
                .or_default()
                .insert(project_id.clone());
        }
        let index = nodes.len();
        add_node(
            &mut nodes,
            &mut node_ids,
            make_evidence_node(
                id.clone(),
                GraphNodeKind::Document,
                label,
                "项目文档".to_owned(),
                sole_project.clone(),
                NodeProvenance {
                    run_id,
                    external_id: &item.external_id,
                    source: &item.source,
                    observed_at: &item.observed_at,
                    metadata: &item.metadata,
                    index,
                },
            ),
        );
        add_edge(
            &mut edges,
            &mut edge_ids,
            sole_project.as_deref().unwrap_or(host_id),
            &id,
            GraphRelationKind::Documents,
            "项目文档",
            evidence_refs(run_id, &item.external_id, &item.source),
        );
    }

    for node in &mut nodes {
        if let Some(project_ids) = usage.get(&node.id) {
            node.project_id = (project_ids.len() == 1)
                .then(|| project_ids.iter().next().cloned())
                .flatten();
        }
        if ignored.contains(&node.id) {
            node.state = ProjectionState::Archived;
        }
    }
    let archived = nodes
        .iter()
        .filter(|node| node.state == ProjectionState::Archived)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    for edge in &mut edges {
        if archived.contains(&edge.from) || archived.contains(&edge.to) {
            edge.state = ProjectionState::Archived;
        }
    }

    GraphSnapshot {
        focus: GraphFocus {
            kind: GraphScopeKind::Global,
            id: "workspace-default".to_owned(),
        },
        nodes,
        edges,
        layout: CanvasLayout {
            layout_id: format!("layout-{draft_id}"),
            scope: format!("host:{host_id}"),
            revision: 1,
        },
    }
}

pub async fn create_draft_from_evidence(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    evidence: &DiscoveryEvidence,
) -> Result<String, ProjectionError> {
    let mut tx = pool.begin().await.map_err(ProjectionError::Storage)?;
    let draft_id = create_draft_from_evidence_in(&mut tx, run_id, host_id, evidence).await?;
    tx.commit().await.map_err(ProjectionError::Storage)?;
    Ok(draft_id)
}

pub(crate) async fn create_draft_from_evidence_in(
    connection: &mut SqliteConnection,
    run_id: &str,
    host_id: &str,
    evidence: &DiscoveryEvidence,
) -> Result<String, ProjectionError> {
    if let Some(row) =
        sqlx::query("SELECT draft_id FROM projection_drafts WHERE discovery_run_id = ?")
            .bind(run_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(ProjectionError::Storage)?
    {
        return row.try_get("draft_id").map_err(ProjectionError::Storage);
    }
    let ignored_rows = sqlx::query(
        "SELECT fingerprint FROM ignore_rules WHERE host_id = ? AND state IN ('ignored', 'archived')",
    )
    .bind(host_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?;
    let ignored = ignored_rows
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("fingerprint").ok())
        .collect::<BTreeSet<_>>();
    let draft_id = format!("draft-{}", &payload_sha256(run_id)[..24]);
    let deterministic = map_evidence(&draft_id, run_id, host_id, evidence, &ignored);
    let baseline = load_confirmed_baseline_in(connection, host_id, &ignored).await?;
    let base_revision = baseline.as_ref().map_or(0, |value| value.revision);
    let revision = base_revision + 1;
    create_discovery_diff_in(
        connection,
        run_id,
        host_id,
        evidence,
        &deterministic,
        baseline.as_ref().map(|value| &value.snapshot),
    )
    .await
    .map_err(|error| match error {
        M3Error::Storage(error) => ProjectionError::Storage(error),
        _ => ProjectionError::Internal,
    })?;
    let mut snapshot = baseline.as_ref().map_or_else(
        || deterministic.clone(),
        |baseline| overlay_confirmed_decisions(deterministic.clone(), baseline),
    );
    snapshot.layout.revision = revision;
    let snapshot_json = serde_json::to_string(&snapshot).map_err(|_| ProjectionError::Internal)?;
    let positions = snapshot
        .nodes
        .iter()
        .map(|node| LayoutPosition {
            node_id: node.id.clone(),
            position: node.position.clone(),
        })
        .collect::<Vec<_>>();
    let positions_json =
        serde_json::to_string(&positions).map_err(|_| ProjectionError::Internal)?;
    let timestamp = now();
    sqlx::query(
        "INSERT INTO projection_drafts(
            draft_id, workspace_id, discovery_run_id, host_id, base_revision, revision, state,
            pending_changes, snapshot_json, created_at, updated_at
         ) VALUES (?, 'workspace-default', ?, ?, ?, ?, 'draft', 0, ?, ?, ?)",
    )
    .bind(&draft_id)
    .bind(run_id)
    .bind(host_id)
    .bind(base_revision)
    .bind(revision)
    .bind(snapshot_json)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?;
    sqlx::query(
        "INSERT INTO canvas_layouts(layout_id, workspace_id, scope, revision, positions_json, updated_at)
         VALUES (?, 'workspace-default', ?, ?, ?, ?)",
    )
    .bind(&snapshot.layout.layout_id)
    .bind(&snapshot.layout.scope)
    .bind(revision)
    .bind(positions_json)
    .bind(&timestamp)
    .execute(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?;
    sqlx::query("UPDATE discovery_runs SET draft_id = ? WHERE run_id = ?")
        .bind(&draft_id)
        .bind(run_id)
        .execute(&mut *connection)
        .await
        .map_err(ProjectionError::Storage)?;
    Ok(draft_id)
}

async fn load_confirmed_baseline_in(
    connection: &mut SqliteConnection,
    host_id: &str,
    ignored: &BTreeSet<String>,
) -> Result<Option<ConfirmedBaseline>, ProjectionError> {
    let row = sqlx::query(
        "SELECT pv.snapshot_json, pv.revision, draft.draft_id,
                draft.discovery_run_id, run.evidence_json
         FROM projection_versions pv
         JOIN projection_drafts draft ON draft.draft_id = pv.draft_id
         LEFT JOIN discovery_runs run ON run.run_id = draft.discovery_run_id
         WHERE pv.host_id = ?
         ORDER BY pv.confirmed_at DESC, pv.version_id DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let snapshot_json: String = row
        .try_get("snapshot_json")
        .map_err(ProjectionError::Storage)?;
    let snapshot = serde_json::from_str(&snapshot_json).map_err(|_| ProjectionError::Internal)?;
    let discovery_run_id: String = row
        .try_get("discovery_run_id")
        .map_err(ProjectionError::Storage)?;
    let draft_id: String = row.try_get("draft_id").map_err(ProjectionError::Storage)?;
    let evidence_json: Option<String> = row
        .try_get("evidence_json")
        .map_err(ProjectionError::Storage)?;
    let deterministic = evidence_json
        .map(|payload| {
            let evidence: DiscoveryEvidence =
                serde_json::from_str(&payload).map_err(|_| ProjectionError::Internal)?;
            Ok(map_evidence(
                &draft_id,
                &discovery_run_id,
                host_id,
                &evidence,
                ignored,
            ))
        })
        .transpose()?;
    Ok(Some(ConfirmedBaseline {
        snapshot,
        deterministic,
        revision: row.try_get("revision").map_err(ProjectionError::Storage)?,
    }))
}

fn overlay_confirmed_decisions(
    mut current: GraphSnapshot,
    baseline: &ConfirmedBaseline,
) -> GraphSnapshot {
    if let Some(previous) = &baseline.deterministic {
        let confirmed_edges = baseline
            .snapshot
            .edges
            .iter()
            .map(|edge| edge.id.as_str())
            .collect::<BTreeSet<_>>();
        let removed_edges = previous
            .edges
            .iter()
            .filter(|edge| !confirmed_edges.contains(edge.id.as_str()))
            .map(|edge| edge.id.as_str())
            .collect::<BTreeSet<_>>();
        current
            .edges
            .retain(|edge| !removed_edges.contains(edge.id.as_str()));
    }

    let mut current_nodes = current
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    for confirmed in &baseline.snapshot.nodes {
        if let Some(index) = current_nodes.get(&confirmed.id).copied() {
            let node = &mut current.nodes[index];
            node.label = confirmed.label.clone();
            node.project_id = confirmed.project_id.clone();
            node.position = confirmed.position.clone();
            if confirmed.state == ProjectionState::Archived {
                node.state = ProjectionState::Archived;
            }
            continue;
        }
        let mut preserved = confirmed.clone();
        let user_declared = preserved
            .source_refs
            .iter()
            .any(|value| value.starts_with("user:"));
        if preserved.state != ProjectionState::Archived {
            preserved.state = if user_declared {
                ProjectionState::Draft
            } else {
                ProjectionState::Stale
            };
        }
        current_nodes.insert(preserved.id.clone(), current.nodes.len());
        current.nodes.push(preserved);
    }

    let node_states = current
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.state.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut edge_ids = current
        .edges
        .iter()
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>();
    for confirmed in &baseline.snapshot.edges {
        if let Some(edge) = current
            .edges
            .iter_mut()
            .find(|edge| edge.id == confirmed.id)
        {
            if confirmed.state == ProjectionState::Archived {
                edge.state = ProjectionState::Archived;
            }
            continue;
        }
        if !node_states.contains_key(&confirmed.from)
            || !node_states.contains_key(&confirmed.to)
            || !edge_ids.insert(confirmed.id.clone())
        {
            continue;
        }
        let mut preserved = confirmed.clone();
        let archived_endpoint = [&preserved.from, &preserved.to].iter().any(|id| {
            node_states
                .get(*id)
                .is_some_and(|state| *state == ProjectionState::Archived)
        });
        let user_declared = preserved
            .source_refs
            .iter()
            .any(|value| value.starts_with("user:"));
        preserved.state = if archived_endpoint || preserved.state == ProjectionState::Archived {
            ProjectionState::Archived
        } else if user_declared {
            ProjectionState::Draft
        } else {
            ProjectionState::Stale
        };
        current.edges.push(preserved);
    }
    current
}

async fn load_draft(pool: &SqlitePool, draft_id: &str) -> Result<DraftRow, ProjectionError> {
    let row = sqlx::query(
        "SELECT draft_id, discovery_run_id, host_id, base_revision, revision, state,
                pending_changes, snapshot_json, updated_at
         FROM projection_drafts WHERE draft_id = ?",
    )
    .bind(draft_id)
    .fetch_optional(pool)
    .await
    .map_err(ProjectionError::Storage)?
    .ok_or_else(|| ProjectionError::not_found("projection_draft", draft_id))?;
    let state = match row
        .try_get::<String, _>("state")
        .map_err(ProjectionError::Storage)?
        .as_str()
    {
        "confirmed" => ProjectionState::Confirmed,
        "archived" => ProjectionState::Archived,
        _ => ProjectionState::Draft,
    };
    let pending: i64 = row
        .try_get("pending_changes")
        .map_err(ProjectionError::Storage)?;
    let snapshot_json: String = row
        .try_get("snapshot_json")
        .map_err(ProjectionError::Storage)?;
    Ok(DraftRow {
        draft_id: row.try_get("draft_id").map_err(ProjectionError::Storage)?,
        discovery_run_id: row
            .try_get("discovery_run_id")
            .map_err(ProjectionError::Storage)?,
        host_id: row.try_get("host_id").map_err(ProjectionError::Storage)?,
        base_revision: row
            .try_get("base_revision")
            .map_err(ProjectionError::Storage)?,
        revision: row.try_get("revision").map_err(ProjectionError::Storage)?,
        state,
        pending_changes: u32::try_from(pending.max(0)).unwrap_or(u32::MAX),
        snapshot: serde_json::from_str(&snapshot_json).map_err(|_| ProjectionError::Internal)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(ProjectionError::Storage)?,
    })
}

async fn load_draft_in(
    connection: &mut SqliteConnection,
    draft_id: &str,
) -> Result<DraftRow, ProjectionError> {
    let row = sqlx::query(
        "SELECT draft_id, discovery_run_id, host_id, base_revision, revision, state,
                pending_changes, snapshot_json, updated_at
         FROM projection_drafts WHERE draft_id = ?",
    )
    .bind(draft_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?
    .ok_or_else(|| ProjectionError::not_found("projection_draft", draft_id))?;
    let state = match row
        .try_get::<String, _>("state")
        .map_err(ProjectionError::Storage)?
        .as_str()
    {
        "confirmed" => ProjectionState::Confirmed,
        "archived" => ProjectionState::Archived,
        _ => ProjectionState::Draft,
    };
    let pending: i64 = row
        .try_get("pending_changes")
        .map_err(ProjectionError::Storage)?;
    let snapshot_json: String = row
        .try_get("snapshot_json")
        .map_err(ProjectionError::Storage)?;
    Ok(DraftRow {
        draft_id: row.try_get("draft_id").map_err(ProjectionError::Storage)?,
        discovery_run_id: row
            .try_get("discovery_run_id")
            .map_err(ProjectionError::Storage)?,
        host_id: row.try_get("host_id").map_err(ProjectionError::Storage)?,
        base_revision: row
            .try_get("base_revision")
            .map_err(ProjectionError::Storage)?,
        revision: row.try_get("revision").map_err(ProjectionError::Storage)?,
        state,
        pending_changes: u32::try_from(pending.max(0)).unwrap_or(u32::MAX),
        snapshot: serde_json::from_str(&snapshot_json).map_err(|_| ProjectionError::Internal)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(ProjectionError::Storage)?,
    })
}

fn draft_data(row: &DraftRow) -> ProjectionDraftData {
    ProjectionDraftData {
        draft_id: row.draft_id.clone(),
        discovery_run_id: row.discovery_run_id.clone(),
        host_id: row.host_id.clone(),
        base_revision: row.base_revision,
        revision: row.revision,
        state: row.state.clone(),
        pending_changes: row.pending_changes,
        updated_at: row.updated_at.clone(),
        snapshot: row.snapshot.clone(),
    }
}

pub(crate) async fn load_projection_draft_data(
    pool: &SqlitePool,
    draft_id: &str,
) -> Result<ProjectionDraftData, ProjectionError> {
    load_draft(pool, draft_id).await.map(|row| draft_data(&row))
}

fn draft_response(row: &DraftRow, request_id: String) -> ProjectionDraftResponse {
    ProjectionDraftResponse {
        data: draft_data(row),
        meta: projection_meta(request_id, row.revision, Freshness::Fresh),
    }
}

async fn replay_mutation<T: DeserializeOwned>(
    pool: &SqlitePool,
    resource_kind: &str,
    resource_id: &str,
    key: &str,
    request_sha256: &str,
) -> Result<Option<T>, ProjectionError> {
    let row = sqlx::query(
        "SELECT request_sha256, response_json FROM projection_mutation_requests
         WHERE resource_kind = ? AND resource_id = ? AND idempotency_key = ?",
    )
    .bind(resource_kind)
    .bind(resource_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(ProjectionError::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let recorded: String = row
        .try_get("request_sha256")
        .map_err(ProjectionError::Storage)?;
    if recorded != request_sha256 {
        return Err(ProjectionError::conflict(
            "IDEMPOTENCY_KEY_REUSED",
            "Idempotency-Key 已用于不同的投影载荷",
            json!({"resource_id": resource_id}),
        ));
    }
    let payload: String = row
        .try_get("response_json")
        .map_err(ProjectionError::Storage)?;
    serde_json::from_str(&payload)
        .map(Some)
        .map_err(|_| ProjectionError::Internal)
}

async fn store_mutation(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    resource_kind: &str,
    resource_id: &str,
    key: &str,
    request_sha256: &str,
    response: &impl Serialize,
) -> Result<(), ProjectionError> {
    sqlx::query(
        "INSERT INTO projection_mutation_requests(
            request_id, resource_kind, resource_id, idempotency_key,
            request_sha256, response_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(resource_kind)
    .bind(resource_id)
    .bind(key)
    .bind(request_sha256)
    .bind(serde_json::to_string(response).map_err(|_| ProjectionError::Internal)?)
    .bind(now())
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(ProjectionError::Storage)
}

fn verify_revision(
    header_revision: i64,
    body_revision: i64,
    current_revision: i64,
) -> Result<(), ProjectionError> {
    if header_revision != body_revision || body_revision != current_revision {
        return Err(ProjectionError::precondition(
            "PRECONDITION_FAILED",
            "投影修订已变化，请重新读取后再修改",
            json!({
                "if_match": header_revision,
                "base_revision": body_revision,
                "current_revision": current_revision
            }),
        ));
    }
    Ok(())
}

fn apply_operation(
    snapshot: &mut GraphSnapshot,
    draft_id: &str,
    operation: &ProjectionPatchOperation,
) -> Result<(), ProjectionError> {
    match operation {
        ProjectionPatchOperation::Rename { node_id, label } => {
            if !valid_label(label) {
                return Err(ProjectionError::bad(
                    "INVALID_LABEL",
                    "节点名称必须为 1 到 120 个可见字符",
                    json!({"node_id": node_id}),
                ));
            }
            let node = snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == *node_id)
                .ok_or_else(|| ProjectionError::not_found("projection_node", node_id))?;
            node.label = label.trim().to_owned();
            node.state = ProjectionState::Draft;
        }
        ProjectionPatchOperation::Move { node_id, position } => {
            if !position.x.is_finite()
                || !position.y.is_finite()
                || position.x.abs() > 100_000.0
                || position.y.abs() > 100_000.0
            {
                return Err(ProjectionError::bad(
                    "INVALID_POSITION",
                    "节点位置超出画布允许范围",
                    json!({"node_id": node_id}),
                ));
            }
            let node = snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == *node_id)
                .ok_or_else(|| ProjectionError::not_found("projection_node", node_id))?;
            node.position = position.clone();
        }
        ProjectionPatchOperation::AssignProject {
            node_id,
            project_id,
        } => {
            if let Some(project_id) = project_id {
                let is_project = snapshot
                    .nodes
                    .iter()
                    .any(|node| node.id == *project_id && node.kind == GraphNodeKind::Project);
                if !is_project {
                    return Err(ProjectionError::not_found("project", project_id));
                }
            }
            let node = snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == *node_id)
                .ok_or_else(|| ProjectionError::not_found("projection_node", node_id))?;
            if matches!(node.kind, GraphNodeKind::Host | GraphNodeKind::Workspace) {
                return Err(ProjectionError::bad(
                    "NODE_NOT_ASSIGNABLE",
                    "HOST 或工作区节点不能归入项目",
                    json!({"node_id": node_id}),
                ));
            }
            node.project_id = project_id.clone();
            node.state = ProjectionState::Draft;
        }
        ProjectionPatchOperation::CreateProject {
            project_id,
            label,
            subtitle,
        } => {
            if !valid_local_id(project_id) || !valid_label(label) || subtitle.chars().count() > 240
            {
                return Err(ProjectionError::bad(
                    "INVALID_PROJECT",
                    "本地项目 ID、名称或说明不符合约束",
                    json!({"project_id": project_id}),
                ));
            }
            if snapshot.nodes.iter().any(|node| node.id == *project_id) {
                return Err(ProjectionError::conflict(
                    "PROJECT_ALREADY_EXISTS",
                    "该本地项目 ID 已存在",
                    json!({"project_id": project_id}),
                ));
            }
            let kind = GraphNodeKind::Project;
            let (width, height) = node_size(&kind);
            snapshot.nodes.push(GraphNode {
                id: project_id.clone(),
                kind,
                label: label.trim().to_owned(),
                subtitle: Some(subtitle.trim().to_owned()),
                state: ProjectionState::Draft,
                source_refs: vec![format!("user:draft:{draft_id}")],
                observed_at: None,
                position: node_position(snapshot.nodes.len(), &GraphNodeKind::Project),
                width,
                height,
                summary: Some("用户在本地草稿中创建的项目边界。".to_owned()),
                facts: vec![GraphFact {
                    label: "声明".to_owned(),
                    value: "user_declared".to_owned(),
                }],
                project_id: Some(project_id.clone()),
                health: None,
            });
        }
        ProjectionPatchOperation::AddRelation {
            from,
            to,
            kind,
            label,
        } => {
            if from == to || !valid_label(label) {
                return Err(ProjectionError::bad(
                    "INVALID_RELATION",
                    "关系端点或名称无效",
                    json!({"from": from, "to": to}),
                ));
            }
            for endpoint in [from, to] {
                if !snapshot
                    .nodes
                    .iter()
                    .any(|node| node.id == endpoint.as_str())
                {
                    return Err(ProjectionError::not_found(
                        "projection_node",
                        endpoint.as_str(),
                    ));
                }
            }
            let id = stable_id("edge", from, &format!("{to}\0{}", enum_string(kind)));
            if snapshot.edges.iter().any(|edge| edge.id == id) {
                return Err(ProjectionError::conflict(
                    "RELATION_ALREADY_EXISTS",
                    "相同端点和类型的关系已存在",
                    json!({"edge_id": id}),
                ));
            }
            snapshot.edges.push(GraphEdge {
                id,
                from: from.clone(),
                to: to.clone(),
                kind: kind.clone(),
                label: label.trim().to_owned(),
                state: ProjectionState::Draft,
                source_refs: vec![format!("user:draft:{draft_id}")],
            });
        }
        ProjectionPatchOperation::RemoveRelation { edge_id } => {
            let before = snapshot.edges.len();
            snapshot.edges.retain(|edge| edge.id != *edge_id);
            if snapshot.edges.len() == before {
                return Err(ProjectionError::not_found("projection_edge", edge_id));
            }
        }
        ProjectionPatchOperation::ArchiveNode { node_id } => {
            let node = snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == *node_id)
                .ok_or_else(|| ProjectionError::not_found("projection_node", node_id))?;
            node.state = ProjectionState::Archived;
            for edge in &mut snapshot.edges {
                if edge.from == *node_id || edge.to == *node_id {
                    edge.state = ProjectionState::Archived;
                }
            }
        }
        ProjectionPatchOperation::RestoreNode { node_id } => {
            let node = snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == *node_id)
                .ok_or_else(|| ProjectionError::not_found("projection_node", node_id))?;
            node.state = ProjectionState::Draft;
            let active = snapshot
                .nodes
                .iter()
                .filter(|node| node.state != ProjectionState::Archived)
                .map(|node| node.id.clone())
                .collect::<BTreeSet<_>>();
            for edge in &mut snapshot.edges {
                if active.contains(&edge.from) && active.contains(&edge.to) {
                    edge.state = ProjectionState::Draft;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_projection_operations(
    snapshot: &GraphSnapshot,
    draft_id: &str,
    operations: &[ProjectionPatchOperation],
) -> Result<GraphSnapshot, ProjectionError> {
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err(ProjectionError::bad(
            "INVALID_OPERATIONS",
            "投影修改必须包含 1 到 128 个操作",
            json!({}),
        ));
    }
    let mut candidate = snapshot.clone();
    for operation in operations {
        apply_operation(&mut candidate, draft_id, operation)?;
    }
    Ok(candidate)
}

async fn persist_agent_draft_in(
    connection: &mut SqliteConnection,
    row: &DraftRow,
    expected_revision: i64,
) -> Result<(), ProjectionError> {
    let snapshot_json =
        serde_json::to_string(&row.snapshot).map_err(|_| ProjectionError::Internal)?;
    let update = sqlx::query(
        "UPDATE projection_drafts SET revision = ?, state = 'draft', pending_changes = ?,
            snapshot_json = ?, updated_at = ? WHERE draft_id = ? AND revision = ?",
    )
    .bind(row.revision)
    .bind(i64::from(row.pending_changes))
    .bind(snapshot_json)
    .bind(&row.updated_at)
    .bind(&row.draft_id)
    .bind(expected_revision)
    .execute(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?;
    if update.rows_affected() != 1 {
        return Err(ProjectionError::precondition(
            "PRECONDITION_FAILED",
            "投影修订已被其他修改更新",
            json!({"draft_id": row.draft_id}),
        ));
    }
    let positions = row
        .snapshot
        .nodes
        .iter()
        .map(|node| LayoutPosition {
            node_id: node.id.clone(),
            position: node.position.clone(),
        })
        .collect::<Vec<_>>();
    let positions_json =
        serde_json::to_string(&positions).map_err(|_| ProjectionError::Internal)?;
    let layout_update = sqlx::query(
        "UPDATE canvas_layouts SET revision = ?, positions_json = ?, updated_at = ?
         WHERE layout_id = ?",
    )
    .bind(row.revision)
    .bind(positions_json)
    .bind(&row.updated_at)
    .bind(&row.snapshot.layout.layout_id)
    .execute(&mut *connection)
    .await
    .map_err(ProjectionError::Storage)?;
    if layout_update.rows_affected() != 1 {
        return Err(ProjectionError::not_found(
            "layout",
            &row.snapshot.layout.layout_id,
        ));
    }
    Ok(())
}

pub(crate) async fn apply_agent_operations_in(
    connection: &mut SqliteConnection,
    draft_id: &str,
    expected_revision: i64,
    operations: &[ProjectionPatchOperation],
) -> Result<AppliedProjectionPatch, ProjectionError> {
    let mut row = load_draft_in(connection, draft_id).await?;
    if row.state != ProjectionState::Draft {
        return Err(ProjectionError::conflict(
            "DRAFT_NOT_EDITABLE",
            "只有未确认草稿可以采用 Agent 建议",
            json!({"draft_id": draft_id}),
        ));
    }
    if row.revision != expected_revision {
        return Err(ProjectionError::precondition(
            "PRECONDITION_FAILED",
            "投影修订已变化，请重新读取建议和草稿",
            json!({"current_revision": row.revision, "base_revision": expected_revision}),
        ));
    }
    let undo = ProjectionUndoState {
        snapshot: row.snapshot.clone(),
        pending_changes: row.pending_changes,
    };
    row.snapshot = validate_projection_operations(&row.snapshot, draft_id, operations)?;
    row.revision += 1;
    row.pending_changes = row
        .pending_changes
        .saturating_add(u32::try_from(operations.len()).unwrap_or(u32::MAX));
    row.updated_at = now();
    row.snapshot.layout.revision = row.revision;
    persist_agent_draft_in(connection, &row, expected_revision).await?;
    Ok(AppliedProjectionPatch {
        data: draft_data(&row),
        undo,
    })
}

pub(crate) async fn undo_agent_operations_in(
    connection: &mut SqliteConnection,
    draft_id: &str,
    expected_revision: i64,
    applied_revision: i64,
    undo: &ProjectionUndoState,
) -> Result<ProjectionDraftData, ProjectionError> {
    let mut row = load_draft_in(connection, draft_id).await?;
    if row.state != ProjectionState::Draft {
        return Err(ProjectionError::conflict(
            "DRAFT_NOT_EDITABLE",
            "只有未确认草稿可以撤销 Agent 建议",
            json!({"draft_id": draft_id}),
        ));
    }
    if expected_revision != applied_revision || row.revision != applied_revision {
        return Err(ProjectionError::precondition(
            "UNDO_HAS_INTERVENING_CHANGES",
            "采用建议后已有其他修改，不能覆盖这些修改",
            json!({
                "current_revision": row.revision,
                "applied_revision": applied_revision,
                "base_revision": expected_revision
            }),
        ));
    }
    row.snapshot = undo.snapshot.clone();
    row.pending_changes = undo.pending_changes;
    row.revision += 1;
    row.updated_at = now();
    row.snapshot.layout.revision = row.revision;
    persist_agent_draft_in(connection, &row, applied_revision).await?;
    Ok(draft_data(&row))
}

#[utoipa::path(
    get,
    path = "/api/v1/projection-drafts/{draft_id}",
    tag = "m2",
    params(("draft_id" = String, Path, description = "Deterministic projection draft identifier")),
    responses((status = 200, body = ProjectionDraftResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_projection_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(draft_id): Path<String>,
) -> Result<Json<ProjectionDraftResponse>, ProjectionError> {
    let row = load_draft(&state.pool, &draft_id).await?;
    Ok(Json(draft_response(&row, request_id(&headers))))
}

#[utoipa::path(
    patch,
    path = "/api/v1/projection-drafts/{draft_id}",
    tag = "m2",
    params(
        ("draft_id" = String, Path, description = "Projection draft identifier"),
        ("If-Match" = String, Header, description = "Current revision-N ETag"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = ProjectionDraftUpdateRequest,
    responses((status = 200, body = ProjectionDraftResponse), (status = 412, body = ApiErrorResponse))
)]
pub async fn update_projection_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(draft_id): Path<String>,
    payload: Result<Json<ProjectionDraftUpdateRequest>, JsonRejection>,
) -> Result<Json<ProjectionDraftResponse>, ProjectionError> {
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let header_revision = if_match_revision(&headers)?;
    if request.operations.is_empty() || request.operations.len() > MAX_OPERATIONS {
        return Err(ProjectionError::bad(
            "INVALID_OPERATIONS",
            "投影修改必须包含 1 到 128 个操作",
            json!({}),
        ));
    }
    let request_hash =
        payload_sha256(serde_json::to_vec(&request).map_err(|_| ProjectionError::Internal)?);
    if let Some(response) = replay_mutation::<ProjectionDraftResponse>(
        &state.pool,
        "draft",
        &draft_id,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let mut row = load_draft(&state.pool, &draft_id).await?;
    if row.state == ProjectionState::Confirmed {
        return Err(ProjectionError::conflict(
            "DRAFT_ALREADY_CONFIRMED",
            "已确认版本保持不可变；需要从当前版本创建新草稿",
            json!({"draft_id": draft_id}),
        ));
    }
    verify_revision(header_revision, request.base_revision, row.revision)?;
    for operation in &request.operations {
        apply_operation(&mut row.snapshot, &draft_id, operation)?;
    }
    row.revision += 1;
    row.pending_changes = row
        .pending_changes
        .saturating_add(u32::try_from(request.operations.len()).unwrap_or(u32::MAX));
    row.updated_at = now();
    row.snapshot.layout.revision = row.revision;
    let response = draft_response(&row, request_id(&headers));
    let snapshot_json =
        serde_json::to_string(&row.snapshot).map_err(|_| ProjectionError::Internal)?;
    let mut tx = state.pool.begin().await.map_err(ProjectionError::Storage)?;
    let update = sqlx::query(
        "UPDATE projection_drafts SET revision = ?, state = 'draft', pending_changes = ?,
            snapshot_json = ?, updated_at = ? WHERE draft_id = ? AND revision = ?",
    )
    .bind(row.revision)
    .bind(i64::from(row.pending_changes))
    .bind(snapshot_json)
    .bind(&row.updated_at)
    .bind(&draft_id)
    .bind(request.base_revision)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    if update.rows_affected() != 1 {
        return Err(ProjectionError::precondition(
            "PRECONDITION_FAILED",
            "投影修订已被其他修改更新",
            json!({"draft_id": draft_id}),
        ));
    }
    store_mutation(&mut tx, "draft", &draft_id, &key, &request_hash, &response).await?;
    tx.commit().await.map_err(ProjectionError::Storage)?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/api/v1/projection-drafts/{draft_id}/confirm",
    tag = "m2",
    params(
        ("draft_id" = String, Path, description = "Projection draft identifier"),
        ("If-Match" = String, Header, description = "Current revision-N ETag"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = ProjectionConfirmRequest,
    responses((status = 201, body = ProjectionVersionResponse), (status = 412, body = ApiErrorResponse))
)]
pub async fn confirm_projection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(draft_id): Path<String>,
    payload: Result<Json<ProjectionConfirmRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ProjectionVersionResponse>), ProjectionError> {
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let header_revision = if_match_revision(&headers)?;
    let request_hash =
        payload_sha256(serde_json::to_vec(&request).map_err(|_| ProjectionError::Internal)?);
    if let Some(response) = replay_mutation::<ProjectionVersionResponse>(
        &state.pool,
        "confirm",
        &draft_id,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok((StatusCode::OK, Json(response)));
    }
    let mut row = load_draft(&state.pool, &draft_id).await?;
    verify_revision(header_revision, request.base_revision, row.revision)?;
    if row.state == ProjectionState::Confirmed {
        return Err(ProjectionError::conflict(
            "DRAFT_ALREADY_CONFIRMED",
            "该草稿已经发布为本地投影",
            json!({"draft_id": draft_id}),
        ));
    }
    for node in &mut row.snapshot.nodes {
        if node.state != ProjectionState::Archived {
            node.state = ProjectionState::Confirmed;
        }
    }
    for edge in &mut row.snapshot.edges {
        if edge.state != ProjectionState::Archived {
            edge.state = ProjectionState::Confirmed;
        }
    }
    let version_id = Uuid::new_v4().to_string();
    let confirmed_at = now();
    let response = ProjectionVersionResponse {
        data: ProjectionVersionData {
            version_id: version_id.clone(),
            draft_id: draft_id.clone(),
            host_id: row.host_id.clone(),
            revision: row.revision,
            confirmed_at: confirmed_at.clone(),
            snapshot: row.snapshot.clone(),
        },
        meta: projection_meta(request_id(&headers), row.revision, Freshness::Fresh),
    };
    let snapshot_json =
        serde_json::to_string(&row.snapshot).map_err(|_| ProjectionError::Internal)?;
    let mut tx = state.pool.begin().await.map_err(ProjectionError::Storage)?;
    sqlx::query(
        "INSERT INTO projection_versions(
            version_id, workspace_id, project_id, draft_id, host_id, revision, confirmed_by, confirmed_at, snapshot_json
         ) VALUES (?, 'workspace-default', NULL, ?, ?, ?, 'owner-local', ?, ?)",
    )
    .bind(&version_id)
    .bind(&draft_id)
    .bind(&row.host_id)
    .bind(row.revision)
    .bind(&confirmed_at)
    .bind(&snapshot_json)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    sqlx::query(
        "UPDATE projection_drafts SET state = 'confirmed', pending_changes = 0,
            snapshot_json = ?, updated_at = ? WHERE draft_id = ? AND revision = ?",
    )
    .bind(snapshot_json)
    .bind(&confirmed_at)
    .bind(&draft_id)
    .bind(row.revision)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    store_mutation(
        &mut tx,
        "confirm",
        &draft_id,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(ProjectionError::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/layouts/{layout_id}",
    tag = "m2",
    params(
        ("layout_id" = String, Path, description = "Canvas layout identifier"),
        ("If-Match" = String, Header, description = "Current revision-N ETag"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = LayoutUpdateRequest,
    responses((status = 200, body = LayoutResponse), (status = 412, body = ApiErrorResponse))
)]
pub async fn update_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(layout_id): Path<String>,
    payload: Result<Json<LayoutUpdateRequest>, JsonRejection>,
) -> Result<Json<LayoutResponse>, ProjectionError> {
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let header_revision = if_match_revision(&headers)?;
    let request_hash =
        payload_sha256(serde_json::to_vec(&request).map_err(|_| ProjectionError::Internal)?);
    if let Some(response) =
        replay_mutation::<LayoutResponse>(&state.pool, "layout", &layout_id, &key, &request_hash)
            .await?
    {
        return Ok(Json(response));
    }
    let draft_id = layout_id
        .strip_prefix("layout-")
        .ok_or_else(|| ProjectionError::not_found("layout", &layout_id))?;
    let mut row = load_draft(&state.pool, draft_id).await?;
    if row.state == ProjectionState::Confirmed {
        return Err(ProjectionError::conflict(
            "DRAFT_ALREADY_CONFIRMED",
            "已确认版本保持不可变；需要从当前版本创建新草稿",
            json!({"draft_id": draft_id}),
        ));
    }
    verify_revision(header_revision, request.base_revision, row.revision)?;
    let positions = request
        .positions
        .iter()
        .map(|position| (position.node_id.as_str(), &position.position))
        .collect::<BTreeMap<_, _>>();
    if positions.len() != request.positions.len() {
        return Err(ProjectionError::bad(
            "DUPLICATE_LAYOUT_NODE",
            "布局中同一节点只能出现一次",
            json!({}),
        ));
    }
    for (node_id, position) in &positions {
        if !position.x.is_finite()
            || !position.y.is_finite()
            || position.x.abs() > 100_000.0
            || position.y.abs() > 100_000.0
        {
            return Err(ProjectionError::bad(
                "INVALID_POSITION",
                "布局位置必须是画布范围内的有限数值",
                json!({"node_id": node_id}),
            ));
        }
        let node = row
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == **node_id)
            .ok_or_else(|| ProjectionError::not_found("projection_node", *node_id))?;
        node.position = (*position).clone();
    }
    row.revision += 1;
    row.snapshot.layout.revision = row.revision;
    row.updated_at = now();
    let response = LayoutResponse {
        data: LayoutUpdate {
            layout_id: layout_id.clone(),
            revision: row.revision,
            positions: request.positions.clone(),
        },
        meta: projection_meta(request_id(&headers), row.revision, Freshness::Fresh),
    };
    let snapshot_json =
        serde_json::to_string(&row.snapshot).map_err(|_| ProjectionError::Internal)?;
    let positions_json =
        serde_json::to_string(&request.positions).map_err(|_| ProjectionError::Internal)?;
    let mut tx = state.pool.begin().await.map_err(ProjectionError::Storage)?;
    let draft_update = sqlx::query(
        "UPDATE projection_drafts SET revision = ?, snapshot_json = ?, updated_at = ?
         WHERE draft_id = ? AND revision = ?",
    )
    .bind(row.revision)
    .bind(snapshot_json)
    .bind(&row.updated_at)
    .bind(draft_id)
    .bind(request.base_revision)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    if draft_update.rows_affected() != 1 {
        return Err(ProjectionError::precondition(
            "PRECONDITION_FAILED",
            "投影修订已被其他修改更新",
            json!({"draft_id": draft_id}),
        ));
    }
    let layout_update = sqlx::query(
        "UPDATE canvas_layouts SET revision = ?, positions_json = ?, updated_at = ?
         WHERE layout_id = ?",
    )
    .bind(row.revision)
    .bind(positions_json)
    .bind(&row.updated_at)
    .bind(&layout_id)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    if layout_update.rows_affected() != 1 {
        return Err(ProjectionError::not_found("layout", &layout_id));
    }
    store_mutation(
        &mut tx,
        "layout",
        &layout_id,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(ProjectionError::Storage)?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/api/v1/ignore-rules",
    tag = "m2",
    params(
        ("If-Match" = String, Header, description = "Current revision-N ETag"),
        ("Idempotency-Key" = String, Header, description = "Stable key for safe request retries")
    ),
    request_body = IgnoreRuleCreateRequest,
    responses((status = 200, body = ProjectionDraftResponse), (status = 412, body = ApiErrorResponse))
)]
pub async fn create_ignore_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<IgnoreRuleCreateRequest>, JsonRejection>,
) -> Result<Json<ProjectionDraftResponse>, ProjectionError> {
    let Json(request) = payload.map_err(invalid_json)?;
    let key = idempotency_key(&headers)?;
    let header_revision = if_match_revision(&headers)?;
    let request_hash =
        payload_sha256(serde_json::to_vec(&request).map_err(|_| ProjectionError::Internal)?);
    if let Some(response) = replay_mutation::<ProjectionDraftResponse>(
        &state.pool,
        "ignore",
        &request.draft_id,
        &key,
        &request_hash,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let mut row = load_draft(&state.pool, &request.draft_id).await?;
    if row.state == ProjectionState::Confirmed {
        return Err(ProjectionError::conflict(
            "DRAFT_ALREADY_CONFIRMED",
            "已确认版本保持不可变；需要从当前版本创建新草稿",
            json!({"draft_id": request.draft_id}),
        ));
    }
    verify_revision(header_revision, request.base_revision, row.revision)?;
    let node = row
        .snapshot
        .nodes
        .iter_mut()
        .find(|node| node.id == request.node_id)
        .ok_or_else(|| ProjectionError::not_found("projection_node", &request.node_id))?;
    let rule_state = match request.action {
        IgnoreRuleAction::Ignore => {
            node.state = ProjectionState::Archived;
            "ignored"
        }
        IgnoreRuleAction::Archive => {
            node.state = ProjectionState::Archived;
            "archived"
        }
        IgnoreRuleAction::Restore => {
            node.state = ProjectionState::Draft;
            "active"
        }
    };
    for edge in &mut row.snapshot.edges {
        if edge.from == request.node_id || edge.to == request.node_id {
            edge.state = if rule_state == "active" {
                ProjectionState::Draft
            } else {
                ProjectionState::Archived
            };
        }
    }
    row.revision += 1;
    row.pending_changes = row.pending_changes.saturating_add(1);
    row.updated_at = now();
    row.snapshot.layout.revision = row.revision;
    let response = draft_response(&row, request_id(&headers));
    let snapshot_json =
        serde_json::to_string(&row.snapshot).map_err(|_| ProjectionError::Internal)?;
    let timestamp = now();
    let mut tx = state.pool.begin().await.map_err(ProjectionError::Storage)?;
    sqlx::query(
        "INSERT INTO ignore_rules(rule_id, host_id, fingerprint, state, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(host_id, fingerprint) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&row.host_id)
    .bind(&request.node_id)
    .bind(rule_state)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    let update = sqlx::query(
        "UPDATE projection_drafts SET revision = ?, pending_changes = ?, snapshot_json = ?,
            updated_at = ? WHERE draft_id = ? AND revision = ?",
    )
    .bind(row.revision)
    .bind(i64::from(row.pending_changes))
    .bind(snapshot_json)
    .bind(&row.updated_at)
    .bind(&row.draft_id)
    .bind(request.base_revision)
    .execute(&mut *tx)
    .await
    .map_err(ProjectionError::Storage)?;
    if update.rows_affected() != 1 {
        return Err(ProjectionError::precondition(
            "PRECONDITION_FAILED",
            "投影修订已被其他修改更新",
            json!({"draft_id": request.draft_id}),
        ));
    }
    store_mutation(
        &mut tx,
        "ignore",
        &row.draft_id,
        &key,
        &request_hash,
        &response,
    )
    .await?;
    tx.commit().await.map_err(ProjectionError::Storage)?;
    Ok(Json(response))
}

async fn latest_host_snapshot(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Option<(GraphSnapshot, i64, bool)>, ProjectionError> {
    if let Some(row) = sqlx::query(
        "SELECT snapshot_json, revision FROM projection_versions
         WHERE host_id = ? ORDER BY confirmed_at DESC, version_id DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(ProjectionError::Storage)?
    {
        let payload: String = row
            .try_get("snapshot_json")
            .map_err(ProjectionError::Storage)?;
        return Ok(Some((
            serde_json::from_str(&payload).map_err(|_| ProjectionError::Internal)?,
            row.try_get("revision").map_err(ProjectionError::Storage)?,
            true,
        )));
    }
    let row = sqlx::query(
        "SELECT snapshot_json, revision FROM projection_drafts
         WHERE host_id = ? ORDER BY updated_at DESC, draft_id DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(ProjectionError::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payload: String = row
        .try_get("snapshot_json")
        .map_err(ProjectionError::Storage)?;
    Ok(Some((
        serde_json::from_str(&payload).map_err(|_| ProjectionError::Internal)?,
        row.try_get("revision").map_err(ProjectionError::Storage)?,
        false,
    )))
}

fn visible_snapshot(mut snapshot: GraphSnapshot) -> GraphSnapshot {
    snapshot
        .nodes
        .retain(|node| node.state != ProjectionState::Archived);
    let visible = snapshot
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    snapshot.edges.retain(|edge| {
        edge.state != ProjectionState::Archived
            && visible.contains(&edge.from)
            && visible.contains(&edge.to)
    });
    snapshot
}

fn projection_health_label(state: &ProjectionState) -> &'static str {
    match state {
        ProjectionState::Fixture => "Fixture",
        ProjectionState::Discovered => "已发现",
        ProjectionState::Draft => "待确认",
        ProjectionState::Confirmed => "已确认",
        ProjectionState::Stale => "已过期",
        ProjectionState::Unavailable => "不可用",
        ProjectionState::Archived => "已归档",
    }
}

fn host_projection_state(status: &HostStatus) -> ProjectionState {
    match status {
        HostStatus::EvidenceReady
        | HostStatus::DiscoveryComplete
        | HostStatus::DiscoveryPartial => ProjectionState::Discovered,
        HostStatus::DiscoveryUnavailable => ProjectionState::Stale,
        HostStatus::Failed | HostStatus::HostKeyChanged => ProjectionState::Unavailable,
        _ => ProjectionState::Stale,
    }
}

async fn registered_host_summaries(pool: &SqlitePool) -> Result<Vec<HostSummary>, ProjectionError> {
    let rows = sqlx::query(
        "SELECT h.host_id, h.display_name, h.address, h.port, h.status,
                h.created_at, h.last_checked_at,
                (SELECT CASE
                    WHEN ct.error_code IN ('DOCKER_PERMISSION_DENIED', 'DOCKER_UNAVAILABLE', 'COMPOSE_UNAVAILABLE')
                      OR (ct.error_code = 'SSH_PROCESS_FAILED' AND lower(COALESCE(ct.error_summary, '')) LIKE 'docker_%')
                    THEN NULL ELSE ct.error_code END
                 FROM connection_tests ct
                 WHERE ct.host_id = h.host_id
                 ORDER BY ct.finished_at DESC, ct.test_id DESC LIMIT 1) AS last_error_code,
                (SELECT CASE
                    WHEN ct.error_code IN ('DOCKER_PERMISSION_DENIED', 'DOCKER_UNAVAILABLE', 'COMPOSE_UNAVAILABLE')
                      OR (ct.error_code = 'SSH_PROCESS_FAILED' AND lower(COALESCE(ct.error_summary, '')) LIKE 'docker_%')
                    THEN NULL ELSE ct.error_summary END
                 FROM connection_tests ct
                 WHERE ct.host_id = h.host_id
                 ORDER BY ct.finished_at DESC, ct.test_id DESC LIMIT 1) AS last_error_summary
         FROM hosts h ORDER BY h.created_at, h.host_id",
    )
    .fetch_all(pool)
    .await
    .map_err(ProjectionError::Storage)?;
    let mut hosts = Vec::with_capacity(rows.len());
    for row in rows {
        let port: i64 = row.try_get("port").map_err(ProjectionError::Storage)?;
        let status_text: String = row.try_get("status").map_err(ProjectionError::Storage)?;
        let status = crate::m1::host_status_from_storage(&status_text);
        let address: String = row.try_get("address").map_err(ProjectionError::Storage)?;
        let created_at: String = row
            .try_get("created_at")
            .map_err(ProjectionError::Storage)?;
        let last_checked_at: Option<String> = row
            .try_get("last_checked_at")
            .map_err(ProjectionError::Storage)?;
        let last_error_code: Option<String> = row
            .try_get("last_error_code")
            .map_err(ProjectionError::Storage)?;
        let last_error_summary: Option<String> = row
            .try_get("last_error_summary")
            .map_err(ProjectionError::Storage)?;
        hosts.push(HostSummary {
            host_id: row.try_get("host_id").map_err(ProjectionError::Storage)?,
            label: row
                .try_get("display_name")
                .map_err(ProjectionError::Storage)?,
            state: host_projection_state(&status),
            address: Some(address),
            port: u16::try_from(port).ok(),
            status: Some(status),
            last_checked_at: last_checked_at.or(Some(created_at)),
            last_error_code,
            last_error_summary,
        });
    }
    Ok(hosts)
}

async fn business_coordinator_agent_node(
    pool: &SqlitePool,
) -> Result<Option<(GraphNode, i64)>, ProjectionError> {
    let row = sqlx::query(
        "SELECT config.base_url, config.model, config.revision, config.updated_at,
                (SELECT test.state FROM model_provider_test_runs test
                 WHERE test.workspace_id = config.workspace_id
                 ORDER BY test.created_at DESC, test.test_id DESC LIMIT 1) AS test_state,
                (SELECT test.error_code FROM model_provider_test_runs test
                 WHERE test.workspace_id = config.workspace_id
                 ORDER BY test.created_at DESC, test.test_id DESC LIMIT 1) AS test_error_code,
                (SELECT test.latency_ms FROM model_provider_test_runs test
                 WHERE test.workspace_id = config.workspace_id
                 ORDER BY test.created_at DESC, test.test_id DESC LIMIT 1) AS test_latency_ms,
                (SELECT test.created_at FROM model_provider_test_runs test
                 WHERE test.workspace_id = config.workspace_id
                 ORDER BY test.created_at DESC, test.test_id DESC LIMIT 1) AS tested_at
         FROM model_provider_configs config
         WHERE config.workspace_id = 'workspace-default'",
    )
    .fetch_optional(pool)
    .await
    .map_err(ProjectionError::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let base_url: String = row.try_get("base_url").map_err(ProjectionError::Storage)?;
    let model: String = row.try_get("model").map_err(ProjectionError::Storage)?;
    let config_revision: i64 = row.try_get("revision").map_err(ProjectionError::Storage)?;
    let updated_at: String = row
        .try_get("updated_at")
        .map_err(ProjectionError::Storage)?;
    let test_state: Option<String> = row
        .try_get("test_state")
        .map_err(ProjectionError::Storage)?;
    let test_error_code: Option<String> = row
        .try_get("test_error_code")
        .map_err(ProjectionError::Storage)?;
    let test_latency_ms: Option<i64> = row
        .try_get("test_latency_ms")
        .map_err(ProjectionError::Storage)?;
    let tested_at: Option<String> = row.try_get("tested_at").map_err(ProjectionError::Storage)?;
    let (state, health_label, health_tone, alerts) = match test_state.as_deref() {
        Some("reachable") => (ProjectionState::Confirmed, "可用", "green", 0),
        Some("failed") => (ProjectionState::Unavailable, "测试失败", "amber", 1),
        _ => (ProjectionState::Stale, "待测试", "muted", 0),
    };
    let mut facts = vec![
        GraphFact {
            label: "OpenAI 兼容 URL".to_owned(),
            value: base_url,
        },
        GraphFact {
            label: "模型".to_owned(),
            value: model.clone(),
        },
        GraphFact {
            label: "Key 状态".to_owned(),
            value: "已保存".to_owned(),
        },
        GraphFact {
            label: "配置版本".to_owned(),
            value: config_revision.to_string(),
        },
    ];
    if let Some(latency_ms) = test_latency_ms {
        facts.push(GraphFact {
            label: "测试延迟".to_owned(),
            value: format!("{latency_ms} ms"),
        });
    }
    if let Some(error_code) = test_error_code {
        facts.push(GraphFact {
            label: "测试错误码".to_owned(),
            value: error_code,
        });
    }
    Ok(Some((
        GraphNode {
            id: "business-coordinator-agent".to_owned(),
            kind: GraphNodeKind::Workspace,
            label: "业务统筹 Agent".to_owned(),
            subtitle: Some(model),
            state,
            source_refs: vec!["registry:model-provider".to_owned()],
            observed_at: Some(tested_at.clone().unwrap_or_else(|| updated_at.clone())),
            position: GraphPosition { x: 390.0, y: 300.0 },
            width: 220.0,
            height: 104.0,
            summary: Some(
                "工作区级业务统筹能力；模型配置已保存，只与真实任务或项目建立统筹关系。".to_owned(),
            ),
            facts,
            project_id: None,
            health: Some(GraphHealth {
                label: health_label.to_owned(),
                tone: health_tone.to_owned(),
                activity: 0,
                alerts,
                updated: tested_at.unwrap_or(updated_at),
            }),
        },
        config_revision,
    )))
}

async fn host_ids(pool: &SqlitePool) -> Result<Vec<String>, ProjectionError> {
    sqlx::query("SELECT host_id FROM hosts ORDER BY created_at, host_id")
        .fetch_all(pool)
        .await
        .map_err(ProjectionError::Storage)?
        .into_iter()
        .map(|row| row.try_get("host_id").map_err(ProjectionError::Storage))
        .collect()
}

pub async fn global_world_response(
    pool: &SqlitePool,
    request_id: String,
) -> Result<Option<GraphSnapshotResponse>, ProjectionError> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_ids = BTreeSet::new();
    let mut edge_ids = BTreeSet::new();
    let mut host_node_ids = BTreeSet::new();
    let mut revision = 0i64;
    let mut all_confirmed = true;
    if let Some((agent, config_revision)) = business_coordinator_agent_node(pool).await? {
        revision = revision.max(config_revision);
        all_confirmed &= agent.state == ProjectionState::Confirmed;
        node_ids.insert(agent.id.clone());
        nodes.push(agent);
    }
    for host_id in host_ids(pool).await? {
        let Some((snapshot, host_revision, confirmed)) =
            latest_host_snapshot(pool, &host_id).await?
        else {
            continue;
        };
        revision = revision.max(host_revision);
        all_confirmed &= confirmed;
        let snapshot = visible_snapshot(snapshot);
        for node in snapshot.nodes {
            if node.kind == GraphNodeKind::Host {
                host_node_ids.insert(node.id);
                continue;
            }
            if node_ids.insert(node.id.clone()) {
                nodes.push(node);
            }
        }
        for edge in snapshot.edges {
            if edge_ids.insert(edge.id.clone()) {
                edges.push(edge);
            }
        }
    }
    edges.retain(|edge| {
        node_ids.contains(&edge.from)
            && node_ids.contains(&edge.to)
            && !host_node_ids.contains(&edge.from)
            && !host_node_ids.contains(&edge.to)
    });
    Ok(Some(GraphSnapshotResponse {
        data: GraphSnapshot {
            focus: GraphFocus {
                kind: GraphScopeKind::Global,
                id: "workspace-default".to_owned(),
            },
            nodes,
            edges,
            layout: CanvasLayout {
                layout_id: "layout-global-real".to_owned(),
                scope: "global".to_owned(),
                revision,
            },
        },
        meta: projection_meta(
            request_id,
            revision,
            if all_confirmed {
                Freshness::Fresh
            } else {
                Freshness::Stale
            },
        ),
    }))
}

pub async fn project_resources_response(
    pool: &SqlitePool,
    project_id: &str,
    request_id: String,
) -> Result<Option<GraphSnapshotResponse>, ProjectionError> {
    let mut selected_nodes = Vec::new();
    let mut selected_edges = Vec::new();
    let mut revision = 0i64;
    let mut confirmed = false;
    for host_id in host_ids(pool).await? {
        let Some((snapshot, host_revision, host_confirmed)) =
            latest_host_snapshot(pool, &host_id).await?
        else {
            continue;
        };
        if !snapshot
            .nodes
            .iter()
            .any(|node| node.id == project_id && node.kind == GraphNodeKind::Project)
        {
            continue;
        }
        revision = host_revision;
        confirmed = host_confirmed;
        let snapshot = visible_snapshot(snapshot);
        let mut ids = snapshot
            .nodes
            .iter()
            .filter(|node| node.id == project_id || node.project_id.as_deref() == Some(project_id))
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        loop {
            let before = ids.len();
            for edge in &snapshot.edges {
                if ids.contains(&edge.from) && edge.from != host_id {
                    ids.insert(edge.to.clone());
                }
            }
            if ids.len() == before {
                break;
            }
        }
        selected_nodes = snapshot
            .nodes
            .into_iter()
            .filter(|node| ids.contains(&node.id))
            .collect();
        selected_edges = snapshot
            .edges
            .into_iter()
            .filter(|edge| ids.contains(&edge.from) && ids.contains(&edge.to))
            .collect();
        break;
    }
    if selected_nodes.is_empty() {
        return Ok(None);
    }
    Ok(Some(GraphSnapshotResponse {
        data: GraphSnapshot {
            focus: GraphFocus {
                kind: GraphScopeKind::Project,
                id: project_id.to_owned(),
            },
            nodes: selected_nodes,
            edges: selected_edges,
            layout: CanvasLayout {
                layout_id: format!("layout-project-{project_id}"),
                scope: format!("project:{project_id}"),
                revision,
            },
        },
        meta: projection_meta(
            request_id,
            revision,
            if confirmed {
                Freshness::Fresh
            } else {
                Freshness::Stale
            },
        ),
    }))
}

pub async fn bootstrap_response(
    pool: &SqlitePool,
    request_id: String,
) -> Result<Option<BootstrapResponse>, ProjectionError> {
    let Some(global) = global_world_response(pool, request_id.clone()).await? else {
        return Ok(None);
    };
    let projects = global
        .data
        .nodes
        .iter()
        .filter(|node| node.kind == GraphNodeKind::Project)
        .map(|node| ProjectSummary {
            project_id: node.id.clone(),
            label: node.label.clone(),
            subtitle: node
                .subtitle
                .clone()
                .unwrap_or_else(|| "本地项目投影".to_owned()),
            state: node.state.clone(),
            health: node
                .health
                .as_ref()
                .map(|health| health.label.clone())
                .unwrap_or_else(|| projection_health_label(&node.state).to_owned()),
            tone: node
                .health
                .as_ref()
                .map(|health| health.tone.clone())
                .unwrap_or_else(|| {
                    if node.state == ProjectionState::Confirmed {
                        "green"
                    } else {
                        "amber"
                    }
                    .to_owned()
                }),
            activity: global
                .data
                .nodes
                .iter()
                .filter(|candidate| candidate.project_id.as_deref() == Some(&node.id))
                .count() as u32,
            alerts: 0,
        })
        .collect::<Vec<_>>();
    let hosts = registered_host_summaries(pool).await?;
    let healthy = projects
        .iter()
        .filter(|project| project.tone == "green")
        .count() as u32;
    let attention = projects
        .iter()
        .filter(|project| project.tone == "amber")
        .count() as u32;
    let unassigned = global
        .data
        .nodes
        .iter()
        .filter(|node| {
            !matches!(node.kind, GraphNodeKind::Host | GraphNodeKind::Project)
                && node.project_id.is_none()
        })
        .count() as u32;
    Ok(Some(BootstrapResponse {
        data: BootstrapData {
            workspace_id: "workspace-default".to_owned(),
            navigation: NavigationCounts {
                projects: projects.len() as u32,
                healthy,
                attention,
                unassigned,
            },
            features: FeatureAvailability {
                global_world: true,
                project_resources: true,
                global_resources: false,
                project_workflow: false,
                agent_assistance: true,
            },
            projects,
            hosts,
        },
        meta: global.meta,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{EvidenceHostIdentity, EvidenceItem, EvidenceKind, RedactionState};

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

    #[test]
    fn deterministic_mapper_keeps_sources_and_builds_project_topology() {
        let evidence = DiscoveryEvidence {
            protocol_version: "1".to_owned(),
            discovery_id: "run-1".to_owned(),
            host: EvidenceHostIdentity {
                host_id: "host-1".to_owned(),
                address: "fixture.local".to_owned(),
                os: "linux".to_owned(),
            },
            host_facts: vec![item(
                EvidenceKind::HostIdentity,
                "host-fact",
                "ssh:host",
                json!({"kernel": "Linux"}),
            )],
            docker_engines: Vec::new(),
            compose_projects: vec![item(
                EvidenceKind::ComposeProject,
                "compose-1",
                "ssh:compose_ls",
                json!({"name": "sample", "status": "running"}),
            )],
            systemd_units: Vec::new(),
            containers: vec![item(
                EvidenceKind::Container,
                "container-1",
                "ssh:containers",
                json!({
                    "id": "container-1",
                    "name": "sample-api",
                    "image": "sample/api:latest",
                    "networks": "sample-net",
                    "mounts": "sample-data",
                    "ports": "0.0.0.0:8080->8080/tcp",
                    "compose_project": "sample",
                    "compose_service": "api"
                }),
            )],
            images: vec![item(
                EvidenceKind::Image,
                "image-1",
                "ssh:image",
                json!({"id": "image-1", "repository": "sample/api", "tag": "latest"}),
            )],
            networks: vec![item(
                EvidenceKind::Network,
                "network-1",
                "ssh:network",
                json!({"id": "network-1", "name": "sample-net"}),
            )],
            volumes: vec![item(
                EvidenceKind::Volume,
                "volume-1",
                "ssh:volume",
                json!({"name": "sample-data"}),
            )],
            document_candidates: vec![item(
                EvidenceKind::Document,
                "document-1",
                "ssh:document",
                json!({"relative_path": "README.md"}),
            )],
            health_checks: Vec::new(),
            warnings: Vec::new(),
            provider_results: Vec::new(),
            started_at: "2026-08-11T00:00:00Z".to_owned(),
            finished_at: "2026-08-11T00:00:01Z".to_owned(),
        };
        let first = map_evidence("draft-1", "run-1", "host-1", &evidence, &BTreeSet::new());
        let second = map_evidence("draft-1", "run-1", "host-1", &evidence, &BTreeSet::new());
        assert_eq!(first, second);
        assert!(
            first
                .nodes
                .iter()
                .any(|node| node.kind == GraphNodeKind::Project)
        );
        assert!(
            first
                .nodes
                .iter()
                .any(|node| node.kind == GraphNodeKind::Service)
        );
        assert!(
            first
                .nodes
                .iter()
                .any(|node| node.kind == GraphNodeKind::Port)
        );
        assert!(
            first
                .edges
                .iter()
                .any(|edge| edge.kind == GraphRelationKind::UsesImage)
        );
        assert!(
            first
                .nodes
                .iter()
                .filter(|node| node.kind != GraphNodeKind::Host)
                .all(|node| !node.source_refs.is_empty())
        );
        let service = first
            .nodes
            .iter()
            .find(|node| node.kind == GraphNodeKind::Service)
            .expect("service node");
        assert!(
            service
                .facts
                .iter()
                .any(|fact| fact.label == "原始名称" && fact.value == "api")
        );
    }
}
