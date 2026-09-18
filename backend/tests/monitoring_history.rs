use std::{borrow::Cow, str::FromStr};

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode, header::SET_COOKIE},
};
use chrono::{DateTime, FixedOffset, SecondsFormat, TimeZone, Utc};
use network_atlas::{
    api,
    auth::{AuthConfig, AuthService},
    model_provider::ModelClient,
    monitoring::{
        CollectionCoverage, FamilyObservation, HostResourceCapture, HostResourceObservation,
        MetricQuality, RawSource, RunCompleteness, parse_host_resource_v1,
    },
    monitoring_history::{self, MetricSampleInput},
    secrets::FileSecretStore,
    ssh::SystemSsh,
    storage,
};
use serde_json::{Value, json};
use sqlx::{
    Row, SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

static ALL_MIGRATIONS: Migrator = sqlx::migrate!("./migrations");

const WORKSPACE_ID: &str = "workspace-default";
const HISTORY_SOURCE: &str = "ssh_host_resource_v1";
const DIMENSIONS_SHA256: &str = "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn millisecond_now() -> DateTime<Utc> {
    Utc.timestamp_millis_opt(Utc::now().timestamp_millis())
        .single()
        .expect("current millisecond")
}

async fn json_body(response: Response<Body>) -> Value {
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

async fn get_json(app: &Router, uri: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .uri(uri)
        .header("accept", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("HTTP response");
    let status = response.status();
    (status, json_body(response).await)
}

async fn test_pool() -> SqlitePool {
    storage::connect("sqlite::memory:")
        .await
        .expect("test database")
}

async fn seed_host(pool: &SqlitePool, host_id: &str, credential_ref: &str) {
    sqlx::query(
        "INSERT OR IGNORE INTO workspaces(workspace_id, owner_id, created_at)
         VALUES (?, 'owner-local', '2026-08-15T00:00:00.000Z')",
    )
    .bind(WORKSPACE_ID)
    .execute(pool)
    .await
    .expect("workspace");
    sqlx::query(
        "INSERT INTO hosts(
            host_id, workspace_id, display_name, address, port, ssh_user, credential_ref,
            host_key_state, transport, os, status, created_at, last_checked_at
         ) VALUES (?, ?, ?, ?, 22, 'fixture', ?, 'verified', 'ssh', 'linux',
            'connection_ready', '2026-08-15T00:00:00.000Z', '2026-08-15T00:00:00.000Z')",
    )
    .bind(host_id)
    .bind(WORKSPACE_ID)
    .bind(format!("History {host_id}"))
    .bind(format!("{host_id}.invalid"))
    .bind(credential_ref)
    .execute(pool)
    .await
    .expect("host");
}

async fn seed_run(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    state: &str,
    submitted_at: DateTime<Utc>,
    stale_after_seconds: i64,
) {
    sqlx::query(
        "INSERT INTO monitor_runs(
            run_id, host_id, request_id, idempotency_key, request_sha256,
            profile, trigger_kind, state, stale_after_seconds, submitted_at,
            started_at, finished_at, accepted_response_json
         ) VALUES (?, ?, ?, ?, ?, 'host_resource_v1', 'manual', ?, ?, ?, ?, ?, '{}')",
    )
    .bind(run_id)
    .bind(host_id)
    .bind(format!("request-{run_id}"))
    .bind(format!("key-{run_id}"))
    .bind(format!("sha-{run_id}"))
    .bind(state)
    .bind(stale_after_seconds)
    .bind(timestamp(submitted_at))
    .bind(timestamp(submitted_at))
    .bind(timestamp(submitted_at))
    .execute(pool)
    .await
    .expect("monitor run");
}

fn sample(
    host_id: &str,
    family: &str,
    metric_name: &str,
    value_real: Option<f64>,
    quality: &str,
) -> MetricSampleInput {
    MetricSampleInput {
        family: family.to_owned(),
        subject_kind: "host".to_owned(),
        subject_id: host_id.to_owned(),
        metric_name: metric_name.to_owned(),
        dimensions_json: "{}".to_owned(),
        dimensions_sha256: DIMENSIONS_SHA256.to_owned(),
        sample_kind: "gauge".to_owned(),
        value_real,
        value_integer: None,
        unit: if metric_name == "availability" {
            "status".to_owned()
        } else {
            "percent".to_owned()
        },
        window_seconds: None,
        quality: quality.to_owned(),
    }
}

async fn insert_samples(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    observed_at: DateTime<Utc>,
    samples: &[MetricSampleInput],
) {
    let mut tx = pool.begin().await.expect("sample transaction");
    monitoring_history::insert_samples(
        &mut tx,
        run_id,
        host_id,
        &timestamp(observed_at),
        observed_at.timestamp_millis(),
        samples,
    )
    .await
    .expect("metric samples");
    tx.commit().await.expect("commit samples");
}

