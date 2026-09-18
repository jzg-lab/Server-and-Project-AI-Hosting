use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use thiserror::Error;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::{
    api::AppState,
    contracts::{
        ApiErrorBody, ApiErrorResponse, ApiMeta, DataSourceDescriptor, DataSourceKind,
        DataSourceStatus, Freshness, MetricHistoryCoverage, MetricHistoryCursor, MetricHistoryData,
        MetricHistoryFamily, MetricHistoryPoint, MetricHistoryRequestedResolution,
        MetricHistoryResolution, MetricHistoryResponse, MetricHistoryRollupStatistics,
        MetricHistorySampleKind, MetricHistorySeries, MetricHistorySourceKind,
        MetricHistorySubjectKind, MonitorFreshness, MonitorMetricQuality,
    },
    monitoring::{FamilyObservation, HostResourceObservation, MetricQuality},
    monitoring_rollup::{RollupResolution, RollupSettings},
};

pub const HISTORY_SOURCE_KIND: &str = "ssh_host_resource_v1";
const MAX_REQUEST_ID: usize = 128;
pub const RAW_QUERY_MAX_SPAN_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const HOUR_QUERY_MAX_SPAN_SECONDS: i64 = 366 * 24 * 60 * 60;
pub const DAY_QUERY_MAX_SPAN_SECONDS: i64 = 3_660 * 24 * 60 * 60;
pub const DEFAULT_HISTORY_PAGE_LIMIT: u32 = 1_000;
pub const MAX_HISTORY_PAGE_LIMIT: u32 = 5_000;
const MAX_SUBJECT_ID_CHARS: usize = 512;
const MAX_METRIC_NAME_CHARS: usize = 128;
const MAX_CURSOR_SAMPLE_ID_CHARS: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct MetricSampleInput {
    pub family: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub metric_name: String,
    pub dimensions_json: String,
    pub dimensions_sha256: String,
    pub sample_kind: String,
    pub value_real: Option<f64>,
    pub value_integer: Option<i64>,
    pub unit: String,
    pub window_seconds: Option<f64>,
    pub quality: String,
}

#[derive(Debug, Clone, Copy)]
struct MetricDescriptor<'a> {
    family: &'a str,
    subject_kind: &'a str,
    subject_id: &'a str,
    metric_name: &'a str,
    sample_kind: &'a str,
    unit: &'a str,
}

pub fn samples_from_observation(
    host_id: &str,
    observation: &HostResourceObservation,
) -> Vec<MetricSampleInput> {
    let mut samples = Vec::new();
    push_family_availability(&mut samples, host_id, "cpu", &observation.cpu);
    push_family_availability(&mut samples, host_id, "memory", &observation.memory);
    push_family_availability(&mut samples, host_id, "load", &observation.load);
    push_family_availability(
        &mut samples,
        host_id,
        "disk_capacity",
        &observation.disk_capacity,
    );
    push_family_availability(&mut samples, host_id, "disk_io", &observation.disk_io);
    push_family_availability(&mut samples, host_id, "network", &observation.network);
    push_family_availability(&mut samples, host_id, "uptime", &observation.uptime);
    push_family_availability(&mut samples, host_id, "process", &observation.process);

    if let Some(cpu) = observation.cpu_counters.value.as_ref() {
        push_cpu_tick_counters(
            &mut samples,
            "host",
            host_id,
            BTreeMap::new(),
            cpu.aggregate,
            observation.cpu_counters.quality,
        );
        for core in &cpu.per_cpu {
            let mut dimensions = BTreeMap::new();
            dimensions.insert("cpu".to_owned(), Value::String(core.cpu.clone()));
            push_cpu_tick_counters(
                &mut samples,
                "cpu",
                &core.cpu,
                dimensions,
                core.ticks,
                observation.cpu_counters.quality,
            );
        }
    }

    if let Some(devices) = observation.disk_io_counters.value.as_ref() {
        for device in devices {
            let mut dimensions = BTreeMap::new();
            dimensions.insert("name".to_owned(), Value::String(device.name.clone()));
            dimensions.insert("major".to_owned(), Value::from(device.major));
            dimensions.insert("minor".to_owned(), Value::from(device.minor));
            for (metric_name, value, unit) in [
                ("reads_completed", device.reads_completed, "count"),
                ("read_sectors", device.read_sectors, "sectors"),
                ("read_time_ms", device.read_time_ms, "milliseconds"),
                ("writes_completed", device.writes_completed, "count"),
                ("write_sectors", device.write_sectors, "sectors"),
                ("write_time_ms", device.write_time_ms, "milliseconds"),
                ("io_time_ms", device.io_time_ms, "milliseconds"),
                (
                    "weighted_io_time_ms",
                    device.weighted_io_time_ms,
                    "milliseconds",
                ),
            ] {
                push_counter_metric(
                    &mut samples,
                    MetricDescriptor {
                        family: "disk_io",
                        subject_kind: "block_device",
                        subject_id: &device.identity,
                        metric_name,
                        sample_kind: "counter",
                        unit,
                    },
                    dimensions.clone(),
                    value,
                    observation.disk_io_counters.quality,
                );
            }
        }
    }

    if let Some(interfaces) = observation.network_counters.value.as_ref() {
        for interface in interfaces {
            let mut dimensions = BTreeMap::new();
            dimensions.insert("name".to_owned(), Value::String(interface.name.clone()));
            dimensions.insert("ifindex".to_owned(), Value::from(interface.ifindex));
            for (metric_name, value, unit) in [
                ("rx_bytes", interface.rx_bytes, "bytes"),
                ("rx_packets", interface.rx_packets, "count"),
                ("rx_errors", interface.rx_errors, "count"),
                ("rx_drops", interface.rx_drops, "count"),
                ("tx_bytes", interface.tx_bytes, "bytes"),
                ("tx_packets", interface.tx_packets, "count"),
                ("tx_errors", interface.tx_errors, "count"),
                ("tx_drops", interface.tx_drops, "count"),
            ] {
                push_counter_metric(
                    &mut samples,
                    MetricDescriptor {
                        family: "network",
                        subject_kind: "interface",
                        subject_id: &interface.identity,
                        metric_name,
                        sample_kind: "counter",
                        unit,
                    },
                    dimensions.clone(),
                    value,
                    observation.network_counters.quality,
                );
            }
        }
    }

    if let Some(cpu) = observation.cpu.value.as_ref() {
        for (name, value) in [
            ("busy_pct", cpu.aggregate.busy_pct),
            ("iowait_pct", cpu.aggregate.iowait_pct),
            ("steal_pct", cpu.aggregate.steal_pct),
        ] {
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "cpu",
                    subject_kind: "host",
                    subject_id: host_id,
                    metric_name: name,
                    sample_kind: "derived",
                    unit: "percent",
                },
                BTreeMap::new(),
                Some(value),
                Some(cpu.window_seconds),
                observation.cpu.quality,
            );
        }
        if let Some(count) = cpu.online_cpu_count {
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "cpu",
                    subject_kind: "host",
                    subject_id: host_id,
                    metric_name: "online_cpu_count",
                    sample_kind: "gauge",
                    unit: "count",
                },
                BTreeMap::new(),
                Some(f64::from(count)),
                None,
                cpu.online_cpu_quality,
            );
        }
        for core in &cpu.per_cpu {
            let mut dimensions = BTreeMap::new();
            dimensions.insert("cpu".to_owned(), Value::String(core.cpu.clone()));
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "cpu",
                    subject_kind: "cpu",
                    subject_id: &core.cpu,
                    metric_name: "availability",
                    sample_kind: "gauge",
                    unit: "status",
                },
                dimensions.clone(),
                availability_value(core.quality),
                Some(cpu.window_seconds),
                core.quality,
            );
            if let Some(rate) = core.rate.as_ref() {
                for (name, value) in [
                    ("busy_pct", rate.busy_pct),
                    ("iowait_pct", rate.iowait_pct),
                    ("steal_pct", rate.steal_pct),
                ] {
                    push_metric(
                        &mut samples,
                        MetricDescriptor {
                            family: "cpu",
                            subject_kind: "cpu",
                            subject_id: &core.cpu,
                            metric_name: name,
                            sample_kind: "derived",
                            unit: "percent",
                        },
                        dimensions.clone(),
                        Some(value),
                        Some(cpu.window_seconds),
                        core.quality,
                    );
                }
            }
        }
    }

    if let Some(memory) = observation.memory.value.as_ref() {
        let byte_metrics = [
            (
                "total_bytes",
                Some(memory.total_kib),
                MetricQuality::Observed,
            ),
            (
                "available_bytes",
                memory.available_kib,
                memory.derived_quality,
            ),
            ("used_bytes", memory.used_kib, memory.derived_quality),
            (
                "swap_total_bytes",
                memory.swap_total_kib,
                MetricQuality::Observed,
            ),
            (
                "swap_free_bytes",
                memory.swap_free_kib,
                MetricQuality::Observed,
            ),
            ("cached_bytes", memory.cached_kib, MetricQuality::Observed),
            ("buffers_bytes", memory.buffers_kib, MetricQuality::Observed),
            ("slab_bytes", memory.slab_kib, MetricQuality::Observed),
        ];
        for (name, value, metric_quality) in byte_metrics {
            if let Some(value) = value {
                push_metric(
                    &mut samples,
                    MetricDescriptor {
                        family: "memory",
                        subject_kind: "host",
                        subject_id: host_id,
                        metric_name: name,
                        sample_kind: if name == "used_bytes" {
                            "derived"
                        } else {
                            "gauge"
                        },
                        unit: "bytes",
                    },
                    BTreeMap::new(),
                    Some(kib_as_f64_bytes(value)),
                    None,
                    metric_quality,
                );
            }
        }
    }

    if let Some(load) = observation.load.value.as_ref() {
        for (name, value) in [
            ("load1", load.load1),
            ("load5", load.load5),
            ("load15", load.load15),
        ] {
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "load",
                    subject_kind: "host",
                    subject_id: host_id,
                    metric_name: name,
                    sample_kind: "gauge",
                    unit: "load",
                },
                BTreeMap::new(),
                Some(value),
                None,
                observation.load.quality,
            );
        }
        for (name, value) in [
            ("normalized_load1", load.normalized_load1),
            ("normalized_load5", load.normalized_load5),
            ("normalized_load15", load.normalized_load15),
        ] {
            if let Some(value) = value {
                push_metric(
                    &mut samples,
                    MetricDescriptor {
                        family: "load",
                        subject_kind: "host",
                        subject_id: host_id,
                        metric_name: name,
                        sample_kind: "derived",
                        unit: "ratio",
                    },
                    BTreeMap::new(),
                    Some(value),
                    None,
                    load.normalized_quality,
                );
            }
        }
        for (name, value) in [
            ("runnable_entities", load.runnable_entities),
            ("scheduling_entities", load.total_scheduling_entities),
        ] {
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "load",
                    subject_kind: "host",
                    subject_id: host_id,
                    metric_name: name,
                    sample_kind: "gauge",
                    unit: "count",
                },
                BTreeMap::new(),
                Some(value as f64),
                None,
                observation.load.quality,
            );
        }
    }

    if let Some(filesystems) = observation.disk_capacity.value.as_ref() {
        for filesystem in filesystems {
            // Remote filesystem sources can contain embedded credentials. The mount path is
            // sufficient as the stable identity because a HOST cannot mount two sources at the
            // same path at once; never derive a persisted identifier from the source URL.
            let subject_id = stable_subject_id("filesystem", &filesystem.mount);
            let mut dimensions = BTreeMap::new();
            dimensions.insert("mount".to_owned(), Value::String(filesystem.mount.clone()));
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "disk_capacity",
                    subject_kind: "filesystem",
                    subject_id: &subject_id,
                    metric_name: "availability",
                    sample_kind: "gauge",
                    unit: "status",
                },
                dimensions.clone(),
                availability_value(observation.disk_capacity.quality),
                None,
                observation.disk_capacity.quality,
            );
            for (name, value) in [
                ("size_bytes", Some(filesystem.size_kib)),
                ("used_bytes", Some(filesystem.used_kib)),
                ("available_bytes", Some(filesystem.available_kib)),
                ("inode_total", filesystem.inode_total),
                ("inode_used", filesystem.inode_used),
                ("inode_available", filesystem.inode_available),
            ] {
                if let Some(value) = value {
                    push_metric(
                        &mut samples,
                        MetricDescriptor {
                            family: "disk_capacity",
                            subject_kind: "filesystem",
                            subject_id: &subject_id,
                            metric_name: name,
                            sample_kind: "gauge",
                            unit: if name.starts_with("inode_") {
                                "count"
                            } else {
                                "bytes"
                            },
                        },
                        dimensions.clone(),
                        Some(if name.starts_with("inode_") {
                            value as f64
                        } else {
                            kib_as_f64_bytes(value)
                        }),
                        None,
                        if name.starts_with("inode_") {
                            filesystem.inode_quality
                        } else {
                            observation.disk_capacity.quality
                        },
                    );
                }
            }
            for (name, value, metric_quality) in [
                (
                    "allocatable_used_ratio",
                    filesystem.allocatable_used_ratio,
                    observation.disk_capacity.quality,
                ),
                (
                    "inode_used_ratio",
                    filesystem.inode_used_ratio,
                    filesystem.inode_quality,
                ),
            ] {
                if let Some(value) = value {
                    push_metric(
                        &mut samples,
                        MetricDescriptor {
                            family: "disk_capacity",
                            subject_kind: "filesystem",
                            subject_id: &subject_id,
                            metric_name: name,
                            sample_kind: "derived",
                            unit: "ratio",
                        },
                        dimensions.clone(),
                        Some(value),
                        None,
                        metric_quality,
                    );
                }
            }
        }
    }

    if let Some(devices) = observation.disk_io.value.as_ref() {
        for device in devices {
            let mut dimensions = BTreeMap::new();
            dimensions.insert("name".to_owned(), Value::String(device.name.clone()));
            dimensions.insert("major".to_owned(), Value::from(device.major));
            dimensions.insert("minor".to_owned(), Value::from(device.minor));
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "disk_io",
                    subject_kind: "block_device",
                    subject_id: &device.identity,
                    metric_name: "availability",
                    sample_kind: "gauge",
                    unit: "status",
                },
                dimensions.clone(),
                availability_value(device.quality),
                Some(device.window_seconds),
                device.quality,
            );
            if let Some(metrics) = device.metrics.as_ref() {
                for (name, value, unit) in [
                    (
                        "read_bytes_per_second",
                        metrics.read_bytes_per_second,
                        "bytes_per_second",
                    ),
                    (
                        "write_bytes_per_second",
                        metrics.write_bytes_per_second,
                        "bytes_per_second",
                    ),
                    ("iops", metrics.iops, "operations_per_second"),
                    ("util_pct", metrics.util_pct, "percent"),
                    ("average_queue_depth", metrics.average_queue_depth, "count"),
                ] {
                    push_metric(
                        &mut samples,
                        MetricDescriptor {
                            family: "disk_io",
                            subject_kind: "block_device",
                            subject_id: &device.identity,
                            metric_name: name,
                            sample_kind: "derived",
                            unit,
                        },
                        dimensions.clone(),
                        Some(value),
                        Some(device.window_seconds),
                        device.quality,
                    );
                }
                for (name, value) in [
                    ("read_await_ms", metrics.read_await_ms),
                    ("write_await_ms", metrics.write_await_ms),
                ] {
                    if let Some(value) = value {
                        push_metric(
                            &mut samples,
                            MetricDescriptor {
                                family: "disk_io",
                                subject_kind: "block_device",
                                subject_id: &device.identity,
                                metric_name: name,
                                sample_kind: "derived",
                                unit: "milliseconds",
                            },
                            dimensions.clone(),
                            Some(value),
                            Some(device.window_seconds),
                            device.quality,
                        );
                    }
                }
            }
        }
    }

    if let Some(interfaces) = observation.network.value.as_ref() {
        for interface in interfaces {
            let mut dimensions = BTreeMap::new();
            dimensions.insert("name".to_owned(), Value::String(interface.name.clone()));
            dimensions.insert("ifindex".to_owned(), Value::from(interface.ifindex));
            if let Some(iflink) = interface.iflink {
                dimensions.insert("iflink".to_owned(), Value::from(iflink));
            }
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "network",
                    subject_kind: "interface",
                    subject_id: &interface.identity,
                    metric_name: "availability",
                    sample_kind: "gauge",
                    unit: "status",
                },
                dimensions.clone(),
                availability_value(interface.quality),
                Some(interface.window_seconds),
                interface.quality,
            );
            if let Some(speed) = interface.speed_mbps {
                push_metric(
                    &mut samples,
                    MetricDescriptor {
                        family: "network",
                        subject_kind: "interface",
                        subject_id: &interface.identity,
                        metric_name: "speed_mbps",
                        sample_kind: "gauge",
                        unit: "megabits_per_second",
                    },
                    dimensions.clone(),
                    Some(speed as f64),
                    None,
                    MetricQuality::Observed,
                );
            }
            if let Some(metrics) = interface.metrics.as_ref() {
                for (name, value, unit) in [
                    (
                        "rx_bytes_per_second",
                        metrics.rx_bytes_per_second,
                        "bytes_per_second",
                    ),
                    (
                        "tx_bytes_per_second",
                        metrics.tx_bytes_per_second,
                        "bytes_per_second",
                    ),
                    (
                        "rx_packets_per_second",
                        metrics.rx_packets_per_second,
                        "packets_per_second",
                    ),
                    (
                        "tx_packets_per_second",
                        metrics.tx_packets_per_second,
                        "packets_per_second",
                    ),
                ] {
                    push_metric(
                        &mut samples,
                        MetricDescriptor {
                            family: "network",
                            subject_kind: "interface",
                            subject_id: &interface.identity,
                            metric_name: name,
                            sample_kind: "derived",
                            unit,
                        },
                        dimensions.clone(),
                        Some(value),
                        Some(interface.window_seconds),
                        interface.quality,
                    );
                }
                for (name, value, unit) in [
                    ("rx_error_drop_pct", metrics.rx_error_drop_pct, "percent"),
                    ("tx_error_drop_pct", metrics.tx_error_drop_pct, "percent"),
                    ("rx_util_pct", metrics.rx_util_pct, "percent"),
                    ("tx_util_pct", metrics.tx_util_pct, "percent"),
                ] {
                    if let Some(value) = value {
                        push_metric(
                            &mut samples,
                            MetricDescriptor {
                                family: "network",
                                subject_kind: "interface",
                                subject_id: &interface.identity,
                                metric_name: name,
                                sample_kind: "derived",
                                unit,
                            },
                            dimensions.clone(),
                            Some(value),
                            Some(interface.window_seconds),
                            interface.quality,
                        );
                    }
                }
            }
        }
    }

    if let Some(uptime) = observation.uptime.value.as_ref() {
        let mut dimensions = BTreeMap::new();
        dimensions.insert("boot_id".to_owned(), Value::String(uptime.boot_id.clone()));
        push_metric(
            &mut samples,
            MetricDescriptor {
                family: "uptime",
                subject_kind: "host",
                subject_id: host_id,
                metric_name: "uptime_seconds",
                sample_kind: "gauge",
                unit: "seconds",
            },
            dimensions,
            Some(uptime.uptime_seconds),
            None,
            observation.uptime.quality,
        );
    }

    if let Some(process) = observation.process.value.as_ref() {
        for (name, value) in [
            ("scanned", process.scanned),
            ("running", process.running),
            ("blocked", process.blocked),
            ("zombie", process.zombie),
            ("raced", process.raced),
        ] {
            push_metric(
                &mut samples,
                MetricDescriptor {
                    family: "process",
                    subject_kind: "process",
                    subject_id: "summary",
                    metric_name: name,
                    sample_kind: "gauge",
                    unit: "count",
                },
                BTreeMap::new(),
                Some(f64::from(value)),
                None,
                observation.process.quality,
            );
        }
    }

    samples
}

