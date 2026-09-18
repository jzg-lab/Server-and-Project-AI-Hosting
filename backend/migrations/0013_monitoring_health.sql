-- H3c: schedule provenance, versioned HOST health policies and deterministic
-- per-run health evaluations.
--
-- Existing schedules receive only a migration-time legacy baseline.  Nothing
-- before provenance_started_at is reconstructed from monitor_runs, current
-- snapshots or metric rollups.

CREATE TABLE monitor_schedule_provenance_metadata (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    provenance_started_at TEXT NOT NULL,
    provenance_started_at_epoch_ms INTEGER NOT NULL CHECK (provenance_started_at_epoch_ms >= 0),
    created_at TEXT NOT NULL
);

INSERT INTO monitor_schedule_provenance_metadata(
    singleton_id, provenance_started_at, provenance_started_at_epoch_ms, created_at
) VALUES (
    1,
    strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
    CAST(strftime('%s', 'now') AS INTEGER) * 1000,
    strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
);

CREATE UNIQUE INDEX monitor_schedules_id_host_idx
ON monitor_schedules(schedule_id, host_id);

CREATE TABLE monitor_schedule_versions (
    schedule_version_id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL,
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 1),
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
    due_from_at TEXT,
    due_from_at_epoch_ms INTEGER CHECK (due_from_at_epoch_ms IS NULL OR due_from_at_epoch_ms >= 0),
    due_until_at TEXT,
    due_until_at_epoch_ms INTEGER CHECK (due_until_at_epoch_ms IS NULL OR due_until_at_epoch_ms >= 0),
    effective_from_at TEXT NOT NULL,
    effective_from_at_epoch_ms INTEGER NOT NULL CHECK (effective_from_at_epoch_ms >= 0),
    effective_until_at TEXT,
    effective_until_at_epoch_ms INTEGER CHECK (
        effective_until_at_epoch_ms IS NULL OR effective_until_at_epoch_ms >= effective_from_at_epoch_ms
    ),
    provenance_kind TEXT NOT NULL CHECK (provenance_kind IN ('legacy_baseline', 'recorded')),
    activated_at TEXT,
    paused_at TEXT,
    resumed_at TEXT,
    archived_at TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(schedule_id, host_id)
        REFERENCES monitor_schedules(schedule_id, host_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    UNIQUE(schedule_id, revision),
    UNIQUE(schedule_version_id, schedule_id, revision),
    CHECK (
        (state = 'enabled' AND due_from_at IS NOT NULL AND due_from_at_epoch_ms IS NOT NULL)
        OR (state IN ('paused', 'archived') AND due_from_at IS NULL AND due_from_at_epoch_ms IS NULL)
    ),
    CHECK (
        (due_until_at IS NULL AND due_until_at_epoch_ms IS NULL)
        OR (due_until_at IS NOT NULL AND due_until_at_epoch_ms IS NOT NULL)
    ),
    CHECK (
        due_until_at_epoch_ms IS NULL
        OR (due_from_at_epoch_ms IS NOT NULL AND due_until_at_epoch_ms >= due_from_at_epoch_ms)
    ),
    CHECK (
        (effective_until_at IS NULL AND effective_until_at_epoch_ms IS NULL)
        OR (effective_until_at IS NOT NULL AND effective_until_at_epoch_ms IS NOT NULL)
    )
);

CREATE UNIQUE INDEX monitor_schedule_versions_current_idx
ON monitor_schedule_versions(schedule_id)
WHERE effective_until_at IS NULL;

CREATE INDEX monitor_schedule_versions_host_effective_idx
ON monitor_schedule_versions(host_id, effective_from_at_epoch_ms, effective_until_at_epoch_ms);

