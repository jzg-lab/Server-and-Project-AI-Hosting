//! Logical TechnicalProject-bound Agent with a closed typed read-only tool set.
//!
//! This module is intentionally independent from the M3 onboarding Agent.  It
//! never calls SSH, a model provider, a shell, or a mutating catalog command.
//! Every target-scoped read verifies the ProjectTarget belongs to the bound
//! TechnicalProject before loading any deployment or HOST data.

use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, DeploymentObservationRecord,
        DeploymentObservationState, DeploymentRecord, DiscoveryChangeKind, DiscoveryDiffCounts,
        DiscoveryDiffData, Freshness, MonitorFreshness, ProjectAgentBindRequest,
        ProjectAgentCapability, ProjectAgentDeploymentObservationData,
        ProjectAgentHostCapabilitiesData, ProjectAgentRecentDiffData, ProjectAgentRecord,
        ProjectAgentResponse, ProjectAgentServiceStatusData, ProjectAgentState,
        ProjectAgentTargetsData, ProjectAgentToolCallData, ProjectAgentToolName,
        ProjectAgentToolRequest, ProjectAgentToolResponse, ProjectAgentToolResult,
        ProjectTargetAdapterKind, ProjectTargetCapability, ProjectTargetRecord, ProjectTargetState,
        TechnicalProjectRecord,
    },
    events::{self, ChangeEventKind},
};

const WORKSPACE_ID: &str = "workspace-default";
const MAX_IDEMPOTENCY_KEY: usize = 200;
const MAX_NAME_CHARS: usize = 160;
const MAX_TOOL_LIMIT: u16 = 200;

const TOOL_NAMES: [&str; 5] = [
    "list_project_targets",
    "read_deployment_observation",
    "read_host_capabilities",
    "read_service_status",
    "read_recent_diff",
];

#[derive(Debug, Error)]
pub enum ProjectAgentError {
    #[error("invalid Project Agent request")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("Project Agent object was not found")]
    NotFound { resource: &'static str, id: String },
    #[error("Project Agent scope conflict")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("Project Agent storage failed")]
    Storage(#[source] sqlx::Error),
    #[error("stored Project Agent response is invalid")]
    StoredResponse,
    #[error("Project Agent serialization failed")]
    Serialization(#[source] serde_json::Error),
}

impl IntoResponse for ProjectAgentError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details) = match self {
            Self::BadRequest {
                code,
                message,
                details,
            } => (StatusCode::BAD_REQUEST, code, message, details),
            Self::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的项目 Agent 对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "project agent storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PROJECT_AGENT_STORAGE_UNAVAILABLE",
                    "项目 Agent 数据暂不可用",
                    json!({}),
                )
            }
            Self::StoredResponse | Self::Serialization(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "PROJECT_AGENT_STATE_INVALID",
                "项目 Agent 状态无法完成该操作",
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

fn idempotency_key(headers: &HeaderMap) -> Result<String, ProjectAgentError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .ok_or(ProjectAgentError::BadRequest {
            code: "IDEMPOTENCY_KEY_REQUIRED",
            message: "绑定项目 Agent 需要 Idempotency-Key",
            details: json!({}),
        })
}

fn meta(request_id: &str, freshness: Freshness, revision: i64) -> ApiMeta {
    let status = match freshness {
        Freshness::Fresh => crate::contracts::DataSourceStatus::Fresh,
        Freshness::Stale => crate::contracts::DataSourceStatus::Stale,
        Freshness::Unavailable => crate::contracts::DataSourceStatus::Unavailable,
    };
    ApiMeta {
        request_id: request_id.to_owned(),
        revision,
        generated_at: now(),
        freshness,
        data_source: crate::contracts::DataSourceDescriptor {
            kind: crate::contracts::DataSourceKind::Real,
            status,
            label: "Project Agent · typed read-only adapter".to_owned(),
        },
    }
}

fn normalized_name(value: Option<&str>, default: &str) -> Result<String, ProjectAgentError> {
    let value = value.unwrap_or(default).trim();
    if value.is_empty()
        || value.chars().count() > MAX_NAME_CHARS
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ProjectAgentError::BadRequest {
            code: "INVALID_AGENT_NAME",
            message: "项目 Agent 名称必须是可见文本",
            details: json!({}),
        });
    }
    Ok(value.to_owned())
}

