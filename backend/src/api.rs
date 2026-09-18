pub mod catalog_api;

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::{OriginalUri, Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::ETAG},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::{RwLock, Semaphore};
use tower_http::{services::ServeDir, trace::TraceLayer};
use utoipa::{
    Modify, OpenApi,
    openapi::{
        OpenApi as OpenApiDocument,
        security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme},
    },
};
use uuid::Uuid;

use crate::{
    auth::{self, AuthService, AuthSessionData, AuthSessionResponse, LoginRequest},
    contracts::*,
    data_management::{self, DataExport, DataExportResponse, DeletionReceipt, DeletionResponse},
    discovery::DiscoveryRunner,
    discovery_diff, events, m1,
    model_provider::{self, ModelClient},
    monitoring_api, monitoring_health, monitoring_history, monitoring_rollup, monitoring_scheduler,
    onboarding, project_agent, projection,
    secrets::FileSecretStore,
    ssh::SystemSsh,
};

#[derive(Clone)]
pub struct AppState {
    pub(crate) pool: SqlitePool,
    pub(crate) secrets: Arc<FileSecretStore>,
    pub(crate) ssh: Arc<SystemSsh>,
    pub(crate) discovery: Arc<DiscoveryRunner>,
    pub(crate) model_client: Arc<ModelClient>,
    pub(crate) auth: Arc<AuthService>,
    pub(crate) data_root: Arc<PathBuf>,
    pub(crate) observation_admission: Arc<RwLock<()>>,
    pub(crate) observation_permits: Arc<Semaphore>,
    pub(crate) monitoring_scheduler: Arc<monitoring_scheduler::MonitoringSchedulerRuntime>,
    pub(crate) monitoring_rollup: Arc<monitoring_rollup::MonitoringRollupRuntime>,
    pub(crate) shutdown_requested: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(pool: SqlitePool) -> Self {
        let data_root = env::var_os("NETWORK_ATLAS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data"));
        let secrets = FileSecretStore::new(data_root.join("secrets"));
        let ssh = SystemSsh::system_default(data_root.join("ssh"));
        Self::with_services_model_auth(
            pool,
            secrets,
            ssh,
            ModelClient::default(),
            AuthService::development_disabled(),
            data_root,
        )
    }

    pub fn with_auth(pool: SqlitePool, auth: AuthService) -> Self {
        let data_root = env::var_os("NETWORK_ATLAS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data"));
        let secrets = FileSecretStore::new(data_root.join("secrets"));
        let ssh = SystemSsh::system_default(data_root.join("ssh"));
        Self::with_services_model_auth(pool, secrets, ssh, ModelClient::default(), auth, data_root)
    }

    pub fn with_services(pool: SqlitePool, secrets: FileSecretStore, ssh: SystemSsh) -> Self {
        Self::with_services_and_model(pool, secrets, ssh, ModelClient::default())
    }

    pub fn with_services_and_model(
        pool: SqlitePool,
        secrets: FileSecretStore,
        ssh: SystemSsh,
        model_client: ModelClient,
    ) -> Self {
        let data_root = secrets
            .root()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("data"));
        Self::with_services_model_auth(
            pool,
            secrets,
            ssh,
            model_client,
            AuthService::development_disabled(),
            data_root,
        )
    }

    pub fn with_services_model_auth(
        pool: SqlitePool,
        secrets: FileSecretStore,
        ssh: SystemSsh,
        model_client: ModelClient,
        auth: AuthService,
        data_root: PathBuf,
    ) -> Self {
        let discovery = DiscoveryRunner::new(ssh.clone());
        let scheduler_settings = monitoring_scheduler::SchedulerSettings::from_environment();
        let rollup_settings = monitoring_rollup::RollupSettings::from_environment();
        Self {
            pool,
            secrets: Arc::new(secrets),
            ssh: Arc::new(ssh),
            discovery: Arc::new(discovery),
            model_client: Arc::new(model_client),
            auth: Arc::new(auth),
            data_root: Arc::new(data_root),
            observation_admission: Arc::new(RwLock::new(())),
            observation_permits: Arc::new(Semaphore::new(
                usize::try_from(scheduler_settings.max_concurrency).unwrap_or(1),
            )),
            monitoring_scheduler: Arc::new(monitoring_scheduler::MonitoringSchedulerRuntime::new(
                scheduler_settings,
            )),
            monitoring_rollup: Arc::new(monitoring_rollup::MonitoringRollupRuntime::new(
                rollup_settings,
            )),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn begin_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    /// Returns the shared admission gate used by every SSH observation worker.
    ///
    /// Data-management operations use the same gate to establish an exclusive
    /// backup/delete boundary; external process orchestration may close it when
    /// observation admission is permanently unavailable.
    pub fn observation_gate(&self) -> Arc<Semaphore> {
        self.observation_permits.clone()
    }
}

struct OwnerSessionSecurity;

impl Modify for OwnerSessionSecurity {
    fn modify(&self, openapi: &mut OpenApiDocument) {
        openapi
            .components
            .get_or_insert_default()
            .add_security_scheme(
                "owner_session",
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                    auth::SESSION_COOKIE,
                    "HttpOnly, Secure, SameSite=Strict single-owner session cookie",
                ))),
            );
        let requirement = SecurityRequirement::new("owner_session", Vec::<String>::new());
        for (path, item) in &mut openapi.paths.paths {
            if !path.starts_with("/api/v1/") || path == "/api/v1/auth/login" {
                continue;
            }
            for operation in [
                item.get.as_mut(),
                item.put.as_mut(),
                item.post.as_mut(),
                item.delete.as_mut(),
                item.patch.as_mut(),
                item.head.as_mut(),
                item.options.as_mut(),
                item.trace.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                operation.security = Some(vec![requirement.clone()]);
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Network Atlas API",
        version = "0.1.0",
        description = "MVP-1 visualization-first contract: Linux SSH evidence, deterministic local projection, optional Agent assistance, and a single-owner remote release boundary"
    ),
    modifiers(&OwnerSessionSecurity),
    paths(
        healthz,
        auth::login,
        auth::get_session,
        auth::logout,
        events::stream,
        get_bootstrap,
        get_global_world,
        m1::get_global_hosts_view,
        catalog_api::list_host_deployments,
        catalog_api::get_deployment,
        catalog_api::list_deployment_candidates,
        catalog_api::list_technical_projects,
        catalog_api::create_technical_project,
        catalog_api::get_technical_project,
        catalog_api::update_technical_project,
        catalog_api::list_project_targets,
        catalog_api::create_project_target,
        catalog_api::update_project_target,
        catalog_api::list_businesses,
        catalog_api::create_business,
        catalog_api::get_business,
        catalog_api::update_business,
        catalog_api::list_business_project_links,
        catalog_api::create_business_project_link,
        catalog_api::delete_business_project_link,
        catalog_api::get_global_resources,
        project_agent::bind_project_agent,
        project_agent::get_project_agent,
        project_agent::invoke_tool,
        monitoring_api::get_host_monitoring,
        monitoring_api::create_monitor_run,
        monitoring_api::get_monitor_run,
        monitoring_history::get_host_metrics,
        monitoring_health::get_health_policy,
        monitoring_health::put_health_policy,
        monitoring_health::get_host_health,
        monitoring_scheduler::get_scheduler_status,
        monitoring_scheduler::list_monitor_schedules,
        monitoring_scheduler::create_monitor_schedule,
        monitoring_scheduler::update_monitor_schedule,
        get_project_resources,
        m1::create_secret_ref,
        m1::create_host,
        m1::list_hosts,
        m1::get_host,
        m1::update_host,
        m1::create_connection_test,
        m1::confirm_host_key,
        m1::create_discovery_run,
        m1::get_discovery_run,
        m1::get_discovery_evidence,
        projection::get_projection_draft,
        projection::update_projection_draft,
        projection::confirm_projection,
        projection::update_layout,
        projection::create_ignore_rule
        ,model_provider::get_model_provider
        ,model_provider::put_model_provider
        ,model_provider::test_model_provider
        ,discovery_diff::get_discovery_diff
        ,onboarding::create_onboarding_session
        ,onboarding::get_onboarding_session
        ,onboarding::get_discovery_proposal
        ,onboarding::send_onboarding_message
        ,data_management::export_workspace
        ,data_management::export_host
        ,data_management::export_project
        ,data_management::export_technical_project
        ,data_management::delete_workspace
        ,data_management::delete_host
        ,data_management::delete_project
    ),
    components(schemas(
        HealthResponse, BootstrapResponse, BootstrapData, ProjectSummary, HostSummary,
        NavigationCounts, FeatureAvailability, GraphSnapshotResponse, GraphSnapshot,
        GraphFocus, GraphScopeKind, GraphNode, GraphNodeKind, GraphEdge,
        GraphRelationKind, GraphPosition, GraphFact, GraphHealth, ProjectionState, CanvasLayout,
        ApiMeta, Freshness, DataSourceDescriptor, DataSourceKind, DataSourceStatus,
        TaskSummary, TaskState, ProjectionDraftSummary, LayoutUpdate, LayoutPosition,
        ApiErrorResponse, ApiErrorBody,
        SecretKind, SecretRefCreateRequest, SecretRefDescriptor, SecretRefResponse,
        HostKeyState, HostStatus, HostCreateRequest, HostUpdateRequest, HostRecord, HostResponse, HostListResponse,
        ConnectionTestState, ConnectionTestData, ConnectionTestResponse,
        HostKeyConfirmationRequest, HostKeyConfirmationResponse,
        DiscoveryRunCreateRequest, DiscoveryRunState, DiscoveryRunAccepted,
        DiscoveryRunAcceptedResponse, DiscoveryRunRecord, DiscoveryRunResponse,
        EvidenceKind, RedactionState, EvidenceItem, EvidenceHostIdentity, EvidenceWarning,
        DiscoveryEvidence, DiscoveryEvidenceResponse,
        DiscoveryProviderStatus, DiscoveryProviderCoverage, HostAssetRecord,
        GlobalHostsViewData, GlobalHostsViewResponse,
        HostMonitorProfile, MonitorRunCreateRequest, MonitorRunState, MonitorRunTrigger,
        MonitorMetricQuality,
        MonitorFreshness,
        MetricHistoryFamily, MetricHistoryRequestedResolution, MetricHistoryResolution,
        MetricHistorySampleKind, MetricHistorySubjectKind, MetricHistorySourceKind,
        MetricHistoryPoint, MetricHistoryRollupStatistics, MetricHistorySeries, MetricHistoryCursor,
        MetricHistoryCoverage, MetricHistoryData, MetricHistoryResponse,
        MonitorFamilyCoverage, HostCpuCoreMetric, HostCpuMetric, HostMemoryMetric,
        HostLoadMetric, HostFilesystemMetric, HostDiskIoMetric, HostNetworkMetric,
        HostUptimeMetric, HostProcessMetric, HostResourceSnapshot,
        MonitorRunAccepted, MonitorRunAcceptedResponse, MonitorRunRecord, MonitorRunResponse,
        HostMonitoringData, HostMonitoringResponse,
        MonitorScheduleState, MonitorScheduleCreateRequest, MonitorScheduleUpdateRequest,
        MonitorScheduleRecord, MonitorScheduleResponse, MonitorScheduleListData,
        MonitorScheduleListResponse, MonitoringSchedulerLimits, MonitoringSchedulerState,
        MonitoringSchedulerStateReason, MonitoringSchedulerStatus,
        MonitoringHistoryMaintenanceState, MonitoringHistoryRetentionPolicy,
        MonitoringHistoryMaintenanceStatus,
        MonitoringSchedulerStatusResponse,
        HealthRequirement, CpuBusySeriesKind, CpuBusyHealthRule, HighHealthRule,
        LowHealthRule, FilesystemCapacityHealthRule, HostHealthPolicyPutRequest,
        HostHealthPolicyConfigurationState, HostHealthPolicyRecord, HostHealthPolicyData,
        HostHealthPolicyResponse, HostHealthStatus, HealthConditionStatus,
        HealthObservationState, HostHealthCondition, HostHealthConditionCoverage,
        HostCurrentHealth, HostHealthWindow, HostHealthBandResolution,
        HostHealthTimeBucket, HostHealthTimeWindow, HostHealthData, HostHealthResponse,
        ProjectionDraftData, ProjectionDraftResponse, ProjectionVersionData,
        ProjectionVersionResponse, ProjectionPatchOperation, ProjectionDraftUpdateRequest,
        ProjectionConfirmRequest, LayoutUpdateRequest, LayoutResponse,
        IgnoreRuleAction, IgnoreRuleCreateRequest,
        ModelProviderPutRequest, ModelProviderData, ModelProviderResponse,
        ModelProviderTestState, ModelProviderTestData, ModelProviderTestResponse,
        OnboardingSessionCreateRequest, AgentConfidence, AgentProposalState, AgentProposal,
        AgentQuestionType, AgentQuestionState, AgentQuestion, OnboardingSessionState,
        OnboardingSessionData, OnboardingSessionResponse, OnboardingAction,
        OnboardingMessageRequest, DiscoveryChangeKind, DiscoveryDiffItem,
        DiscoveryDiffCounts, DiscoveryDiffData, DiscoveryDiffResponse
        ,LoginRequest, AuthSessionData, AuthSessionResponse
        ,DataExport, DataExportResponse, DeletionReceipt, DeletionResponse
        ,TechnicalProjectState, TechnicalProjectCreateRequest, TechnicalProjectUpdateRequest,
        TechnicalProjectRecord, TechnicalProjectResponse, TechnicalProjectListResponse,
        DeploymentProviderKind, DeploymentCatalogState, DeploymentObservationState,
        DeploymentRecord, DeploymentObservationRecord, DeploymentDetailData,
        DeploymentResponse, DeploymentListData, DeploymentListResponse,
        ProjectTargetAdapterKind, ProjectTargetCapability, ProjectTargetApprovalPolicy,
        ProjectTargetState, ProjectTargetCreateRequest, ProjectTargetUpdateRequest,
        ProjectTargetRecord, ProjectTargetResponse, ProjectTargetListData,
        ProjectTargetListResponse,
        BusinessState, BusinessOrigin, BusinessCreateRequest, BusinessUpdateRequest,
        BusinessRecord, BusinessResponse, BusinessListResponse,
        BusinessProjectLinkCreateRequest, BusinessProjectLinkState,
        BusinessProjectLinkRecord, BusinessProjectLinkResponse,
        BusinessProjectLinkListData, BusinessProjectLinkListResponse,
        GlobalResourceLens, GlobalResourceNodeKind, GlobalResourceNode,
        GlobalResourceEdgeKind, GlobalResourceEdge, GlobalResourceSummary,
        GlobalResourceFacet, GlobalResourceViewData, GlobalResourceViewResponse
        ,ProjectAgentState, ProjectAgentCapability, ProjectAgentToolName,
        ProjectAgentBindRequest, ProjectAgentRecord, ProjectAgentResponse,
        ProjectAgentToolRequest, ProjectAgentTargetsData,
        ProjectAgentDeploymentObservationData, ProjectAgentHostCapabilitiesData,
        ProjectAgentServiceStatusData, ProjectAgentRecentDiffData,
        ProjectAgentToolResult, ProjectAgentToolCallData
    )),
    tags(
        (name = "m0", description = "M0 visual contract and fixture-backed API"),
        (name = "m1", description = "M1 Linux SSH read-only discovery"),
        (name = "m2", description = "M2 deterministic visual management projection")
        ,(name = "m3", description = "M3 optional Agent assistance and discovery differences")
        ,(name = "m4", description = "M4 single-owner remote release boundary")
        ,(name = "monitoring", description = "Current HOST snapshots, persistent interval scheduling, and typed metric history")
        ,(name = "catalog", description = "Stable deployments and user-confirmed technical project targets")
        ,(name = "business", description = "User-declared Business membership and global resource relations")
        ,(name = "project-agent", description = "TechnicalProject-bound typed read-only Project Agent")
    )
)]
pub struct ApiDoc;