pub async fn insert_samples(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    host_id: &str,
    observed_at: &str,
    observed_at_epoch_ms: i64,
    samples: &[MetricSampleInput],
) -> Result<u64, sqlx::Error> {
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut inserted = 0u64;
    for sample in samples {
        let result = sqlx::query(
            "INSERT INTO metric_samples(
                sample_id, run_id, host_id, family, subject_kind, subject_id,
                metric_name, dimensions_json, dimensions_sha256, sample_kind,
                value_real, value_integer, unit, window_seconds, quality,
                observed_at, observed_at_epoch_ms, source_kind, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(run_id)
        .bind(host_id)
        .bind(&sample.family)
        .bind(&sample.subject_kind)
        .bind(&sample.subject_id)
        .bind(&sample.metric_name)
        .bind(&sample.dimensions_json)
        .bind(&sample.dimensions_sha256)
        .bind(&sample.sample_kind)
        .bind(sample.value_real)
        .bind(sample.value_integer)
        .bind(&sample.unit)
        .bind(sample.window_seconds)
        .bind(&sample.quality)
        .bind(observed_at)
        .bind(observed_at_epoch_ms)
        .bind(HISTORY_SOURCE_KIND)
        .bind(&created_at)
        .execute(&mut **tx)
        .await?;
        inserted = inserted.saturating_add(result.rows_affected());
    }
    Ok(inserted)
}

fn push_family_availability<T>(
    samples: &mut Vec<MetricSampleInput>,
    host_id: &str,
    family: &str,
    observation: &FamilyObservation<T>,
) {
    push_metric(
        samples,
        MetricDescriptor {
            family,
            subject_kind: "host",
            subject_id: host_id,
            metric_name: "availability",
            sample_kind: "gauge",
            unit: "status",
        },
        BTreeMap::new(),
        availability_value(observation.quality),
        None,
        observation.quality,
    );
}

fn availability_value(quality: MetricQuality) -> Option<f64> {
    quality.is_observed().then_some(1.0)
}

fn push_cpu_tick_counters(
    samples: &mut Vec<MetricSampleInput>,
    subject_kind: &str,
    subject_id: &str,
    dimensions: BTreeMap<String, Value>,
    ticks: crate::monitoring::CpuTicks,
    metric_quality: MetricQuality,
) {
    for (metric_name, value) in [
        ("user_ticks", ticks.user),
        ("nice_ticks", ticks.nice),
        ("system_ticks", ticks.system),
        ("idle_ticks", ticks.idle),
        ("iowait_ticks", ticks.iowait),
        ("irq_ticks", ticks.irq),
        ("softirq_ticks", ticks.softirq),
        ("steal_ticks", ticks.steal),
    ] {
        push_counter_metric(
            samples,
            MetricDescriptor {
                family: "cpu",
                subject_kind,
                subject_id,
                metric_name,
                sample_kind: "counter",
                unit: "ticks",
            },
            dimensions.clone(),
            value,
            metric_quality,
        );
    }
}

fn push_counter_metric(
    samples: &mut Vec<MetricSampleInput>,
    descriptor: MetricDescriptor<'_>,
    dimensions: BTreeMap<String, Value>,
    value: u64,
    metric_quality: MetricQuality,
) {
    let (value_integer, metric_quality) = if metric_quality.is_observed() {
        match i64::try_from(value) {
            Ok(value) => (Some(value), metric_quality),
            Err(_) => (None, MetricQuality::CounterUnreliable),
        }
    } else {
        (None, metric_quality)
    };
    let dimensions_json = serde_json::to_string(&dimensions).unwrap_or_else(|_| "{}".to_owned());
    samples.push(MetricSampleInput {
        family: descriptor.family.to_owned(),
        subject_kind: descriptor.subject_kind.to_owned(),
        subject_id: descriptor.subject_id.to_owned(),
        metric_name: descriptor.metric_name.to_owned(),
        dimensions_sha256: hex_digest(&Sha256::digest(dimensions_json.as_bytes())),
        dimensions_json,
        sample_kind: descriptor.sample_kind.to_owned(),
        value_real: None,
        value_integer,
        unit: descriptor.unit.to_owned(),
        window_seconds: None,
        quality: quality_name(metric_quality).to_owned(),
    });
}

fn push_metric(
    samples: &mut Vec<MetricSampleInput>,
    descriptor: MetricDescriptor<'_>,
    dimensions: BTreeMap<String, Value>,
    value_real: Option<f64>,
    window_seconds: Option<f64>,
    metric_quality: MetricQuality,
) {
    let finite_value = value_real.filter(|value| value.is_finite());
    let (value_real, metric_quality) = if metric_quality.is_observed() {
        match finite_value {
            Some(value) => (Some(value), metric_quality),
            None => (None, MetricQuality::ParseFailed),
        }
    } else {
        (None, metric_quality)
    };
    let dimensions_json = serde_json::to_string(&dimensions).unwrap_or_else(|_| "{}".to_owned());
    samples.push(MetricSampleInput {
        family: descriptor.family.to_owned(),
        subject_kind: descriptor.subject_kind.to_owned(),
        subject_id: descriptor.subject_id.to_owned(),
        metric_name: descriptor.metric_name.to_owned(),
        dimensions_sha256: hex_digest(&Sha256::digest(dimensions_json.as_bytes())),
        dimensions_json,
        sample_kind: descriptor.sample_kind.to_owned(),
        value_real,
        value_integer: None,
        unit: descriptor.unit.to_owned(),
        window_seconds: window_seconds.filter(|value| value.is_finite() && *value > 0.0),
        quality: quality_name(metric_quality).to_owned(),
    });
}

fn quality_name(value: MetricQuality) -> &'static str {
    match value {
        MetricQuality::Observed => "observed",
        MetricQuality::Unsupported => "unsupported",
        MetricQuality::ParseFailed => "parse_failed",
        MetricQuality::CounterReset => "counter_reset",
        MetricQuality::CounterUnreliable => "counter_unreliable",
        MetricQuality::InsufficientInterval => "insufficient_interval",
        MetricQuality::PermissionDenied => "permission_denied",
        MetricQuality::TimedOut => "timed_out",
    }
}

