use std::{
    collections::BTreeMap,
    env,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use tokio::{
    sync::{RwLock, watch},
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        MonitoringHistoryMaintenanceState, MonitoringHistoryMaintenanceStatus,
        MonitoringHistoryRetentionPolicy,
    },
};

const HOUR_MILLISECONDS: i64 = 60 * 60 * 1_000;
const DAY_MILLISECONDS: i64 = 24 * HOUR_MILLISECONDS;
const DEFAULT_TICK_SECONDS: u32 = 3_600;
const DEFAULT_PARTITIONS_PER_TICK: u32 = 32;
const DEFAULT_DELETE_BATCH_SIZE: u32 = 5_000;
const DEFAULT_RAW_OBSERVED_RETENTION_DAYS: u32 = 7;
const DEFAULT_RAW_NON_OBSERVED_RETENTION_DAYS: u32 = 30;
const DEFAULT_HOUR_RETENTION_DAYS: u32 = 90;
const DEFAULT_DAY_RETENTION_DAYS: u32 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RollupResolution {
    Hour,
    Day,
}

impl RollupResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    pub fn bucket_milliseconds(self) -> i64 {
        match self {
            Self::Hour => HOUR_MILLISECONDS,
            Self::Day => DAY_MILLISECONDS,
        }
    }

    pub fn bucket_seconds(self) -> u32 {
        u32::try_from(self.bucket_milliseconds() / 1_000).unwrap_or(u32::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollupSettings {
    pub tick_seconds: u32,
    pub partitions_per_tick: u32,
    pub delete_batch_size: u32,
    pub retention_enabled: bool,
    pub raw_observed_retention_days: u32,
    pub raw_non_observed_retention_days: u32,
    pub hour_retention_days: u32,
    pub day_retention_days: u32,
}

impl Default for RollupSettings {
    fn default() -> Self {
        Self {
            tick_seconds: DEFAULT_TICK_SECONDS,
            partitions_per_tick: DEFAULT_PARTITIONS_PER_TICK,
            delete_batch_size: DEFAULT_DELETE_BATCH_SIZE,
            // The retention engine is implemented, but destructive cleanup is
            // opt-in until real multi-HOST growth and restore drills establish
            // an evidence-based budget.
            retention_enabled: false,
            raw_observed_retention_days: DEFAULT_RAW_OBSERVED_RETENTION_DAYS,
            raw_non_observed_retention_days: DEFAULT_RAW_NON_OBSERVED_RETENTION_DAYS,
            hour_retention_days: DEFAULT_HOUR_RETENTION_DAYS,
            day_retention_days: DEFAULT_DAY_RETENTION_DAYS,
        }
    }
}

impl RollupSettings {
    pub fn from_environment() -> Self {
        let defaults = Self::default();
        let raw_observed_retention_days = env_u32(
            "NETWORK_ATLAS_MONITOR_RAW_OBSERVED_RETENTION_DAYS",
            defaults.raw_observed_retention_days,
            1,
            365,
        );
        let raw_non_observed_retention_days = env_u32(
            "NETWORK_ATLAS_MONITOR_RAW_NON_OBSERVED_RETENTION_DAYS",
            defaults
                .raw_non_observed_retention_days
                .max(raw_observed_retention_days),
            raw_observed_retention_days,
            730,
        );
        let hour_retention_days = env_u32(
            "NETWORK_ATLAS_MONITOR_HOUR_RETENTION_DAYS",
            defaults
                .hour_retention_days
                .max(raw_non_observed_retention_days),
            raw_non_observed_retention_days,
            3_650,
        );
        let day_retention_days = env_u32(
            "NETWORK_ATLAS_MONITOR_DAY_RETENTION_DAYS",
            defaults.day_retention_days.max(hour_retention_days),
            hour_retention_days,
            7_300,
        );
        Self {
            tick_seconds: env_u32(
                "NETWORK_ATLAS_MONITOR_COMPACTOR_TICK_SECONDS",
                defaults.tick_seconds,
                60,
                86_400,
            ),
            partitions_per_tick: env_u32(
                "NETWORK_ATLAS_MONITOR_COMPACTOR_PARTITIONS_PER_TICK",
                defaults.partitions_per_tick,
                1,
                1_024,
            ),
            delete_batch_size: env_u32(
                "NETWORK_ATLAS_MONITOR_RETENTION_DELETE_BATCH_SIZE",
                defaults.delete_batch_size,
                1,
                100_000,
            ),
            retention_enabled: env_bool(
                "NETWORK_ATLAS_MONITOR_RETENTION_ENABLED",
                defaults.retention_enabled,
            ),
            raw_observed_retention_days,
            raw_non_observed_retention_days,
            hour_retention_days,
            day_retention_days,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RuntimeHealth {
    last_successful_at: Option<DateTime<Utc>>,
    last_error_at: Option<DateTime<Utc>>,
    last_error_code: Option<String>,
    consecutive_failures: u32,
}

#[derive(Debug)]
pub struct MonitoringRollupRuntime {
    started: AtomicBool,
    running: AtomicBool,
    settings: RollupSettings,
    health: RwLock<RuntimeHealth>,
}

impl MonitoringRollupRuntime {
    pub fn new(settings: RollupSettings) -> Self {
        Self {
            started: AtomicBool::new(false),
            running: AtomicBool::new(false),
            settings,
            health: RwLock::new(RuntimeHealth::default()),
        }
    }

    pub fn settings(&self) -> RollupSettings {
        self.settings
    }
}

pub async fn maintenance_status(
    state: &AppState,
) -> Result<MonitoringHistoryMaintenanceStatus, RollupError> {
    let row = sqlx::query(
        "SELECT state, last_started_at, last_completed_at, last_successful_at,
                last_error_at, last_error_code, last_hour_partition_count,
                last_day_partition_count, last_raw_deleted_count,
                last_hour_deleted_count, last_day_deleted_count
         FROM monitoring_history_maintenance WHERE singleton_id = 1",
    )
    .fetch_one(&state.pool)
    .await?;
    let settings = state.monitoring_rollup.settings();
    let runtime_health = state.monitoring_rollup.health.read().await.clone();
    let storage = storage_counts(&state.pool).await?;
    let persisted_state: String = row.try_get("state")?;
    let persisted_state = match persisted_state.as_str() {
        "idle" => MonitoringHistoryMaintenanceState::Idle,
        "running" => MonitoringHistoryMaintenanceState::Running,
        "succeeded" => MonitoringHistoryMaintenanceState::Succeeded,
        "failed" => MonitoringHistoryMaintenanceState::Failed,
        _ => return Err(RollupError::InvalidData),
    };
    Ok(MonitoringHistoryMaintenanceStatus {
        state: persisted_state,
        runtime_started: state.monitoring_rollup.started.load(Ordering::Acquire),
        runtime_running: state.monitoring_rollup.running.load(Ordering::Acquire),
        last_started_at: row.try_get("last_started_at")?,
        last_completed_at: row.try_get("last_completed_at")?,
        last_successful_at: row.try_get("last_successful_at")?,
        last_error_at: row.try_get("last_error_at")?,
        last_error_code: row.try_get("last_error_code")?,
        consecutive_runtime_failures: runtime_health.consecutive_failures,
        last_hour_partition_count: row_count(&row, "last_hour_partition_count")?,
        last_day_partition_count: row_count(&row, "last_day_partition_count")?,
        last_raw_deleted_count: row_count(&row, "last_raw_deleted_count")?,
        last_hour_deleted_count: row_count(&row, "last_hour_deleted_count")?,
        last_day_deleted_count: row_count(&row, "last_day_deleted_count")?,
        raw_sample_count: count_value(storage.raw_sample_count),
        hour_rollup_count: count_value(storage.hour_rollup_count),
        day_rollup_count: count_value(storage.day_rollup_count),
        database_bytes: u64::try_from(storage.database_bytes.max(0)).unwrap_or(u64::MAX),
        tick_seconds: settings.tick_seconds,
        partitions_per_tick: settings.partitions_per_tick,
        retention: MonitoringHistoryRetentionPolicy {
            enabled: settings.retention_enabled,
            raw_observed_days: settings.raw_observed_retention_days,
            raw_non_observed_days: settings.raw_non_observed_retention_days,
            hour_days: settings.hour_retention_days,
            day_days: settings.day_retention_days,
            delete_batch_size: settings.delete_batch_size,
        },
    })
}

fn row_count(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<u32, RollupError> {
    Ok(count_value(row.try_get(field)?))
}

fn count_value(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CompactionOutcome {
    pub hour_partition_count: u32,
    pub day_partition_count: u32,
    pub raw_deleted_count: u32,
    pub hour_deleted_count: u32,
    pub day_deleted_count: u32,
}

#[derive(Debug, Error)]
pub enum RollupError {
    #[error("monitoring compactor is already running")]
    AlreadyRunning,
    #[error("monitoring compactor storage failed")]
    Storage(#[from] sqlx::Error),
    #[error("monitoring compactor found invalid persisted data")]
    InvalidData,
    #[error("monitoring compactor arithmetic overflow")]
    Overflow,
    #[error("monitoring compactor found {0} late samples in sealed partitions")]
    LateSamples(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    family: String,
    subject_kind: String,
    subject_id: String,
    metric_name: String,
    dimensions_sha256: String,
    sample_kind: String,
    source_kind: String,
    unit: String,
}

#[derive(Debug, Clone)]
struct RawRollupSample {
    sample_id: String,
    run_id: String,
    key: SeriesKey,
    dimensions_json: String,
    value_real: Option<f64>,
    value_integer: Option<i64>,
    quality: String,
    observed_at: String,
    observed_at_epoch_ms: i64,
    boot_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CounterBoundary {
    sample_id: String,
    value_integer: i64,
    boot_id: Option<String>,
}

#[derive(Debug, Clone)]
struct SeriesRollup {
    key: SeriesKey,
    dimensions_json: String,
    sample_count: u32,
    observed_count: u32,
    min_value: Option<f64>,
    max_value: Option<f64>,
    average_value: Option<f64>,
    p95_value: Option<f64>,
    last_value: Option<f64>,
    counter_first: Option<i64>,
    counter_last: Option<i64>,
    counter_delta: Option<i64>,
    reset_count: u32,
    quality: String,
    quality_counts_json: String,
    input_digest: String,
    boundary_sample_id: Option<String>,
    last_sample_id: String,
    last_run_id: String,
    last_sample_at: String,
    last_sample_at_epoch_ms: i64,
    last_valid_sample_at: Option<String>,
    last_valid_sample_at_epoch_ms: Option<i64>,
    last_valid_run_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PartitionCandidate {
    host_id: String,
    bucket_start_epoch_ms: i64,
    input_count: u32,
}

#[derive(Debug, Clone)]
struct BucketBounds {
    start_epoch_ms: i64,
    end_epoch_ms: i64,
    start: String,
    end: String,
}

pub fn bucket_start_epoch_ms(epoch_ms: i64, resolution: RollupResolution) -> Option<i64> {
    if epoch_ms < 0 {
        return None;
    }
    let width = resolution.bucket_milliseconds();
    Some((epoch_ms / width) * width)
}

fn bucket_bounds(
    start_epoch_ms: i64,
    resolution: RollupResolution,
) -> Result<BucketBounds, RollupError> {
    let end_epoch_ms = start_epoch_ms
        .checked_add(resolution.bucket_milliseconds())
        .ok_or(RollupError::Overflow)?;
    let start = Utc
        .timestamp_millis_opt(start_epoch_ms)
        .single()
        .ok_or(RollupError::InvalidData)?;
    let end = Utc
        .timestamp_millis_opt(end_epoch_ms)
        .single()
        .ok_or(RollupError::InvalidData)?;
    Ok(BucketBounds {
        start_epoch_ms,
        end_epoch_ms,
        start: timestamp(start),
        end: timestamp(end),
    })
}

fn aggregate_series(
    key: SeriesKey,
    mut samples: Vec<RawRollupSample>,
    boundary: Option<CounterBoundary>,
) -> Result<SeriesRollup, RollupError> {
    samples.sort_by(|left, right| {
        (left.observed_at_epoch_ms, left.sample_id.as_str())
            .cmp(&(right.observed_at_epoch_ms, right.sample_id.as_str()))
    });
    let last = samples.last().ok_or(RollupError::InvalidData)?;
    let dimensions_json = last.dimensions_json.clone();
    let sample_count = u32::try_from(samples.len()).map_err(|_| RollupError::Overflow)?;
    let observed_count = u32::try_from(
        samples
            .iter()
            .filter(|sample| sample.quality == "observed")
            .count(),
    )
    .map_err(|_| RollupError::Overflow)?;
    let mut quality_counts = BTreeMap::<String, u32>::new();
    for sample in &samples {
        let count = quality_counts.entry(sample.quality.clone()).or_default();
        *count = count.saturating_add(1);
    }
    let mut representative_quality = samples
        .iter()
        .max_by_key(|sample| quality_rank(&sample.quality))
        .map(|sample| sample.quality.clone())
        .ok_or(RollupError::InvalidData)?;

    let (
        min_value,
        max_value,
        average_value,
        p95_value,
        last_value,
        counter_first,
        counter_last,
        counter_delta,
        reset_count,
        boundary_sample_id,
    ) = if key.sample_kind == "counter" {
        let counter = aggregate_counter(&samples, boundary)?;
        if counter.overflowed {
            representative_quality = "counter_unreliable".to_owned();
        }
        (
            None,
            None,
            None,
            None,
            None,
            counter.first,
            counter.last,
            counter.delta,
            counter.reset_count,
            counter.boundary_sample_id,
        )
    } else {
        let mut values = samples
            .iter()
            .filter(|sample| sample.quality == "observed")
            .filter_map(|sample| sample.value_real)
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let statistics = gauge_statistics(&values);
        let last_value = samples
            .iter()
            .rev()
            .find(|sample| sample.quality == "observed")
            .and_then(|sample| sample.value_real)
            .filter(|value| value.is_finite());
        (
            statistics.min,
            statistics.max,
            statistics.average,
            statistics.p95,
            last_value,
            None,
            None,
            None,
            0,
            None,
        )
    };

    let input_digest = series_input_digest(&samples, boundary_sample_id.as_deref());
    let last_valid = samples
        .iter()
        .rev()
        .find(|sample| sample.quality == "observed");
    Ok(SeriesRollup {
        key,
        dimensions_json,
        sample_count,
        observed_count,
        min_value,
        max_value,
        average_value,
        p95_value,
        last_value,
        counter_first,
        counter_last,
        counter_delta,
        reset_count,
        quality: representative_quality,
        quality_counts_json: serde_json::to_string(&quality_counts)
            .map_err(|_| RollupError::InvalidData)?,
        input_digest,
        boundary_sample_id,
        last_sample_id: last.sample_id.clone(),
        last_run_id: last.run_id.clone(),
        last_sample_at: last.observed_at.clone(),
        last_sample_at_epoch_ms: last.observed_at_epoch_ms,
        last_valid_sample_at: last_valid.map(|sample| sample.observed_at.clone()),
        last_valid_sample_at_epoch_ms: last_valid.map(|sample| sample.observed_at_epoch_ms),
        last_valid_run_id: last_valid.map(|sample| sample.run_id.clone()),
    })
}

#[derive(Debug, Default)]
struct GaugeStatistics {
    min: Option<f64>,
    max: Option<f64>,
    average: Option<f64>,
    p95: Option<f64>,
}

fn gauge_statistics(sorted_values: &[f64]) -> GaugeStatistics {
    if sorted_values.is_empty() {
        return GaugeStatistics::default();
    }
    // Adding individually finite values can still overflow to +/-INF. Scale
    // first so the mean remains inside the finite input range.
    let scale = sorted_values
        .iter()
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);
    let average = if scale == 0.0 {
        0.0
    } else {
        let normalized_sum = sorted_values.iter().map(|value| value / scale).sum::<f64>();
        ((normalized_sum / sorted_values.len() as f64).clamp(-1.0, 1.0)) * scale
    };
    let rank = (sorted_values.len().saturating_mul(95).saturating_add(99) / 100)
        .saturating_sub(1)
        .min(sorted_values.len() - 1);
    GaugeStatistics {
        min: sorted_values.first().copied(),
        max: sorted_values.last().copied(),
        average: average.is_finite().then_some(average),
        p95: sorted_values.get(rank).copied(),
    }
}

#[derive(Debug, Default)]
struct CounterStatistics {
    first: Option<i64>,
    last: Option<i64>,
    delta: Option<i64>,
    reset_count: u32,
    boundary_sample_id: Option<String>,
    overflowed: bool,
}

fn aggregate_counter(
    samples: &[RawRollupSample],
    boundary: Option<CounterBoundary>,
) -> Result<CounterStatistics, RollupError> {
    let observed = samples
        .iter()
        .filter(|sample| sample.quality == "observed")
        .filter_map(|sample| sample.value_integer.map(|value| (sample, value)))
        .collect::<Vec<_>>();
    let Some((_, first_value)) = observed.first().copied() else {
        return Ok(CounterStatistics::default());
    };

    let mut previous = boundary
        .as_ref()
        .map(|value| (value.value_integer, value.boot_id.as_deref()));
    let mut delta = 0i64;
    let mut transition_count = 0u32;
    let mut reset_count = 0u32;
    let mut overflowed = false;
    for (sample, value) in &observed {
        if let Some((previous_value, previous_boot_id)) = previous {
            let boot_changed = previous_boot_id.is_some()
                && sample.boot_id.as_deref().is_some()
                && previous_boot_id != sample.boot_id.as_deref();
            if boot_changed || *value < previous_value {
                reset_count = reset_count.saturating_add(1);
            } else {
                let increment = *value - previous_value;
                match delta.checked_add(increment) {
                    Some(next) => {
                        delta = next;
                        transition_count = transition_count.saturating_add(1);
                    }
                    None => overflowed = true,
                }
            }
        }
        previous = Some((*value, sample.boot_id.as_deref()));
    }
    let last_value = observed.last().map(|(_, value)| *value);
    Ok(CounterStatistics {
        first: Some(first_value),
        last: last_value,
        delta: (!overflowed && transition_count > 0).then_some(delta),
        reset_count,
        boundary_sample_id: boundary.map(|value| value.sample_id),
        overflowed,
    })
}

fn quality_rank(quality: &str) -> u8 {
    match quality {
        "observed" => 0,
        "unsupported" => 1,
        "insufficient_interval" => 2,
        "counter_reset" => 3,
        "counter_unreliable" => 4,
        "parse_failed" => 5,
        "permission_denied" => 6,
        "timed_out" => 7,
        _ => u8::MAX,
    }
}

fn series_input_digest(samples: &[RawRollupSample], boundary_sample_id: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, boundary_sample_id.unwrap_or(""));
    for sample in samples {
        digest_sample(&mut digest, sample);
    }
    hex_digest(&digest.finalize())
}

fn partition_input_digest(samples: &[RawRollupSample]) -> String {
    let mut digest = Sha256::new();
    for sample in samples {
        digest_sample(&mut digest, sample);
    }
    hex_digest(&digest.finalize())
}

fn digest_sample(digest: &mut Sha256, sample: &RawRollupSample) {
    for field in [
        sample.sample_id.as_str(),
        sample.run_id.as_str(),
        sample.key.family.as_str(),
        sample.key.subject_kind.as_str(),
        sample.key.subject_id.as_str(),
        sample.key.metric_name.as_str(),
        sample.dimensions_json.as_str(),
        sample.key.dimensions_sha256.as_str(),
        sample.key.sample_kind.as_str(),
        sample.key.source_kind.as_str(),
        sample.key.unit.as_str(),
        sample.quality.as_str(),
        sample.observed_at.as_str(),
        sample.boot_id.as_deref().unwrap_or(""),
    ] {
        digest_field(digest, field);
    }
    digest_field(digest, &sample.observed_at_epoch_ms.to_string());
    digest_field(
        digest,
        &sample
            .value_real
            .map(f64::to_bits)
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    digest_field(
        digest,
        &sample
            .value_integer
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
}

fn digest_field(digest: &mut Sha256, value: &str) {
    digest.update(value.len().to_be_bytes());
    digest.update(value.as_bytes());
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

pub async fn recover_interrupted_compactions(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let finished_at = timestamp(Utc::now());
    let result = sqlx::query(
        "UPDATE monitoring_compaction_runs
         SET state = 'interrupted', finished_at = ?, error_code = 'APP_RESTARTED'
         WHERE state = 'running'",
    )
    .bind(&finished_at)
    .execute(pool)
    .await?;
    let code = if result.rows_affected() > 0 {
        "APP_RESTARTED"
    } else {
        "COMPACTOR_TERMINAL_SYNC_FAILED"
    };
    repair_orphaned_maintenance(pool, &finished_at, code).await?;
    Ok(result.rows_affected())
}

async fn repair_orphaned_maintenance(
    pool: &SqlitePool,
    completed_at: &str,
    code: &str,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE monitoring_history_maintenance
         SET state = 'failed', last_completed_at = ?, last_error_at = ?,
             last_error_code = ?, updated_at = ?
         WHERE singleton_id = 1 AND state = 'running'
           AND NOT EXISTS (
               SELECT 1 FROM monitoring_compaction_runs WHERE state = 'running'
           )",
    )
    .bind(completed_at)
    .bind(completed_at)
    .bind(code)
    .bind(completed_at)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

pub fn spawn(state: AppState, mut shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    state
        .monitoring_rollup
        .started
        .store(true, Ordering::Release);
    tokio::spawn(async move {
        let settings = state.monitoring_rollup.settings();
        let mut ticker = interval(StdDuration::from_secs(u64::from(settings.tick_seconds)));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // The first immediate interval tick performs startup catch-up. It only
        // reads local SQLite and never starts SSH or modifies TARGET_HOST.
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if state.shutdown_requested.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(error) = compact_once(&state, Utc::now()).await
                        && !matches!(error, RollupError::AlreadyRunning)
                    {
                        tracing::error!(error = %error, "monitoring history compaction failed");
                    }
                }
            }
        }
        state
            .monitoring_rollup
            .started
            .store(false, Ordering::Release);
    })
}

pub async fn compact_once(
    state: &AppState,
    evaluated_at: DateTime<Utc>,
) -> Result<CompactionOutcome, RollupError> {
    if state
        .monitoring_rollup
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(RollupError::AlreadyRunning);
    }
    let result = compact_once_inner(state, evaluated_at).await;
    state
        .monitoring_rollup
        .running
        .store(false, Ordering::Release);
    let mut health = state.monitoring_rollup.health.write().await;
    match &result {
        Ok(_) => {
            health.last_successful_at = Some(Utc::now());
            health.consecutive_failures = 0;
        }
        Err(error) => {
            health.last_error_at = Some(Utc::now());
            health.last_error_code = Some(error_code(error).to_owned());
            health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        }
    }
    result
}

async fn compact_once_inner(
    state: &AppState,
    evaluated_at: DateTime<Utc>,
) -> Result<CompactionOutcome, RollupError> {
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Ok(CompactionOutcome::default());
    }
    // DELETE obtains the write side of the same gate before its verified
    // backup. Keeping this read guard through rollup and retention writes makes
    // the backup a complete boundary for both raw and compacted history.
    let _admission = state.observation_admission.read().await;
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Ok(CompactionOutcome::default());
    }
    let settings = state.monitoring_rollup.settings();
    let run_id = Uuid::new_v4().to_string();
    let started_at = timestamp(Utc::now());
    repair_orphaned_maintenance(&state.pool, &started_at, "COMPACTOR_TERMINAL_SYNC_FAILED").await?;
    let settings_json = serde_json::to_string(&json!({
        "retention_enabled": settings.retention_enabled,
        "raw_observed_retention_days": settings.raw_observed_retention_days,
        "raw_non_observed_retention_days": settings.raw_non_observed_retention_days,
        "hour_retention_days": settings.hour_retention_days,
        "day_retention_days": settings.day_retention_days,
        "partitions_per_tick": settings.partitions_per_tick,
        "delete_batch_size": settings.delete_batch_size,
    }))
    .map_err(|_| RollupError::InvalidData)?;
    let insert = sqlx::query(
        "INSERT INTO monitoring_compaction_runs(
            compaction_run_id, state, started_at, settings_json
         ) VALUES (?, 'running', ?, ?)",
    )
    .bind(&run_id)
    .bind(&started_at)
    .bind(&settings_json)
    .execute(&state.pool)
    .await;
    if let Err(error) = insert {
        if error.to_string().to_ascii_lowercase().contains("unique") {
            return Err(RollupError::AlreadyRunning);
        }
        return Err(RollupError::Storage(error));
    }
    let maintenance_start = sqlx::query(
        "UPDATE monitoring_history_maintenance
         SET state = 'running', retention_enabled = ?,
             raw_observed_retention_days = ?, raw_non_observed_retention_days = ?,
             hour_retention_days = ?, day_retention_days = ?,
             last_started_at = ?, last_error_code = NULL, updated_at = ?
         WHERE singleton_id = 1",
    )
    .bind(settings.retention_enabled)
    .bind(i64::from(settings.raw_observed_retention_days))
    .bind(i64::from(settings.raw_non_observed_retention_days))
    .bind(i64::from(settings.hour_retention_days))
    .bind(i64::from(settings.day_retention_days))
    .bind(&started_at)
    .bind(&started_at)
    .execute(&state.pool)
    .await;
    if let Err(error) = maintenance_start {
        let rollup_error = RollupError::Storage(error);
        let _ = mark_failed(
            &state.pool,
            &run_id,
            &timestamp(Utc::now()),
            error_code(&rollup_error),
        )
        .await;
        return Err(rollup_error);
    }

    let mut outcome = CompactionOutcome::default();
    let work = async {
        outcome.hour_partition_count = compact_resolution(
            state,
            RollupResolution::Hour,
            evaluated_at,
            settings.partitions_per_tick,
        )
        .await?;
        if !state.shutdown_requested.load(Ordering::Acquire) {
            outcome.day_partition_count = compact_resolution(
                state,
                RollupResolution::Day,
                evaluated_at,
                settings.partitions_per_tick,
            )
            .await?;
        }
        let compaction_backlog = has_pending_closed_partitions(&state.pool, evaluated_at).await?;
        if settings.retention_enabled
            && !compaction_backlog
            && !state.shutdown_requested.load(Ordering::Acquire)
        {
            let deleted = enforce_retention(&state.pool, evaluated_at, settings).await?;
            outcome.raw_deleted_count = deleted.raw_deleted_count;
            outcome.hour_deleted_count = deleted.hour_deleted_count;
            outcome.day_deleted_count = deleted.day_deleted_count;
        }
        let late_sample_count = count_late_samples(&state.pool).await?;
        if late_sample_count > 0 {
            return Err(RollupError::LateSamples(late_sample_count));
        }
        storage_counts(&state.pool)
            .await
            .map_err(RollupError::Storage)
    }
    .await;
    let completed_at = timestamp(Utc::now());
    match work {
        Ok(storage) => {
            let terminal =
                mark_succeeded(&state.pool, &run_id, &completed_at, outcome, storage).await;
            if let Err(error) = terminal {
                let rollup_error = RollupError::Storage(error);
                let _ = mark_failed(
                    &state.pool,
                    &run_id,
                    &completed_at,
                    error_code(&rollup_error),
                )
                .await;
                return Err(rollup_error);
            }
            Ok(outcome)
        }
        Err(error) => {
            let code = error_code(&error);
            let _ = mark_failed(&state.pool, &run_id, &completed_at, code).await;
            Err(error)
        }
    }
}

async fn mark_succeeded(
    pool: &SqlitePool,
    run_id: &str,
    completed_at: &str,
    outcome: CompactionOutcome,
    storage: StorageCounts,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE monitoring_compaction_runs
                 SET state = 'succeeded', finished_at = ?,
                     hour_partition_count = ?, day_partition_count = ?,
                     raw_deleted_count = ?, hour_deleted_count = ?, day_deleted_count = ?
                 WHERE compaction_run_id = ? AND state = 'running'",
    )
    .bind(completed_at)
    .bind(i64::from(outcome.hour_partition_count))
    .bind(i64::from(outcome.day_partition_count))
    .bind(i64::from(outcome.raw_deleted_count))
    .bind(i64::from(outcome.hour_deleted_count))
    .bind(i64::from(outcome.day_deleted_count))
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE monitoring_history_maintenance
                 SET state = 'succeeded', last_completed_at = ?, last_successful_at = ?,
                     last_error_code = NULL,
                     last_hour_partition_count = ?, last_day_partition_count = ?,
                     last_raw_deleted_count = ?, last_hour_deleted_count = ?,
                     last_day_deleted_count = ?, raw_sample_count = ?,
                     hour_rollup_count = ?, day_rollup_count = ?, database_bytes = ?,
                     updated_at = ? WHERE singleton_id = 1",
    )
    .bind(completed_at)
    .bind(completed_at)
    .bind(i64::from(outcome.hour_partition_count))
    .bind(i64::from(outcome.day_partition_count))
    .bind(i64::from(outcome.raw_deleted_count))
    .bind(i64::from(outcome.hour_deleted_count))
    .bind(i64::from(outcome.day_deleted_count))
    .bind(storage.raw_sample_count)
    .bind(storage.hour_rollup_count)
    .bind(storage.day_rollup_count)
    .bind(storage.database_bytes)
    .bind(completed_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

async fn mark_failed(
    pool: &SqlitePool,
    run_id: &str,
    completed_at: &str,
    code: &str,
) -> Result<(), sqlx::Error> {
    // Release the unique running-row fence first. If updating the singleton is
    // itself the failing storage path, the next tick can still recover/proceed.
    let run_terminal = sqlx::query(
        "UPDATE monitoring_compaction_runs
         SET state = 'failed', finished_at = ?, error_code = ?
         WHERE compaction_run_id = ? AND state = 'running'",
    )
    .bind(completed_at)
    .bind(code)
    .bind(run_id)
    .execute(pool)
    .await;
    let maintenance_terminal = sqlx::query(
        "UPDATE monitoring_history_maintenance
         SET state = 'failed', last_completed_at = ?, last_error_at = ?,
             last_error_code = ?, updated_at = ? WHERE singleton_id = 1",
    )
    .bind(completed_at)
    .bind(completed_at)
    .bind(code)
    .bind(completed_at)
    .execute(pool)
    .await;
    if run_terminal.is_ok() && maintenance_terminal.is_ok() {
        return Ok(());
    }

    // A trigger, transient constraint, or partial terminal-write error must not
    // strand either durable state as `running`. The generic fallback uses a
    // distinct code so it can survive a failure specific to the detailed code.
    sqlx::query(
        "UPDATE monitoring_compaction_runs
         SET state = 'failed', finished_at = ?,
             error_code = 'COMPACTOR_TERMINAL_SYNC_FAILED'
         WHERE compaction_run_id = ? AND state = 'running'",
    )
    .bind(completed_at)
    .bind(run_id)
    .execute(pool)
    .await?;
    repair_orphaned_maintenance(pool, completed_at, "COMPACTOR_TERMINAL_SYNC_FAILED").await?;
    Ok(())
}

fn error_code(error: &RollupError) -> &'static str {
    match error {
        RollupError::AlreadyRunning => "COMPACTOR_ALREADY_RUNNING",
        RollupError::Storage(_) => "COMPACTOR_STORAGE_FAILED",
        RollupError::InvalidData => "COMPACTOR_INVALID_DATA",
        RollupError::Overflow => "COMPACTOR_ARITHMETIC_OVERFLOW",
        RollupError::LateSamples(_) => "COMPACTOR_LATE_SAMPLE",
    }
}

async fn compact_resolution(
    state: &AppState,
    resolution: RollupResolution,
    evaluated_at: DateTime<Utc>,
    limit: u32,
) -> Result<u32, RollupError> {
    let cutoff = bucket_start_epoch_ms(evaluated_at.timestamp_millis(), resolution)
        .ok_or(RollupError::InvalidData)?;
    let candidates = load_partition_candidates(&state.pool, resolution, cutoff, limit).await?;
    let mut completed = 0u32;
    for candidate in candidates {
        if state.shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        if compact_partition(&state.pool, resolution, candidate).await? {
            completed = completed.saturating_add(1);
        }
    }
    Ok(completed)
}

async fn load_partition_candidates(
    pool: &SqlitePool,
    resolution: RollupResolution,
    cutoff_epoch_ms: i64,
    limit: u32,
) -> Result<Vec<PartitionCandidate>, RollupError> {
    let width = resolution.bucket_milliseconds();
    let rows = sqlx::query(
        "SELECT samples.host_id,
                (samples.observed_at_epoch_ms / ?) * ? AS bucket_start_epoch_ms,
                COUNT(*) AS input_count
         FROM metric_samples samples
         LEFT JOIN metric_rollup_partitions partitions
           ON partitions.host_id = samples.host_id
          AND partitions.resolution = ?
          AND partitions.bucket_start_epoch_ms =
              (samples.observed_at_epoch_ms / ?) * ?
         WHERE samples.observed_at_epoch_ms < ?
            AND (partitions.partition_id IS NULL OR (
                 partitions.state = 'complete'
                 AND (
                     partitions.input_count != (
                         SELECT COUNT(*) FROM metric_samples current_samples
                         WHERE current_samples.host_id = samples.host_id
                           AND current_samples.observed_at_epoch_ms >=
                               (samples.observed_at_epoch_ms / ?) * ?
                           AND current_samples.observed_at_epoch_ms <
                               ((samples.observed_at_epoch_ms / ?) * ?) + ?
                     )
                     OR EXISTS (
                         SELECT 1 FROM metric_samples changed_samples
                         WHERE changed_samples.host_id = samples.host_id
                           AND changed_samples.observed_at_epoch_ms >=
                               (samples.observed_at_epoch_ms / ?) * ?
                           AND changed_samples.observed_at_epoch_ms <
                               ((samples.observed_at_epoch_ms / ?) * ?) + ?
                           AND changed_samples.created_at > partitions.compacted_at
                     )
                 )
            ))
         GROUP BY samples.host_id, (samples.observed_at_epoch_ms / ?) * ?
         ORDER BY bucket_start_epoch_ms, samples.host_id
         LIMIT ?",
    )
    .bind(width)
    .bind(width)
    .bind(resolution.as_str())
    .bind(width)
    .bind(width)
    .bind(cutoff_epoch_ms)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(width)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let input_count: i64 = row.try_get("input_count")?;
            Ok(PartitionCandidate {
                host_id: row.try_get("host_id")?,
                bucket_start_epoch_ms: row.try_get("bucket_start_epoch_ms")?,
                input_count: u32::try_from(input_count).map_err(|_| RollupError::Overflow)?,
            })
        })
        .collect::<Result<Vec<_>, RollupError>>()
}

async fn compact_partition(
    pool: &SqlitePool,
    resolution: RollupResolution,
    candidate: PartitionCandidate,
) -> Result<bool, RollupError> {
    let bounds = bucket_bounds(candidate.bucket_start_epoch_ms, resolution)?;
    let mut tx = pool.begin().await?;
    let existing = sqlx::query(
        "SELECT partition_id, state, input_count, input_digest
         FROM metric_rollup_partitions
         WHERE host_id = ? AND resolution = ? AND bucket_start_epoch_ms = ?",
    )
    .bind(&candidate.host_id)
    .bind(resolution.as_str())
    .bind(bounds.start_epoch_ms)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = existing.as_ref() {
        let state: String = row.try_get("state")?;
        if state == "sealed" {
            return Err(RollupError::InvalidData);
        }
    }

    let samples = load_raw_partition(&mut tx, &candidate.host_id, &bounds).await?;
    if samples.is_empty() || samples.len() != candidate.input_count as usize {
        tx.rollback().await?;
        // A concurrent terminal observation committed after candidate
        // selection. The next tick will see the new count and retry without
        // writing a partial partition.
        return Ok(false);
    }
    let partition_digest = partition_input_digest(&samples);
    if let Some(row) = existing.as_ref() {
        let input_count: i64 = row.try_get("input_count")?;
        let input_digest: String = row.try_get("input_digest")?;
        if u32::try_from(input_count).ok() == Some(candidate.input_count)
            && input_digest == partition_digest
        {
            tx.rollback().await?;
            return Ok(false);
        }
    }
    let mut grouped = BTreeMap::<SeriesKey, Vec<RawRollupSample>>::new();
    for sample in samples {
        grouped.entry(sample.key.clone()).or_default().push(sample);
    }
    let partition_id = existing
        .as_ref()
        .and_then(|row| row.try_get::<String, _>("partition_id").ok())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let compacted_at = timestamp(Utc::now());
    let series_count = u32::try_from(grouped.len()).map_err(|_| RollupError::Overflow)?;
    sqlx::query(
        "INSERT INTO metric_rollup_partitions(
            partition_id, host_id, resolution, bucket_start, bucket_start_epoch_ms,
            bucket_end, bucket_end_epoch_ms, input_count, input_digest,
            series_count, state, compacted_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'complete', ?)
         ON CONFLICT(host_id, resolution, bucket_start_epoch_ms) DO UPDATE SET
            bucket_start = excluded.bucket_start,
            bucket_end = excluded.bucket_end,
            bucket_end_epoch_ms = excluded.bucket_end_epoch_ms,
            input_count = excluded.input_count,
            input_digest = excluded.input_digest,
            series_count = excluded.series_count,
            compacted_at = excluded.compacted_at",
    )
    .bind(&partition_id)
    .bind(&candidate.host_id)
    .bind(resolution.as_str())
    .bind(&bounds.start)
    .bind(bounds.start_epoch_ms)
    .bind(&bounds.end)
    .bind(bounds.end_epoch_ms)
    .bind(i64::from(candidate.input_count))
    .bind(&partition_digest)
    .bind(i64::from(series_count))
    .bind(&compacted_at)
    .execute(&mut *tx)
    .await?;

    for (key, series_samples) in grouped {
        let boundary = if key.sample_kind == "counter" {
            load_counter_boundary(&mut tx, &candidate.host_id, &key, bounds.start_epoch_ms).await?
        } else {
            None
        };
        let rollup = aggregate_series(key, series_samples, boundary)?;
        upsert_rollup(
            &mut tx,
            &partition_id,
            &candidate.host_id,
            resolution,
            &bounds,
            &compacted_at,
            rollup,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

async fn load_raw_partition(
    tx: &mut Transaction<'_, Sqlite>,
    host_id: &str,
    bounds: &BucketBounds,
) -> Result<Vec<RawRollupSample>, RollupError> {
    let rows = sqlx::query(
        "SELECT samples.sample_id, samples.run_id, samples.family,
                samples.subject_kind, samples.subject_id, samples.metric_name,
                samples.dimensions_json, samples.dimensions_sha256,
                samples.sample_kind, samples.source_kind, samples.unit,
                samples.value_real, samples.value_integer, samples.quality,
                samples.observed_at, samples.observed_at_epoch_ms, runs.boot_id
         FROM metric_samples samples
         JOIN monitor_runs runs ON runs.run_id = samples.run_id
         WHERE samples.host_id = ?
           AND samples.observed_at_epoch_ms >= ?
           AND samples.observed_at_epoch_ms < ?
         ORDER BY samples.observed_at_epoch_ms, samples.sample_id",
    )
    .bind(host_id)
    .bind(bounds.start_epoch_ms)
    .bind(bounds.end_epoch_ms)
    .fetch_all(&mut **tx)
    .await?;
    rows.into_iter().map(raw_sample_from_row).collect()
}

fn raw_sample_from_row(row: sqlx::sqlite::SqliteRow) -> Result<RawRollupSample, RollupError> {
    let value_real: Option<f64> = row.try_get("value_real")?;
    if value_real.is_some_and(|value| !value.is_finite()) {
        return Err(RollupError::InvalidData);
    }
    Ok(RawRollupSample {
        sample_id: row.try_get("sample_id")?,
        run_id: row.try_get("run_id")?,
        key: SeriesKey {
            family: row.try_get("family")?,
            subject_kind: row.try_get("subject_kind")?,
            subject_id: row.try_get("subject_id")?,
            metric_name: row.try_get("metric_name")?,
            dimensions_sha256: row.try_get("dimensions_sha256")?,
            sample_kind: row.try_get("sample_kind")?,
            source_kind: row.try_get("source_kind")?,
            unit: row.try_get("unit")?,
        },
        dimensions_json: row.try_get("dimensions_json")?,
        value_real,
        value_integer: row.try_get("value_integer")?,
        quality: row.try_get("quality")?,
        observed_at: row.try_get("observed_at")?,
        observed_at_epoch_ms: row.try_get("observed_at_epoch_ms")?,
        boot_id: row.try_get("boot_id")?,
    })
}

async fn load_counter_boundary(
    tx: &mut Transaction<'_, Sqlite>,
    host_id: &str,
    key: &SeriesKey,
    before_epoch_ms: i64,
) -> Result<Option<CounterBoundary>, RollupError> {
    let row = sqlx::query(
        "SELECT sample_id, value_integer, boot_id
         FROM (
             SELECT samples.sample_id, samples.value_integer, runs.boot_id,
                    samples.observed_at_epoch_ms, 0 AS source_rank
             FROM metric_samples samples
             JOIN monitor_runs runs ON runs.run_id = samples.run_id
             WHERE samples.host_id = ?
               AND samples.family = ? AND samples.subject_kind = ?
               AND samples.subject_id = ? AND samples.metric_name = ?
               AND samples.dimensions_sha256 = ? AND samples.sample_kind = ?
               AND samples.source_kind = ? AND samples.unit = ?
               AND samples.observed_at_epoch_ms < ?
               AND samples.quality = 'observed' AND samples.value_integer IS NOT NULL
             UNION ALL
             SELECT rollups.last_sample_id AS sample_id,
                    rollups.counter_last AS value_integer, runs.boot_id,
                    rollups.last_sample_at_epoch_ms AS observed_at_epoch_ms,
                    1 AS source_rank
             FROM metric_rollups rollups
             JOIN monitor_runs runs ON runs.run_id = rollups.last_run_id
             WHERE rollups.host_id = ?
               AND rollups.family = ? AND rollups.subject_kind = ?
               AND rollups.subject_id = ? AND rollups.metric_name = ?
               AND rollups.dimensions_sha256 = ? AND rollups.sample_kind = ?
               AND rollups.source_kind = ? AND rollups.unit = ?
               AND rollups.last_sample_at_epoch_ms < ?
               AND rollups.counter_last IS NOT NULL
         ) boundaries
         ORDER BY observed_at_epoch_ms DESC, sample_id DESC, source_rank
         LIMIT 1",
    )
    .bind(host_id)
    .bind(&key.family)
    .bind(&key.subject_kind)
    .bind(&key.subject_id)
    .bind(&key.metric_name)
    .bind(&key.dimensions_sha256)
    .bind(&key.sample_kind)
    .bind(&key.source_kind)
    .bind(&key.unit)
    .bind(before_epoch_ms)
    .bind(host_id)
    .bind(&key.family)
    .bind(&key.subject_kind)
    .bind(&key.subject_id)
    .bind(&key.metric_name)
    .bind(&key.dimensions_sha256)
    .bind(&key.sample_kind)
    .bind(&key.source_kind)
    .bind(&key.unit)
    .bind(before_epoch_ms)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        Ok(CounterBoundary {
            sample_id: row.try_get("sample_id")?,
            value_integer: row.try_get("value_integer")?,
            boot_id: row.try_get("boot_id")?,
        })
    })
    .transpose()
}

#[allow(clippy::too_many_arguments)]
async fn upsert_rollup(
    tx: &mut Transaction<'_, Sqlite>,
    partition_id: &str,
    host_id: &str,
    resolution: RollupResolution,
    bounds: &BucketBounds,
    updated_at: &str,
    rollup: SeriesRollup,
) -> Result<(), RollupError> {
    let non_observed_count = rollup.sample_count.saturating_sub(rollup.observed_count);
    sqlx::query(
        "INSERT INTO metric_rollups(
            rollup_id, partition_id, host_id, family, subject_kind, subject_id,
            metric_name, dimensions_json, dimensions_sha256, sample_kind,
            source_kind, unit, resolution, bucket_start, bucket_start_epoch_ms,
            bucket_end, bucket_end_epoch_ms, sample_count, observed_count,
            non_observed_count, expected_count, missing_count, min_value, max_value,
            average_value, p95_value, last_value, counter_first, counter_last,
            counter_delta, reset_count, quality, quality_counts_json, input_count,
            input_digest, boundary_sample_id, last_sample_id, last_run_id,
            last_sample_at, last_sample_at_epoch_ms, last_valid_sample_at,
            last_valid_sample_at_epoch_ms, last_valid_run_id,
            created_at, updated_at
         ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
            NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?, ?
         )
         ON CONFLICT(
            host_id, family, subject_kind, subject_id, metric_name,
            dimensions_sha256, sample_kind, source_kind, unit,
            resolution, bucket_start_epoch_ms
         ) DO UPDATE SET
            partition_id = excluded.partition_id,
            dimensions_json = excluded.dimensions_json,
            bucket_start = excluded.bucket_start,
            bucket_end = excluded.bucket_end,
            bucket_end_epoch_ms = excluded.bucket_end_epoch_ms,
            sample_count = excluded.sample_count,
            observed_count = excluded.observed_count,
            non_observed_count = excluded.non_observed_count,
            expected_count = excluded.expected_count,
            missing_count = excluded.missing_count,
            min_value = excluded.min_value,
            max_value = excluded.max_value,
            average_value = excluded.average_value,
            p95_value = excluded.p95_value,
            last_value = excluded.last_value,
            counter_first = excluded.counter_first,
            counter_last = excluded.counter_last,
            counter_delta = excluded.counter_delta,
            reset_count = excluded.reset_count,
            quality = excluded.quality,
            quality_counts_json = excluded.quality_counts_json,
            input_count = excluded.input_count,
            input_digest = excluded.input_digest,
            boundary_sample_id = excluded.boundary_sample_id,
            last_sample_id = excluded.last_sample_id,
            last_run_id = excluded.last_run_id,
            last_sample_at = excluded.last_sample_at,
            last_sample_at_epoch_ms = excluded.last_sample_at_epoch_ms,
            last_valid_sample_at = excluded.last_valid_sample_at,
            last_valid_sample_at_epoch_ms = excluded.last_valid_sample_at_epoch_ms,
            last_valid_run_id = excluded.last_valid_run_id,
            updated_at = excluded.updated_at",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(partition_id)
    .bind(host_id)
    .bind(&rollup.key.family)
    .bind(&rollup.key.subject_kind)
    .bind(&rollup.key.subject_id)
    .bind(&rollup.key.metric_name)
    .bind(&rollup.dimensions_json)
    .bind(&rollup.key.dimensions_sha256)
    .bind(&rollup.key.sample_kind)
    .bind(&rollup.key.source_kind)
    .bind(&rollup.key.unit)
    .bind(resolution.as_str())
    .bind(&bounds.start)
    .bind(bounds.start_epoch_ms)
    .bind(&bounds.end)
    .bind(bounds.end_epoch_ms)
    .bind(i64::from(rollup.sample_count))
    .bind(i64::from(rollup.observed_count))
    .bind(i64::from(non_observed_count))
    .bind(rollup.min_value)
    .bind(rollup.max_value)
    .bind(rollup.average_value)
    .bind(rollup.p95_value)
    .bind(rollup.last_value)
    .bind(rollup.counter_first)
    .bind(rollup.counter_last)
    .bind(rollup.counter_delta)
    .bind(i64::from(rollup.reset_count))
    .bind(&rollup.quality)
    .bind(&rollup.quality_counts_json)
    .bind(i64::from(rollup.sample_count))
    .bind(&rollup.input_digest)
    .bind(&rollup.boundary_sample_id)
    .bind(&rollup.last_sample_id)
    .bind(&rollup.last_run_id)
    .bind(&rollup.last_sample_at)
    .bind(rollup.last_sample_at_epoch_ms)
    .bind(&rollup.last_valid_sample_at)
    .bind(rollup.last_valid_sample_at_epoch_ms)
    .bind(&rollup.last_valid_run_id)
    .bind(updated_at)
    .bind(updated_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Debug, Default)]
struct StorageCounts {
    raw_sample_count: i64,
    hour_rollup_count: i64,
    day_rollup_count: i64,
    database_bytes: i64,
}

async fn storage_counts(pool: &SqlitePool) -> Result<StorageCounts, sqlx::Error> {
    let raw_sample_count = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
        .fetch_one(pool)
        .await?;
    let hour_rollup_count =
        sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups WHERE resolution = 'hour'")
            .fetch_one(pool)
            .await?;
    let day_rollup_count =
        sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups WHERE resolution = 'day'")
            .fetch_one(pool)
            .await?;
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(pool)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(pool)
        .await?;
    Ok(StorageCounts {
        raw_sample_count,
        hour_rollup_count,
        day_rollup_count,
        database_bytes: page_count.saturating_mul(page_size),
    })
}

async fn count_late_samples(pool: &SqlitePool) -> Result<u64, RollupError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT samples.sample_id)
         FROM metric_samples samples
         JOIN metric_rollup_partitions partitions
           ON partitions.host_id = samples.host_id
          AND partitions.state = 'sealed'
          AND partitions.bucket_start_epoch_ms =
              CASE partitions.resolution
                  WHEN 'hour' THEN (samples.observed_at_epoch_ms / ?) * ?
                  WHEN 'day' THEN (samples.observed_at_epoch_ms / ?) * ?
              END
         WHERE samples.created_at >= partitions.sealed_at",
    )
    .bind(HOUR_MILLISECONDS)
    .bind(HOUR_MILLISECONDS)
    .bind(DAY_MILLISECONDS)
    .bind(DAY_MILLISECONDS)
    .fetch_one(pool)
    .await?;
    u64::try_from(count).map_err(|_| RollupError::Overflow)
}