-- Version payload and grid fields are immutable.  The only permitted update
-- closes the current half-open interval exactly once.
CREATE TRIGGER monitor_schedule_versions_close_only
BEFORE UPDATE ON monitor_schedule_versions
WHEN
    OLD.effective_until_at IS NOT NULL
    OR NEW.effective_until_at IS NULL
    OR NEW.effective_until_at_epoch_ms IS NULL
    OR NEW.schedule_version_id IS NOT OLD.schedule_version_id
    OR NEW.schedule_id IS NOT OLD.schedule_id
    OR NEW.host_id IS NOT OLD.host_id
    OR NEW.revision IS NOT OLD.revision
    OR NEW.profile IS NOT OLD.profile
    OR NEW.interval_seconds IS NOT OLD.interval_seconds
    OR NEW.jitter_seconds IS NOT OLD.jitter_seconds
    OR NEW.jitter_offset_seconds IS NOT OLD.jitter_offset_seconds
    OR NEW.stale_after_seconds IS NOT OLD.stale_after_seconds
    OR NEW.state IS NOT OLD.state
    OR NEW.due_from_at IS NOT OLD.due_from_at
    OR NEW.due_from_at_epoch_ms IS NOT OLD.due_from_at_epoch_ms
    OR NEW.provenance_kind IS NOT OLD.provenance_kind
    OR NEW.effective_from_at IS NOT OLD.effective_from_at
    OR NEW.effective_from_at_epoch_ms IS NOT OLD.effective_from_at_epoch_ms
    OR NEW.activated_at IS NOT OLD.activated_at
    OR NEW.paused_at IS NOT OLD.paused_at
    OR NEW.resumed_at IS NOT OLD.resumed_at
    OR NEW.archived_at IS NOT OLD.archived_at
    OR NEW.created_at IS NOT OLD.created_at
    OR (OLD.state = 'enabled' AND (NEW.due_until_at IS NULL OR NEW.due_until_at_epoch_ms IS NULL))
    OR (OLD.state IN ('paused', 'archived') AND
        (NEW.due_until_at IS NOT NULL OR NEW.due_until_at_epoch_ms IS NOT NULL))
BEGIN
    SELECT RAISE(ABORT, 'monitor_schedule_versions are immutable except for one close');
END;

INSERT INTO monitor_schedule_versions(
    schedule_version_id, schedule_id, host_id, revision, profile,
    interval_seconds, jitter_seconds, jitter_offset_seconds, stale_after_seconds,
    state, due_from_at, due_from_at_epoch_ms,
    effective_from_at, effective_from_at_epoch_ms, provenance_kind,
    activated_at, paused_at, archived_at, created_at
)
SELECT
    schedule_id || ':revision:' || revision || ':legacy',
    schedule_id, host_id, revision, profile,
    interval_seconds, jitter_seconds, jitter_offset_seconds, stale_after_seconds,
    state,
    CASE WHEN state = 'enabled' THEN next_due_at ELSE NULL END,
    CASE WHEN state = 'enabled' THEN CAST(strftime('%s', next_due_at) AS INTEGER) * 1000 ELSE NULL END,
    metadata.provenance_started_at,
    metadata.provenance_started_at_epoch_ms,
    'legacy_baseline',
    CASE WHEN state = 'enabled' THEN metadata.provenance_started_at ELSE NULL END,
    CASE WHEN state = 'paused' THEN metadata.provenance_started_at ELSE NULL END,
    CASE WHEN state = 'archived' THEN metadata.provenance_started_at ELSE NULL END,
    metadata.created_at
FROM monitor_schedules
CROSS JOIN monitor_schedule_provenance_metadata AS metadata
WHERE metadata.singleton_id = 1;