fn kib_as_f64_bytes(value: u64) -> f64 {
    value as f64 * 1024.0
}

pub(crate) fn stable_subject_id(kind: &str, value: &str) -> String {
    format!("{kind}:{}", hex_digest(&Sha256::digest(value.as_bytes())))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct MetricHistoryQuery {
    pub from: String,
    pub to: String,
    #[serde(default = "default_requested_resolution")]
    #[param(required = false)]
    pub resolution: MetricHistoryRequestedResolution,
    #[param(required = false)]
    pub family: Option<MetricHistoryFamily>,
    #[param(required = false)]
    pub subject_kind: Option<MetricHistorySubjectKind>,
    #[param(required = false, min_length = 1, max_length = 512)]
    pub subject_id: Option<String>,
    #[param(
        required = false,
        min_length = 1,
        max_length = 128,
        pattern = "^[A-Za-z0-9_.:-]+$"
    )]
    pub metric_name: Option<String>,
    #[param(required = false)]
    pub sample_kind: Option<MetricHistorySampleKind>,
    #[param(required = false, minimum = 0)]
    pub after_epoch_ms: Option<i64>,
    #[param(required = false, min_length = 36, max_length = 36)]
    pub after_sample_id: Option<String>,
    #[param(required = false, minimum = 1, maximum = 5000)]
    pub limit: Option<u32>,
}

fn default_requested_resolution() -> MetricHistoryRequestedResolution {
    MetricHistoryRequestedResolution::Auto
}

#[derive(Debug, Clone)]
struct EffectiveHistoryFilters {
    family: Option<String>,
    subject_kind: String,
    subject_id: Option<String>,
    metric_name: Option<String>,
    sample_kind: Option<String>,
}

#[derive(Debug)]
struct MetricHistoryPage {
    series: Vec<MetricHistorySeries>,
    has_more: bool,
    next_cursor: Option<MetricHistoryCursor>,
}

fn validate_query_options(
    host_id: &str,
    query: &MetricHistoryQuery,
) -> Result<(EffectiveHistoryFilters, Option<MetricHistoryCursor>, u32), HistoryError> {
    if query.subject_id.is_some() && query.subject_kind.is_none() {
        return Err(bad_query(
            "INVALID_SUBJECT_FILTER",
            "subject_id 必须与 subject_kind 同时使用",
            json!({"field": "subject_id"}),
        ));
    }
    if let Some(subject_id) = query.subject_id.as_deref()
        && !valid_bounded_text(subject_id, MAX_SUBJECT_ID_CHARS)
    {
        return Err(bad_query(
            "INVALID_SUBJECT_FILTER",
            "subject_id 长度或字符无效",
            json!({"field": "subject_id", "maximum_characters": MAX_SUBJECT_ID_CHARS}),
        ));
    }
    if let Some(metric_name) = query.metric_name.as_deref()
        && !valid_metric_name(metric_name)
    {
        return Err(bad_query(
            "INVALID_METRIC_NAME",
            "metric_name 只能包含字母、数字、点、下划线、冒号或连字符",
            json!({"field": "metric_name", "maximum_characters": MAX_METRIC_NAME_CHARS}),
        ));
    }

    let subject_kind = query
        .subject_kind
        .as_ref()
        .map(subject_kind_name)
        .unwrap_or("host")
        .to_owned();
    let subject_id = match (&query.subject_kind, query.subject_id.as_deref()) {
        (None, None) | (Some(MetricHistorySubjectKind::Host), None) => Some(host_id.to_owned()),
        (Some(MetricHistorySubjectKind::Host), Some(value)) if value != host_id => {
            return Err(bad_query(
                "INVALID_SUBJECT_FILTER",
                "host subject_id 必须等于路径中的 HOST id",
                json!({"field": "subject_id"}),
            ));
        }
        (_, value) => value.map(str::to_owned),
    };

    let cursor = match (query.after_epoch_ms, query.after_sample_id.as_deref()) {
        (None, None) => None,
        (Some(after_epoch_ms), Some(after_sample_id))
            if after_epoch_ms >= 0 && valid_cursor_sample_id(after_sample_id) =>
        {
            Some(MetricHistoryCursor {
                after_epoch_ms,
                after_sample_id: after_sample_id.to_owned(),
            })
        }
        _ => {
            return Err(bad_query(
                "INVALID_CURSOR",
                "after_epoch_ms 与 after_sample_id 必须成对提供且值有效",
                json!({
                    "fields": ["after_epoch_ms", "after_sample_id"],
                    "ordering": ["observed_at_epoch_ms", "sample_id"]
                }),
            ));
        }
    };

    let limit = query.limit.unwrap_or(DEFAULT_HISTORY_PAGE_LIMIT);
    if !(1..=MAX_HISTORY_PAGE_LIMIT).contains(&limit) {
        return Err(bad_query(
            "INVALID_LIMIT",
            "limit 超出允许范围",
            json!({"minimum": 1, "maximum": MAX_HISTORY_PAGE_LIMIT}),
        ));
    }

    Ok((
        EffectiveHistoryFilters {
            family: query.family.as_ref().map(family_name).map(str::to_owned),
            subject_kind,
            subject_id,
            metric_name: query.metric_name.clone(),
            sample_kind: query
                .sample_kind
                .as_ref()
                .map(sample_kind_name)
                .map(str::to_owned),
        },
        cursor,
        limit,
    ))
}

fn valid_bounded_text(value: &str, maximum_characters: usize) -> bool {
    let count = value.chars().count();
    (1..=maximum_characters).contains(&count)
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_metric_name(value: &str) -> bool {
    valid_bounded_text(value, MAX_METRIC_NAME_CHARS)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_cursor_sample_id(value: &str) -> bool {
    valid_bounded_text(value, MAX_CURSOR_SAMPLE_ID_CHARS)
        && Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("invalid metric history query")]
    BadRequest {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error("HOST was not found")]
    NotFound { host_id: String },
    #[error("metric history storage unavailable")]
    Storage(#[source] sqlx::Error),
    #[error("metric history contains an invalid persisted value")]
    Internal,
}

impl IntoResponse for HistoryError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4().to_string();
        let (status, code, message, details) = match self {
            Self::BadRequest {
                code,
                message,
                details,
            } => (StatusCode::BAD_REQUEST, code, message, details),
            Self::NotFound { host_id } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "请求的 HOST 不存在",
                json!({"resource": "host", "id": host_id}),
            ),
            Self::Storage(error) => {
                tracing::error!(%request_id, error = %error, "metric history storage failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "本地监控历史存储暂不可用",
                    json!({}),
                )
            }
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "监控历史内部处理失败",
                json!({}),
            ),
        };
        (
            status,
            Json(ApiErrorResponse {
                error: ApiErrorBody {
                    code: code.to_owned(),
                    message: message.to_owned(),
                    details,
                    request_id,
                },
            }),
        )
            .into_response()
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host_id}/metrics",
    tag = "monitoring",
    params(
        ("host_id" = String, Path, description = "Registered Linux host identifier"),
        MetricHistoryQuery
    ),
    responses(
        (status = 200, body = MetricHistoryResponse),
        (status = 400, body = ApiErrorResponse),
        (status = 404, body = ApiErrorResponse),
        (status = 503, body = ApiErrorResponse)
    )
)]
pub async fn get_host_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
    query: Result<Query<MetricHistoryQuery>, QueryRejection>,
) -> Result<Json<MetricHistoryResponse>, HistoryError> {
    let Query(query) = query.map_err(|_| bad_query("INVALID_QUERY", "查询参数无效", json!({})))?;
    ensure_host_exists(&state, &host_id).await?;
    let from = parse_query_time("from", &query.from)?;
    let to = parse_query_time("to", &query.to)?;
    validate_range(from, to, &query.resolution)?;
    let (filters, cursor, limit) = validate_query_options(&host_id, &query)?;
    let now = Utc::now();
    let history_started_at: String = sqlx::query_scalar(
        "SELECT history_started_at
         FROM monitoring_history_metadata WHERE singleton_id = 1",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    let history_started = DateTime::parse_from_rfc3339(&history_started_at)
        .map_err(|_| HistoryError::Internal)?
        .with_timezone(&Utc);
    let effective_from = from.max(history_started);
    let actual_resolution = choose_resolution(
        &query.resolution,
        from,
        to,
        now,
        state.monitoring_rollup.settings(),
    );
    let page = match &actual_resolution {
        MetricHistoryResolution::Raw => {
            query_metric_series(
                &state,
                &host_id,
                effective_from,
                to,
                &filters,
                cursor.as_ref(),
                limit,
            )
            .await?
        }
        MetricHistoryResolution::Hour => {
            query_rollup_series(
                &state,
                &host_id,
                effective_from,
                to,
                &filters,
                cursor.as_ref(),
                limit,
                RollupResolution::Hour,
            )
            .await?
        }
        MetricHistoryResolution::Day => {
            query_rollup_series(
                &state,
                &host_id,
                effective_from,
                to,
                &filters,
                cursor.as_ref(),
                limit,
                RollupResolution::Day,
            )
            .await?
        }
    };
    let coverage = query_coverage(
        &state,
        &host_id,
        effective_from,
        to,
        filters.family.as_deref(),
        &actual_resolution,
    )
    .await?;
    let latest_observation_at = query_latest_observation(
        &state,
        &host_id,
        effective_from,
        to,
        &filters,
        &actual_resolution,
    )
    .await?;
    let (latest_valid_sample_at, latest_valid_until, freshness) = query_freshness(
        &state,
        &host_id,
        effective_from,
        to,
        now,
        &filters,
        &actual_resolution,
    )
    .await?;
    let requested_resolution = query.resolution;
    let retention_tier = match &actual_resolution {
        MetricHistoryResolution::Raw => "full",
        MetricHistoryResolution::Hour => "hour",
        MetricHistoryResolution::Day => "day",
    };
    Ok(Json(MetricHistoryResponse {
        data: MetricHistoryData {
            host_id,
            from: timestamp(from),
            to: timestamp(to),
            requested_resolution,
            actual_resolution,
            retention_tier: retention_tier.to_owned(),
            history_started_at,
            latest_observation_at,
            latest_valid_sample_at,
            latest_valid_until,
            freshness,
            coverage,
            series: page.series,
            limit,
            has_more: page.has_more,
            next_cursor: page.next_cursor,
        },
        meta: history_meta(request_id(&headers)),
    }))
}

async fn ensure_host_exists(state: &AppState, host_id: &str) -> Result<(), HistoryError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hosts WHERE host_id = ?")
        .bind(host_id)
        .fetch_one(&state.pool)
        .await
        .map_err(HistoryError::Storage)?;
    if exists == 0 {
        return Err(HistoryError::NotFound {
            host_id: host_id.to_owned(),
        });
    }
    Ok(())
}