async fn has_pending_closed_partitions(
    pool: &SqlitePool,
    evaluated_at: DateTime<Utc>,
) -> Result<bool, RollupError> {
    for resolution in [RollupResolution::Hour, RollupResolution::Day] {
        let cutoff = bucket_start_epoch_ms(evaluated_at.timestamp_millis(), resolution)
            .ok_or(RollupError::InvalidData)?;
        if !load_partition_candidates(pool, resolution, cutoff, 1)
            .await?
            .is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn enforce_retention(
    pool: &SqlitePool,
    evaluated_at: DateTime<Utc>,
    settings: RollupSettings,
) -> Result<CompactionOutcome, RollupError> {
    let now = timestamp(Utc::now());
    let observed_cutoff = day_aligned_cutoff(evaluated_at, settings.raw_observed_retention_days)?;
    let non_observed_cutoff =
        day_aligned_cutoff(evaluated_at, settings.raw_non_observed_retention_days)?;
    let hour_cutoff = day_aligned_cutoff(evaluated_at, settings.hour_retention_days)?;
    let day_cutoff = day_aligned_cutoff(evaluated_at, settings.day_retention_days)?;
    let mut tx = pool.begin().await?;

    for resolution in [RollupResolution::Hour, RollupResolution::Day] {
        sqlx::query(
            "UPDATE metric_rollup_partitions
             SET state = 'sealed', sealed_at = COALESCE(sealed_at, ?)
             WHERE state = 'complete' AND bucket_end_epoch_ms <= ?
               AND resolution = ?
               AND input_count = (
                   SELECT COUNT(*) FROM metric_samples samples
                   WHERE samples.host_id = metric_rollup_partitions.host_id
                     AND samples.observed_at_epoch_ms >=
                         metric_rollup_partitions.bucket_start_epoch_ms
                     AND samples.observed_at_epoch_ms <
                         metric_rollup_partitions.bucket_end_epoch_ms
               )
               AND series_count = (
                   SELECT COUNT(*) FROM metric_rollups rollups
                   WHERE rollups.partition_id = metric_rollup_partitions.partition_id
               )",
        )
        .bind(&now)
        .bind(observed_cutoff)
        .bind(resolution.as_str())
        .execute(&mut *tx)
        .await?;
    }

    let observed_deleted =
        delete_raw_batch(&mut tx, true, observed_cutoff, settings.delete_batch_size).await?;
    let remaining = settings.delete_batch_size.saturating_sub(observed_deleted);
    let non_observed_deleted = if remaining > 0 {
        delete_raw_batch(&mut tx, false, non_observed_cutoff, remaining).await?
    } else {
        0
    };

    sqlx::query(
        "UPDATE metric_rollup_partitions
         SET rollup_rows_pruned_at = COALESCE(rollup_rows_pruned_at, ?)
         WHERE partition_id IN (
             SELECT hour_partition.partition_id
             FROM metric_rollup_partitions hour_partition
             WHERE hour_partition.resolution = 'hour'
               AND hour_partition.state = 'sealed'
               AND hour_partition.bucket_end_epoch_ms <= ?
               AND hour_partition.rollup_rows_pruned_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM metric_samples samples
                   WHERE samples.host_id = hour_partition.host_id
                     AND samples.observed_at_epoch_ms >= hour_partition.bucket_start_epoch_ms
                     AND samples.observed_at_epoch_ms < hour_partition.bucket_end_epoch_ms
               )
               AND EXISTS (
                   SELECT 1 FROM metric_rollup_partitions day_partition
                   WHERE day_partition.host_id = hour_partition.host_id
                     AND day_partition.resolution = 'day'
                     AND day_partition.bucket_start_epoch_ms =
                         (hour_partition.bucket_start_epoch_ms / ?) * ?
                     AND day_partition.state IN ('complete', 'sealed')
                     AND day_partition.rollup_rows_pruned_at IS NULL
               )
             ORDER BY hour_partition.bucket_start_epoch_ms, hour_partition.host_id
             LIMIT ?
         )",
    )
    .bind(&now)
    .bind(hour_cutoff)
    .bind(DAY_MILLISECONDS)
    .bind(DAY_MILLISECONDS)
    .bind(i64::from(settings.partitions_per_tick))
    .execute(&mut *tx)
    .await?;
    let hour_deleted = sqlx::query(
        "DELETE FROM metric_rollups
         WHERE resolution = 'hour' AND partition_id IN (
             SELECT partition_id FROM metric_rollup_partitions
             WHERE resolution = 'hour' AND rollup_rows_pruned_at IS NOT NULL
         )",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    sqlx::query(
        "UPDATE metric_rollup_partitions
         SET rollup_rows_pruned_at = COALESCE(rollup_rows_pruned_at, ?)
         WHERE partition_id IN (
             SELECT day_partition.partition_id
             FROM metric_rollup_partitions day_partition
             WHERE day_partition.resolution = 'day'
               AND day_partition.state = 'sealed'
               AND day_partition.bucket_end_epoch_ms <= ?
               AND day_partition.rollup_rows_pruned_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM metric_samples samples
                   WHERE samples.host_id = day_partition.host_id
                     AND samples.observed_at_epoch_ms >= day_partition.bucket_start_epoch_ms
                     AND samples.observed_at_epoch_ms < day_partition.bucket_end_epoch_ms
               )
             ORDER BY day_partition.bucket_start_epoch_ms, day_partition.host_id
             LIMIT ?
         )",
    )
    .bind(&now)
    .bind(day_cutoff)
    .bind(i64::from(settings.partitions_per_tick))
    .execute(&mut *tx)
    .await?;
    let day_deleted = sqlx::query(
        "DELETE FROM metric_rollups
         WHERE resolution = 'day' AND partition_id IN (
             SELECT partition_id FROM metric_rollup_partitions
             WHERE resolution = 'day' AND rollup_rows_pruned_at IS NOT NULL
         )",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(CompactionOutcome {
        hour_partition_count: 0,
        day_partition_count: 0,
        raw_deleted_count: u32::try_from(
            u64::from(observed_deleted) + u64::from(non_observed_deleted),
        )
        .unwrap_or(u32::MAX),
        hour_deleted_count: u32::try_from(hour_deleted).unwrap_or(u32::MAX),
        day_deleted_count: u32::try_from(day_deleted).unwrap_or(u32::MAX),
    })
}

