CREATE TABLE monitor_runs (
    run_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    profile TEXT NOT NULL CHECK (profile IN ('host_resource_v1')),
    trigger_kind TEXT NOT NULL CHECK (trigger_kind IN ('manual')),
    state TEXT NOT NULL CHECK (
        state IN (
            'queued', 'running', 'succeeded', 'partial', 'failed',
            'timed_out', 'skipped_overlap', 'interrupted'
        )
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
    UNIQUE(host_id, idempotency_key)
);

CREATE INDEX monitor_runs_host_submitted_idx
ON monitor_runs(host_id, submitted_at DESC);

CREATE UNIQUE INDEX monitor_runs_one_active_per_host_idx
ON monitor_runs(host_id)
WHERE state IN ('queued', 'running');

CREATE TABLE monitoring_current (
    host_id TEXT PRIMARY KEY REFERENCES hosts(host_id) ON DELETE CASCADE,
    -- Keep the source receipt id without coupling current data retention to
    -- later run-ledger cleanup. The triggers below still reject cross-HOST
    -- pointers when a current snapshot is written.
    run_id TEXT NOT NULL UNIQUE,
    profile TEXT NOT NULL CHECK (profile IN ('host_resource_v1')),
    collector_version TEXT NOT NULL,
    boot_id TEXT,
    snapshot_json TEXT NOT NULL CHECK (json_valid(snapshot_json)),
    coverage_json TEXT NOT NULL CHECK (json_valid(coverage_json)),
    metric_count INTEGER NOT NULL CHECK (metric_count >= 0),
    unknown_count INTEGER NOT NULL CHECK (unknown_count >= 0),
    observed_at TEXT NOT NULL,
    valid_until TEXT NOT NULL,
    retention_tier TEXT NOT NULL DEFAULT 'full' CHECK (
        retention_tier IN ('full', 'hourly_rollup', 'daily_rollup', 'summary')
    ),
    snapshot_sha256 TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

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
