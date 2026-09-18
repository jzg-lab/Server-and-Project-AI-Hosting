use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataSourceKind {
    Fixture,
    Real,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataSourceStatus {
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DataSourceDescriptor {
    pub kind: DataSourceKind,
    pub status: DataSourceStatus,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ApiMeta {
    pub request_id: String,
    pub revision: i64,
    pub generated_at: String,
    pub freshness: Freshness,
    pub data_source: DataSourceDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionState {
    Fixture,
    Discovered,
    Draft,
    Confirmed,
    Stale,
    Unavailable,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    Workspace,
    Host,
    Project,
    ComposeProject,
    Service,
    Container,
    Image,
    Network,
    Volume,
    Port,
    Document,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphRelationKind {
    Contains,
    Deploys,
    DependsOn,
    ConnectsTo,
    Mounts,
    Exposes,
    UsesImage,
    Documents,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GraphPosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GraphFact {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GraphHealth {
    pub label: String,
    pub tone: String,
    pub activity: u32,
    pub alerts: u32,
    pub updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub state: ProjectionState,
    pub source_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub position: GraphPosition,
    pub width: f64,
    pub height: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<GraphFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<GraphHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GraphEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: GraphRelationKind,
    pub label: String,
    pub state: ProjectionState,
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphScopeKind {
    Global,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GraphFocus {
    pub kind: GraphScopeKind,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct CanvasLayout {
    pub layout_id: String,
    pub scope: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GraphSnapshot {
    pub focus: GraphFocus,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub layout: CanvasLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectSummary {
    pub project_id: String,
    pub label: String,
    pub subtitle: String,
    pub state: ProjectionState,
    pub health: String,
    pub tone: String,
    pub activity: u32,
    pub alerts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostSummary {
    pub host_id: String,
    pub label: String,
    pub state: ProjectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<HostStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct NavigationCounts {
    pub projects: u32,
    pub healthy: u32,
    pub attention: u32,
    pub unassigned: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct FeatureAvailability {
    pub global_world: bool,
    pub project_resources: bool,
    pub global_resources: bool,
    pub project_workflow: bool,
    pub agent_assistance: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BootstrapData {
    pub workspace_id: String,
    pub projects: Vec<ProjectSummary>,
    pub hosts: Vec<HostSummary>,
    pub navigation: NavigationCounts,
    pub features: FeatureAvailability,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
    pub build_revision: String,
    pub executable_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BootstrapResponse {
    pub data: BootstrapData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GraphSnapshotResponse {
    pub data: GraphSnapshot,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Requested,
    Accepted,
    Running,
    Succeeded,
    Failed,
    NeedsConfirmation,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TaskSummary {
    pub request_id: String,
    pub task_id: String,
    pub kind: String,
    pub state: TaskState,
    pub submitted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectionDraftSummary {
    pub draft_id: String,
    pub discovery_run_id: String,
    pub base_revision: i64,
    pub state: ProjectionState,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct LayoutUpdate {
    pub layout_id: String,
    pub revision: i64,
    pub positions: Vec<LayoutPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct LayoutPosition {
    pub node_id: String,
    pub position: GraphPosition,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    SshKey,
    SshPassword,
    ModelKey,
}

#[derive(Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretRefCreateRequest {
    SshKey { private_key: String },
    SshPassword { password: String },
    ModelKey { api_key: String },
}

impl std::fmt::Debug for SecretRefCreateRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::SshKey { .. } => "ssh_key",
            Self::SshPassword { .. } => "ssh_password",
            Self::ModelKey { .. } => "model_key",
        };
        formatter
            .debug_struct("SecretRefCreateRequest")
            .field("kind", &kind)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct SecretRefDescriptor {
    pub credential_ref: String,
    pub kind: SecretKind,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct SecretRefResponse {
    pub data: SecretRefDescriptor,
    pub meta: ApiMeta,
}

#[cfg(test)]
mod secret_request_tests {
    use super::SecretRefCreateRequest;

    #[test]
    fn secret_request_debug_output_is_redacted() {
        let cases = [
            SecretRefCreateRequest::SshKey {
                private_key: "PRIVATE_KEY_FIXTURE".to_owned(),
            },
            SecretRefCreateRequest::SshPassword {
                password: "PASSWORD_FIXTURE".to_owned(),
            },
            SecretRefCreateRequest::ModelKey {
                api_key: "MODEL_KEY_FIXTURE".to_owned(),
            },
        ];

        for request in cases {
            let rendered = format!("{request:?}");
            assert!(rendered.contains("[REDACTED]"));
            assert!(!rendered.contains("FIXTURE"));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostKeyState {
    Unverified,
    Verified,
    Changed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostStatus {
    HostRegistered,
    FingerprintFetching,
    HostKeyUnverified,
    HostKeyVerified,
    HostKeyChanged,
    ConnectionChecking,
    ConnectionReady,
    DockerUnavailable,
    DockerPermissionDenied,
    DiscoveryRunning,
    EvidenceReady,
    DiscoveryComplete,
    DiscoveryPartial,
    DiscoveryUnavailable,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostCreateRequest {
    pub display_name: String,
    pub address: String,
    #[schema(minimum = 1, maximum = 65535)]
    pub port: u16,
    pub ssh_user: String,
    pub credential_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[schema(minimum = 1, maximum = 65535)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostRecord {
    pub host_id: String,
    pub display_name: String,
    pub address: String,
    pub port: u16,
    pub ssh_user: String,
    pub credential_kind: SecretKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key_fingerprint: Option<String>,
    pub host_key_state: HostKeyState,
    pub transport: String,
    pub os: String,
    pub status: HostStatus,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostResponse {
    pub data: HostRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionTestState {
    HostKeyUnverified,
    HostKeyVerified,
    HostKeyChanged,
    ConnectionReady,
    DockerUnavailable,
    DockerPermissionDenied,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthTransport {
    RusshClient,
    OpensshAskpass,
    OpensshKey,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ConnectionTestData {
    pub request_id: String,
    pub test_id: String,
    pub host_id: String,
    pub port: u16,
    pub ssh_user: String,
    pub credential_kind: SecretKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_transport: Option<SshAuthTransport>,
    pub state: ConnectionTestState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_fingerprint: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ConnectionTestResponse {
    pub data: ConnectionTestData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostKeyConfirmationRequest {
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryRunState {
    Accepted,
    Running,
    EvidenceReady,
    DiscoveryComplete,
    DiscoveryPartial,
    DiscoveryUnavailable,
    SshUnreachable,
    SshAuthFailed,
    PermissionDenied,
    DockerPermissionDenied,
    DockerUnavailable,
    ComposeUnavailable,
    DiscoveryTimeout,
    DocumentReadFailed,
    EvidenceConflict,
}

impl DiscoveryRunState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Accepted | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryRunAccepted {
    pub request_id: String,
    pub run_id: String,
    pub state: DiscoveryRunState,
    pub submitted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryRunAcceptedResponse {
    pub data: DiscoveryRunAccepted,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryRunRecord {
    pub request_id: String,
    pub run_id: String,
    pub host_id: String,
    pub protocol_version: String,
    pub state: DiscoveryRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_summary: Option<String>,
    pub submitted_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_id: Option<String>,
    pub evidence_item_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_sha256: Option<String>,
    pub evidence_retention: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryRunResponse {
    pub data: DiscoveryRunRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    HostIdentity,
    DockerEngine,
    ComposeProject,
    SystemdUnit,
    Container,
    Image,
    Network,
    Volume,
    Document,
    HealthCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionState {
    NotRequired,
    Redacted,
    MetadataOnly,
    Truncated,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct EvidenceItem {
    pub external_id: String,
    pub kind: EvidenceKind,
    pub source: String,
    pub observed_at: String,
    pub freshness: Freshness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub redaction_state: RedactionState,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct EvidenceHostIdentity {
    pub host_id: String,
    pub address: String,
    pub os: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct EvidenceWarning {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DiscoveryEvidence {
    pub protocol_version: String,
    pub discovery_id: String,
    pub host: EvidenceHostIdentity,
    pub host_facts: Vec<EvidenceItem>,
    pub docker_engines: Vec<EvidenceItem>,
    pub compose_projects: Vec<EvidenceItem>,
    #[serde(default)]
    pub systemd_units: Vec<EvidenceItem>,
    pub containers: Vec<EvidenceItem>,
    pub images: Vec<EvidenceItem>,
    pub networks: Vec<EvidenceItem>,
    pub volumes: Vec<EvidenceItem>,
    pub document_candidates: Vec<EvidenceItem>,
    pub health_checks: Vec<EvidenceItem>,
    pub warnings: Vec<EvidenceWarning>,
    #[serde(default)]
    pub provider_results: Vec<DiscoveryProviderCoverage>,
    pub started_at: String,
    pub finished_at: String,
}

impl DiscoveryEvidence {
    pub fn items(&self) -> impl Iterator<Item = &EvidenceItem> {
        self.host_facts
            .iter()
            .chain(&self.docker_engines)
            .chain(&self.compose_projects)
            .chain(&self.systemd_units)
            .chain(&self.containers)
            .chain(&self.images)
            .chain(&self.networks)
            .chain(&self.volumes)
            .chain(&self.document_candidates)
            .chain(&self.health_checks)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DiscoveryEvidenceResponse {
    pub data: DiscoveryEvidence,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryRunCreateRequest {
    /// Optional provider selection. An empty list keeps the legacy automatic selection.
    #[serde(default)]
    pub provider_kinds: Vec<String>,
    /// User-confirmed roots are bounded inputs for non-Docker providers.
    #[serde(default)]
    pub root_refs: Vec<String>,
    /// Capability hints are advisory; the server validates every adapter.
    #[serde(default)]
    pub requested_capabilities: Vec<String>,
}

#[cfg(test)]
mod discovery_request_tests {
    use super::DiscoveryRunCreateRequest;

    #[test]
    fn discovery_request_keeps_empty_body_compatibility_and_accepts_provider_inputs() {
        let legacy: DiscoveryRunCreateRequest = serde_json::from_str("{}").unwrap();
        assert!(legacy.provider_kinds.is_empty());
        assert!(legacy.root_refs.is_empty());
        assert!(legacy.requested_capabilities.is_empty());

        let request: DiscoveryRunCreateRequest = serde_json::from_str(
            r#"{
                "provider_kinds": ["docker", "systemd", "root"],
                "root_refs": ["/srv/app"],
                "requested_capabilities": ["read_only"]
            }"#,
        )
        .unwrap();
        assert_eq!(request.provider_kinds, ["docker", "systemd", "root"]);
        assert_eq!(request.root_refs, ["/srv/app"]);
        assert_eq!(request.requested_capabilities, ["read_only"]);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostKeyConfirmationResponse {
    pub data: HostRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostListResponse {
    pub data: Vec<HostRecord>,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryProviderStatus {
    Ready,
    Unavailable,
    PermissionDenied,
    TimedOut,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryProviderCoverage {
    pub provider_kind: String,
    pub status: DiscoveryProviderStatus,
    pub observed_count: u32,
    pub evidence_refs: Vec<String>,
    pub warnings: Vec<EvidenceWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostMonitorProfile {
    #[default]
    HostResourceV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MonitorRunCreateRequest {
    #[serde(default)]
    pub profile: HostMonitorProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitorRunTrigger {
    Manual,
    Scheduled,
    CatchUp,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitorScheduleState {
    Enabled,
    Paused,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MonitorScheduleCreateRequest {
    #[serde(default)]
    pub profile: HostMonitorProfile,
    pub interval_seconds: u32,
    pub jitter_seconds: u32,
    pub stale_after_seconds: u32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MonitorScheduleUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_after_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<MonitorScheduleState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitorScheduleRecord {
    pub schedule_id: String,
    pub host_id: String,
    pub profile: HostMonitorProfile,
    pub interval_seconds: u32,
    pub jitter_seconds: u32,
    pub jitter_offset_seconds: u32,
    pub stale_after_seconds: u32,
    pub state: MonitorScheduleState,
    pub next_due_at: Option<String>,
    pub last_due_at: Option<String>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitorScheduleResponse {
    pub data: MonitorScheduleRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitorScheduleListData {
    pub host_id: String,
    pub schedules: Vec<MonitorScheduleRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitorScheduleListResponse {
    pub data: MonitorScheduleListData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthRequirement {
    Required,
    Optional,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CpuBusySeriesKind {
    /// The approximately one-second, two-frame delta collected inside one
    /// host_resource_v1 SSH run. This is not a cross-run period average.
    CollectorWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CpuBusyHealthRule {
    pub requirement: HealthRequirement,
    pub series_kind: CpuBusySeriesKind,
    pub minimum_window_seconds: f64,
    pub warning_at_or_above: f64,
    pub critical_at_or_above: f64,
    pub recovery_below: f64,
    pub enter_count: u32,
    pub recover_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HighHealthRule {
    pub requirement: HealthRequirement,
    pub warning_at_or_above: f64,
    pub critical_at_or_above: f64,
    pub recovery_below: f64,
    pub enter_count: u32,
    pub recover_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LowHealthRule {
    pub requirement: HealthRequirement,
    pub warning_below: f64,
    pub critical_below: f64,
    pub recovery_at_or_above: f64,
    pub enter_count: u32,
    pub recover_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FilesystemCapacityHealthRule {
    pub mount: String,
    pub requirement: HealthRequirement,
    pub warning_at_or_above: f64,
    pub critical_at_or_above: f64,
    pub recovery_below: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_available_bytes_below: Option<u64>,
    pub enter_count: u32,
    pub recover_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostHealthPolicyPutRequest {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_busy: Option<CpuBusyHealthRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_available_ratio: Option<LowHealthRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized_load5: Option<HighHealthRule>,
    #[serde(default)]
    pub filesystems: Vec<FilesystemCapacityHealthRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostHealthPolicyConfigurationState {
    NotConfigured,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthPolicyRecord {
    pub policy_id: String,
    pub policy_version_id: String,
    pub host_id: String,
    pub revision: i64,
    pub enabled: bool,
    pub source_kind: String,
    pub cpu_busy: Option<CpuBusyHealthRule>,
    pub memory_available_ratio: Option<LowHealthRule>,
    pub normalized_load5: Option<HighHealthRule>,
    pub filesystems: Vec<FilesystemCapacityHealthRule>,
    pub policy_sha256: String,
    pub effective_from_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthPolicyData {
    pub host_id: String,
    pub configured: bool,
    pub state: HostHealthPolicyConfigurationState,
    pub policy: Option<HostHealthPolicyRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthPolicyResponse {
    pub data: HostHealthPolicyData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostHealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthConditionStatus {
    Ok,
    Warning,
    Critical,
    Unknown,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthObservationState {
    Complete,
    Partial,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthCondition {
    pub condition_key: String,
    pub condition_kind: String,
    pub requirement: HealthRequirement,
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_label: String,
    pub status: HealthConditionStatus,
    pub candidate_status: HealthConditionStatus,
    pub reason_code: String,
    pub value: Option<f64>,
    pub unit: String,
    pub window_seconds: Option<f64>,
    pub streak_count: u32,
    pub streak_required: u32,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostHealthConditionCoverage {
    pub required_total: u32,
    pub required_ok: u32,
    pub required_warning: u32,
    pub required_critical: u32,
    pub required_unknown: u32,
    pub optional_total: u32,
    pub optional_ok: u32,
    pub optional_warning: u32,
    pub optional_critical: u32,
    pub optional_unknown: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostCurrentHealth {
    pub host_id: String,
    pub health: HostHealthStatus,
    pub reason_code: String,
    pub freshness: MonitorFreshness,
    pub observation_state: HealthObservationState,
    pub evaluation_id: Option<String>,
    pub run_id: Option<String>,
    pub trigger_kind: Option<MonitorRunTrigger>,
    pub policy_id: Option<String>,
    pub policy_revision: Option<i64>,
    pub evaluated_at: String,
    pub observed_at: Option<String>,
    pub valid_until: Option<String>,
    pub coverage: HostHealthConditionCoverage,
    pub conditions: Vec<HostHealthCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostHealthWindow {
    #[serde(rename = "24h")]
    Hours24,
    #[serde(rename = "7d")]
    Days7,
    #[serde(rename = "30d")]
    Days30,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostHealthBandResolution {
    Hour,
    SixHour,
    Day,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthTimeBucket {
    pub bucket_start: String,
    pub bucket_end: String,
    pub effective_end: String,
    pub bucket_width_seconds: u32,
    pub is_partial: bool,
    pub count_basis: String,
    pub ok_count: u32,
    pub warning_count: u32,
    pub critical_count: u32,
    /// Includes explicit unknown evaluations and scheduled slots with no
    /// evaluation. `gap_count` identifies the latter subset.
    pub unknown_count: u32,
    pub expected_count: Option<u32>,
    pub evaluated_count: u32,
    pub observed_count: u32,
    pub gap_count: Option<u32>,
    pub late_observation_count: u32,
    pub coverage: Option<f64>,
    pub worst_status: Option<HealthConditionStatus>,
    pub reason_code: String,
    pub unknown_reason_counts: std::collections::BTreeMap<String, u32>,
    pub provenance_complete: bool,
    pub policy_revisions: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthTimeWindow {
    pub name: HostHealthWindow,
    pub actual_resolution: HostHealthBandResolution,
    pub from: String,
    pub to: String,
    pub bucket_width_seconds: u32,
    pub bucket_count: u32,
    pub count_basis: String,
    pub provenance_started_at: String,
    pub provenance_complete: bool,
    pub buckets: Vec<HostHealthTimeBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthData {
    pub host_id: String,
    pub evaluated_at: String,
    pub policy: HostHealthPolicyData,
    pub current: HostCurrentHealth,
    pub window: HostHealthTimeWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostHealthResponse {
    pub data: HostHealthData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitoringSchedulerLimits {
    pub minimum_interval_seconds: u32,
    pub maximum_interval_seconds: u32,
    pub maximum_stale_after_seconds: u32,
    pub maximum_concurrency: u32,
    pub tick_seconds: u32,
    pub late_after_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringSchedulerState {
    Running,
    Late,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringSchedulerStateReason {
    SchedulerNotStarted,
    AwaitingFirstSuccessfulTick,
    LatestTickFailed,
    SuccessfulTickLate,
    SuccessfulTickRecent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringHistoryMaintenanceState {
    Idle,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitoringHistoryRetentionPolicy {
    pub enabled: bool,
    pub raw_observed_days: u32,
    pub raw_non_observed_days: u32,
    pub hour_days: u32,
    pub day_days: u32,
    pub delete_batch_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitoringHistoryMaintenanceStatus {
    pub state: MonitoringHistoryMaintenanceState,
    pub runtime_started: bool,
    pub runtime_running: bool,
    pub last_started_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_successful_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error_code: Option<String>,
    pub consecutive_runtime_failures: u32,
    pub last_hour_partition_count: u32,
    pub last_day_partition_count: u32,
    pub last_raw_deleted_count: u32,
    pub last_hour_deleted_count: u32,
    pub last_day_deleted_count: u32,
    pub raw_sample_count: u32,
    pub hour_rollup_count: u32,
    pub day_rollup_count: u32,
    pub database_bytes: u64,
    pub tick_seconds: u32,
    pub partitions_per_tick: u32,
    pub retention: MonitoringHistoryRetentionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitoringSchedulerStatus {
    pub state: MonitoringSchedulerState,
    pub state_reason: MonitoringSchedulerStateReason,
    pub active_schedule_count: u32,
    pub due_schedule_count: u32,
    pub queued_run_count: u32,
    pub running_run_count: u32,
    pub oldest_due_at: Option<String>,
    pub last_tick_at: Option<String>,
    pub last_tick_error_at: Option<String>,
    pub last_tick_error_code: Option<String>,
    pub consecutive_tick_failures: u32,
    pub limits: MonitoringSchedulerLimits,
    pub history_maintenance: MonitoringHistoryMaintenanceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitoringSchedulerStatusResponse {
    pub data: MonitoringSchedulerStatus,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitorRunState {
    Queued,
    Running,
    Succeeded,
    Partial,
    Failed,
    TimedOut,
    SkippedOverlap,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMetricQuality {
    Observed,
    Unsupported,
    ParseFailed,
    CounterReset,
    CounterUnreliable,
    InsufficientInterval,
    PermissionDenied,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitorFreshness {
    Unknown,
    Fresh,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricHistoryFamily {
    Cpu,
    Memory,
    Load,
    DiskCapacity,
    DiskIo,
    Network,
    Uptime,
    Process,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricHistoryRequestedResolution {
    Auto,
    Raw,
    Hour,
    Day,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricHistoryResolution {
    Raw,
    Hour,
    Day,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MetricHistoryRollupStatistics {
    pub resolution: MetricHistoryResolution,
    pub bucket_start: String,
    pub bucket_end: String,
    pub bucket_width_seconds: u32,
    pub sample_count: u32,
    pub observed_count: u32,
    pub non_observed_count: u32,
    pub expected_count: Option<u32>,
    pub missing_count: Option<u32>,
    pub reset_count: u32,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub average: Option<f64>,
    pub p95: Option<f64>,
    pub last: Option<f64>,
    /// Exact decimal counter delta. Gauge and derived rollups leave this null.
    pub counter_delta_integer: Option<String>,
    pub quality_counts: Value,
    pub input_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricHistorySampleKind {
    Gauge,
    Counter,
    Derived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricHistorySubjectKind {
    Host,
    Cpu,
    Filesystem,
    BlockDevice,
    Interface,
    Process,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricHistorySourceKind {
    SshHostResourceV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MetricHistoryPoint {
    pub sample_id: String,
    pub run_id: String,
    pub at: String,
    pub at_epoch_ms: i64,
    pub value: Option<f64>,
    /// Exact decimal representation for INTEGER counter samples. `value`
    /// remains available for charting, but may round values above JavaScript's
    /// safe-integer range.
    pub value_integer: Option<String>,
    pub quality: MonitorMetricQuality,
    pub window_seconds: Option<f64>,
    /// Present only for compacted hour/day points. Raw point JSON remains
    /// backward-compatible and omits this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollup: Option<MetricHistoryRollupStatistics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MetricHistorySeries {
    pub family: MetricHistoryFamily,
    pub subject_kind: MetricHistorySubjectKind,
    pub subject_id: String,
    pub metric_name: String,
    pub dimensions: Value,
    pub sample_kind: MetricHistorySampleKind,
    pub source_kind: MetricHistorySourceKind,
    pub unit: String,
    pub points: Vec<MetricHistoryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MetricHistoryCoverage {
    pub expected_count: Option<u32>,
    pub observed_count: u32,
    pub gap_count: Option<u32>,
    pub coverage: Option<f64>,
    pub provenance_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MetricHistoryCursor {
    #[schema(minimum = 0)]
    pub after_epoch_ms: i64,
    #[schema(min_length = 36, max_length = 36)]
    pub after_sample_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MetricHistoryData {
    pub host_id: String,
    pub from: String,
    pub to: String,
    pub requested_resolution: MetricHistoryRequestedResolution,
    pub actual_resolution: MetricHistoryResolution,
    pub retention_tier: String,
    pub history_started_at: String,
    pub latest_observation_at: Option<String>,
    pub latest_valid_sample_at: Option<String>,
    pub latest_valid_until: Option<String>,
    pub freshness: MonitorFreshness,
    pub coverage: MetricHistoryCoverage,
    pub series: Vec<MetricHistorySeries>,
    #[schema(minimum = 1, maximum = 5000)]
    pub limit: u32,
    pub has_more: bool,
    pub next_cursor: Option<MetricHistoryCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MetricHistoryResponse {
    pub data: MetricHistoryData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MonitorFamilyCoverage {
    pub family: String,
    pub required: bool,
    pub quality: MonitorMetricQuality,
    pub observed_item_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostCpuCoreMetric {
    pub cpu: String,
    pub quality: MonitorMetricQuality,
    pub busy_percent: Option<f64>,
    pub iowait_percent: Option<f64>,
    pub steal_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostCpuMetric {
    pub quality: MonitorMetricQuality,
    pub busy_percent: Option<f64>,
    pub iowait_percent: Option<f64>,
    pub steal_percent: Option<f64>,
    pub online_cpu_count: Option<u32>,
    pub window_seconds: Option<f64>,
    pub per_cpu: Vec<HostCpuCoreMetric>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostMemoryMetric {
    pub quality: MonitorMetricQuality,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub swap_free_bytes: Option<u64>,
    pub cached_bytes: Option<u64>,
    pub buffers_bytes: Option<u64>,
    pub slab_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostLoadMetric {
    pub quality: MonitorMetricQuality,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub normalized_load1: Option<f64>,
    pub normalized_load5: Option<f64>,
    pub normalized_load15: Option<f64>,
    pub runnable_entities: Option<u64>,
    pub total_scheduling_entities: Option<u64>,
    pub online_cpu_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostFilesystemMetric {
    pub mount: String,
    pub size_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub allocatable_used_ratio: Option<f64>,
    pub inode_total: Option<u64>,
    pub inode_used: Option<u64>,
    pub inode_available: Option<u64>,
    pub inode_used_ratio: Option<f64>,
    pub inode_quality: MonitorMetricQuality,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostDiskIoMetric {
    pub identity: String,
    pub name: String,
    pub major: u32,
    pub minor: u32,
    pub quality: MonitorMetricQuality,
    pub window_seconds: f64,
    pub read_bytes_per_second: Option<f64>,
    pub write_bytes_per_second: Option<f64>,
    pub iops: Option<f64>,
    pub read_await_ms: Option<f64>,
    pub write_await_ms: Option<f64>,
    pub util_percent: Option<f64>,
    pub average_queue_depth: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostNetworkMetric {
    pub identity: String,
    pub name: String,
    pub ifindex: u32,
    pub iflink: Option<u32>,
    pub operstate: String,
    pub speed_mbps: Option<u64>,
    pub quality: MonitorMetricQuality,
    pub window_seconds: f64,
    pub rx_bytes_per_second: Option<f64>,
    pub tx_bytes_per_second: Option<f64>,
    pub rx_packets_per_second: Option<f64>,
    pub tx_packets_per_second: Option<f64>,
    pub rx_error_drop_percent: Option<f64>,
    pub tx_error_drop_percent: Option<f64>,
    pub rx_util_percent: Option<f64>,
    pub tx_util_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostUptimeMetric {
    pub quality: MonitorMetricQuality,
    pub boot_id: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub idle_seconds: Option<f64>,
    pub rebooted_during_sample: Option<bool>,
    pub window_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct HostProcessMetric {
    pub quality: MonitorMetricQuality,
    pub scanned: Option<u32>,
    pub running: Option<u32>,
    pub blocked: Option<u32>,
    pub zombie: Option<u32>,
    pub raced: Option<u32>,
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostResourceSnapshot {
    pub observation_id: String,
    pub run_id: String,
    pub host_id: String,
    pub profile: HostMonitorProfile,
    pub collector_version: String,
    pub observed_at: String,
    pub valid_until: String,
    pub freshness: MonitorFreshness,
    pub retention_tier: String,
    pub ssh_session_count: u32,
    pub coverage: Vec<MonitorFamilyCoverage>,
    pub metric_count: u32,
    pub unknown_count: u32,
    pub cpu: HostCpuMetric,
    pub memory: HostMemoryMetric,
    pub load: HostLoadMetric,
    pub filesystems: Vec<HostFilesystemMetric>,
    pub disk_io: Vec<HostDiskIoMetric>,
    pub network: Vec<HostNetworkMetric>,
    pub uptime: HostUptimeMetric,
    pub process: HostProcessMetric,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MonitorRunAccepted {
    pub request_id: String,
    pub run_id: String,
    pub host_id: String,
    pub profile: HostMonitorProfile,
    pub state: MonitorRunState,
    pub submitted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MonitorRunAcceptedResponse {
    pub data: MonitorRunAccepted,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MonitorRunRecord {
    pub request_id: String,
    pub run_id: String,
    pub host_id: String,
    pub profile: HostMonitorProfile,
    pub trigger_kind: MonitorRunTrigger,
    pub state: MonitorRunState,
    pub schedule_id: Option<String>,
    pub schedule_revision: Option<i64>,
    pub scheduled_for: Option<String>,
    pub stale_after_seconds: u32,
    pub due_interval_seconds: Option<u32>,
    pub missed_due_count: Option<u32>,
    pub collector_version: Option<String>,
    pub boot_id: Option<String>,
    pub coverage: Vec<MonitorFamilyCoverage>,
    pub output_bytes: u64,
    pub ssh_session_count: u32,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
    pub submitted_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct MonitorRunResponse {
    pub data: MonitorRunRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostMonitoringData {
    pub host_id: String,
    pub latest_run: Option<MonitorRunRecord>,
    pub current_snapshot: Option<HostResourceSnapshot>,
    pub monitor_freshness: MonitorFreshness,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostMonitoringResponse {
    pub data: HostMonitoringData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct HostAssetRecord {
    pub host: HostRecord,
    pub connection_state: HostStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_state: Option<DiscoveryRunState>,
    pub latest_discovery_run_id: Option<String>,
    pub latest_evidence_run_id: Option<String>,
    pub latest_projection_draft_id: Option<String>,
    pub latest_monitor_run: Option<MonitorRunRecord>,
    pub current_snapshot_run_id: Option<String>,
    pub monitor_observed_at: Option<String>,
    pub monitor_freshness: MonitorFreshness,
    pub monitor_unknown_count: u32,
    pub monitor_schedule_state: Option<MonitorScheduleState>,
    pub monitor_schedule_next_due_at: Option<String>,
    pub monitor_schedule_last_due_at: Option<String>,
    pub provider_coverage: Vec<DiscoveryProviderCoverage>,
    pub deployment_count: u32,
    pub project_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_at: Option<String>,
    pub freshness: Freshness,
    pub attention_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GlobalHostsViewData {
    pub host_count: u32,
    pub connection_ready_count: u32,
    pub connection_failed_count: u32,
    pub discovery_partial_count: u32,
    pub stale_evidence_count: u32,
    pub unknown_count: u32,
    pub hosts: Vec<HostAssetRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GlobalHostsViewResponse {
    pub data: GlobalHostsViewData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectionDraftData {
    pub draft_id: String,
    pub discovery_run_id: String,
    pub host_id: String,
    pub base_revision: i64,
    pub revision: i64,
    pub state: ProjectionState,
    pub pending_changes: u32,
    pub updated_at: String,
    #[serde(flatten)]
    pub snapshot: GraphSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectionDraftResponse {
    pub data: ProjectionDraftData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectionVersionData {
    pub version_id: String,
    pub draft_id: String,
    pub host_id: String,
    pub revision: i64,
    pub confirmed_at: String,
    #[serde(flatten)]
    pub snapshot: GraphSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectionVersionResponse {
    pub data: ProjectionVersionData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProjectionPatchOperation {
    Rename {
        node_id: String,
        label: String,
    },
    Move {
        node_id: String,
        position: GraphPosition,
    },
    AssignProject {
        node_id: String,
        project_id: Option<String>,
    },
    CreateProject {
        project_id: String,
        label: String,
        subtitle: String,
    },
    AddRelation {
        from: String,
        to: String,
        kind: GraphRelationKind,
        label: String,
    },
    RemoveRelation {
        edge_id: String,
    },
    ArchiveNode {
        node_id: String,
    },
    RestoreNode {
        node_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectionDraftUpdateRequest {
    pub base_revision: i64,
    pub operations: Vec<ProjectionPatchOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectionConfirmRequest {
    pub base_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct LayoutUpdateRequest {
    pub base_revision: i64,
    pub positions: Vec<LayoutPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreRuleAction {
    Ignore,
    Archive,
    Restore,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct IgnoreRuleCreateRequest {
    pub draft_id: String,
    pub node_id: String,
    pub action: IgnoreRuleAction,
    pub base_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct LayoutResponse {
    pub data: LayoutUpdate,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ModelProviderPutRequest {
    pub base_url: String,
    pub model: String,
    pub credential_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ModelProviderData {
    pub base_url: String,
    pub model: String,
    pub key_state: String,
    pub revision: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ModelProviderResponse {
    pub data: ModelProviderData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderTestState {
    Reachable,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ModelProviderTestData {
    pub test_id: String,
    pub state: ModelProviderTestState,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ModelProviderTestResponse {
    pub data: ModelProviderTestData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct OnboardingSessionCreateRequest {
    pub draft_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProposalState {
    Pending,
    Adopted,
    Modified,
    Rejected,
    Undone,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct AgentProposal {
    pub proposal_id: String,
    pub title: String,
    pub reason: String,
    pub confidence: AgentConfidence,
    pub evidence_refs: Vec<String>,
    pub requires_user_confirmation: bool,
    pub patch: Vec<ProjectionPatchOperation>,
    pub state: AgentProposalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentQuestionType {
    Choice,
    Text,
    Confirm,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentQuestionState {
    Pending,
    Answered,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct AgentQuestion {
    pub question_id: String,
    pub kind: AgentQuestionType,
    pub prompt: String,
    pub options: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub blocking: bool,
    pub state: AgentQuestionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingSessionState {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct OnboardingSessionData {
    pub session_id: String,
    pub draft_id: String,
    pub discovery_run_id: String,
    pub state: OnboardingSessionState,
    pub facts_used: Vec<String>,
    pub proposals: Vec<AgentProposal>,
    pub questions: Vec<AgentQuestion>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct OnboardingSessionResponse {
    pub data: OnboardingSessionData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingAction {
    Adopt,
    Modify,
    Reject,
    Undo,
    Answer,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct OnboardingMessageRequest {
    pub action: OnboardingAction,
    #[serde(default)]
    pub proposal_id: Option<String>,
    #[serde(default)]
    pub question_id: Option<String>,
    #[serde(default)]
    pub answer: Option<serde_json::Value>,
    #[serde(default)]
    pub operations: Option<Vec<ProjectionPatchOperation>>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub base_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryChangeKind {
    Added,
    Changed,
    Missing,
    Conflict,
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryDiffItem {
    pub entity_key: String,
    pub evidence_kind: EvidenceKind,
    pub external_id: String,
    pub change: DiscoveryChangeKind,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_sha256: Option<String>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, Default)]
pub struct DiscoveryDiffCounts {
    pub added: u32,
    pub changed: u32,
    pub missing: u32,
    pub conflict: u32,
    pub unchanged: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryDiffData {
    pub diff_id: String,
    pub run_id: String,
    pub host_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_run_id: Option<String>,
    pub counts: DiscoveryDiffCounts,
    pub items: Vec<DiscoveryDiffItem>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DiscoveryDiffResponse {
    pub data: DiscoveryDiffData,
    pub meta: ApiMeta,
}

// H4 catalog contracts. These records are additive to the legacy Project /
// ProjectionDraft contract and intentionally keep observation provenance
// separate from user-owned project bindings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TechnicalProjectState {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TechnicalProjectCreateRequest {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TechnicalProjectUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<TechnicalProjectState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TechnicalProjectRecord {
    pub technical_project_id: String,
    pub workspace_id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub state: TechnicalProjectState,
    pub revision: i64,
    pub created_by: String,
    pub created_at: String,
    pub updated_by: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TechnicalProjectResponse {
    pub data: TechnicalProjectRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TechnicalProjectListResponse {
    pub data: Vec<TechnicalProjectRecord>,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentProviderKind {
    Docker,
    Compose,
    Systemd,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentCatalogState {
    Observed,
    Unassigned,
    Stale,
    Ignored,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentObservationState {
    Observed,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DeploymentRecord {
    pub deployment_id: String,
    pub workspace_id: String,
    pub host_id: String,
    pub provider_kind: DeploymentProviderKind,
    pub external_id: String,
    pub identity_key: String,
    pub display_name: String,
    pub state: DeploymentCatalogState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_at_epoch_ms: Option<i64>,
    pub freshness: Freshness,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DeploymentObservationRecord {
    pub deployment_observation_id: String,
    pub deployment_id: String,
    pub discovery_run_id: String,
    pub provider_kind: DeploymentProviderKind,
    pub external_id: String,
    pub observation_state: DeploymentObservationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_status: Option<DiscoveryProviderStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at_epoch_ms: Option<i64>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub metadata: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DeploymentDetailData {
    pub deployment: DeploymentRecord,
    pub observations: Vec<DeploymentObservationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DeploymentResponse {
    pub data: DeploymentDetailData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DeploymentListData {
    pub host_id: String,
    pub deployments: Vec<DeploymentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DeploymentListResponse {
    pub data: DeploymentListData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTargetAdapterKind {
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTargetCapability {
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTargetApprovalPolicy {
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTargetState {
    Confirmed,
    Stale,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectTargetCreateRequest {
    pub technical_project_id: String,
    pub deployment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectTargetUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<ProjectTargetState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectTargetRecord {
    pub project_target_id: String,
    pub technical_project_id: String,
    pub deployment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub adapter_kind: ProjectTargetAdapterKind,
    pub capabilities: Vec<ProjectTargetCapability>,
    pub approval_policy: ProjectTargetApprovalPolicy,
    pub state: ProjectTargetState,
    pub revision: i64,
    pub confirmed_by: String,
    pub confirmed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectTargetResponse {
    pub data: ProjectTargetRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectTargetListData {
    pub technical_project_id: String,
    pub targets: Vec<ProjectTargetRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectTargetListResponse {
    pub data: ProjectTargetListData,
    pub meta: ApiMeta,
}

// H5 Business and global resource read-model contracts.  Business membership
// is user-declared; resource graph edges are never inferred from names or host
// co-location.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BusinessState {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BusinessOrigin {
    UserDeclared,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessCreateRequest {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub initial_project_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<BusinessState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessRecord {
    pub business_id: String,
    pub workspace_id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub state: BusinessState,
    pub origin: BusinessOrigin,
    pub revision: i64,
    pub created_by: String,
    pub created_at: String,
    pub updated_by: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessResponse {
    pub data: BusinessRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessListResponse {
    pub data: Vec<BusinessRecord>,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessProjectLinkCreateRequest {
    pub technical_project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BusinessProjectLinkState {
    Confirmed,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessProjectLinkRecord {
    pub business_project_link_id: String,
    pub business_id: String,
    pub technical_project_id: String,
    pub state: BusinessProjectLinkState,
    pub origin: BusinessOrigin,
    pub revision: i64,
    pub confirmed_by: String,
    pub confirmed_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessProjectLinkResponse {
    pub data: BusinessProjectLinkRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessProjectLinkListData {
    pub business_id: String,
    pub links: Vec<BusinessProjectLinkRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct BusinessProjectLinkListResponse {
    pub data: BusinessProjectLinkListData,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GlobalResourceLens {
    Topology,
    Shared,
    Impact,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GlobalResourceNodeKind {
    Host,
    TechnicalProject,
    Deployment,
    Resource,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GlobalResourceNode {
    pub node_id: String,
    pub kind: GlobalResourceNodeKind,
    pub display_name: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub technical_project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<DeploymentProviderKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GlobalResourceEdgeKind {
    ProjectTargetsDeployment,
    DeploymentRunsOnHost,
    DeploymentUsesResource,
    ProjectReferencesResource,
    ProjectImpactedByResource,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GlobalResourceEdge {
    pub edge_id: String,
    pub kind: GlobalResourceEdgeKind,
    pub from_node_id: String,
    pub to_node_id: String,
    pub state: String,
    pub origin: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub path_refs: Vec<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GlobalResourceSummary {
    pub host_count: i64,
    pub technical_project_count: i64,
    pub deployment_count: i64,
    pub resource_count: i64,
    pub shared_resource_count: i64,
    pub unassigned_deployment_count: i64,
    pub hidden_node_count: i64,
    pub hidden_edge_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GlobalResourceFacet {
    pub key: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GlobalResourceViewData {
    pub schema_version: String,
    pub lens: GlobalResourceLens,
    pub summary: GlobalResourceSummary,
    pub nodes: Vec<GlobalResourceNode>,
    pub edges: Vec<GlobalResourceEdge>,
    #[serde(default)]
    pub facets: Vec<GlobalResourceFacet>,
    pub hidden_counts: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GlobalResourceViewResponse {
    pub data: GlobalResourceViewData,
    pub meta: ApiMeta,
}

// H6 Project Agent contracts.  The binding is a logical responsibility
// identity; the only capability exposed in this phase is typed read-only
// observation of the owning TechnicalProject.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAgentState {
    Active,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAgentCapability {
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAgentToolName {
    ListProjectTargets,
    ReadDeploymentObservation,
    ReadHostCapabilities,
    ReadServiceStatus,
    ReadRecentDiff,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectAgentBindRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectAgentRecord {
    pub project_agent_id: String,
    pub workspace_id: String,
    pub technical_project_id: String,
    pub display_name: String,
    pub state: ProjectAgentState,
    pub capabilities: Vec<ProjectAgentCapability>,
    pub tool_names: Vec<ProjectAgentToolName>,
    pub revision: i64,
    pub created_by: String,
    pub created_at: String,
    pub updated_by: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectAgentResponse {
    pub data: ProjectAgentRecord,
    pub meta: ApiMeta,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProjectAgentToolRequest {
    /// Every target-scoped tool must receive this stable binding explicitly.
    /// `list_project_targets` is the sole project-scoped exception.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_target_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentTargetsData {
    pub technical_project: TechnicalProjectRecord,
    pub targets: Vec<ProjectTargetRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentDeploymentObservationData {
    pub deployment: DeploymentRecord,
    pub observations: Vec<DeploymentObservationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentHostCapabilitiesData {
    pub host_id: String,
    pub display_name: String,
    pub os: String,
    pub transport: String,
    pub status: HostStatus,
    pub provider_coverage: Vec<DiscoveryProviderCoverage>,
    pub latest_discovery_run_id: Option<String>,
    pub latest_discovery_state: Option<DiscoveryRunState>,
    pub monitor_freshness: MonitorFreshness,
    pub monitor_observed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentServiceStatusData {
    pub project_target_id: String,
    pub deployment: DeploymentRecord,
    pub observation_state: Option<DeploymentObservationState>,
    pub provider_status: Option<DiscoveryProviderStatus>,
    pub observed_at: Option<String>,
    pub freshness: Freshness,
    pub evidence_refs: Vec<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentRecentDiffData {
    pub project_target_id: String,
    pub host_id: String,
    pub diff: Option<DiscoveryDiffData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ProjectAgentToolResult {
    ProjectTargets(ProjectAgentTargetsData),
    DeploymentObservation(ProjectAgentDeploymentObservationData),
    HostCapabilities(ProjectAgentHostCapabilitiesData),
    ServiceStatus(ProjectAgentServiceStatusData),
    RecentDiff(ProjectAgentRecentDiffData),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentToolCallData {
    pub tool_call_id: String,
    pub project_agent_id: String,
    pub technical_project_id: String,
    pub project_target_id: Option<String>,
    pub tool_name: ProjectAgentToolName,
    pub result: ProjectAgentToolResult,
    pub evidence_refs: Vec<String>,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAgentToolResponse {
    pub data: ProjectAgentToolCallData,
    pub meta: ApiMeta,
}

#[cfg(test)]
mod catalog_contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_enums_use_stable_snake_case_values() {
        assert_eq!(
            serde_json::to_value(DeploymentProviderKind::Systemd).unwrap(),
            json!("systemd")
        );
        assert_eq!(
            serde_json::to_value(DeploymentCatalogState::Unassigned).unwrap(),
            json!("unassigned")
        );
        assert_eq!(
            serde_json::to_value(ProjectTargetCapability::ReadOnly).unwrap(),
            json!("read_only")
        );
    }

    #[test]
    fn project_target_contract_does_not_duplicate_host_identity() {
        let value = serde_json::to_value(ProjectTargetRecord {
            project_target_id: "target-1".to_owned(),
            technical_project_id: "project-1".to_owned(),
            deployment_id: "deployment-1".to_owned(),
            display_name: None,
            adapter_kind: ProjectTargetAdapterKind::ReadOnly,
            capabilities: vec![ProjectTargetCapability::ReadOnly],
            approval_policy: ProjectTargetApprovalPolicy::ReadOnly,
            state: ProjectTargetState::Confirmed,
            revision: 1,
            confirmed_by: "user".to_owned(),
            confirmed_at: "2026-08-15T00:00:00Z".to_owned(),
            last_observed_at: None,
            created_at: "2026-08-15T00:00:00Z".to_owned(),
            updated_at: "2026-08-15T00:00:00Z".to_owned(),
        })
        .unwrap();
        assert!(value.get("host_id").is_none());
        assert_eq!(value["capabilities"], json!(["read_only"]));
    }
}