fn parse_query_time(field: &'static str, value: &str) -> Result<DateTime<Utc>, HistoryError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            bad_query(
                "INVALID_TIME_RANGE",
                "from/to 必须是 RFC3339 时间",
                json!({"field": field}),
            )
        })
}

fn validate_range(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    resolution: &MetricHistoryRequestedResolution,
) -> Result<(), HistoryError> {
    if from >= to {
        return Err(bad_query(
            "INVALID_TIME_RANGE",
            "from 必须早于 to",
            json!({}),
        ));
    }
    let maximum_seconds = match resolution {
        MetricHistoryRequestedResolution::Raw => RAW_QUERY_MAX_SPAN_SECONDS,
        MetricHistoryRequestedResolution::Hour => HOUR_QUERY_MAX_SPAN_SECONDS,
        MetricHistoryRequestedResolution::Auto | MetricHistoryRequestedResolution::Day => {
            DAY_QUERY_MAX_SPAN_SECONDS
        }
    };
    if to - from > Duration::seconds(maximum_seconds) {
        return Err(bad_query(
            "TIME_RANGE_TOO_LARGE",
            "指标查询范围超过所选 resolution 的上限",
            json!({"maximum_seconds": maximum_seconds}),
        ));
    }
    if to > Utc::now() + Duration::minutes(5) {
        return Err(bad_query(
            "INVALID_TIME_RANGE",
            "to 不得超过当前时间五分钟",
            json!({}),
        ));
    }
    Ok(())
}

fn choose_resolution(
    requested: &MetricHistoryRequestedResolution,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    evaluated_at: DateTime<Utc>,
    retention: RollupSettings,
) -> MetricHistoryResolution {
    let span = to - from;
    let raw_horizon = retention_horizon(evaluated_at, retention.raw_observed_retention_days);
    let hour_horizon = retention_horizon(evaluated_at, retention.hour_retention_days);
    match requested {
        MetricHistoryRequestedResolution::Raw => MetricHistoryResolution::Raw,
        MetricHistoryRequestedResolution::Hour => MetricHistoryResolution::Hour,
        MetricHistoryRequestedResolution::Day => MetricHistoryResolution::Day,
        MetricHistoryRequestedResolution::Auto
            if span <= Duration::seconds(RAW_QUERY_MAX_SPAN_SECONDS)
                && (!retention.retention_enabled || from >= raw_horizon) =>
        {
            MetricHistoryResolution::Raw
        }
        MetricHistoryRequestedResolution::Auto
            if span <= Duration::seconds(HOUR_QUERY_MAX_SPAN_SECONDS)
                && (!retention.retention_enabled || from >= hour_horizon) =>
        {
            MetricHistoryResolution::Hour
        }
        MetricHistoryRequestedResolution::Auto => MetricHistoryResolution::Day,
    }
}

fn retention_horizon(evaluated_at: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    evaluated_at
        .checked_sub_signed(Duration::days(i64::from(days)))
        .map(|value| {
            value
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .map(|value| value.and_utc())
                .unwrap_or(value)
        })
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
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

struct SeriesAccumulator {
    dimensions: Value,
    points: Vec<MetricHistoryPoint>,
}

async fn query_metric_series(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    filters: &EffectiveHistoryFilters,
    cursor: Option<&MetricHistoryCursor>,
    limit: u32,
) -> Result<MetricHistoryPage, HistoryError> {
    let from_epoch_ms = from.timestamp_millis();
    let to_epoch_ms = to.timestamp_millis();
    let cursor_epoch_ms = cursor.map(|value| value.after_epoch_ms);
    let cursor_sample_id = cursor.map(|value| value.after_sample_id.as_str());
    let mut rows = sqlx::query(
        "SELECT sample_id, run_id, family, subject_kind, subject_id, metric_name,
                dimensions_json, dimensions_sha256, sample_kind,
                source_kind, unit, observed_at, observed_at_epoch_ms, value_real,
                value_integer,
                window_seconds, quality
         FROM metric_samples
         WHERE host_id = ?
           AND observed_at_epoch_ms >= ? AND observed_at_epoch_ms < ?
           AND (? IS NULL OR family = ?)
           AND subject_kind = ?
           AND (? IS NULL OR subject_id = ?)
           AND (? IS NULL OR metric_name = ?)
           AND (? IS NULL OR sample_kind = ?)
           AND (
                ? IS NULL
                OR observed_at_epoch_ms > ?
                OR (observed_at_epoch_ms = ? AND sample_id > ?)
           )
         ORDER BY observed_at_epoch_ms, sample_id
         LIMIT ?",
    )
    .bind(host_id)
    .bind(from_epoch_ms)
    .bind(to_epoch_ms)
    .bind(filters.family.as_deref())
    .bind(filters.family.as_deref())
    .bind(&filters.subject_kind)
    .bind(filters.subject_id.as_deref())
    .bind(filters.subject_id.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(cursor_epoch_ms)
    .bind(cursor_epoch_ms)
    .bind(cursor_epoch_ms)
    .bind(cursor_sample_id)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                Ok(MetricHistoryCursor {
                    after_epoch_ms: row
                        .try_get("observed_at_epoch_ms")
                        .map_err(HistoryError::Storage)?,
                    after_sample_id: row.try_get("sample_id").map_err(HistoryError::Storage)?,
                })
            })
            .transpose()?
    } else {
        None
    };
    let mut grouped = BTreeMap::<SeriesKey, SeriesAccumulator>::new();
    for row in rows {
        let key = SeriesKey {
            family: row.try_get("family").map_err(HistoryError::Storage)?,
            subject_kind: row.try_get("subject_kind").map_err(HistoryError::Storage)?,
            subject_id: row.try_get("subject_id").map_err(HistoryError::Storage)?,
            metric_name: row.try_get("metric_name").map_err(HistoryError::Storage)?,
            dimensions_sha256: row
                .try_get("dimensions_sha256")
                .map_err(HistoryError::Storage)?,
            sample_kind: row.try_get("sample_kind").map_err(HistoryError::Storage)?,
            source_kind: row.try_get("source_kind").map_err(HistoryError::Storage)?,
            unit: row.try_get("unit").map_err(HistoryError::Storage)?,
        };
        let dimensions_json: String = row
            .try_get("dimensions_json")
            .map_err(HistoryError::Storage)?;
        let dimensions =
            serde_json::from_str(&dimensions_json).map_err(|_| HistoryError::Internal)?;
        let quality_name: String = row.try_get("quality").map_err(HistoryError::Storage)?;
        let value_real: Option<f64> = row.try_get("value_real").map_err(HistoryError::Storage)?;
        let value_integer: Option<i64> = row
            .try_get("value_integer")
            .map_err(HistoryError::Storage)?;
        let point = MetricHistoryPoint {
            sample_id: row.try_get("sample_id").map_err(HistoryError::Storage)?,
            run_id: row.try_get("run_id").map_err(HistoryError::Storage)?,
            at: row.try_get("observed_at").map_err(HistoryError::Storage)?,
            at_epoch_ms: row
                .try_get("observed_at_epoch_ms")
                .map_err(HistoryError::Storage)?,
            value: value_real.or_else(|| value_integer.map(|value| value as f64)),
            value_integer: value_integer.map(|value| value.to_string()),
            quality: parse_metric_quality(&quality_name)?,
            window_seconds: row
                .try_get("window_seconds")
                .map_err(HistoryError::Storage)?,
            rollup: None,
        };
        grouped
            .entry(key)
            .or_insert_with(|| SeriesAccumulator {
                dimensions,
                points: Vec::new(),
            })
            .points
            .push(point);
    }
    let series = grouped
        .into_iter()
        .map(|(key, value)| {
            Ok(MetricHistorySeries {
                family: parse_family(&key.family)?,
                subject_kind: parse_subject_kind(&key.subject_kind)?,
                subject_id: key.subject_id,
                metric_name: key.metric_name,
                dimensions: value.dimensions,
                sample_kind: parse_sample_kind(&key.sample_kind)?,
                source_kind: parse_source_kind(&key.source_kind)?,
                unit: key.unit,
                points: value.points,
            })
        })
        .collect::<Result<Vec<_>, HistoryError>>()?;
    Ok(MetricHistoryPage {
        series,
        has_more,
        next_cursor,
    })
}

