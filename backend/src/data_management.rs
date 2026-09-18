use std::{fs, path::Path, str::FromStr};

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, DataSourceDescriptor, DataSourceKind,
        DataSourceStatus, Freshness, GraphSnapshot, LayoutPosition,
    },
    monitoring_history::{
        DAY_QUERY_MAX_SPAN_SECONDS, DEFAULT_HISTORY_PAGE_LIMIT, HOUR_QUERY_MAX_SPAN_SECONDS,
        MAX_HISTORY_PAGE_LIMIT, RAW_QUERY_MAX_SPAN_SECONDS,
    },
    storage,
};

const WORKSPACE_ID: &str = "workspace-default";
const MAX_IDEMPOTENCY_KEY: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DataExport {
    pub schema_version: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub exported_at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DataExportResponse {
    pub data: DataExport,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DeletionReceipt {
    pub scope_kind: String,
    pub scope_id: String,
    pub backup_ref: String,
    pub backup_sha256: String,
    pub secret_cleanup: String,
    pub deleted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DeletionResponse {
    pub data: DeletionReceipt,
    pub meta: ApiMeta,
}

#[derive(Debug, Error)]
pub enum DataError {
    #[error("resource not found")]
    NotFound { resource: &'static str, id: String },
    #[error("request confirmation is invalid")]
    Confirmation,
    #[error("idempotency key is required")]
    IdempotencyKey,
    #[error("idempotency key was reused")]
    IdempotencyConflict,
    #[error("stored response is invalid")]
    StoredResponse,
    #[error("database operation failed")]
    Storage(#[source] sqlx::Error),
    #[error("backup operation failed")]
    Backup(#[source] anyhow::Error),
    #[error("backup file operation failed")]
    Io(#[source] std::io::Error),
    #[error("observation admission gate is closed")]
    ObservationGateClosed,
    #[error("projection payload is invalid")]
    InvalidProjection,
}

impl IntoResponse for DataError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details) = match self {
            Self::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的数据范围不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Confirmation => (
                StatusCode::BAD_REQUEST,
                "DELETE_CONFIRMATION_REQUIRED",
                "删除请求需要匹配当前范围的 X-Confirm-Delete",
                json!({}),
            ),
            Self::IdempotencyKey => (
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_KEY_REQUIRED",
                "删除请求需要 Idempotency-Key",
                json!({}),
            ),
            Self::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "IDEMPOTENCY_KEY_REUSED",
                "Idempotency-Key 已用于不同删除请求",
                json!({}),
            ),
            Self::StoredResponse | Self::InvalidProjection => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DATA_STATE_INVALID",
                "本地数据状态无法完成该操作",
                json!({}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "data management storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地数据存储暂不可用",
                    json!({}),
                )
            }
            Self::Backup(error) => {
                tracing::error!(%request_id, error = %error, "pre-delete backup failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "BACKUP_FAILED",
                    "删除前备份未完成，数据保持不变",
                    json!({}),
                )
            }
            Self::Io(error) => {
                tracing::error!(%request_id, error = %error, "backup file operation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "BACKUP_IO_FAILED",
                    "删除前备份文件未完成，数据保持不变",
                    json!({}),
                )
            }
            Self::ObservationGateClosed => (
                StatusCode::SERVICE_UNAVAILABLE,
                "OBSERVATION_GATE_CLOSED",
                "观察任务闸门已关闭，删除未开始",
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

#[utoipa::path(
    get,
    path = "/api/v1/exports/workspace",
    tag = "m4",
    responses((status = 200, body = DataExportResponse), (status = 401, body = ApiErrorResponse))
)]
pub async fn export_workspace(
    State(state): State<AppState>,
) -> Result<Json<DataExportResponse>, DataError> {
    let payload = workspace_payload(
        &state.pool,
        state.monitoring_rollup.settings().retention_enabled,
    )
    .await?;
    Ok(Json(export_response("workspace", WORKSPACE_ID, payload)))
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/export",
    tag = "m4",
    params(("host_id" = String, Path)),
    responses(
        (status = 200, body = DataExportResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 401, body = ApiErrorResponse)
    )
)]
pub async fn export_host(
    State(state): State<AppState>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<Json<DataExportResponse>, DataError> {
    let payload = host_payload(
        &state.pool,
        &host_id,
        state.monitoring_rollup.settings().retention_enabled,
    )
    .await?;
    Ok(Json(export_response("host", &host_id, payload)))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_id}/export",
    tag = "m4",
    params(("project_id" = String, Path)),
    responses(
        (status = 200, body = DataExportResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 401, body = ApiErrorResponse)
    )
)]
pub async fn export_project(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<DataExportResponse>, DataError> {
    let payload = project_payload(&state.pool, &project_id).await?;
    Ok(Json(export_response("project", &project_id, payload)))
}

#[utoipa::path(
    get,
    path = "/api/v1/technical-projects/{technical_project_id}/export",
    tag = "catalog",
    params(("technical_project_id" = String, Path)),
    responses(
        (status = 200, body = DataExportResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 401, body = ApiErrorResponse)
    )
)]
pub async fn export_technical_project(
    State(state): State<AppState>,
    AxumPath(technical_project_id): AxumPath<String>,
) -> Result<Json<DataExportResponse>, DataError> {
    let payload = technical_project_payload(&state.pool, &technical_project_id).await?;
    Ok(Json(export_response(
        "technical_project",
        &technical_project_id,
        payload,
    )))
}

#[utoipa::path(
    delete,
    path = "/api/v1/workspace",
    tag = "m4",
    params(
        ("Idempotency-Key" = String, Header),
        ("X-Confirm-Delete" = String, Header, description = "workspace:workspace-default")
    ),
    responses(
        (status = 200, body = DeletionResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 503, body = ApiErrorResponse)
    )
)]
pub async fn delete_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DeletionResponse>, DataError> {
    delete_scope(&state, &headers, "workspace", WORKSPACE_ID).await
}

#[utoipa::path(
    delete,
    path = "/api/v1/hosts/{host_id}",
    tag = "m4",
    params(
        ("host_id" = String, Path),
        ("Idempotency-Key" = String, Header),
        ("X-Confirm-Delete" = String, Header, description = "host:{host_id}")
    ),
    responses(
        (status = 200, body = DeletionResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 503, body = ApiErrorResponse)
    )
)]
pub async fn delete_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(host_id): AxumPath<String>,
) -> Result<Json<DeletionResponse>, DataError> {
    delete_scope(&state, &headers, "host", &host_id).await
}

#[utoipa::path(
    delete,
    path = "/api/v1/projects/{project_id}",
    tag = "m4",
    params(
        ("project_id" = String, Path),
        ("Idempotency-Key" = String, Header),
        ("X-Confirm-Delete" = String, Header, description = "project:{project_id}")
    ),
    responses(
        (status = 200, body = DeletionResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 503, body = ApiErrorResponse)
    )
)]
pub async fn delete_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<DeletionResponse>, DataError> {
    delete_scope(&state, &headers, "project", &project_id).await
}

async fn delete_scope(
    state: &AppState,
    headers: &HeaderMap,
    scope_kind: &str,
    scope_id: &str,
) -> Result<Json<DeletionResponse>, DataError> {
    confirm_delete(headers, scope_kind, scope_id)?;
    let idempotency_key = idempotency_key(headers)?;
    let request_hash = sha256(format!("delete:{scope_kind}:{scope_id}"));
    if let Some(response) = replay_mutation::<DeletionResponse>(
        &state.pool,
        scope_kind,
        scope_id,
        &idempotency_key,
        &request_hash,
    )
    .await?
    {
        return Ok(Json(response));
    }

    let credential_refs = match scope_kind {
        "workspace" => workspace_secret_refs(&state.pool).await?,
        "host" => vec![host_credential_ref(&state.pool, scope_id).await?],
        "project" => {
            ensure_project_exists(&state.pool, scope_id).await?;
            Vec::new()
        }
        _ => return Err(DataError::InvalidProjection),
    };
    if scope_kind == "workspace" {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE workspace_id = ?")
                .bind(scope_id)
                .fetch_one(&state.pool)
                .await
                .map_err(DataError::Storage)?;
        if exists == 0 {
            return Err(DataError::NotFound {
                resource: "workspace",
                id: scope_id.to_owned(),
            });
        }
    }

    // Stop new manual/discovery admissions before draining every observation
    // permit. Runs admitted earlier are already durable; their workers either
    // finish before the barrier is granted or wait behind it and are therefore
    // included as queued receipts in the backup. Scheduler claims acquire a
    // permit before inserting a run. Holding both fences through the cascade
    // leaves no write window between the verified snapshot and deletion.
    let observation_admission = state.observation_admission.write().await;
    let maximum = state.monitoring_scheduler.settings().max_concurrency;
    let observation_barrier = state
        .observation_gate()
        .acquire_many_owned(maximum)
        .await
        .map_err(|_| DataError::ObservationGateClosed)?;

    let backup = create_backup(state, scope_kind, scope_id).await?;
    let deleted_at = now();
    match scope_kind {
        "workspace" => delete_workspace_records(&state.pool, &deleted_at).await?,
        "host" => delete_host_records(&state.pool, scope_id).await?,
        "project" => {
            remove_project_from_snapshots(&state.pool, scope_id, &deleted_at).await?;
        }
        _ => return Err(DataError::InvalidProjection),
    }
    drop(observation_barrier);
    drop(observation_admission);

    let mut secret_cleanup = "not_applicable".to_owned();
    if !credential_refs.is_empty() {
        secret_cleanup = "completed".to_owned();
        for credential_ref in credential_refs {
            if let Err(error) = state.secrets.delete(&credential_ref).await {
                tracing::error!(credential_ref, error = %error, "deleted data but secret cleanup is pending");
                secret_cleanup = "pending".to_owned();
            }
        }
    }
    let response = DeletionResponse {
        data: DeletionReceipt {
            scope_kind: scope_kind.to_owned(),
            scope_id: scope_id.to_owned(),
            backup_ref: format!("backup://{}", backup.id),
            backup_sha256: backup.sha256,
            secret_cleanup,
            deleted_at,
        },
        meta: m4_meta(Uuid::new_v4().to_string(), 0),
    };
    store_mutation(
        &state.pool,
        scope_kind,
        scope_id,
        &idempotency_key,
        &request_hash,
        &response,
    )
    .await?;
    Ok(Json(response))
}

