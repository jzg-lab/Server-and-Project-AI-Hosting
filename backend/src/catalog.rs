use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};
use thiserror::Error;
use uuid::Uuid;

use crate::contracts::{
    DiscoveryEvidence, DiscoveryProviderCoverage, DiscoveryProviderStatus, EvidenceItem,
};

const CATALOG_PROVIDER_KINDS: [&str; 3] = ["compose", "docker", "systemd"];

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("catalog serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

struct ObservedDeployment<'a> {
    provider_kind: &'static str,
    item: &'a EvidenceItem,
}

#[derive(Clone)]
struct ProviderCoverage {
    status: DiscoveryProviderStatus,
    observed_at: String,
    evidence_refs: Vec<String>,
    metadata: Value,
}

struct ExistingDeployment {
    deployment_id: String,
    external_id: String,
}

/// Persists the catalog sidecar in the caller's discovery transaction.
///
/// Existing user-owned catalog state is preserved. A ready provider can prove a
/// previously seen deployment is missing; every other provider status records
/// an unknown observation instead of inferring removal.
pub async fn persist_discovery_success_in(
    connection: &mut SqliteConnection,
    run_id: &str,
    host_id: &str,
    evidence: &DiscoveryEvidence,
) -> Result<(), CatalogError> {
    let workspace_id: String =
        sqlx::query_scalar("SELECT workspace_id FROM hosts WHERE host_id = ?")
            .bind(host_id)
            .fetch_one(&mut *connection)
            .await?;
    let observed = observed_deployments(evidence);
    let observed_external_ids = observed_external_ids(&observed);
    let coverage = provider_coverage(evidence, &observed_external_ids)?;
    let persisted_at = now();

    for observed_deployment in observed.values() {
        let provider = coverage.get(observed_deployment.provider_kind);
        persist_observed_deployment(
            connection,
            run_id,
            host_id,
            &workspace_id,
            observed_deployment.provider_kind,
            observed_deployment.item,
            provider,
            &persisted_at,
            &evidence.finished_at,
        )
        .await?;
    }

    for (provider_kind, provider) in &coverage {
        let observed_ids = observed_external_ids
            .get(provider_kind.as_str())
            .cloned()
            .unwrap_or_default();
        for deployment in existing_deployments(connection, host_id, provider_kind).await? {
            if observed_ids.contains(&deployment.external_id) {
                continue;
            }
            if provider.status == DiscoveryProviderStatus::Ready {
                persist_missing_observation(
                    connection,
                    run_id,
                    &deployment,
                    provider_kind,
                    provider,
                    &persisted_at,
                )
                .await?;
            } else {
                persist_unknown_observation(
                    connection,
                    run_id,
                    &deployment,
                    provider_kind,
                    provider,
                    &persisted_at,
                )
                .await?;
            }
        }
    }

    persist_resource_evidence(
        connection,
        run_id,
        host_id,
        &workspace_id,
        evidence,
        &observed,
        &coverage,
        &persisted_at,
    )
    .await?;

    Ok(())
}