async fn try_insert_sample(
    pool: &SqlitePool,
    run_id: &str,
    host_id: &str,
    observed_at: DateTime<Utc>,
    sample: &MetricSampleInput,
) -> Result<(), sqlx::Error> {
    seed_run(pool, run_id, host_id, "succeeded", observed_at, 900).await;
    let mut tx = pool.begin().await?;
    match monitoring_history::insert_samples(
        &mut tx,
        run_id,
        host_id,
        &timestamp(observed_at),
        observed_at.timestamp_millis(),
        std::slice::from_ref(sample),
    )
    .await
    {
        Ok(_) => tx.commit().await,
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

async fn set_history_start(pool: &SqlitePool, at: DateTime<Utc>) {
    sqlx::query(
        "UPDATE monitoring_history_metadata
         SET history_started_at = ?, updated_at = ?
         WHERE singleton_id = 1",
    )
    .bind(timestamp(at))
    .bind(timestamp(at))
    .execute(pool)
    .await
    .expect("history metadata");
}

fn metrics_uri(host_id: &str, from: &str, to: &str, family: Option<&str>) -> String {
    let family = family
        .map(|value| format!("&family={value}"))
        .unwrap_or_default();
    format!("/api/v1/hosts/{host_id}/metrics?from={from}&to={to}&resolution=raw{family}")
}

async fn insert_current(
    pool: &SqlitePool,
    host_id: &str,
    run_id: &str,
    observed_at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO monitoring_current(
            host_id, run_id, profile, collector_version, snapshot_json, coverage_json,
            metric_count, unknown_count, observed_at, valid_until, snapshot_sha256, updated_at
         ) VALUES (?, ?, 'host_resource_v1', 'contract-v1', '{}', '[]', 1, 0, ?, ?, ?, ?)",
    )
    .bind(host_id)
    .bind(run_id)
    .bind(timestamp(observed_at))
    .bind(timestamp(observed_at + chrono::Duration::minutes(15)))
    .bind("a".repeat(64))
    .bind(timestamp(observed_at))
    .execute(pool)
    .await
    .expect("current snapshot");
}

#[tokio::test]
async fn migration_does_not_turn_a_current_snapshot_into_fake_history() {
    let directory = TempDir::new().expect("temporary database directory");
    let database_path = directory.path().join("history-upgrade.db");
    let options = SqliteConnectOptions::from_str(&format!(
        "sqlite://{}?mode=rwc",
        database_path.to_string_lossy().replace('\\', "/")
    ))
    .expect("SQLite URL")
    .create_if_missing(true)
    .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("pre-H3 database");
    let through_h2 = Migrator {
        migrations: Cow::Owned(
            ALL_MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 10)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    through_h2.run(&pool).await.expect("migrate through H2");

    seed_host(
        &pool,
        "host-no-backfill",
        "secret-ref://fixture/no-backfill",
    )
    .await;
    let observed_at = millisecond_now() - chrono::Duration::minutes(1);
    seed_run(
        &pool,
        "run-current-only",
        "host-no-backfill",
        "succeeded",
        observed_at,
        900,
    )
    .await;
    insert_current(&pool, "host-no-backfill", "run-current-only", observed_at).await;

    ALL_MIGRATIONS
        .run(&pool)
        .await
        .expect("migrate through the current additive schema");
    let migration_version: i64 =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(&pool)
            .await
            .expect("migration version");
    // The current additive schema includes H6 Project Agent sidecars (v16).
    assert_eq!(migration_version, 16);
    let preserved_host: String =
        sqlx::query_scalar("SELECT host_id FROM hosts WHERE host_id = 'host-no-backfill'")
            .fetch_one(&pool)
            .await
            .expect("pre-v11 HOST");
    assert_eq!(preserved_host, "host-no-backfill");
    let preserved_run: (String, String) =
        sqlx::query_as("SELECT host_id, state FROM monitor_runs WHERE run_id = 'run-current-only'")
            .fetch_one(&pool)
            .await
            .expect("pre-v11 monitor run");
    assert_eq!(
        preserved_run,
        ("host-no-backfill".to_owned(), "succeeded".to_owned())
    );
    let preserved_current: (String, String) = sqlx::query_as(
        "SELECT host_id, run_id FROM monitoring_current WHERE host_id = 'host-no-backfill'",
    )
    .fetch_one(&pool)
    .await
    .expect("pre-v11 current snapshot");
    assert_eq!(
        preserved_current,
        ("host-no-backfill".to_owned(), "run-current-only".to_owned())
    );
    let foreign_key_violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .expect("foreign key check after v10 to current schema");
    assert!(foreign_key_violations.is_empty());
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .expect("integrity check after v10 to current schema");
    assert_eq!(integrity, "ok");
    let rollup_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'metric_rollups'",
    )
    .fetch_one(&pool)
    .await
    .expect("rollup table inventory");
    assert_eq!(rollup_tables, 1, "H3b adds the implemented rollup store");
    let rollup_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups")
        .fetch_one(&pool)
        .await
        .expect("rollup rows after migration");
    assert_eq!(rollup_rows, 0, "current snapshots are not rollup evidence");
    let provenance = sqlx::query(
        "SELECT due_interval_seconds, missed_due_count
         FROM monitor_runs WHERE run_id = 'run-current-only'",
    )
    .fetch_one(&pool)
    .await
    .expect("H2 run provenance after migration");
    assert!(
        provenance
            .get::<Option<i64>, _>("due_interval_seconds")
            .is_none()
    );
    assert!(
        provenance
            .get::<Option<i64>, _>("missed_due_count")
            .is_none()
    );
    let sample_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
        .fetch_one(&pool)
        .await
        .expect("sample count");
    assert_eq!(
        sample_count, 0,
        "a current pointer is not historical evidence"
    );

    let to = millisecond_now() + chrono::Duration::seconds(1);
    let uri = metrics_uri(
        "host-no-backfill",
        &timestamp(observed_at - chrono::Duration::minutes(1)),
        &timestamp(to),
        None,
    );
    let app = api::router(api::AppState::new(pool.clone()), "../frontend");
    let (status, body) = get_json(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["series"].as_array().unwrap().is_empty());
    assert!(body["data"]["latest_observation_at"].is_null());
    assert!(body["data"]["latest_valid_sample_at"].is_null());
    assert!(body["data"]["latest_valid_until"].is_null());
    assert_eq!(body["data"]["freshness"], "unknown");
    assert_eq!(body["data"]["coverage"]["observed_count"], 0);
}

#[tokio::test]
async fn metric_sample_values_are_exclusive_typed_and_dimensions_are_objects() {
    let pool = test_pool().await;
    let host_id = "host-value-constraints";
    seed_host(&pool, host_id, "secret-ref://fixture/value-constraints").await;
    let at = millisecond_now() - chrono::Duration::minutes(1);
    set_history_start(&pool, at - chrono::Duration::minutes(1)).await;

    let mut integer = sample(host_id, "cpu", "user_ticks", None, "observed");
    integer.sample_kind = "counter".to_owned();
    integer.unit = "ticks".to_owned();
    integer.value_integer = Some(i64::MAX);
    try_insert_sample(&pool, "run-integer-only", host_id, at, &integer)
        .await
        .expect("observed integer-only sample");
    let stored_integer: (Option<f64>, Option<i64>) = sqlx::query_as(
        "SELECT value_real, value_integer FROM metric_samples
         WHERE run_id = 'run-integer-only'",
    )
    .fetch_one(&pool)
    .await
    .expect("stored integer sample");
    assert_eq!(stored_integer, (None, Some(i64::MAX)));

    let mut both = integer.clone();
    both.value_real = Some(1.0);
    assert!(
        try_insert_sample(&pool, "run-both-values", host_id, at, &both)
            .await
            .is_err(),
        "observed samples must not contain both numeric representations"
    );

    let mut observed_without_value = integer.clone();
    observed_without_value.value_integer = None;
    assert!(
        try_insert_sample(
            &pool,
            "run-observed-no-value",
            host_id,
            at,
            &observed_without_value,
        )
        .await
        .is_err(),
        "observed samples require exactly one typed value"
    );

    let mut failed_without_value = observed_without_value.clone();
    failed_without_value.quality = "counter_reset".to_owned();
    try_insert_sample(
        &pool,
        "run-failed-no-value",
        host_id,
        at,
        &failed_without_value,
    )
    .await
    .expect("failed quality point with null values");

    let mut failed_with_value = failed_without_value.clone();
    failed_with_value.value_integer = Some(1);
    assert!(
        try_insert_sample(
            &pool,
            "run-failed-with-value",
            host_id,
            at,
            &failed_with_value,
        )
        .await
        .is_err(),
        "non-observed samples must keep both typed values null"
    );

    for (run_id, dimensions_json) in [
        ("run-array-dimensions", "[]"),
        ("run-invalid-dimensions", "{"),
    ] {
        let mut invalid_dimensions = integer.clone();
        invalid_dimensions.metric_name = run_id.to_owned();
        invalid_dimensions.dimensions_json = dimensions_json.to_owned();
        assert!(
            try_insert_sample(&pool, run_id, host_id, at, &invalid_dimensions)
                .await
                .is_err(),
            "dimensions_json must be a valid JSON object"
        );
    }

    let uri = format!(
        "{}&subject_kind=host&metric_name=user_ticks&sample_kind=counter",
        metrics_uri(
            host_id,
            &timestamp(at - chrono::Duration::seconds(1)),
            &timestamp(at + chrono::Duration::seconds(1)),
            Some("cpu"),
        )
    );
    let app = api::router(api::AppState::new(pool), "../frontend");
    let (status, body) = get_json(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["freshness"], "fresh");
    let observed_point = body["data"]["series"]
        .as_array()
        .expect("integer series")
        .iter()
        .flat_map(|series| series["points"].as_array().into_iter().flatten())
        .find(|point| point["quality"] == "observed")
        .expect("observed integer point");
    assert_eq!(
        observed_point["value"].as_f64(),
        Some(i64::MAX as f64),
        "the existing f64 DTO remains readable while SQLite retains the exact integer"
    );
    assert_eq!(
        observed_point["value_integer"],
        i64::MAX.to_string(),
        "raw export clients must also receive the exact counter value"
    );
    assert!(observed_point["sample_id"].as_str().is_some());
    assert_eq!(observed_point["at_epoch_ms"], at.timestamp_millis());
}

#[tokio::test]
async fn raw_http_query_uses_fractional_offset_epoch_boundaries_and_half_open_ranges() {
    let pool = test_pool().await;
    let host_id = "host-time-boundary";
    seed_host(&pool, host_id, "secret-ref://fixture/time-boundary").await;
    let from = millisecond_now() - chrono::Duration::minutes(10);
    let to = from + chrono::Duration::seconds(2);
    set_history_start(&pool, from - chrono::Duration::hours(1)).await;

    for (suffix, at) in [
        ("before", from - chrono::Duration::milliseconds(1)),
        ("from", from),
        ("inside", to - chrono::Duration::milliseconds(1)),
        ("to", to),
    ] {
        let run_id = format!("run-boundary-{suffix}");
        seed_run(&pool, &run_id, host_id, "succeeded", at, 900).await;
        insert_samples(
            &pool,
            &run_id,
            host_id,
            at,
            &[sample(host_id, "cpu", "busy_pct", Some(42.0), "observed")],
        )
        .await;
    }

    let offset = FixedOffset::west_opt(4 * 60 * 60).expect("fixed offset");
    let from_offset = from
        .with_timezone(&offset)
        .to_rfc3339_opts(SecondsFormat::Millis, false);
    let to_offset = to
        .with_timezone(&offset)
        .to_rfc3339_opts(SecondsFormat::Millis, false);
    let uri = metrics_uri(host_id, &from_offset, &to_offset, Some("cpu"));
    let app = api::router(api::AppState::new(pool), "../frontend");
    let (status, body) = get_json(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let points = body["data"]["series"][0]["points"]
        .as_array()
        .expect("raw points");
    assert_eq!(points.len(), 2, "from is inclusive and to is exclusive");
    assert_eq!(points[0]["run_id"], "run-boundary-from");
    assert_eq!(points[1]["run_id"], "run-boundary-inside");
    assert_eq!(points[0]["at"], timestamp(from));
    assert_eq!(
        points[1]["at"],
        timestamp(to - chrono::Duration::milliseconds(1))
    );
}

#[tokio::test]
async fn family_filter_isolates_freshness_and_keeps_gap_math_unknown_without_provenance() {
    let pool = test_pool().await;
    let host_id = "host-family-freshness";
    seed_host(&pool, host_id, "secret-ref://fixture/family").await;
    let now = millisecond_now();
    let memory_at = now - chrono::Duration::hours(2);
    let cpu_at = now - chrono::Duration::minutes(1);
    set_history_start(&pool, memory_at - chrono::Duration::minutes(1)).await;

    seed_run(
        &pool,
        "run-memory-old",
        host_id,
        "succeeded",
        memory_at,
        900,
    )
    .await;
    insert_samples(
        &pool,
        "run-memory-old",
        host_id,
        memory_at,
        &[sample(
            host_id,
            "memory",
            "availability",
            Some(1.0),
            "observed",
        )],
    )
    .await;
    seed_run(&pool, "run-cpu-recent", host_id, "succeeded", cpu_at, 900).await;
    insert_samples(
        &pool,
        "run-cpu-recent",
        host_id,
        cpu_at,
        &[sample(
            host_id,
            "cpu",
            "availability",
            Some(1.0),
            "observed",
        )],
    )
    .await;

    let app = api::router(api::AppState::new(pool), "../frontend");
    let from = timestamp(memory_at - chrono::Duration::minutes(1));
    let to = timestamp(now + chrono::Duration::seconds(1));
    let (cpu_status, cpu) =
        get_json(&app, &metrics_uri(host_id, &from, &to, Some("cpu")), None).await;
    let (memory_status, memory) = get_json(
        &app,
        &metrics_uri(host_id, &from, &to, Some("memory")),
        None,
    )
    .await;
    assert_eq!(cpu_status, StatusCode::OK, "{cpu}");
    assert_eq!(memory_status, StatusCode::OK, "{memory}");
    assert_eq!(cpu["data"]["freshness"], "fresh");
    assert_eq!(cpu["data"]["latest_observation_at"], timestamp(cpu_at));
    assert_eq!(cpu["data"]["latest_valid_sample_at"], timestamp(cpu_at));
    assert_eq!(
        cpu["data"]["latest_valid_until"],
        timestamp(cpu_at + chrono::Duration::seconds(900))
    );
    assert_eq!(memory["data"]["freshness"], "stale");
    assert_eq!(
        memory["data"]["latest_observation_at"],
        timestamp(memory_at)
    );
    assert_eq!(
        memory["data"]["latest_valid_sample_at"],
        timestamp(memory_at)
    );
    assert_eq!(
        memory["data"]["latest_valid_until"],
        timestamp(memory_at + chrono::Duration::seconds(900))
    );
    for body in [&cpu, &memory] {
        assert_eq!(body["data"]["coverage"]["observed_count"], 1);
        assert!(body["data"]["coverage"]["expected_count"].is_null());
        assert!(body["data"]["coverage"]["gap_count"].is_null());
        assert!(body["data"]["coverage"]["coverage"].is_null());
        assert_eq!(body["data"]["coverage"]["provenance_complete"], false);
    }
}

#[tokio::test]
async fn raw_history_is_preserved_and_each_http_query_is_bounded_to_seven_days() {
    let pool = test_pool().await;
    let old_host = "host-retention-old";
    let new_host = "host-retention-new";
    seed_host(&pool, old_host, "secret-ref://fixture/retention-old").await;
    seed_host(&pool, new_host, "secret-ref://fixture/retention-new").await;
    let recent_at = millisecond_now() - chrono::Duration::minutes(1);
    let old_at = recent_at - chrono::Duration::days(8);
    set_history_start(&pool, old_at - chrono::Duration::minutes(1)).await;

    seed_run(
        &pool,
        "run-retention-old",
        old_host,
        "succeeded",
        old_at,
        900,
    )
    .await;
    insert_samples(
        &pool,
        "run-retention-old",
        old_host,
        old_at,
        &[sample(
            old_host,
            "cpu",
            "availability",
            Some(1.0),
            "observed",
        )],
    )
    .await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
        .fetch_one(&pool)
        .await
        .expect("initial sample count");
    assert_eq!(before, 1);

    seed_run(
        &pool,
        "run-retention-new",
        new_host,
        "succeeded",
        recent_at,
        900,
    )
    .await;
    insert_samples(
        &pool,
        "run-retention-new",
        new_host,
        recent_at,
        &[sample(
            new_host,
            "cpu",
            "availability",
            Some(1.0),
            "observed",
        )],
    )
    .await;

    let remaining = sqlx::query(
        "SELECT host_id, run_id, observed_at_epoch_ms FROM metric_samples ORDER BY observed_at_epoch_ms",
    )
    .fetch_all(&pool)
    .await
    .expect("preserved samples");
    assert_eq!(remaining.len(), 2, "H3a performs no automatic cleanup");
    assert_eq!(remaining[0].get::<String, _>("host_id"), old_host);
    assert_eq!(remaining[0].get::<String, _>("run_id"), "run-retention-old");
    assert_eq!(
        remaining[1].get::<i64, _>("observed_at_epoch_ms"),
        recent_at.timestamp_millis()
    );

    let app = api::router(api::AppState::new(pool), "../frontend");
    let old_window = metrics_uri(
        old_host,
        &timestamp(old_at - chrono::Duration::seconds(1)),
        &timestamp(old_at + chrono::Duration::seconds(1)),
        Some("cpu"),
    );
    let (old_status, old_body) = get_json(&app, &old_window, None).await;
    assert_eq!(old_status, StatusCode::OK, "{old_body}");
    assert_eq!(
        old_body["data"]["series"][0]["points"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let oversized = metrics_uri(
        old_host,
        &timestamp(old_at),
        &timestamp(recent_at),
        Some("cpu"),
    );
    let (oversized_status, oversized_body) = get_json(&app, &oversized, None).await;
    assert_eq!(
        oversized_status,
        StatusCode::BAD_REQUEST,
        "{oversized_body}"
    );
    assert_eq!(oversized_body["error"]["code"], "TIME_RANGE_TOO_LARGE");
    assert_eq!(
        oversized_body["error"]["details"]["maximum_seconds"],
        604_800
    );

    let fractional_over = metrics_uri(
        new_host,
        &timestamp(recent_at - chrono::Duration::days(7) - chrono::Duration::milliseconds(1)),
        &timestamp(recent_at),
        Some("cpu"),
    );
    let (fractional_status, fractional_body) = get_json(&app, &fractional_over, None).await;
    assert_eq!(
        fractional_status,
        StatusCode::BAD_REQUEST,
        "{fractional_body}"
    );
    assert_eq!(fractional_body["error"]["code"], "TIME_RANGE_TOO_LARGE");
}

fn unavailable<T>(quality: MetricQuality) -> FamilyObservation<T> {
    FamilyObservation {
        quality,
        value: None,
    }
}

fn failed_observation() -> HostResourceObservation {
    HostResourceObservation {
        profile: "host_resource_v1",
        protocol_version: 1,
        cpu: unavailable(MetricQuality::TimedOut),
        cpu_counters: unavailable(MetricQuality::TimedOut),
        memory: unavailable(MetricQuality::ParseFailed),
        load: unavailable(MetricQuality::Unsupported),
        disk_capacity: unavailable(MetricQuality::PermissionDenied),
        disk_io: unavailable(MetricQuality::CounterUnreliable),
        disk_io_counters: unavailable(MetricQuality::CounterUnreliable),
        network: unavailable(MetricQuality::InsufficientInterval),
        network_counters: unavailable(MetricQuality::InsufficientInterval),
        uptime: unavailable(MetricQuality::ParseFailed),
        process: unavailable(MetricQuality::TimedOut),
        coverage: CollectionCoverage {
            required_observed: 0,
            required_total: 5,
            optional_observed: 0,
            optional_total: 3,
            completeness: RunCompleteness::Failed,
        },
    }
}

#[tokio::test]
async fn failed_run_quality_is_queryable_without_replacing_the_last_current_snapshot() {
    let pool = test_pool().await;
    let host_id = "host-failed-quality";
    seed_host(&pool, host_id, "secret-ref://fixture/failed-quality").await;
    let now = millisecond_now();
    let current_at = now - chrono::Duration::minutes(10);
    let failed_at = now - chrono::Duration::minutes(1);
    set_history_start(&pool, current_at - chrono::Duration::minutes(1)).await;

    seed_run(
        &pool,
        "run-last-current",
        host_id,
        "succeeded",
        current_at,
        900,
    )
    .await;
    insert_samples(
        &pool,
        "run-last-current",
        host_id,
        current_at,
        &[sample(
            host_id,
            "cpu",
            "availability",
            Some(1.0),
            "observed",
        )],
    )
    .await;
    insert_current(&pool, host_id, "run-last-current", current_at).await;

    seed_run(
        &pool,
        "run-failed-quality",
        host_id,
        "failed",
        failed_at,
        900,
    )
    .await;
    let quality_samples =
        monitoring_history::samples_from_observation(host_id, &failed_observation());
    assert_eq!(quality_samples.len(), 8, "one availability fact per family");
    assert!(
        quality_samples
            .iter()
            .all(|sample| sample.value_real.is_none() && sample.value_integer.is_none())
    );
    insert_samples(
        &pool,
        "run-failed-quality",
        host_id,
        failed_at,
        &quality_samples,
    )
    .await;

    let current_run: String =
        sqlx::query_scalar("SELECT run_id FROM monitoring_current WHERE host_id = ?")
            .bind(host_id)
            .fetch_one(&pool)
            .await
            .expect("current run pointer");
    assert_eq!(current_run, "run-last-current");

    let app = api::router(api::AppState::new(pool), "../frontend");
    let uri = metrics_uri(
        host_id,
        &timestamp(current_at - chrono::Duration::minutes(1)),
        &timestamp(now + chrono::Duration::seconds(1)),
        Some("cpu"),
    );
    let (status, body) = get_json(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["latest_observation_at"],
        timestamp(failed_at),
        "the latest failed observation must remain visible"
    );
    assert_eq!(
        body["data"]["latest_valid_sample_at"],
        timestamp(current_at),
        "freshness must be anchored to the latest valid sample"
    );
    let points = body["data"]["series"][0]["points"]
        .as_array()
        .expect("availability points");
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["quality"], "observed");
    assert_eq!(points[0]["value"], 1.0);
    assert_eq!(points[1]["at"], timestamp(failed_at));
    assert_eq!(points[1]["quality"], "timed_out");
    assert!(points[1]["value"].is_null());
}

#[tokio::test]
async fn deleting_a_host_cascades_raw_history_and_keeps_sqlite_consistent() {
    let pool = test_pool().await;
    let host_id = "host-delete-cascade";
    seed_host(&pool, host_id, "secret-ref://fixture/delete").await;
    let at = millisecond_now() - chrono::Duration::minutes(1);
    seed_run(&pool, "run-delete-cascade", host_id, "succeeded", at, 900).await;
    insert_samples(
        &pool,
        "run-delete-cascade",
        host_id,
        at,
        &[sample(
            host_id,
            "cpu",
            "availability",
            Some(1.0),
            "observed",
        )],
    )
    .await;
    insert_current(&pool, host_id, "run-delete-cascade", at).await;

    let immutable_update = sqlx::query(
        "UPDATE metric_samples SET value_real = 2
         WHERE run_id = 'run-delete-cascade'",
    )
    .execute(&pool)
    .await;
    assert!(
        immutable_update.is_err(),
        "raw observations are append-only until retention deletes them"
    );

    let foreign_keys_enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .expect("foreign key setting");
    assert_eq!(foreign_keys_enabled, 1);
    sqlx::query("DELETE FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .execute(&pool)
        .await
        .expect("delete host");
    for table in ["monitor_runs", "metric_samples", "monitoring_current"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("cascade count");
        assert_eq!(count, 0, "{table} must cascade with HOST deletion");
    }
    let foreign_key_violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .expect("foreign key check");
    assert!(foreign_key_violations.is_empty());
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .expect("integrity check");
    assert_eq!(integrity, "ok");
}

#[tokio::test]
async fn derived_raw_history_and_http_response_do_not_expose_credentials_or_source_urls() {
    const CREDENTIAL_MARKER: &str = "secret-ref://fixture/credential-marker";
    const SOURCE_MARKER: &str = "source-marker-7fd2";
    let pool = test_pool().await;
    let host_id = "host-secret-absence";
    seed_host(&pool, host_id, CREDENTIAL_MARKER).await;
    let at = millisecond_now() - chrono::Duration::minutes(1);
    set_history_start(&pool, at - chrono::Duration::minutes(1)).await;
    seed_run(&pool, "run-secret-absence", host_id, "failed", at, 900).await;

    let capture = HostResourceCapture {
        disk_capacity: RawSource::output(format!(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\nhttps://{SOURCE_MARKER}.invalid/private?token=FIXTURE 100 20 80 20% /srv/data\n"
        )),
        ..HostResourceCapture::default()
    };
    let observation = parse_host_resource_v1(&capture);
    let samples = monitoring_history::samples_from_observation(host_id, &observation);
    assert!(
        samples
            .iter()
            .any(|sample| sample.subject_kind == "filesystem")
    );
    insert_samples(&pool, "run-secret-absence", host_id, at, &samples).await;

    let persisted = sqlx::query(
        "SELECT family, subject_kind, subject_id, metric_name, dimensions_json,
                sample_kind, unit, quality, source_kind
         FROM metric_samples WHERE host_id = ?",
    )
    .bind(host_id)
    .fetch_all(&pool)
    .await
    .expect("persisted history")
    .into_iter()
    .flat_map(|row| {
        (0..9)
            .map(|index| row.get::<String, _>(index))
            .collect::<Vec<_>>()
    })
    .collect::<Vec<_>>()
    .join("\n");
    for forbidden in [
        CREDENTIAL_MARKER,
        SOURCE_MARKER,
        "credential_ref",
        "password=",
        "token=FIXTURE",
    ] {
        assert!(
            !persisted.contains(forbidden),
            "metric DB leaked {forbidden}"
        );
    }
    assert!(persisted.contains(HISTORY_SOURCE));

    let app = api::router(api::AppState::new(pool), "../frontend");
    let uri = metrics_uri(
        host_id,
        &timestamp(at - chrono::Duration::minutes(1)),
        &timestamp(millisecond_now() + chrono::Duration::seconds(1)),
        Some("disk_capacity"),
    );
    let (status, body) = get_json(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = body.to_string();
    for forbidden in [
        CREDENTIAL_MARKER,
        SOURCE_MARKER,
        "credential_ref",
        "password=",
        "token=FIXTURE",
    ] {
        assert!(
            !response.contains(forbidden),
            "HTTP response leaked {forbidden}"
        );
    }
}

#[tokio::test]
async fn typed_filters_default_to_the_path_host_and_only_explicitly_drill_down() {
    let pool = test_pool().await;
    let host_id = "host-typed-filters";
    seed_host(&pool, host_id, "secret-ref://fixture/typed-filters").await;
    let at = millisecond_now() - chrono::Duration::minutes(1);
    set_history_start(&pool, at - chrono::Duration::minutes(1)).await;
    seed_run(&pool, "run-typed-filters", host_id, "succeeded", at, 900).await;

    let host_sample = sample(
        host_id,
        "network",
        "rx_bytes_per_second",
        Some(10.0),
        "observed",
    );
    let mut interface_two = host_sample.clone();
    interface_two.subject_kind = "interface".to_owned();
    interface_two.subject_id = "if:2:2".to_owned();
    interface_two.sample_kind = "derived".to_owned();
    interface_two.value_real = Some(20.0);
    let mut interface_three = interface_two.clone();
    interface_three.subject_id = "if:3:3".to_owned();
    interface_three.value_real = Some(30.0);
    insert_samples(
        &pool,
        "run-typed-filters",
        host_id,
        at,
        &[host_sample, interface_two, interface_three],
    )
    .await;

    let app = api::router(api::AppState::new(pool), "../frontend");
    let from = timestamp(at - chrono::Duration::seconds(1));
    let to = timestamp(millisecond_now() + chrono::Duration::seconds(1));
    let default_uri = metrics_uri(host_id, &from, &to, Some("network"));
    let (default_status, default_body) = get_json(&app, &default_uri, None).await;
    assert_eq!(default_status, StatusCode::OK, "{default_body}");
    assert_eq!(default_body["data"]["series"].as_array().unwrap().len(), 1);
    assert_eq!(default_body["data"]["series"][0]["subject_kind"], "host");
    assert_eq!(default_body["data"]["series"][0]["subject_id"], host_id);
    assert_eq!(
        default_body["data"]["series"][0]["points"][0]["value"],
        10.0
    );

    let drill_down_uri = format!(
        "{default_uri}&subject_kind=interface&subject_id=if:2:2&metric_name=rx_bytes_per_second&sample_kind=derived"
    );
    let (drill_status, drill_body) = get_json(&app, &drill_down_uri, None).await;
    assert_eq!(drill_status, StatusCode::OK, "{drill_body}");
    assert_eq!(drill_body["data"]["series"].as_array().unwrap().len(), 1);
    assert_eq!(drill_body["data"]["series"][0]["subject_kind"], "interface");
    assert_eq!(drill_body["data"]["series"][0]["subject_id"], "if:2:2");
    assert_eq!(drill_body["data"]["series"][0]["sample_kind"], "derived");
    assert_eq!(drill_body["data"]["series"][0]["points"][0]["value"], 20.0);
    assert_eq!(drill_body["data"]["latest_observation_at"], timestamp(at));

    for (suffix, expected_code) in [
        ("&subject_id=if:2:2", "INVALID_SUBJECT_FILTER"),
        (
            "&subject_kind=host&subject_id=another-host",
            "INVALID_SUBJECT_FILTER",
        ),
        ("&metric_name=bad/name", "INVALID_METRIC_NAME"),
        (
            "&subject_kind=interface&subject_id=",
            "INVALID_SUBJECT_FILTER",
        ),
        ("&limit=0", "INVALID_LIMIT"),
        ("&limit=5001", "INVALID_LIMIT"),
        ("&after_epoch_ms=1", "INVALID_CURSOR"),
        (
            "&after_sample_id=00000000-0000-0000-0000-000000000000",
            "INVALID_CURSOR",
        ),
        (
            "&after_epoch_ms=-1&after_sample_id=00000000-0000-0000-0000-000000000000",
            "INVALID_CURSOR",
        ),
        (
            "&after_epoch_ms=1&after_sample_id=NOT-A-CANONICAL-UUID",
            "INVALID_CURSOR",
        ),
    ] {
        let (status, body) = get_json(&app, &format!("{default_uri}{suffix}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{suffix}: {body}");
        assert_eq!(body["error"]["code"], expected_code, "{suffix}: {body}");
    }
    let too_long_name = "a".repeat(129);
    let (long_status, long_body) = get_json(
        &app,
        &format!("{default_uri}&metric_name={too_long_name}"),
        None,
    )
    .await;
    assert_eq!(long_status, StatusCode::BAD_REQUEST, "{long_body}");
    assert_eq!(long_body["error"]["code"], "INVALID_METRIC_NAME");

    let too_long_subject = "s".repeat(513);
    let (subject_status, subject_body) = get_json(
        &app,
        &format!("{default_uri}&subject_kind=interface&subject_id={too_long_subject}"),
        None,
    )
    .await;
    assert_eq!(subject_status, StatusCode::BAD_REQUEST, "{subject_body}");
    assert_eq!(subject_body["error"]["code"], "INVALID_SUBJECT_FILTER");

    let (enum_status, enum_body) =
        get_json(&app, &format!("{default_uri}&subject_kind=container"), None).await;
    assert_eq!(enum_status, StatusCode::BAD_REQUEST, "{enum_body}");
    assert_eq!(enum_body["error"]["code"], "INVALID_QUERY");
}

#[tokio::test]
async fn keyset_pages_same_millisecond_rows_without_duplicates_or_gaps() {
    let pool = test_pool().await;
    let host_id = "host-keyset-pages";
    seed_host(&pool, host_id, "secret-ref://fixture/keyset").await;
    let at = millisecond_now() - chrono::Duration::minutes(1);
    set_history_start(&pool, at - chrono::Duration::minutes(1)).await;

    for index in 0..7 {
        let run_id = format!("run-keyset-{index}");
        seed_run(&pool, &run_id, host_id, "succeeded", at, 900).await;
        insert_samples(
            &pool,
            &run_id,
            host_id,
            at,
            &[sample(
                host_id,
                "cpu",
                "busy_pct",
                Some(index as f64),
                "observed",
            )],
        )
        .await;
    }
    let expected = sqlx::query(
        "SELECT sample_id, run_id FROM metric_samples
         WHERE host_id = ? ORDER BY observed_at_epoch_ms, sample_id",
    )
    .bind(host_id)
    .fetch_all(&pool)
    .await
    .expect("expected keyset order");
    assert_eq!(expected.len(), 7);

    let app = api::router(api::AppState::new(pool), "../frontend");
    let base_uri = format!(
        "{}&family=cpu&metric_name=busy_pct&limit=2",
        metrics_uri(
            host_id,
            &timestamp(at - chrono::Duration::seconds(1)),
            &timestamp(millisecond_now() + chrono::Duration::seconds(1)),
            None,
        )
    );
    let mut uri = base_uri.clone();
    let mut observed_runs = Vec::new();
    let mut cursors = Vec::new();
    loop {
        let (status, body) = get_json(&app, &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["limit"], 2);
        let page_runs = body["data"]["series"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|series| series["points"].as_array().unwrap())
            .map(|point| point["run_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(!page_runs.is_empty());
        observed_runs.extend(page_runs);

        let has_more = body["data"]["has_more"].as_bool().unwrap();
        if !has_more {
            assert!(body["data"]["next_cursor"].is_null());
            break;
        }
        let cursor = body["data"]["next_cursor"].clone();
        let epoch = cursor["after_epoch_ms"].as_i64().unwrap();
        let sample_id = cursor["after_sample_id"].as_str().unwrap().to_owned();
        assert_eq!(epoch, at.timestamp_millis());
        cursors.push((epoch, sample_id.clone()));
        uri = format!("{base_uri}&after_epoch_ms={epoch}&after_sample_id={sample_id}");
    }

    let expected_runs = expected
        .iter()
        .map(|row| row.get::<String, _>("run_id"))
        .collect::<Vec<_>>();
    assert_eq!(observed_runs, expected_runs);
    let unique_runs = observed_runs
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique_runs.len(), observed_runs.len());
    assert_eq!(cursors.len(), 3);
    for (page_index, (_, sample_id)) in cursors.iter().enumerate() {
        assert_eq!(
            sample_id,
            &expected[page_index * 2 + 1].get::<String, _>("sample_id")
        );
    }

    let (repeat_status, repeated_first_page) = get_json(&app, &base_uri, None).await;
    assert_eq!(repeat_status, StatusCode::OK, "{repeated_first_page}");
    assert_eq!(
        repeated_first_page["data"]["next_cursor"]["after_sample_id"],
        expected[1].get::<String, _>("sample_id")
    );
}

#[tokio::test]
async fn bounded_page_replaces_the_old_global_series_cap_and_remains_fully_traversable() {
    let pool = test_pool().await;
    let host_id = "host-many-series";
    seed_host(&pool, host_id, "secret-ref://fixture/many-series").await;
    let at = millisecond_now() - chrono::Duration::minutes(1);
    set_history_start(&pool, at - chrono::Duration::minutes(1)).await;
    seed_run(&pool, "run-many-series", host_id, "succeeded", at, 900).await;
    let samples = (0..300)
        .map(|index| {
            let mut value = sample(
                host_id,
                "network",
                "rx_bytes_per_second",
                Some(index as f64),
                "observed",
            );
            value.subject_kind = "interface".to_owned();
            value.subject_id = format!("if:{index}:{index}");
            value.sample_kind = "derived".to_owned();
            value
        })
        .collect::<Vec<_>>();
    insert_samples(&pool, "run-many-series", host_id, at, &samples).await;

    let app = api::router(api::AppState::new(pool.clone()), "../frontend");
    let uri = format!(
        "{}&subject_kind=interface&metric_name=rx_bytes_per_second&sample_kind=derived&limit=500",
        metrics_uri(
            host_id,
            &timestamp(at - chrono::Duration::seconds(1)),
            &timestamp(millisecond_now() + chrono::Duration::seconds(1)),
            Some("network"),
        )
    );
    let (status, body) = get_json(&app, &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["series"].as_array().unwrap().len(), 300);
    assert_eq!(body["data"]["limit"], 500);
    assert_eq!(body["data"]["has_more"], false);
    assert!(body["data"]["next_cursor"].is_null());
    let traversal_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'metric_samples_host_observed_sample_idx'",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or_default();
    assert_eq!(traversal_index, 1);
}

#[test]
fn openapi_exposes_typed_filters_bounded_keyset_and_page_cursor() {
    let document = serde_json::to_value(api::openapi()).expect("OpenAPI JSON");
    let parameters = document["paths"]["/api/v1/hosts/{host_id}/metrics"]["get"]["parameters"]
        .as_array()
        .expect("metric history parameters");
    let names = parameters
        .iter()
        .filter_map(|parameter| parameter["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for expected in [
        "host_id",
        "from",
        "to",
        "resolution",
        "family",
        "subject_kind",
        "subject_id",
        "metric_name",
        "sample_kind",
        "after_epoch_ms",
        "after_sample_id",
        "limit",
    ] {
        assert!(names.contains(expected), "OpenAPI missing {expected}");
    }
    for optional in [
        "resolution",
        "family",
        "subject_kind",
        "subject_id",
        "metric_name",
        "sample_kind",
        "after_epoch_ms",
        "after_sample_id",
        "limit",
    ] {
        let parameter = parameters
            .iter()
            .find(|parameter| parameter["name"] == optional)
            .expect("optional metric parameter");
        assert_eq!(parameter["required"], false, "{optional} must be optional");
    }
    let limit_parameter = parameters
        .iter()
        .find(|parameter| parameter["name"] == "limit")
        .unwrap();
    let limit_schema = limit_parameter["schema"].to_string();
    assert!(limit_schema.contains("5000"));
    assert!(limit_schema.contains("minimum"));
    let cursor = &document["components"]["schemas"]["MetricHistoryCursor"];
    assert!(cursor.is_object());
    assert!(cursor["properties"]["after_epoch_ms"].is_object());
    assert!(cursor["properties"]["after_sample_id"].is_object());
    let data = &document["components"]["schemas"]["MetricHistoryData"]["properties"];
    for expected in ["limit", "has_more", "next_cursor"] {
        assert!(
            data[expected].is_object(),
            "OpenAPI missing response {expected}"
        );
    }
}

#[tokio::test]
async fn raw_metric_history_http_route_requires_an_owner_session_when_auth_is_enabled() {
    const ORIGIN: &str = "https://atlas.test";
    let directory = TempDir::new().expect("auth fixture directory");
    let database_path = directory.path().join("auth-history.db");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        database_path.to_string_lossy().replace('\\', "/")
    );
    let pool = storage::connect(&database_url)
        .await
        .expect("auth database");
    let host_id = "host-auth-history";
    seed_host(&pool, host_id, "secret-ref://fixture/auth").await;

    let password = format!("fixture-{}", Uuid::new_v4());
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).expect("test salt");
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("password hash")
        .to_string();
    let auth = AuthConfig::required("owner", password_hash, ORIGIN, true).expect("auth config");
    let state = api::AppState::with_services_model_auth(
        pool,
        FileSecretStore::new(directory.path().join("secrets")),
        SystemSsh::system_default(directory.path().join("ssh")),
        ModelClient::default(),
        AuthService::new(auth),
        directory.path().to_path_buf(),
    );
    let app = api::router(state, "../frontend");
    let now = millisecond_now();
    let uri = metrics_uri(
        host_id,
        &timestamp(now - chrono::Duration::minutes(1)),
        &timestamp(now + chrono::Duration::seconds(1)),
        None,
    );

    let (anonymous_status, anonymous) = get_json(&app, &uri, None).await;
    assert_eq!(anonymous_status, StatusCode::UNAUTHORIZED, "{anonymous}");
    assert_eq!(anonymous["error"]["code"], "AUTH_REQUIRED");

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .header("origin", ORIGIN)
                .body(Body::from(
                    json!({"username": "owner", "password": password}).to_string(),
                ))
                .expect("login request"),
        )
        .await
        .expect("login response");
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get(SET_COOKIE)
        .expect("session cookie")
        .to_str()
        .expect("cookie text")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    let _ = json_body(login).await;

    let (authenticated_status, authenticated) = get_json(&app, &uri, Some(&cookie)).await;
    assert_eq!(authenticated_status, StatusCode::OK, "{authenticated}");
    assert_eq!(authenticated["data"]["host_id"], host_id);
}
