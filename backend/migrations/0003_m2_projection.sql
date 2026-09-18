PRAGMA foreign_keys = ON;

ALTER TABLE discovery_runs ADD COLUMN draft_id TEXT;

-- M0 already owns the base projection tables. M2 only adds the fields needed
-- to bind those generic records to a concrete SSH discovery run and draft revision.
ALTER TABLE projection_drafts ADD COLUMN host_id TEXT REFERENCES hosts(host_id) ON DELETE CASCADE;
ALTER TABLE projection_drafts ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE projection_drafts ADD COLUMN pending_changes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projection_drafts ADD COLUMN created_at TEXT;
UPDATE projection_drafts SET created_at = updated_at WHERE created_at IS NULL;

CREATE INDEX projection_drafts_host_updated_idx
ON projection_drafts(host_id, updated_at DESC);
CREATE UNIQUE INDEX projection_drafts_discovery_run_unique_idx
ON projection_drafts(discovery_run_id) WHERE discovery_run_id IS NOT NULL;

ALTER TABLE projection_versions ADD COLUMN draft_id TEXT REFERENCES projection_drafts(draft_id) ON DELETE CASCADE;
ALTER TABLE projection_versions ADD COLUMN host_id TEXT REFERENCES hosts(host_id) ON DELETE CASCADE;

CREATE UNIQUE INDEX projection_versions_draft_revision_unique_idx
ON projection_versions(draft_id, revision) WHERE draft_id IS NOT NULL;
CREATE INDEX projection_versions_host_revision_idx
ON projection_versions(host_id, revision DESC);

CREATE TABLE ignore_rules (
    rule_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('ignored', 'archived', 'active')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(host_id, fingerprint)
);

CREATE TABLE projection_mutation_requests (
    request_id TEXT PRIMARY KEY,
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(resource_kind, resource_id, idempotency_key)
);

CREATE INDEX projection_mutation_requests_lookup_idx
ON projection_mutation_requests(resource_kind, resource_id, idempotency_key);
