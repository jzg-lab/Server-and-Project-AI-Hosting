//! Read models and user-confirmed commands for the H4 deployment catalog.
//!
//! The discovery worker writes immutable observations in `crate::catalog`.
//! This module only exposes those observations and the small set of local
//! confirmation commands needed to create a TechnicalProject and bind a
//! read-only ProjectTarget.  It deliberately never invokes SSH or a provider.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, BusinessCreateRequest, BusinessListResponse,
        BusinessOrigin, BusinessProjectLinkCreateRequest, BusinessProjectLinkListData,
        BusinessProjectLinkListResponse, BusinessProjectLinkRecord, BusinessProjectLinkResponse,
        BusinessProjectLinkState, BusinessRecord, BusinessResponse, BusinessState,
        BusinessUpdateRequest, DataSourceDescriptor, DataSourceKind, DataSourceStatus,
        DeploymentCatalogState, DeploymentDetailData, DeploymentListData, DeploymentListResponse,
        DeploymentObservationRecord, DeploymentObservationState, DeploymentProviderKind,
        DeploymentRecord, DeploymentResponse, Freshness, GlobalResourceEdge,
        GlobalResourceEdgeKind, GlobalResourceFacet, GlobalResourceLens, GlobalResourceNode,
        GlobalResourceNodeKind, GlobalResourceSummary, GlobalResourceViewData,
        GlobalResourceViewResponse, ProjectTargetAdapterKind, ProjectTargetApprovalPolicy,
        ProjectTargetCapability, ProjectTargetCreateRequest, ProjectTargetListData,
        ProjectTargetListResponse, ProjectTargetRecord, ProjectTargetResponse, ProjectTargetState,
        ProjectTargetUpdateRequest, TechnicalProjectCreateRequest, TechnicalProjectListResponse,
        TechnicalProjectRecord, TechnicalProjectResponse, TechnicalProjectState,
        TechnicalProjectUpdateRequest,
    },
    events::{self, ChangeEventKind},
};

const WORKSPACE_ID: &str = "workspace-default";
const MAX_IDEMPOTENCY_KEY: usize = 200;
const MAX_REQUEST_ID: usize = 128;

