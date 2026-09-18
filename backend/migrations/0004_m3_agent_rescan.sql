PRAGMA foreign_keys = ON;

CREATE TABLE secret_ref_descriptors_m3 (
    credential_ref TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('ssh_key', 'model_key')),
    idempotency_key TEXT NOT NULL UNIQUE,
    secret_sha256 TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

INSERT INTO secret_ref_descriptors_m3(
    credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
)
SELECT credential_ref, kind, idempotency_key, secret_sha256, response_json, created_at
FROM secret_ref_descriptors;

DROP TABLE secret_ref_descriptors;
ALTER TABLE secret_ref_descriptors_m3 RENAME TO secret_ref_descriptors;

CREATE TABLE model_provider_configs (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    base_url TEXT NOT NULL,
    model TEXT NOT NULL,
    credential_ref TEXT NOT NULL,
    revision INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE model_provider_test_runs (
    test_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('reachable', 'failed')),
    error_code TEXT,
    latency_ms INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE onboarding_sessions (
    session_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    draft_id TEXT NOT NULL REFERENCES projection_drafts(draft_id) ON DELETE CASCADE,
    discovery_run_id TEXT NOT NULL REFERENCES discovery_runs(run_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('ready', 'degraded', 'unavailable')),
    facts_used_json TEXT NOT NULL DEFAULT '[]',
    warnings_json TEXT NOT NULL DEFAULT '[]',
    error_code TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX onboarding_sessions_draft_updated_idx
ON onboarding_sessions(draft_id, updated_at DESC);

CREATE TABLE agent_proposals (
    proposal_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES onboarding_sessions(session_id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    reason TEXT NOT NULL,
    confidence TEXT NOT NULL CHECK (confidence IN ('low', 'medium', 'high')),
    evidence_refs_json TEXT NOT NULL,
    patch_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'adopted', 'modified', 'rejected', 'undone')),
    before_snapshot_json TEXT,
    applied_revision INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX agent_proposals_session_state_idx
ON agent_proposals(session_id, state, created_at);

CREATE TABLE agent_questions (
    question_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES onboarding_sessions(session_id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('choice', 'text', 'confirm')),
    prompt TEXT NOT NULL,
    options_json TEXT NOT NULL DEFAULT '[]',
    evidence_refs_json TEXT NOT NULL,
    blocking INTEGER NOT NULL CHECK (blocking IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('pending', 'answered')),
    answer_json TEXT,
    created_at TEXT NOT NULL,
    answered_at TEXT
);

CREATE INDEX agent_questions_session_state_idx
ON agent_questions(session_id, state, created_at);

CREATE TABLE model_invocations (
    invocation_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES onboarding_sessions(session_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('succeeded', 'failed')),
    error_code TEXT,
    result_summary_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    finished_at TEXT NOT NULL
);

CREATE TABLE onboarding_messages (
    message_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES onboarding_sessions(session_id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    proposal_id TEXT REFERENCES agent_proposals(proposal_id) ON DELETE SET NULL,
    question_id TEXT REFERENCES agent_questions(question_id) ON DELETE SET NULL,
    summary_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE TABLE discovery_diffs (
    diff_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE REFERENCES discovery_runs(run_id) ON DELETE CASCADE,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    previous_run_id TEXT REFERENCES discovery_runs(run_id) ON DELETE SET NULL,
    counts_json TEXT NOT NULL,
    items_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX discovery_diffs_host_created_idx
ON discovery_diffs(host_id, created_at DESC);

ALTER TABLE discovery_runs ADD COLUMN diff_id TEXT;

-- Each rescan owns a new draft layout for the same HOST scope. The original
-- scope uniqueness constraint only allowed the first draft to be persisted.
CREATE TABLE canvas_layouts_m3 (
    layout_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    revision INTEGER NOT NULL,
    positions_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO canvas_layouts_m3(
    layout_id, workspace_id, scope, revision, positions_json, updated_at
)
SELECT layout_id, workspace_id, scope, revision, positions_json, updated_at
FROM canvas_layouts;

DROP TABLE canvas_layouts;
ALTER TABLE canvas_layouts_m3 RENAME TO canvas_layouts;
CREATE INDEX canvas_layouts_scope_updated_idx
ON canvas_layouts(workspace_id, scope, updated_at DESC);

CREATE TABLE m3_mutation_requests (
    request_id TEXT PRIMARY KEY,
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(resource_kind, resource_id, idempotency_key)
);

CREATE INDEX m3_mutation_requests_lookup_idx
ON m3_mutation_requests(resource_kind, resource_id, idempotency_key);