async fn delete_raw_batch(
    tx: &mut Transaction<'_, Sqlite>,
    observed: bool,
    cutoff_epoch_ms: i64,
    limit: u32,
) -> Result<u32, RollupError> {
    if limit == 0 {
        return Ok(0);
    }
    let quality_operator = if observed { "=" } else { "<>" };
    let statement = format!(
        "DELETE FROM metric_samples WHERE sample_id IN (
            SELECT samples.sample_id
            FROM metric_samples samples
            JOIN metric_rollup_partitions hour_partition
              ON hour_partition.host_id = samples.host_id
             AND hour_partition.resolution = 'hour'
             AND hour_partition.bucket_start_epoch_ms =
                 (samples.observed_at_epoch_ms / ?) * ?
             AND hour_partition.state = 'sealed'
             AND hour_partition.rollup_rows_pruned_at IS NULL
              AND samples.created_at < hour_partition.sealed_at
             AND hour_partition.series_count = (
                 SELECT COUNT(*) FROM metric_rollups hour_rollups
                 WHERE hour_rollups.partition_id = hour_partition.partition_id
             )
            JOIN metric_rollup_partitions day_partition
              ON day_partition.host_id = samples.host_id
             AND day_partition.resolution = 'day'
             AND day_partition.bucket_start_epoch_ms =
                 (samples.observed_at_epoch_ms / ?) * ?
             AND day_partition.state = 'sealed'
             AND day_partition.rollup_rows_pruned_at IS NULL
              AND samples.created_at < day_partition.sealed_at
             AND day_partition.series_count = (
                 SELECT COUNT(*) FROM metric_rollups day_rollups
                 WHERE day_rollups.partition_id = day_partition.partition_id
             )
            WHERE samples.quality {quality_operator} 'observed'
              AND samples.observed_at_epoch_ms < ?
            ORDER BY samples.observed_at_epoch_ms, samples.sample_id
            LIMIT ?
         )"
    );
    let deleted = sqlx::query(&statement)
        .bind(HOUR_MILLISECONDS)
        .bind(HOUR_MILLISECONDS)
        .bind(DAY_MILLISECONDS)
        .bind(DAY_MILLISECONDS)
        .bind(cutoff_epoch_ms)
        .bind(i64::from(limit))
        .execute(&mut **tx)
        .await?
        .rows_affected();
    Ok(u32::try_from(deleted).unwrap_or(u32::MAX))
}