pub fn router(state: AppState, frontend: impl AsRef<Path>) -> Router {
    let protected = Router::new()
        .route("/auth/session", get(auth::get_session))
        .route("/auth/logout", post(auth::logout))
        .route("/events/stream", get(events::stream))
        .route("/bootstrap", get(get_bootstrap))
        .route("/views/global/world", get(get_global_world))
        .route("/views/global/hosts", get(m1::get_global_hosts_view))
        .route(
            "/hosts/{host_id}/deployments",
            get(catalog_api::list_host_deployments),
        )
        .route(
            "/deployments/{deployment_id}",
            get(catalog_api::get_deployment),
        )
        .route(
            "/deployment-candidates",
            get(catalog_api::list_deployment_candidates),
        )
        .route(
            "/technical-projects",
            get(catalog_api::list_technical_projects).post(catalog_api::create_technical_project),
        )
        .route(
            "/technical-projects/{technical_project_id}",
            get(catalog_api::get_technical_project).patch(catalog_api::update_technical_project),
        )
        .route(
            "/technical-projects/{technical_project_id}/targets",
            get(catalog_api::list_project_targets),
        )
        .route("/project-targets", post(catalog_api::create_project_target))
        .route(
            "/project-targets/{project_target_id}",
            axum::routing::patch(catalog_api::update_project_target),
        )
        .route(
            "/businesses",
            get(catalog_api::list_businesses).post(catalog_api::create_business),
        )
        .route(
            "/businesses/{business_id}",
            get(catalog_api::get_business).patch(catalog_api::update_business),
        )
        .route(
            "/businesses/{business_id}/project-links",
            get(catalog_api::list_business_project_links)
                .post(catalog_api::create_business_project_link),
        )
        .route(
            "/businesses/{business_id}/project-links/{technical_project_id}",
            axum::routing::delete(catalog_api::delete_business_project_link),
        )
        .route(
            "/views/global/resources",
            get(catalog_api::get_global_resources),
        )
        .route(
            "/technical-projects/{technical_project_id}/agent",
            get(project_agent::get_project_agent).post(project_agent::bind_project_agent),
        )
        .route(
            "/project-agents/{project_agent_id}/tools/{tool_name}",
            post(project_agent::invoke_tool),
        )
        .route(
            "/hosts/{host_id}/monitoring",
            get(monitoring_api::get_host_monitoring),
        )
        .route(
            "/hosts/{host_id}/metrics",
            get(monitoring_history::get_host_metrics),
        )
        .route(
            "/hosts/{host_id}/health-policy",
            get(monitoring_health::get_health_policy).put(monitoring_health::put_health_policy),
        )
        .route(
            "/hosts/{host_id}/health",
            get(monitoring_health::get_host_health),
        )
        .route(
            "/hosts/{host_id}/monitor-runs",
            post(monitoring_api::create_monitor_run),
        )
        .route(
            "/monitor-runs/{run_id}",
            get(monitoring_api::get_monitor_run),
        )
        .route(
            "/monitoring/status",
            get(monitoring_scheduler::get_scheduler_status),
        )
        .route(
            "/hosts/{host_id}/monitor-schedules",
            get(monitoring_scheduler::list_monitor_schedules)
                .post(monitoring_scheduler::create_monitor_schedule),
        )
        .route(
            "/monitor-schedules/{schedule_id}",
            axum::routing::patch(monitoring_scheduler::update_monitor_schedule),
        )
        .route(
            "/projects/{project_id}/views/resources",
            get(get_project_resources),
        )
        .route("/secret-refs", post(m1::create_secret_ref))
        .route("/hosts", post(m1::create_host).get(m1::list_hosts))
        .route("/hosts/{host_id}", get(m1::get_host).patch(m1::update_host))
        .route(
            "/hosts/{host_id}/connection-tests",
            post(m1::create_connection_test),
        )
        .route(
            "/hosts/{host_id}/host-key-confirmations",
            post(m1::confirm_host_key),
        )
        .route(
            "/hosts/{host_id}/discovery-runs",
            post(m1::create_discovery_run),
        )
        .route("/discovery-runs/{run_id}", get(m1::get_discovery_run))
        .route(
            "/discovery-runs/{run_id}/evidence",
            get(m1::get_discovery_evidence),
        )
        .route(
            "/discovery-runs/{run_id}/diff",
            get(discovery_diff::get_discovery_diff),
        )
        .route(
            "/discovery-runs/{run_id}/proposal",
            get(onboarding::get_discovery_proposal),
        )
        .route(
            "/projection-drafts/{draft_id}",
            get(projection::get_projection_draft).patch(projection::update_projection_draft),
        )
        .route(
            "/projection-drafts/{draft_id}/confirm",
            post(projection::confirm_projection),
        )
        .route(
            "/layouts/{layout_id}",
            axum::routing::patch(projection::update_layout),
        )
        .route("/ignore-rules", post(projection::create_ignore_rule))
        .route(
            "/model-provider",
            get(model_provider::get_model_provider).put(model_provider::put_model_provider),
        )
        .route(
            "/model-provider/test",
            post(model_provider::test_model_provider),
        )
        .route(
            "/onboarding-sessions",
            post(onboarding::create_onboarding_session),
        )
        .route(
            "/onboarding-sessions/{session_id}",
            get(onboarding::get_onboarding_session),
        )
        .route(
            "/onboarding-sessions/{session_id}/messages",
            post(onboarding::send_onboarding_message),
        )
        .route("/exports/workspace", get(data_management::export_workspace))
        .route("/hosts/{host_id}/export", get(data_management::export_host))
        .route(
            "/projects/{project_id}/export",
            get(data_management::export_project),
        )
        .route(
            "/technical-projects/{technical_project_id}/export",
            get(data_management::export_technical_project),
        )
        .route("/workspace", delete(data_management::delete_workspace))
        .route("/hosts/{host_id}", delete(data_management::delete_host))
        .route(
            "/projects/{project_id}",
            delete(data_management::delete_project),
        )
        .fallback(api_route_not_found)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::protect_business_api,
        ));
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .merge(protected);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/openapi.json", get(openapi_json))
        .nest("/api/v1", api)
        .fallback_service(ServeDir::new(frontend).append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(auth::add_security_headers))
        .with_state(state)
}