/// Materializes only resource relationships that are present in the observed
/// evidence. Names are used to match provider references within the same
/// discovery run; co-location alone never creates a relationship.
#[allow(clippy::too_many_arguments)]
async fn persist_resource_evidence(
    connection: &mut SqliteConnection,
    run_id: &str,
    host_id: &str,
    workspace_id: &str,
    evidence: &DiscoveryEvidence,
    observed_deployments: &BTreeMap<(String, String), ObservedDeployment<'_>>,
    coverage: &BTreeMap<String, ProviderCoverage>,
    persisted_at: &str,
) -> Result<(), CatalogError> {
    // Resource links are observations, not permanent ownership. A ready
    // provider gives us permission to mark its previous observed links stale;
    // unavailable providers must leave the last known relationship intact.
    for (resource_kind, provider_kind) in [
        ("network", "docker"),
        ("volume", "docker"),
        ("image", "docker"),
        ("document", "compose"),
    ] {
        if coverage
            .get(provider_kind)
            .is_some_and(|provider| provider.status == DiscoveryProviderStatus::Ready)
        {
            let relation_kind = format!("uses_{resource_kind}");
            sqlx::query(
                "UPDATE deployment_resource_links
                 SET state = 'stale', revision = revision + 1, updated_at = ?
                 WHERE relation_kind = ? AND origin = 'observed' AND state = 'observed'
                   AND deployment_id IN (
                       SELECT deployment_id FROM deployments WHERE host_id = ?
                   )",
            )
            .bind(persisted_at)
            .bind(relation_kind)
            .bind(host_id)
            .execute(&mut *connection)
            .await?;
        }
    }

    let resource_items = evidence
        .networks
        .iter()
        .map(|item| ("network", item))
        .chain(evidence.volumes.iter().map(|item| ("volume", item)))
        .chain(evidence.images.iter().map(|item| ("image", item)))
        .chain(
            evidence
                .document_candidates
                .iter()
                .map(|item| ("document", item)),
        )
        .collect::<Vec<_>>();

    for (resource_kind, item) in resource_items {
        let resource_id = Uuid::new_v4().to_string();
        let source = bounded_text(&item.source, 128);
        let external_id = bounded_text(&item.external_id, 512);
        let display_name = resource_display_name(item, &external_id);
        let freshness = freshness_name(&item.freshness);
        let metadata = item_metadata(item);
        let metadata_json = serde_json::to_string(&metadata)?;
        sqlx::query(
            "INSERT INTO resource_entities(
                resource_entity_id, workspace_id, resource_kind, source, external_id,
                display_name, freshness, metadata_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(workspace_id, source, external_id) DO UPDATE SET
                display_name = excluded.display_name,
                freshness = excluded.freshness,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at",
        )
        .bind(&resource_id)
        .bind(workspace_id)
        .bind(resource_kind)
        .bind(&source)
        .bind(&external_id)
        .bind(&display_name)
        .bind(freshness)
        .bind(&metadata_json)
        .bind(persisted_at)
        .bind(persisted_at)
        .execute(&mut *connection)
        .await?;

        let resource_entity_id: String = sqlx::query_scalar(
            "SELECT resource_entity_id FROM resource_entities
             WHERE workspace_id = ? AND source = ? AND external_id = ?",
        )
        .bind(workspace_id)
        .bind(&source)
        .bind(&external_id)
        .fetch_one(&mut *connection)
        .await?;

        for deployment in matching_resource_deployments(
            host_id,
            resource_kind,
            item,
            evidence,
            observed_deployments,
        ) {
            let deployment_id: String = sqlx::query_scalar(
                "SELECT deployment_id FROM deployments
                 WHERE host_id = ? AND provider_kind = ? AND external_id = ?",
            )
            .bind(host_id)
            .bind(deployment.provider_kind)
            .bind(&deployment.item.external_id)
            .fetch_one(&mut *connection)
            .await?;
            let link_id = Uuid::new_v4().to_string();
            let relation_kind = format!("uses_{resource_kind}");
            let source_refs = serde_json::to_string(&observed_evidence_refs(run_id, item))?;
            sqlx::query(
                "INSERT INTO deployment_resource_links(
                    deployment_resource_link_id, deployment_id, resource_entity_id,
                    relation_kind, state, origin, source_refs_json, observed_at,
                    revision, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, 'observed', 'observed', ?, ?, 1, ?, ?)
                  ON CONFLICT(deployment_id, resource_entity_id, relation_kind) DO UPDATE SET
                     state = CASE
                         WHEN deployment_resource_links.origin = 'user_declared'
                              OR deployment_resource_links.state IN ('confirmed', 'archived')
                         THEN deployment_resource_links.state
                         ELSE 'observed'
                     END,
                     origin = CASE
                         WHEN deployment_resource_links.origin = 'user_declared'
                         THEN deployment_resource_links.origin
                         ELSE 'observed'
                     END,
                     source_refs_json = CASE
                         WHEN deployment_resource_links.origin = 'user_declared'
                         THEN deployment_resource_links.source_refs_json
                         ELSE excluded.source_refs_json
                     END,
                     observed_at = excluded.observed_at, revision = deployment_resource_links.revision + 1,
                     updated_at = excluded.updated_at",
            )
            .bind(&link_id)
            .bind(&deployment_id)
            .bind(&resource_entity_id)
            .bind(&relation_kind)
            .bind(&source_refs)
            .bind(&item.observed_at)
            .bind(persisted_at)
            .bind(persisted_at)
            .execute(&mut *connection)
            .await?;
        }
    }

    Ok(())
}

