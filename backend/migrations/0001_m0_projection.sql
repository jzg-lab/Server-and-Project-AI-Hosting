PRAGMA foreign_keys = ON;

CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE projects (
    project_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    projection_state TEXT NOT NULL CHECK (projection_state IN ('draft', 'confirmed', 'stale', 'archived')),
    source_refs_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX projects_workspace_state_idx ON projects(workspace_id, projection_state);

CREATE TABLE entities (
    entity_id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(project_id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    external_id TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    projection_state TEXT NOT NULL CHECK (projection_state IN ('discovered', 'draft', 'confirmed', 'stale', 'archived')),
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(source, external_id)
);

CREATE INDEX entities_project_kind_idx ON entities(project_id, kind);

CREATE TABLE entity_relations (
    relation_id TEXT PRIMARY KEY,
    from_entity_id TEXT NOT NULL REFERENCES entities(entity_id) ON DELETE CASCADE,
    to_entity_id TEXT NOT NULL REFERENCES entities(entity_id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    projection_state TEXT NOT NULL CHECK (projection_state IN ('discovered', 'draft', 'confirmed', 'stale', 'archived')),
    source_refs_json TEXT NOT NULL DEFAULT '[]',
    UNIQUE(from_entity_id, to_entity_id, kind)
);

CREATE INDEX entity_relations_from_to_idx ON entity_relations(from_entity_id, to_entity_id);

CREATE TABLE projection_drafts (
    draft_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    discovery_run_id TEXT,
    base_revision INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('draft', 'confirmed', 'archived')),
    snapshot_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX projection_drafts_state_updated_idx ON projection_drafts(state, updated_at DESC);

CREATE TABLE projection_versions (
    version_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    project_id TEXT REFERENCES projects(project_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    confirmed_by TEXT NOT NULL,
    confirmed_at TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    UNIQUE(workspace_id, project_id, revision)
);

CREATE INDEX projection_versions_project_revision_idx ON projection_versions(project_id, revision DESC);

CREATE TABLE canvas_layouts (
    layout_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    revision INTEGER NOT NULL,
    positions_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(workspace_id, scope)
);
