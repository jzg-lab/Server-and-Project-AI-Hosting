-- H2: persistent interval schedules for the closed host_resource_v1 profile.
-- Schedules only refresh the current snapshot and run ledger. Metric history,
-- rollups and health evaluation are deliberately separate migrations.

DROP TRIGGER IF EXISTS monitoring_current_run_host_insert;
DROP TRIGGER IF EXISTS monitoring_current_run_host_update;
DROP INDEX IF EXISTS monitor_runs_host_submitted_idx;
DROP INDEX IF EXISTS monitor_runs_one_active_per_host_idx;

ALTER TABLE monitor_runs RENAME TO monitor_runs_v9;

CREATE TABLE monitor_schedules (
    schedule_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    profile TEXT NOT NULL CHECK (profile IN ('host_resource_v1')),
    interval_seconds INTEGER NOT NULL CHECK (interval_seconds BETWEEN 300 AND 86400),
    jitter_seconds INTEGER NOT NULL CHECK (
        jitter_seconds >= 0 AND jitter_seconds < interval_seconds
    ),
    jitter_offset_seconds INTEGER NOT NULL CHECK (
        jitter_offset_seconds >= 0 AND jitter_offset_seconds <= jitter_seconds
    ),
    stale_after_seconds INTEGER NOT NULL CHECK (
        stale_after_seconds >= interval_seconds AND stale_after_seconds <= 604800
    ),
    state TEXT NOT NULL CHECK (state IN ('enabled', 'paused', 'archived')),
    next_due_at TEXT,
    last_due_at TEXT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    lease_owner TEXT,
    lease_token TEXT,
    lease_until TEXT,
    create_idempotency_key TEXT NOT NULL,
    create_request_sha256 TEXT NOT NULL,
    created_response_json TEXT NOT NULL CHECK (json_valid(created_response_json)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (state = 'enabled' AND next_due_at IS NOT NULL) OR
        (state IN ('paused', 'archived') AND next_due_at IS NULL)
    ),
    CHECK (
        (lease_owner IS NULL AND lease_token IS NULL AND lease_until IS NULL) OR
        (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_until IS NOT NULL)
    ),
    UNIQUE(host_id, create_idempotency_key)
);

CREATE UNIQUE INDEX monitor_schedules_one_active_profile_idx
ON monitor_schedules(host_id, profile)
WHERE state != 'archived';

CREATE INDEX monitor_schedules_due_idx
ON monitor_schedules(state, next_due_at)
WHERE state = 'enabled';

CREATE TABLE monitor_runs (
    run_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    profile TEXT NOT NULL CHECK (profile IN ('host_resource_v1')),
    trigger_kind TEXT NOT NULL CHECK (trigger_kind IN ('manual', 'scheduled', 'catch_up')),
    state TEXT NOT NULL CHECK (
        state IN (
            'queued', 'running', 'succeeded', 'partial', 'failed',
            'timed_out', 'skipped_overlap', 'interrupted'
        )
    ),
    schedule_id TEXT REFERENCES monitor_schedules(schedule_id) ON DELETE SET NULL,
    schedule_revision INTEGER,
    scheduled_for TEXT,
    stale_after_seconds INTEGER NOT NULL DEFAULT 900 CHECK (
        stale_after_seconds BETWEEN 60 AND 604800
    ),
    collector_version TEXT,
    boot_id TEXT,
    coverage_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(coverage_json)),
    output_bytes INTEGER NOT NULL DEFAULT 0 CHECK (output_bytes >= 0),
    ssh_session_count INTEGER NOT NULL DEFAULT 0 CHECK (ssh_session_count >= 0),
    failure_code TEXT,
    failure_summary TEXT,
    submitted_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    accepted_response_json TEXT NOT NULL CHECK (json_valid(accepted_response_json)),
    UNIQUE(host_id, idempotency_key),
    UNIQUE(schedule_id, scheduled_for),
    CHECK (
        (trigger_kind = 'manual' AND schedule_id IS NULL AND scheduled_for IS NULL) OR
        (trigger_kind IN ('scheduled', 'catch_up') AND schedule_id IS NOT NULL AND scheduled_for IS NOT NULL)
    )
);