#[allow(clippy::too_many_arguments)]
async fn query_rollup_series(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    filters: &EffectiveHistoryFilters,
    cursor: Option<&MetricHistoryCursor>,
    limit: u32,
    resolution: RollupResolution,
) -> Result<MetricHistoryPage, HistoryError> {
    let cursor_epoch_ms = cursor.map(|value| value.after_epoch_ms);
    let cursor_rollup_id = cursor.map(|value| value.after_sample_id.as_str());
    let mut rows = sqlx::query(
        "SELECT rollup_id, last_run_id, family, subject_kind, subject_id,
                metric_name, dimensions_json, dimensions_sha256, sample_kind,
                source_kind, unit, bucket_start, bucket_start_epoch_ms,
                bucket_end, bucket_end_epoch_ms, sample_count, observed_count,
                non_observed_count, expected_count, missing_count, min_value,
                max_value, average_value, p95_value, last_value, counter_delta,
                reset_count, quality, quality_counts_json, input_digest
         FROM metric_rollups
         WHERE host_id = ? AND resolution = ?
           AND bucket_end_epoch_ms > ? AND bucket_start_epoch_ms < ?
           AND (? IS NULL OR family = ?)
           AND subject_kind = ?
           AND (? IS NULL OR subject_id = ?)
           AND (? IS NULL OR metric_name = ?)
           AND (? IS NULL OR sample_kind = ?)
           AND (
                ? IS NULL
                OR bucket_start_epoch_ms > ?
                OR (bucket_start_epoch_ms = ? AND rollup_id > ?)
           )
         ORDER BY bucket_start_epoch_ms, rollup_id
         LIMIT ?",
    )
    .bind(host_id)
    .bind(resolution.as_str())
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(filters.family.as_deref())
    .bind(filters.family.as_deref())
    .bind(&filters.subject_kind)
    .bind(filters.subject_id.as_deref())
    .bind(filters.subject_id.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(cursor_epoch_ms)
    .bind(cursor_epoch_ms)
    .bind(cursor_epoch_ms)
    .bind(cursor_rollup_id)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if has_more {
        rows.last()
            .map(|row| {
                Ok(MetricHistoryCursor {
                    after_epoch_ms: row
                        .try_get("bucket_start_epoch_ms")
                        .map_err(HistoryError::Storage)?,
                    after_sample_id: row.try_get("rollup_id").map_err(HistoryError::Storage)?,
                })
            })
            .transpose()?
    } else {
        None
    };
    let contract_resolution = match resolution {
        RollupResolution::Hour => MetricHistoryResolution::Hour,
        RollupResolution::Day => MetricHistoryResolution::Day,
    };
    let mut grouped = BTreeMap::<SeriesKey, SeriesAccumulator>::new();
    for row in rows {
        let key = SeriesKey {
            family: row.try_get("family").map_err(HistoryError::Storage)?,
            subject_kind: row.try_get("subject_kind").map_err(HistoryError::Storage)?,
            subject_id: row.try_get("subject_id").map_err(HistoryError::Storage)?,
            metric_name: row.try_get("metric_name").map_err(HistoryError::Storage)?,
            dimensions_sha256: row
                .try_get("dimensions_sha256")
                .map_err(HistoryError::Storage)?,
            sample_kind: row.try_get("sample_kind").map_err(HistoryError::Storage)?,
            source_kind: row.try_get("source_kind").map_err(HistoryError::Storage)?,
            unit: row.try_get("unit").map_err(HistoryError::Storage)?,
        };
        let dimensions_json: String = row
            .try_get("dimensions_json")
            .map_err(HistoryError::Storage)?;
        let dimensions: Value =
            serde_json::from_str(&dimensions_json).map_err(|_| HistoryError::Internal)?;
        if !dimensions.is_object() {
            return Err(HistoryError::Internal);
        }
        let quality_counts_json: String = row
            .try_get("quality_counts_json")
            .map_err(HistoryError::Storage)?;
        let quality_counts: Value =
            serde_json::from_str(&quality_counts_json).map_err(|_| HistoryError::Internal)?;
        if !quality_counts.is_object() {
            return Err(HistoryError::Internal);
        }
        let sample_kind: String = row.try_get("sample_kind").map_err(HistoryError::Storage)?;
        let counter_delta: Option<i64> = row
            .try_get("counter_delta")
            .map_err(HistoryError::Storage)?;
        let average: Option<f64> = row
            .try_get("average_value")
            .map_err(HistoryError::Storage)?;
        let sample_count =
            non_negative_u32(row.try_get("sample_count").map_err(HistoryError::Storage)?);
        let observed_count = non_negative_u32(
            row.try_get("observed_count")
                .map_err(HistoryError::Storage)?,
        );
        let non_observed_count = non_negative_u32(
            row.try_get("non_observed_count")
                .map_err(HistoryError::Storage)?,
        );
        let expected_count = optional_non_negative_u32(
            row.try_get("expected_count")
                .map_err(HistoryError::Storage)?,
        );
        let missing_count = optional_non_negative_u32(
            row.try_get("missing_count")
                .map_err(HistoryError::Storage)?,
        );
        let bucket_start: String = row.try_get("bucket_start").map_err(HistoryError::Storage)?;
        let bucket_end: String = row.try_get("bucket_end").map_err(HistoryError::Storage)?;
        let point = MetricHistoryPoint {
            sample_id: row.try_get("rollup_id").map_err(HistoryError::Storage)?,
            run_id: row.try_get("last_run_id").map_err(HistoryError::Storage)?,
            at: bucket_start.clone(),
            at_epoch_ms: row
                .try_get("bucket_start_epoch_ms")
                .map_err(HistoryError::Storage)?,
            value: if sample_kind == "counter" {
                counter_delta.map(|value| value as f64)
            } else {
                average
            },
            value_integer: counter_delta.map(|value| value.to_string()),
            quality: parse_metric_quality(
                &row.try_get::<String, _>("quality")
                    .map_err(HistoryError::Storage)?,
            )?,
            window_seconds: Some(f64::from(resolution.bucket_seconds())),
            rollup: Some(MetricHistoryRollupStatistics {
                resolution: contract_resolution.clone(),
                bucket_start,
                bucket_end,
                bucket_width_seconds: resolution.bucket_seconds(),
                sample_count,
                observed_count,
                non_observed_count,
                expected_count,
                missing_count,
                reset_count: non_negative_u32(
                    row.try_get("reset_count").map_err(HistoryError::Storage)?,
                ),
                min: row.try_get("min_value").map_err(HistoryError::Storage)?,
                max: row.try_get("max_value").map_err(HistoryError::Storage)?,
                average,
                p95: row.try_get("p95_value").map_err(HistoryError::Storage)?,
                last: row.try_get("last_value").map_err(HistoryError::Storage)?,
                counter_delta_integer: counter_delta.map(|value| value.to_string()),
                quality_counts,
                input_digest: row.try_get("input_digest").map_err(HistoryError::Storage)?,
            }),
        };
        grouped
            .entry(key)
            .or_insert_with(|| SeriesAccumulator {
                dimensions,
                points: Vec::new(),
            })
            .points
            .push(point);
    }
    let series = grouped
        .into_iter()
        .map(|(key, value)| {
            Ok(MetricHistorySeries {
                family: parse_family(&key.family)?,
                subject_kind: parse_subject_kind(&key.subject_kind)?,
                subject_id: key.subject_id,
                metric_name: key.metric_name,
                dimensions: value.dimensions,
                sample_kind: parse_sample_kind(&key.sample_kind)?,
                source_kind: parse_source_kind(&key.source_kind)?,
                unit: key.unit,
                points: value.points,
            })
        })
        .collect::<Result<Vec<_>, HistoryError>>()?;
    Ok(MetricHistoryPage {
        series,
        has_more,
        next_cursor,
    })
}

async fn query_coverage(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    family: Option<&str>,
    resolution: &MetricHistoryResolution,
) -> Result<MetricHistoryCoverage, HistoryError> {
    if !matches!(resolution, MetricHistoryResolution::Raw) {
        return query_rollup_coverage(state, host_id, from, to, family, resolution).await;
    }
    let observed: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT run_id)
         FROM metric_samples
         WHERE host_id = ? AND observed_at_epoch_ms >= ? AND observed_at_epoch_ms < ?
           AND subject_kind = 'host' AND metric_name = 'availability'
           AND quality = 'observed' AND (? IS NULL OR family = ?)",
    )
    .bind(host_id)
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(family)
    .bind(family)
    .fetch_one(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    Ok(MetricHistoryCoverage {
        expected_count: None,
        observed_count: non_negative_u32(observed),
        gap_count: None,
        coverage: None,
        provenance_complete: false,
    })
}