#[derive(Debug, Error)]
pub enum CatalogApiError {
    #[error("invalid catalog request")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("catalog object was not found")]
    NotFound { resource: &'static str, id: String },
    #[error("catalog state conflicts with the request")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("If-Match is required")]
    PreconditionRequired,
    #[error("catalog storage failed")]
    Storage(#[source] sqlx::Error),
    #[error("stored catalog response is invalid")]
    StoredResponse,
    #[error("catalog response serialization failed")]
    Serialization(#[source] serde_json::Error),
}

impl IntoResponse for CatalogApiError {
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
                "请求的目录对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            Self::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "IF_MATCH_REQUIRED",
                "修改目录对象需要 If-Match 修订",
                json!({}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "catalog storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "CATALOG_STORAGE_UNAVAILABLE",
                    "项目目录暂不可用",
                    json!({}),
                )
            }
            Self::StoredResponse | Self::Serialization(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "CATALOG_STATE_INVALID",
                "项目目录状态无法完成该操作",
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

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeploymentListQuery {
    pub state: Option<String>,
    pub provider_kind: Option<String>,
    #[serde(default)]
    pub include_ignored: bool,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CandidateListQuery {
    pub host_id: Option<String>,
    pub provider_kind: Option<String>,
    pub state: Option<String>,
    #[serde(default)]
    pub include_ignored: bool,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TechnicalProjectListQuery {
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BusinessListQuery {
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GlobalResourceQuery {
    pub lens: Option<String>,
    pub focus_kind: Option<String>,
    pub focus_id: Option<String>,
    pub business_id: Option<String>,
    pub technical_project_id: Option<String>,
    pub host_id: Option<String>,
    pub provider_kind: Option<String>,
    pub state: Option<String>,
    pub freshness: Option<String>,
    #[serde(default)]
    pub include_candidates: bool,
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/deployments",
    tag = "catalog",
    params(("host_id" = String, Path), DeploymentListQuery),
    responses((status = 200, body = DeploymentListResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn list_host_deployments(
    State(state): State<AppState>,
    Path(host_id): Path<String>,
    Query(query): Query<DeploymentListQuery>,
) -> Result<Json<DeploymentListResponse>, CatalogApiError> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM hosts WHERE host_id = ? AND workspace_id = ?")
            .bind(&host_id)
            .bind(WORKSPACE_ID)
            .fetch_optional(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?;
    if exists.is_none() {
        return Err(CatalogApiError::NotFound {
            resource: "host",
            id: host_id,
        });
    }
    let deployments = load_deployments(
        &state.pool,
        Some(&host_id),
        query.state.as_deref(),
        query.provider_kind.as_deref(),
        query.include_ignored,
    )
    .await?;
    Ok(Json(DeploymentListResponse {
        data: DeploymentListData {
            host_id,
            deployments,
        },
        meta: real_meta(&request_id(&HeaderMap::new()), Freshness::Fresh, 1),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/deployments/{deployment_id}",
    tag = "catalog",
    params(("deployment_id" = String, Path)),
    responses((status = 200, body = DeploymentResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_deployment(
    State(state): State<AppState>,
    Path(deployment_id): Path<String>,
) -> Result<Json<DeploymentResponse>, CatalogApiError> {
    let deployment = load_deployment(&state.pool, &deployment_id).await?;
    let rows = sqlx::query(
        "SELECT deployment_observation_id, deployment_id, discovery_run_id, provider_kind,
                external_id, observation_state, provider_status, observed_at,
                observed_at_epoch_ms, evidence_refs_json, metadata_json, created_at
         FROM deployment_observations
         WHERE deployment_id = ?
         ORDER BY COALESCE(observed_at_epoch_ms, 0) DESC, created_at DESC
         LIMIT 200",
    )
    .bind(&deployment_id)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let observations = rows
        .iter()
        .map(observation_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let freshness = deployment.freshness.clone();
    Ok(Json(DeploymentResponse {
        data: DeploymentDetailData {
            deployment,
            observations,
        },
        meta: real_meta(&Uuid::new_v4().to_string(), freshness, 1),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/deployment-candidates",
    tag = "catalog",
    params(CandidateListQuery),
    responses((status = 200, body = DeploymentListResponse))
)]
pub async fn list_deployment_candidates(
    State(state): State<AppState>,
    Query(query): Query<CandidateListQuery>,
) -> Result<Json<DeploymentListResponse>, CatalogApiError> {
    let deployments = load_deployments(
        &state.pool,
        query.host_id.as_deref(),
        query.state.as_deref(),
        query.provider_kind.as_deref(),
        query.include_ignored,
    )
    .await?;
    Ok(Json(DeploymentListResponse {
        data: DeploymentListData {
            host_id: query.host_id.unwrap_or_else(|| "*".to_owned()),
            deployments,
        },
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, 1),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/technical-projects",
    tag = "catalog",
    params(TechnicalProjectListQuery),
    responses((status = 200, body = TechnicalProjectListResponse))
)]
pub async fn list_technical_projects(
    State(state): State<AppState>,
    Query(query): Query<TechnicalProjectListQuery>,
) -> Result<Json<TechnicalProjectListResponse>, CatalogApiError> {
    let rows = if let Some(state_filter) = query.state.as_deref() {
        validate_project_state(state_filter)?;
        sqlx::query(
            "SELECT technical_project_id, workspace_id, display_name, summary, state, revision,
                    created_by, created_at, updated_by, updated_at
             FROM technical_projects WHERE workspace_id = ? AND state = ?
             ORDER BY updated_at DESC, technical_project_id",
        )
        .bind(WORKSPACE_ID)
        .bind(state_filter)
        .fetch_all(&state.pool)
        .await
        .map_err(CatalogApiError::Storage)?
    } else {
        sqlx::query(
            "SELECT technical_project_id, workspace_id, display_name, summary, state, revision,
                    created_by, created_at, updated_by, updated_at
             FROM technical_projects WHERE workspace_id = ?
             ORDER BY updated_at DESC, technical_project_id",
        )
        .bind(WORKSPACE_ID)
        .fetch_all(&state.pool)
        .await
        .map_err(CatalogApiError::Storage)?
    };
    let projects = rows
        .iter()
        .map(technical_project_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let revision = projects.iter().map(|p| p.revision).max().unwrap_or(0);
    Ok(Json(TechnicalProjectListResponse {
        data: projects,
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, revision),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/technical-projects",
    tag = "catalog",
    request_body = TechnicalProjectCreateRequest,
    params(("Idempotency-Key" = String, Header)),
    responses((status = 201, body = TechnicalProjectResponse), (status = 400, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn create_technical_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TechnicalProjectCreateRequest>,
) -> Result<(StatusCode, Json<TechnicalProjectResponse>), CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let name = normalized_name(&request.display_name, 160)?;
    let summary = normalized_optional(&request.summary, 2000)?;
    let normalized = TechnicalProjectCreateRequest {
        display_name: name,
        summary,
    };
    let digest = digest_json(&normalized)?;
    let resource_id = format!("create:{key}");
    if let Some(response) = replay_mutation::<TechnicalProjectResponse>(
        &state.pool,
        "technical_project.create",
        &resource_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::CREATED, Json(response)));
    }
    let request_id = request_id(&headers);
    let now = now();
    let project_id = Uuid::new_v4().to_string();
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_optional(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?
            .ok_or_else(|| CatalogApiError::NotFound {
                resource: "workspace",
                id: WORKSPACE_ID.to_owned(),
            })?;
    let project = TechnicalProjectRecord {
        technical_project_id: project_id.clone(),
        workspace_id: WORKSPACE_ID.to_owned(),
        display_name: normalized.display_name,
        summary: normalized.summary,
        state: TechnicalProjectState::Active,
        revision: 1,
        created_by: owner_id.clone(),
        created_at: now.clone(),
        updated_by: owner_id,
        updated_at: now,
    };
    let response = TechnicalProjectResponse {
        data: project,
        meta: real_meta(&request_id, Freshness::Fresh, 1),
    };
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    sqlx::query(
        "INSERT INTO technical_projects(
            technical_project_id, workspace_id, display_name, summary, state, revision,
            created_by, created_at, updated_by, updated_at
         ) VALUES (?, ?, ?, ?, 'active', 1, ?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(WORKSPACE_ID)
    .bind(&response.data.display_name)
    .bind(&response.data.summary)
    .bind(&response.data.created_by)
    .bind(&response.data.created_at)
    .bind(&response.data.updated_by)
    .bind(&response.data.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    store_mutation_in(
        &mut tx,
        "technical_project.create",
        &resource_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(&mut tx, "technical-project", &project_id, 1).await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/technical-projects/{technical_project_id}",
    tag = "catalog",
    params(("technical_project_id" = String, Path)),
    responses((status = 200, body = TechnicalProjectResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_technical_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<TechnicalProjectResponse>, CatalogApiError> {
    let row = sqlx::query(
        "SELECT technical_project_id, workspace_id, display_name, summary, state, revision,
                created_by, created_at, updated_by, updated_at
         FROM technical_projects WHERE technical_project_id = ? AND workspace_id = ?",
    )
    .bind(&project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "technical_project",
        id: project_id,
    })?;
    let record = technical_project_from_row(&row)?;
    let revision = record.revision;
    Ok(Json(TechnicalProjectResponse {
        data: record,
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, revision),
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/technical-projects/{technical_project_id}",
    tag = "catalog",
    request_body = TechnicalProjectUpdateRequest,
    params(("technical_project_id" = String, Path), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)),
    responses((status = 200, body = TechnicalProjectResponse), (status = 409, body = ApiErrorResponse), (status = 428, body = ApiErrorResponse))
)]
pub async fn update_technical_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<TechnicalProjectUpdateRequest>,
) -> Result<Json<TechnicalProjectResponse>, CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let expected = if_match_revision(&headers)?;
    if request.base_revision.is_some_and(|value| value != expected) {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_MISMATCH",
            message: "请求正文与 If-Match 修订不一致",
            details: json!({"if_match": expected, "base_revision": request.base_revision}),
        });
    }
    let normalized = TechnicalProjectUpdateRequest {
        display_name: request
            .display_name
            .as_deref()
            .map(|v| normalized_name(v, 160))
            .transpose()?,
        summary: request
            .summary
            .as_deref()
            .map(|v| normalized_optional(&Some(v.to_owned()), 2000))
            .transpose()?
            .flatten(),
        state: request.state,
        base_revision: Some(expected),
    };
    if normalized.display_name.is_none()
        && normalized.summary.is_none()
        && normalized.state.is_none()
    {
        return Err(CatalogApiError::BadRequest {
            code: "EMPTY_PATCH",
            message: "至少需要一个项目字段",
            details: json!({}),
        });
    }
    let digest = digest_json(&normalized)?;
    if let Some(response) = replay_mutation::<TechnicalProjectResponse>(
        &state.pool,
        "technical_project.update",
        &project_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_one(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?;
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    let row = sqlx::query(
        "SELECT technical_project_id, workspace_id, display_name, summary, state, revision,
                created_by, created_at, updated_by, updated_at
         FROM technical_projects WHERE technical_project_id = ? AND workspace_id = ?",
    )
    .bind(&project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "technical_project",
        id: project_id.clone(),
    })?;
    let current = technical_project_from_row(&row)?;
    if current.revision != expected {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_CONFLICT",
            message: "项目已被其他修改更新",
            details: json!({"current_revision": current.revision, "if_match": expected}),
        });
    }
    let next_revision = current.revision + 1;
    let current_state_name = project_state_name_from_record(&current);
    let next_name = normalized.display_name.unwrap_or(current.display_name);
    let next_summary = normalized.summary.or(current.summary);
    let next_state = normalized
        .state
        .as_ref()
        .map(project_state_name)
        .unwrap_or(current_state_name);
    let updated_at = now();
    sqlx::query(
        "UPDATE technical_projects SET display_name = ?, summary = ?, state = ?, revision = ?,
                updated_by = ?, updated_at = ?
         WHERE technical_project_id = ? AND revision = ?",
    )
    .bind(&next_name)
    .bind(&next_summary)
    .bind(next_state)
    .bind(next_revision)
    .bind(&owner_id)
    .bind(&updated_at)
    .bind(&project_id)
    .bind(expected)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let updated = TechnicalProjectRecord {
        technical_project_id: current.technical_project_id,
        workspace_id: current.workspace_id,
        display_name: next_name,
        summary: next_summary,
        state: normalized.state.unwrap_or(current.state),
        revision: next_revision,
        created_by: current.created_by,
        created_at: current.created_at,
        updated_by: owner_id,
        updated_at,
    };
    let response = TechnicalProjectResponse {
        data: updated,
        meta: real_meta(&request_id(&headers), Freshness::Fresh, next_revision),
    };
    store_mutation_in(
        &mut tx,
        "technical_project.update",
        &project_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(&mut tx, "technical-project", &project_id, next_revision).await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/api/v1/technical-projects/{technical_project_id}/targets",
    tag = "catalog",
    params(("technical_project_id" = String, Path)),
    responses((status = 200, body = ProjectTargetListResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn list_project_targets(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectTargetListResponse>, CatalogApiError> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM technical_projects WHERE technical_project_id = ? AND workspace_id = ?",
    )
    .bind(&project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    if exists.is_none() {
        return Err(CatalogApiError::NotFound {
            resource: "technical_project",
            id: project_id,
        });
    }
    let rows = sqlx::query(
        "SELECT targets.project_target_id, targets.technical_project_id, targets.deployment_id,
                targets.display_name, targets.adapter_kind, targets.capabilities_json,
                targets.approval_policy, targets.state, targets.revision, targets.confirmed_by,
                targets.confirmed_at, targets.last_observed_at, targets.created_at, targets.updated_at
         FROM project_targets targets
         JOIN technical_projects projects ON projects.technical_project_id = targets.technical_project_id
         WHERE targets.technical_project_id = ? AND projects.workspace_id = ?
         ORDER BY targets.updated_at DESC, targets.project_target_id",
    )
    .bind(&project_id)
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let targets = rows
        .iter()
        .map(target_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let revision = targets.iter().map(|t| t.revision).max().unwrap_or(0);
    Ok(Json(ProjectTargetListResponse {
        data: ProjectTargetListData {
            technical_project_id: project_id,
            targets,
        },
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, revision),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/project-targets",
    tag = "catalog",
    request_body = ProjectTargetCreateRequest,
    params(("Idempotency-Key" = String, Header)),
    responses((status = 201, body = ProjectTargetResponse), (status = 400, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn create_project_target(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProjectTargetCreateRequest>,
) -> Result<(StatusCode, Json<ProjectTargetResponse>), CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let normalized = ProjectTargetCreateRequest {
        technical_project_id: request.technical_project_id.trim().to_owned(),
        deployment_id: request.deployment_id.trim().to_owned(),
        display_name: request
            .display_name
            .as_deref()
            .map(|v| normalized_name(v, 160))
            .transpose()?,
    };
    if normalized.technical_project_id.is_empty() || normalized.deployment_id.is_empty() {
        return Err(CatalogApiError::BadRequest {
            code: "TARGET_REFERENCE_REQUIRED",
            message: "TechnicalProject 和 Deployment 引用不能为空",
            details: json!({}),
        });
    }
    let digest = digest_json(&normalized)?;
    let resource_id = format!("create:{key}");
    if let Some(response) = replay_mutation::<ProjectTargetResponse>(
        &state.pool,
        "project_target.create",
        &resource_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::CREATED, Json(response)));
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_one(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?;
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    let project_exists: Option<String> = sqlx::query_scalar(
        "SELECT technical_project_id FROM technical_projects
         WHERE technical_project_id = ? AND workspace_id = ? AND state = 'active'",
    )
    .bind(&normalized.technical_project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    if project_exists.is_none() {
        return Err(CatalogApiError::Conflict {
            code: "TECHNICAL_PROJECT_NOT_ACTIVE",
            message: "只能绑定到当前工作区的活动 TechnicalProject",
            details: json!({"technical_project_id": normalized.technical_project_id}),
        });
    }
    let deployment_row = sqlx::query(
        "SELECT deployment_id, freshness, last_observed_at
         FROM deployments WHERE deployment_id = ? AND workspace_id = ?",
    )
    .bind(&normalized.deployment_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let deployment_row = deployment_row.ok_or_else(|| CatalogApiError::NotFound {
        resource: "deployment",
        id: normalized.deployment_id.clone(),
    })?;
    let deployment_freshness = parse_freshness(
        &deployment_row
            .try_get::<String, _>("freshness")
            .map_err(CatalogApiError::Storage)?,
    )?;
    let deployment_last_observed_at: Option<String> = deployment_row
        .try_get("last_observed_at")
        .map_err(CatalogApiError::Storage)?;
    let duplicate: Option<String> = sqlx::query_scalar(
        "SELECT project_target_id FROM project_targets
         WHERE technical_project_id = ? AND deployment_id = ? AND state <> 'archived'",
    )
    .bind(&normalized.technical_project_id)
    .bind(&normalized.deployment_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    if let Some(existing) = duplicate {
        return Err(CatalogApiError::Conflict {
            code: "PROJECT_TARGET_EXISTS",
            message: "该项目已经绑定此 Deployment",
            details: json!({"project_target_id": existing}),
        });
    }
    let target_id = Uuid::new_v4().to_string();
    let timestamp = now();
    let target_state = match &deployment_freshness {
        Freshness::Fresh => ProjectTargetState::Confirmed,
        Freshness::Stale | Freshness::Unavailable => ProjectTargetState::Stale,
    };
    let record = ProjectTargetRecord {
        project_target_id: target_id.clone(),
        technical_project_id: normalized.technical_project_id.clone(),
        deployment_id: normalized.deployment_id.clone(),
        display_name: normalized.display_name,
        adapter_kind: ProjectTargetAdapterKind::ReadOnly,
        capabilities: vec![ProjectTargetCapability::ReadOnly],
        approval_policy: ProjectTargetApprovalPolicy::ReadOnly,
        state: target_state,
        revision: 1,
        confirmed_by: owner_id.clone(),
        confirmed_at: timestamp.clone(),
        last_observed_at: deployment_last_observed_at,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    };
    sqlx::query(
        "INSERT INTO project_targets(
            project_target_id, technical_project_id, deployment_id, display_name,
            adapter_kind, capabilities_json, approval_policy, state, revision,
            confirmed_by, confirmed_at, last_observed_at, created_at, updated_at
         ) VALUES (?, ?, ?, ?, 'read_only', '[\"read_only\"]', 'read_only', ?, 1, ?, ?, ?, ?, ?)",
    )
    .bind(&target_id)
    .bind(&record.technical_project_id)
    .bind(&record.deployment_id)
    .bind(&record.display_name)
    .bind(target_state_name(&record.state))
    .bind(&record.confirmed_by)
    .bind(&record.confirmed_at)
    .bind(&record.last_observed_at)
    .bind(&record.created_at)
    .bind(&record.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let response = ProjectTargetResponse {
        data: record,
        meta: real_meta(&request_id(&headers), deployment_freshness, 1),
    };
    store_mutation_in(
        &mut tx,
        "project_target.create",
        &resource_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(&mut tx, "project-target", &target_id, 1).await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/project-targets/{project_target_id}",
    tag = "catalog",
    request_body = ProjectTargetUpdateRequest,
    params(("project_target_id" = String, Path), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)),
    responses((status = 200, body = ProjectTargetResponse), (status = 409, body = ApiErrorResponse), (status = 428, body = ApiErrorResponse))
)]
pub async fn update_project_target(
    State(state): State<AppState>,
    Path(target_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ProjectTargetUpdateRequest>,
) -> Result<Json<ProjectTargetResponse>, CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let expected = if_match_revision(&headers)?;
    if request.base_revision.is_some_and(|value| value != expected) {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_MISMATCH",
            message: "请求正文与 If-Match 修订不一致",
            details: json!({"if_match": expected, "base_revision": request.base_revision}),
        });
    }
    let normalized = ProjectTargetUpdateRequest {
        display_name: request
            .display_name
            .as_deref()
            .map(|v| normalized_name(v, 160))
            .transpose()?,
        state: request.state,
        base_revision: Some(expected),
    };
    if normalized.display_name.is_none() && normalized.state.is_none() {
        return Err(CatalogApiError::BadRequest {
            code: "EMPTY_PATCH",
            message: "至少需要一个目标字段",
            details: json!({}),
        });
    }
    let digest = digest_json(&normalized)?;
    if let Some(response) = replay_mutation::<ProjectTargetResponse>(
        &state.pool,
        "project_target.update",
        &target_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    let row = sqlx::query(
        "SELECT project_target_id, technical_project_id, deployment_id, display_name,
                adapter_kind, capabilities_json, approval_policy, state, revision, confirmed_by,
                confirmed_at, last_observed_at, created_at, updated_at
         FROM project_targets
         JOIN technical_projects ON technical_projects.technical_project_id = project_targets.technical_project_id
         WHERE project_targets.project_target_id = ? AND technical_projects.workspace_id = ?",
    )
    .bind(&target_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "project_target",
        id: target_id.clone(),
    })?;
    let current = target_from_row(&row)?;
    if current.revision != expected {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_CONFLICT",
            message: "项目目标已被其他修改更新",
            details: json!({"current_revision": current.revision, "if_match": expected}),
        });
    }
    let next_revision = current.revision + 1;
    let next_name = normalized.display_name.or(current.display_name);
    let next_state = normalized.state.unwrap_or(current.state);
    let updated_at = now();
    sqlx::query(
        "UPDATE project_targets SET display_name = ?, state = ?, revision = ?, updated_at = ?
         WHERE project_target_id = ? AND revision = ?",
    )
    .bind(&next_name)
    .bind(target_state_name(&next_state))
    .bind(next_revision)
    .bind(&updated_at)
    .bind(&target_id)
    .bind(expected)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let updated = ProjectTargetRecord {
        project_target_id: current.project_target_id,
        technical_project_id: current.technical_project_id,
        deployment_id: current.deployment_id,
        display_name: next_name,
        adapter_kind: current.adapter_kind,
        capabilities: current.capabilities,
        approval_policy: current.approval_policy,
        state: next_state,
        revision: next_revision,
        confirmed_by: current.confirmed_by,
        confirmed_at: current.confirmed_at,
        last_observed_at: current.last_observed_at,
        created_at: current.created_at,
        updated_at,
    };
    let response = ProjectTargetResponse {
        data: updated,
        meta: real_meta(&request_id(&headers), Freshness::Fresh, next_revision),
    };
    store_mutation_in(
        &mut tx,
        "project_target.update",
        &target_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(&mut tx, "project-target", &target_id, next_revision).await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/api/v1/businesses",
    tag = "catalog",
    params(BusinessListQuery),
    responses((status = 200, body = BusinessListResponse))
)]
pub async fn list_businesses(
    State(state): State<AppState>,
    Query(query): Query<BusinessListQuery>,
) -> Result<Json<BusinessListResponse>, CatalogApiError> {
    if let Some(filter) = query.state.as_deref() {
        validate_business_state(filter)?;
    }
    let rows = if let Some(filter) = query.state.as_deref() {
        sqlx::query(
            "SELECT business_id, workspace_id, display_name, summary, state, origin,
                    revision, created_by, created_at, updated_by, updated_at
             FROM businesses WHERE workspace_id = ? AND state = ?
             ORDER BY updated_at DESC, business_id",
        )
        .bind(WORKSPACE_ID)
        .bind(filter)
        .fetch_all(&state.pool)
        .await
        .map_err(CatalogApiError::Storage)?
    } else {
        sqlx::query(
            "SELECT business_id, workspace_id, display_name, summary, state, origin,
                    revision, created_by, created_at, updated_by, updated_at
             FROM businesses WHERE workspace_id = ?
             ORDER BY updated_at DESC, business_id",
        )
        .bind(WORKSPACE_ID)
        .fetch_all(&state.pool)
        .await
        .map_err(CatalogApiError::Storage)?
    };
    let businesses = rows
        .iter()
        .map(business_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let revision = businesses.iter().map(|b| b.revision).max().unwrap_or(0);
    Ok(Json(BusinessListResponse {
        data: businesses,
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, revision),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/businesses",
    tag = "catalog",
    request_body = BusinessCreateRequest,
    params(("Idempotency-Key" = String, Header)),
    responses((status = 201, body = BusinessResponse), (status = 400, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn create_business(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<BusinessCreateRequest>,
) -> Result<(StatusCode, Json<BusinessResponse>), CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let normalized = BusinessCreateRequest {
        display_name: normalized_name(&request.display_name, 120)?,
        summary: normalized_optional(&request.summary, 500)?,
        initial_project_ids: normalized_project_ids(&request.initial_project_ids)?,
    };
    let digest = digest_json(&normalized)?;
    let resource_id = format!("create:{key}");
    if let Some(response) = replay_mutation::<BusinessResponse>(
        &state.pool,
        "business.create",
        &resource_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::CREATED, Json(response)));
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_optional(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?
            .ok_or_else(|| CatalogApiError::NotFound {
                resource: "workspace",
                id: WORKSPACE_ID.to_owned(),
            })?;
    let timestamp = now();
    let business_id = Uuid::new_v4().to_string();
    let record = BusinessRecord {
        business_id: business_id.clone(),
        workspace_id: WORKSPACE_ID.to_owned(),
        display_name: normalized.display_name,
        summary: normalized.summary,
        state: BusinessState::Active,
        origin: BusinessOrigin::UserDeclared,
        revision: 1,
        created_by: owner_id.clone(),
        created_at: timestamp.clone(),
        updated_by: owner_id.clone(),
        updated_at: timestamp.clone(),
    };
    let response = BusinessResponse {
        data: record,
        meta: real_meta(&request_id(&headers), Freshness::Fresh, 1),
    };
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    for project_id in &normalized.initial_project_ids {
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT technical_project_id FROM technical_projects
             WHERE technical_project_id = ? AND workspace_id = ? AND state = 'active'",
        )
        .bind(project_id)
        .bind(WORKSPACE_ID)
        .fetch_optional(&mut *tx)
        .await
        .map_err(CatalogApiError::Storage)?;
        if exists.is_none() {
            return Err(CatalogApiError::Conflict {
                code: "TECHNICAL_PROJECT_NOT_ACTIVE",
                message: "只能关联当前工作区的活动 TechnicalProject",
                details: json!({"technical_project_id": project_id}),
            });
        }
    }
    sqlx::query(
        "INSERT INTO businesses(
            business_id, workspace_id, display_name, summary, state, origin, revision,
            created_by, created_at, updated_by, updated_at
         ) VALUES (?, ?, ?, ?, 'active', 'user_declared', 1, ?, ?, ?, ?)",
    )
    .bind(&business_id)
    .bind(WORKSPACE_ID)
    .bind(&response.data.display_name)
    .bind(&response.data.summary)
    .bind(&response.data.created_by)
    .bind(&response.data.created_at)
    .bind(&response.data.updated_by)
    .bind(&response.data.updated_at)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    for project_id in &normalized.initial_project_ids {
        insert_business_project_link(&mut tx, &business_id, project_id, &owner_id, &timestamp)
            .await?;
    }
    store_mutation_in(
        &mut tx,
        "business.create",
        &resource_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(&mut tx, "business", &business_id, 1).await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/businesses/{business_id}",
    tag = "catalog",
    params(("business_id" = String, Path)),
    responses((status = 200, body = BusinessResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_business(
    State(state): State<AppState>,
    Path(business_id): Path<String>,
) -> Result<Json<BusinessResponse>, CatalogApiError> {
    let record = load_business(&state.pool, &business_id).await?;
    let revision = record.revision;
    Ok(Json(BusinessResponse {
        data: record,
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, revision),
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/businesses/{business_id}",
    tag = "catalog",
    request_body = BusinessUpdateRequest,
    params(("business_id" = String, Path), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)),
    responses((status = 200, body = BusinessResponse), (status = 409, body = ApiErrorResponse), (status = 428, body = ApiErrorResponse))
)]
pub async fn update_business(
    State(state): State<AppState>,
    Path(business_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<BusinessUpdateRequest>,
) -> Result<Json<BusinessResponse>, CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let expected = if_match_revision(&headers)?;
    if request.base_revision.is_some_and(|value| value != expected) {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_MISMATCH",
            message: "请求正文与 If-Match 修订不一致",
            details: json!({"if_match": expected, "base_revision": request.base_revision}),
        });
    }
    let normalized = BusinessUpdateRequest {
        display_name: request
            .display_name
            .as_deref()
            .map(|value| normalized_name(value, 120))
            .transpose()?,
        summary: request
            .summary
            .as_deref()
            .map(|value| normalized_optional(&Some(value.to_owned()), 500))
            .transpose()?
            .flatten(),
        state: request.state,
        base_revision: Some(expected),
    };
    if normalized.display_name.is_none()
        && normalized.summary.is_none()
        && normalized.state.is_none()
    {
        return Err(CatalogApiError::BadRequest {
            code: "EMPTY_PATCH",
            message: "至少需要一个业务字段",
            details: json!({}),
        });
    }
    let digest = digest_json(&normalized)?;
    if let Some(response) = replay_mutation::<BusinessResponse>(
        &state.pool,
        "business.update",
        &business_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_one(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?;
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    let row = sqlx::query(
        "SELECT business_id, workspace_id, display_name, summary, state, origin,
                revision, created_by, created_at, updated_by, updated_at
         FROM businesses WHERE business_id = ? AND workspace_id = ?",
    )
    .bind(&business_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "business",
        id: business_id.clone(),
    })?;
    let current = business_from_row(&row)?;
    if current.revision != expected {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_CONFLICT",
            message: "业务已被其他修改更新",
            details: json!({"current_revision": current.revision, "if_match": expected}),
        });
    }
    let next_revision = current.revision + 1;
    let next_name = normalized
        .display_name
        .unwrap_or(current.display_name.clone());
    let next_summary = normalized.summary.or(current.summary.clone());
    let next_state = normalized.state.clone().unwrap_or(current.state.clone());
    let updated_at = now();
    sqlx::query(
        "UPDATE businesses SET display_name = ?, summary = ?, state = ?, revision = ?,
                updated_by = ?, updated_at = ?
         WHERE business_id = ? AND revision = ?",
    )
    .bind(&next_name)
    .bind(&next_summary)
    .bind(business_state_name(&next_state))
    .bind(next_revision)
    .bind(&owner_id)
    .bind(&updated_at)
    .bind(&business_id)
    .bind(expected)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let updated = BusinessRecord {
        business_id: current.business_id,
        workspace_id: current.workspace_id,
        display_name: next_name,
        summary: next_summary,
        state: next_state,
        origin: current.origin,
        revision: next_revision,
        created_by: current.created_by,
        created_at: current.created_at,
        updated_by: owner_id,
        updated_at,
    };
    let response = BusinessResponse {
        data: updated,
        meta: real_meta(&request_id(&headers), Freshness::Fresh, next_revision),
    };
    store_mutation_in(
        &mut tx,
        "business.update",
        &business_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(&mut tx, "business", &business_id, next_revision).await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/api/v1/businesses/{business_id}/project-links",
    tag = "catalog",
    params(("business_id" = String, Path)),
    responses((status = 200, body = BusinessProjectLinkListResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn list_business_project_links(
    State(state): State<AppState>,
    Path(business_id): Path<String>,
) -> Result<Json<BusinessProjectLinkListResponse>, CatalogApiError> {
    let _business = load_business(&state.pool, &business_id).await?;
    let rows = sqlx::query(
        "SELECT business_project_link_id, business_id, technical_project_id, state, origin,
                revision, confirmed_by, confirmed_at, created_at, updated_at
         FROM business_project_links
         WHERE business_id = ? ORDER BY updated_at DESC, business_project_link_id",
    )
    .bind(&business_id)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let links = rows
        .iter()
        .map(business_project_link_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let revision = links.iter().map(|link| link.revision).max().unwrap_or(0);
    Ok(Json(BusinessProjectLinkListResponse {
        data: BusinessProjectLinkListData { business_id, links },
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, revision),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/businesses/{business_id}/project-links",
    tag = "catalog",
    request_body = BusinessProjectLinkCreateRequest,
    params(("business_id" = String, Path), ("Idempotency-Key" = String, Header)),
    responses((status = 201, body = BusinessProjectLinkResponse), (status = 400, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse))
)]
pub async fn create_business_project_link(
    State(state): State<AppState>,
    Path(business_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<BusinessProjectLinkCreateRequest>,
) -> Result<(StatusCode, Json<BusinessProjectLinkResponse>), CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let project_id = request.technical_project_id.trim().to_owned();
    if project_id.is_empty() {
        return Err(CatalogApiError::BadRequest {
            code: "TECHNICAL_PROJECT_REQUIRED",
            message: "TechnicalProject 引用不能为空",
            details: json!({}),
        });
    }
    let normalized = BusinessProjectLinkCreateRequest {
        technical_project_id: project_id,
    };
    let digest = digest_json(&normalized)?;
    let resource_id = format!("{business_id}:{}", normalized.technical_project_id);
    if let Some(response) = replay_mutation::<BusinessProjectLinkResponse>(
        &state.pool,
        "business_project_link.create",
        &resource_id,
        &key,
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::CREATED, Json(response)));
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_one(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?;
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    let business_state: Option<String> = sqlx::query_scalar(
        "SELECT state FROM businesses WHERE business_id = ? AND workspace_id = ?",
    )
    .bind(&business_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    match business_state.as_deref() {
        None => {
            return Err(CatalogApiError::NotFound {
                resource: "business",
                id: business_id,
            });
        }
        Some("archived") => {
            return Err(CatalogApiError::Conflict {
                code: "BUSINESS_NOT_ACTIVE",
                message: "只能向活动 Business 添加项目",
                details: json!({}),
            });
        }
        Some("active") => {}
        _ => return Err(CatalogApiError::StoredResponse),
    }
    let project_exists: Option<String> = sqlx::query_scalar(
        "SELECT technical_project_id FROM technical_projects
         WHERE technical_project_id = ? AND workspace_id = ? AND state = 'active'",
    )
    .bind(&normalized.technical_project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    if project_exists.is_none() {
        return Err(CatalogApiError::Conflict {
            code: "TECHNICAL_PROJECT_NOT_ACTIVE",
            message: "只能关联当前工作区的活动 TechnicalProject",
            details: json!({"technical_project_id": normalized.technical_project_id}),
        });
    }
    let existing = sqlx::query(
        "SELECT business_project_link_id, business_id, technical_project_id, state, origin,
                revision, confirmed_by, confirmed_at, created_at, updated_at
         FROM business_project_links WHERE business_id = ? AND technical_project_id = ?",
    )
    .bind(&business_id)
    .bind(&normalized.technical_project_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let timestamp = now();
    let link = if let Some(row) = existing {
        let current = business_project_link_from_row(&row)?;
        if current.state == BusinessProjectLinkState::Confirmed {
            return Err(CatalogApiError::Conflict {
                code: "BUSINESS_PROJECT_LINK_EXISTS",
                message: "该 Business 已关联此 TechnicalProject",
                details: json!({"business_project_link_id": current.business_project_link_id}),
            });
        }
        let revision = current.revision + 1;
        sqlx::query(
            "UPDATE business_project_links SET state = 'confirmed', revision = ?,
                    confirmed_by = ?, confirmed_at = ?, updated_at = ?
             WHERE business_project_link_id = ? AND revision = ?",
        )
        .bind(revision)
        .bind(&owner_id)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(&current.business_project_link_id)
        .bind(current.revision)
        .execute(&mut *tx)
        .await
        .map_err(CatalogApiError::Storage)?;
        BusinessProjectLinkRecord {
            business_project_link_id: current.business_project_link_id,
            business_id: current.business_id,
            technical_project_id: current.technical_project_id,
            state: BusinessProjectLinkState::Confirmed,
            origin: current.origin,
            revision,
            confirmed_by: owner_id.clone(),
            confirmed_at: timestamp.clone(),
            created_at: current.created_at,
            updated_at: timestamp.clone(),
        }
    } else {
        let link_id = Uuid::new_v4().to_string();
        insert_business_project_link(
            &mut tx,
            &business_id,
            &normalized.technical_project_id,
            &owner_id,
            &timestamp,
        )
        .await?;
        BusinessProjectLinkRecord {
            business_project_link_id: link_id,
            business_id: business_id.clone(),
            technical_project_id: normalized.technical_project_id.clone(),
            state: BusinessProjectLinkState::Confirmed,
            origin: BusinessOrigin::UserDeclared,
            revision: 1,
            confirmed_by: owner_id.clone(),
            confirmed_at: timestamp.clone(),
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
        }
    };
    // The insert helper generates the ID itself.  Re-read it to avoid having
    // two identities in the response when this is a new link.
    let link = if link.business_project_link_id.is_empty() {
        link
    } else {
        let row = sqlx::query(
            "SELECT business_project_link_id, business_id, technical_project_id, state, origin,
                    revision, confirmed_by, confirmed_at, created_at, updated_at
             FROM business_project_links WHERE business_id = ? AND technical_project_id = ?",
        )
        .bind(&business_id)
        .bind(&normalized.technical_project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(CatalogApiError::Storage)?;
        business_project_link_from_row(&row)?
    };
    let response = BusinessProjectLinkResponse {
        data: link.clone(),
        meta: real_meta(&request_id(&headers), Freshness::Fresh, link.revision),
    };
    store_mutation_in(
        &mut tx,
        "business_project_link.create",
        &resource_id,
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(
        &mut tx,
        "business-project-link",
        &link.business_project_link_id,
        link.revision,
    )
    .await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/businesses/{business_id}/project-links/{technical_project_id}",
    tag = "catalog",
    params(("business_id" = String, Path), ("technical_project_id" = String, Path), ("If-Match" = String, Header), ("Idempotency-Key" = String, Header)),
    responses((status = 200, body = BusinessProjectLinkResponse), (status = 404, body = ApiErrorResponse), (status = 409, body = ApiErrorResponse), (status = 428, body = ApiErrorResponse))
)]
pub async fn delete_business_project_link(
    State(state): State<AppState>,
    Path((business_id, technical_project_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<BusinessProjectLinkResponse>, CatalogApiError> {
    let key = idempotency_key(&headers)?;
    let expected = if_match_revision(&headers)?;
    let digest = digest_json(&(business_id.clone(), technical_project_id.clone(), expected))?;
    if let Some(response) = replay_mutation::<BusinessProjectLinkResponse>(
        &state.pool,
        "business_project_link.delete",
        &format!("{business_id}:{technical_project_id}"),
        &key,
        &digest,
    )
    .await?
    {
        return Ok(Json(response));
    }
    let owner_id: String =
        sqlx::query_scalar("SELECT owner_id FROM workspaces WHERE workspace_id = ?")
            .bind(WORKSPACE_ID)
            .fetch_one(&state.pool)
            .await
            .map_err(CatalogApiError::Storage)?;
    let mut tx = state.pool.begin().await.map_err(CatalogApiError::Storage)?;
    let row = sqlx::query(
        "SELECT links.business_project_link_id, links.business_id, links.technical_project_id,
                links.state, links.origin, links.revision, links.confirmed_by, links.confirmed_at,
                links.created_at, links.updated_at
         FROM business_project_links links
         JOIN businesses businesses ON businesses.business_id = links.business_id
         WHERE links.business_id = ? AND links.technical_project_id = ?
           AND businesses.workspace_id = ?",
    )
    .bind(&business_id)
    .bind(&technical_project_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "business_project_link",
        id: format!("{business_id}:{technical_project_id}"),
    })?;
    let current = business_project_link_from_row(&row)?;
    if current.revision != expected {
        return Err(CatalogApiError::Conflict {
            code: "REVISION_CONFLICT",
            message: "业务项目关系已被其他修改更新",
            details: json!({"current_revision": current.revision, "if_match": expected}),
        });
    }
    if current.state == BusinessProjectLinkState::Archived {
        return Err(CatalogApiError::Conflict {
            code: "BUSINESS_PROJECT_LINK_ARCHIVED",
            message: "该业务项目关系已经解除",
            details: json!({"business_project_link_id": current.business_project_link_id}),
        });
    }
    let next_revision = current.revision + 1;
    let updated_at = now();
    sqlx::query(
        "UPDATE business_project_links SET state = 'archived', revision = ?, updated_at = ?
         WHERE business_project_link_id = ? AND revision = ?",
    )
    .bind(next_revision)
    .bind(&updated_at)
    .bind(&current.business_project_link_id)
    .bind(expected)
    .execute(&mut *tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    let record = BusinessProjectLinkRecord {
        business_project_link_id: current.business_project_link_id,
        business_id: current.business_id,
        technical_project_id: current.technical_project_id,
        state: BusinessProjectLinkState::Archived,
        origin: current.origin,
        revision: next_revision,
        confirmed_by: current.confirmed_by,
        confirmed_at: current.confirmed_at,
        created_at: current.created_at,
        updated_at,
    };
    let response = BusinessProjectLinkResponse {
        data: record.clone(),
        meta: real_meta(&request_id(&headers), Freshness::Fresh, next_revision),
    };
    store_mutation_in(
        &mut tx,
        "business_project_link.delete",
        &format!("{business_id}:{technical_project_id}"),
        &key,
        &digest,
        &response,
    )
    .await?;
    publish_catalog_event(
        &mut tx,
        "business-project-link",
        &record.business_project_link_id,
        next_revision,
    )
    .await?;
    tx.commit().await.map_err(CatalogApiError::Storage)?;
    let _ = owner_id;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/api/v1/views/global/resources",
    tag = "catalog",
    params(GlobalResourceQuery),
    responses((status = 200, body = GlobalResourceViewResponse), (status = 400, body = ApiErrorResponse), (status = 404, body = ApiErrorResponse))
)]
pub async fn get_global_resources(
    State(state): State<AppState>,
    Query(query): Query<GlobalResourceQuery>,
) -> Result<Json<GlobalResourceViewResponse>, CatalogApiError> {
    let lens = parse_global_resource_lens(query.lens.as_deref().unwrap_or("topology"))?;
    let business_filter_active = query.business_id.is_some();
    let business_project_ids = if let Some(business_id) = query.business_id.as_deref() {
        let _ = load_business(&state.pool, business_id).await?;
        sqlx::query_scalar::<_, String>(
            "SELECT technical_project_id FROM business_project_links
             WHERE business_id = ? AND state = 'confirmed'",
        )
        .bind(business_id)
        .fetch_all(&state.pool)
        .await
        .map_err(CatalogApiError::Storage)?
        .into_iter()
        .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let requested_project = query.technical_project_id.as_deref();
    let requested_host = query.host_id.as_deref();

    let host_rows = sqlx::query(
        "SELECT host_id, display_name, status, last_checked_at
         FROM hosts WHERE workspace_id = ? ORDER BY host_id",
    )
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let mut hosts = BTreeMap::<String, GlobalResourceNode>::new();
    for row in host_rows {
        let host_id: String = row.try_get("host_id").map_err(CatalogApiError::Storage)?;
        if requested_host.is_some_and(|value| value != host_id) {
            continue;
        }
        hosts.insert(
            host_id.clone(),
            GlobalResourceNode {
                node_id: format!("host:{host_id}"),
                kind: GlobalResourceNodeKind::Host,
                display_name: row
                    .try_get("display_name")
                    .map_err(CatalogApiError::Storage)?,
                state: row.try_get("status").map_err(CatalogApiError::Storage)?,
                host_id: Some(host_id.clone()),
                technical_project_id: None,
                deployment_id: None,
                provider_kind: None,
                freshness: None,
                metadata: json!({
                    "last_checked_at": row.try_get::<Option<String>, _>("last_checked_at")
                        .map_err(CatalogApiError::Storage)?
                }),
            },
        );
    }

    let project_rows = sqlx::query(
        "SELECT technical_project_id, display_name, state, revision
         FROM technical_projects WHERE workspace_id = ? ORDER BY technical_project_id",
    )
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let mut projects = BTreeMap::<String, GlobalResourceNode>::new();
    for row in project_rows {
        let project_id: String = row
            .try_get("technical_project_id")
            .map_err(CatalogApiError::Storage)?;
        let project_state: String = row.try_get("state").map_err(CatalogApiError::Storage)?;
        if project_state == "archived"
            || requested_project.is_some_and(|value| value != project_id)
            || (business_filter_active && !business_project_ids.contains(&project_id))
        {
            continue;
        }
        if let Some(filter) = query.state.as_deref()
            && filter != project_state
        {
            continue;
        }
        projects.insert(
            project_id.clone(),
            GlobalResourceNode {
                node_id: format!("technical_project:{project_id}"),
                kind: GlobalResourceNodeKind::TechnicalProject,
                display_name: row
                    .try_get("display_name")
                    .map_err(CatalogApiError::Storage)?,
                state: project_state,
                host_id: None,
                technical_project_id: Some(project_id.clone()),
                deployment_id: None,
                provider_kind: None,
                freshness: None,
                metadata: json!({
                    "revision": row.try_get::<i64, _>("revision")
                        .map_err(CatalogApiError::Storage)?
                }),
            },
        );
    }

    let mut deployment_sql = String::from(
        "SELECT deployment_id, host_id, provider_kind, external_id, display_name,
                catalog_state, freshness, latest_observation_id, last_observed_at
         FROM deployments WHERE workspace_id = ? AND catalog_state <> 'ignored'",
    );
    if !query.include_candidates {
        deployment_sql.push_str(" AND catalog_state <> 'unassigned'");
    }
    if let Some(provider) = query.provider_kind.as_deref() {
        validate_provider_kind(provider)?;
        deployment_sql.push_str(" AND provider_kind = ?");
    }
    if let Some(freshness) = query.freshness.as_deref() {
        if !matches!(freshness, "fresh" | "stale" | "unavailable") {
            return Err(CatalogApiError::BadRequest {
                code: "INVALID_FRESHNESS",
                message: "freshness 参数不受支持",
                details: json!({"freshness": freshness}),
            });
        }
        deployment_sql.push_str(" AND freshness = ?");
    }
    if let Some(state_filter) = query.state.as_deref()
        && matches!(
            state_filter,
            "observed" | "unassigned" | "stale" | "ignored"
        )
    {
        deployment_sql.push_str(" AND catalog_state = ?");
    }
    if let Some(host) = requested_host {
        deployment_sql.push_str(" AND host_id = ?");
        if !hosts.contains_key(host) {
            return Err(CatalogApiError::NotFound {
                resource: "host",
                id: host.to_owned(),
            });
        }
    }
    let mut deployment_query = sqlx::query(&deployment_sql).bind(WORKSPACE_ID);
    if let Some(provider) = query.provider_kind.as_deref() {
        deployment_query = deployment_query.bind(provider);
    }
    if let Some(freshness) = query.freshness.as_deref() {
        deployment_query = deployment_query.bind(freshness);
    }
    if let Some(state_filter) = query.state.as_deref()
        && matches!(
            state_filter,
            "observed" | "unassigned" | "stale" | "ignored"
        )
    {
        deployment_query = deployment_query.bind(state_filter);
    }
    if let Some(host) = requested_host {
        deployment_query = deployment_query.bind(host);
    }
    let deployment_rows = deployment_query
        .fetch_all(&state.pool)
        .await
        .map_err(CatalogApiError::Storage)?;
    let mut deployments = BTreeMap::<String, GlobalResourceNode>::new();
    for row in deployment_rows {
        let deployment_id: String = row
            .try_get("deployment_id")
            .map_err(CatalogApiError::Storage)?;
        let host_id: String = row.try_get("host_id").map_err(CatalogApiError::Storage)?;
        let provider_kind: String = row
            .try_get("provider_kind")
            .map_err(CatalogApiError::Storage)?;
        let deployment_state: String = row
            .try_get("catalog_state")
            .map_err(CatalogApiError::Storage)?;
        let freshness: String = row.try_get("freshness").map_err(CatalogApiError::Storage)?;
        let provider = parse_provider_kind(&provider_kind)?;
        deployments.insert(
            deployment_id.clone(),
            GlobalResourceNode {
                node_id: format!("deployment:{deployment_id}"),
                kind: GlobalResourceNodeKind::Deployment,
                display_name: row.try_get("display_name").map_err(CatalogApiError::Storage)?,
                state: deployment_state,
                host_id: Some(host_id),
                technical_project_id: None,
                deployment_id: Some(deployment_id.clone()),
                provider_kind: Some(provider),
                freshness: Some(parse_freshness(&freshness)?),
                metadata: json!({
                    "external_id": row.try_get::<String, _>("external_id")
                        .map_err(CatalogApiError::Storage)?,
                    "latest_observation_id": row.try_get::<Option<String>, _>("latest_observation_id")
                        .map_err(CatalogApiError::Storage)?,
                    "last_observed_at": row.try_get::<Option<String>, _>("last_observed_at")
                        .map_err(CatalogApiError::Storage)?
                }),
            },
        );
    }

    let target_rows = sqlx::query(
        "SELECT targets.project_target_id, targets.technical_project_id, targets.deployment_id,
                targets.state, deployments.host_id
         FROM project_targets targets
         JOIN technical_projects projects ON projects.technical_project_id = targets.technical_project_id
         JOIN deployments ON deployments.deployment_id = targets.deployment_id
         WHERE projects.workspace_id = ? AND projects.state = 'active'
           AND targets.state = 'confirmed'",
    )
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let mut target_project_by_deployment = BTreeMap::<String, BTreeSet<String>>::new();
    let mut target_deployment_by_project = BTreeMap::<String, BTreeSet<String>>::new();
    let mut target_host_by_project = BTreeMap::<String, BTreeSet<String>>::new();
    let mut target_ids_by_project_deployment =
        BTreeMap::<(String, String), BTreeSet<String>>::new();
    let mut target_edges = Vec::new();
    for row in target_rows {
        let target_id: String = row
            .try_get("project_target_id")
            .map_err(CatalogApiError::Storage)?;
        let project_id: String = row
            .try_get("technical_project_id")
            .map_err(CatalogApiError::Storage)?;
        let deployment_id: String = row
            .try_get("deployment_id")
            .map_err(CatalogApiError::Storage)?;
        let host_id: String = row.try_get("host_id").map_err(CatalogApiError::Storage)?;
        if !projects.contains_key(&project_id) || !deployments.contains_key(&deployment_id) {
            continue;
        }
        target_project_by_deployment
            .entry(deployment_id.clone())
            .or_default()
            .insert(project_id.clone());
        target_ids_by_project_deployment
            .entry((project_id.clone(), deployment_id.clone()))
            .or_default()
            .insert(target_id.clone());
        target_deployment_by_project
            .entry(project_id.clone())
            .or_default()
            .insert(deployment_id.clone());
        target_host_by_project
            .entry(project_id.clone())
            .or_default()
            .insert(host_id.clone());
        target_edges.push(GlobalResourceEdge {
            edge_id: format!("project-target:{target_id}"),
            kind: GlobalResourceEdgeKind::ProjectTargetsDeployment,
            from_node_id: format!("technical_project:{project_id}"),
            to_node_id: format!("deployment:{deployment_id}"),
            state: "confirmed".to_owned(),
            origin: "user_declared".to_owned(),
            source_refs: vec![format!("project_target:{target_id}")],
            path_refs: vec![format!("project_target:{target_id}")],
            metadata: json!({"host_id": host_id}),
        });
    }

    let resource_rows = sqlx::query(
        "SELECT resource_entity_id, resource_kind, source, external_id, display_name,
                freshness, metadata_json
         FROM resource_entities WHERE workspace_id = ? ORDER BY resource_entity_id",
    )
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let mut resources = BTreeMap::<String, GlobalResourceNode>::new();
    for row in resource_rows {
        let resource_id: String = row
            .try_get("resource_entity_id")
            .map_err(CatalogApiError::Storage)?;
        let freshness: String = row.try_get("freshness").map_err(CatalogApiError::Storage)?;
        let metadata: String = row
            .try_get("metadata_json")
            .map_err(CatalogApiError::Storage)?;
        let metadata_value: Value =
            serde_json::from_str(&metadata).map_err(|_| CatalogApiError::StoredResponse)?;
        resources.insert(
            resource_id.clone(),
            GlobalResourceNode {
                node_id: format!("resource:{resource_id}"),
                kind: GlobalResourceNodeKind::Resource,
                display_name: row
                    .try_get("display_name")
                    .map_err(CatalogApiError::Storage)?,
                state: "observed".to_owned(),
                host_id: None,
                technical_project_id: None,
                deployment_id: None,
                provider_kind: None,
                freshness: Some(parse_freshness(&freshness)?),
                metadata: json!({
                    "resource_kind": row.try_get::<String, _>("resource_kind")
                        .map_err(CatalogApiError::Storage)?,
                    "source": row.try_get::<String, _>("source")
                        .map_err(CatalogApiError::Storage)?,
                    "external_id": row.try_get::<String, _>("external_id")
                        .map_err(CatalogApiError::Storage)?,
                    "attributes": metadata_value
                }),
            },
        );
    }

    let deployment_resource_rows = sqlx::query(
        "SELECT links.deployment_resource_link_id, links.deployment_id,
                links.resource_entity_id, links.relation_kind, links.state, links.origin,
                links.source_refs_json, links.observed_at
         FROM deployment_resource_links links
         JOIN deployments ON deployments.deployment_id = links.deployment_id
         WHERE deployments.workspace_id = ? AND links.state <> 'archived'",
    )
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    let mut resource_project_ids = BTreeMap::<String, BTreeSet<String>>::new();
    let mut resource_project_path_refs = BTreeMap::<(String, String), BTreeSet<String>>::new();
    let mut resource_deployment_ids = BTreeMap::<String, BTreeSet<String>>::new();
    let mut resource_edges = Vec::new();
    for row in deployment_resource_rows {
        let link_id: String = row
            .try_get("deployment_resource_link_id")
            .map_err(CatalogApiError::Storage)?;
        let deployment_id: String = row
            .try_get("deployment_id")
            .map_err(CatalogApiError::Storage)?;
        let resource_id: String = row
            .try_get("resource_entity_id")
            .map_err(CatalogApiError::Storage)?;
        if !deployments.contains_key(&deployment_id) || !resources.contains_key(&resource_id) {
            continue;
        }
        resource_deployment_ids
            .entry(resource_id.clone())
            .or_default()
            .insert(deployment_id.clone());
        let refs: String = row
            .try_get("source_refs_json")
            .map_err(CatalogApiError::Storage)?;
        let source_refs: Vec<String> =
            serde_json::from_str(&refs).map_err(|_| CatalogApiError::StoredResponse)?;
        let relation_state: String = row.try_get("state").map_err(CatalogApiError::Storage)?;
        resource_edges.push(GlobalResourceEdge {
            edge_id: format!("deployment-resource:{link_id}"),
            kind: GlobalResourceEdgeKind::DeploymentUsesResource,
            from_node_id: format!("deployment:{deployment_id}"),
            to_node_id: format!("resource:{resource_id}"),
            state: relation_state.clone(),
            origin: row.try_get("origin").map_err(CatalogApiError::Storage)?,
            source_refs,
            path_refs: vec![
                format!("deployment:{deployment_id}"),
                format!("resource:{resource_id}"),
            ],
            metadata: json!({
                "relation_kind": row.try_get::<String, _>("relation_kind")
                    .map_err(CatalogApiError::Storage)?,
                "observed_at": row.try_get::<Option<String>, _>("observed_at")
                    .map_err(CatalogApiError::Storage)?
            }),
        });
        // A stale edge remains visible as historical context, but it no
        // longer proves a current project/resource relationship and therefore
        // must not make a resource appear shared.
        if !matches!(relation_state.as_str(), "observed" | "confirmed") {
            continue;
        }
        if let Some(project_ids) = target_project_by_deployment.get(&deployment_id) {
            resource_project_ids
                .entry(resource_id.clone())
                .or_default()
                .extend(project_ids.iter().cloned());
        }
    }

    let project_resource_rows = sqlx::query(
        "SELECT links.technical_project_resource_link_id, links.technical_project_id,
                links.resource_entity_id, links.state, links.origin, links.source_refs_json
         FROM technical_project_resource_links links
         JOIN technical_projects projects ON projects.technical_project_id = links.technical_project_id
         WHERE projects.workspace_id = ? AND projects.state = 'active' AND links.state <> 'archived'",
    )
    .bind(WORKSPACE_ID)
    .fetch_all(&state.pool)
    .await
    .map_err(CatalogApiError::Storage)?;
    for row in project_resource_rows {
        let link_id: String = row
            .try_get("technical_project_resource_link_id")
            .map_err(CatalogApiError::Storage)?;
        let project_id: String = row
            .try_get("technical_project_id")
            .map_err(CatalogApiError::Storage)?;
        let resource_id: String = row
            .try_get("resource_entity_id")
            .map_err(CatalogApiError::Storage)?;
        if !projects.contains_key(&project_id) || !resources.contains_key(&resource_id) {
            continue;
        }
        resource_project_ids
            .entry(resource_id.clone())
            .or_default()
            .insert(project_id.clone());
        resource_project_path_refs
            .entry((resource_id.clone(), project_id.clone()))
            .or_default()
            .insert(format!("technical_project_resource:{link_id}"));
        let refs: String = row
            .try_get("source_refs_json")
            .map_err(CatalogApiError::Storage)?;
        resource_edges.push(GlobalResourceEdge {
            edge_id: format!("project-resource:{link_id}"),
            kind: GlobalResourceEdgeKind::ProjectReferencesResource,
            from_node_id: format!("technical_project:{project_id}"),
            to_node_id: format!("resource:{resource_id}"),
            state: row.try_get("state").map_err(CatalogApiError::Storage)?,
            origin: row.try_get("origin").map_err(CatalogApiError::Storage)?,
            source_refs: serde_json::from_str(&refs)
                .map_err(|_| CatalogApiError::StoredResponse)?,
            path_refs: vec![format!("technical_project_resource:{link_id}")],
            metadata: json!({}),
        });
    }

    // Derived impact edges are emitted only after both sides have an explicit
    // confirmed/user-declared path.  They never become stored facts.
    for (resource_id, project_ids) in &resource_project_ids {
        if project_ids.is_empty() {
            continue;
        }
        for project_id in project_ids {
            let mut path_refs = resource_deployment_ids
                .get(resource_id)
                .into_iter()
                .flat_map(|deployments| deployments.iter())
                .filter_map(|deployment_id| {
                    target_ids_by_project_deployment
                        .get(&(project_id.clone(), deployment_id.clone()))
                        .map(|target_ids| {
                            target_ids
                                .iter()
                                .map(|target_id| format!("project_target:{target_id}"))
                                .collect::<Vec<_>>()
                        })
                })
                .flatten()
                .collect::<Vec<_>>();
            if path_refs.is_empty() {
                path_refs.extend(
                    resource_project_path_refs
                        .get(&(resource_id.clone(), project_id.clone()))
                        .into_iter()
                        .flat_map(|refs| refs.iter().cloned()),
                );
            }
            resource_edges.push(GlobalResourceEdge {
                edge_id: format!("impact:{project_id}:{resource_id}"),
                kind: GlobalResourceEdgeKind::ProjectImpactedByResource,
                from_node_id: format!("technical_project:{project_id}"),
                to_node_id: format!("resource:{resource_id}"),
                state: "derived".to_owned(),
                origin: "derived".to_owned(),
                source_refs: Vec::new(),
                path_refs,
                metadata: json!({"confirmed_project_count": project_ids.len()}),
            });
        }
    }

    let mut edges = target_edges;
    for (deployment_id, deployment) in &deployments {
        if let Some(host_id) = deployment.host_id.as_deref()
            && hosts.contains_key(host_id)
        {
            edges.push(GlobalResourceEdge {
                edge_id: format!("deployment-host:{deployment_id}:{host_id}"),
                kind: GlobalResourceEdgeKind::DeploymentRunsOnHost,
                from_node_id: format!("deployment:{deployment_id}"),
                to_node_id: format!("host:{host_id}"),
                state: "observed".to_owned(),
                origin: "observed".to_owned(),
                source_refs: vec![format!("deployment:{deployment_id}")],
                path_refs: Vec::new(),
                metadata: json!({}),
            });
        }
    }
    edges.extend(resource_edges);

    let mut allowed_hosts = hosts.keys().cloned().collect::<BTreeSet<_>>();
    let mut allowed_projects = projects.keys().cloned().collect::<BTreeSet<_>>();
    let mut allowed_deployments = deployments.keys().cloned().collect::<BTreeSet<_>>();
    let mut allowed_resources = resources
        .keys()
        .filter(|id| resource_project_ids.contains_key(*id))
        .cloned()
        .collect::<BTreeSet<_>>();
    match lens {
        GlobalResourceLens::Topology => {}
        GlobalResourceLens::Shared => {
            allowed_resources.retain(|resource_id| {
                resource_project_ids
                    .get(resource_id)
                    .is_some_and(|ids| ids.len() >= 2)
            });
            allowed_deployments.retain(|deployment_id| {
                resource_deployment_ids
                    .iter()
                    .any(|(resource_id, deployments)| {
                        allowed_resources.contains(resource_id)
                            && deployments.contains(deployment_id)
                    })
            });
            allowed_projects.retain(|project_id| {
                resource_project_ids.iter().any(|(resource_id, projects)| {
                    allowed_resources.contains(resource_id) && projects.contains(project_id)
                })
            });
            allowed_hosts.retain(|host_id| {
                allowed_deployments.iter().any(|deployment_id| {
                    deployments
                        .get(deployment_id)
                        .and_then(|node| node.host_id.as_ref())
                        == Some(host_id)
                })
            });
        }
        GlobalResourceLens::Impact => {
            let (kind, focus) = match (query.focus_kind.as_deref(), query.focus_id.as_deref()) {
                (Some(kind), Some(focus)) => (kind, focus),
                _ => {
                    return Err(CatalogApiError::BadRequest {
                        code: "IMPACT_FOCUS_REQUIRED",
                        message: "impact 视角需要 focus_kind 和 focus_id",
                        details: json!({}),
                    });
                }
            };
            match kind {
                "host" => {
                    if !hosts.contains_key(focus) {
                        return Err(CatalogApiError::NotFound {
                            resource: "host",
                            id: focus.to_owned(),
                        });
                    }
                    allowed_hosts = BTreeSet::from([focus.to_owned()]);
                    allowed_deployments.retain(|deployment_id| {
                        deployments
                            .get(deployment_id)
                            .and_then(|node| node.host_id.as_deref())
                            == Some(focus)
                    });
                    allowed_projects.retain(|project_id| {
                        target_host_by_project
                            .get(project_id)
                            .is_some_and(|hosts| hosts.contains(focus))
                    });
                    allowed_resources.retain(|resource_id| {
                        resource_deployment_ids.get(resource_id).is_some_and(|ids| {
                            ids.iter().any(|id| allowed_deployments.contains(id))
                        }) || resource_project_ids
                            .get(resource_id)
                            .is_some_and(|ids| ids.iter().any(|id| allowed_projects.contains(id)))
                    });
                }
                "deployment" => {
                    if !deployments.contains_key(focus) {
                        return Err(CatalogApiError::NotFound {
                            resource: "deployment",
                            id: focus.to_owned(),
                        });
                    }
                    allowed_deployments = BTreeSet::from([focus.to_owned()]);
                    allowed_projects = target_project_by_deployment
                        .get(focus)
                        .cloned()
                        .unwrap_or_default();
                    allowed_hosts = deployments
                        .get(focus)
                        .and_then(|node| node.host_id.clone())
                        .map(|host_id| BTreeSet::from([host_id]))
                        .unwrap_or_default();
                    allowed_resources.retain(|resource_id| {
                        resource_deployment_ids
                            .get(resource_id)
                            .is_some_and(|ids| ids.contains(focus))
                    });
                }
                "technical_project" => {
                    if !projects.contains_key(focus) {
                        return Err(CatalogApiError::NotFound {
                            resource: "technical_project",
                            id: focus.to_owned(),
                        });
                    }
                    allowed_projects = BTreeSet::from([focus.to_owned()]);
                    allowed_deployments = target_deployment_by_project
                        .get(focus)
                        .cloned()
                        .unwrap_or_default();
                    allowed_hosts = target_host_by_project
                        .get(focus)
                        .cloned()
                        .unwrap_or_default();
                    allowed_resources.retain(|resource_id| {
                        resource_project_ids
                            .get(resource_id)
                            .is_some_and(|ids| ids.contains(focus))
                    });
                }
                "resource" => {
                    if !resources.contains_key(focus) {
                        return Err(CatalogApiError::NotFound {
                            resource: "resource",
                            id: focus.to_owned(),
                        });
                    }
                    allowed_resources = BTreeSet::from([focus.to_owned()]);
                    allowed_projects = resource_project_ids.get(focus).cloned().unwrap_or_default();
                    allowed_deployments = resource_deployment_ids
                        .get(focus)
                        .cloned()
                        .unwrap_or_default();
                    allowed_hosts = allowed_deployments
                        .iter()
                        .filter_map(|deployment_id| {
                            deployments
                                .get(deployment_id)
                                .and_then(|node| node.host_id.clone())
                        })
                        .collect();
                }
                _ => {
                    return Err(CatalogApiError::BadRequest {
                        code: "INVALID_FOCUS_KIND",
                        message: "focus_kind 只支持 host、technical_project、deployment 或 resource",
                        details: json!({"focus_kind": kind}),
                    });
                }
            }
        }
    }

    let mut nodes = Vec::new();
    nodes.extend(
        hosts
            .into_iter()
            .filter_map(|(id, node)| allowed_hosts.contains(&id).then_some(node)),
    );
    nodes.extend(
        projects
            .into_iter()
            .filter_map(|(id, node)| allowed_projects.contains(&id).then_some(node)),
    );
    nodes.extend(
        deployments
            .into_iter()
            .filter_map(|(id, node)| allowed_deployments.contains(&id).then_some(node)),
    );
    nodes.extend(
        resources
            .into_iter()
            .filter_map(|(id, node)| allowed_resources.contains(&id).then_some(node)),
    );
    let node_ids = nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<BTreeSet<_>>();
    edges.retain(|edge| {
        node_ids.contains(&edge.from_node_id) && node_ids.contains(&edge.to_node_id)
    });
    let provider_values = nodes
        .iter()
        .filter_map(|node| {
            node.provider_kind.as_ref().map(|provider| {
                serde_json::to_string(provider)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_owned()
            })
        })
        .collect::<BTreeSet<_>>();
    let state_values = nodes
        .iter()
        .map(|node| node.state.clone())
        .collect::<BTreeSet<_>>();
    let resource_count = nodes
        .iter()
        .filter(|node| node.kind == GlobalResourceNodeKind::Resource)
        .count() as i64;
    let shared_count = allowed_resources
        .iter()
        .filter(|resource_id| {
            resource_project_ids
                .get(*resource_id)
                .is_some_and(|ids| ids.len() >= 2)
        })
        .count() as i64;
    let summary = GlobalResourceSummary {
        host_count: nodes
            .iter()
            .filter(|node| node.kind == GlobalResourceNodeKind::Host)
            .count() as i64,
        technical_project_count: nodes
            .iter()
            .filter(|node| node.kind == GlobalResourceNodeKind::TechnicalProject)
            .count() as i64,
        deployment_count: nodes
            .iter()
            .filter(|node| node.kind == GlobalResourceNodeKind::Deployment)
            .count() as i64,
        resource_count,
        shared_resource_count: shared_count,
        unassigned_deployment_count: nodes
            .iter()
            .filter(|node| {
                node.kind == GlobalResourceNodeKind::Deployment && node.state == "unassigned"
            })
            .count() as i64,
        hidden_node_count: 0,
        hidden_edge_count: 0,
    };
    let data = GlobalResourceViewData {
        schema_version: "global-resource.v1".to_owned(),
        lens,
        summary,
        nodes,
        edges: edges.clone(),
        facets: vec![
            GlobalResourceFacet {
                key: "provider_kind".to_owned(),
                values: provider_values.into_iter().collect(),
            },
            GlobalResourceFacet {
                key: "state".to_owned(),
                values: state_values.into_iter().collect(),
            },
        ],
        hidden_counts: json!({"nodes": 0, "edges": 0}),
    };
    Ok(Json(GlobalResourceViewResponse {
        data,
        meta: real_meta(&Uuid::new_v4().to_string(), Freshness::Fresh, 1),
    }))
}

async fn load_deployments(
    pool: &SqlitePool,
    host_id: Option<&str>,
    state: Option<&str>,
    provider_kind: Option<&str>,
    include_ignored: bool,
) -> Result<Vec<DeploymentRecord>, CatalogApiError> {
    if let Some(state) = state {
        validate_catalog_state(state)?;
    }
    if let Some(provider) = provider_kind {
        validate_provider_kind(provider)?;
    }
    let mut sql = String::from(
        "SELECT deployment_id, workspace_id, host_id, provider_kind, external_id, identity_key,
                display_name, catalog_state, latest_observation_id, last_observed_at,
                last_observed_at_epoch_ms, freshness, created_at, updated_at
         FROM deployments WHERE workspace_id = ?",
    );
    if host_id.is_some() {
        sql.push_str(" AND host_id = ?");
    }
    if state.is_some() {
        sql.push_str(" AND catalog_state = ?");
    } else if !include_ignored {
        sql.push_str(" AND catalog_state <> 'ignored'");
    }
    if provider_kind.is_some() {
        sql.push_str(" AND provider_kind = ?");
    }
    sql.push_str(" ORDER BY COALESCE(last_observed_at_epoch_ms, 0) DESC, deployment_id");
    let mut query = sqlx::query(&sql).bind(WORKSPACE_ID);
    if let Some(host_id) = host_id {
        query = query.bind(host_id);
    }
    if let Some(state) = state {
        query = query.bind(state);
    }
    if let Some(provider) = provider_kind {
        query = query.bind(provider);
    }
    let rows = query
        .fetch_all(pool)
        .await
        .map_err(CatalogApiError::Storage)?;
    rows.iter().map(deployment_from_row).collect()
}

async fn load_deployment(
    pool: &SqlitePool,
    deployment_id: &str,
) -> Result<DeploymentRecord, CatalogApiError> {
    let row = sqlx::query(
        "SELECT deployment_id, workspace_id, host_id, provider_kind, external_id, identity_key,
                display_name, catalog_state, latest_observation_id, last_observed_at,
                last_observed_at_epoch_ms, freshness, created_at, updated_at
         FROM deployments WHERE deployment_id = ? AND workspace_id = ?",
    )
    .bind(deployment_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "deployment",
        id: deployment_id.to_owned(),
    })?;
    deployment_from_row(&row)
}

fn technical_project_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<TechnicalProjectRecord, CatalogApiError> {
    Ok(TechnicalProjectRecord {
        technical_project_id: row
            .try_get("technical_project_id")
            .map_err(CatalogApiError::Storage)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(CatalogApiError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(CatalogApiError::Storage)?,
        summary: row.try_get("summary").map_err(CatalogApiError::Storage)?,
        state: parse_project_state(
            &row.try_get::<String, _>("state")
                .map_err(CatalogApiError::Storage)?,
        )?,
        revision: row.try_get("revision").map_err(CatalogApiError::Storage)?,
        created_by: row
            .try_get("created_by")
            .map_err(CatalogApiError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(CatalogApiError::Storage)?,
        updated_by: row
            .try_get("updated_by")
            .map_err(CatalogApiError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(CatalogApiError::Storage)?,
    })
}

fn deployment_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<DeploymentRecord, CatalogApiError> {
    Ok(DeploymentRecord {
        deployment_id: row
            .try_get("deployment_id")
            .map_err(CatalogApiError::Storage)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(CatalogApiError::Storage)?,
        host_id: row.try_get("host_id").map_err(CatalogApiError::Storage)?,
        provider_kind: parse_provider_kind(
            &row.try_get::<String, _>("provider_kind")
                .map_err(CatalogApiError::Storage)?,
        )?,
        external_id: row
            .try_get("external_id")
            .map_err(CatalogApiError::Storage)?,
        identity_key: row
            .try_get("identity_key")
            .map_err(CatalogApiError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(CatalogApiError::Storage)?,
        state: parse_catalog_state(
            &row.try_get::<String, _>("catalog_state")
                .map_err(CatalogApiError::Storage)?,
        )?,
        latest_observation_id: row
            .try_get("latest_observation_id")
            .map_err(CatalogApiError::Storage)?,
        last_observed_at: row
            .try_get("last_observed_at")
            .map_err(CatalogApiError::Storage)?,
        last_observed_at_epoch_ms: row
            .try_get("last_observed_at_epoch_ms")
            .map_err(CatalogApiError::Storage)?,
        freshness: parse_freshness(
            &row.try_get::<String, _>("freshness")
                .map_err(CatalogApiError::Storage)?,
        )?,
        created_at: row
            .try_get("created_at")
            .map_err(CatalogApiError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(CatalogApiError::Storage)?,
    })
}

fn observation_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<DeploymentObservationRecord, CatalogApiError> {
    let refs: String = row
        .try_get("evidence_refs_json")
        .map_err(CatalogApiError::Storage)?;
    let metadata: String = row
        .try_get("metadata_json")
        .map_err(CatalogApiError::Storage)?;
    Ok(DeploymentObservationRecord {
        deployment_observation_id: row
            .try_get("deployment_observation_id")
            .map_err(CatalogApiError::Storage)?,
        deployment_id: row
            .try_get("deployment_id")
            .map_err(CatalogApiError::Storage)?,
        discovery_run_id: row
            .try_get("discovery_run_id")
            .map_err(CatalogApiError::Storage)?,
        provider_kind: parse_provider_kind(
            &row.try_get::<String, _>("provider_kind")
                .map_err(CatalogApiError::Storage)?,
        )?,
        external_id: row
            .try_get("external_id")
            .map_err(CatalogApiError::Storage)?,
        observation_state: parse_observation_state(
            &row.try_get::<String, _>("observation_state")
                .map_err(CatalogApiError::Storage)?,
        )?,
        provider_status: row
            .try_get::<Option<String>, _>("provider_status")
            .map_err(CatalogApiError::Storage)?
            .as_deref()
            .map(parse_provider_status)
            .transpose()?,
        observed_at: row
            .try_get("observed_at")
            .map_err(CatalogApiError::Storage)?,
        observed_at_epoch_ms: row
            .try_get("observed_at_epoch_ms")
            .map_err(CatalogApiError::Storage)?,
        evidence_refs: serde_json::from_str(&refs).map_err(|_| CatalogApiError::StoredResponse)?,
        metadata: serde_json::from_str(&metadata).map_err(|_| CatalogApiError::StoredResponse)?,
        created_at: row
            .try_get("created_at")
            .map_err(CatalogApiError::Storage)?,
    })
}

fn target_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ProjectTargetRecord, CatalogApiError> {
    let capabilities: String = row
        .try_get("capabilities_json")
        .map_err(CatalogApiError::Storage)?;
    Ok(ProjectTargetRecord {
        project_target_id: row
            .try_get("project_target_id")
            .map_err(CatalogApiError::Storage)?,
        technical_project_id: row
            .try_get("technical_project_id")
            .map_err(CatalogApiError::Storage)?,
        deployment_id: row
            .try_get("deployment_id")
            .map_err(CatalogApiError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(CatalogApiError::Storage)?,
        adapter_kind: parse_adapter_kind(
            &row.try_get::<String, _>("adapter_kind")
                .map_err(CatalogApiError::Storage)?,
        )?,
        capabilities: serde_json::from_str::<Vec<String>>(&capabilities)
            .map_err(|_| CatalogApiError::StoredResponse)?
            .iter()
            .map(|v| parse_capability(v))
            .collect::<Result<Vec<_>, _>>()?,
        approval_policy: parse_approval_policy(
            &row.try_get::<String, _>("approval_policy")
                .map_err(CatalogApiError::Storage)?,
        )?,
        state: parse_target_state(
            &row.try_get::<String, _>("state")
                .map_err(CatalogApiError::Storage)?,
        )?,
        revision: row.try_get("revision").map_err(CatalogApiError::Storage)?,
        confirmed_by: row
            .try_get("confirmed_by")
            .map_err(CatalogApiError::Storage)?,
        confirmed_at: row
            .try_get("confirmed_at")
            .map_err(CatalogApiError::Storage)?,
        last_observed_at: row
            .try_get("last_observed_at")
            .map_err(CatalogApiError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(CatalogApiError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(CatalogApiError::Storage)?,
    })
}

fn business_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<BusinessRecord, CatalogApiError> {
    Ok(BusinessRecord {
        business_id: row
            .try_get("business_id")
            .map_err(CatalogApiError::Storage)?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(CatalogApiError::Storage)?,
        display_name: row
            .try_get("display_name")
            .map_err(CatalogApiError::Storage)?,
        summary: row.try_get("summary").map_err(CatalogApiError::Storage)?,
        state: parse_business_state(
            &row.try_get::<String, _>("state")
                .map_err(CatalogApiError::Storage)?,
        )?,
        origin: parse_business_origin(
            &row.try_get::<String, _>("origin")
                .map_err(CatalogApiError::Storage)?,
        )?,
        revision: row.try_get("revision").map_err(CatalogApiError::Storage)?,
        created_by: row
            .try_get("created_by")
            .map_err(CatalogApiError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(CatalogApiError::Storage)?,
        updated_by: row
            .try_get("updated_by")
            .map_err(CatalogApiError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(CatalogApiError::Storage)?,
    })
}

fn business_project_link_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BusinessProjectLinkRecord, CatalogApiError> {
    Ok(BusinessProjectLinkRecord {
        business_project_link_id: row
            .try_get("business_project_link_id")
            .map_err(CatalogApiError::Storage)?,
        business_id: row
            .try_get("business_id")
            .map_err(CatalogApiError::Storage)?,
        technical_project_id: row
            .try_get("technical_project_id")
            .map_err(CatalogApiError::Storage)?,
        state: parse_business_project_link_state(
            &row.try_get::<String, _>("state")
                .map_err(CatalogApiError::Storage)?,
        )?,
        origin: parse_business_origin(
            &row.try_get::<String, _>("origin")
                .map_err(CatalogApiError::Storage)?,
        )?,
        revision: row.try_get("revision").map_err(CatalogApiError::Storage)?,
        confirmed_by: row
            .try_get("confirmed_by")
            .map_err(CatalogApiError::Storage)?,
        confirmed_at: row
            .try_get("confirmed_at")
            .map_err(CatalogApiError::Storage)?,
        created_at: row
            .try_get("created_at")
            .map_err(CatalogApiError::Storage)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(CatalogApiError::Storage)?,
    })
}

async fn load_business(
    pool: &SqlitePool,
    business_id: &str,
) -> Result<BusinessRecord, CatalogApiError> {
    let row = sqlx::query(
        "SELECT business_id, workspace_id, display_name, summary, state, origin,
                revision, created_by, created_at, updated_by, updated_at
         FROM businesses WHERE business_id = ? AND workspace_id = ?",
    )
    .bind(business_id)
    .bind(WORKSPACE_ID)
    .fetch_optional(pool)
    .await
    .map_err(CatalogApiError::Storage)?
    .ok_or_else(|| CatalogApiError::NotFound {
        resource: "business",
        id: business_id.to_owned(),
    })?;
    business_from_row(&row)
}

async fn insert_business_project_link(
    tx: &mut Transaction<'_, Sqlite>,
    business_id: &str,
    technical_project_id: &str,
    owner_id: &str,
    timestamp: &str,
) -> Result<(), CatalogApiError> {
    sqlx::query(
        "INSERT INTO business_project_links(
            business_project_link_id, business_id, technical_project_id, state, origin,
            revision, confirmed_by, confirmed_at, created_at, updated_at
         ) VALUES (?, ?, ?, 'confirmed', 'user_declared', 1, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(business_id)
    .bind(technical_project_id)
    .bind(owner_id)
    .bind(timestamp)
    .bind(timestamp)
    .bind(timestamp)
    .execute(&mut **tx)
    .await
    .map_err(CatalogApiError::Storage)?;
    Ok(())
}

fn normalized_project_ids(values: &[String]) -> Result<Vec<String>, CatalogApiError> {
    let mut ids = BTreeSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.len() > 128 {
            return Err(CatalogApiError::BadRequest {
                code: "INVALID_PROJECT_REFERENCE",
                message: "TechnicalProject 引用格式不正确",
                details: json!({"maximum": 128}),
            });
        }
        ids.insert(value.to_owned());
    }
    Ok(ids.into_iter().collect())
}

fn parse_business_state(value: &str) -> Result<BusinessState, CatalogApiError> {
    match value {
        "active" => Ok(BusinessState::Active),
        "archived" => Ok(BusinessState::Archived),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_business_origin(value: &str) -> Result<BusinessOrigin, CatalogApiError> {
    match value {
        "user_declared" => Ok(BusinessOrigin::UserDeclared),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_business_project_link_state(
    value: &str,
) -> Result<BusinessProjectLinkState, CatalogApiError> {
    match value {
        "confirmed" => Ok(BusinessProjectLinkState::Confirmed),
        "archived" => Ok(BusinessProjectLinkState::Archived),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn business_state_name(value: &BusinessState) -> &'static str {
    match value {
        BusinessState::Active => "active",
        BusinessState::Archived => "archived",
    }
}

fn validate_business_state(value: &str) -> Result<(), CatalogApiError> {
    parse_business_state(value).map(|_| ())
}

fn parse_global_resource_lens(value: &str) -> Result<GlobalResourceLens, CatalogApiError> {
    match value {
        "topology" => Ok(GlobalResourceLens::Topology),
        "shared" => Ok(GlobalResourceLens::Shared),
        "impact" => Ok(GlobalResourceLens::Impact),
        _ => Err(CatalogApiError::BadRequest {
            code: "INVALID_RESOURCE_LENS",
            message: "lens 只支持 topology、shared 或 impact",
            details: json!({"lens": value}),
        }),
    }
}

fn parse_provider_kind(value: &str) -> Result<DeploymentProviderKind, CatalogApiError> {
    match value {
        "docker" => Ok(DeploymentProviderKind::Docker),
        "compose" => Ok(DeploymentProviderKind::Compose),
        "systemd" => Ok(DeploymentProviderKind::Systemd),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_catalog_state(value: &str) -> Result<DeploymentCatalogState, CatalogApiError> {
    match value {
        "observed" => Ok(DeploymentCatalogState::Observed),
        "unassigned" => Ok(DeploymentCatalogState::Unassigned),
        "stale" => Ok(DeploymentCatalogState::Stale),
        "ignored" => Ok(DeploymentCatalogState::Ignored),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_freshness(value: &str) -> Result<Freshness, CatalogApiError> {
    match value {
        "fresh" => Ok(Freshness::Fresh),
        "stale" => Ok(Freshness::Stale),
        "unavailable" => Ok(Freshness::Unavailable),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_observation_state(value: &str) -> Result<DeploymentObservationState, CatalogApiError> {
    match value {
        "observed" => Ok(DeploymentObservationState::Observed),
        "missing" => Ok(DeploymentObservationState::Missing),
        "unknown" => Ok(DeploymentObservationState::Unknown),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_provider_status(
    value: &str,
) -> Result<crate::contracts::DiscoveryProviderStatus, CatalogApiError> {
    match value {
        "ready" => Ok(crate::contracts::DiscoveryProviderStatus::Ready),
        "unavailable" => Ok(crate::contracts::DiscoveryProviderStatus::Unavailable),
        "permission_denied" => Ok(crate::contracts::DiscoveryProviderStatus::PermissionDenied),
        "timed_out" => Ok(crate::contracts::DiscoveryProviderStatus::TimedOut),
        "failed" => Ok(crate::contracts::DiscoveryProviderStatus::Failed),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_project_state(value: &str) -> Result<TechnicalProjectState, CatalogApiError> {
    match value {
        "active" => Ok(TechnicalProjectState::Active),
        "archived" => Ok(TechnicalProjectState::Archived),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_adapter_kind(value: &str) -> Result<ProjectTargetAdapterKind, CatalogApiError> {
    match value {
        "read_only" => Ok(ProjectTargetAdapterKind::ReadOnly),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_capability(value: &str) -> Result<ProjectTargetCapability, CatalogApiError> {
    match value {
        "read_only" => Ok(ProjectTargetCapability::ReadOnly),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_approval_policy(value: &str) -> Result<ProjectTargetApprovalPolicy, CatalogApiError> {
    match value {
        "read_only" => Ok(ProjectTargetApprovalPolicy::ReadOnly),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn parse_target_state(value: &str) -> Result<ProjectTargetState, CatalogApiError> {
    match value {
        "confirmed" => Ok(ProjectTargetState::Confirmed),
        "stale" => Ok(ProjectTargetState::Stale),
        "archived" => Ok(ProjectTargetState::Archived),
        _ => Err(CatalogApiError::StoredResponse),
    }
}

fn project_state_name(value: &TechnicalProjectState) -> &'static str {
    match value {
        TechnicalProjectState::Active => "active",
        TechnicalProjectState::Archived => "archived",
    }
}

fn project_state_name_from_record(value: &TechnicalProjectRecord) -> &'static str {
    project_state_name(&value.state)
}

fn target_state_name(value: &ProjectTargetState) -> &'static str {
    match value {
        ProjectTargetState::Confirmed => "confirmed",
        ProjectTargetState::Stale => "stale",
        ProjectTargetState::Archived => "archived",
    }
}

fn validate_project_state(value: &str) -> Result<(), CatalogApiError> {
    parse_project_state(value).map(|_| ())
}

fn validate_catalog_state(value: &str) -> Result<(), CatalogApiError> {
    parse_catalog_state(value).map(|_| ())
}

fn validate_provider_kind(value: &str) -> Result<(), CatalogApiError> {
    parse_provider_kind(value).map(|_| ())
}

fn normalized_name(value: &str, max: usize) -> Result<String, CatalogApiError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(CatalogApiError::BadRequest {
            code: "INVALID_NAME",
            message: "名称长度不符合要求",
            details: json!({"maximum": max}),
        });
    }
    Ok(value.to_owned())
}

fn normalized_optional(
    value: &Option<String>,
    max: usize,
) -> Result<Option<String>, CatalogApiError> {
    value
        .as_deref()
        .map(|v| {
            let trimmed = v.trim();
            if trimmed.chars().count() > max {
                return Err(CatalogApiError::BadRequest {
                    code: "INVALID_SUMMARY",
                    message: "摘要长度超出限制",
                    details: json!({"maximum": max}),
                });
            }
            Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
        })
        .transpose()
        .map(|v| v.flatten())
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, CatalogApiError> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= MAX_IDEMPOTENCY_KEY
                && v.bytes().all(|b| !b.is_ascii_control())
        })
        .map(ToOwned::to_owned)
        .ok_or(CatalogApiError::BadRequest {
            code: "IDEMPOTENCY_KEY_REQUIRED",
            message: "该操作需要 Idempotency-Key",
            details: json!({}),
        })
}

fn if_match_revision(headers: &HeaderMap) -> Result<i64, CatalogApiError> {
    let value = headers
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .ok_or(CatalogApiError::PreconditionRequired)?;
    value
        .trim_matches('"')
        .strip_prefix("revision-")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v >= 1)
        .ok_or(CatalogApiError::BadRequest {
            code: "INVALID_IF_MATCH",
            message: "If-Match 必须使用 revision-N 格式",
            details: json!({}),
        })
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty() && v.len() <= MAX_REQUEST_ID && v.bytes().all(|b| !b.is_ascii_control())
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn digest_json<T: Serialize>(value: &T) -> Result<String, CatalogApiError> {
    let bytes = serde_json::to_vec(value).map_err(CatalogApiError::Serialization)?;
    Ok(hex_digest(&Sha256::digest(bytes)))
}

async fn replay_mutation<T: DeserializeOwned>(
    pool: &SqlitePool,
    kind: &str,
    resource_id: &str,
    key: &str,
    digest: &str,
) -> Result<Option<T>, CatalogApiError> {
    let row = sqlx::query("SELECT request_sha256, response_json FROM catalog_mutation_requests WHERE workspace_id = ? AND resource_kind = ? AND resource_id = ? AND idempotency_key = ?")
        .bind(WORKSPACE_ID).bind(kind).bind(resource_id).bind(key).fetch_optional(pool).await.map_err(CatalogApiError::Storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let recorded: String = row
        .try_get("request_sha256")
        .map_err(CatalogApiError::Storage)?;
    if recorded != digest {
        return Err(CatalogApiError::Conflict {
            code: "IDEMPOTENCY_KEY_REUSED",
            message: "Idempotency-Key 已用于不同目录请求",
            details: json!({}),
        });
    }
    let payload: String = row
        .try_get("response_json")
        .map_err(CatalogApiError::Storage)?;
    serde_json::from_str(&payload)
        .map(Some)
        .map_err(|_| CatalogApiError::StoredResponse)
}

async fn store_mutation_in<T: Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    resource_id: &str,
    key: &str,
    digest: &str,
    response: &T,
) -> Result<(), CatalogApiError> {
    let payload = serde_json::to_string(response).map_err(CatalogApiError::Serialization)?;
    sqlx::query("INSERT INTO catalog_mutation_requests(catalog_request_id, workspace_id, resource_kind, resource_id, idempotency_key, request_sha256, response_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(WORKSPACE_ID).bind(kind).bind(resource_id).bind(key).bind(digest).bind(payload).bind(now()).execute(&mut **tx).await.map_err(CatalogApiError::Storage)?;
    Ok(())
}

async fn publish_catalog_event(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    id: &str,
    revision: i64,
) -> Result<(), CatalogApiError> {
    events::publish_in_transaction(
        tx,
        ChangeEventKind::ProjectionChanged,
        &format!("catalog:{kind}:{id}"),
        revision,
        json!({"catalog_kind": kind, "id": id}),
    )
    .await
    .map_err(CatalogApiError::Storage)?;
    Ok(())
}

fn real_meta(request_id: &str, freshness: Freshness, revision: i64) -> ApiMeta {
    ApiMeta {
        request_id: request_id.to_owned(),
        revision,
        generated_at: now(),
        freshness: freshness.clone(),
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status: match freshness {
                Freshness::Fresh => DataSourceStatus::Fresh,
                Freshness::Stale => DataSourceStatus::Stale,
                Freshness::Unavailable => DataSourceStatus::Unavailable,
            },
            label: "SQLite deployment catalog".to_owned(),
        },
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