async fn workspace_payload(
    pool: &SqlitePool,
    runtime_retention_enabled: bool,
) -> Result<Value, DataError> {
    let workspace = fetch_one_json(
        pool,
        "SELECT json_object(
            'workspace_id', workspace_id, 'owner_id', owner_id, 'created_at', created_at
         ) AS item FROM workspaces WHERE workspace_id = ?",
        &[WORKSPACE_ID],
    )
    .await?
    .ok_or_else(|| DataError::NotFound {
        resource: "workspace",
        id: WORKSPACE_ID.to_owned(),
    })?;
    let content_manifest =
        monitoring_history_json_export_manifest(pool, None, runtime_retention_enabled).await?;
    Ok(json!({
        "_content_manifest": content_manifest,
        "workspace": workspace,
        "hosts": fetch_json_rows(pool, "SELECT json_object(
            'host_id', host_id, 'display_name', display_name, 'address', address,
            'port', port, 'ssh_user', ssh_user, 'credential_ref', credential_ref,
            'host_key_fingerprint', host_key_fingerprint, 'host_key_state', host_key_state,
            'transport', transport, 'os', os, 'status', status,
            'created_at', created_at, 'last_checked_at', last_checked_at
         ) AS item FROM hosts WHERE workspace_id = ? ORDER BY created_at", &[WORKSPACE_ID]).await?,
        "technical_projects": fetch_json_rows(pool, "SELECT json_object(
            'technical_project_id', technical_project_id, 'workspace_id', workspace_id,
            'display_name', display_name, 'summary', summary, 'state', state,
            'revision', revision, 'created_by', created_by, 'created_at', created_at,
            'updated_by', updated_by, 'updated_at', updated_at
         ) AS item FROM technical_projects WHERE workspace_id = ?
         ORDER BY updated_at, technical_project_id", &[WORKSPACE_ID]).await?,
        "project_agents": fetch_json_rows(pool, "SELECT json_object(
            'project_agent_id', project_agent_id, 'workspace_id', workspace_id,
            'technical_project_id', technical_project_id, 'display_name', display_name,
            'state', state, 'capabilities', json(capabilities_json),
            'tool_names', json(tool_names_json), 'revision', revision,
            'created_by', created_by, 'created_at', created_at,
            'updated_by', updated_by, 'updated_at', updated_at
         ) AS item FROM project_agents WHERE workspace_id = ?
         ORDER BY technical_project_id, project_agent_id", &[WORKSPACE_ID]).await?,
        "project_agent_tool_calls": fetch_json_rows(pool, "SELECT json_object(
            'tool_call_id', calls.tool_call_id, 'project_agent_id', calls.project_agent_id,
            'technical_project_id', calls.technical_project_id, 'project_target_id', calls.project_target_id,
            'tool_name', calls.tool_name, 'request', json(calls.request_json),
            'result', json(calls.result_json), 'evidence_refs', json(calls.evidence_refs_json),
            'observed_at', calls.observed_at, 'created_at', calls.created_at
         ) AS item FROM project_agent_tool_calls calls
         JOIN project_agents agents ON agents.project_agent_id = calls.project_agent_id
         JOIN technical_projects projects
           ON projects.technical_project_id = agents.technical_project_id
         WHERE agents.workspace_id = ? AND projects.workspace_id = ?
           AND calls.technical_project_id = agents.technical_project_id
         ORDER BY calls.created_at, calls.tool_call_id", &[WORKSPACE_ID, WORKSPACE_ID]).await?,
        "businesses": fetch_json_rows(pool, "SELECT json_object(
            'business_id', business_id, 'workspace_id', workspace_id,
            'display_name', display_name, 'summary', summary, 'state', state,
            'origin', origin, 'revision', revision, 'created_by', created_by,
            'created_at', created_at, 'updated_by', updated_by, 'updated_at', updated_at
         ) AS item FROM businesses WHERE workspace_id = ?
         ORDER BY updated_at, business_id", &[WORKSPACE_ID]).await?,
        "business_project_links": fetch_json_rows(pool, "SELECT json_object(
            'business_project_link_id', links.business_project_link_id,
            'business_id', links.business_id, 'technical_project_id', links.technical_project_id,
            'state', links.state, 'origin', links.origin, 'revision', links.revision,
            'confirmed_by', links.confirmed_by, 'confirmed_at', links.confirmed_at,
            'created_at', links.created_at, 'updated_at', links.updated_at
         ) AS item FROM business_project_links links
         JOIN businesses businesses ON businesses.business_id = links.business_id
         JOIN technical_projects projects
           ON projects.technical_project_id = links.technical_project_id
         WHERE businesses.workspace_id = ? AND projects.workspace_id = ?
         ORDER BY links.business_id, links.technical_project_id", &[WORKSPACE_ID, WORKSPACE_ID]).await?,
        "resource_entities": fetch_json_rows(pool, "SELECT json_object(
            'resource_entity_id', resource_entity_id, 'workspace_id', workspace_id,
            'resource_kind', resource_kind, 'source', source, 'external_id', external_id,
            'display_name', display_name, 'freshness', freshness,
            'metadata', json(metadata_json), 'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM resource_entities WHERE workspace_id = ?
         ORDER BY resource_entity_id", &[WORKSPACE_ID]).await?,
        "deployment_resource_links": fetch_json_rows(pool, "SELECT json_object(
            'deployment_resource_link_id', links.deployment_resource_link_id,
            'deployment_id', links.deployment_id, 'resource_entity_id', links.resource_entity_id,
            'relation_kind', links.relation_kind, 'state', links.state, 'origin', links.origin,
            'source_refs', json(links.source_refs_json), 'observed_at', links.observed_at,
            'confirmed_by', links.confirmed_by, 'confirmed_at', links.confirmed_at,
            'revision', links.revision, 'created_at', links.created_at, 'updated_at', links.updated_at
         ) AS item FROM deployment_resource_links links
         JOIN deployments deployments ON deployments.deployment_id = links.deployment_id
         JOIN resource_entities resources
           ON resources.resource_entity_id = links.resource_entity_id
         WHERE deployments.workspace_id = ? AND resources.workspace_id = ?
         ORDER BY links.deployment_id, links.resource_entity_id", &[WORKSPACE_ID, WORKSPACE_ID]).await?,
        "technical_project_resource_links": fetch_json_rows(pool, "SELECT json_object(
            'technical_project_resource_link_id', links.technical_project_resource_link_id,
            'technical_project_id', links.technical_project_id, 'resource_entity_id', links.resource_entity_id,
            'state', links.state, 'origin', links.origin, 'source_refs', json(links.source_refs_json),
            'revision', links.revision, 'confirmed_by', links.confirmed_by, 'confirmed_at', links.confirmed_at,
            'created_at', links.created_at, 'updated_at', links.updated_at
         ) AS item FROM technical_project_resource_links links
         JOIN technical_projects projects
           ON projects.technical_project_id = links.technical_project_id
         JOIN resource_entities resources
           ON resources.resource_entity_id = links.resource_entity_id
         WHERE projects.workspace_id = ? AND resources.workspace_id = ?
         ORDER BY links.technical_project_id, links.resource_entity_id", &[WORKSPACE_ID, WORKSPACE_ID]).await?,
        "deployments": fetch_json_rows(pool, "SELECT json_object(
            'deployment_id', deployment_id, 'workspace_id', workspace_id, 'host_id', host_id,
            'provider_kind', provider_kind, 'external_id', external_id,
            'identity_key', identity_key, 'display_name', display_name,
            'catalog_state', catalog_state, 'latest_observation_id', latest_observation_id,
            'last_observed_at', last_observed_at,
            'last_observed_at_epoch_ms', last_observed_at_epoch_ms,
            'freshness', freshness, 'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM deployments WHERE workspace_id = ?
         ORDER BY host_id, provider_kind, external_id", &[WORKSPACE_ID]).await?,
        "deployment_observations": fetch_json_rows(pool, "SELECT json_object(
            'deployment_observation_id', observations.deployment_observation_id, 'deployment_id', observations.deployment_id,
            'discovery_run_id', observations.discovery_run_id, 'provider_kind', observations.provider_kind,
            'external_id', observations.external_id, 'observation_state', observations.observation_state,
            'provider_status', observations.provider_status, 'observed_at', observations.observed_at,
            'observed_at_epoch_ms', observations.observed_at_epoch_ms,
            'evidence_refs', json(observations.evidence_refs_json), 'metadata', json(observations.metadata_json),
            'created_at', observations.created_at
         ) AS item FROM deployment_observations observations
         JOIN deployments deployments ON deployments.deployment_id = observations.deployment_id
         WHERE deployments.workspace_id = ?
         ORDER BY observations.created_at, observations.deployment_observation_id", &[WORKSPACE_ID]).await?,
        "project_targets": fetch_json_rows(pool, "SELECT json_object(
            'project_target_id', targets.project_target_id, 'technical_project_id', targets.technical_project_id,
            'deployment_id', targets.deployment_id, 'display_name', targets.display_name,
            'adapter_kind', targets.adapter_kind, 'capabilities', json(targets.capabilities_json),
            'approval_policy', targets.approval_policy, 'state', targets.state, 'revision', targets.revision,
            'confirmed_by', targets.confirmed_by, 'confirmed_at', targets.confirmed_at,
            'last_observed_at', targets.last_observed_at, 'created_at', targets.created_at,
            'updated_at', targets.updated_at
         ) AS item FROM project_targets targets
         JOIN technical_projects projects
           ON projects.technical_project_id = targets.technical_project_id
         JOIN deployments deployments ON deployments.deployment_id = targets.deployment_id
         WHERE projects.workspace_id = ? AND deployments.workspace_id = ?
         ORDER BY targets.technical_project_id, targets.project_target_id", &[WORKSPACE_ID, WORKSPACE_ID]).await?,
        "catalog_mutation_requests": fetch_json_rows(pool, "SELECT json_object(
            'catalog_request_id', catalog_request_id, 'resource_kind', resource_kind,
            'resource_id', resource_id, 'idempotency_key', idempotency_key,
            'request_sha256', request_sha256, 'response', json(response_json),
            'created_at', created_at
         ) AS item FROM catalog_mutation_requests WHERE workspace_id = ?
         ORDER BY created_at, catalog_request_id", &[WORKSPACE_ID]).await?,
        "discovery_runs": fetch_json_rows(pool, "SELECT json_object(
            'run_id', runs.run_id, 'host_id', runs.host_id, 'state', runs.state,
            'failure_code', runs.failure_code, 'failure_summary', runs.failure_summary,
            'submitted_at', runs.submitted_at, 'started_at', runs.started_at, 'finished_at', runs.finished_at,
            'evidence', CASE WHEN runs.evidence_json IS NULL THEN NULL ELSE json(runs.evidence_json) END,
            'evidence_sha256', runs.evidence_sha256, 'evidence_item_count', runs.evidence_item_count,
            'evidence_retention', runs.evidence_retention, 'draft_id', runs.draft_id, 'diff_id', runs.diff_id
         ) AS item FROM discovery_runs runs
         JOIN hosts hosts ON hosts.host_id = runs.host_id
         WHERE hosts.workspace_id = ? ORDER BY runs.submitted_at", &[WORKSPACE_ID]).await?,
        "monitor_runs": fetch_json_rows(pool, "SELECT json_object(
            'run_id', runs.run_id, 'host_id', runs.host_id, 'profile', runs.profile,
            'trigger_kind', runs.trigger_kind, 'state', runs.state,
            'schedule_id', runs.schedule_id, 'schedule_revision', runs.schedule_revision,
            'schedule_version_id', runs.schedule_version_id,
            'health_policy_version_id', runs.health_policy_version_id,
            'scheduled_for', runs.scheduled_for,
            'due_interval_seconds', runs.due_interval_seconds,
            'missed_due_count', runs.missed_due_count,
            'stale_after_seconds', runs.stale_after_seconds,
            'collector_version', runs.collector_version, 'boot_id', runs.boot_id,
            'coverage', json(runs.coverage_json), 'output_bytes', runs.output_bytes,
            'ssh_session_count', runs.ssh_session_count, 'failure_code', runs.failure_code,
            'failure_summary', runs.failure_summary, 'submitted_at', runs.submitted_at,
            'started_at', runs.started_at, 'finished_at', runs.finished_at
         ) AS item FROM monitor_runs runs
         JOIN hosts hosts ON hosts.host_id = runs.host_id
         WHERE hosts.workspace_id = ? ORDER BY runs.submitted_at", &[WORKSPACE_ID]).await?,
        "monitor_schedules": fetch_json_rows(pool, "SELECT json_object(
            'schedule_id', schedules.schedule_id, 'host_id', schedules.host_id, 'profile', schedules.profile,
            'interval_seconds', schedules.interval_seconds, 'jitter_seconds', schedules.jitter_seconds,
            'jitter_offset_seconds', schedules.jitter_offset_seconds,
            'stale_after_seconds', schedules.stale_after_seconds, 'state', schedules.state,
            'next_due_at', schedules.next_due_at, 'last_due_at', schedules.last_due_at,
            'revision', schedules.revision, 'created_at', schedules.created_at, 'updated_at', schedules.updated_at
         ) AS item FROM monitor_schedules schedules
         JOIN hosts hosts ON hosts.host_id = schedules.host_id
         WHERE hosts.workspace_id = ? ORDER BY schedules.created_at", &[WORKSPACE_ID]).await?,
        "monitor_schedule_provenance_metadata": fetch_json_rows(pool, "SELECT json_object(
            'singleton_id', singleton_id, 'provenance_started_at', provenance_started_at,
            'provenance_started_at_epoch_ms', provenance_started_at_epoch_ms,
            'created_at', created_at
         ) AS item FROM monitor_schedule_provenance_metadata WHERE singleton_id = 1", &[]).await?,
        "monitor_schedule_versions": fetch_json_rows(pool, "SELECT json_object(
            'schedule_version_id', versions.schedule_version_id, 'schedule_id', versions.schedule_id,
            'host_id', versions.host_id, 'revision', versions.revision, 'profile', versions.profile,
            'interval_seconds', versions.interval_seconds, 'jitter_seconds', versions.jitter_seconds,
            'jitter_offset_seconds', versions.jitter_offset_seconds,
            'stale_after_seconds', versions.stale_after_seconds, 'state', versions.state,
            'due_from_at', versions.due_from_at, 'due_from_at_epoch_ms', versions.due_from_at_epoch_ms,
            'due_until_at', versions.due_until_at, 'due_until_at_epoch_ms', versions.due_until_at_epoch_ms,
            'effective_from_at', versions.effective_from_at,
            'effective_from_at_epoch_ms', versions.effective_from_at_epoch_ms,
            'effective_until_at', versions.effective_until_at,
            'effective_until_at_epoch_ms', versions.effective_until_at_epoch_ms,
            'provenance_kind', versions.provenance_kind, 'activated_at', versions.activated_at,
            'paused_at', versions.paused_at, 'resumed_at', versions.resumed_at, 'archived_at', versions.archived_at,
            'created_at', versions.created_at
         ) AS item FROM monitor_schedule_versions versions
         JOIN monitor_schedules schedules ON schedules.schedule_id = versions.schedule_id
         JOIN hosts hosts ON hosts.host_id = schedules.host_id
         WHERE hosts.workspace_id = ?
         ORDER BY versions.schedule_id, versions.revision", &[WORKSPACE_ID]).await?,
        "health_policy_versions": fetch_json_rows(pool, "SELECT json_object(
            'policy_version_id', versions.policy_version_id, 'policy_id', versions.policy_id,
            'host_id', versions.host_id, 'revision', versions.revision, 'lifecycle_state', versions.lifecycle_state,
            'enabled', versions.enabled, 'source_kind', versions.source_kind,
            'policy', json(versions.policy_json), 'policy_sha256', versions.policy_sha256,
            'effective_from_at', versions.effective_from_at,
            'effective_from_at_epoch_ms', versions.effective_from_at_epoch_ms,
            'effective_until_at', versions.effective_until_at,
            'effective_until_at_epoch_ms', versions.effective_until_at_epoch_ms,
            'created_by', versions.created_by, 'created_at', versions.created_at
         ) AS item FROM health_policy_versions versions
         JOIN hosts hosts ON hosts.host_id = versions.host_id
         WHERE hosts.workspace_id = ? ORDER BY versions.host_id, versions.revision", &[WORKSPACE_ID]).await?,
        "health_evaluations": fetch_json_rows(pool, "SELECT json_object(
            'evaluation_id', evaluations.evaluation_id, 'run_id', evaluations.run_id, 'host_id', evaluations.host_id,
            'policy_version_id', evaluations.policy_version_id, 'policy_id', evaluations.policy_id,
            'policy_revision', evaluations.policy_revision, 'status', evaluations.status, 'reason_code', evaluations.reason_code,
            'required_condition_count', evaluations.required_condition_count,
            'optional_condition_count', evaluations.optional_condition_count, 'ok_count', evaluations.ok_count,
            'warning_count', evaluations.warning_count, 'critical_count', evaluations.critical_count,
            'unknown_count', evaluations.unknown_count, 'observation_state', evaluations.observation_state,
            'input_sha256', evaluations.input_sha256, 'evaluated_at', evaluations.evaluated_at,
            'evaluated_at_epoch_ms', evaluations.evaluated_at_epoch_ms, 'observed_at', evaluations.observed_at,
            'observed_at_epoch_ms', evaluations.observed_at_epoch_ms, 'valid_until', evaluations.valid_until,
            'valid_until_epoch_ms', evaluations.valid_until_epoch_ms, 'created_at', evaluations.created_at
         ) AS item FROM health_evaluations evaluations
         JOIN hosts hosts ON hosts.host_id = evaluations.host_id
         WHERE hosts.workspace_id = ?
         ORDER BY evaluations.evaluated_at, evaluations.evaluation_id", &[WORKSPACE_ID]).await?,
        "health_condition_evaluations": fetch_json_rows(pool, "SELECT json_object(
            'condition_evaluation_id', conditions.condition_evaluation_id,
            'evaluation_id', conditions.evaluation_id, 'condition_key', conditions.condition_key,
            'condition_kind', conditions.condition_kind, 'requirement', conditions.requirement,
            'subject_kind', conditions.subject_kind, 'subject_id', conditions.subject_id,
            'subject_label', conditions.subject_label, 'status', conditions.status,
            'candidate_status', conditions.candidate_status, 'reason_code', conditions.reason_code,
            'value_real', conditions.value_real, 'unit', conditions.unit, 'window_seconds', conditions.window_seconds,
            'streak_count', conditions.streak_count, 'streak_required', conditions.streak_required,
            'evidence_ref_count', json_array_length(conditions.evidence_refs_json),
            'evidence_refs', json('[]'), 'input_sha256', conditions.input_sha256,
            'created_at', conditions.created_at
         ) AS item FROM health_condition_evaluations conditions
         JOIN health_evaluations evaluations
           ON evaluations.evaluation_id = conditions.evaluation_id
         JOIN hosts hosts ON hosts.host_id = evaluations.host_id
         WHERE hosts.workspace_id = ?
         ORDER BY conditions.evaluation_id, conditions.condition_key", &[WORKSPACE_ID]).await?,
        "monitoring_current": fetch_json_rows(pool, "SELECT json_object(
            'host_id', current.host_id, 'run_id', current.run_id, 'profile', current.profile,
            'collector_version', current.collector_version, 'boot_id', current.boot_id,
            'snapshot', json(current.snapshot_json), 'coverage', json(current.coverage_json),
            'metric_count', current.metric_count, 'unknown_count', current.unknown_count,
            'observed_at', current.observed_at, 'valid_until', current.valid_until,
            'retention_tier', current.retention_tier, 'snapshot_sha256', current.snapshot_sha256,
            'updated_at', current.updated_at
         ) AS item FROM monitoring_current current
         JOIN hosts hosts ON hosts.host_id = current.host_id
         WHERE hosts.workspace_id = ? ORDER BY current.host_id", &[WORKSPACE_ID]).await?,
        "projection_drafts": fetch_json_rows(pool, "SELECT json_object(
            'draft_id', draft_id, 'host_id', host_id, 'discovery_run_id', discovery_run_id,
            'base_revision', base_revision, 'revision', revision, 'state', state,
            'pending_changes', pending_changes, 'snapshot', json(snapshot_json),
            'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM projection_drafts WHERE workspace_id = ? ORDER BY updated_at", &[WORKSPACE_ID]).await?,
        "projection_versions": fetch_json_rows(pool, "SELECT json_object(
            'version_id', version_id, 'draft_id', draft_id, 'host_id', host_id,
            'project_id', project_id, 'revision', revision, 'confirmed_by', confirmed_by,
            'confirmed_at', confirmed_at, 'snapshot', json(snapshot_json)
         ) AS item FROM projection_versions WHERE workspace_id = ? ORDER BY confirmed_at", &[WORKSPACE_ID]).await?,
        "ignore_rules": fetch_json_rows(pool, "SELECT json_object(
            'rule_id', rules.rule_id, 'host_id', rules.host_id, 'fingerprint', rules.fingerprint,
            'state', rules.state, 'created_at', rules.created_at, 'updated_at', rules.updated_at
         ) AS item FROM ignore_rules rules
         JOIN hosts hosts ON hosts.host_id = rules.host_id
         WHERE hosts.workspace_id = ? ORDER BY rules.created_at", &[WORKSPACE_ID]).await?,
        "model_provider": fetch_one_json(pool, "SELECT json_object(
            'base_url', base_url, 'model', model, 'credential_ref', credential_ref,
            'revision', revision, 'updated_at', updated_at
         ) AS item FROM model_provider_configs WHERE workspace_id = ?", &[WORKSPACE_ID]).await?,
        "onboarding_sessions": fetch_json_rows(pool, "SELECT json_object(
            'session_id', session_id, 'draft_id', draft_id, 'discovery_run_id', discovery_run_id,
            'state', state, 'facts_used', json(facts_used_json), 'warnings', json(warnings_json),
            'error_code', error_code, 'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM onboarding_sessions WHERE workspace_id = ? ORDER BY created_at", &[WORKSPACE_ID]).await?,
        "audit": fetch_json_rows(pool, "SELECT json_object(
            'audit_id', audit_id, 'actor_id', actor_id, 'kind', kind,
            'target_ref', target_ref, 'status_code', status_code,
            'request_id', request_id, 'summary', json(summary_json), 'occurred_at', occurred_at
         ) AS item FROM audit_events WHERE workspace_id = ? ORDER BY audit_id", &[WORKSPACE_ID]).await?,
    }))
}

async fn host_payload(
    pool: &SqlitePool,
    host_id: &str,
    runtime_retention_enabled: bool,
) -> Result<Value, DataError> {
    let host = fetch_one_json(
        pool,
        "SELECT json_object(
            'host_id', host_id, 'display_name', display_name, 'address', address,
            'port', port, 'ssh_user', ssh_user, 'credential_ref', credential_ref,
            'host_key_fingerprint', host_key_fingerprint, 'host_key_state', host_key_state,
            'transport', transport, 'os', os, 'status', status,
            'created_at', created_at, 'last_checked_at', last_checked_at
         ) AS item FROM hosts WHERE host_id = ?",
        &[host_id],
    )
    .await?
    .ok_or_else(|| DataError::NotFound {
        resource: "host",
        id: host_id.to_owned(),
    })?;
    let content_manifest =
        monitoring_history_json_export_manifest(pool, Some(host_id), runtime_retention_enabled)
            .await?;
    Ok(json!({
        "_content_manifest": content_manifest,
        "host": host,
        "connection_tests": fetch_json_rows(pool, "SELECT json_object(
            'test_id', test_id, 'state', state, 'candidate_fingerprint', candidate_fingerprint,
            'capabilities', json(capabilities_json), 'error_code', error_code,
            'error_summary', error_summary, 'started_at', started_at, 'finished_at', finished_at
         ) AS item FROM connection_tests WHERE host_id = ? ORDER BY started_at", &[host_id]).await?,
        "discovery_runs": fetch_json_rows(pool, "SELECT json_object(
            'run_id', run_id, 'state', state, 'failure_code', failure_code,
            'failure_summary', failure_summary, 'submitted_at', submitted_at,
            'started_at', started_at, 'finished_at', finished_at,
            'evidence', CASE WHEN evidence_json IS NULL THEN NULL ELSE json(evidence_json) END,
            'evidence_sha256', evidence_sha256, 'evidence_item_count', evidence_item_count,
            'evidence_retention', evidence_retention, 'draft_id', draft_id, 'diff_id', diff_id
         ) AS item FROM discovery_runs WHERE host_id = ? ORDER BY submitted_at", &[host_id]).await?,
        "deployments": fetch_json_rows(pool, "SELECT json_object(
            'deployment_id', deployment_id, 'workspace_id', workspace_id, 'host_id', host_id,
            'provider_kind', provider_kind, 'external_id', external_id,
            'identity_key', identity_key, 'display_name', display_name,
            'catalog_state', catalog_state, 'latest_observation_id', latest_observation_id,
            'last_observed_at', last_observed_at,
            'last_observed_at_epoch_ms', last_observed_at_epoch_ms,
            'freshness', freshness, 'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM deployments WHERE host_id = ?
         ORDER BY provider_kind, external_id", &[host_id]).await?,
        "deployment_observations": fetch_json_rows(pool, "SELECT json_object(
            'deployment_observation_id', observations.deployment_observation_id,
            'deployment_id', observations.deployment_id, 'discovery_run_id', observations.discovery_run_id,
            'provider_kind', observations.provider_kind, 'external_id', observations.external_id,
            'observation_state', observations.observation_state,
            'provider_status', observations.provider_status, 'observed_at', observations.observed_at,
            'observed_at_epoch_ms', observations.observed_at_epoch_ms,
            'evidence_refs', json(observations.evidence_refs_json),
            'metadata', json(observations.metadata_json), 'created_at', observations.created_at
         ) AS item FROM deployment_observations observations
         JOIN deployments ON deployments.deployment_id = observations.deployment_id
         WHERE deployments.host_id = ? ORDER BY observations.created_at", &[host_id]).await?,
        "project_targets": fetch_json_rows(pool, "SELECT json_object(
            'project_target_id', targets.project_target_id,
            'technical_project_id', targets.technical_project_id,
            'deployment_id', targets.deployment_id, 'display_name', targets.display_name,
            'adapter_kind', targets.adapter_kind, 'capabilities', json(targets.capabilities_json),
            'approval_policy', targets.approval_policy, 'state', targets.state,
            'revision', targets.revision, 'confirmed_by', targets.confirmed_by,
            'confirmed_at', targets.confirmed_at, 'last_observed_at', targets.last_observed_at,
            'created_at', targets.created_at, 'updated_at', targets.updated_at
         ) AS item FROM project_targets targets
         JOIN deployments ON deployments.deployment_id = targets.deployment_id
         WHERE deployments.host_id = ? ORDER BY targets.project_target_id", &[host_id]).await?,
        "resource_entities": fetch_json_rows(pool, "SELECT json_object(
            'resource_entity_id', resources.resource_entity_id,
            'workspace_id', resources.workspace_id, 'resource_kind', resources.resource_kind,
            'source', resources.source, 'external_id', resources.external_id,
            'display_name', resources.display_name, 'freshness', resources.freshness,
            'metadata', json(resources.metadata_json), 'created_at', resources.created_at,
            'updated_at', resources.updated_at
         ) AS item FROM (
         SELECT DISTINCT resources.resource_entity_id, resources.workspace_id,
             resources.resource_kind, resources.source, resources.external_id,
             resources.display_name, resources.freshness, resources.metadata_json,
             resources.created_at, resources.updated_at
         FROM resource_entities resources
         JOIN deployment_resource_links links ON links.resource_entity_id = resources.resource_entity_id
         JOIN deployments ON deployments.deployment_id = links.deployment_id
         WHERE deployments.host_id = ?
         ) resources ORDER BY resources.resource_entity_id", &[host_id]).await?,
        "deployment_resource_links": fetch_json_rows(pool, "SELECT json_object(
            'deployment_resource_link_id', links.deployment_resource_link_id,
            'deployment_id', links.deployment_id, 'resource_entity_id', links.resource_entity_id,
            'relation_kind', links.relation_kind, 'state', links.state, 'origin', links.origin,
            'source_refs', json(links.source_refs_json), 'observed_at', links.observed_at,
            'confirmed_by', links.confirmed_by, 'confirmed_at', links.confirmed_at,
            'revision', links.revision, 'created_at', links.created_at, 'updated_at', links.updated_at
         ) AS item FROM deployment_resource_links links
         JOIN deployments ON deployments.deployment_id = links.deployment_id
         WHERE deployments.host_id = ? ORDER BY links.deployment_id, links.resource_entity_id", &[host_id]).await?,
        "monitor_runs": fetch_json_rows(pool, "SELECT json_object(
            'run_id', run_id, 'profile', profile, 'trigger_kind', trigger_kind,
            'state', state, 'schedule_id', schedule_id,
            'schedule_revision', schedule_revision, 'scheduled_for', scheduled_for,
            'schedule_version_id', schedule_version_id,
            'health_policy_version_id', health_policy_version_id,
            'due_interval_seconds', due_interval_seconds,
            'missed_due_count', missed_due_count,
            'stale_after_seconds', stale_after_seconds,
            'collector_version', collector_version, 'boot_id', boot_id,
            'coverage', json(coverage_json), 'output_bytes', output_bytes,
            'ssh_session_count', ssh_session_count, 'failure_code', failure_code,
            'failure_summary', failure_summary, 'submitted_at', submitted_at,
            'started_at', started_at, 'finished_at', finished_at
         ) AS item FROM monitor_runs WHERE host_id = ? ORDER BY submitted_at", &[host_id]).await?,
        "monitor_schedules": fetch_json_rows(pool, "SELECT json_object(
            'schedule_id', schedule_id, 'host_id', host_id, 'profile', profile,
            'interval_seconds', interval_seconds, 'jitter_seconds', jitter_seconds,
            'jitter_offset_seconds', jitter_offset_seconds,
            'stale_after_seconds', stale_after_seconds, 'state', state,
            'next_due_at', next_due_at, 'last_due_at', last_due_at,
            'revision', revision, 'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM monitor_schedules WHERE host_id = ? ORDER BY created_at", &[host_id]).await?,
        "monitor_schedule_provenance_metadata": fetch_json_rows(pool, "SELECT json_object(
            'singleton_id', singleton_id, 'provenance_started_at', provenance_started_at,
            'provenance_started_at_epoch_ms', provenance_started_at_epoch_ms,
            'created_at', created_at
         ) AS item FROM monitor_schedule_provenance_metadata WHERE singleton_id = 1", &[]).await?,
        "monitor_schedule_versions": fetch_json_rows(pool, "SELECT json_object(
            'schedule_version_id', schedule_version_id, 'schedule_id', schedule_id,
            'host_id', host_id, 'revision', revision, 'profile', profile,
            'interval_seconds', interval_seconds, 'jitter_seconds', jitter_seconds,
            'jitter_offset_seconds', jitter_offset_seconds,
            'stale_after_seconds', stale_after_seconds, 'state', state,
            'due_from_at', due_from_at, 'due_from_at_epoch_ms', due_from_at_epoch_ms,
            'due_until_at', due_until_at, 'due_until_at_epoch_ms', due_until_at_epoch_ms,
            'effective_from_at', effective_from_at,
            'effective_from_at_epoch_ms', effective_from_at_epoch_ms,
            'effective_until_at', effective_until_at,
            'effective_until_at_epoch_ms', effective_until_at_epoch_ms,
            'provenance_kind', provenance_kind, 'activated_at', activated_at,
            'paused_at', paused_at, 'resumed_at', resumed_at, 'archived_at', archived_at,
            'created_at', created_at
         ) AS item FROM monitor_schedule_versions
         WHERE host_id = ? ORDER BY schedule_id, revision", &[host_id]).await?,
        "health_policy_versions": fetch_json_rows(pool, "SELECT json_object(
            'policy_version_id', policy_version_id, 'policy_id', policy_id,
            'host_id', host_id, 'revision', revision, 'lifecycle_state', lifecycle_state,
            'enabled', enabled, 'source_kind', source_kind,
            'policy', json(policy_json), 'policy_sha256', policy_sha256,
            'effective_from_at', effective_from_at,
            'effective_from_at_epoch_ms', effective_from_at_epoch_ms,
            'effective_until_at', effective_until_at,
            'effective_until_at_epoch_ms', effective_until_at_epoch_ms,
            'created_by', created_by, 'created_at', created_at
         ) AS item FROM health_policy_versions
         WHERE host_id = ? ORDER BY revision", &[host_id]).await?,
        "health_evaluations": fetch_json_rows(pool, "SELECT json_object(
            'evaluation_id', evaluation_id, 'run_id', run_id, 'host_id', host_id,
            'policy_version_id', policy_version_id, 'policy_id', policy_id,
            'policy_revision', policy_revision, 'status', status, 'reason_code', reason_code,
            'required_condition_count', required_condition_count,
            'optional_condition_count', optional_condition_count, 'ok_count', ok_count,
            'warning_count', warning_count, 'critical_count', critical_count,
            'unknown_count', unknown_count, 'observation_state', observation_state,
            'input_sha256', input_sha256, 'evaluated_at', evaluated_at,
            'evaluated_at_epoch_ms', evaluated_at_epoch_ms, 'observed_at', observed_at,
            'observed_at_epoch_ms', observed_at_epoch_ms, 'valid_until', valid_until,
            'valid_until_epoch_ms', valid_until_epoch_ms, 'created_at', created_at
         ) AS item FROM health_evaluations
         WHERE host_id = ? ORDER BY evaluated_at, evaluation_id", &[host_id]).await?,
        "health_condition_evaluations": fetch_json_rows(pool, "SELECT json_object(
            'condition_evaluation_id', condition_evaluation_id,
            'evaluation_id', evaluation_id, 'condition_key', condition_key,
            'condition_kind', condition_kind, 'requirement', requirement,
            'subject_kind', subject_kind, 'subject_id', subject_id,
            'subject_label', subject_label, 'status', status,
            'candidate_status', candidate_status, 'reason_code', reason_code,
            'value_real', value_real, 'unit', unit, 'window_seconds', window_seconds,
            'streak_count', streak_count, 'streak_required', streak_required,
            'evidence_ref_count', json_array_length(evidence_refs_json),
            'evidence_refs', json('[]'), 'input_sha256', input_sha256,
            'created_at', created_at
         ) AS item FROM health_condition_evaluations
         WHERE evaluation_id IN (
             SELECT evaluation_id FROM health_evaluations WHERE host_id = ?
         ) ORDER BY evaluation_id, condition_key", &[host_id]).await?,
        "monitoring_current": fetch_one_json(pool, "SELECT json_object(
            'run_id', run_id, 'profile', profile, 'collector_version', collector_version,
            'boot_id', boot_id, 'snapshot', json(snapshot_json),
            'coverage', json(coverage_json), 'metric_count', metric_count,
            'unknown_count', unknown_count, 'observed_at', observed_at,
            'valid_until', valid_until, 'retention_tier', retention_tier,
            'snapshot_sha256', snapshot_sha256, 'updated_at', updated_at
         ) AS item FROM monitoring_current WHERE host_id = ?", &[host_id]).await?,
        "projection_drafts": fetch_json_rows(pool, "SELECT json_object(
            'draft_id', draft_id, 'discovery_run_id', discovery_run_id,
            'base_revision', base_revision, 'revision', revision, 'state', state,
            'pending_changes', pending_changes, 'snapshot', json(snapshot_json),
            'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM projection_drafts WHERE host_id = ? ORDER BY updated_at", &[host_id]).await?,
        "projection_versions": fetch_json_rows(pool, "SELECT json_object(
            'version_id', version_id, 'draft_id', draft_id, 'revision', revision,
            'confirmed_at', confirmed_at, 'snapshot', json(snapshot_json)
         ) AS item FROM projection_versions WHERE host_id = ? ORDER BY confirmed_at", &[host_id]).await?,
        "ignore_rules": fetch_json_rows(pool, "SELECT json_object(
            'rule_id', rule_id, 'fingerprint', fingerprint, 'state', state,
            'created_at', created_at, 'updated_at', updated_at
         ) AS item FROM ignore_rules WHERE host_id = ? ORDER BY created_at", &[host_id]).await?,
    }))
}

async fn project_payload(pool: &SqlitePool, project_id: &str) -> Result<Value, DataError> {
    let rows = sqlx::query(
        "SELECT draft_id, host_id, revision, state, snapshot_json, updated_at
         FROM projection_drafts ORDER BY updated_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(DataError::Storage)?;
    let mut snapshots = Vec::new();
    for row in rows {
        let encoded: String = row.try_get("snapshot_json").map_err(DataError::Storage)?;
        let snapshot: GraphSnapshot =
            serde_json::from_str(&encoded).map_err(|_| DataError::InvalidProjection)?;
        if snapshot.nodes.iter().any(|node| node.id == project_id) {
            snapshots.push(json!({
                "draft_id": row.try_get::<String, _>("draft_id").map_err(DataError::Storage)?,
                "host_id": row.try_get::<Option<String>, _>("host_id").map_err(DataError::Storage)?,
                "revision": row.try_get::<i64, _>("revision").map_err(DataError::Storage)?,
                "state": row.try_get::<String, _>("state").map_err(DataError::Storage)?,
                "updated_at": row.try_get::<String, _>("updated_at").map_err(DataError::Storage)?,
                "snapshot": filter_project_snapshot(snapshot, project_id),
            }));
        }
    }
    if snapshots.is_empty() {
        return Err(DataError::NotFound {
            resource: "project",
            id: project_id.to_owned(),
        });
    }
    Ok(json!({"project_id": project_id, "projection_snapshots": snapshots}))
}

async fn technical_project_payload(
    pool: &SqlitePool,
    technical_project_id: &str,
) -> Result<Value, DataError> {
    let project = fetch_one_json(
        pool,
        "SELECT json_object(
            'technical_project_id', technical_project_id, 'workspace_id', workspace_id,
            'display_name', display_name, 'summary', summary, 'state', state,
            'revision', revision, 'created_by', created_by, 'created_at', created_at,
            'updated_by', updated_by, 'updated_at', updated_at
         ) AS item FROM technical_projects
         WHERE technical_project_id = ? AND workspace_id = ?",
        &[technical_project_id, WORKSPACE_ID],
    )
    .await?
    .ok_or_else(|| DataError::NotFound {
        resource: "technical_project",
        id: technical_project_id.to_owned(),
    })?;
    Ok(json!({
        "technical_project": project,
        "project_targets": fetch_json_rows(pool, "SELECT json_object(
            'project_target_id', targets.project_target_id,
            'technical_project_id', targets.technical_project_id,
            'deployment_id', targets.deployment_id, 'display_name', targets.display_name,
            'adapter_kind', targets.adapter_kind, 'capabilities', json(targets.capabilities_json),
            'approval_policy', targets.approval_policy, 'state', targets.state,
            'revision', targets.revision, 'confirmed_by', targets.confirmed_by,
            'confirmed_at', targets.confirmed_at, 'last_observed_at', targets.last_observed_at,
            'created_at', targets.created_at, 'updated_at', targets.updated_at
         ) AS item FROM project_targets targets
         WHERE targets.technical_project_id = ? ORDER BY targets.project_target_id", &[technical_project_id]).await?,
        "deployments": fetch_json_rows(pool, "SELECT json_object(
            'deployment_id', deployments.deployment_id, 'workspace_id', deployments.workspace_id,
            'host_id', deployments.host_id, 'provider_kind', deployments.provider_kind,
            'external_id', deployments.external_id, 'identity_key', deployments.identity_key,
            'display_name', deployments.display_name, 'catalog_state', deployments.catalog_state,
            'latest_observation_id', deployments.latest_observation_id,
            'last_observed_at', deployments.last_observed_at,
            'last_observed_at_epoch_ms', deployments.last_observed_at_epoch_ms,
            'freshness', deployments.freshness, 'created_at', deployments.created_at,
            'updated_at', deployments.updated_at
         ) AS item FROM deployments
         JOIN project_targets ON project_targets.deployment_id = deployments.deployment_id
         WHERE project_targets.technical_project_id = ?
         ORDER BY deployments.host_id, deployments.provider_kind, deployments.external_id", &[technical_project_id]).await?,
        "deployment_observations": fetch_json_rows(pool, "SELECT json_object(
            'deployment_observation_id', observations.deployment_observation_id,
            'deployment_id', observations.deployment_id, 'discovery_run_id', observations.discovery_run_id,
            'provider_kind', observations.provider_kind, 'external_id', observations.external_id,
            'observation_state', observations.observation_state,
            'provider_status', observations.provider_status, 'observed_at', observations.observed_at,
            'observed_at_epoch_ms', observations.observed_at_epoch_ms,
            'evidence_refs', json(observations.evidence_refs_json),
            'metadata', json(observations.metadata_json), 'created_at', observations.created_at
         ) AS item FROM deployment_observations observations
         JOIN project_targets ON project_targets.deployment_id = observations.deployment_id
         WHERE project_targets.technical_project_id = ?
         ORDER BY observations.created_at, observations.deployment_observation_id", &[technical_project_id]).await?,
        "business_project_links": fetch_json_rows(pool, "SELECT json_object(
            'business_project_link_id', links.business_project_link_id,
            'business_id', links.business_id, 'technical_project_id', links.technical_project_id,
            'state', links.state, 'origin', links.origin, 'revision', links.revision,
            'confirmed_by', links.confirmed_by, 'confirmed_at', links.confirmed_at,
            'created_at', links.created_at, 'updated_at', links.updated_at
         ) AS item FROM business_project_links links
         WHERE links.technical_project_id = ? ORDER BY links.business_id", &[technical_project_id]).await?,
        "project_agent": fetch_one_json(pool, "SELECT json_object(
            'project_agent_id', project_agent_id, 'workspace_id', workspace_id,
            'technical_project_id', technical_project_id, 'display_name', display_name,
            'state', state, 'capabilities', json(capabilities_json),
            'tool_names', json(tool_names_json), 'revision', revision,
            'created_by', created_by, 'created_at', created_at,
            'updated_by', updated_by, 'updated_at', updated_at
         ) AS item FROM project_agents
         WHERE technical_project_id = ? AND workspace_id = ?", &[technical_project_id, WORKSPACE_ID]).await?,
        "project_agent_tool_calls": fetch_json_rows(pool, "SELECT json_object(
            'tool_call_id', tool_call_id, 'project_agent_id', project_agent_id,
            'technical_project_id', technical_project_id, 'project_target_id', project_target_id,
            'tool_name', tool_name, 'request', json(request_json),
            'result', json(result_json), 'evidence_refs', json(evidence_refs_json),
            'observed_at', observed_at, 'created_at', created_at
         ) AS item FROM project_agent_tool_calls
         WHERE technical_project_id = ? ORDER BY created_at, tool_call_id", &[technical_project_id]).await?,
        "technical_project_resource_links": fetch_json_rows(pool, "SELECT json_object(
            'technical_project_resource_link_id', links.technical_project_resource_link_id,
            'technical_project_id', links.technical_project_id,
            'resource_entity_id', links.resource_entity_id, 'state', links.state,
            'origin', links.origin, 'source_refs', json(links.source_refs_json),
            'revision', links.revision, 'confirmed_by', links.confirmed_by,
            'confirmed_at', links.confirmed_at, 'created_at', links.created_at,
            'updated_at', links.updated_at
         ) AS item FROM technical_project_resource_links links
         WHERE links.technical_project_id = ? ORDER BY links.resource_entity_id", &[technical_project_id]).await?,
    }))
}

async fn delete_workspace_records(pool: &SqlitePool, deleted_at: &str) -> Result<(), DataError> {
    let mut tx = pool.begin().await.map_err(DataError::Storage)?;
    // H3c adds provenance and health rows that reference monitor runs and
    // policy/schedule revisions. Remove the HOST-scoped runs first so those
    // secondary rows are cascaded before the workspace/host cascade executes.
    sqlx::query(
        "DELETE FROM monitor_runs
         WHERE host_id IN (SELECT host_id FROM hosts WHERE workspace_id = ?)",
    )
    .bind(WORKSPACE_ID)
    .execute(&mut *tx)
    .await
    .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM workspaces WHERE workspace_id = ?")
        .bind(WORKSPACE_ID)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM secret_ref_descriptors")
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM projection_mutation_requests")
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM m3_mutation_requests")
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM m4_mutation_requests")
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    // Catalog mutation receipts are not foreign keys because they also cover
    // create-command idempotency. Clear them explicitly with the workspace.
    sqlx::query("DELETE FROM catalog_mutation_requests WHERE workspace_id = ?")
        .bind(WORKSPACE_ID)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM audit_events WHERE workspace_id = ?")
        .bind(WORKSPACE_ID)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("DELETE FROM change_events WHERE workspace_id = ?")
        .bind(WORKSPACE_ID)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    sqlx::query("UPDATE owner_sessions SET revoked_at = ? WHERE revoked_at IS NULL")
        .bind(deleted_at)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    tx.commit().await.map_err(DataError::Storage)
}

async fn delete_host_records(pool: &SqlitePool, host_id: &str) -> Result<(), DataError> {
    let mut tx = pool.begin().await.map_err(DataError::Storage)?;
    let draft_ids = sqlx::query("SELECT draft_id FROM projection_drafts WHERE host_id = ?")
        .bind(host_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(DataError::Storage)?
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("draft_id").ok())
        .collect::<Vec<_>>();
    for draft_id in draft_ids {
        sqlx::query("DELETE FROM projection_mutation_requests WHERE resource_id = ?")
            .bind(&draft_id)
            .execute(&mut *tx)
            .await
            .map_err(DataError::Storage)?;
        sqlx::query("DELETE FROM m3_mutation_requests WHERE resource_id = ?")
            .bind(&draft_id)
            .execute(&mut *tx)
            .await
            .map_err(DataError::Storage)?;
    }
    // Delete monitor runs explicitly before HOST so H3c evaluation rows and
    // their pinned schedule/policy references cannot block the parent delete
    // on SQLite foreign-key enforcement.
    sqlx::query("DELETE FROM monitor_runs WHERE host_id = ?")
        .bind(host_id)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;

    // Catalog mutation receipts are intentionally not foreign keys because
    // they also cover idempotency keys for create commands.  Remove receipts
    // for targets that disappear with this HOST so a replay cannot return a
    // response containing a deleted ProjectTarget.
    sqlx::query(
        "DELETE FROM catalog_mutation_requests
         WHERE resource_kind = 'project_target.update'
           AND resource_id IN (
               SELECT project_target_id FROM project_targets
               WHERE deployment_id IN (
                   SELECT deployment_id FROM deployments WHERE host_id = ?
               )
           )",
    )
    .bind(host_id)
    .execute(&mut *tx)
    .await
    .map_err(DataError::Storage)?;
    sqlx::query(
        "DELETE FROM catalog_mutation_requests
         WHERE resource_kind = 'project_target.create'
           AND json_extract(response_json, '$.data.deployment_id') IN (
               SELECT deployment_id FROM deployments WHERE host_id = ?
           )",
    )
    .bind(host_id)
    .execute(&mut *tx)
    .await
    .map_err(DataError::Storage)?;
    let deleted = sqlx::query("DELETE FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    if deleted.rows_affected() != 1 {
        return Err(DataError::NotFound {
            resource: "host",
            id: host_id.to_owned(),
        });
    }
    tx.commit().await.map_err(DataError::Storage)
}

async fn remove_project_from_snapshots(
    pool: &SqlitePool,
    project_id: &str,
    updated_at: &str,
) -> Result<(), DataError> {
    let mut tx = pool.begin().await.map_err(DataError::Storage)?;
    let rows = sqlx::query("SELECT draft_id, revision, snapshot_json FROM projection_drafts")
        .fetch_all(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    let mut changed = 0_u64;
    for row in rows {
        let draft_id: String = row.try_get("draft_id").map_err(DataError::Storage)?;
        let revision: i64 = row.try_get("revision").map_err(DataError::Storage)?;
        let encoded: String = row.try_get("snapshot_json").map_err(DataError::Storage)?;
        let mut snapshot: GraphSnapshot =
            serde_json::from_str(&encoded).map_err(|_| DataError::InvalidProjection)?;
        let removed = remove_project(&mut snapshot, project_id);
        if removed.is_empty() {
            continue;
        }
        changed += 1;
        snapshot.layout.revision = revision + 1;
        sqlx::query(
            "UPDATE projection_drafts
             SET revision = ?, pending_changes = pending_changes + 1, snapshot_json = ?, updated_at = ?
             WHERE draft_id = ?",
        )
        .bind(revision + 1)
        .bind(serde_json::to_string(&snapshot).map_err(|_| DataError::InvalidProjection)?)
        .bind(updated_at)
        .bind(&draft_id)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
        let layout_row =
            sqlx::query("SELECT positions_json FROM canvas_layouts WHERE layout_id = ?")
                .bind(&snapshot.layout.layout_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(DataError::Storage)?;
        if let Some(layout_row) = layout_row {
            let positions: String = layout_row
                .try_get("positions_json")
                .map_err(DataError::Storage)?;
            let mut positions: Vec<LayoutPosition> =
                serde_json::from_str(&positions).map_err(|_| DataError::InvalidProjection)?;
            positions.retain(|position| !removed.contains(&position.node_id));
            sqlx::query(
                "UPDATE canvas_layouts SET revision = ?, positions_json = ?, updated_at = ? WHERE layout_id = ?",
            )
            .bind(revision + 1)
            .bind(serde_json::to_string(&positions).map_err(|_| DataError::InvalidProjection)?)
            .bind(updated_at)
            .bind(&snapshot.layout.layout_id)
            .execute(&mut *tx)
            .await
            .map_err(DataError::Storage)?;
        }
    }
    let versions = sqlx::query("SELECT version_id, snapshot_json FROM projection_versions")
        .fetch_all(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    for row in versions {
        let version_id: String = row.try_get("version_id").map_err(DataError::Storage)?;
        let encoded: String = row.try_get("snapshot_json").map_err(DataError::Storage)?;
        let mut snapshot: GraphSnapshot =
            serde_json::from_str(&encoded).map_err(|_| DataError::InvalidProjection)?;
        if remove_project(&mut snapshot, project_id).is_empty() {
            continue;
        }
        sqlx::query("UPDATE projection_versions SET snapshot_json = ? WHERE version_id = ?")
            .bind(serde_json::to_string(&snapshot).map_err(|_| DataError::InvalidProjection)?)
            .bind(version_id)
            .execute(&mut *tx)
            .await
            .map_err(DataError::Storage)?;
    }
    sqlx::query("DELETE FROM projects WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(DataError::Storage)?;
    if changed == 0 {
        return Err(DataError::NotFound {
            resource: "project",
            id: project_id.to_owned(),
        });
    }
    tx.commit().await.map_err(DataError::Storage)
}

fn remove_project(snapshot: &mut GraphSnapshot, project_id: &str) -> Vec<String> {
    let removed = snapshot
        .nodes
        .iter()
        .filter(|node| node.id == project_id || node.project_id.as_deref() == Some(project_id))
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    snapshot.nodes.retain(|node| !removed.contains(&node.id));
    snapshot
        .edges
        .retain(|edge| !removed.contains(&edge.from) && !removed.contains(&edge.to));
    removed
}

fn filter_project_snapshot(mut snapshot: GraphSnapshot, project_id: &str) -> GraphSnapshot {
    let keep = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.id == project_id
                || node.project_id.as_deref() == Some(project_id)
                || snapshot.edges.iter().any(|edge| {
                    (edge.from == project_id && edge.to == node.id)
                        || (edge.to == project_id && edge.from == node.id)
                })
        })
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    snapshot.nodes.retain(|node| keep.contains(&node.id));
    snapshot
        .edges
        .retain(|edge| keep.contains(&edge.from) && keep.contains(&edge.to));
    snapshot.focus.id = project_id.to_owned();
    snapshot
}

async fn ensure_project_exists(pool: &SqlitePool, project_id: &str) -> Result<(), DataError> {
    project_payload(pool, project_id).await.map(|_| ())
}

async fn host_credential_ref(pool: &SqlitePool, host_id: &str) -> Result<String, DataError> {
    sqlx::query_scalar("SELECT credential_ref FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .fetch_optional(pool)
        .await
        .map_err(DataError::Storage)?
        .ok_or_else(|| DataError::NotFound {
            resource: "host",
            id: host_id.to_owned(),
        })
}

async fn workspace_secret_refs(pool: &SqlitePool) -> Result<Vec<String>, DataError> {
    sqlx::query_scalar("SELECT credential_ref FROM secret_ref_descriptors ORDER BY credential_ref")
        .fetch_all(pool)
        .await
        .map_err(DataError::Storage)
}

struct BackupInfo {
    id: String,
    sha256: String,
}

async fn create_backup(
    state: &AppState,
    scope_kind: &str,
    scope_id: &str,
) -> Result<BackupInfo, DataError> {
    let backup_id = Uuid::new_v4().to_string();
    let safe_scope_id = sha256(scope_id);
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let directory = state.data_root.join("backups").join(format!(
        "delete-{scope_kind}-{stamp}-{}-{}",
        &safe_scope_id[..12],
        &backup_id[..8]
    ));
    fs::create_dir_all(&directory).map_err(DataError::Io)?;
    set_private_directory(&directory).map_err(DataError::Io)?;
    let database = directory.join("network-atlas.db");
    storage::backup_database(&state.pool, &database)
        .await
        .map_err(DataError::Backup)?;
    let secret_source = state.secrets.root().to_path_buf();
    let secret_destination = directory.join("secrets");
    tokio::task::spawn_blocking(move || copy_directory(&secret_source, &secret_destination))
        .await
        .map_err(|error| DataError::Io(std::io::Error::other(error.to_string())))?
        .map_err(DataError::Io)?;
    let database_sha256 = hash_file(&database).map_err(DataError::Io)?;
    let database_contents = inspect_backup_database_contents(&database)
        .await
        .map_err(DataError::Backup)?;
    let manifest = json!({
        "schema_version": "network-atlas-backup.v1",
        "backup_id": backup_id,
        "scope_kind": scope_kind,
        "scope_id_sha256": sha256(scope_id),
        "database_sha256": database_sha256,
        "database_contents": database_contents,
        "created_at": now(),
        "contains_server_secrets": true,
    });
    let manifest_path = directory.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).map_err(|_| DataError::InvalidProjection)?,
    )
    .map_err(DataError::Io)?;
    set_private_file(&database).map_err(DataError::Io)?;
    set_private_file(&manifest_path).map_err(DataError::Io)?;
    sqlx::query(
        "INSERT INTO backup_records(
            backup_id, scope_kind, scope_id, storage_path, database_sha256, state, created_at
         ) VALUES (?, ?, ?, ?, ?, 'ready', ?)",
    )
    .bind(&backup_id)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(directory.to_string_lossy().as_ref())
    .bind(&database_sha256)
    .bind(now())
    .execute(&state.pool)
    .await
    .map_err(DataError::Storage)?;
    Ok(BackupInfo {
        id: backup_id,
        sha256: database_sha256,
    })
}

#[derive(Debug)]
struct MonitoringHistoryInventory {
    history_started_at: String,
    metric_sample_count: i64,
    hour_rollup_count: i64,
    day_rollup_count: i64,
    rollup_partition_count: i64,
    compaction_run_count: i64,
    retention_enabled: bool,
    maintenance_state: String,
}

async fn monitoring_history_inventory(
    pool: &SqlitePool,
    host_id: Option<&str>,
) -> Result<MonitoringHistoryInventory, sqlx::Error> {
    let metadata = sqlx::query(
        "SELECT history_started_at
         FROM monitoring_history_metadata WHERE singleton_id = 1",
    )
    .fetch_one(pool)
    .await?;
    let metric_sample_count = if let Some(host_id) = host_id {
        sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples WHERE host_id = ?")
            .bind(host_id)
            .fetch_one(pool)
            .await?
    } else {
        sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(pool)
            .await?
    };
    let (hour_rollup_count, day_rollup_count, rollup_partition_count) = if let Some(host_id) =
        host_id
    {
        (
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM metric_rollups
                     WHERE host_id = ? AND resolution = 'hour'",
            )
            .bind(host_id)
            .fetch_one(pool)
            .await?,
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM metric_rollups
                     WHERE host_id = ? AND resolution = 'day'",
            )
            .bind(host_id)
            .fetch_one(pool)
            .await?,
            sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollup_partitions WHERE host_id = ?")
                .bind(host_id)
                .fetch_one(pool)
                .await?,
        )
    } else {
        (
            sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups WHERE resolution = 'hour'")
                .fetch_one(pool)
                .await?,
            sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups WHERE resolution = 'day'")
                .fetch_one(pool)
                .await?,
            sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollup_partitions")
                .fetch_one(pool)
                .await?,
        )
    };
    let maintenance = sqlx::query(
        "SELECT retention_enabled, state FROM monitoring_history_maintenance
         WHERE singleton_id = 1",
    )
    .fetch_one(pool)
    .await?;
    let compaction_run_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM monitoring_compaction_runs")
            .fetch_one(pool)
            .await?;
    Ok(MonitoringHistoryInventory {
        history_started_at: metadata.try_get("history_started_at")?,
        metric_sample_count,
        hour_rollup_count,
        day_rollup_count,
        rollup_partition_count,
        compaction_run_count,
        retention_enabled: maintenance.try_get::<i64, _>("retention_enabled")? == 1,
        maintenance_state: maintenance.try_get("state")?,
    })
}

async fn monitoring_history_json_export_manifest(
    pool: &SqlitePool,
    host_id: Option<&str>,
    runtime_retention_enabled: bool,
) -> Result<Value, DataError> {
    let inventory = monitoring_history_inventory(pool, host_id)
        .await
        .map_err(DataError::Storage)?;
    Ok(json!({
        "schema_version": "network-atlas-export-content.v1",
        "monitoring_history": {
            "history_started_at": inventory.history_started_at,
            "raw_query_max_span_seconds": RAW_QUERY_MAX_SPAN_SECONDS,
            "hour_query_max_span_seconds": HOUR_QUERY_MAX_SPAN_SECONDS,
            "day_query_max_span_seconds": DAY_QUERY_MAX_SPAN_SECONDS,
            "retention_enforcement": {
                "mode": "implemented_opt_in",
                "automatic_cleanup": runtime_retention_enabled,
                "last_persisted_run_setting": inventory.retention_enabled,
                "periodic_compactor": "available",
                "default_cleanup_state": "disabled_until_capacity_validation"
            },
            "capabilities": {
                "raw_bounded_query": "available",
                "hour_resolution": "available",
                "day_resolution": "available",
                "rollup_export": "semantic_query_available"
            },
            "metric_samples": {
                "included": false,
                "selection": "excluded_by_default",
                "reason": "bounded_high_volume_dataset",
                "omitted_row_count": inventory.metric_sample_count,
                "omitted_row_count_semantics": "informational_at_manifest_generation",
                "retrieval": {
                    "mode": "partitioned_keyset_query",
                    "representation": "semantic_metric_points",
                    "snapshot_consistency": "not_provided",
                    "lossless": false,
                    "method": "GET",
                    "path_template": "/api/v1/hosts/{host_id}/metrics",
                    "scoped_host_id": host_id,
                    "workspace_host_ids_from": if host_id.is_none() {
                        Some("payload.hosts[].host_id")
                    } else {
                        None
                    },
                    "required_query_parameters": ["from", "to", "resolution", "subject_kind"],
                    "fixed_query_parameters": {"resolution": "raw"},
                    "subject_kind_partitions": [
                        "host", "cpu", "filesystem", "block_device", "interface", "process"
                    ],
                    "window": {
                        "range": "half_open_[from,to)",
                        "start": "monitoring_history.history_started_at",
                        "maximum_span_seconds": RAW_QUERY_MAX_SPAN_SECONDS,
                        "repeat_until": "caller_selected_fixed_cutoff"
                    },
                    "pagination": {
                        "default_limit": DEFAULT_HISTORY_PAGE_LIMIT,
                        "maximum_limit": MAX_HISTORY_PAGE_LIMIT,
                        "ordering": ["observed_at_epoch_ms", "sample_id"],
                        "continue_while": "data.has_more=true",
                        "cursor_response": "data.next_cursor",
                        "cursor_query_parameters": ["after_epoch_ms", "after_sample_id"],
                        "reset_cursor_for_each_window_and_subject_kind": true
                    },
                    "verification": {
                        "row_count_equality_supported": false,
                        "reason": "live_queries_are_not_a_fixed_database_snapshot"
                    },
                    "complete_export": false,
                    "complete_recovery_artifact": "verified_pre_delete_sqlite_backup"
                }
            },
            "metric_rollups": {
                "included": false,
                "selection": "excluded_by_default",
                "hour_omitted_row_count": inventory.hour_rollup_count,
                "day_omitted_row_count": inventory.day_rollup_count,
                "partition_omitted_row_count": inventory.rollup_partition_count,
                "omitted_row_count_semantics": "informational_at_manifest_generation",
                "retrieval": {
                    "mode": "partitioned_keyset_query",
                    "representation": "semantic_metric_rollup_points",
                    "snapshot_consistency": "not_provided",
                    "lossless": false,
                    "method": "GET",
                    "path_template": "/api/v1/hosts/{host_id}/metrics",
                    "resolution_values": ["hour", "day"],
                    "pagination": {
                        "ordering": ["bucket_start_epoch_ms", "rollup_id"],
                        "cursor_query_parameters": ["after_epoch_ms", "after_sample_id"]
                    },
                    "complete_export": false,
                    "complete_recovery_artifact": "verified_pre_delete_sqlite_backup"
                }
            },
            "maintenance_ledgers": {
                "included": false,
                "selection": "excluded_by_default",
                "omitted_row_count_semantics": "informational_at_manifest_generation",
                "tables": {
                    "monitoring_history_maintenance": {
                        "omitted_row_count": 1
                    },
                    "monitoring_compaction_runs": {
                        "omitted_row_count": inventory.compaction_run_count
                    }
                },
                "complete_export": false,
                "complete_recovery_artifact": "verified_pre_delete_sqlite_backup"
            }
        }
    }))
}

async fn inspect_backup_database_contents(database: &Path) -> anyhow::Result<Value> {
    let url = format!(
        "sqlite://{}?mode=ro",
        database.to_string_lossy().replace('\\', "/")
    );
    let options = SqliteConnectOptions::from_str(&url)?
        .read_only(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let inventory = monitoring_history_inventory(&pool, None).await;
    let catalog_counts = json!({
        "technical_projects": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM technical_projects",
        )
        .fetch_one(&pool)
        .await?,
        "deployments": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deployments")
            .fetch_one(&pool)
            .await?,
        "deployment_observations": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM deployment_observations",
        )
        .fetch_one(&pool)
        .await?,
        "project_targets": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM project_targets")
            .fetch_one(&pool)
            .await?,
        "catalog_mutation_requests": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM catalog_mutation_requests",
        )
        .fetch_one(&pool)
        .await?,
        "businesses": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM businesses")
            .fetch_one(&pool)
            .await?,
        "business_project_links": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM business_project_links",
        )
        .fetch_one(&pool)
        .await?,
        "resource_entities": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM resource_entities",
        )
        .fetch_one(&pool)
        .await?,
        "deployment_resource_links": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM deployment_resource_links",
        )
        .fetch_one(&pool)
        .await?,
        "technical_project_resource_links": sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM technical_project_resource_links",
        )
        .fetch_one(&pool)
        .await?,
    });
    pool.close().await;
    let inventory = inventory?;
    Ok(json!({
        "database_scope": "full_workspace_snapshot",
        "monitoring_history": {
            "included": true,
            "history_started_at": inventory.history_started_at,
            "raw_query_max_span_seconds": RAW_QUERY_MAX_SPAN_SECONDS,
            "hour_query_max_span_seconds": HOUR_QUERY_MAX_SPAN_SECONDS,
            "day_query_max_span_seconds": DAY_QUERY_MAX_SPAN_SECONDS,
            "monitoring_history_metadata": {
                "row_count": 1
            },
            "metric_samples": {
                "row_count": inventory.metric_sample_count
            },
            "metric_rollup_partitions": {
                "row_count": inventory.rollup_partition_count
            },
            "metric_rollups": {
                "hour_row_count": inventory.hour_rollup_count,
                "day_row_count": inventory.day_rollup_count
            },
            "monitoring_history_maintenance": {
                "row_count": 1,
                "state": inventory.maintenance_state,
                "retention_enabled": inventory.retention_enabled
            },
            "monitoring_compaction_runs": {
                "row_count": inventory.compaction_run_count
            }
        },
        "deployment_catalog": {
            "included": true,
            "lossless": true,
            "rows": catalog_counts,
            "complete_recovery_artifact": "verified_pre_delete_sqlite_backup"
        }
    }))
}

