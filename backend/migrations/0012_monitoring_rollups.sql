-- H3b: deterministic UTC hour/day metric rollups and auditable maintenance.
--
-- Raw samples remain the source of truth until a complete rollup partition is
-- committed. Retention is implemented separately and is disabled by default;
-- a partition is sealed before any of its raw rows may be removed.

CREATE TABLE metric_rollup_partitions (
    partition_id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    resolution TEXT NOT NULL CHECK (resolution IN ('hour', 'day')),
    bucket_start TEXT NOT NULL,
    bucket_start_epoch_ms INTEGER NOT NULL CHECK (bucket_start_epoch_ms >= 0),
    bucket_end TEXT NOT NULL,
    bucket_end_epoch_ms INTEGER NOT NULL CHECK (bucket_end_epoch_ms > bucket_start_epoch_ms),
    input_count INTEGER NOT NULL CHECK (input_count > 0),
    input_digest TEXT NOT NULL CHECK (length(input_digest) = 64),
    series_count INTEGER NOT NULL CHECK (series_count > 0),
    state TEXT NOT NULL CHECK (state IN ('complete', 'sealed')),
    compacted_at TEXT NOT NULL,
    sealed_at TEXT,
    rollup_rows_pruned_at TEXT,
    FOREIGN KEY(host_id) REFERENCES hosts(host_id) ON DELETE CASCADE,
    UNIQUE(host_id, resolution, bucket_start_epoch_ms),
    UNIQUE(partition_id, host_id, resolution, bucket_start_epoch_ms),
    CHECK (
        (state = 'complete' AND sealed_at IS NULL)
        OR (state = 'sealed' AND sealed_at IS NOT NULL)
    )
);

CREATE INDEX metric_rollup_partitions_resolution_bucket_idx
ON metric_rollup_partitions(resolution, bucket_start_epoch_ms, host_id);

CREATE TABLE metric_rollups (
    rollup_id TEXT PRIMARY KEY CHECK (length(rollup_id) = 36),
    partition_id TEXT NOT NULL,
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
    dimensions_json TEXT NOT NULL
        CHECK (json_valid(dimensions_json) AND json_type(dimensions_json) = 'object'),
    dimensions_sha256 TEXT NOT NULL CHECK (length(dimensions_sha256) = 64),
    sample_kind TEXT NOT NULL CHECK (sample_kind IN ('gauge', 'counter', 'derived')),
    source_kind TEXT NOT NULL CHECK (source_kind IN ('ssh_host_resource_v1')),
    unit TEXT NOT NULL CHECK (length(unit) BETWEEN 1 AND 64),
    resolution TEXT NOT NULL CHECK (resolution IN ('hour', 'day')),
    bucket_start TEXT NOT NULL,
    bucket_start_epoch_ms INTEGER NOT NULL CHECK (bucket_start_epoch_ms >= 0),
    bucket_end TEXT NOT NULL,
    bucket_end_epoch_ms INTEGER NOT NULL CHECK (bucket_end_epoch_ms > bucket_start_epoch_ms),
    sample_count INTEGER NOT NULL CHECK (sample_count > 0),
    observed_count INTEGER NOT NULL CHECK (observed_count BETWEEN 0 AND sample_count),
    non_observed_count INTEGER NOT NULL
        CHECK (non_observed_count = sample_count - observed_count),
    expected_count INTEGER CHECK (expected_count IS NULL OR expected_count >= observed_count),
    missing_count INTEGER CHECK (missing_count IS NULL OR missing_count >= 0),
    min_value REAL,
    max_value REAL,
    average_value REAL,
    p95_value REAL,
    last_value REAL,
    counter_first INTEGER,
    counter_last INTEGER,
    counter_delta INTEGER CHECK (counter_delta IS NULL OR counter_delta >= 0),
    reset_count INTEGER NOT NULL DEFAULT 0 CHECK (reset_count >= 0),
    quality TEXT NOT NULL CHECK (quality IN (
        'observed', 'unsupported', 'parse_failed', 'counter_reset',
        'counter_unreliable', 'insufficient_interval', 'permission_denied',
        'timed_out'
    )),
    quality_counts_json TEXT NOT NULL
        CHECK (json_valid(quality_counts_json) AND json_type(quality_counts_json) = 'object'),
    input_count INTEGER NOT NULL CHECK (input_count = sample_count),
    input_digest TEXT NOT NULL CHECK (length(input_digest) = 64),
    boundary_sample_id TEXT,
    last_sample_id TEXT NOT NULL,
    last_run_id TEXT NOT NULL,
    last_sample_at TEXT NOT NULL,
    last_sample_at_epoch_ms INTEGER NOT NULL CHECK (last_sample_at_epoch_ms >= 0),
    last_valid_sample_at TEXT,
    last_valid_sample_at_epoch_ms INTEGER
        CHECK (last_valid_sample_at_epoch_ms IS NULL OR last_valid_sample_at_epoch_ms >= 0),
    last_valid_run_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(partition_id, host_id, resolution, bucket_start_epoch_ms)
        REFERENCES metric_rollup_partitions(
            partition_id, host_id, resolution, bucket_start_epoch_ms
        ) ON DELETE CASCADE,
    UNIQUE(
        host_id, family, subject_kind, subject_id, metric_name,
        dimensions_sha256, sample_kind, source_kind, unit,
        resolution, bucket_start_epoch_ms
    ),
    CHECK (
        (expected_count IS NULL AND missing_count IS NULL)
        OR (expected_count IS NOT NULL AND missing_count = expected_count - observed_count)
    ),
    CHECK (
        (
            sample_kind = 'counter'
            AND min_value IS NULL AND max_value IS NULL
            AND average_value IS NULL AND p95_value IS NULL AND last_value IS NULL
        )
        OR (
            sample_kind <> 'counter'
            AND counter_first IS NULL AND counter_last IS NULL AND counter_delta IS NULL
            AND reset_count = 0
        )
    ),
    CHECK (
        observed_count > 0
        OR (
            min_value IS NULL AND max_value IS NULL AND average_value IS NULL
            AND p95_value IS NULL AND last_value IS NULL
            AND counter_first IS NULL AND counter_last IS NULL AND counter_delta IS NULL
        )
    )
);