fn matching_resource_deployments<'a>(
    host_id: &str,
    resource_kind: &str,
    resource: &EvidenceItem,
    evidence: &'a DiscoveryEvidence,
    observed_deployments: &'a BTreeMap<(String, String), ObservedDeployment<'a>>,
) -> Vec<&'a ObservedDeployment<'a>> {
    observed_deployments
        .values()
        .filter(|deployment| {
            deployment_matches_resource(host_id, resource_kind, resource, evidence, deployment)
        })
        .collect()
}

fn deployment_matches_resource(
    _host_id: &str,
    resource_kind: &str,
    resource: &EvidenceItem,
    evidence: &DiscoveryEvidence,
    deployment: &ObservedDeployment<'_>,
) -> bool {
    if deployment.provider_kind == "systemd" {
        return false;
    }
    let resource_values = resource_reference_values(resource_kind, resource);
    if resource_values.is_empty() {
        return false;
    }
    let mut deployment_metadata = deployment.item.metadata.to_string().to_ascii_lowercase();
    let deployment_name = deployment
        .item
        .metadata
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if deployment.provider_kind == "compose" {
        // Compose projects are represented by a single Deployment while the
        // resource references live on their container evidence. Restrict the
        // search to containers explicitly owned by this compose project.
        for container in &evidence.containers {
            if container
                .metadata
                .get("compose_project")
                .and_then(Value::as_str)
                .is_some_and(|owner| owner == deployment_name)
            {
                deployment_metadata.push_str(&container.metadata.to_string().to_ascii_lowercase());
            }
        }
        return resource_values.iter().any(|value| {
            deployment_metadata.contains(&value.to_ascii_lowercase()) || value == deployment_name
        });
    }
    resource_values
        .iter()
        .any(|value| deployment_metadata.contains(&value.to_ascii_lowercase()))
}

fn resource_reference_values(resource_kind: &str, item: &EvidenceItem) -> Vec<String> {
    let keys = match resource_kind {
        "network" | "volume" => ["name", "id"].as_slice(),
        "image" => ["id", "repository", "repo", "tag"].as_slice(),
        "document" => ["relative_path", "path"].as_slice(),
        _ => [].as_slice(),
    };
    keys.iter()
        .filter_map(|key| item.metadata.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn resource_display_name(item: &EvidenceItem, fallback: &str) -> String {
    ["name", "repository", "relative_path", "path", "id"]
        .iter()
        .find_map(|key| {
            item.metadata
                .get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| bounded_text(value, 512))
        .unwrap_or_else(|| bounded_text(fallback, 512))
}

fn freshness_name(value: &crate::contracts::Freshness) -> &'static str {
    match value {
        crate::contracts::Freshness::Fresh => "fresh",
        crate::contracts::Freshness::Stale => "stale",
        crate::contracts::Freshness::Unavailable => "unavailable",
    }
}

fn observed_deployments(
    evidence: &DiscoveryEvidence,
) -> BTreeMap<(String, String), ObservedDeployment<'_>> {
    let mut deployments = BTreeMap::new();
    for item in &evidence.compose_projects {
        insert_observed(&mut deployments, "compose", item);
    }
    for item in &evidence.systemd_units {
        insert_observed(&mut deployments, "systemd", item);
    }
    for item in evidence
        .containers
        .iter()
        .filter(|item| standalone_docker_container(item))
    {
        insert_observed(&mut deployments, "docker", item);
    }
    deployments
}

fn insert_observed<'a>(
    deployments: &mut BTreeMap<(String, String), ObservedDeployment<'a>>,
    provider_kind: &'static str,
    item: &'a EvidenceItem,
) {
    deployments
        .entry((provider_kind.to_owned(), item.external_id.clone()))
        .or_insert(ObservedDeployment {
            provider_kind,
            item,
        });
}

