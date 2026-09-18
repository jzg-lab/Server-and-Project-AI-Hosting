PRAGMA foreign_keys = ON;

CREATE TABLE secret_ref_descriptors (
    credential_ref TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind = 'ssh_key'),
    idempotency_key TEXT NOT NULL UNIQUE,
    secret_sha256 TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE hosts (
    host_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    address TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    ssh_user TEXT NOT NULL,
    credential_ref TEXT NOT NULL,
    host_key_fingerprint TEXT,
    pending_host_key_fingerprint TEXT,
    pending_host_key_line TEXT,
    host_key_state TEXT NOT NULL CHECK (host_key_state IN ('unverified', 'verified', 'changed')),
    transport TEXT NOT NULL CHECK (transport = 'ssh'),
    os TEXT NOT NULL CHECK (os = 'linux'),
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_checked_at TEXT,
    UNIQUE(workspace_id, address, port, ssh_user)
);

CREATE INDEX hosts_workspace_status_idx ON hosts(workspace_id, status);

CREATE TABLE host_registration_requests (
    idempotency_key TEXT PRIMARY KEY,
    request_sha256 TEXT NOT NULL,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE connection_tests (
    test_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL,
    candidate_fingerprint TEXT,
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    error_code TEXT,
    error_summary TEXT,
    response_json TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    UNIQUE(host_id, idempotency_key)
);

CREATE INDEX connection_tests_host_started_idx ON connection_tests(host_id, started_at DESC);

CREATE TABLE host_key_confirmations (
    confirmation_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    response_json TEXT NOT NULL,
    confirmed_at TEXT NOT NULL,
    UNIQUE(host_id, idempotency_key)
);

CREATE TABLE discovery_runs (
    run_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    state TEXT NOT NULL,
    response_json TEXT NOT NULL,
    failure_code TEXT,
    failure_summary TEXT,
    submitted_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    evidence_json TEXT,
    evidence_sha256 TEXT,
    evidence_item_count INTEGER NOT NULL DEFAULT 0,
    evidence_retention TEXT NOT NULL DEFAULT 'complete' CHECK (evidence_retention IN ('complete', 'summary')),
    UNIQUE(host_id, idempotency_key)
);

CREATE UNIQUE INDEX discovery_runs_one_active_per_host_idx
ON discovery_runs(host_id)
WHERE state IN ('accepted', 'running');

CREATE INDEX discovery_runs_host_submitted_idx ON discovery_runs(host_id, submitted_at DESC);

CREATE TABLE evidence_items (
    evidence_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES discovery_runs(run_id) ON DELETE CASCADE,
    external_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    freshness TEXT NOT NULL CHECK (freshness IN ('fresh', 'stale', 'unavailable')),
    sha256 TEXT,
    redaction_state TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    UNIQUE(run_id, external_id, kind)
);

CREATE INDEX evidence_items_run_kind_idx ON evidence_items(run_id, kind);

CREATE TABLE discovery_command_audits (
    audit_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES discovery_runs(run_id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    exit_code INTEGER,
    output_bytes INTEGER NOT NULL,
    stderr_summary TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL
);

CREATE INDEX discovery_command_audits_run_idx ON discovery_command_audits(run_id, started_at);
