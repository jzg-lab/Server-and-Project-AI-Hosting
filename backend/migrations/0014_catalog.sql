-- H4: stable deployment catalog, immutable discovery observations, and
-- user-confirmed TechnicalProject / ProjectTarget bindings. These tables are
-- additive sidecars; legacy Project and ProjectionDraft records remain intact.

CREATE TABLE technical_projects (
    technical_project_id TEXT PRIMARY KEY CHECK (length(technical_project_id) = 36),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 160),
    summary TEXT CHECK (summary IS NULL OR length(summary) <= 2000),
    state TEXT NOT NULL CHECK (state IN ('active', 'archived')),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_by TEXT NOT NULL CHECK (length(created_by) BETWEEN 1 AND 128),
    created_at TEXT NOT NULL,
    updated_by TEXT NOT NULL CHECK (length(updated_by) BETWEEN 1 AND 128),
    updated_at TEXT NOT NULL
);

CREATE INDEX technical_projects_workspace_state_idx
ON technical_projects(workspace_id, state, updated_at DESC);

CREATE TABLE deployments (
    deployment_id TEXT PRIMARY KEY CHECK (length(deployment_id) = 36),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    provider_kind TEXT NOT NULL CHECK (provider_kind IN ('docker', 'compose', 'systemd')),
    external_id TEXT NOT NULL CHECK (length(external_id) BETWEEN 1 AND 512),
    identity_key TEXT NOT NULL CHECK (length(identity_key) = 64),
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 512),
    catalog_state TEXT NOT NULL CHECK (catalog_state IN ('observed', 'unassigned', 'stale', 'ignored')),
    latest_observation_id TEXT,
    last_observed_at TEXT,
    last_observed_at_epoch_ms INTEGER CHECK (
        last_observed_at_epoch_ms IS NULL OR last_observed_at_epoch_ms >= 0
    ),
    freshness TEXT NOT NULL CHECK (freshness IN ('fresh', 'stale', 'unavailable')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(host_id, provider_kind, external_id)
);

CREATE INDEX deployments_host_catalog_idx
ON deployments(host_id, provider_kind, catalog_state, last_observed_at_epoch_ms DESC);

CREATE INDEX deployments_workspace_catalog_idx
ON deployments(workspace_id, catalog_state, updated_at DESC);

CREATE TABLE deployment_observations (
    deployment_observation_id TEXT PRIMARY KEY CHECK (length(deployment_observation_id) = 36),
    deployment_id TEXT NOT NULL REFERENCES deployments(deployment_id) ON DELETE CASCADE,
    discovery_run_id TEXT NOT NULL REFERENCES discovery_runs(run_id) ON DELETE CASCADE,
    provider_kind TEXT NOT NULL CHECK (provider_kind IN ('docker', 'compose', 'systemd')),
    external_id TEXT NOT NULL CHECK (length(external_id) BETWEEN 1 AND 512),
    observation_state TEXT NOT NULL CHECK (observation_state IN ('observed', 'missing', 'unknown')),
    provider_status TEXT CHECK (provider_status IN ('ready', 'unavailable', 'permission_denied', 'timed_out', 'failed')),
    observed_at TEXT,
    observed_at_epoch_ms INTEGER CHECK (observed_at_epoch_ms IS NULL OR observed_at_epoch_ms >= 0),
    evidence_refs_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(evidence_refs_json) AND json_type(evidence_refs_json) = 'array'),
    metadata_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(metadata_json) AND json_type(metadata_json) = 'object'),
    created_at TEXT NOT NULL,
    UNIQUE(deployment_id, discovery_run_id)
);

CREATE INDEX deployment_observations_deployment_time_idx
ON deployment_observations(deployment_id, observed_at_epoch_ms DESC, deployment_observation_id DESC);

CREATE INDEX deployment_observations_run_idx
ON deployment_observations(discovery_run_id, provider_kind);

CREATE TRIGGER deployment_observations_identity_insert
BEFORE INSERT ON deployment_observations
WHEN NOT EXISTS (
    SELECT 1 FROM deployments
    WHERE deployment_id = NEW.deployment_id
      AND provider_kind = NEW.provider_kind
      AND external_id = NEW.external_id
)
BEGIN
    SELECT RAISE(ABORT, 'deployment observation identity must match deployment');
END;

CREATE TRIGGER deployment_observations_reject_update
BEFORE UPDATE ON deployment_observations
BEGIN
    SELECT RAISE(ABORT, 'deployment observations are immutable');
END;

CREATE TABLE project_targets (
    project_target_id TEXT PRIMARY KEY CHECK (length(project_target_id) = 36),
    technical_project_id TEXT NOT NULL REFERENCES technical_projects(technical_project_id) ON DELETE CASCADE,
    deployment_id TEXT NOT NULL REFERENCES deployments(deployment_id) ON DELETE CASCADE,
    display_name TEXT CHECK (display_name IS NULL OR length(display_name) BETWEEN 1 AND 160),
    adapter_kind TEXT NOT NULL CHECK (adapter_kind = 'read_only'),
    capabilities_json TEXT NOT NULL CHECK (
        json_valid(capabilities_json)
        AND json_type(capabilities_json) = 'array'
        AND json_array_length(capabilities_json) = 1
        AND json_extract(capabilities_json, '$[0]') = 'read_only'
    ),
    approval_policy TEXT NOT NULL CHECK (approval_policy = 'read_only'),
    state TEXT NOT NULL CHECK (state IN ('confirmed', 'stale', 'archived')),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    confirmed_by TEXT NOT NULL CHECK (length(confirmed_by) BETWEEN 1 AND 128),
    confirmed_at TEXT NOT NULL,
    last_observed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(technical_project_id, deployment_id)
);

CREATE INDEX project_targets_project_state_idx
ON project_targets(technical_project_id, state, updated_at DESC);

CREATE INDEX project_targets_deployment_state_idx
ON project_targets(deployment_id, state, updated_at DESC);

CREATE TABLE catalog_mutation_requests (
    catalog_request_id TEXT PRIMARY KEY CHECK (length(catalog_request_id) = 36),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    resource_kind TEXT NOT NULL CHECK (length(resource_kind) BETWEEN 1 AND 128),
    resource_id TEXT NOT NULL CHECK (length(resource_id) BETWEEN 1 AND 512),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    request_sha256 TEXT NOT NULL CHECK (length(request_sha256) = 64),
    response_json TEXT NOT NULL CHECK (json_valid(response_json)),
    created_at TEXT NOT NULL,
    UNIQUE(workspace_id, resource_kind, resource_id, idempotency_key)
);

CREATE INDEX catalog_mutation_requests_lookup_idx
ON catalog_mutation_requests(workspace_id, resource_kind, resource_id, idempotency_key);