async fn query_latest_observation(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    filters: &EffectiveHistoryFilters,
    resolution: &MetricHistoryResolution,
) -> Result<Option<String>, HistoryError> {
    if !matches!(resolution, MetricHistoryResolution::Raw) {
        return query_rollup_latest_observation(state, host_id, from, to, filters, resolution)
            .await;
    }
    sqlx::query_scalar(
        "SELECT observed_at
         FROM metric_samples
         WHERE host_id = ?
           AND observed_at_epoch_ms >= ? AND observed_at_epoch_ms < ?
           AND (? IS NULL OR family = ?)
           AND subject_kind = ?
           AND (? IS NULL OR subject_id = ?)
           AND (? IS NULL OR metric_name = ?)
           AND (? IS NULL OR sample_kind = ?)
         ORDER BY observed_at_epoch_ms DESC, sample_id DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(filters.family.as_deref())
    .bind(filters.family.as_deref())
    .bind(&filters.subject_kind)
    .bind(filters.subject_id.as_deref())
    .bind(filters.subject_id.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(filters.sample_kind.as_deref())
    .fetch_optional(&state.pool)
    .await
    .map_err(HistoryError::Storage)
}

async fn query_freshness(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    evaluated_at: DateTime<Utc>,
    filters: &EffectiveHistoryFilters,
    resolution: &MetricHistoryResolution,
) -> Result<(Option<String>, Option<String>, MonitorFreshness), HistoryError> {
    if !matches!(resolution, MetricHistoryResolution::Raw) {
        return query_rollup_freshness(state, host_id, from, to, evaluated_at, filters, resolution)
            .await;
    }
    let row = sqlx::query(
        "SELECT samples.observed_at, runs.stale_after_seconds
         FROM metric_samples samples
         JOIN monitor_runs runs ON runs.run_id = samples.run_id
         WHERE samples.host_id = ?
           AND samples.observed_at_epoch_ms >= ? AND samples.observed_at_epoch_ms < ?
           AND samples.quality = 'observed'
           AND (samples.value_real IS NOT NULL OR samples.value_integer IS NOT NULL)
           AND (? IS NULL OR samples.family = ?)
           AND samples.subject_kind = ?
           AND (? IS NULL OR samples.subject_id = ?)
           AND (? IS NULL OR samples.metric_name = ?)
           AND (? IS NULL OR samples.sample_kind = ?)
         ORDER BY samples.observed_at_epoch_ms DESC, samples.sample_id DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(filters.family.as_deref())
    .bind(filters.family.as_deref())
    .bind(&filters.subject_kind)
    .bind(filters.subject_id.as_deref())
    .bind(filters.subject_id.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(filters.sample_kind.as_deref())
    .fetch_optional(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    let Some(row) = row else {
        return Ok((None, None, MonitorFreshness::Unknown));
    };
    let observed_at: String = row.try_get("observed_at").map_err(HistoryError::Storage)?;
    let stale_after: i64 = row
        .try_get("stale_after_seconds")
        .map_err(HistoryError::Storage)?;
    let observed = DateTime::parse_from_rfc3339(&observed_at)
        .map_err(|_| HistoryError::Internal)?
        .with_timezone(&Utc);
    let freshness = if observed + Duration::seconds(stale_after.max(60)) >= evaluated_at {
        MonitorFreshness::Fresh
    } else {
        MonitorFreshness::Stale
    };
    let valid_until = timestamp(observed + Duration::seconds(stale_after.max(60)));
    Ok((Some(observed_at), Some(valid_until), freshness))
}

async fn query_rollup_coverage(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    family: Option<&str>,
    resolution: &MetricHistoryResolution,
) -> Result<MetricHistoryCoverage, HistoryError> {
    let resolution = rollup_resolution_name(resolution)?;
    // Without complete schedule-version provenance, expected/gap remain null.
    // MAX per bucket is a conservative de-duplication of the eight family
    // availability series; it never presents this count as complete coverage.
    let observed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bucket_observed), 0) FROM (
            SELECT bucket_start_epoch_ms, MAX(observed_count) AS bucket_observed
            FROM metric_rollups
            WHERE host_id = ? AND resolution = ?
              AND bucket_end_epoch_ms > ? AND bucket_start_epoch_ms < ?
              AND subject_kind = 'host' AND metric_name = 'availability'
              AND (? IS NULL OR family = ?)
            GROUP BY bucket_start_epoch_ms
         )",
    )
    .bind(host_id)
    .bind(resolution)
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(family)
    .bind(family)
    .fetch_one(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    Ok(MetricHistoryCoverage {
        expected_count: None,
        observed_count: non_negative_u32(observed),
        gap_count: None,
        coverage: None,
        provenance_complete: false,
    })
}

async fn query_rollup_latest_observation(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    filters: &EffectiveHistoryFilters,
    resolution: &MetricHistoryResolution,
) -> Result<Option<String>, HistoryError> {
    let resolution = rollup_resolution_name(resolution)?;
    sqlx::query_scalar(
        "SELECT last_sample_at FROM metric_rollups
         WHERE host_id = ? AND resolution = ?
           AND bucket_end_epoch_ms > ? AND bucket_start_epoch_ms < ?
           AND (? IS NULL OR family = ?)
           AND subject_kind = ?
           AND (? IS NULL OR subject_id = ?)
           AND (? IS NULL OR metric_name = ?)
           AND (? IS NULL OR sample_kind = ?)
         ORDER BY last_sample_at_epoch_ms DESC, rollup_id DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(resolution)
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(filters.family.as_deref())
    .bind(filters.family.as_deref())
    .bind(&filters.subject_kind)
    .bind(filters.subject_id.as_deref())
    .bind(filters.subject_id.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(filters.sample_kind.as_deref())
    .fetch_optional(&state.pool)
    .await
    .map_err(HistoryError::Storage)
}

#[allow(clippy::too_many_arguments)]
async fn query_rollup_freshness(
    state: &AppState,
    host_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    evaluated_at: DateTime<Utc>,
    filters: &EffectiveHistoryFilters,
    resolution: &MetricHistoryResolution,
) -> Result<(Option<String>, Option<String>, MonitorFreshness), HistoryError> {
    let resolution = rollup_resolution_name(resolution)?;
    let row = sqlx::query(
        "SELECT rollups.last_valid_sample_at, runs.stale_after_seconds
         FROM metric_rollups rollups
         JOIN monitor_runs runs ON runs.run_id = rollups.last_valid_run_id
         WHERE rollups.host_id = ? AND rollups.resolution = ?
           AND rollups.bucket_end_epoch_ms > ? AND rollups.bucket_start_epoch_ms < ?
           AND rollups.last_valid_sample_at IS NOT NULL
           AND (? IS NULL OR rollups.family = ?)
           AND rollups.subject_kind = ?
           AND (? IS NULL OR rollups.subject_id = ?)
           AND (? IS NULL OR rollups.metric_name = ?)
           AND (? IS NULL OR rollups.sample_kind = ?)
         ORDER BY rollups.last_valid_sample_at_epoch_ms DESC, rollups.rollup_id DESC
         LIMIT 1",
    )
    .bind(host_id)
    .bind(resolution)
    .bind(from.timestamp_millis())
    .bind(to.timestamp_millis())
    .bind(filters.family.as_deref())
    .bind(filters.family.as_deref())
    .bind(&filters.subject_kind)
    .bind(filters.subject_id.as_deref())
    .bind(filters.subject_id.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.metric_name.as_deref())
    .bind(filters.sample_kind.as_deref())
    .bind(filters.sample_kind.as_deref())
    .fetch_optional(&state.pool)
    .await
    .map_err(HistoryError::Storage)?;
    let Some(row) = row else {
        return Ok((None, None, MonitorFreshness::Unknown));
    };
    let observed_at: String = row
        .try_get("last_valid_sample_at")
        .map_err(HistoryError::Storage)?;
    let stale_after: i64 = row
        .try_get("stale_after_seconds")
        .map_err(HistoryError::Storage)?;
    let observed = DateTime::parse_from_rfc3339(&observed_at)
        .map_err(|_| HistoryError::Internal)?
        .with_timezone(&Utc);
    let valid_until = observed + Duration::seconds(stale_after.max(60));
    let freshness = if valid_until >= evaluated_at {
        MonitorFreshness::Fresh
    } else {
        MonitorFreshness::Stale
    };
    Ok((Some(observed_at), Some(timestamp(valid_until)), freshness))
}

fn rollup_resolution_name(
    resolution: &MetricHistoryResolution,
) -> Result<&'static str, HistoryError> {
    match resolution {
        MetricHistoryResolution::Hour => Ok("hour"),
        MetricHistoryResolution::Day => Ok("day"),
        MetricHistoryResolution::Raw => Err(HistoryError::Internal),
    }
}

fn family_name(value: &MetricHistoryFamily) -> &'static str {
    match value {
        MetricHistoryFamily::Cpu => "cpu",
        MetricHistoryFamily::Memory => "memory",
        MetricHistoryFamily::Load => "load",
        MetricHistoryFamily::DiskCapacity => "disk_capacity",
        MetricHistoryFamily::DiskIo => "disk_io",
        MetricHistoryFamily::Network => "network",
        MetricHistoryFamily::Uptime => "uptime",
        MetricHistoryFamily::Process => "process",
    }
}

fn sample_kind_name(value: &MetricHistorySampleKind) -> &'static str {
    match value {
        MetricHistorySampleKind::Gauge => "gauge",
        MetricHistorySampleKind::Counter => "counter",
        MetricHistorySampleKind::Derived => "derived",
    }
}

fn subject_kind_name(value: &MetricHistorySubjectKind) -> &'static str {
    match value {
        MetricHistorySubjectKind::Host => "host",
        MetricHistorySubjectKind::Cpu => "cpu",
        MetricHistorySubjectKind::Filesystem => "filesystem",
        MetricHistorySubjectKind::BlockDevice => "block_device",
        MetricHistorySubjectKind::Interface => "interface",
        MetricHistorySubjectKind::Process => "process",
    }
}

fn parse_family(value: &str) -> Result<MetricHistoryFamily, HistoryError> {
    match value {
        "cpu" => Ok(MetricHistoryFamily::Cpu),
        "memory" => Ok(MetricHistoryFamily::Memory),
        "load" => Ok(MetricHistoryFamily::Load),
        "disk_capacity" => Ok(MetricHistoryFamily::DiskCapacity),
        "disk_io" => Ok(MetricHistoryFamily::DiskIo),
        "network" => Ok(MetricHistoryFamily::Network),
        "uptime" => Ok(MetricHistoryFamily::Uptime),
        "process" => Ok(MetricHistoryFamily::Process),
        _ => Err(HistoryError::Internal),
    }
}

fn parse_sample_kind(value: &str) -> Result<MetricHistorySampleKind, HistoryError> {
    match value {
        "gauge" => Ok(MetricHistorySampleKind::Gauge),
        "counter" => Ok(MetricHistorySampleKind::Counter),
        "derived" => Ok(MetricHistorySampleKind::Derived),
        _ => Err(HistoryError::Internal),
    }
}

fn parse_subject_kind(value: &str) -> Result<MetricHistorySubjectKind, HistoryError> {
    match value {
        "host" => Ok(MetricHistorySubjectKind::Host),
        "cpu" => Ok(MetricHistorySubjectKind::Cpu),
        "filesystem" => Ok(MetricHistorySubjectKind::Filesystem),
        "block_device" => Ok(MetricHistorySubjectKind::BlockDevice),
        "interface" => Ok(MetricHistorySubjectKind::Interface),
        "process" => Ok(MetricHistorySubjectKind::Process),
        _ => Err(HistoryError::Internal),
    }
}

fn parse_source_kind(value: &str) -> Result<MetricHistorySourceKind, HistoryError> {
    match value {
        HISTORY_SOURCE_KIND => Ok(MetricHistorySourceKind::SshHostResourceV1),
        _ => Err(HistoryError::Internal),
    }
}

fn parse_metric_quality(value: &str) -> Result<MonitorMetricQuality, HistoryError> {
    match value {
        "observed" => Ok(MonitorMetricQuality::Observed),
        "unsupported" => Ok(MonitorMetricQuality::Unsupported),
        "parse_failed" => Ok(MonitorMetricQuality::ParseFailed),
        "counter_reset" => Ok(MonitorMetricQuality::CounterReset),
        "counter_unreliable" => Ok(MonitorMetricQuality::CounterUnreliable),
        "insufficient_interval" => Ok(MonitorMetricQuality::InsufficientInterval),
        "permission_denied" => Ok(MonitorMetricQuality::PermissionDenied),
        "timed_out" => Ok(MonitorMetricQuality::TimedOut),
        _ => Err(HistoryError::Internal),
    }
}

fn non_negative_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn optional_non_negative_u32(value: Option<i64>) -> Option<u32> {
    value.map(non_negative_u32)
}

fn bad_query(code: &'static str, message: &'static str, details: Value) -> HistoryError {
    HistoryError::BadRequest {
        code,
        message,
        details,
    }
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_REQUEST_ID
                && value.bytes().all(|byte| !byte.is_ascii_control())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn history_meta(request_id: String) -> ApiMeta {
    ApiMeta {
        request_id,
        revision: 1,
        generated_at: timestamp(Utc::now()),
        freshness: Freshness::Fresh,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Real,
            status: DataSourceStatus::Fresh,
            label: "SQLite · HOST 指标历史".to_owned(),
        },
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod sample_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::monitoring::{HostResourceCapture, RawSource, parse_host_resource_v1};

    fn complete_capture() -> HostResourceCapture {
        let mut capture = HostResourceCapture::default();
        capture.frame_a.boot_uptime = RawSource::output("boot-a\n100.0 0.0\n");
        capture.frame_b.boot_uptime = RawSource::output("boot-a\n101.0 0.0\n");
        capture.frame_a.cpu = RawSource::output(
            "cpu 100 10 50 800 20 10 5 5 0 0\n\
             cpu0 100 10 50 800 20 10 5 5 0 0\n",
        );
        capture.frame_b.cpu = RawSource::output(
            "cpu 150 10 70 850 30 10 10 10 0 0\n\
             cpu0 150 10 70 850 30 10 10 10 0 0\n",
        );
        capture.cpu_online = RawSource::output("0\n");
        capture.memory = RawSource::output(
            "MemTotal: 1000000 kB\n\
             MemAvailable: 400000 kB\n\
             SwapTotal: 200000 kB\n\
             SwapFree: 150000 kB\n\
             Cached: 120000 kB\n\
             Buffers: 10000 kB\n\
             Slab: 30000 kB\n",
        );
        capture.load = RawSource::output("0.50 1.00 1.50 2/100 1234\n");
        capture.disk_capacity = RawSource::output(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
             credential-like-source 1000 400 500 45% /\n",
        );
        capture.disk_inodes = RawSource::output(
            "Filesystem Inodes IUsed IFree IUse% Mounted on\n\
             credential-like-source 1000 400 500 45% /\n",
        );
        capture.frame_a.disk_io = RawSource::output("8 0 sda 100 0 200 50 300 0 400 60 0 70 80\n");
        capture.frame_b.disk_io = RawSource::output("8 0 sda 110 0 220 55 305 0 430 70 0 90 110\n");
        capture.frame_a.network = RawSource::output(
            "Inter-| Receive | Transmit\n\
             face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
             eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0\n",
        );
        capture.frame_b.network = RawSource::output(
            "Inter-| Receive | Transmit\n\
             face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
             eth0: 3000 30 1 0 0 0 0 0 5000 50 0 1 0 0 0 0\n",
        );
        capture.frame_a.network_identity = RawSource::output("eth0\t2\t2\tup\t1000\n");
        capture.frame_b.network_identity = RawSource::output("eth0\t2\t2\tup\t1000\n");
        capture.process_summary =
            RawSource::output("scanned=120 running=3 blocked=2 zombie=1 raced=4 truncated=0\n");
        capture
    }

    #[test]
    fn raw_samples_are_typed_unique_and_do_not_persist_filesystem_sources() {
        let observation = parse_host_resource_v1(&complete_capture());
        let samples = samples_from_observation("host-history", &observation);
        let natural_keys = samples
            .iter()
            .map(|sample| {
                (
                    &sample.family,
                    &sample.subject_kind,
                    &sample.subject_id,
                    &sample.metric_name,
                    &sample.dimensions_sha256,
                    &sample.sample_kind,
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(natural_keys.len(), samples.len());

        let host_availability = samples
            .iter()
            .filter(|sample| sample.subject_kind == "host" && sample.metric_name == "availability")
            .collect::<Vec<_>>();
        assert_eq!(host_availability.len(), 8);
        assert!(host_availability.iter().all(|sample| {
            sample.quality == "observed"
                && sample.value_real == Some(1.0)
                && sample.value_integer.is_none()
        }));
        assert!(samples.iter().all(|sample| {
            (sample.quality == "observed")
                == (sample.value_real.is_some() ^ sample.value_integer.is_some())
                && sample.value_real.is_none_or(|value| value.is_finite())
                && sample
                    .window_seconds
                    .is_none_or(|value| value.is_finite() && value > 0.0)
        }));

        let aggregate_cpu_counters = samples
            .iter()
            .filter(|sample| {
                sample.family == "cpu"
                    && sample.subject_kind == "host"
                    && sample.sample_kind == "counter"
            })
            .collect::<Vec<_>>();
        assert_eq!(aggregate_cpu_counters.len(), 8);
        assert!(aggregate_cpu_counters.iter().all(|sample| {
            sample.value_real.is_none()
                && sample.value_integer.is_some()
                && sample.quality == "observed"
        }));
        assert_eq!(
            aggregate_cpu_counters
                .iter()
                .find(|sample| sample.metric_name == "user_ticks")
                .and_then(|sample| sample.value_integer),
            Some(150)
        );
        assert_eq!(
            samples
                .iter()
                .find(|sample| {
                    sample.family == "disk_io"
                        && sample.metric_name == "weighted_io_time_ms"
                        && sample.sample_kind == "counter"
                })
                .and_then(|sample| sample.value_integer),
            Some(110)
        );
        assert_eq!(
            samples
                .iter()
                .find(|sample| {
                    sample.family == "network"
                        && sample.metric_name == "tx_bytes"
                        && sample.sample_kind == "counter"
                })
                .and_then(|sample| sample.value_integer),
            Some(5_000)
        );

        let filesystem = samples
            .iter()
            .find(|sample| sample.subject_kind == "filesystem")
            .expect("filesystem sample");
        assert_eq!(filesystem.dimensions_json, r#"{"mount":"/"}"#);
        assert!(
            !filesystem
                .dimensions_json
                .contains("credential-like-source")
        );
        assert!(!filesystem.subject_id.contains("credential-like-source"));
    }

    #[test]
    fn interface_state_changes_do_not_split_numeric_series() {
        let original = parse_host_resource_v1(&complete_capture());
        let mut changed = original.clone();
        changed.network.value.as_mut().expect("network members")[0].operstate = "down".to_owned();
        let dimensions = |observation| {
            samples_from_observation("host-history", observation)
                .into_iter()
                .filter(|sample| sample.family == "network")
                .map(|sample| (sample.metric_name, sample.dimensions_sha256))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(dimensions(&original), dimensions(&changed));
    }

    #[test]
    fn invalid_counter_windows_become_null_quality_points_instead_of_bad_rows() {
        let mut capture = complete_capture();
        capture.frame_b.boot_uptime = RawSource::output("boot-a\n100.0 0.0\n");
        let observation = parse_host_resource_v1(&capture);
        let samples = samples_from_observation("host-history", &observation);
        let unavailable = samples
            .iter()
            .filter(|sample| {
                matches!(sample.family.as_str(), "cpu" | "disk_io" | "network")
                    && sample.quality != "observed"
            })
            .collect::<Vec<_>>();
        assert!(!unavailable.is_empty());
        assert!(unavailable.iter().all(|sample| {
            sample.value_real.is_none()
                && sample.value_integer.is_none()
                && sample.window_seconds.is_none()
        }));
        let retained_counters = samples
            .iter()
            .filter(|sample| sample.sample_kind == "counter")
            .collect::<Vec<_>>();
        assert_eq!(retained_counters.len(), 32);
        assert!(retained_counters.iter().all(|sample| {
            sample.quality == "observed"
                && sample.value_real.is_none()
                && sample.value_integer.is_some()
        }));
    }

    #[test]
    fn reboot_resets_rates_but_retains_frame_b_counter_baselines() {
        let mut capture = complete_capture();
        capture.frame_b.boot_uptime = RawSource::output("boot-b\n1.0 0.0\n");
        let observation = parse_host_resource_v1(&capture);
        assert_eq!(observation.cpu.quality, MetricQuality::CounterReset);
        assert_eq!(observation.disk_io.quality, MetricQuality::CounterReset);
        assert_eq!(observation.network.quality, MetricQuality::CounterReset);

        let samples = samples_from_observation("host-history", &observation);
        let retained_counters = samples
            .iter()
            .filter(|sample| sample.sample_kind == "counter")
            .collect::<Vec<_>>();
        assert_eq!(retained_counters.len(), 32);
        assert!(retained_counters.iter().all(|sample| {
            sample.quality == "observed"
                && sample.value_real.is_none()
                && sample.value_integer.is_some()
        }));
        assert!(samples.iter().any(|sample| {
            sample.family == "disk_io"
                && sample.subject_id == "boot-b:8:0"
                && sample.metric_name == "reads_completed"
                && sample.value_integer == Some(110)
        }));
        assert!(samples.iter().any(|sample| {
            sample.family == "network"
                && sample.subject_id == "boot-b:2"
                && sample.metric_name == "rx_bytes"
                && sample.value_integer == Some(3_000)
        }));
    }

    #[test]
    fn counters_above_sqlite_integer_range_become_failed_null_points() {
        let mut capture = complete_capture();
        capture.frame_b.disk_io = RawSource::output(format!(
            "8 0 sda {} 0 220 55 305 0 430 70 0 90 110\n",
            u64::MAX
        ));
        let observation = parse_host_resource_v1(&capture);
        let samples = samples_from_observation("host-history", &observation);
        let overflow = samples
            .iter()
            .find(|sample| {
                sample.family == "disk_io"
                    && sample.subject_kind == "block_device"
                    && sample.metric_name == "reads_completed"
                    && sample.sample_kind == "counter"
            })
            .expect("overflow counter quality point");
        assert_eq!(overflow.quality, "counter_unreliable");
        assert!(overflow.value_real.is_none());
        assert!(overflow.value_integer.is_none());
    }

    #[test]
    fn cpu_counters_are_not_observed_without_the_run_boot_identity() {
        let mut capture = complete_capture();
        capture.frame_b.boot_uptime = RawSource::Unavailable(MetricQuality::TimedOut);
        let observation = parse_host_resource_v1(&capture);
        let counters = samples_from_observation("host-history", &observation)
            .into_iter()
            .filter(|sample| sample.family == "cpu" && sample.sample_kind == "counter")
            .collect::<Vec<_>>();

        assert_eq!(counters.len(), 16);
        assert!(counters.iter().all(|sample| {
            sample.quality == "timed_out"
                && sample.value_real.is_none()
                && sample.value_integer.is_none()
        }));
    }

    #[test]
    fn one_unidentified_interface_does_not_erase_other_counter_baselines() {
        let mut capture = complete_capture();
        capture.frame_b.network = RawSource::output(
            "Inter-| Receive | Transmit\n\
             face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
             eth0: 3000 30 1 0 0 0 0 0 5000 50 0 1 0 0 0 0\n\
             raced0: 7000 70 0 0 0 0 0 0 9000 90 0 0 0 0 0 0\n",
        );
        let observation = parse_host_resource_v1(&capture);
        let counters = samples_from_observation("host-history", &observation)
            .into_iter()
            .filter(|sample| sample.family == "network" && sample.sample_kind == "counter")
            .collect::<Vec<_>>();

        assert_eq!(
            counters.len(),
            8,
            "the identified eth0 baseline remains complete"
        );
        assert!(counters.iter().all(|sample| {
            sample.subject_id == "boot-a:2"
                && sample.quality == "observed"
                && sample.value_integer.is_some()
        }));
        assert!(
            counters
                .iter()
                .all(|sample| !sample.dimensions_json.contains("raced0"))
        );
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::{Path, Query, State};
    use chrono::{Duration, TimeZone, Utc};

    use super::*;
    use crate::{
        monitoring::{
            BlockIoRate, CollectionCoverage, FamilyObservation, FilesystemCapacity,
            HOST_RESOURCE_PROFILE_ID, HOST_RESOURCE_PROTOCOL_VERSION, HostResourceObservation,
            InterfaceRate, MetricQuality, RunCompleteness,
        },
        monitoring_api, storage,
    };

    fn unavailable<T>() -> FamilyObservation<T> {
        FamilyObservation {
            quality: MetricQuality::Unsupported,
            value: None,
        }
    }

    fn fixture_observation(
        completeness: RunCompleteness,
        filesystem_source: &str,
    ) -> HostResourceObservation {
        HostResourceObservation {
            profile: HOST_RESOURCE_PROFILE_ID,
            protocol_version: HOST_RESOURCE_PROTOCOL_VERSION,
            cpu: unavailable(),
            cpu_counters: unavailable(),
            memory: unavailable(),
            load: unavailable(),
            disk_capacity: FamilyObservation {
                quality: MetricQuality::Observed,
                value: Some(vec![FilesystemCapacity {
                    source: filesystem_source.to_owned(),
                    mount: "/srv/data".to_owned(),
                    size_kib: 1_000,
                    used_kib: 400,
                    available_kib: 600,
                    allocatable_used_ratio: Some(0.4),
                    inode_total: Some(100),
                    inode_used: Some(20),
                    inode_available: Some(80),
                    inode_used_ratio: Some(0.2),
                    inode_quality: MetricQuality::Observed,
                }]),
            },
            disk_io: FamilyObservation {
                quality: MetricQuality::CounterReset,
                value: Some(vec![BlockIoRate {
                    identity: "8:0".to_owned(),
                    name: "sda".to_owned(),
                    major: 8,
                    minor: 0,
                    quality: MetricQuality::CounterReset,
                    window_seconds: 0.0,
                    metrics: None,
                }]),
            },
            disk_io_counters: unavailable(),
            network: FamilyObservation {
                quality: MetricQuality::InsufficientInterval,
                value: Some(vec![InterfaceRate {
                    identity: "if:2:2".to_owned(),
                    name: "eth0".to_owned(),
                    ifindex: 2,
                    iflink: Some(2),
                    operstate: "down".to_owned(),
                    speed_mbps: Some(1_000),
                    quality: MetricQuality::InsufficientInterval,
                    window_seconds: 0.0,
                    metrics: None,
                }]),
            },
            network_counters: unavailable(),
            uptime: unavailable(),
            process: unavailable(),
            coverage: CollectionCoverage {
                required_observed: usize::from(completeness != RunCompleteness::Failed),
                required_total: 1,
                optional_observed: 0,
                optional_total: 3,
                completeness,
            },
        }
    }

    async fn seed_host(pool: &sqlx::SqlitePool, host_id: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO workspaces(workspace_id, owner_id, created_at)
             VALUES ('workspace-default', 'owner-local', '2026-08-15T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO hosts(
                host_id, workspace_id, display_name, address, port, ssh_user,
                credential_ref, host_key_state, transport, os, status, created_at
             ) VALUES (?, 'workspace-default', ?, 'fixture.invalid', 22, 'fixture',
                'secret-ref-fixture', 'verified', 'ssh', 'linux', 'connection_ready',
                '2026-08-15T00:00:00Z')",
        )
        .bind(host_id)
        .bind(host_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_run(pool: &sqlx::SqlitePool, run_id: &str, host_id: &str, state: &str) {
        sqlx::query(
            "INSERT INTO monitor_runs(
                run_id, host_id, request_id, idempotency_key, request_sha256,
                profile, trigger_kind, state, stale_after_seconds, submitted_at,
                accepted_response_json
             ) VALUES (?, ?, ?, ?, 'digest', 'host_resource_v1', 'manual', ?, 300,
                '2026-08-15T00:00:00Z', '{}')",
        )
        .bind(run_id)
        .bind(host_id)
        .bind(format!("request-{run_id}"))
        .bind(format!("key-{run_id}"))
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn observation_samples_are_atomic_family_scoped_and_secret_free() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        seed_host(&pool, "host-history").await;
        seed_run(&pool, "run-history-good", "host-history", "running").await;
        let secret_source = "//account:password@files.example/share";
        monitoring_api::persist_observation(
            &pool,
            "run-history-good",
            "host-history",
            128,
            fixture_observation(RunCompleteness::Succeeded, secret_source),
        )
        .await
        .unwrap();

        let sample_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_samples WHERE run_id = 'run-history-good'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            sample_count > 8,
            "all eight family availability rows must coexist"
        );
        let dimensions: String = sqlx::query_scalar(
            "SELECT GROUP_CONCAT(dimensions_json, '') FROM metric_samples
             WHERE run_id = 'run-history-good'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!dimensions.contains("account"));
        assert!(!dimensions.contains("password"));
        assert!(!dimensions.contains("files.example"));
        assert!(dimensions.contains("/srv/data"));
        let invalid_windows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_samples
             WHERE window_seconds IS NOT NULL AND window_seconds <= 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(invalid_windows, 0);
        let current_run: String = sqlx::query_scalar(
            "SELECT run_id FROM monitoring_current WHERE host_id = 'host-history'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current_run, "run-history-good");

        let before = sample_count;
        assert!(
            monitoring_api::persist_observation(
                &pool,
                "run-history-good",
                "host-history",
                128,
                fixture_observation(RunCompleteness::Succeeded, secret_source),
            )
            .await
            .is_err()
        );
        let after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_samples WHERE run_id = 'run-history-good'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            after, before,
            "duplicate completion must not append history"
        );

        seed_run(&pool, "run-history-failed", "host-history", "running").await;
        monitoring_api::persist_observation(
            &pool,
            "run-history-failed",
            "host-history",
            64,
            fixture_observation(RunCompleteness::Failed, secret_source),
        )
        .await
        .unwrap();
        let failed_samples: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_samples WHERE run_id = 'run-history-failed'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            failed_samples > 0,
            "truthful failed-family quality is historical data"
        );
        let current_run: String = sqlx::query_scalar(
            "SELECT run_id FROM monitoring_current WHERE host_id = 'host-history'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current_run, "run-history-good");
    }

    #[tokio::test]
    async fn sample_constraint_failure_rolls_back_terminal_run_and_current_pointer() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        seed_host(&pool, "host-history-atomic").await;
        seed_run(
            &pool,
            "run-history-atomic",
            "host-history-atomic",
            "running",
        )
        .await;
        sqlx::query(
            "CREATE TRIGGER reject_atomic_fixture_sample
             BEFORE INSERT ON metric_samples
             WHEN NEW.run_id = 'run-history-atomic'
             BEGIN
               SELECT RAISE(ABORT, 'fixture sample failure');
             END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = monitoring_api::persist_observation(
            &pool,
            "run-history-atomic",
            "host-history-atomic",
            64,
            fixture_observation(RunCompleteness::Succeeded, "/dev/fixture"),
        )
        .await;
        assert!(result.is_err());
        let state: String = sqlx::query_scalar(
            "SELECT state FROM monitor_runs WHERE run_id = 'run-history-atomic'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "running", "terminal run update must roll back");
        let samples: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_samples WHERE run_id = 'run-history-atomic'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(samples, 0);
        let current: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM monitoring_current WHERE host_id = 'host-history-atomic'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current, 0);
    }

    fn raw_sample(family: &str, metric_name: &str, value: f64) -> MetricSampleInput {
        MetricSampleInput {
            family: family.to_owned(),
            subject_kind: "host".to_owned(),
            subject_id: "host-query".to_owned(),
            metric_name: metric_name.to_owned(),
            dimensions_json: "{}".to_owned(),
            dimensions_sha256: hex_digest(&Sha256::digest(b"{}")),
            sample_kind: "gauge".to_owned(),
            value_real: Some(value),
            value_integer: None,
            unit: "count".to_owned(),
            window_seconds: None,
            quality: "observed".to_owned(),
        }
    }

    async fn insert_fixture_sample(
        pool: &sqlx::SqlitePool,
        run_id: &str,
        observed_at: DateTime<Utc>,
        sample: MetricSampleInput,
    ) {
        seed_run(pool, run_id, "host-query", "succeeded").await;
        let mut tx = pool.begin().await.unwrap();
        insert_samples(
            &mut tx,
            run_id,
            "host-query",
            &timestamp(observed_at),
            observed_at.timestamp_millis(),
            &[sample],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn raw_query_uses_epoch_boundaries_family_freshness_and_unknown_gap() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        seed_host(&pool, "host-query").await;
        let now = Utc::now();
        sqlx::query(
            "UPDATE monitoring_history_metadata SET history_started_at = ? WHERE singleton_id = 1",
        )
        .bind(timestamp(now - Duration::hours(2)))
        .execute(&pool)
        .await
        .unwrap();
        let same_second_old = now - Duration::minutes(20);
        let same_second_new = same_second_old + Duration::milliseconds(750);
        insert_fixture_sample(
            &pool,
            "run-query-old",
            same_second_old,
            raw_sample("network", "rx_bytes_per_second", 1.0),
        )
        .await;
        insert_fixture_sample(
            &pool,
            "run-query-new",
            same_second_new,
            raw_sample("network", "rx_bytes_per_second", 2.0),
        )
        .await;
        insert_fixture_sample(
            &pool,
            "run-query-cpu",
            now - Duration::seconds(5),
            raw_sample("cpu", "online_cpu_count", 8.0),
        )
        .await;

        let state = AppState::new(pool);
        let response = get_host_metrics(
            State(state),
            HeaderMap::new(),
            Path("host-query".to_owned()),
            Ok(Query(MetricHistoryQuery {
                from: timestamp(same_second_old + Duration::milliseconds(500)),
                to: timestamp(now),
                resolution: MetricHistoryRequestedResolution::Raw,
                family: Some(MetricHistoryFamily::Network),
                subject_kind: None,
                subject_id: None,
                metric_name: None,
                sample_kind: None,
                after_epoch_ms: None,
                after_sample_id: None,
                limit: None,
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            response.data.actual_resolution,
            MetricHistoryResolution::Raw
        );
        assert_eq!(response.data.series.len(), 1);
        assert_eq!(response.data.series[0].points.len(), 1);
        assert_eq!(response.data.series[0].points[0].value, Some(2.0));
        assert_eq!(response.data.freshness, MonitorFreshness::Stale);
        assert_eq!(response.data.coverage.expected_count, None);
        assert_eq!(response.data.coverage.gap_count, None);
        assert!(!response.data.coverage.provenance_complete);
    }

    #[tokio::test]
    async fn query_validation_is_bounded_and_empty_rollup_queries_are_explicit() {
        let pool = storage::connect("sqlite::memory:").await.unwrap();
        seed_host(&pool, "host-query").await;
        let state = AppState::new(pool);
        let now = Utc::now();
        let bad_range = get_host_metrics(
            State(state.clone()),
            HeaderMap::new(),
            Path("host-query".to_owned()),
            Ok(Query(MetricHistoryQuery {
                from: timestamp(now),
                to: timestamp(now),
                resolution: MetricHistoryRequestedResolution::Raw,
                family: None,
                subject_kind: None,
                subject_id: None,
                metric_name: None,
                sample_kind: None,
                after_epoch_ms: None,
                after_sample_id: None,
                limit: None,
            })),
        )
        .await;
        assert!(matches!(
            bad_range,
            Err(HistoryError::BadRequest {
                code: "INVALID_TIME_RANGE",
                ..
            })
        ));

        let rollup = get_host_metrics(
            State(state),
            HeaderMap::new(),
            Path("host-query".to_owned()),
            Ok(Query(MetricHistoryQuery {
                from: timestamp(now - Duration::hours(1)),
                to: timestamp(now),
                resolution: MetricHistoryRequestedResolution::Hour,
                family: None,
                subject_kind: None,
                subject_id: None,
                metric_name: None,
                sample_kind: None,
                after_epoch_ms: None,
                after_sample_id: None,
                limit: None,
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(rollup.data.actual_resolution, MetricHistoryResolution::Hour);
        assert_eq!(rollup.data.retention_tier, "hour");
        assert!(rollup.data.series.is_empty());
    }

    #[test]
    fn auto_resolution_obeys_enabled_retention_horizons() {
        let evaluated_at = Utc
            .with_ymd_and_hms(2026, 8, 15, 12, 0, 0)
            .single()
            .unwrap();
        let short_to = evaluated_at - Duration::days(10);
        let short_from = short_to - Duration::hours(3);

        assert_eq!(
            choose_resolution(
                &MetricHistoryRequestedResolution::Auto,
                short_from,
                short_to,
                evaluated_at,
                RollupSettings::default(),
            ),
            MetricHistoryResolution::Raw,
            "disabled cleanup keeps span-first auto behavior",
        );

        let enabled_hour = RollupSettings {
            retention_enabled: true,
            raw_observed_retention_days: 7,
            raw_non_observed_retention_days: 30,
            hour_retention_days: 90,
            day_retention_days: 365,
            ..RollupSettings::default()
        };
        assert_eq!(
            choose_resolution(
                &MetricHistoryRequestedResolution::Auto,
                short_from,
                short_to,
                evaluated_at,
                enabled_hour,
            ),
            MetricHistoryResolution::Hour,
        );

        let enabled_day = RollupSettings {
            retention_enabled: true,
            raw_observed_retention_days: 1,
            raw_non_observed_retention_days: 2,
            hour_retention_days: 3,
            day_retention_days: 365,
            ..RollupSettings::default()
        };
        assert_eq!(
            choose_resolution(
                &MetricHistoryRequestedResolution::Auto,
                short_from,
                short_to,
                evaluated_at,
                enabled_day,
            ),
            MetricHistoryResolution::Day,
        );
    }
}
