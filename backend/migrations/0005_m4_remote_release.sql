PRAGMA foreign_keys = ON;

CREATE TABLE owner_sessions (
    session_id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    token_sha256 TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE INDEX owner_sessions_active_idx
ON owner_sessions(token_sha256, expires_at)
WHERE revoked_at IS NULL;

CREATE TABLE audit_events (
    audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT,
    actor_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    status_code INTEGER NOT NULL,
    request_id TEXT NOT NULL,
    summary_json TEXT NOT NULL DEFAULT '{}',
    occurred_at TEXT NOT NULL
);

CREATE INDEX audit_events_occurred_idx
ON audit_events(occurred_at DESC);

CREATE INDEX audit_events_kind_target_idx
ON audit_events(kind, target_ref, occurred_at DESC);

CREATE TABLE change_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'host.connection.changed',
        'discovery.run.changed',
        'projection.changed',
        'onboarding.changed'
    )),
    subject_ref TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 0,
    summary_json TEXT NOT NULL DEFAULT '{}',
    committed_at TEXT NOT NULL
);

CREATE INDEX change_events_workspace_cursor_idx
ON change_events(workspace_id, cursor);

CREATE TABLE backup_records (
    backup_id TEXT PRIMARY KEY,
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('workspace', 'host', 'project', 'migration')),
    scope_id TEXT NOT NULL,
    storage_path TEXT NOT NULL,
    database_sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('ready', 'restored', 'failed')),
    created_at TEXT NOT NULL
);

CREATE INDEX backup_records_scope_created_idx
ON backup_records(scope_kind, scope_id, created_at DESC);

CREATE TABLE m4_mutation_requests (
    request_id TEXT PRIMARY KEY,
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(resource_kind, resource_id, idempotency_key)
);

CREATE INDEX m4_mutation_requests_lookup_idx
ON m4_mutation_requests(resource_kind, resource_id, idempotency_key);
