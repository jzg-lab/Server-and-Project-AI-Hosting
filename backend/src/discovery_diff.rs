use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::{
    api::AppState,
    contracts::{
        ApiErrorResponse, DiscoveryChangeKind, DiscoveryDiffCounts, DiscoveryDiffData,
        DiscoveryDiffItem, DiscoveryDiffResponse, DiscoveryEvidence, EvidenceItem, Freshness,
        GraphNode, GraphSnapshot, ProjectionState,
    },
    model_provider::{M3Error, m3_meta, request_id},
};

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn enum_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn digest(value: impl AsRef<[u8]>) -> String {
    Sha256::digest(value.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn evidence_key(item: &EvidenceItem) -> String {
    format!("{}:{}", enum_string(&item.kind), item.external_id)
}

fn evidence_digest(item: &EvidenceItem) -> String {
    digest(
        serde_json::to_vec(&json!({
            "kind": item.kind,
            "external_id": item.external_id,
            "sha256": item.sha256,
            "metadata": item.metadata
        }))
        .unwrap_or_default(),
    )
}

fn evidence_map(evidence: &DiscoveryEvidence) -> BTreeMap<String, EvidenceItem> {
    evidence
        .items()
        .map(|item| (evidence_key(item), item.clone()))
        .collect()
}

fn matching_node<'a>(snapshot: &'a GraphSnapshot, external_id: &str) -> Option<&'a GraphNode> {
    let evidence_ref = format!("evidence:{external_id}");
    snapshot
        .nodes
        .iter()
        .find(|node| node.source_refs.iter().any(|value| value == &evidence_ref))
}

fn confirmed_conflict(
    item: &EvidenceItem,
    deterministic: &GraphSnapshot,
    confirmed: Option<&GraphSnapshot>,
) -> Option<&'static str> {
    let current = matching_node(deterministic, &item.external_id)?;
    if current.state == ProjectionState::Archived {
        return Some("已忽略对象再次出现在扫描事实中");
    }
    let confirmed = confirmed?.nodes.iter().find(|node| node.id == current.id)?;
    if confirmed.state == ProjectionState::Archived {
        return Some("已归档对象再次出现在扫描事实中");
    }
    if confirmed.label != current.label {
        return Some("外部事实名称与用户确认名称不同");
    }
    if confirmed.project_id != current.project_id {
        return Some("确定性归类与用户确认归类不同");
    }
    None
}

fn increment(counts: &mut DiscoveryDiffCounts, change: &DiscoveryChangeKind) {
    match change {
        DiscoveryChangeKind::Added => counts.added += 1,
        DiscoveryChangeKind::Changed => counts.changed += 1,
        DiscoveryChangeKind::Missing => counts.missing += 1,
        DiscoveryChangeKind::Conflict => counts.conflict += 1,
        DiscoveryChangeKind::Unchanged => counts.unchanged += 1,
    }
}

