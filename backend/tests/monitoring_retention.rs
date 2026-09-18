use std::{borrow::Cow, str::FromStr};

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
};
use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use network_atlas::{
    api::AppState,
    contracts::{
        MetricHistoryFamily, MetricHistoryRequestedResolution, MetricHistoryResolution,
        MetricHistorySampleKind, MetricHistorySubjectKind,
    },
    monitoring_history::{MetricHistoryQuery, get_host_metrics},
    monitoring_rollup::{self, RollupResolution},
    storage,
};
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

static ALL_MIGRATIONS: Migrator = sqlx::migrate!("./migrations");

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn seed_host_history(pool: &SqlitePool, observed_at: DateTime<Utc>) {
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
            'host-retention', 'workspace-default', 'Retention', 'fixture.invalid', 22,
            'fixture', 'secret-ref-fixture', 'verified', 'ssh', 'linux',
            'connection_ready', ?
         )",
    )
    .bind(timestamp(observed_at))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE monitoring_history_metadata
         SET history_started_at = ?, updated_at = ? WHERE singleton_id = 1",
    )
    .bind(timestamp(observed_at - Duration::hours(1)))
    .bind(timestamp(observed_at))
    .execute(pool)
    .await
    .unwrap();

    for (offset, busy) in [(0i64, 10.0), (10, 20.0), (20, 30.0)] {
        let at = observed_at + Duration::minutes(offset);
        let run_id = format!("00000000-0000-0000-0000-{offset:012}");
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, boot_id,
                submitted_at, started_at, finished_at, accepted_response_json
             ) VALUES (?, 'host-retention', ?, ?, ?, 'host_resource_v1', 'manual',
                'succeeded', 900, 'boot-retention', ?, ?, ?, '{}')",
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
        for (suffix, metric_name, sample_kind, value) in [
            ("busy", "busy_pct", "derived", busy),
            ("available", "availability", "gauge", 1.0),
        ] {
            let sample_id = format!("{suffix}-{offset:02}-0000-0000-0000-000000000000");
            sqlx::query(
                "INSERT INTO metric_samples(
                    sample_id, run_id, host_id, family, subject_kind, subject_id,
                    metric_name, dimensions_json, dimensions_sha256, sample_kind,
                    value_real, unit, quality, observed_at, observed_at_epoch_ms,
                    source_kind, created_at
                 ) VALUES (?, ?, 'host-retention', 'cpu', 'host', 'host-retention',
                    ?, '{}',
                    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                    ?, ?, ?, 'observed', ?, ?, 'ssh_host_resource_v1', ?)",
            )
            .bind(sample_id)
            .bind(&run_id)
            .bind(metric_name)
            .bind(sample_kind)
            .bind(value)
            .bind(if metric_name == "availability" {
                "status"
            } else {
                "percent"
            })
            .bind(timestamp(at))
            .bind(at.timestamp_millis())
            .bind(timestamp(at))
            .execute(pool)
            .await
            .unwrap();
        }
    }
}

fn query(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    resolution: MetricHistoryRequestedResolution,
    metric_name: Option<&str>,
    limit: Option<u32>,
    cursor: Option<(i64, String)>,
) -> MetricHistoryQuery {
    MetricHistoryQuery {
        from: timestamp(from),
        to: timestamp(to),
        resolution,
        family: Some(MetricHistoryFamily::Cpu),
        subject_kind: Some(MetricHistorySubjectKind::Host),
        subject_id: None,
        metric_name: metric_name.map(str::to_owned),
        sample_kind: metric_name.map(|name| {
            if name == "availability" {
                MetricHistorySampleKind::Gauge
            } else {
                MetricHistorySampleKind::Derived
            }
        }),
        after_epoch_ms: cursor.as_ref().map(|value| value.0),
        after_sample_id: cursor.map(|value| value.1),
        limit,
    }
}