pub fn openapi() -> OpenApiDocument {
    ApiDoc::openapi()
}

pub fn export_openapi(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, openapi().to_pretty_json()?)?;
    Ok(())
}

async fn openapi_json() -> Json<OpenApiDocument> {
    Json(openapi())
}

#[utoipa::path(
    get,
    path = "/healthz",
    tag = "m0",
    responses(
        (status = 200, description = "Process and SQLite are healthy", body = HealthResponse),
        (status = 503, description = "SQLite is unavailable", body = ApiErrorResponse)
    )
)]
async fn healthz(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::storage)?;
    Ok(Json(HealthResponse {
        status: "ok".to_owned(),
        service: "network-atlas".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        build_revision: env!("NETWORK_ATLAS_BUILD_REVISION").to_owned(),
        executable_sha256: executable_sha256().unwrap_or_else(|| "unavailable".to_owned()),
    }))
}

fn executable_sha256() -> Option<String> {
    let executable = env::current_exe().ok()?;
    let bytes = fs::read(executable).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

#[utoipa::path(
    get,
    path = "/api/v1/bootstrap",
    tag = "m0",
    responses((status = 200, description = "Workspace navigation bootstrap", body = BootstrapResponse))
)]
async fn get_bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let response = projection::bootstrap_response(&state.pool, request_id(&headers))
        .await
        .map_err(AppError::Projection)?
        .ok_or(AppError::Projection(projection::ProjectionError::Internal))?;
    let revision = response.meta.revision;
    Ok(response_with_etag(response, revision).into_response())
}