fn day_aligned_cutoff(
    evaluated_at: DateTime<Utc>,
    retention_days: u32,
) -> Result<i64, RollupError> {
    let cutoff = evaluated_at
        .checked_sub_signed(Duration::days(i64::from(retention_days)))
        .ok_or(RollupError::Overflow)?;
    bucket_start_epoch_ms(cutoff.timestamp_millis(), RollupResolution::Day)
        .ok_or(RollupError::InvalidData)
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn env_u32(name: &str, default: u32, minimum: u32, maximum: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .unwrap_or(default.clamp(minimum, maximum))
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name).ok().as_deref().map(str::trim) {
        Some("1" | "true" | "TRUE" | "yes" | "YES") => true,
        Some("0" | "false" | "FALSE" | "no" | "NO") => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::TimeZone;
    use serde_json::Value;
    use sqlx::Row;

    use super::*;
    use crate::{api::AppState, storage};

    #[allow(clippy::too_many_arguments)]
    fn sample(
        id: &str,
        run_id: &str,
        sample_kind: &str,
        value_real: Option<f64>,
        value_integer: Option<i64>,
        quality: &str,
        observed_at_epoch_ms: i64,
        boot_id: &str,
    ) -> RawRollupSample {
        RawRollupSample {
            sample_id: id.to_owned(),
            run_id: run_id.to_owned(),
            key: SeriesKey {
                family: "cpu".to_owned(),
                subject_kind: "host".to_owned(),
                subject_id: "host-rollup".to_owned(),
                metric_name: if sample_kind == "counter" {
                    "user_ticks".to_owned()
                } else {
                    "busy_pct".to_owned()
                },
                dimensions_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                sample_kind: sample_kind.to_owned(),
                source_kind: "ssh_host_resource_v1".to_owned(),
                unit: if sample_kind == "counter" {
                    "ticks".to_owned()
                } else {
                    "percent".to_owned()
                },
            },
            dimensions_json: "{}".to_owned(),
            value_real,
            value_integer,
            quality: quality.to_owned(),
            observed_at: Utc
                .timestamp_millis_opt(observed_at_epoch_ms)
                .single()
                .map(timestamp)
                .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned()),
            observed_at_epoch_ms,
            boot_id: Some(boot_id.to_owned()),
        }
    }

    #[test]
    fn utc_buckets_are_fixed_and_half_open() {
        let instant = Utc
            .with_ymd_and_hms(2026, 8, 15, 7, 59, 59)
            .single()
            .unwrap()
            + Duration::milliseconds(999);
        let hour =
            bucket_start_epoch_ms(instant.timestamp_millis(), RollupResolution::Hour).unwrap();
        let day = bucket_start_epoch_ms(instant.timestamp_millis(), RollupResolution::Day).unwrap();
        assert_eq!(
            Utc.timestamp_millis_opt(hour).single().unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 15, 7, 0, 0).single().unwrap()
        );
        assert_eq!(
            Utc.timestamp_millis_opt(day).single().unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, 0).single().unwrap()
        );
        assert_eq!(
            bucket_start_epoch_ms(hour + HOUR_MILLISECONDS, RollupResolution::Hour),
            Some(hour + HOUR_MILLISECONDS)
        );
    }

    #[test]
    fn gauge_rollup_is_deterministic_and_keeps_quality_counts() {
        let mut values = vec![
            sample(
                "s3",
                "r3",
                "derived",
                Some(30.0),
                None,
                "observed",
                3,
                "boot-a",
            ),
            sample(
                "s1",
                "r1",
                "derived",
                Some(10.0),
                None,
                "observed",
                1,
                "boot-a",
            ),
            sample("s4", "r4", "derived", None, None, "timed_out", 4, "boot-a"),
            sample(
                "s2",
                "r2",
                "derived",
                Some(20.0),
                None,
                "observed",
                2,
                "boot-a",
            ),
        ];
        let key = values[0].key.clone();
        let first = aggregate_series(key.clone(), values.clone(), None).unwrap();
        values.reverse();
        let repeated = aggregate_series(key, values, None).unwrap();
        assert_eq!(first.input_digest, repeated.input_digest);
        assert_eq!(first.sample_count, 4);
        assert_eq!(first.observed_count, 3);
        assert_eq!(first.min_value, Some(10.0));
        assert_eq!(first.max_value, Some(30.0));
        assert_eq!(first.average_value, Some(20.0));
        assert_eq!(first.p95_value, Some(30.0));
        assert_eq!(first.last_value, Some(30.0));
        assert_eq!(first.quality, "timed_out");
        assert_eq!(
            serde_json::from_str::<Value>(&first.quality_counts_json).unwrap(),
            json!({"observed": 3, "timed_out": 1})
        );
    }

    #[test]
    fn gauge_average_stays_finite_for_extreme_finite_inputs() {
        let balanced = gauge_statistics(&[-f64::MAX, f64::MAX]);
        assert_eq!(balanced.average, Some(0.0));
        let maximum = gauge_statistics(&[f64::MAX, f64::MAX]);
        assert_eq!(maximum.average, Some(f64::MAX));
        assert!(maximum.average.unwrap().is_finite());
    }

    #[test]
    fn counter_rollup_uses_boundary_and_never_crosses_boot_reset() {
        let values = vec![
            sample(
                "s1",
                "r1",
                "counter",
                None,
                Some(110),
                "observed",
                1,
                "boot-a",
            ),
            sample(
                "s2",
                "r2",
                "counter",
                None,
                Some(150),
                "observed",
                2,
                "boot-a",
            ),
            sample(
                "s3",
                "r3",
                "counter",
                None,
                Some(20),
                "observed",
                3,
                "boot-b",
            ),
            sample(
                "s4",
                "r4",
                "counter",
                None,
                Some(35),
                "observed",
                4,
                "boot-b",
            ),
        ];
        let rollup = aggregate_series(
            values[0].key.clone(),
            values,
            Some(CounterBoundary {
                sample_id: "boundary".to_owned(),
                value_integer: 100,
                boot_id: Some("boot-a".to_owned()),
            }),
        )
        .unwrap();
        assert_eq!(rollup.counter_first, Some(110));
        assert_eq!(rollup.counter_last, Some(35));
        assert_eq!(rollup.counter_delta, Some(65));
        assert_eq!(rollup.reset_count, 1);
        assert_eq!(rollup.boundary_sample_id.as_deref(), Some("boundary"));
    }

    async fn seed_rollup_fixture(pool: &SqlitePool, observed_at: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, owner_id, created_at)
             VALUES ('workspace-default', 'owner-local', ?)",
        )
        .bind(timestamp(observed_at))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, transport, os, status, created_at
             ) VALUES (
                'host-rollup', 'workspace-default', 'Rollup', 'fixture.invalid', 22,
                'fixture', 'secret-ref-fixture', 'verified', 'ssh', 'linux',
                'connection_ready', ?
             )",
        )
        .bind(timestamp(observed_at))
        .execute(pool)
        .await
        .unwrap();
        for (offset, value) in [(0i64, 10.0), (10, 20.0), (20, 30.0)] {
            let at = observed_at + Duration::minutes(offset);
            let run_id = format!("00000000-0000-0000-0000-{offset:012}");
            let sample_id = format!("10000000-0000-0000-0000-{offset:012}");
            sqlx::query(
                "INSERT INTO monitor_runs(
                    run_id, host_id, request_id, idempotency_key, request_sha256,
                    profile, trigger_kind, state, stale_after_seconds, boot_id,
                    submitted_at, started_at, finished_at, accepted_response_json
                 ) VALUES (?, 'host-rollup', ?, ?, ?, 'host_resource_v1', 'manual',
                    'succeeded', 900, 'boot-a', ?, ?, ?, '{}')",
            )
            .bind(&run_id)
            .bind(format!("request-{offset}"))
            .bind(format!("key-{offset}"))
            .bind(format!("digest-{offset}"))
            .bind(timestamp(at))
            .bind(timestamp(at))
            .bind(timestamp(at))
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO metric_samples(
                    sample_id, run_id, host_id, family, subject_kind, subject_id,
                    metric_name, dimensions_json, dimensions_sha256, sample_kind,
                    value_real, unit, quality, observed_at, observed_at_epoch_ms,
                    source_kind, created_at
                 ) VALUES (?, ?, 'host-rollup', 'cpu', 'host', 'host-rollup',
                    'busy_pct', '{}',
                    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                    'derived', ?, 'percent', 'observed', ?, ?,
                    'ssh_host_resource_v1', ?)",
            )
            .bind(&sample_id)
            .bind(&run_id)
            .bind(value)
            .bind(timestamp(at))
            .bind(at.timestamp_millis())
            .bind(timestamp(at))
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn insert_counter_fixture(
        pool: &SqlitePool,
        run_id: &str,
        sample_id: &str,
        observed_at: DateTime<Utc>,
        value: i64,
    ) {
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, boot_id,
                submitted_at, started_at, finished_at, accepted_response_json
             ) VALUES (?, 'host-rollup', ?, ?, ?, 'host_resource_v1', 'manual',
                'succeeded', 900, 'boot-a', ?, ?, ?, '{}')",
        )
        .bind(run_id)
        .bind(format!("request-{run_id}"))
        .bind(format!("key-{run_id}"))
        .bind(format!("digest-{run_id}"))
        .bind(timestamp(observed_at))
        .bind(timestamp(observed_at))
        .bind(timestamp(observed_at))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metric_samples(
                sample_id, run_id, host_id, family, subject_kind, subject_id,
                metric_name, dimensions_json, dimensions_sha256, sample_kind,
                value_integer, unit, quality, observed_at, observed_at_epoch_ms,
                source_kind, created_at
             ) VALUES (?, ?, 'host-rollup', 'cpu', 'host', 'host-rollup',
                'user_ticks', '{}',
                'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'counter', ?, 'ticks', 'observed', ?, ?, 'ssh_host_resource_v1', ?)",
        )
        .bind(sample_id)
        .bind(run_id)
        .bind(value)
        .bind(timestamp(observed_at))
        .bind(observed_at.timestamp_millis())
        .bind(timestamp(observed_at))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn compaction_is_idempotent_and_retention_is_opt_in() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 10, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        let state = AppState::new(pool.clone());
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 11, 1, 0, 0).single().unwrap();
        let first = compact_once(&state, evaluated_at).await.unwrap();
        assert_eq!(first.hour_partition_count, 1);
        assert_eq!(first.day_partition_count, 1);
        assert_eq!(first.raw_deleted_count, 0);
        let before = sqlx::query(
            "SELECT COUNT(*) AS rows, MIN(input_digest) AS digest
             FROM metric_rollups",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before.get::<i64, _>("rows"), 2);

        let repeated = compact_once(&state, evaluated_at).await.unwrap();
        assert_eq!(repeated.hour_partition_count, 0);
        assert_eq!(repeated.day_partition_count, 0);
        let after = sqlx::query(
            "SELECT COUNT(*) AS rows, MIN(input_digest) AS digest
             FROM metric_rollups",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after.get::<i64, _>("rows"), before.get::<i64, _>("rows"));
        assert_eq!(
            after.get::<String, _>("digest"),
            before.get::<String, _>("digest")
        );
        let raw: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(raw, 3, "default compaction never deletes real raw data");
        let status = maintenance_status(&state).await.unwrap();
        assert_eq!(status.state, MonitoringHistoryMaintenanceState::Succeeded);
        assert!(!status.retention.enabled);
        assert_eq!(status.raw_sample_count, 3);
        assert_eq!(status.hour_rollup_count, 1);
        assert_eq!(status.day_rollup_count, 1);
        assert!(status.database_bytes > 0);
    }

    #[tokio::test]
    async fn enabled_retention_deletes_only_after_hour_and_day_rollups_commit() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        let mut state = AppState::new(pool.clone());
        state.monitoring_rollup = Arc::new(MonitoringRollupRuntime::new(RollupSettings {
            retention_enabled: true,
            raw_observed_retention_days: 1,
            raw_non_observed_retention_days: 2,
            hour_retention_days: 90,
            day_retention_days: 365,
            ..RollupSettings::default()
        }));
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap();
        let outcome = compact_once(&state, evaluated_at).await.unwrap();
        assert_eq!(outcome.raw_deleted_count, 3);
        let raw: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(raw, 0);
        let rollups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rollups, 2);
        let sealed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_rollup_partitions WHERE state = 'sealed'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sealed, 2);
    }

    #[tokio::test]
    async fn batched_raw_cleanup_finishes_before_bounded_rollup_pruning() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        let mut state = AppState::new(pool.clone());
        state.monitoring_rollup = Arc::new(MonitoringRollupRuntime::new(RollupSettings {
            delete_batch_size: 1,
            retention_enabled: true,
            raw_observed_retention_days: 1,
            raw_non_observed_retention_days: 1,
            hour_retention_days: 1,
            day_retention_days: 1,
            ..RollupSettings::default()
        }));
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap();

        for expected_raw in [2i64, 1] {
            let outcome = compact_once(&state, evaluated_at).await.unwrap();
            assert_eq!(outcome.raw_deleted_count, 1);
            assert_eq!(outcome.hour_deleted_count, 0);
            assert_eq!(outcome.day_deleted_count, 0);
            let raw: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
                .fetch_one(&pool)
                .await
                .unwrap();
            let rollups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(raw, expected_raw);
            assert_eq!(rollups, 2, "a partial raw batch must not prune its proof");
        }

        let finished = compact_once(&state, evaluated_at).await.unwrap();
        assert_eq!(finished.raw_deleted_count, 1);
        assert_eq!(finished.hour_deleted_count, 1);
        assert_eq!(finished.day_deleted_count, 1);
        let repeated = compact_once(&state, evaluated_at).await.unwrap();
        assert_eq!(repeated.raw_deleted_count, 0);
        assert_eq!(repeated.hour_deleted_count, 0);
        assert_eq!(repeated.day_deleted_count, 0);
        let remaining: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM metric_samples),
                    (SELECT COUNT(*) FROM metric_rollups)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, (0, 0));
    }

    #[tokio::test]
    async fn retention_waits_for_compaction_backlog_before_deleting_counter_boundaries() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let first_hour = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, first_hour).await;
        insert_counter_fixture(
            &pool,
            "30000000-0000-0000-0000-000000000011",
            "counter-backlog-old",
            first_hour + Duration::minutes(35),
            100,
        )
        .await;
        insert_counter_fixture(
            &pool,
            "30000000-0000-0000-0000-000000000012",
            "counter-backlog-new",
            first_hour + Duration::hours(1),
            150,
        )
        .await;
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap();
        let hour_cutoff =
            bucket_start_epoch_ms(evaluated_at.timestamp_millis(), RollupResolution::Hour).unwrap();
        let raw_hour_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT observed_at_epoch_ms / ?) FROM metric_samples",
        )
        .bind(HOUR_MILLISECONDS)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(raw_hour_count, 2);
        let initial_hour_candidates =
            load_partition_candidates(&pool, RollupResolution::Hour, hour_cutoff, 10)
                .await
                .unwrap();
        assert_eq!(initial_hour_candidates.len(), 2);
        let mut retention_state = AppState::new(pool.clone());
        retention_state.monitoring_rollup =
            Arc::new(MonitoringRollupRuntime::new(RollupSettings {
                partitions_per_tick: 1,
                retention_enabled: true,
                raw_observed_retention_days: 1,
                raw_non_observed_retention_days: 1,
                hour_retention_days: 1,
                day_retention_days: 365,
                ..RollupSettings::default()
            }));

        let first = compact_once(&retention_state, evaluated_at).await.unwrap();
        assert_eq!(first.hour_partition_count, 1);
        assert_eq!(
            first.raw_deleted_count, 0,
            "retention must pause while a closed hour partition is still pending",
        );
        let raw_before_next_hour: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(raw_before_next_hour, 5);

        let mut catch_up_state = AppState::new(pool.clone());
        catch_up_state.monitoring_rollup = Arc::new(MonitoringRollupRuntime::new(RollupSettings {
            partitions_per_tick: 1,
            retention_enabled: false,
            ..RollupSettings::default()
        }));
        let catch_up = compact_once(&catch_up_state, evaluated_at).await.unwrap();
        assert_eq!(catch_up.hour_partition_count, 1);
        let boundary: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT boundary_sample_id, counter_delta
             FROM metric_rollups
             WHERE resolution = 'hour'
               AND last_sample_id = 'counter-backlog-new'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(boundary.0.as_deref(), Some("counter-backlog-old"));
        assert_eq!(boundary.1, Some(50));
    }

    #[tokio::test]
    async fn same_count_newer_raw_is_rehashed_and_recompacted() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        let state = AppState::new(pool.clone());
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 2, 0, 0, 0).single().unwrap();
        compact_once(&state, evaluated_at).await.unwrap();
        let before: String = sqlx::query_scalar(
            "SELECT input_digest FROM metric_rollup_partitions
             WHERE resolution = 'hour'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        tokio::time::sleep(StdDuration::from_millis(2)).await;
        sqlx::query(
            "DELETE FROM metric_samples
             WHERE sample_id = '10000000-0000-0000-0000-000000000010'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let replacement_at = observed_at + Duration::minutes(10);
        sqlx::query(
            "INSERT INTO metric_samples(
                sample_id, run_id, host_id, family, subject_kind, subject_id,
                metric_name, dimensions_json, dimensions_sha256, sample_kind,
                value_real, unit, quality, observed_at, observed_at_epoch_ms,
                source_kind, created_at
             ) VALUES ('same-count-replacement',
                '00000000-0000-0000-0000-000000000010', 'host-rollup', 'cpu',
                'host', 'host-rollup', 'busy_pct', '{}',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'derived', 100.0, 'percent', 'observed', ?, ?,
                'ssh_host_resource_v1', ?)",
        )
        .bind(timestamp(replacement_at))
        .bind(replacement_at.timestamp_millis())
        .bind(timestamp(Utc::now()))
        .execute(&pool)
        .await
        .unwrap();

        let outcome = compact_once(&state, evaluated_at).await.unwrap();
        assert_eq!(outcome.hour_partition_count, 1);
        assert_eq!(outcome.day_partition_count, 1);
        let after: (String, f64) = sqlx::query_as(
            "SELECT partitions.input_digest, rollups.average_value
             FROM metric_rollup_partitions partitions
             JOIN metric_rollups rollups ON rollups.partition_id = partitions.partition_id
             WHERE partitions.resolution = 'hour' AND rollups.metric_name = 'busy_pct'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(after.0, before);
        assert!((after.1 - (140.0 / 3.0)).abs() < 1e-12);
    }

    #[tokio::test]
    async fn counter_boundary_falls_back_to_retained_rollup_after_raw_expiry() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let first_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 40, 0).single().unwrap();
        seed_rollup_fixture(&pool, first_at - Duration::minutes(35)).await;
        insert_counter_fixture(
            &pool,
            "30000000-0000-0000-0000-000000000001",
            "counter-boundary-old",
            first_at,
            100,
        )
        .await;
        let mut state = AppState::new(pool.clone());
        state.monitoring_rollup = Arc::new(MonitoringRollupRuntime::new(RollupSettings {
            retention_enabled: true,
            raw_observed_retention_days: 1,
            raw_non_observed_retention_days: 2,
            hour_retention_days: 90,
            day_retention_days: 365,
            ..RollupSettings::default()
        }));
        compact_once(
            &state,
            Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap(),
        )
        .await
        .unwrap();
        let raw_after_first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            raw_after_first, 0,
            "retention must not pin an inactive counter raw row"
        );

        let next_at = Utc.with_ymd_and_hms(2026, 8, 5, 2, 5, 0).single().unwrap();
        insert_counter_fixture(
            &pool,
            "30000000-0000-0000-0000-000000000002",
            "counter-boundary-new",
            next_at,
            150,
        )
        .await;
        compact_once(
            &state,
            Utc.with_ymd_and_hms(2026, 8, 8, 0, 0, 0).single().unwrap(),
        )
        .await
        .unwrap();
        let boundaries: Vec<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT resolution, boundary_sample_id, counter_delta
             FROM metric_rollups
             WHERE metric_name = 'user_ticks' AND last_sample_id = 'counter-boundary-new'
             ORDER BY resolution",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(boundaries.len(), 2);
        for (_, boundary, delta) in boundaries {
            assert_eq!(boundary.as_deref(), Some("counter-boundary-old"));
            assert_eq!(delta, Some(50));
        }
        let raw_after_second: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(raw_after_second, 0);
    }

    #[tokio::test]
    async fn sealed_partition_reports_and_preserves_a_late_sample() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        let mut state = AppState::new(pool.clone());
        state.monitoring_rollup = Arc::new(MonitoringRollupRuntime::new(RollupSettings {
            retention_enabled: true,
            raw_observed_retention_days: 1,
            raw_non_observed_retention_days: 2,
            hour_retention_days: 90,
            day_retention_days: 365,
            ..RollupSettings::default()
        }));
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap();
        compact_once(&state, evaluated_at).await.unwrap();

        let run_id = "20000000-0000-0000-0000-000000000000";
        let created_at = Utc::now() + Duration::seconds(1);
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, boot_id,
                submitted_at, started_at, finished_at, accepted_response_json
             ) VALUES (?, 'host-rollup', 'late-request', 'late-key', 'late-digest',
                'host_resource_v1', 'manual', 'succeeded', 900, 'boot-a',
                ?, ?, ?, '{}')",
        )
        .bind(run_id)
        .bind(timestamp(created_at))
        .bind(timestamp(created_at))
        .bind(timestamp(created_at))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metric_samples(
                sample_id, run_id, host_id, family, subject_kind, subject_id,
                metric_name, dimensions_json, dimensions_sha256, sample_kind,
                value_real, unit, quality, observed_at, observed_at_epoch_ms,
                source_kind, created_at
             ) VALUES ('late-sample', ?, 'host-rollup', 'cpu', 'host', 'host-rollup',
                'busy_pct', '{}',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'derived', 42.0, 'percent', 'observed', ?, ?,
                'ssh_host_resource_v1', ?)",
        )
        .bind(run_id)
        .bind(timestamp(observed_at + Duration::minutes(30)))
        .bind((observed_at + Duration::minutes(30)).timestamp_millis())
        .bind(timestamp(created_at))
        .execute(&pool)
        .await
        .unwrap();

        assert!(matches!(
            compact_once(&state, evaluated_at).await,
            Err(RollupError::LateSamples(1))
        ));
        let late_exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_samples WHERE sample_id = 'late-sample'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(late_exists, 1, "late raw must never be silently deleted");
        let terminal: (String, Option<String>) = sqlx::query_as(
            "SELECT state, error_code FROM monitoring_compaction_runs
             ORDER BY started_at DESC, compaction_run_id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            terminal,
            (
                "failed".to_owned(),
                Some("COMPACTOR_LATE_SAMPLE".to_owned())
            )
        );
    }

    #[tokio::test]
    async fn host_cascade_removes_raw_rollups_and_partitions_without_orphans() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        let state = AppState::new(pool.clone());
        compact_once(
            &state,
            Utc.with_ymd_and_hms(2026, 8, 2, 0, 0, 0).single().unwrap(),
        )
        .await
        .unwrap();

        let mismatched_partition = sqlx::query(
            "UPDATE metric_rollups
             SET resolution = CASE resolution WHEN 'hour' THEN 'day' ELSE 'hour' END
             WHERE rollup_id = (SELECT rollup_id FROM metric_rollups LIMIT 1)",
        )
        .execute(&pool)
        .await;
        assert!(
            mismatched_partition.is_err(),
            "rollup resolution must match its owning partition",
        );

        sqlx::query("DELETE FROM hosts WHERE host_id = 'host-rollup'")
            .execute(&pool)
            .await
            .unwrap();
        for table in [
            "metric_samples",
            "metric_rollups",
            "metric_rollup_partitions",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(count, 0, "{table} must cascade with the HOST");
        }
        let maintenance: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM monitoring_history_maintenance WHERE singleton_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(maintenance, 1);
        let live_status = maintenance_status(&state).await.unwrap();
        assert_eq!(live_status.raw_sample_count, 0);
        assert_eq!(live_status.hour_rollup_count, 0);
        assert_eq!(live_status.day_rollup_count, 0);
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failed_rollup_write_keeps_raw_and_finishes_the_maintenance_run() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        sqlx::query(
            "CREATE TRIGGER fixture_reject_rollup BEFORE INSERT ON metric_rollups
             BEGIN SELECT RAISE(ABORT, 'fixture rollup failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut state = AppState::new(pool.clone());
        state.monitoring_rollup = Arc::new(MonitoringRollupRuntime::new(RollupSettings {
            retention_enabled: true,
            raw_observed_retention_days: 1,
            raw_non_observed_retention_days: 2,
            hour_retention_days: 90,
            day_retention_days: 365,
            ..RollupSettings::default()
        }));
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap();
        assert!(matches!(
            compact_once(&state, evaluated_at).await,
            Err(RollupError::Storage(_))
        ));
        let raw: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(raw, 3);
        let rollups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rollups, 0);
        let run_state: String = sqlx::query_scalar(
            "SELECT state FROM monitoring_compaction_runs ORDER BY started_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(run_state, "failed");
        let maintenance_state: String = sqlx::query_scalar(
            "SELECT state FROM monitoring_history_maintenance WHERE singleton_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(maintenance_state, "failed");
    }

    #[tokio::test]
    async fn terminal_fallback_clears_running_when_detailed_failure_update_is_rejected() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        let observed_at = Utc.with_ymd_and_hms(2026, 8, 1, 1, 5, 0).single().unwrap();
        seed_rollup_fixture(&pool, observed_at).await;
        sqlx::query(
            "CREATE TRIGGER fixture_reject_rollup_terminal
             BEFORE INSERT ON metric_rollups
             BEGIN SELECT RAISE(ABORT, 'fixture rollup failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER fixture_reject_detailed_maintenance_failure
             BEFORE UPDATE ON monitoring_history_maintenance
             WHEN NEW.state = 'failed'
              AND NEW.last_error_code = 'COMPACTOR_STORAGE_FAILED'
             BEGIN SELECT RAISE(ABORT, 'fixture detailed terminal failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool.clone());
        let evaluated_at = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).single().unwrap();

        assert!(matches!(
            compact_once(&state, evaluated_at).await,
            Err(RollupError::Storage(_))
        ));
        let running_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM monitoring_compaction_runs WHERE state = 'running'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(running_runs, 0);
        let maintenance: (String, Option<String>) = sqlx::query_as(
            "SELECT state, last_error_code FROM monitoring_history_maintenance
             WHERE singleton_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            maintenance,
            (
                "failed".to_owned(),
                Some("COMPACTOR_TERMINAL_SYNC_FAILED".to_owned())
            )
        );

        assert!(matches!(
            compact_once(&state, evaluated_at).await,
            Err(RollupError::Storage(_))
        ));
        let running_after_retry: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM monitoring_compaction_runs WHERE state = 'running'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            running_after_retry, 0,
            "the unique running fence must be reusable"
        );
    }
}
