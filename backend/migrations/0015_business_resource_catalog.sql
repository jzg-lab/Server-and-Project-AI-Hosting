-- H5: user-declared Business membership and explicit resource relationship
-- records.  These tables are additive to the H4 catalog and are intentionally
-- independent from the legacy Project/Entity projection tables.

CREATE TABLE businesses (
    business_id TEXT PRIMARY KEY CHECK (length(business_id) = 36),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 120),
    summary TEXT CHECK (summary IS NULL OR length(summary) <= 500),
    state TEXT NOT NULL CHECK (state IN ('active', 'archived')),
    origin TEXT NOT NULL CHECK (origin = 'user_declared'),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_by TEXT NOT NULL CHECK (length(created_by) BETWEEN 1 AND 128),
    created_at TEXT NOT NULL,
    updated_by TEXT NOT NULL CHECK (length(updated_by) BETWEEN 1 AND 128),
    updated_at TEXT NOT NULL
);

CREATE INDEX businesses_workspace_state_idx
ON businesses(workspace_id, state, updated_at DESC);

CREATE TABLE business_project_links (
    business_project_link_id TEXT PRIMARY KEY CHECK (length(business_project_link_id) = 36),
    business_id TEXT NOT NULL REFERENCES businesses(business_id) ON DELETE CASCADE,
    technical_project_id TEXT NOT NULL REFERENCES technical_projects(technical_project_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('confirmed', 'archived')),
    origin TEXT NOT NULL CHECK (origin = 'user_declared'),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    confirmed_by TEXT NOT NULL CHECK (length(confirmed_by) BETWEEN 1 AND 128),
    confirmed_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(business_id, technical_project_id)
);

CREATE INDEX business_project_links_business_state_idx
ON business_project_links(business_id, state, updated_at DESC);

CREATE INDEX business_project_links_project_state_idx
ON business_project_links(technical_project_id, state, updated_at DESC);

-- ResourceEntity is deliberately generic: provider-specific fields stay in
-- metadata_json while source + external_id remains the stable identity.
CREATE TABLE resource_entities (
    resource_entity_id TEXT PRIMARY KEY CHECK (length(resource_entity_id) = 36),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    resource_kind TEXT NOT NULL CHECK (length(trim(resource_kind)) BETWEEN 1 AND 64),
    source TEXT NOT NULL CHECK (length(trim(source)) BETWEEN 1 AND 128),
    external_id TEXT NOT NULL CHECK (length(trim(external_id)) BETWEEN 1 AND 512),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 512),
    freshness TEXT NOT NULL CHECK (freshness IN ('fresh', 'stale', 'unavailable')),
    metadata_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(metadata_json) AND json_type(metadata_json) = 'object'),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(workspace_id, source, external_id)
);

CREATE INDEX resource_entities_workspace_kind_idx
ON resource_entities(workspace_id, resource_kind, updated_at DESC);

CREATE TABLE deployment_resource_links (
    deployment_resource_link_id TEXT PRIMARY KEY CHECK (length(deployment_resource_link_id) = 36),
    deployment_id TEXT NOT NULL REFERENCES deployments(deployment_id) ON DELETE CASCADE,
    resource_entity_id TEXT NOT NULL REFERENCES resource_entities(resource_entity_id) ON DELETE CASCADE,
    relation_kind TEXT NOT NULL CHECK (length(trim(relation_kind)) BETWEEN 1 AND 64),
    state TEXT NOT NULL CHECK (state IN ('observed', 'confirmed', 'stale', 'archived')),
    origin TEXT NOT NULL CHECK (origin IN ('observed', 'user_declared', 'derived')),
    source_refs_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(source_refs_json) AND json_type(source_refs_json) = 'array'),
    observed_at TEXT,
    confirmed_by TEXT,
    confirmed_at TEXT,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(deployment_id, resource_entity_id, relation_kind)
);

CREATE INDEX deployment_resource_links_resource_state_idx
ON deployment_resource_links(resource_entity_id, state, updated_at DESC);

CREATE INDEX deployment_resource_links_deployment_state_idx
ON deployment_resource_links(deployment_id, state, updated_at DESC);

CREATE TABLE technical_project_resource_links (
    technical_project_resource_link_id TEXT PRIMARY KEY CHECK (length(technical_project_resource_link_id) = 36),
    technical_project_id TEXT NOT NULL REFERENCES technical_projects(technical_project_id) ON DELETE CASCADE,
    resource_entity_id TEXT NOT NULL REFERENCES resource_entities(resource_entity_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('confirmed', 'archived')),
    origin TEXT NOT NULL CHECK (origin = 'user_declared'),
    source_refs_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(source_refs_json) AND json_type(source_refs_json) = 'array'),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    confirmed_by TEXT NOT NULL CHECK (length(confirmed_by) BETWEEN 1 AND 128),
    confirmed_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(technical_project_id, resource_entity_id)
);

CREATE INDEX technical_project_resource_links_resource_state_idx
ON technical_project_resource_links(resource_entity_id, state, updated_at DESC);

CREATE INDEX technical_project_resource_links_project_state_idx
ON technical_project_resource_links(technical_project_id, state, updated_at DESC);