CREATE INDEX metric_rollups_host_resolution_bucket_idx
ON metric_rollups(host_id, resolution, bucket_start_epoch_ms, rollup_id);

CREATE INDEX metric_rollups_series_bucket_idx
ON metric_rollups(
    host_id, family, subject_kind, subject_id, metric_name,
    sample_kind, resolution, bucket_start_epoch_ms
);

-- Boundary lookup prefers remaining raw and falls back to the latest retained
-- rollup. Raw can therefore expire without leaving an unbounded anchor row.
CREATE INDEX metric_samples_counter_retention_idx
ON metric_samples(
    host_id, family, subject_kind, subject_id, metric_name,
    dimensions_sha256, source_kind, unit, observed_at_epoch_ms, sample_id
)
WHERE sample_kind = 'counter' AND quality = 'observed';

CREATE TABLE monitoring_history_maintenance (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    state TEXT NOT NULL CHECK (state IN ('idle', 'running', 'succeeded', 'failed')),
    retention_enabled INTEGER NOT NULL CHECK (retention_enabled IN (0, 1)),
    raw_observed_retention_days INTEGER NOT NULL CHECK (raw_observed_retention_days >= 1),
    raw_non_observed_retention_days INTEGER NOT NULL
        CHECK (raw_non_observed_retention_days >= raw_observed_retention_days),
    -- Hour rollups must outlive every raw class. Otherwise a small raw-delete
    -- batch can prune the covering hour row before older non-observed raw is
    -- eligible, leaving that raw permanently stranded.
    hour_retention_days INTEGER NOT NULL
        CHECK (hour_retention_days >= raw_non_observed_retention_days),
    day_retention_days INTEGER NOT NULL CHECK (day_retention_days >= hour_retention_days),
    last_started_at TEXT,
    last_completed_at TEXT,
    last_successful_at TEXT,
    last_error_at TEXT,
    last_error_code TEXT,
    last_hour_partition_count INTEGER NOT NULL DEFAULT 0 CHECK (last_hour_partition_count >= 0),
    last_day_partition_count INTEGER NOT NULL DEFAULT 0 CHECK (last_day_partition_count >= 0),
    last_raw_deleted_count INTEGER NOT NULL DEFAULT 0 CHECK (last_raw_deleted_count >= 0),
    last_hour_deleted_count INTEGER NOT NULL DEFAULT 0 CHECK (last_hour_deleted_count >= 0),
    last_day_deleted_count INTEGER NOT NULL DEFAULT 0 CHECK (last_day_deleted_count >= 0),
    raw_sample_count INTEGER NOT NULL DEFAULT 0 CHECK (raw_sample_count >= 0),
    hour_rollup_count INTEGER NOT NULL DEFAULT 0 CHECK (hour_rollup_count >= 0),
    day_rollup_count INTEGER NOT NULL DEFAULT 0 CHECK (day_rollup_count >= 0),
    database_bytes INTEGER NOT NULL DEFAULT 0 CHECK (database_bytes >= 0),
    updated_at TEXT NOT NULL
);

INSERT INTO monitoring_history_maintenance(
    singleton_id, state, retention_enabled,
    raw_observed_retention_days, raw_non_observed_retention_days,
    hour_retention_days, day_retention_days, updated_at
) VALUES (
    1, 'idle', 0, 7, 30, 90, 365,
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

CREATE TABLE monitoring_compaction_runs (
    compaction_run_id TEXT PRIMARY KEY CHECK (length(compaction_run_id) = 36),
    state TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'interrupted')),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    hour_partition_count INTEGER NOT NULL DEFAULT 0 CHECK (hour_partition_count >= 0),
    day_partition_count INTEGER NOT NULL DEFAULT 0 CHECK (day_partition_count >= 0),
    raw_deleted_count INTEGER NOT NULL DEFAULT 0 CHECK (raw_deleted_count >= 0),
    hour_deleted_count INTEGER NOT NULL DEFAULT 0 CHECK (hour_deleted_count >= 0),
    day_deleted_count INTEGER NOT NULL DEFAULT 0 CHECK (day_deleted_count >= 0),
    error_code TEXT,
    settings_json TEXT NOT NULL
        CHECK (json_valid(settings_json) AND json_type(settings_json) = 'object'),
    CHECK (
        (state = 'running' AND finished_at IS NULL AND error_code IS NULL)
        OR (state = 'succeeded' AND finished_at IS NOT NULL AND error_code IS NULL)
        OR (state IN ('failed', 'interrupted') AND finished_at IS NOT NULL AND error_code IS NOT NULL)
    )
);

CREATE INDEX monitoring_compaction_runs_started_idx
ON monitoring_compaction_runs(started_at DESC);

CREATE UNIQUE INDEX monitoring_compaction_one_running_idx
ON monitoring_compaction_runs((1)) WHERE state = 'running';