CREATE TABLE health_policy_versions (
    policy_version_id TEXT PRIMARY KEY CHECK (length(policy_version_id) = 36),
    policy_id TEXT NOT NULL CHECK (length(policy_id) = 36),
    host_id TEXT NOT NULL REFERENCES hosts(host_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    lifecycle_state TEXT NOT NULL CHECK (lifecycle_state IN ('current', 'superseded')),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    source_kind TEXT NOT NULL CHECK (source_kind IN ('user_confirmed')),
    policy_json TEXT NOT NULL CHECK (json_valid(policy_json) AND json_type(policy_json) = 'object'),
    policy_sha256 TEXT NOT NULL CHECK (length(policy_sha256) = 64),
    effective_from_at TEXT NOT NULL,
    effective_from_at_epoch_ms INTEGER NOT NULL CHECK (effective_from_at_epoch_ms >= 0),
    effective_until_at TEXT,
    effective_until_at_epoch_ms INTEGER CHECK (
        effective_until_at_epoch_ms IS NULL OR effective_until_at_epoch_ms >= effective_from_at_epoch_ms
    ),
    created_by TEXT NOT NULL CHECK (length(created_by) BETWEEN 1 AND 128),
    created_at TEXT NOT NULL,
    UNIQUE(policy_id, revision),
    UNIQUE(host_id, revision),
    UNIQUE(policy_version_id, host_id),
    UNIQUE(policy_version_id, host_id, policy_id, revision),
    CHECK (
        (lifecycle_state = 'current' AND effective_until_at IS NULL AND effective_until_at_epoch_ms IS NULL)
        OR (lifecycle_state = 'superseded' AND effective_until_at IS NOT NULL AND effective_until_at_epoch_ms IS NOT NULL)
    )
);

CREATE UNIQUE INDEX health_policy_versions_current_host_idx
ON health_policy_versions(host_id)
WHERE lifecycle_state = 'current';

CREATE INDEX health_policy_versions_host_effective_idx
ON health_policy_versions(host_id, effective_from_at_epoch_ms, effective_until_at_epoch_ms);

CREATE TRIGGER health_policy_versions_single_identity
BEFORE INSERT ON health_policy_versions
WHEN EXISTS (
    SELECT 1 FROM health_policy_versions
    WHERE host_id = NEW.host_id AND policy_id <> NEW.policy_id
)
BEGIN
    SELECT RAISE(ABORT, 'health policy identity cannot change for a host');
END;

CREATE TRIGGER health_policy_versions_close_only
BEFORE UPDATE ON health_policy_versions
WHEN
    OLD.lifecycle_state <> 'current'
    OR NEW.lifecycle_state <> 'superseded'
    OR NEW.effective_until_at IS NULL
    OR NEW.effective_until_at_epoch_ms IS NULL
    OR NEW.policy_version_id IS NOT OLD.policy_version_id
    OR NEW.policy_id IS NOT OLD.policy_id
    OR NEW.host_id IS NOT OLD.host_id
    OR NEW.revision IS NOT OLD.revision
    OR NEW.enabled IS NOT OLD.enabled
    OR NEW.source_kind IS NOT OLD.source_kind
    OR NEW.policy_json IS NOT OLD.policy_json
    OR NEW.policy_sha256 IS NOT OLD.policy_sha256
    OR NEW.effective_from_at IS NOT OLD.effective_from_at
    OR NEW.effective_from_at_epoch_ms IS NOT OLD.effective_from_at_epoch_ms
    OR NEW.created_by IS NOT OLD.created_by
    OR NEW.created_at IS NOT OLD.created_at
BEGIN
    SELECT RAISE(ABORT, 'health_policy_versions are immutable except for one close');
END;

ALTER TABLE monitor_runs ADD COLUMN schedule_version_id TEXT
    REFERENCES monitor_schedule_versions(schedule_version_id)
    DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE monitor_runs ADD COLUMN health_policy_version_id TEXT
    REFERENCES health_policy_versions(policy_version_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX monitor_runs_schedule_version_idx
ON monitor_runs(schedule_version_id, scheduled_for);

CREATE INDEX monitor_runs_health_policy_version_idx
ON monitor_runs(health_policy_version_id, finished_at);

CREATE TRIGGER monitor_runs_schedule_version_host_insert
BEFORE INSERT ON monitor_runs
WHEN NEW.schedule_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM monitor_schedule_versions
    WHERE schedule_version_id = NEW.schedule_version_id
      AND schedule_id = NEW.schedule_id
      AND revision = NEW.schedule_revision
      AND host_id = NEW.host_id
)
BEGIN
    SELECT RAISE(ABORT, 'monitor run schedule version must match host, schedule and revision');
END;

CREATE TRIGGER monitor_runs_schedule_version_host_update
BEFORE UPDATE OF schedule_version_id, schedule_id, schedule_revision, host_id ON monitor_runs
WHEN NEW.schedule_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM monitor_schedule_versions
    WHERE schedule_version_id = NEW.schedule_version_id
      AND schedule_id = NEW.schedule_id
      AND revision = NEW.schedule_revision
      AND host_id = NEW.host_id
)
BEGIN
    SELECT RAISE(ABORT, 'monitor run schedule version must match host, schedule and revision');
