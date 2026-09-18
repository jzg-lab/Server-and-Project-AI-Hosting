-- H3a: typed raw HOST metric history and schedule-gap provenance.
-- Existing monitoring_current rows are intentionally not backfilled: they are
-- the latest snapshot, not evidence of historical samples.

ALTER TABLE monitor_runs ADD COLUMN due_interval_seconds INTEGER
    CHECK (due_interval_seconds IS NULL OR due_interval_seconds BETWEEN 300 AND 86400);

ALTER TABLE monitor_runs ADD COLUMN missed_due_count INTEGER
    CHECK (missed_due_count IS NULL OR missed_due_count >= 0);

CREATE UNIQUE INDEX monitor_runs_run_host_idx
ON monitor_runs(run_id, host_id);

CREATE TABLE monitoring_history_metadata (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    history_started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO monitoring_history_metadata(
    singleton_id, history_started_at, updated_at
) VALUES (
    1,
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

CREATE TABLE metric_samples (
    sample_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    host_id TEXT NOT NULL,
    family TEXT NOT NULL CHECK (family IN (
        'cpu', 'memory', 'load', 'disk_capacity', 'disk_io',
        'network', 'uptime', 'process'
    )),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN (
        'host', 'cpu', 'filesystem', 'block_device', 'interface', 'process'
    )),
    subject_id TEXT NOT NULL CHECK (length(subject_id) BETWEEN 1 AND 512),
    metric_name TEXT NOT NULL CHECK (length(metric_name) BETWEEN 1 AND 128),
    dimensions_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(dimensions_json) AND json_type(dimensions_json) = 'object'),
    dimensions_sha256 TEXT NOT NULL CHECK (length(dimensions_sha256) = 64),
    sample_kind TEXT NOT NULL CHECK (sample_kind IN ('gauge', 'counter', 'derived')),
    value_real REAL,
    value_integer INTEGER,
    unit TEXT NOT NULL CHECK (length(unit) BETWEEN 1 AND 64),
    window_seconds REAL CHECK (window_seconds IS NULL OR window_seconds > 0),
    quality TEXT NOT NULL CHECK (quality IN (
        'observed', 'unsupported', 'parse_failed', 'counter_reset',
        'counter_unreliable', 'insufficient_interval', 'permission_denied',
        'timed_out'
    )),
    observed_at TEXT NOT NULL,
    observed_at_epoch_ms INTEGER NOT NULL CHECK (observed_at_epoch_ms >= 0),
    source_kind TEXT NOT NULL CHECK (source_kind IN ('ssh_host_resource_v1')),
    created_at TEXT NOT NULL,
    FOREIGN KEY(run_id, host_id) REFERENCES monitor_runs(run_id, host_id) ON DELETE CASCADE,
    UNIQUE(
        run_id, family, subject_kind, subject_id, metric_name,
        dimensions_sha256, sample_kind, source_kind
    ),
    CHECK (
        (
            quality = 'observed'
            AND ((value_real IS NOT NULL) + (value_integer IS NOT NULL) = 1)
        )
        OR (
            quality <> 'observed'
            AND value_real IS NULL
            AND value_integer IS NULL
        )
    )
);

CREATE INDEX metric_samples_host_family_observed_idx
ON metric_samples(host_id, family, observed_at_epoch_ms);

CREATE INDEX metric_samples_observed_host_idx
ON metric_samples(observed_at_epoch_ms, host_id);

CREATE INDEX metric_samples_host_observed_sample_idx
ON metric_samples(host_id, observed_at_epoch_ms, sample_id);

-- Samples are append-only until an explicit retention transaction removes an
-- already compacted range. In-place mutation would invalidate export and
-- future rollup digests.
CREATE TRIGGER metric_samples_reject_update
BEFORE UPDATE ON metric_samples
BEGIN
    SELECT RAISE(ABORT, 'metric_samples are immutable');
END;