async fn replay_mutation<T: DeserializeOwned>(
    pool: &SqlitePool,
    resource_kind: &str,
    resource_id: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<T>, DataError> {
    let row = sqlx::query(
        "SELECT request_sha256, response_json FROM m4_mutation_requests
         WHERE resource_kind = ? AND resource_id = ? AND idempotency_key = ?",
    )
    .bind(resource_kind)
    .bind(resource_id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(DataError::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_hash: String = row.try_get("request_sha256").map_err(DataError::Storage)?;
    if stored_hash != request_hash {
        return Err(DataError::IdempotencyConflict);
    }
    let response: String = row.try_get("response_json").map_err(DataError::Storage)?;
    serde_json::from_str(&response)
        .map(Some)
        .map_err(|_| DataError::StoredResponse)
}

async fn store_mutation<T: Serialize>(
    pool: &SqlitePool,
    resource_kind: &str,
    resource_id: &str,
    key: &str,
    request_hash: &str,
    response: &T,
) -> Result<(), DataError> {
    sqlx::query(
        "INSERT INTO m4_mutation_requests(
            request_id, resource_kind, resource_id, idempotency_key,
            request_sha256, response_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(resource_kind)
    .bind(resource_id)
    .bind(key)
    .bind(request_hash)
    .bind(serde_json::to_string(response).map_err(|_| DataError::StoredResponse)?)
    .bind(now())
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(DataError::Storage)
}

async fn fetch_json_rows(
    pool: &SqlitePool,
    sql: &str,
    bindings: &[&str],
) -> Result<Vec<Value>, DataError> {
    let mut query = sqlx::query(sql);
    for binding in bindings {
        query = query.bind(*binding);
    }
    let rows = query.fetch_all(pool).await.map_err(DataError::Storage)?;
    rows.into_iter()
        .map(|row| {
            let encoded: String = row.try_get("item").map_err(DataError::Storage)?;
            serde_json::from_str(&encoded).map_err(|_| DataError::InvalidProjection)
        })
        .collect()
}

async fn fetch_one_json(
    pool: &SqlitePool,
    sql: &str,
    bindings: &[&str],
) -> Result<Option<Value>, DataError> {
    Ok(fetch_json_rows(pool, sql, bindings)
        .await?
        .into_iter()
        .next())
}

fn export_response(scope_kind: &str, scope_id: &str, payload: Value) -> DataExportResponse {
    DataExportResponse {
        data: DataExport {
            schema_version: "network-atlas-export.v1".to_owned(),
            scope_kind: scope_kind.to_owned(),
            scope_id: scope_id.to_owned(),
            exported_at: now(),
            payload,
        },
        meta: m4_meta(Uuid::new_v4().to_string(), 1),
    }
}

fn m4_meta(request_id: String, revision: i64) -> ApiMeta {
    ApiMeta {
        request_id,
        revision,
        generated_at: now(),
        freshness: Freshness::Fresh,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status: DataSourceStatus::Fresh,
            label: "本地数据管理".to_owned(),
        },
    }
}

fn confirm_delete(headers: &HeaderMap, scope_kind: &str, scope_id: &str) -> Result<(), DataError> {
    let expected = format!("{scope_kind}:{scope_id}");
    headers
        .get("x-confirm-delete")
        .and_then(|value| value.to_str().ok())
        .filter(|value| *value == expected)
        .map(|_| ())
        .ok_or(DataError::Confirmation)
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, DataError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_IDEMPOTENCY_KEY
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .ok_or(DataError::IdempotencyKey)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(destination)?;
    set_private_directory(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(entry.path(), &target)?;
            set_private_file(&target)?;
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = fs::read(path)?;
    Ok(sha256(bytes))
}

fn sha256(value: impl AsRef<[u8]>) -> String {
    Sha256::digest(value.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        CanvasLayout, GraphEdge, GraphFocus, GraphNode, GraphNodeKind, GraphPosition,
        GraphRelationKind, GraphScopeKind, ProjectionState,
    };

    #[test]
    fn project_filter_keeps_only_project_owned_and_directly_linked_nodes() {
        let node = |id: &str, project_id: Option<&str>| GraphNode {
            id: id.to_owned(),
            kind: if id == "project-a" {
                GraphNodeKind::Project
            } else {
                GraphNodeKind::Service
            },
            label: id.to_owned(),
            subtitle: None,
            state: ProjectionState::Confirmed,
            source_refs: vec!["evidence:test".to_owned()],
            observed_at: None,
            position: GraphPosition { x: 0.0, y: 0.0 },
            width: 100.0,
            height: 60.0,
            summary: None,
            facts: Vec::new(),
            project_id: project_id.map(str::to_owned),
            health: None,
        };
        let snapshot = GraphSnapshot {
            focus: GraphFocus {
                kind: GraphScopeKind::Global,
                id: WORKSPACE_ID.to_owned(),
            },
            nodes: vec![
                node("project-a", None),
                node("service-a", Some("project-a")),
                node("service-b", Some("project-b")),
            ],
            edges: vec![GraphEdge {
                id: "edge-a".to_owned(),
                from: "project-a".to_owned(),
                to: "service-a".to_owned(),
                kind: GraphRelationKind::Contains,
                label: "contains".to_owned(),
                state: ProjectionState::Confirmed,
                source_refs: vec!["evidence:test".to_owned()],
            }],
            layout: CanvasLayout {
                layout_id: "layout".to_owned(),
                scope: "global".to_owned(),
                revision: 1,
            },
        };
        let filtered = filter_project_snapshot(snapshot, "project-a");
        assert_eq!(filtered.nodes.len(), 2);
        assert_eq!(filtered.edges.len(), 1);
    }
}