#[utoipa::path(
    get,
    path = "/api/v1/views/global/world",
    tag = "m0",
    responses((status = 200, description = "Global graph snapshot", body = GraphSnapshotResponse))
)]
async fn get_global_world(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let response = projection::global_world_response(&state.pool, request_id(&headers))
        .await
        .map_err(AppError::Projection)?
        .ok_or(AppError::Projection(projection::ProjectionError::Internal))?;
    let revision = response.meta.revision;
    Ok(response_with_etag(response, revision).into_response())
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_id}/views/resources",
    tag = "m0",
    params(("project_id" = String, Path, description = "Local project identifier")),
    responses(
        (status = 200, description = "Project resource graph snapshot", body = GraphSnapshotResponse),
        (status = 404, description = "Project not found", body = ApiErrorResponse)
    )
)]
async fn get_project_resources(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Response, AppError> {
    let response =
        projection::project_resources_response(&state.pool, &project_id, request_id(&headers))
            .await
            .map_err(AppError::Projection)?;
    let response = response.ok_or_else(|| AppError::NotFound {
        resource: "project",
        id: project_id,
    })?;
    let revision = response.meta.revision;
    Ok(response_with_etag(response, revision).into_response())
}

async fn api_route_not_found(OriginalUri(uri): OriginalUri) -> AppError {
    AppError::NotFound {
        resource: "api_route",
        id: uri.path().to_owned(),
    }
}

fn response_with_etag<T>(body: T, revision: i64) -> impl IntoResponse
where
    T: serde::Serialize,
{
    let mut headers = HeaderMap::new();
    headers.insert(
        ETAG,
        HeaderValue::from_str(&format!("\"revision-{revision}\"")).expect("valid ETag"),
    );
    (headers, Json(body))
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

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{resource} {id} was not found")]
    NotFound { resource: &'static str, id: String },
    #[error("storage unavailable")]
    Storage(#[source] Arc<sqlx::Error>),
    #[error("projection read failed")]
    Projection(#[source] projection::ProjectionError),
}

impl AppError {
    fn storage(error: sqlx::Error) -> Self {
        Self::Storage(Arc::new(error))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        if let Self::Projection(error) = self {
            return error.into_response();
        }
        let (status, code, message, details) = match self {
            Self::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的本地投影对象不存在",
                json!({"resource": resource, "id": id}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "storage health check failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地数据存储暂不可用",
                    json!({}),
                )
            }
            Self::Projection(_) => unreachable!("projection errors return above"),
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