fn digest_json<T: Serialize>(value: &T) -> Result<String, ProjectAgentError> {
    let bytes = serde_json::to_vec(value).map_err(ProjectAgentError::Serialization)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

async fn replay_mutation<T: DeserializeOwned>(
    pool: &SqlitePool,
    resource_id: &str,
    key: &str,
    digest: &str,
) -> Result<Option<T>, ProjectAgentError> {
    let row = sqlx::query(
        "SELECT request_sha256, response_json FROM catalog_mutation_requests
         WHERE workspace_id = ? AND resource_kind = 'project_agent.bind'
           AND resource_id = ? AND idempotency_key = ?",
    )
    .bind(WORKSPACE_ID)
    .bind(resource_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    let Some(row) = row else { return Ok(None) };
    let stored: String = row
        .try_get("request_sha256")
        .map_err(ProjectAgentError::Storage)?;
    if stored != digest {
        return Err(ProjectAgentError::Conflict {
            code: "IDEMPOTENCY_KEY_REUSED",
            message: "Idempotency-Key 已用于不同的项目 Agent 绑定请求",
            details: json!({"technical_project_id": resource_id}),
        });
    }
    let response: String = row
        .try_get("response_json")
        .map_err(ProjectAgentError::Storage)?;
    serde_json::from_str(&response)
        .map(Some)
        .map_err(|_| ProjectAgentError::StoredResponse)
}

async fn store_mutation(
    connection: &mut SqliteConnection,
    resource_id: &str,
    key: &str,
    digest: &str,
    response: &ProjectAgentResponse,
) -> Result<(), ProjectAgentError> {
    sqlx::query(
        "INSERT INTO catalog_mutation_requests(
            catalog_request_id, workspace_id, resource_kind, resource_id,
            idempotency_key, request_sha256, response_json, created_at
         ) VALUES (?, ?, 'project_agent.bind', ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(WORKSPACE_ID)
    .bind(resource_id)
    .bind(key)
    .bind(digest)
    .bind(serde_json::to_string(response).map_err(ProjectAgentError::Serialization)?)
    .bind(now())
    .execute(connection)
    .await
    .map_err(ProjectAgentError::Storage)?;
    Ok(())
}

fn parse<T: DeserializeOwned>(value: &str) -> Result<T, ProjectAgentError> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|_| ProjectAgentError::StoredResponse)
}

fn parse_json<T: DeserializeOwned>(value: &str) -> Result<T, ProjectAgentError> {
    serde_json::from_str(value).map_err(|_| ProjectAgentError::StoredResponse)
}

fn tool_name(value: &str) -> Result<ProjectAgentToolName, ProjectAgentError> {
    let value = value.trim();
    if !TOOL_NAMES.contains(&value) {
        return Err(ProjectAgentError::BadRequest {
            code: "PROJECT_AGENT_TOOL_NOT_ALLOWED",
            message: "该项目 Agent 工具不在固定只读工具目录中",
            details: json!({"tool_name": value, "allowed_tools": TOOL_NAMES}),
        });
    }
    parse(value)
}

fn tool_name_str(value: &ProjectAgentToolName) -> &'static str {
    match value {
        ProjectAgentToolName::ListProjectTargets => "list_project_targets",
        ProjectAgentToolName::ReadDeploymentObservation => "read_deployment_observation",
        ProjectAgentToolName::ReadHostCapabilities => "read_host_capabilities",
        ProjectAgentToolName::ReadServiceStatus => "read_service_status",
        ProjectAgentToolName::ReadRecentDiff => "read_recent_diff",
    }
}