#[tokio::test]
async fn hour_day_auto_and_rollup_keyset_return_explicit_aggregate_semantics() {
    let pool = storage::connect("sqlite::memory:").await.unwrap();
    let now = Utc::now();
    let historic = now - Duration::days(10);
    let observed_at = Utc
        .timestamp_millis_opt(
            monitoring_rollup::bucket_start_epoch_ms(
                historic.timestamp_millis(),
                RollupResolution::Hour,
            )
            .unwrap(),
        )
        .single()
        .unwrap()
        + Duration::minutes(5);
    seed_host_history(&pool, observed_at).await;
    let state = AppState::new(pool.clone());
    let outcome = monitoring_rollup::compact_once(&state, now).await.unwrap();
    assert_eq!(outcome.hour_partition_count, 1);
    assert_eq!(outcome.day_partition_count, 1);

    let hour = get_host_metrics(
        State(state.clone()),
        HeaderMap::new(),
        Path("host-retention".to_owned()),
        Ok(Query(query(
            observed_at - Duration::hours(1),
            observed_at + Duration::hours(2),
            MetricHistoryRequestedResolution::Hour,
            Some("busy_pct"),
            None,
            None,
        ))),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(hour.data.actual_resolution, MetricHistoryResolution::Hour);
    assert_eq!(hour.data.retention_tier, "hour");
    assert_eq!(hour.data.series.len(), 1);
    let hour_point = &hour.data.series[0].points[0];
    assert_eq!(hour_point.value, Some(20.0));
    assert!(hour_point.value_integer.is_none());
    let aggregate = hour_point.rollup.as_ref().unwrap();
    assert_eq!(aggregate.resolution, MetricHistoryResolution::Hour);
    assert_eq!(
        aggregate.bucket_width_seconds,
        RollupResolution::Hour.bucket_seconds()
    );
    assert_eq!(aggregate.sample_count, 3);
    assert_eq!(aggregate.observed_count, 3);
    assert_eq!(aggregate.average, Some(20.0));
    assert_eq!(aggregate.min, Some(10.0));
    assert_eq!(aggregate.max, Some(30.0));
    assert_eq!(aggregate.p95, Some(30.0));
    assert_eq!(aggregate.quality_counts, serde_json::json!({"observed": 3}));
    assert_eq!(aggregate.input_digest.len(), 64);

    let day = get_host_metrics(
        State(state.clone()),
        HeaderMap::new(),
        Path("host-retention".to_owned()),
        Ok(Query(query(
            observed_at - Duration::days(1),
            observed_at + Duration::days(1),
            MetricHistoryRequestedResolution::Day,
            Some("busy_pct"),
            None,
            None,
        ))),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(day.data.actual_resolution, MetricHistoryResolution::Day);
    assert_eq!(day.data.series[0].points[0].value, Some(20.0));
    assert_eq!(
        day.data.latest_observation_at,
        Some(timestamp(observed_at + Duration::minutes(20)))
    );
    assert_eq!(day.data.coverage.observed_count, 3);
    assert_eq!(
        day.data.series[0].points[0]
            .rollup
            .as_ref()
            .unwrap()
            .bucket_width_seconds,
        RollupResolution::Day.bucket_seconds()
    );

    let auto = get_host_metrics(
        State(state.clone()),
        HeaderMap::new(),
        Path("host-retention".to_owned()),
        Ok(Query(query(
            now - Duration::days(30),
            now,
            MetricHistoryRequestedResolution::Auto,
            Some("busy_pct"),
            None,
            None,
        ))),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(auto.data.actual_resolution, MetricHistoryResolution::Hour);
    assert_eq!(auto.data.series[0].points[0].value, Some(20.0));

    let first_page = get_host_metrics(
        State(state.clone()),
        HeaderMap::new(),
        Path("host-retention".to_owned()),
        Ok(Query(query(
            observed_at - Duration::hours(1),
            observed_at + Duration::hours(2),
            MetricHistoryRequestedResolution::Hour,
            None,
            Some(1),
            None,
        ))),
    )
    .await
    .unwrap()
    .0;
    assert!(first_page.data.has_more);
    let cursor = first_page.data.next_cursor.unwrap();
    let first_id = first_page.data.series[0].points[0].sample_id.clone();
    let second_page = get_host_metrics(
        State(state),
        HeaderMap::new(),
        Path("host-retention".to_owned()),
        Ok(Query(query(
            observed_at - Duration::hours(1),
            observed_at + Duration::hours(2),
            MetricHistoryRequestedResolution::Hour,
            None,
            Some(1),
            Some((cursor.after_epoch_ms, cursor.after_sample_id)),
        ))),
    )
    .await
    .unwrap()
    .0;
    assert!(!second_page.data.has_more);
    assert_ne!(second_page.data.series[0].points[0].sample_id, first_id);

    sqlx::query("DELETE FROM metric_samples WHERE host_id = 'host-retention'")
        .execute(&pool)
        .await
        .unwrap();
    let after_raw_cleanup = get_host_metrics(
        State(AppState::new(pool)),
        HeaderMap::new(),
        Path("host-retention".to_owned()),
        Ok(Query(query(
            observed_at - Duration::hours(1),
            observed_at + Duration::hours(2),
            MetricHistoryRequestedResolution::Hour,
            Some("busy_pct"),
            None,
            None,
        ))),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(after_raw_cleanup.data.series[0].points[0].value, Some(20.0));
    assert_eq!(
        after_raw_cleanup.data.latest_observation_at,
        Some(timestamp(observed_at + Duration::minutes(20)))
    );
}

#[tokio::test]
async fn v11_to_v12_preserves_raw_history_without_fabricating_rollups() {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let through_h3a = Migrator {
        migrations: Cow::Owned(
            ALL_MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 11)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    through_h3a.run(&pool).await.unwrap();
    let observed_at = Utc::now() - Duration::days(1);
    seed_host_history(&pool, observed_at).await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, 6);

    // This test is the H3b v11→v12 compatibility boundary.  Keep H3c's
    // health/provenance migration out of it; v12→v13 has its own migration
    // coverage and must not change the rollup assertions below.
    let through_h3b = Migrator {
        migrations: Cow::Owned(
            ALL_MIGRATIONS
                .iter()
                .filter(|migration| migration.version <= 12)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    through_h3b.run(&pool).await.unwrap();
    let version: i64 =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(version, 12);
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_samples")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    let rollups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_rollups")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rollups, 0);
    let retention_enabled: i64 = sqlx::query_scalar(
        "SELECT retention_enabled FROM monitoring_history_maintenance WHERE singleton_id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(retention_enabled, 0);
    let foreign_keys = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(foreign_keys.is_empty());
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");
}