pub(crate) async fn create_discovery_diff_in(
    connection: &mut SqliteConnection,
    run_id: &str,
    host_id: &str,
    evidence: &DiscoveryEvidence,
    deterministic: &GraphSnapshot,
    confirmed: Option<&GraphSnapshot>,
) -> Result<String, M3Error> {
    if let Some(row) = sqlx::query("SELECT diff_id FROM discovery_diffs WHERE run_id = ?")
        .bind(run_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(M3Error::Storage)?
    {
        return row.try_get("diff_id").map_err(M3Error::Storage);
    }

    let previous = sqlx::query(
        "SELECT run_id, evidence_json FROM discovery_runs
         WHERE host_id = ? AND run_id <> ?
           AND state IN ('evidence_ready', 'discovery_complete', 'discovery_partial')
           AND evidence_json IS NOT NULL
         ORDER BY finished_at DESC, submitted_at DESC, rowid DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(run_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(M3Error::Storage)?;
    let (previous_run_id, previous_map) = if let Some(row) = previous {
        let previous_run_id: String = row.try_get("run_id").map_err(M3Error::Storage)?;
        let payload: String = row.try_get("evidence_json").map_err(M3Error::Storage)?;
        let previous_evidence: DiscoveryEvidence =
            serde_json::from_str(&payload).map_err(|_| M3Error::Internal)?;
        (Some(previous_run_id), evidence_map(&previous_evidence))
    } else {
        (None, BTreeMap::new())
    };
    let current_map = evidence_map(evidence);
    let mut keys = previous_map
        .keys()
        .chain(current_map.keys())
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();

    let mut counts = DiscoveryDiffCounts::default();
    let mut items = Vec::with_capacity(keys.len());
    for key in keys {
        let previous = previous_map.get(&key);
        let current = current_map.get(&key);
        let previous_sha256 = previous.map(evidence_digest);
        let current_sha256 = current.map(evidence_digest);
        let conflict = current.and_then(|item| confirmed_conflict(item, deterministic, confirmed));
        let (change, summary, representative) = match (previous, current, conflict) {
            (_, Some(current), Some(summary)) => {
                (DiscoveryChangeKind::Conflict, summary.to_owned(), current)
            }
            (None, Some(current), None) => (
                DiscoveryChangeKind::Added,
                "本次扫描新增事实".to_owned(),
                current,
            ),
            (Some(previous), None, _) => (
                DiscoveryChangeKind::Missing,
                "此前事实本次未再观察到；本地投影未删除".to_owned(),
                previous,
            ),
            (Some(_), Some(current), None) => {
                if previous_sha256 == current_sha256 {
                    (
                        DiscoveryChangeKind::Unchanged,
                        "事实未变化".to_owned(),
                        current,
                    )
                } else {
                    (
                        DiscoveryChangeKind::Changed,
                        "结构化事实发生变化".to_owned(),
                        current,
                    )
                }
            }
            (None, None, _) => continue,
        };
        increment(&mut counts, &change);
        let mut evidence_refs = Vec::new();
        if let Some(previous_run_id) = &previous_run_id {
            evidence_refs.push(format!("discovery:{previous_run_id}"));
        }
        if current.is_some() {
            evidence_refs.push(format!("discovery:{run_id}"));
        }
        evidence_refs.push(format!("evidence:{}", representative.external_id));
        items.push(DiscoveryDiffItem {
            entity_key: key,
            evidence_kind: representative.kind.clone(),
            external_id: representative.external_id.clone(),
            change,
            summary,
            previous_sha256,
            current_sha256,
            evidence_refs,
        });
    }

    let diff_id = format!("diff-{}", &digest(run_id)[..24]);
    let created_at = now();
    sqlx::query(
        "INSERT INTO discovery_diffs(
            diff_id, run_id, host_id, previous_run_id, counts_json, items_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&diff_id)
    .bind(run_id)
    .bind(host_id)
    .bind(&previous_run_id)
    .bind(serde_json::to_string(&counts).map_err(|_| M3Error::Internal)?)
    .bind(serde_json::to_string(&items).map_err(|_| M3Error::Internal)?)
    .bind(&created_at)
    .execute(&mut *connection)
    .await
    .map_err(M3Error::Storage)?;
    sqlx::query("UPDATE discovery_runs SET diff_id = ? WHERE run_id = ?")
        .bind(&diff_id)
        .bind(run_id)
        .execute(&mut *connection)
        .await
        .map_err(M3Error::Storage)?;
    Ok(diff_id)
}

async fn load_diff(pool: &SqlitePool, run_id: &str) -> Result<DiscoveryDiffData, M3Error> {
    let row = sqlx::query(
        "SELECT diff_id, run_id, host_id, previous_run_id, counts_json, items_json, created_at
         FROM discovery_diffs WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(M3Error::Storage)?
    .ok_or_else(|| M3Error::not_found("discovery_diff", run_id))?;
    let counts_json: String = row.try_get("counts_json").map_err(M3Error::Storage)?;
    let items_json: String = row.try_get("items_json").map_err(M3Error::Storage)?;
    Ok(DiscoveryDiffData {
        diff_id: row.try_get("diff_id").map_err(M3Error::Storage)?,
        run_id: row.try_get("run_id").map_err(M3Error::Storage)?,
        host_id: row.try_get("host_id").map_err(M3Error::Storage)?,
        previous_run_id: row.try_get("previous_run_id").map_err(M3Error::Storage)?,
        counts: serde_json::from_str(&counts_json).map_err(|_| M3Error::Internal)?,
        items: serde_json::from_str(&items_json).map_err(|_| M3Error::Internal)?,
        created_at: row.try_get("created_at").map_err(M3Error::Storage)?,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/discovery-runs/{run_id}/diff",
    tag = "m3",
    params(("run_id" = String, Path, description = "Discovery run identifier")),
    responses(
        (status = 200, body = DiscoveryDiffResponse),
        (status = 404, body = ApiErrorResponse)
    )
)]
pub async fn get_discovery_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<DiscoveryDiffResponse>, M3Error> {
    let data = load_diff(&state.pool, &run_id).await?;
    let mut meta = m3_meta(request_id(&headers), 1, Freshness::Fresh);
    meta.data_source.label = "SSH · 二次扫描差异".to_owned();
    Ok(Json(DiscoveryDiffResponse { data, meta }))
}