fn agent_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ProjectAgentRecord, ProjectAgentError> {
    let capabilities: String = row
        .try_get("capabilities_json")
        .map_err(ProjectAgentError::Storage)?;
    let tools: String = row
        .try_get("tool_names_json")
        .map_err(ProjectAgentError::Storage)?;
    Ok(ProjectAgentRecord {
        project_agent_id: row
            .try_get("project_agent_id")
            .map_err(ProjectAgentError::Storage)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(ProjectAgentError::Storage)?,
        technical_project_id: row
            .try_get("technical_project_id")
            .map_err(ProjectAgentError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(ProjectAgentError::Storage)?,
        state: parse(
            &row.try_get::<String, _>("state")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        capabilities: parse_json::<Vec<String>>(&capabilities)?
            .iter()
            .map(|value| parse(value))
            .collect::<Result<Vec<ProjectAgentCapability>, _>>()?,
        tool_names: parse_json::<Vec<String>>(&tools)?
            .iter()
            .map(|value| parse(value))
            .collect::<Result<Vec<ProjectAgentToolName>, _>>()?,
        revision: row
            .try_get("revision")
            .map_err(ProjectAgentError::Storage)?,
        created_by: row
            .try_get("created_by")
            .map_err(ProjectAgentError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(ProjectAgentError::Storage)?,
        updated_by: row
            .try_get("updated_by")
            .map_err(ProjectAgentError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(ProjectAgentError::Storage)?,
    })
}

async fn load_agent(
    pool: &SqlitePool,
    agent_id: &str,
) -> Result<ProjectAgentRecord, ProjectAgentError> {
    let row = sqlx::query(
        "SELECT project_agent_id, workspace_id, technical_project_id, display_name,
                state, capabilities_json, tool_names_json, revision, created_by, created_at,
                updated_by, updated_at
         FROM project_agents WHERE project_agent_id = ? AND workspace_id = ?",
    )
    .bind(agent_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?
    .ok_or_else(|| ProjectAgentError::NotFound {
        resource: "project_agent",
        id: agent_id.to_owned(),
    })?;
    agent_from_row(&row)
}

async fn load_agent_for_project(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<ProjectAgentRecord, ProjectAgentError> {
    let row = sqlx::query(
        "SELECT project_agent_id, workspace_id, technical_project_id, display_name,
                state, capabilities_json, tool_names_json, revision, created_by, created_at,
                updated_by, updated_at
         FROM project_agents WHERE technical_project_id = ? AND workspace_id = ?",
    )
    .bind(project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?
    .ok_or_else(|| ProjectAgentError::NotFound {
        resource: "project_agent",
        id: project_id.to_owned(),
    })?;
    agent_from_row(&row)
}

fn technical_project_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<TechnicalProjectRecord, ProjectAgentError> {
    Ok(TechnicalProjectRecord {
        technical_project_id: row
            .try_get("technical_project_id")
            .map_err(ProjectAgentError::Storage)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(ProjectAgentError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(ProjectAgentError::Storage)?,
        summary: row.try_get("summary").map_err(ProjectAgentError::Storage)?,
        state: parse(
            &row.try_get::<String, _>("state")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        revision: row
            .try_get("revision")
            .map_err(ProjectAgentError::Storage)?,
        created_by: row
            .try_get("created_by")
            .map_err(ProjectAgentError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(ProjectAgentError::Storage)?,
        updated_by: row
            .try_get("updated_by")
            .map_err(ProjectAgentError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(ProjectAgentError::Storage)?,
    })
}

async fn load_project(
    pool: &SqlitePool,
    project_id: &str,
    require_active: bool,
) -> Result<TechnicalProjectRecord, ProjectAgentError> {
    let row = sqlx::query(
        "SELECT technical_project_id, workspace_id, display_name, summary, state, revision,
                created_by, created_at, updated_by, updated_at
         FROM technical_projects WHERE technical_project_id = ? AND workspace_id = ?",
    )
    .bind(project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?
    .ok_or_else(|| ProjectAgentError::NotFound {
        resource: "technical_project",
        id: project_id.to_owned(),
    })?;
    let project = technical_project_from_row(&row)?;
    if require_active
        && !matches!(
            project.state,
            crate::contracts::TechnicalProjectState::Active
        )
    {
        return Err(ProjectAgentError::Conflict {
            code: "TECHNICAL_PROJECT_NOT_ACTIVE",
            message: "项目 Agent 只能绑定或读取活动 TechnicalProject",
            details: json!({"technical_project_id": project_id}),
        });
    }
    Ok(project)
}

fn target_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ProjectTargetRecord, ProjectAgentError> {
    let capabilities: String = row
        .try_get("capabilities_json")
        .map_err(ProjectAgentError::Storage)?;
    Ok(ProjectTargetRecord {
        project_target_id: row
            .try_get("project_target_id")
            .map_err(ProjectAgentError::Storage)?,
        technical_project_id: row
            .try_get("technical_project_id")
            .map_err(ProjectAgentError::Storage)?,
        deployment_id: row
            .try_get("deployment_id")
            .map_err(ProjectAgentError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(ProjectAgentError::Storage)?,
        adapter_kind: parse(
            &row.try_get::<String, _>("adapter_kind")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        capabilities: parse_json::<Vec<String>>(&capabilities)?
            .iter()
            .map(|value| parse(value))
            .collect::<Result<Vec<ProjectTargetCapability>, _>>()?,
        approval_policy: parse(
            &row.try_get::<String, _>("approval_policy")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        state: parse(
            &row.try_get::<String, _>("state")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        revision: row
            .try_get("revision")
            .map_err(ProjectAgentError::Storage)?,
        confirmed_by: row
            .try_get("confirmed_by")
            .map_err(ProjectAgentError::Storage)?,
        confirmed_at: row
            .try_get("confirmed_at")
            .map_err(ProjectAgentError::Storage)?,
        last_observed_at: row
            .try_get("last_observed_at")
            .map_err(ProjectAgentError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(ProjectAgentError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(ProjectAgentError::Storage)?,
    })
}

fn deployment_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<DeploymentRecord, ProjectAgentError> {
    Ok(DeploymentRecord {
        deployment_id: row
            .try_get("deployment_id")
            .map_err(ProjectAgentError::Storage)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(ProjectAgentError::Storage)?,
        host_id: row.try_get("host_id").map_err(ProjectAgentError::Storage)?,
        provider_kind: parse(
            &row.try_get::<String, _>("provider_kind")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        external_id: row
            .try_get("external_id")
            .map_err(ProjectAgentError::Storage)?,
        identity_key: row
            .try_get("identity_key")
            .map_err(ProjectAgentError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(ProjectAgentError::Storage)?,
        state: parse(
            &row.try_get::<String, _>("catalog_state")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        latest_observation_id: row
            .try_get("latest_observation_id")
            .map_err(ProjectAgentError::Storage)?,
        last_observed_at: row
            .try_get("last_observed_at")
            .map_err(ProjectAgentError::Storage)?,
        last_observed_at_epoch_ms: row
            .try_get("last_observed_at_epoch_ms")
            .map_err(ProjectAgentError::Storage)?,
        freshness: parse(
            &row.try_get::<String, _>("freshness")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        created_at: row
            .try_get("created_at")
            .map_err(ProjectAgentError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(ProjectAgentError::Storage)?,
    })
}

fn observation_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<DeploymentObservationRecord, ProjectAgentError> {
    let refs: String = row
        .try_get("evidence_refs_json")
        .map_err(ProjectAgentError::Storage)?;
    let metadata: String = row
        .try_get("metadata_json")
        .map_err(ProjectAgentError::Storage)?;
    Ok(DeploymentObservationRecord {
        deployment_observation_id: row
            .try_get("deployment_observation_id")
            .map_err(ProjectAgentError::Storage)?,
        deployment_id: row
            .try_get("deployment_id")
            .map_err(ProjectAgentError::Storage)?,
        discovery_run_id: row
            .try_get("discovery_run_id")
            .map_err(ProjectAgentError::Storage)?,
        provider_kind: parse(
            &row.try_get::<String, _>("provider_kind")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        external_id: row
            .try_get("external_id")
            .map_err(ProjectAgentError::Storage)?,
        observation_state: parse(
            &row.try_get::<String, _>("observation_state")
                .map_err(ProjectAgentError::Storage)?,
        )?,
        provider_status: row
            .try_get::<Option<String>, _>("provider_status")
            .map_err(ProjectAgentError::Storage)?
            .as_deref()
            .map(parse)
            .transpose()?,
        observed_at: row
            .try_get("observed_at")
            .map_err(ProjectAgentError::Storage)?,
        observed_at_epoch_ms: row
            .try_get("observed_at_epoch_ms")
            .map_err(ProjectAgentError::Storage)?,
        evidence_refs: parse_json(&refs)?,
        metadata: parse_json(&metadata)?,
        created_at: row
            .try_get("created_at")
            .map_err(ProjectAgentError::Storage)?,
    })
}

async fn load_targets(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<ProjectTargetRecord>, ProjectAgentError> {
    let rows = sqlx::query(
        "SELECT project_target_id, technical_project_id, deployment_id, display_name,
                adapter_kind, capabilities_json, approval_policy, state, revision, confirmed_by,
                confirmed_at, last_observed_at, created_at, updated_at
         FROM project_targets WHERE technical_project_id = ?
         ORDER BY updated_at DESC, project_target_id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    rows.iter().map(target_from_row).collect()
}

struct TargetContext {
    deployment: DeploymentRecord,
}

async fn load_target_context(
    pool: &SqlitePool,
    project_id: &str,
    target_id: &str,
) -> Result<TargetContext, ProjectAgentError> {
    let row = sqlx::query(
        "SELECT targets.project_target_id, targets.technical_project_id, targets.deployment_id,
                targets.display_name, targets.adapter_kind, targets.capabilities_json,
                targets.approval_policy, targets.state, targets.revision, targets.confirmed_by,
                targets.confirmed_at, targets.last_observed_at, targets.created_at, targets.updated_at,
                deployments.deployment_id AS deployment_id, deployments.workspace_id,
                deployments.host_id, deployments.provider_kind, deployments.external_id,
                deployments.identity_key, deployments.display_name AS deployment_display_name,
                deployments.catalog_state, deployments.latest_observation_id,
                deployments.last_observed_at AS deployment_last_observed_at,
                deployments.last_observed_at_epoch_ms, deployments.freshness,
                deployments.created_at AS deployment_created_at,
                deployments.updated_at AS deployment_updated_at
         FROM project_targets targets
         JOIN technical_projects projects
           ON projects.technical_project_id = targets.technical_project_id
         JOIN deployments ON deployments.deployment_id = targets.deployment_id
         WHERE targets.project_target_id = ?
           AND targets.technical_project_id = ?
           AND projects.workspace_id = ?",
    )
    .bind(target_id)
    .bind(project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?
    .ok_or_else(|| ProjectAgentError::NotFound { resource: "project_target", id: target_id.to_owned() })?;
    let target = target_from_row(&row)?;
    if matches!(target.state, ProjectTargetState::Archived)
        || !target
            .capabilities
            .contains(&ProjectTargetCapability::ReadOnly)
        || !matches!(target.adapter_kind, ProjectTargetAdapterKind::ReadOnly)
    {
        return Err(ProjectAgentError::Conflict {
            code: "PROJECT_TARGET_NOT_READABLE",
            message: "该 ProjectTarget 当前没有可用的只读观察能力",
            details: json!({"project_target_id": target_id, "state": target.state}),
        });
    }
    let deployment = deployment_from_row(&row)?;
    Ok(TargetContext { deployment })
}

async fn deployment_observations(
    pool: &SqlitePool,
    deployment_id: &str,
    limit: u16,
) -> Result<Vec<DeploymentObservationRecord>, ProjectAgentError> {
    let rows = sqlx::query(
        "SELECT deployment_observation_id, deployment_id, discovery_run_id, provider_kind,
                external_id, observation_state, provider_status, observed_at,
                observed_at_epoch_ms, evidence_refs_json, metadata_json, created_at
         FROM deployment_observations WHERE deployment_id = ?
         ORDER BY COALESCE(observed_at_epoch_ms, 0) DESC, created_at DESC
         LIMIT ?",
    )
    .bind(deployment_id)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    rows.iter().map(observation_from_row).collect()
}

async fn host_capabilities(
    pool: &SqlitePool,
    target_id: &str,
    host_id: &str,
    deployment_external_id: &str,
) -> Result<(ProjectAgentHostCapabilitiesData, Vec<String>), ProjectAgentError> {
    let host = sqlx::query(
        "SELECT host_id, display_name, os, transport, status
         FROM hosts WHERE host_id = ? AND workspace_id = ?",
    )
    .bind(host_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?
    .ok_or_else(|| ProjectAgentError::NotFound {
        resource: "host",
        id: host_id.to_owned(),
    })?;
    let latest_run = sqlx::query(
        "SELECT run_id, state, evidence_json FROM discovery_runs
         WHERE host_id = ? ORDER BY submitted_at DESC, rowid DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    let (latest_discovery_run_id, latest_discovery_state, provider_coverage, mut refs) =
        if let Some(run) = latest_run {
            let run_id: String = run.try_get("run_id").map_err(ProjectAgentError::Storage)?;
            let state: String = run.try_get("state").map_err(ProjectAgentError::Storage)?;
            let evidence_json: Option<String> = run
                .try_get("evidence_json")
                .map_err(ProjectAgentError::Storage)?;
            let evidence = evidence_json.as_deref().and_then(|payload| {
                serde_json::from_str::<crate::contracts::DiscoveryEvidence>(payload).ok()
            });
            let target_evidence_ref = format!("evidence:{deployment_external_id}");
            let coverage = evidence
                .as_ref()
                .map(|value| {
                    value
                        .provider_results
                        .iter()
                        .cloned()
                        .map(|mut provider| {
                            provider
                                .evidence_refs
                                .retain(|reference| reference == &target_evidence_ref);
                            provider
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut refs = vec![format!("discovery:{run_id}")];
            if let Some(evidence) = evidence {
                refs.extend(
                    evidence
                        .provider_results
                        .iter()
                        .flat_map(|provider| provider.evidence_refs.clone())
                        .filter(|reference| reference == &target_evidence_ref),
                );
            }
            (Some(run_id), Some(parse(&state)?), coverage, refs)
        } else {
            (None, None, Vec::new(), Vec::new())
        };
    let current =
        sqlx::query("SELECT observed_at, valid_until FROM monitoring_current WHERE host_id = ?")
            .bind(host_id)
            .fetch_optional(pool)
            .await
            .map_err(ProjectAgentError::Storage)?;
    let (monitor_freshness, monitor_observed_at) = if let Some(row) = current {
        let observed_at: String = row
            .try_get("observed_at")
            .map_err(ProjectAgentError::Storage)?;
        let valid_until: String = row
            .try_get("valid_until")
            .map_err(ProjectAgentError::Storage)?;
        let freshness = DateTime::parse_from_rfc3339(&valid_until)
            .ok()
            .map(|value| {
                if value.with_timezone(&Utc) >= Utc::now() {
                    MonitorFreshness::Fresh
                } else {
                    MonitorFreshness::Stale
                }
            })
            .unwrap_or(MonitorFreshness::Unknown);
        (freshness, Some(observed_at))
    } else {
        (MonitorFreshness::Unknown, None)
    };
    refs.push(format!("project-target:{target_id}"));
    Ok((
        ProjectAgentHostCapabilitiesData {
            host_id: host
                .try_get("host_id")
                .map_err(ProjectAgentError::Storage)?,
            display_name: host
                .try_get("display_name")
                .map_err(ProjectAgentError::Storage)?,
            os: host.try_get("os").map_err(ProjectAgentError::Storage)?,
            transport: host
                .try_get("transport")
                .map_err(ProjectAgentError::Storage)?,
            status: parse(
                &host
                    .try_get::<String, _>("status")
                    .map_err(ProjectAgentError::Storage)?,
            )?,
            provider_coverage,
            latest_discovery_run_id,
            latest_discovery_state,
            monitor_freshness,
            monitor_observed_at,
        },
        refs,
    ))
}

async fn recent_diff(
    pool: &SqlitePool,
    project_target_id: &str,
    host_id: &str,
    deployment_external_id: &str,
) -> Result<(ProjectAgentRecentDiffData, Vec<String>), ProjectAgentError> {
    let row = sqlx::query(
        "SELECT diff_id, run_id, host_id, previous_run_id, counts_json, items_json, created_at
         FROM discovery_diffs WHERE host_id = ? ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    let Some(row) = row else {
        return Ok((
            ProjectAgentRecentDiffData {
                project_target_id: project_target_id.to_owned(),
                host_id: host_id.to_owned(),
                diff: None,
            },
            vec![format!("project-target:{project_target_id}")],
        ));
    };
    let all_items: Vec<crate::contracts::DiscoveryDiffItem> = parse_json(
        &row.try_get::<String, _>("items_json")
            .map_err(ProjectAgentError::Storage)?,
    )?;
    // A discovery diff is HOST-wide. Filter it before exposing it to a
    // project-scoped Agent so another project's deployment names/evidence do
    // not cross the ProjectTarget boundary.
    let target_evidence_ref = format!("evidence:{deployment_external_id}");
    let items = all_items
        .into_iter()
        .filter(|item| {
            item.external_id == deployment_external_id
                || item
                    .evidence_refs
                    .iter()
                    .any(|reference| reference == &target_evidence_ref)
        })
        .collect::<Vec<_>>();
    let mut counts = DiscoveryDiffCounts::default();
    for item in &items {
        match item.change {
            DiscoveryChangeKind::Added => counts.added += 1,
            DiscoveryChangeKind::Changed => counts.changed += 1,
            DiscoveryChangeKind::Missing => counts.missing += 1,
            DiscoveryChangeKind::Conflict => counts.conflict += 1,
            DiscoveryChangeKind::Unchanged => counts.unchanged += 1,
        }
    }
    let diff = DiscoveryDiffData {
        diff_id: row.try_get("diff_id").map_err(ProjectAgentError::Storage)?,
        run_id: row.try_get("run_id").map_err(ProjectAgentError::Storage)?,
        host_id: row.try_get("host_id").map_err(ProjectAgentError::Storage)?,
        previous_run_id: row
            .try_get("previous_run_id")
            .map_err(ProjectAgentError::Storage)?,
        counts,
        items,
        created_at: row
            .try_get("created_at")
            .map_err(ProjectAgentError::Storage)?,
    };
    let mut refs = vec![
        format!("discovery:{}", diff.run_id),
        format!("project-target:{project_target_id}"),
    ];
    refs.extend(
        diff.items
            .iter()
            .flat_map(|item| item.evidence_refs.clone()),
    );
    Ok((
        ProjectAgentRecentDiffData {
            project_target_id: project_target_id.to_owned(),
            host_id: host_id.to_owned(),
            diff: Some(diff),
        },
        refs,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/technical-projects/{technical_project_id}/agent",
    tag = "project-agent",
    params(("technical_project_id" = String, Path), ("Idempotency-Key" = String, Header)),
    request_body = ProjectAgentBindRequest,
    responses((status = 201, body = ProjectAgentResponse), (status = 400, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn bind_project_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    payload: Result<Json<ProjectAgentBindRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ProjectAgentResponse>), ProjectAgentError> {
    let key = idempotency_key(&headers)?;
    let Json(request) = payload.map_err(|_| ProjectAgentError::BadRequest {
        code: "INVALID_JSON",
        message: "请求正文不是符合契约的 JSON",
        details: json!({}),
    })?;
    // The retry digest is based only on the caller-owned payload.  A later
    // project rename must not make an otherwise identical retry conflict.
    let digest = digest_json(&request)?;
    // Confirm that the identity still exists, but allow a committed retry to
    // replay after the project was subsequently archived.
    let project = load_project(&state.pool, &project_id, false).await?;
    if let Some(response) =
        replay_mutation::<ProjectAgentResponse>(&state.pool, &project_id, &key, &digest).await?
    {
        return Ok((StatusCode::CREATED, Json(response)));
    }
    if !matches!(
        project.state,
        crate::contracts::TechnicalProjectState::Active
    ) {
        return Err(ProjectAgentError::Conflict {
            code: "TECHNICAL_PROJECT_NOT_ACTIVE",
            message: "项目 Agent 只能绑定活动 TechnicalProject",
            details: json!({"technical_project_id": project_id}),
        });
    }
    let display_name = normalized_name(
        request.display_name.as_deref(),
        &format!("{} Project Agent", project.display_name),
    )?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT project_agent_id FROM project_agents
         WHERE technical_project_id = ? AND workspace_id = ?",
    )
    .bind(&project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&state.pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    if let Some(agent_id) = existing {
        return Err(ProjectAgentError::Conflict {
            code: "PROJECT_AGENT_ALREADY_BOUND",
            message: "该 TechnicalProject 已绑定 Project Agent",
            details: json!({"project_agent_id": agent_id, "technical_project_id": project_id}),
        });
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_one(&state.pool)
            .await
            .map_err(ProjectAgentError::Storage)?;
    let timestamp = now();
    let agent_id = Uuid::new_v4().to_string();
    let tools = serde_json::to_string(&TOOL_NAMES).map_err(ProjectAgentError::Serialization)?;
    let capabilities =
        serde_json::to_string(&["read_only"]).map_err(ProjectAgentError::Serialization)?;
    let record = ProjectAgentRecord {
        project_agent_id: agent_id.clone(),
        workspace_id: WORKSPACE_ID.to_owned(),
        technical_project_id: project_id.clone(),
        display_name,
        state: ProjectAgentState::Active,
        capabilities: vec![ProjectAgentCapability::ReadOnly],
        tool_names: TOOL_NAMES
            .iter()
            .map(|value| tool_name(value))
            .collect::<Result<Vec<_>, _>>()?,
        revision: 1,
        created_by: owner_id.clone(),
        created_at: timestamp.clone(),
        updated_by: owner_id,
        updated_at: timestamp,
    };
    let response = ProjectAgentResponse {
        data: record,
        meta: meta(&request_id(&headers), Freshness::Fresh, 1),
    };
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(ProjectAgentError::Storage)?;
    sqlx::query(
        "INSERT INTO project_agents(
            project_agent_id, workspace_id, technical_project_id, display_name, state,
            capabilities_json, tool_names_json, revision, created_by, created_at,
            updated_by, updated_at
         ) VALUES (?, ?, ?, ?, 'active', ?, ?, 1, ?, ?, ?, ?)",
    )
    .bind(&agent_id)
    .bind(WORKSPACE_ID)
    .bind(&project_id)
    .bind(&response.data.display_name)
    .bind(capabilities)
    .bind(tools)
    .bind(&response.data.created_by)
    .bind(&response.data.created_at)
    .bind(&response.data.updated_by)
    .bind(&response.data.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(ProjectAgentError::Storage)?;
    store_mutation(&mut tx, &project_id, &key, &digest, &response).await?;
    events::publish_in_transaction(
        &mut tx,
        ChangeEventKind::ProjectAgentChanged,
        &format!("project-agent:{agent_id}"),
        1,
        json!({"technical_project_id": project_id, "state": "active", "read_only": true}),
    )
    .await
    .map_err(|error| ProjectAgentError::Storage(sqlx::Error::Protocol(error.to_string())))?;
    tx.commit().await.map_err(ProjectAgentError::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/technical-projects/{technical_project_id}/agent",
    tag = "project-agent",
    params(("technical_project_id" = String, Path)),
    responses((status = 200, body = ProjectAgentResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_project_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectAgentResponse>, ProjectAgentError> {
    let agent = load_agent_for_project(&state.pool, &project_id).await?;
    Ok(Json(ProjectAgentResponse {
        data: agent.clone(),
        meta: meta(&request_id(&headers), Freshness::Fresh, agent.revision),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/project-agents/{project_agent_id}/tools/{tool_name}",
    tag = "project-agent",
    params(("project_agent_id" = String, Path), ("tool_name" = String, Path)),
    request_body = ProjectAgentToolRequest,
    responses((status = 200, body = ProjectAgentToolResponse), (status = 400, body = ApiErrorResponse), (status = 404, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn invoke_tool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((agent_id, requested_tool)): Path<(String, String)>,
    payload: Result<Json<ProjectAgentToolRequest>, JsonRejection>,
) -> Result<Json<ProjectAgentToolResponse>, ProjectAgentError> {
    let Json(request) = payload.map_err(|_| ProjectAgentError::BadRequest {
        code: "INVALID_JSON",
        message: "请求正文不是符合契约的 JSON",
        details: json!({}),
    })?;
    let agent = load_agent(&state.pool, &agent_id).await?;
    if !matches!(agent.state, ProjectAgentState::Active) {
        return Err(ProjectAgentError::Conflict {
            code: "PROJECT_AGENT_DISABLED",
            message: "该项目 Agent 当前已停用",
            details: json!({"project_agent_id": agent_id}),
        });
    }
    let tool = tool_name(&requested_tool)?;
    if !agent.tool_names.contains(&tool)
        || !agent
            .capabilities
            .contains(&ProjectAgentCapability::ReadOnly)
    {
        return Err(ProjectAgentError::BadRequest {
            code: "PROJECT_AGENT_TOOL_NOT_ALLOWED",
            message: "该项目 Agent 当前未授予此只读工具",
            details: json!({"tool_name": requested_tool}),
        });
    }
    let limit = request.limit.unwrap_or(50);
    if limit == 0 || limit > MAX_TOOL_LIMIT {
        return Err(ProjectAgentError::BadRequest {
            code: "INVALID_TOOL_LIMIT",
            message: "工具 limit 必须在 1 到 200 之间",
            details: json!({"limit": limit}),
        });
    }
    let project = load_project(&state.pool, &agent.technical_project_id, true).await?;
    let mut target_id = request.project_target_id.clone();
    let (result, evidence_refs, freshness) = match tool {
        ProjectAgentToolName::ListProjectTargets => {
            if let Some(project_target_id) = target_id.as_deref() {
                return Err(ProjectAgentError::BadRequest {
                    code: "PROJECT_TARGET_NOT_APPLICABLE",
                    message: "list_project_targets 只接受项目作用域，不接受 project_target_id",
                    details: json!({"project_target_id": project_target_id}),
                });
            }
            let targets = load_targets(&state.pool, &project.technical_project_id).await?;
            (
                ProjectAgentToolResult::ProjectTargets(ProjectAgentTargetsData {
                    technical_project: project,
                    targets,
                }),
                Vec::new(),
                Freshness::Fresh,
            )
        }
        ProjectAgentToolName::ReadDeploymentObservation => {
            let id = target_id.clone().ok_or(ProjectAgentError::BadRequest {
                code: "PROJECT_TARGET_REQUIRED",
                message: "该工具必须显式提供 project_target_id",
                details: json!({"tool_name": tool_name_str(&tool)}),
            })?;
            let context =
                load_target_context(&state.pool, &agent.technical_project_id, &id).await?;
            let observations =
                deployment_observations(&state.pool, &context.deployment.deployment_id, limit)
                    .await?;
            let refs = observations
                .iter()
                .flat_map(|item| item.evidence_refs.clone())
                .collect::<Vec<_>>();
            let freshness = context.deployment.freshness.clone();
            (
                ProjectAgentToolResult::DeploymentObservation(
                    ProjectAgentDeploymentObservationData {
                        deployment: context.deployment,
                        observations,
                    },
                ),
                refs,
                freshness,
            )
        }
        ProjectAgentToolName::ReadHostCapabilities => {
            let id = target_id.clone().ok_or(ProjectAgentError::BadRequest {
                code: "PROJECT_TARGET_REQUIRED",
                message: "该工具必须显式提供 project_target_id",
                details: json!({"tool_name": tool_name_str(&tool)}),
            })?;
            let context =
                load_target_context(&state.pool, &agent.technical_project_id, &id).await?;
            let (data, refs) = host_capabilities(
                &state.pool,
                &id,
                &context.deployment.host_id,
                &context.deployment.external_id,
            )
            .await?;
            let freshness = match data.monitor_freshness {
                MonitorFreshness::Fresh => Freshness::Fresh,
                MonitorFreshness::Stale => Freshness::Stale,
                MonitorFreshness::Unknown if data.latest_discovery_run_id.is_some() => {
                    Freshness::Fresh
                }
                MonitorFreshness::Unknown => Freshness::Unavailable,
            };
            (
                ProjectAgentToolResult::HostCapabilities(data),
                refs,
                freshness,
            )
        }
        ProjectAgentToolName::ReadServiceStatus => {
            let id = target_id.clone().ok_or(ProjectAgentError::BadRequest {
                code: "PROJECT_TARGET_REQUIRED",
                message: "该工具必须显式提供 project_target_id",
                details: json!({"tool_name": tool_name_str(&tool)}),
            })?;
            let context =
                load_target_context(&state.pool, &agent.technical_project_id, &id).await?;
            let observation =
                deployment_observations(&state.pool, &context.deployment.deployment_id, 1)
                    .await?
                    .into_iter()
                    .next();
            let refs = observation
                .as_ref()
                .map(|item| item.evidence_refs.clone())
                .unwrap_or_default();
            let service_freshness = observation
                .as_ref()
                .map(|item| match item.observation_state {
                    DeploymentObservationState::Observed => context.deployment.freshness.clone(),
                    DeploymentObservationState::Missing => Freshness::Stale,
                    DeploymentObservationState::Unknown => Freshness::Unavailable,
                })
                .unwrap_or(Freshness::Unavailable);
            let data = ProjectAgentServiceStatusData {
                project_target_id: id,
                deployment: context.deployment,
                observation_state: observation
                    .as_ref()
                    .map(|item| item.observation_state.clone()),
                provider_status: observation
                    .as_ref()
                    .and_then(|item| item.provider_status.clone()),
                observed_at: observation
                    .as_ref()
                    .and_then(|item| item.observed_at.clone()),
                freshness: service_freshness,
                evidence_refs: refs.clone(),
                metadata: observation
                    .map(|item| item.metadata)
                    .unwrap_or_else(|| json!({})),
            };
            let freshness = data.freshness.clone();
            (ProjectAgentToolResult::ServiceStatus(data), refs, freshness)
        }
        ProjectAgentToolName::ReadRecentDiff => {
            let id = target_id.clone().ok_or(ProjectAgentError::BadRequest {
                code: "PROJECT_TARGET_REQUIRED",
                message: "该工具必须显式提供 project_target_id",
                details: json!({"tool_name": tool_name_str(&tool)}),
            })?;
            let context =
                load_target_context(&state.pool, &agent.technical_project_id, &id).await?;
            let (data, refs) = recent_diff(
                &state.pool,
                &id,
                &context.deployment.host_id,
                &context.deployment.external_id,
            )
            .await?;
            let freshness = if data.diff.is_some() {
                Freshness::Fresh
            } else {
                Freshness::Unavailable
            };
            (ProjectAgentToolResult::RecentDiff(data), refs, freshness)
        }
    };
    let observed_at = now();
    let call_id = Uuid::new_v4().to_string();
    let result_json = serde_json::to_string(&result).map_err(ProjectAgentError::Serialization)?;
    let request_json = serde_json::to_string(&request).map_err(ProjectAgentError::Serialization)?;
    let refs = dedupe_refs(evidence_refs);
    let response = ProjectAgentToolResponse {
        data: ProjectAgentToolCallData {
            tool_call_id: call_id.clone(),
            project_agent_id: agent.project_agent_id.clone(),
            technical_project_id: agent.technical_project_id.clone(),
            project_target_id: target_id.take(),
            tool_name: tool.clone(),
            result,
            evidence_refs: refs.clone(),
            observed_at: observed_at.clone(),
        },
        meta: meta(&request_id(&headers), freshness, agent.revision),
    };
    sqlx::query(
        "INSERT INTO project_agent_tool_calls(
            tool_call_id, project_agent_id, technical_project_id, project_target_id,
            tool_name, request_json, result_json, evidence_refs_json, observed_at, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&call_id)
    .bind(&agent.project_agent_id)
    .bind(&agent.technical_project_id)
    .bind(&response.data.project_target_id)
    .bind(tool_name_str(&tool))
    .bind(request_json)
    .bind(result_json)
    .bind(serde_json::to_string(&refs).map_err(ProjectAgentError::Serialization)?)
    .bind(&observed_at)
    .bind(&observed_at)
    .execute(&state.pool)
    .await
    .map_err(ProjectAgentError::Storage)?;
    Ok(Json(response))
}

fn dedupe_refs(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