fn observed_external_ids(
    observed: &BTreeMap<(String, String), ObservedDeployment<'_>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_provider = BTreeMap::new();
    for (provider_kind, external_id) in observed.keys() {
        by_provider
            .entry(provider_kind.clone())
            .or_insert_with(BTreeSet::new)
            .insert(external_id.clone());
    }
    by_provider
}

fn standalone_docker_container(item: &EvidenceItem) -> bool {
    let has_compose_owner = ["compose_project", "compose_service"]
        .into_iter()
        .filter_map(|key| item.metadata.get(key).and_then(Value::as_str))
        .any(|value| !value.trim().is_empty());
    !has_compose_owner
}

fn provider_coverage(
    evidence: &DiscoveryEvidence,
    observed_external_ids: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, ProviderCoverage>, CatalogError> {
    let mut coverage = BTreeMap::new();

    if evidence.provider_results.is_empty() {
        for provider_kind in ["docker", "compose"] {
            coverage.insert(
                provider_kind.to_owned(),
                ready_coverage(provider_kind, &evidence.finished_at),
            );
        }
    } else {
        for provider_kind in CATALOG_PROVIDER_KINDS {
            let matching = evidence
                .provider_results
                .iter()
                .filter(|candidate| candidate.provider_kind == provider_kind)
                .collect::<Vec<_>>();
            if matching.is_empty() {
                continue;
            }
            coverage.insert(
                provider_kind.to_owned(),
                merge_provider_coverage(provider_kind, &matching, &evidence.finished_at)?,
            );
        }
    }

    for provider_kind in observed_external_ids.keys() {
        coverage
            .entry(provider_kind.clone())
            .or_insert_with(|| ready_coverage(provider_kind, &evidence.finished_at));
    }
    Ok(coverage)
}

fn ready_coverage(provider_kind: &str, finished_at: &str) -> ProviderCoverage {
    ProviderCoverage {
        status: DiscoveryProviderStatus::Ready,
        observed_at: finished_at.to_owned(),
        evidence_refs: Vec::new(),
        metadata: json!({
            "provider_kind": provider_kind,
            "status": "ready",
            "observed_count": 0,
            "warnings": [],
        }),
    }
}

fn merge_provider_coverage(
    provider_kind: &str,
    matching: &[&DiscoveryProviderCoverage],
    finished_at: &str,
) -> Result<ProviderCoverage, CatalogError> {
    // Any non-ready result is treated conservatively: absence cannot prove removal.
    let status = matching
        .iter()
        .find_map(|candidate| {
            (candidate.status != DiscoveryProviderStatus::Ready).then(|| candidate.status.clone())
        })
        .unwrap_or(DiscoveryProviderStatus::Ready);
    let observed_at = matching
        .iter()
        .filter_map(|candidate| candidate.observed_at.as_deref())
        .max()
        .unwrap_or(finished_at)
        .to_owned();
    let evidence_refs = matching
        .iter()
        .flat_map(|candidate| candidate.evidence_refs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let observed_count = matching.iter().fold(0u64, |count, candidate| {
        count.saturating_add(u64::from(candidate.observed_count))
    });
    let warnings = matching
        .iter()
        .flat_map(|candidate| candidate.warnings.iter())
        .collect::<Vec<_>>();
    Ok(ProviderCoverage {
        status: status.clone(),
        observed_at,
        evidence_refs,
        metadata: json!({
            "provider_kind": provider_kind,
            "status": provider_status_name(&status),
            "observed_count": observed_count,
            "warnings": warnings,
        }),
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_observed_deployment(
    connection: &mut SqliteConnection,
    run_id: &str,
    host_id: &str,
    workspace_id: &str,
    provider_kind: &str,
    item: &EvidenceItem,
    coverage: Option<&ProviderCoverage>,
    persisted_at: &str,
    fallback_observed_at: &str,
) -> Result<(), CatalogError> {
    let display_name = display_name(provider_kind, item);
    let observed_at = non_empty(&item.observed_at).unwrap_or(fallback_observed_at);
    let observed_at_epoch_ms = timestamp_millis(observed_at);
    let identity_key = identity_key(host_id, provider_kind, &item.external_id);
    let deployment_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO deployments(
            deployment_id, workspace_id, host_id, provider_kind, external_id, identity_key,
            display_name, catalog_state, freshness, created_at, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, 'observed', 'fresh', ?, ?)
         ON CONFLICT(host_id, provider_kind, external_id) DO NOTHING",
    )
    .bind(&deployment_id)
    .bind(workspace_id)
    .bind(host_id)
    .bind(provider_kind)
    .bind(&item.external_id)
    .bind(identity_key)
    .bind(&display_name)
    .bind(persisted_at)
    .bind(persisted_at)
    .execute(&mut *connection)
    .await?;

    let deployment_id: String = sqlx::query_scalar(
        "SELECT deployment_id FROM deployments
         WHERE host_id = ? AND provider_kind = ? AND external_id = ?",
    )
    .bind(host_id)
    .bind(provider_kind)
    .bind(&item.external_id)
    .fetch_one(&mut *connection)
    .await?;
    let observation_id = Uuid::new_v4().to_string();
    let provider_status = coverage.map(|value| provider_status_name(&value.status));
    let metadata = item_metadata(item);
    let inserted = sqlx::query(
        "INSERT INTO deployment_observations(
            deployment_observation_id, deployment_id, discovery_run_id, provider_kind, external_id,
            observation_state, provider_status, observed_at, observed_at_epoch_ms,
            evidence_refs_json, metadata_json, created_at
         ) VALUES (?, ?, ?, ?, ?, 'observed', ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deployment_id, discovery_run_id) DO NOTHING",
    )
    .bind(&observation_id)
    .bind(&deployment_id)
    .bind(run_id)
    .bind(provider_kind)
    .bind(&item.external_id)
    .bind(provider_status)
    .bind(observed_at)
    .bind(observed_at_epoch_ms)
    .bind(serde_json::to_string(&observed_evidence_refs(
        run_id, item,
    ))?)
    .bind(serde_json::to_string(&metadata)?)
    .bind(persisted_at)
    .execute(&mut *connection)
    .await?
    .rows_affected()
        == 1;

    if inserted {
        sqlx::query(
            "UPDATE deployments SET
                display_name = ?,
                catalog_state = CASE WHEN catalog_state = 'stale' THEN 'unassigned' ELSE catalog_state END,
                latest_observation_id = ?,
                last_observed_at = ?,
                last_observed_at_epoch_ms = ?,
                freshness = 'fresh',
                updated_at = ?
             WHERE deployment_id = ?",
        )
        .bind(display_name)
        .bind(observation_id)
        .bind(observed_at)
        .bind(observed_at_epoch_ms)
        .bind(persisted_at)
        .bind(&deployment_id)
        .execute(&mut *connection)
        .await?;

        // A fresh observation also refreshes every confirmed target that
        // points at this deployment.  Target state is a derived freshness
        // marker, so a previously stale target can recover without a user
        // mutation; archived targets remain archived.
        sqlx::query(
            "UPDATE project_targets SET
                state = CASE WHEN state = 'stale' THEN 'confirmed' ELSE state END,
                last_observed_at = CASE
                    WHEN last_observed_at IS NULL OR last_observed_at < ? THEN ?
                    ELSE last_observed_at
                END,
                updated_at = ?
             WHERE deployment_id = ? AND state <> 'archived'",
        )
        .bind(observed_at)
        .bind(observed_at)
        .bind(persisted_at)
        .bind(&deployment_id)
        .execute(&mut *connection)
        .await?;
    }

    Ok(())
}

async fn existing_deployments(
    connection: &mut SqliteConnection,
    host_id: &str,
    provider_kind: &str,
) -> Result<Vec<ExistingDeployment>, CatalogError> {
    let rows = sqlx::query(
        "SELECT deployment_id, external_id
         FROM deployments WHERE host_id = ? AND provider_kind = ?",
    )
    .bind(host_id)
    .bind(provider_kind)
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ExistingDeployment {
                deployment_id: row.try_get("deployment_id")?,
                external_id: row.try_get("external_id")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(CatalogError::Storage)
}

async fn persist_missing_observation(
    connection: &mut SqliteConnection,
    run_id: &str,
    deployment: &ExistingDeployment,
    provider_kind: &str,
    coverage: &ProviderCoverage,
    persisted_at: &str,
) -> Result<(), CatalogError> {
    let observation_id = Uuid::new_v4().to_string();
    let inserted = insert_coverage_observation(
        connection,
        &observation_id,
        run_id,
        deployment,
        provider_kind,
        "missing",
        &coverage.status,
        coverage,
        persisted_at,
    )
    .await?;
    if inserted {
        sqlx::query(
            "UPDATE deployments SET
                catalog_state = CASE WHEN catalog_state = 'ignored' THEN 'ignored' ELSE 'stale' END,
                latest_observation_id = ?,
                freshness = 'stale',
                updated_at = ?
             WHERE deployment_id = ?",
        )
        .bind(observation_id)
        .bind(persisted_at)
        .bind(&deployment.deployment_id)
        .execute(&mut *connection)
        .await?;

        // A ready provider proving absence makes all non-archived bindings
        // stale as well.  Their last known observation is deliberately kept
        // so callers can explain when the deployment was last seen.
        sqlx::query(
            "UPDATE project_targets SET state = 'stale', updated_at = ?
             WHERE deployment_id = ? AND state <> 'archived'",
        )
        .bind(persisted_at)
        .bind(&deployment.deployment_id)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

async fn persist_unknown_observation(
    connection: &mut SqliteConnection,
    run_id: &str,
    deployment: &ExistingDeployment,
    provider_kind: &str,
    coverage: &ProviderCoverage,
    persisted_at: &str,
) -> Result<(), CatalogError> {
    let observation_id = Uuid::new_v4().to_string();
    let inserted = insert_coverage_observation(
        connection,
        &observation_id,
        run_id,
        deployment,
        provider_kind,
        "unknown",
        &coverage.status,
        coverage,
        persisted_at,
    )
    .await?;
    if inserted {
        sqlx::query(
            "UPDATE deployments SET
                latest_observation_id = ?,
                freshness = 'unavailable',
                updated_at = ?
             WHERE deployment_id = ?",
        )
        .bind(observation_id)
        .bind(persisted_at)
        .bind(&deployment.deployment_id)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_coverage_observation(
    connection: &mut SqliteConnection,
    observation_id: &str,
    run_id: &str,
    deployment: &ExistingDeployment,
    provider_kind: &str,
    observation_state: &str,
    provider_status: &DiscoveryProviderStatus,
    coverage: &ProviderCoverage,
    persisted_at: &str,
) -> Result<bool, CatalogError> {
    let result = sqlx::query(
        "INSERT INTO deployment_observations(
            deployment_observation_id, deployment_id, discovery_run_id, provider_kind, external_id,
            observation_state, provider_status, observed_at, observed_at_epoch_ms,
            evidence_refs_json, metadata_json, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deployment_id, discovery_run_id) DO NOTHING",
    )
    .bind(observation_id)
    .bind(&deployment.deployment_id)
    .bind(run_id)
    .bind(provider_kind)
    .bind(&deployment.external_id)
    .bind(observation_state)
    .bind(provider_status_name(provider_status))
    .bind(&coverage.observed_at)
    .bind(timestamp_millis(&coverage.observed_at))
    .bind(serde_json::to_string(&coverage_evidence_refs(
        run_id,
        provider_kind,
        &coverage.evidence_refs,
    ))?)
    .bind(serde_json::to_string(&coverage.metadata)?)
    .bind(persisted_at)
    .execute(&mut *connection)
    .await?;
    Ok(result.rows_affected() == 1)
}

fn display_name(provider_kind: &str, item: &EvidenceItem) -> String {
    let preferred_key = match provider_kind {
        "compose" => "name",
        "systemd" => "unit",
        "docker" => "name",
        _ => "",
    };
    let value = item
        .metadata
        .get(preferred_key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&item.external_id);
    bounded_text(value, 512)
}

fn item_metadata(item: &EvidenceItem) -> Value {
    match &item.metadata {
        Value::Object(_) => item.metadata.clone(),
        value => json!({"value": value}),
    }
}

fn observed_evidence_refs(run_id: &str, item: &EvidenceItem) -> Vec<String> {
    vec![
        format!("discovery:{run_id}"),
        format!("evidence:{}", item.external_id),
        item.source.clone(),
    ]
}

fn coverage_evidence_refs(
    run_id: &str,
    provider_kind: &str,
    evidence_refs: &[String],
) -> Vec<String> {
    let mut refs = BTreeSet::from([
        format!("discovery:{run_id}"),
        format!("provider:{provider_kind}"),
    ]);
    refs.extend(
        evidence_refs
            .iter()
            .map(|external_id| format!("evidence:{external_id}")),
    );
    refs.into_iter().collect()
}

fn identity_key(host_id: &str, provider_kind: &str, external_id: &str) -> String {
    let digest = Sha256::digest(format!("{host_id}\0{provider_kind}\0{external_id}").as_bytes());
    hex_digest(&digest)
}

fn provider_status_name(status: &DiscoveryProviderStatus) -> &'static str {
    match status {
        DiscoveryProviderStatus::Ready => "ready",
        DiscoveryProviderStatus::Unavailable => "unavailable",
        DiscoveryProviderStatus::PermissionDenied => "permission_denied",
        DiscoveryProviderStatus::TimedOut => "timed_out",
        DiscoveryProviderStatus::Failed => "failed",
    }
}

fn timestamp_millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis())
        .filter(|value| *value >= 0)
}

fn bounded_text(value: &str, maximum_chars: usize) -> String {
    let bounded = value.trim().chars().take(maximum_chars).collect::<String>();
    if bounded.is_empty() {
        "unknown deployment".to_owned()
    } else {
        bounded
    }
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{EvidenceKind, Freshness, RedactionState};

    fn item(metadata: Value) -> EvidenceItem {
        EvidenceItem {
            external_id: "container:fixture".to_owned(),
            kind: EvidenceKind::Container,
            source: "ssh:containers".to_owned(),
            observed_at: "2026-08-15T00:00:00Z".to_owned(),
            freshness: Freshness::Fresh,
            sha256: None,
            redaction_state: RedactionState::NotRequired,
            metadata,
        }
    }

    #[test]
    fn compose_owned_containers_are_not_second_docker_deployments() {
        assert!(!standalone_docker_container(&item(json!({
            "compose_project": "orders",
            "compose_service": "api",
        }))));
        assert!(standalone_docker_container(&item(
            json!({"name": "sidecar"})
        )));
    }

    #[test]
    fn stable_identity_is_namespaced_by_host_provider_and_external_id() {
        let identity = identity_key("host-a", "compose", "compose:orders");
        assert_eq!(identity.len(), 64);
        assert_ne!(
            identity,
            identity_key("host-b", "compose", "compose:orders")
        );
        assert_ne!(
            identity,
            identity_key("host-a", "systemd", "compose:orders")
        );
    }
}