END;

CREATE TRIGGER monitor_runs_policy_version_host_insert
BEFORE INSERT ON monitor_runs
WHEN NEW.health_policy_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM health_policy_versions
    WHERE policy_version_id = NEW.health_policy_version_id AND host_id = NEW.host_id
)
BEGIN
    SELECT RAISE(ABORT, 'monitor run policy version must match host');
END;

CREATE TRIGGER monitor_runs_policy_version_host_update
BEFORE UPDATE OF health_policy_version_id, host_id ON monitor_runs
WHEN NEW.health_policy_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM health_policy_versions
    WHERE policy_version_id = NEW.health_policy_version_id AND host_id = NEW.host_id
)
BEGIN
    SELECT RAISE(ABORT, 'monitor run policy version must match host');
END;

CREATE TABLE health_evaluations (
    evaluation_id TEXT PRIMARY KEY CHECK (length(evaluation_id) = 36),
    run_id TEXT NOT NULL UNIQUE,
    host_id TEXT NOT NULL,
    policy_version_id TEXT,
    policy_id TEXT,
    policy_revision INTEGER CHECK (policy_revision IS NULL OR policy_revision >= 1),
    status TEXT NOT NULL CHECK (status IN ('healthy', 'degraded', 'unhealthy', 'unknown')),
    reason_code TEXT NOT NULL CHECK (length(reason_code) BETWEEN 1 AND 128),
    required_condition_count INTEGER NOT NULL DEFAULT 0 CHECK (required_condition_count >= 0),
    optional_condition_count INTEGER NOT NULL DEFAULT 0 CHECK (optional_condition_count >= 0),
    ok_count INTEGER NOT NULL DEFAULT 0 CHECK (ok_count >= 0),
    warning_count INTEGER NOT NULL DEFAULT 0 CHECK (warning_count >= 0),
    critical_count INTEGER NOT NULL DEFAULT 0 CHECK (critical_count >= 0),
    unknown_count INTEGER NOT NULL DEFAULT 0 CHECK (unknown_count >= 0),
    observation_state TEXT NOT NULL CHECK (observation_state IN ('complete', 'partial', 'none')),
    input_sha256 TEXT NOT NULL CHECK (length(input_sha256) = 64),
    evaluated_at TEXT NOT NULL,
    evaluated_at_epoch_ms INTEGER NOT NULL CHECK (evaluated_at_epoch_ms >= 0),
    observed_at TEXT,
    observed_at_epoch_ms INTEGER CHECK (observed_at_epoch_ms IS NULL OR observed_at_epoch_ms >= 0),
    valid_until TEXT,
    valid_until_epoch_ms INTEGER CHECK (
        valid_until_epoch_ms IS NULL OR (
            observed_at_epoch_ms IS NOT NULL AND valid_until_epoch_ms >= observed_at_epoch_ms
        )
    ),
    created_at TEXT NOT NULL,
    FOREIGN KEY(run_id, host_id) REFERENCES monitor_runs(run_id, host_id) ON DELETE CASCADE,
    FOREIGN KEY(policy_version_id, host_id, policy_id, policy_revision)
        REFERENCES health_policy_versions(policy_version_id, host_id, policy_id, revision)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (policy_version_id IS NULL AND policy_id IS NULL AND policy_revision IS NULL)
        OR (policy_version_id IS NOT NULL AND policy_id IS NOT NULL AND policy_revision IS NOT NULL)
    ),
    CHECK (
        ok_count + warning_count + critical_count + unknown_count
        = required_condition_count + optional_condition_count
    ),
    CHECK (
        (observation_state = 'none' AND observed_at IS NULL
            AND observed_at_epoch_ms IS NULL AND valid_until IS NULL
            AND valid_until_epoch_ms IS NULL)
        OR (observation_state IN ('complete', 'partial') AND observed_at IS NOT NULL
            AND observed_at_epoch_ms IS NOT NULL AND valid_until IS NOT NULL
            AND valid_until_epoch_ms IS NOT NULL)
    )
);