INSERT INTO monitor_runs(
    run_id, host_id, request_id, idempotency_key, request_sha256,
    profile, trigger_kind, state, stale_after_seconds, collector_version,
    boot_id, coverage_json, output_bytes, ssh_session_count, failure_code,
    failure_summary, submitted_at, started_at, finished_at, accepted_response_json
)
SELECT
    run_id, host_id, request_id, idempotency_key, request_sha256,
    profile, trigger_kind, state, 900, collector_version,
    boot_id, coverage_json, output_bytes, ssh_session_count, failure_code,
    failure_summary, submitted_at, started_at, finished_at, accepted_response_json
FROM monitor_runs_v9;

DROP TABLE monitor_runs_v9;

CREATE INDEX monitor_runs_host_submitted_idx
ON monitor_runs(host_id, submitted_at DESC);

CREATE UNIQUE INDEX monitor_runs_one_active_per_host_idx
ON monitor_runs(host_id)
WHERE state IN ('queued', 'running');

CREATE INDEX monitor_runs_schedule_due_idx
ON monitor_runs(schedule_id, scheduled_for);

CREATE TRIGGER monitoring_current_run_host_insert
BEFORE INSERT ON monitoring_current
WHEN NOT EXISTS (
    SELECT 1 FROM monitor_runs
    WHERE monitor_runs.run_id = NEW.run_id AND monitor_runs.host_id = NEW.host_id
)
BEGIN
    SELECT RAISE(ABORT, 'monitoring_current run must belong to host');
END;

CREATE TRIGGER monitoring_current_run_host_update
BEFORE UPDATE OF host_id, run_id ON monitoring_current
WHEN NOT EXISTS (
    SELECT 1 FROM monitor_runs
    WHERE monitor_runs.run_id = NEW.run_id AND monitor_runs.host_id = NEW.host_id
)
BEGIN
    SELECT RAISE(ABORT, 'monitoring_current run must belong to host');
END;

-- Discovery and monitoring are both SSH observation jobs. Enforce the shared
-- per-HOST exclusion in SQLite so concurrent HTTP and scheduler writers cannot
-- bypass it.
CREATE TRIGGER monitor_runs_block_active_discovery_insert
BEFORE INSERT ON monitor_runs
WHEN NEW.state IN ('queued', 'running') AND EXISTS (
    SELECT 1 FROM discovery_runs
    WHERE host_id = NEW.host_id AND state IN ('accepted', 'running')
)
BEGIN
    SELECT RAISE(ABORT, 'host observation already active');
END;

CREATE TRIGGER monitor_runs_block_active_discovery_update
BEFORE UPDATE OF state ON monitor_runs
WHEN NEW.state IN ('queued', 'running') AND EXISTS (
    SELECT 1 FROM discovery_runs
    WHERE host_id = NEW.host_id AND state IN ('accepted', 'running')
)
BEGIN
    SELECT RAISE(ABORT, 'host observation already active');
END;

CREATE TRIGGER discovery_runs_block_active_monitor_insert
BEFORE INSERT ON discovery_runs
WHEN NEW.state IN ('accepted', 'running') AND EXISTS (
    SELECT 1 FROM monitor_runs
    WHERE host_id = NEW.host_id AND state IN ('queued', 'running')
)
BEGIN
    SELECT RAISE(ABORT, 'host observation already active');
END;

CREATE TRIGGER discovery_runs_block_active_monitor_update
BEFORE UPDATE OF state ON discovery_runs
WHEN NEW.state IN ('accepted', 'running') AND EXISTS (
    SELECT 1 FROM monitor_runs
    WHERE host_id = NEW.host_id AND state IN ('queued', 'running')
)
BEGIN
    SELECT RAISE(ABORT, 'host observation already active');
END;

-- Add the two monitoring event kinds without rewriting existing cursors.
DROP INDEX IF EXISTS change_events_workspace_cursor_idx;
ALTER TABLE change_events RENAME TO change_events_v9;

CREATE TABLE change_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'host.connection.changed',
        'discovery.run.changed',
        'projection.changed',
        'onboarding.changed',
        'monitor.schedule.changed',
        'monitor.run.changed'
    )),
    subject_ref TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 0,
    summary_json TEXT NOT NULL DEFAULT '{}',
    committed_at TEXT NOT NULL
);

INSERT INTO change_events(
    cursor, workspace_id, kind, subject_ref, revision, summary_json, committed_at
)
SELECT cursor, workspace_id, kind, subject_ref, revision, summary_json, committed_at
FROM change_events_v9;

DROP TABLE change_events_v9;

CREATE INDEX change_events_workspace_cursor_idx
ON change_events(workspace_id, cursor);