CREATE INDEX health_evaluations_host_observed_idx
ON health_evaluations(host_id, evaluated_at_epoch_ms, evaluation_id);

CREATE TRIGGER health_evaluations_reject_update
BEFORE UPDATE ON health_evaluations
BEGIN
    SELECT RAISE(ABORT, 'health_evaluations are immutable');
END;

CREATE TABLE health_condition_evaluations (
    condition_evaluation_id TEXT PRIMARY KEY CHECK (length(condition_evaluation_id) = 36),
    evaluation_id TEXT NOT NULL REFERENCES health_evaluations(evaluation_id) ON DELETE CASCADE,
    condition_key TEXT NOT NULL CHECK (length(condition_key) BETWEEN 1 AND 128),
    condition_kind TEXT NOT NULL CHECK (condition_kind IN (
        'cpu_busy_instant_percent',
        'memory_available_ratio',
        'normalized_load5',
        'filesystem_allocatable_used_ratio'
    )),
    requirement TEXT NOT NULL CHECK (requirement IN ('required', 'optional')),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('host', 'filesystem')),
    subject_id TEXT NOT NULL CHECK (length(subject_id) BETWEEN 1 AND 512),
    subject_label TEXT NOT NULL CHECK (length(subject_label) BETWEEN 1 AND 512),
    status TEXT NOT NULL CHECK (status IN ('ok', 'warning', 'critical', 'unknown')),
    candidate_status TEXT NOT NULL CHECK (candidate_status IN ('ok', 'warning', 'critical', 'unknown')),
    reason_code TEXT NOT NULL CHECK (length(reason_code) BETWEEN 1 AND 128),
    value_real REAL,
    unit TEXT NOT NULL CHECK (length(unit) BETWEEN 1 AND 64),
    window_seconds REAL CHECK (window_seconds IS NULL OR window_seconds > 0),
    streak_count INTEGER NOT NULL CHECK (streak_count >= 0),
    streak_required INTEGER NOT NULL CHECK (streak_required >= 1),
    evidence_refs_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(evidence_refs_json) AND json_type(evidence_refs_json) = 'array'),
    input_sha256 TEXT NOT NULL CHECK (length(input_sha256) = 64),
    created_at TEXT NOT NULL,
    UNIQUE(evaluation_id, condition_key),
    CHECK (
        (candidate_status = 'unknown' AND value_real IS NULL)
        OR (candidate_status <> 'unknown' AND value_real IS NOT NULL)
    )
);

CREATE INDEX health_condition_evaluations_continuity_idx
ON health_condition_evaluations(evaluation_id, condition_key);

CREATE TRIGGER health_condition_evaluations_reject_update
BEFORE UPDATE ON health_condition_evaluations
BEGIN
    SELECT RAISE(ABORT, 'health_condition_evaluations are immutable');
END;
