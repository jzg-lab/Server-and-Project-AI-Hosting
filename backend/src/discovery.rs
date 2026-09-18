use std::{collections::BTreeSet, sync::OnceLock};

use chrono::{SecondsFormat, Utc};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    contracts::{
        DiscoveryEvidence, DiscoveryProviderCoverage, DiscoveryProviderStatus, DiscoveryRunState,
        EvidenceHostIdentity, EvidenceItem, EvidenceKind, EvidenceWarning, Freshness,
        RedactionState,
    },
    ssh::{CommandOutput, SshError, SshFailure, SshTarget, SystemSsh},
};

pub const PROTOCOL_VERSION: &str = "1";
pub const LINUX_IDENTITY_COMMAND: &str = "LC_ALL=C sh -c 'uname -s; uname -r; if [ -r /etc/os-release ]; then . /etc/os-release; printf \"%s\\n%s\\n\" \"$ID\" \"$VERSION_ID\"; else printf \"unknown\\nunknown\\n\"; fi'";
pub const DOCKER_VERSION_COMMAND: &str = "LC_ALL=C docker version --format '{\"version\":{{json .Server.Version}},\"api_version\":{{json .Server.APIVersion}},\"os\":{{json .Server.Os}},\"arch\":{{json .Server.Arch}}}'";
pub const COMPOSE_LS_COMMAND: &str = "LC_ALL=C docker compose ls --format json";
pub const CONTAINERS_COMMAND: &str = "LC_ALL=C docker ps -a --no-trunc --format '{\"id\":{{json .ID}},\"name\":{{json .Names}},\"image\":{{json .Image}},\"state\":{{json .State}},\"status\":{{json .Status}},\"ports\":{{json .Ports}},\"networks\":{{json .Networks}},\"mounts\":{{json .Mounts}},\"created_at\":{{json .CreatedAt}},\"compose_project\":{{json (.Label \"com.docker.compose.project\")}},\"compose_service\":{{json (.Label \"com.docker.compose.service\")}},\"compose_working_dir\":{{json (.Label \"com.docker.compose.project.working_dir\")}}}'";
pub const IMAGES_COMMAND: &str = "LC_ALL=C docker image ls --no-trunc --format '{\"id\":{{json .ID}},\"repository\":{{json .Repository}},\"tag\":{{json .Tag}},\"digest\":{{json .Digest}},\"created_at\":{{json .CreatedAt}},\"size\":{{json .Size}}}'";
pub const NETWORKS_COMMAND: &str = "LC_ALL=C docker network ls --no-trunc --format '{\"id\":{{json .ID}},\"name\":{{json .Name}},\"driver\":{{json .Driver}},\"scope\":{{json .Scope}}}'";
pub const VOLUMES_COMMAND: &str = "LC_ALL=C docker volume ls --format '{\"name\":{{json .Name}},\"driver\":{{json .Driver}},\"scope\":{{json .Scope}}}'";
pub const SYSTEMD_UNITS_COMMAND: &str =
    "LC_ALL=C systemctl list-units --type=service --all --no-legend --no-pager --plain";

const MAX_TOTAL_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 32;
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_DOCUMENT_TOTAL_BYTES: usize = 512 * 1024;
const MAX_PROJECT_ROOTS: usize = 8;

#[derive(Debug, Clone)]
pub struct DiscoveryRunner {
    ssh: SystemSsh,
}

#[derive(Debug, Clone)]
pub struct CommandAudit {
    pub action: String,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
    pub stderr_summary: Option<String>,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Debug, Clone)]
pub struct DiscoverySuccess {
    pub evidence: DiscoveryEvidence,
    pub audits: Vec<CommandAudit>,
    pub state: DiscoveryRunState,
}

#[derive(Debug, Clone)]
pub struct DiscoveryFailure {
    pub state: DiscoveryRunState,
    pub code: &'static str,
    pub summary: String,
    pub audits: Vec<CommandAudit>,
}

struct ParsedContainers {
    containers: Vec<EvidenceItem>,
    health_checks: Vec<EvidenceItem>,
    roots: BTreeSet<String>,
}

struct EvidenceSpec<'a> {
    kind: EvidenceKind,
    action: &'a str,
    source_value: &'a str,
    redaction_state: RedactionState,
    sha256: Option<String>,
}

impl DiscoveryRunner {
    pub fn new(ssh: SystemSsh) -> Self {
        Self { ssh }
    }

    pub fn ssh(&self) -> &SystemSsh {
        &self.ssh
    }

    pub async fn run(
        &self,
        discovery_id: &str,
        target: &SshTarget,
    ) -> Result<DiscoverySuccess, DiscoveryFailure> {
        let started_at = now();
        let mut audits = Vec::new();
        let mut total_output = 0usize;

        let linux = self
            .action(
                target,
                "linux_identity",
                LINUX_IDENTITY_COMMAND,
                64 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("linux_identity", error, &audits))?;
        let host_fact = parse_linux_identity(&linux.stdout, target, &started_at)
            .map_err(|summary| parse_failure(summary, &audits))?;

        let docker = self
            .action(
                target,
                "docker_version",
                DOCKER_VERSION_COMMAND,
                128 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("docker_version", error, &audits))?;
        let docker_engine = parse_docker_engine(&docker.stdout, target, &started_at)
            .map_err(|summary| parse_failure(summary, &audits))?;

        let compose = self
            .action(
                target,
                "compose_ls",
                COMPOSE_LS_COMMAND,
                256 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("compose_ls", error, &audits))?;
        let compose_projects = parse_compose_projects(&compose.stdout, target, &started_at)
            .map_err(|summary| parse_failure(summary, &audits))?;

        let containers = self
            .action(
                target,
                "containers",
                CONTAINERS_COMMAND,
                1024 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("containers", error, &audits))?;
        let parsed_containers = parse_containers(&containers.stdout, target, &started_at)
            .map_err(|summary| parse_failure(summary, &audits))?;

        let images = self
            .action(
                target,
                "images",
                IMAGES_COMMAND,
                512 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("images", error, &audits))?;
        let image_items = parse_simple_docker_items(
            &images.stdout,
            target,
            &started_at,
            EvidenceKind::Image,
            "image",
            &["id", "repository", "tag", "digest", "created_at", "size"],
        )
        .map_err(|summary| parse_failure(summary, &audits))?;

        let networks = self
            .action(
                target,
                "networks",
                NETWORKS_COMMAND,
                512 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("networks", error, &audits))?;
        let network_items = parse_simple_docker_items(
            &networks.stdout,
            target,
            &started_at,
            EvidenceKind::Network,
            "network",
            &["id", "name", "driver", "scope"],
        )
        .map_err(|summary| parse_failure(summary, &audits))?;

        let volumes = self
            .action(
                target,
                "volumes",
                VOLUMES_COMMAND,
                512 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("volumes", error, &audits))?;
        let volume_items = parse_simple_docker_items(
            &volumes.stdout,
            target,
            &started_at,
            EvidenceKind::Volume,
            "volume",
            &["name", "driver", "scope"],
        )
        .map_err(|summary| parse_failure(summary, &audits))?;

        let mut document_candidates = Vec::new();
        let mut document_bytes = 0usize;
        let mut roots = parsed_containers
            .roots
            .into_iter()
            .take(MAX_PROJECT_ROOTS)
            .collect::<Vec<_>>();
        roots.sort();
        for root in roots {
            if document_candidates.len() >= MAX_DOCUMENTS
                || document_bytes >= MAX_DOCUMENT_TOTAL_BYTES
            {
                break;
            }
            let list_command = document_list_command(&root)
                .map_err(|summary| document_failure(summary, &audits))?;
            let listed = self
                .action(
                    target,
                    "document_list",
                    &list_command,
                    128 * 1024,
                    &mut total_output,
                    &mut audits,
                )
                .await
                .map_err(|error| classify_failure("document_list", error, &audits))?;
            for path in listed.stdout.lines() {
                if document_candidates.len() >= MAX_DOCUMENTS
                    || document_bytes >= MAX_DOCUMENT_TOTAL_BYTES
                {
                    break;
                }
                let Some(relative) = allowed_document_path(&root, path) else {
                    continue;
                };
                let read_command = document_read_command(path)
                    .map_err(|summary| document_failure(summary, &audits))?;
                let document = self
                    .action(
                        target,
                        "document_read",
                        &read_command,
                        MAX_DOCUMENT_BYTES + 1024,
                        &mut total_output,
                        &mut audits,
                    )
                    .await
                    .map_err(|error| classify_failure("document_read", error, &audits))?;
                let parsed = parse_document(&document.stdout, target, &relative, &started_at)
                    .map_err(|summary| document_failure(summary, &audits))?;
                document_bytes = document_bytes.saturating_add(
                    parsed.metadata["captured_bytes"].as_u64().unwrap_or(0) as usize,
                );
                document_candidates.push(parsed);
            }
        }

        let finished_at = now();
        let evidence = DiscoveryEvidence {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            discovery_id: discovery_id.to_owned(),
            host: EvidenceHostIdentity {
                host_id: target.host_id.clone(),
                address: target.address.clone(),
                os: "linux".to_owned(),
            },
            host_facts: vec![host_fact],
            docker_engines: vec![docker_engine],
            compose_projects,
            systemd_units: Vec::new(),
            containers: parsed_containers.containers,
            images: image_items,
            networks: network_items,
            volumes: volume_items,
            document_candidates,
            health_checks: parsed_containers.health_checks,
            warnings: Vec::new(),
            provider_results: Vec::new(),
            started_at,
            finished_at,
        };
        Ok(DiscoverySuccess {
            evidence,
            audits,
            state: DiscoveryRunState::EvidenceReady,
        })
    }

    /// A non-Docker discovery slice used when SSH/Linux is ready but Docker is
    /// absent or inaccessible. The Linux baseline remains mandatory; systemd
    /// is an independent provider whose failure is recorded without changing
    /// the HOST connection result.
    pub async fn run_systemd(
        &self,
        discovery_id: &str,
        target: &SshTarget,
    ) -> Result<DiscoverySuccess, DiscoveryFailure> {
        let started_at = now();
        let mut audits = Vec::new();
        let mut total_output = 0usize;
        let linux = self
            .action(
                target,
                "linux_identity",
                LINUX_IDENTITY_COMMAND,
                64 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
            .map_err(|error| classify_failure("linux_identity", error, &audits))?;
        let host_fact = parse_linux_identity(&linux.stdout, target, &started_at)
            .map_err(|summary| parse_failure(summary, &audits))?;

        let (systemd_units, warnings, provider_status) = match self
            .action(
                target,
                "systemd_units",
                SYSTEMD_UNITS_COMMAND,
                512 * 1024,
                &mut total_output,
                &mut audits,
            )
            .await
        {
            Ok(output) => match parse_systemd_units(&output.stdout, target, &started_at) {
                Ok(items) => (items, Vec::new(), DiscoveryProviderStatus::Ready),
                Err(summary) => (
                    Vec::new(),
                    vec![EvidenceWarning {
                        code: "SYSTEMD_EVIDENCE_INVALID".to_owned(),
                        summary,
                    }],
                    DiscoveryProviderStatus::Failed,
                ),
            },
            Err(error) => {
                let failure = classify_failure("systemd_units", error, &audits);
                let status = match failure.code {
                    "SYSTEMD_PERMISSION_DENIED" => DiscoveryProviderStatus::PermissionDenied,
                    "SYSTEMD_UNAVAILABLE" => DiscoveryProviderStatus::Unavailable,
                    "DISCOVERY_TIMEOUT" => DiscoveryProviderStatus::TimedOut,
                    _ => DiscoveryProviderStatus::Failed,
                };
                (
                    Vec::new(),
                    vec![EvidenceWarning {
                        code: failure.code.to_owned(),
                        summary: failure.summary,
                    }],
                    status,
                )
            }
        };
        let state = if provider_status == DiscoveryProviderStatus::Ready {
            DiscoveryRunState::DiscoveryComplete
        } else {
            DiscoveryRunState::DiscoveryUnavailable
        };
        let finished_at = now();
        Ok(DiscoverySuccess {
            evidence: DiscoveryEvidence {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                discovery_id: discovery_id.to_owned(),
                host: EvidenceHostIdentity {
                    host_id: target.host_id.clone(),
                    address: target.address.clone(),
                    os: "linux".to_owned(),
                },
                host_facts: vec![host_fact],
                docker_engines: Vec::new(),
                compose_projects: Vec::new(),
                systemd_units: systemd_units.clone(),
                containers: Vec::new(),
                images: Vec::new(),
                networks: Vec::new(),
                volumes: Vec::new(),
                document_candidates: Vec::new(),
                health_checks: Vec::new(),
                provider_results: vec![DiscoveryProviderCoverage {
                    provider_kind: "systemd".to_owned(),
                    status: provider_status,
                    observed_count: u32::try_from(systemd_units.len()).unwrap_or(u32::MAX),
                    evidence_refs: systemd_units
                        .iter()
                        .map(|item| item.external_id.clone())
                        .collect(),
                    warnings: warnings.clone(),
                    observed_at: Some(finished_at.clone()),
                }],
                warnings,
                started_at,
                finished_at,
            },
            audits,
            state,
        })
    }

    async fn action(
        &self,
        target: &SshTarget,
        action: &str,
        command: &str,
        limit: usize,
        total_output: &mut usize,
        audits: &mut Vec<CommandAudit>,
    ) -> Result<CommandOutput, SshError> {
        let started_at = now();
        let result = self.ssh.execute(target, command, limit).await;
        let finished_at = now();
        match &result {
            Ok(output) => {
                *total_output = total_output.saturating_add(output.output_bytes);
                audits.push(CommandAudit {
                    action: action.to_owned(),
                    exit_code: Some(output.exit_code),
                    output_bytes: output.output_bytes,
                    stderr_summary: output.stderr_summary.clone(),
                    started_at,
                    finished_at,
                });
            }
            Err(error) => audits.push(CommandAudit {
                action: action.to_owned(),
                exit_code: error.exit_code,
                output_bytes: error.output_bytes,
                stderr_summary: Some(error.summary.clone()),
                started_at,
                finished_at,
            }),
        }
        if *total_output > MAX_TOTAL_OUTPUT_BYTES {
            return Err(SshError {
                failure: SshFailure::OutputLimit,
                summary: "discovery total output limit exceeded".to_owned(),
                exit_code: None,
                output_bytes: *total_output,
                auth_transport: None,
            });
        }
        result
    }
}

fn parse_systemd_units(
    output: &str,
    target: &SshTarget,
    observed_at: &str,
) -> Result<Vec<EvidenceItem>, String> {
    let mut items = Vec::new();
    for raw_line in output.lines().filter(|line| !line.trim().is_empty()) {
        let line = raw_line.trim().trim_start_matches('●').trim();
        let mut fields = line.split_whitespace();
        let Some(unit) = fields.next() else { continue };
        let Some(load) = fields.next() else {
            return Err("systemd returned an incomplete unit row".to_owned());
        };
        let Some(active) = fields.next() else {
            return Err("systemd returned an incomplete unit row".to_owned());
        };
        let Some(sub) = fields.next() else {
            return Err("systemd returned an incomplete unit row".to_owned());
        };
        if !unit.ends_with(".service") {
            continue;
        }
        let mut redactions = 0usize;
        let description = safe_text(&fields.collect::<Vec<_>>().join(" "), &mut redactions);
        items.push(evidence_item(
            target,
            observed_at,
            EvidenceSpec {
                kind: EvidenceKind::SystemdUnit,
                action: "systemd_units",
                source_value: unit,
                redaction_state: RedactionState::MetadataOnly,
                sha256: None,
            },
            json!({
                "unit": unit,
                "load": load,
                "active": active,
                "sub": sub,
                "description": description,
            }),
        ));
    }
    Ok(items)
}

fn parse_linux_identity(
    output: &str,
    target: &SshTarget,
    observed_at: &str,
) -> Result<EvidenceItem, String> {
    let lines = output.lines().map(str::trim).collect::<Vec<_>>();
    if lines.len() < 4 || !lines[0].eq_ignore_ascii_case("linux") {
        return Err("target did not report a Linux identity".to_owned());
    }
    Ok(evidence_item(
        target,
        observed_at,
        EvidenceSpec {
            kind: EvidenceKind::HostIdentity,
            action: "host_identity",
            source_value: &target.host_id,
            redaction_state: RedactionState::NotRequired,
            sha256: None,
        },
        json!({
            "kernel": lines[0],
            "kernel_release": lines[1],
            "distribution": lines[2],
            "distribution_version": lines[3],
        }),
    ))
}

fn parse_docker_engine(
    output: &str,
    target: &SshTarget,
    observed_at: &str,
) -> Result<EvidenceItem, String> {
    let value: Value = serde_json::from_str(output.trim())
        .map_err(|_| "docker version returned invalid JSON".to_owned())?;
    let metadata = whitelist_object(&value, &["version", "api_version", "os", "arch"]);
    if metadata.get("version").and_then(Value::as_str).is_none() {
        return Err("docker version omitted server version".to_owned());
    }
    Ok(evidence_item(
        target,
        observed_at,
        EvidenceSpec {
            kind: EvidenceKind::DockerEngine,
            action: "docker_version",
            source_value: "engine",
            redaction_state: RedactionState::NotRequired,
            sha256: None,
        },
        Value::Object(metadata),
    ))
}

fn parse_compose_projects(
    output: &str,
    target: &SshTarget,
    observed_at: &str,
) -> Result<Vec<EvidenceItem>, String> {
    parse_values(output)?
        .into_iter()
        .map(|value| {
            let name = string_field_any(&value, &["Name", "name"])
                .ok_or_else(|| "compose project omitted name".to_owned())?;
            let status = string_field_any(&value, &["Status", "status"]).unwrap_or_default();
            let config_files_value =
                string_field_any(&value, &["ConfigFiles", "config_files"]).unwrap_or_default();
            let config_files = config_files_value
                .split(',')
                .filter_map(|path| path.rsplit('/').next())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>();
            Ok(evidence_item(
                target,
                observed_at,
                EvidenceSpec {
                    kind: EvidenceKind::ComposeProject,
                    action: "compose_ls",
                    source_value: &name,
                    redaction_state: RedactionState::MetadataOnly,
                    sha256: None,
                },
                json!({"name": name, "status": status, "config_file_names": config_files}),
            ))
        })
        .collect()
}

fn parse_containers(
    output: &str,
    target: &SshTarget,
    observed_at: &str,
) -> Result<ParsedContainers, String> {
    let mut containers = Vec::new();
    let mut health_checks = Vec::new();
    let mut roots = BTreeSet::new();
    for value in parse_values(output)? {
        let id = string_field(&value, "id").ok_or_else(|| "container omitted id".to_owned())?;
        let name = string_field(&value, "name").unwrap_or_default();
        let status = string_field(&value, "status").unwrap_or_default();
        let root = string_field(&value, "compose_working_dir").unwrap_or_default();
        if valid_project_root(&root) {
            roots.insert(root.clone());
        }
        let metadata = whitelist_object(
            &value,
            &[
                "id",
                "name",
                "image",
                "state",
                "status",
                "ports",
                "networks",
                "mounts",
                "created_at",
                "compose_project",
                "compose_service",
            ],
        );
        containers.push(evidence_item(
            target,
            observed_at,
            EvidenceSpec {
                kind: EvidenceKind::Container,
                action: "containers",
                source_value: &id,
                redaction_state: RedactionState::NotRequired,
                sha256: None,
            },
            Value::Object(metadata),
        ));
        let lower = status.to_ascii_lowercase();
        let health = if lower.contains("unhealthy") {
            Some("unhealthy")
        } else if lower.contains("healthy") {
            Some("healthy")
        } else {
            None
        };
        if let Some(health) = health {
            health_checks.push(evidence_item(
                target,
                observed_at,
                EvidenceSpec {
                    kind: EvidenceKind::HealthCheck,
                    action: "containers",
                    source_value: &id,
                    redaction_state: RedactionState::NotRequired,
                    sha256: None,
                },
                json!({"container_id": id, "container_name": name, "status": health}),
            ));
        }
    }
    Ok(ParsedContainers {
        containers,
        health_checks,
        roots,
    })
}

fn parse_simple_docker_items(
    output: &str,
    target: &SshTarget,
    observed_at: &str,
    kind: EvidenceKind,
    action: &str,
    fields: &[&str],
) -> Result<Vec<EvidenceItem>, String> {
    parse_values(output)?
        .into_iter()
        .map(|value| {
            let identifier = fields
                .iter()
                .find_map(|field| string_field(&value, field))
                .ok_or_else(|| format!("{action} item omitted identifier"))?;
            Ok(evidence_item(
                target,
                observed_at,
                EvidenceSpec {
                    kind: kind.clone(),
                    action,
                    source_value: &identifier,
                    redaction_state: RedactionState::NotRequired,
                    sha256: None,
                },
                Value::Object(whitelist_object(&value, fields)),
            ))
        })
        .collect()
}

fn parse_document(
    output: &str,
    target: &SshTarget,
    relative_path: &str,
    observed_at: &str,
) -> Result<EvidenceItem, String> {
    let mut segments = output.splitn(3, '\n');
    let declared_size = segments
        .next()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .ok_or_else(|| "document size header invalid".to_owned())?;
    let declared_hash = segments
        .next()
        .map(str::trim)
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "document hash header invalid".to_owned())?;
    let captured = segments.next().unwrap_or_default();
    let captured_bytes = captured.len().min(MAX_DOCUMENT_BYTES);
    let captured = &captured[..captured.floor_char_boundary(captured_bytes)];
    let (metadata, redactions) = if is_compose_path(relative_path) {
        let (compose, redactions) = parse_compose_metadata(captured);
        (
            json!({
                "relative_path": relative_path,
                "declared_bytes": declared_size,
                "captured_bytes": captured.len(),
                "compose": compose,
                "redaction_count": redactions,
            }),
            redactions,
        )
    } else {
        let (excerpt, redactions) = redact_excerpt(captured);
        (
            json!({
                "relative_path": relative_path,
                "declared_bytes": declared_size,
                "captured_bytes": captured.len(),
                "summary_excerpt": excerpt,
                "redaction_count": redactions,
            }),
            redactions,
        )
    };
    let redaction_state = if declared_size > MAX_DOCUMENT_BYTES {
        RedactionState::Truncated
    } else if redactions > 0 {
        RedactionState::Redacted
    } else {
        RedactionState::MetadataOnly
    };
    let action = format!("document:{relative_path}");
    Ok(evidence_item(
        target,
        observed_at,
        EvidenceSpec {
            kind: EvidenceKind::Document,
            action: &action,
            source_value: relative_path,
            redaction_state,
            sha256: Some(declared_hash.to_ascii_lowercase()),
        },
        metadata,
    ))
}

fn is_compose_path(relative_path: &str) -> bool {
    relative_path.rsplit('/').next().is_some_and(|name| {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "compose.yml" | "docker-compose.yml"
        )
    })
}

fn parse_compose_metadata(captured: &str) -> (Value, usize) {
    let Ok(root) = serde_yaml::from_str::<Value>(captured) else {
        return (json!({"parse_state": "invalid"}), 0);
    };
    let Some(root) = root.as_object() else {
        return (json!({"parse_state": "invalid"}), 0);
    };
    let mut redactions = 0usize;
    let project_name = root
        .get("name")
        .and_then(|value| safe_scalar(value, &mut redactions));
    let mut services = Vec::new();
    if let Some(service_map) = root.get("services").and_then(Value::as_object) {
        for (service_name, service) in service_map {
            let mut service_metadata = serde_json::Map::new();
            service_metadata.insert(
                "name".to_owned(),
                Value::String(safe_text(service_name, &mut redactions)),
            );
            if let Some(service) = service.as_object() {
                if let Some(image) = service
                    .get("image")
                    .and_then(|value| safe_scalar(value, &mut redactions))
                {
                    service_metadata.insert("image".to_owned(), Value::String(image));
                }
                service_metadata.insert(
                    "ports".to_owned(),
                    Value::Array(compose_ports(service.get("ports"), &mut redactions)),
                );
                service_metadata.insert(
                    "networks".to_owned(),
                    Value::Array(
                        compose_reference_names(service.get("networks"), &mut redactions)
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
                service_metadata.insert(
                    "volume_targets".to_owned(),
                    Value::Array(
                        compose_volume_targets(service.get("volumes"), &mut redactions)
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
                service_metadata.insert(
                    "label_keys".to_owned(),
                    Value::Array(
                        compose_label_keys(service.get("labels"), &mut redactions)
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
            }
            services.push(Value::Object(service_metadata));
        }
    }
    let networks = root
        .get("networks")
        .map(|value| compose_reference_names(Some(value), &mut redactions))
        .unwrap_or_default();
    let volumes = root
        .get("volumes")
        .map(|value| compose_reference_names(Some(value), &mut redactions))
        .unwrap_or_default();
    (
        json!({
            "parse_state": "parsed",
            "project_name": project_name,
            "services": services,
            "networks": networks,
            "volumes": volumes,
            "dropped_sections": ["environment", "env_file", "secrets", "configs", "command", "entrypoint"],
        }),
        redactions,
    )
}

fn safe_scalar(value: &Value, redactions: &mut usize) -> Option<String> {
    let raw = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    Some(safe_text(&raw, redactions))
}

fn safe_text(value: &str, redactions: &mut usize) -> String {
    let (value, count) = redact_excerpt(value);
    *redactions = redactions.saturating_add(count);
    value
}

fn compose_reference_names(value: Option<&Value>, redactions: &mut usize) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(values) => values
            .iter()
            .filter_map(|value| safe_scalar(value, redactions))
            .collect(),
        Value::Object(values) => values
            .keys()
            .map(|value| safe_text(value, redactions))
            .collect(),
        _ => safe_scalar(value, redactions).into_iter().collect(),
    }
}

fn compose_ports(value: Option<&Value>, redactions: &mut usize) -> Vec<Value> {
    let Some(Value::Array(values)) = value else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            Value::Object(port) => {
                let mut safe = serde_json::Map::new();
                for key in ["target", "published", "protocol", "mode", "host_ip"] {
                    if let Some(value) = port
                        .get(key)
                        .and_then(|value| safe_scalar(value, redactions))
                    {
                        safe.insert(key.to_owned(), Value::String(value));
                    }
                }
                (!safe.is_empty()).then_some(Value::Object(safe))
            }
            _ => safe_scalar(value, redactions).map(Value::String),
        })
        .collect()
}

fn compose_volume_targets(value: Option<&Value>, redactions: &mut usize) -> Vec<String> {
    let Some(Value::Array(values)) = value else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            Value::Object(volume) => volume
                .get("target")
                .and_then(|value| safe_scalar(value, redactions)),
            _ => safe_scalar(value, redactions).map(|value| {
                let mut segments = value.split(':').collect::<Vec<_>>();
                if segments
                    .last()
                    .is_some_and(|mode| matches!(*mode, "ro" | "rw" | "z" | "Z"))
                {
                    segments.pop();
                }
                segments.last().copied().unwrap_or_default().to_owned()
            }),
        })
        .filter(|value| !value.is_empty())
        .collect()
}

fn compose_label_keys(value: Option<&Value>, redactions: &mut usize) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Object(labels) => labels
            .keys()
            .map(|label| safe_text(label, redactions))
            .collect(),
        Value::Array(labels) => labels
            .iter()
            .filter_map(|label| safe_scalar(label, redactions))
            .map(|label| label.split('=').next().unwrap_or_default().to_owned())
            .filter(|label| !label.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_values(output: &str) -> Result<Vec<Value>, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed)
            .map_err(|_| "command returned invalid JSON array".to_owned());
    }
    trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|_| "command returned invalid JSON line".to_owned())
        })
        .collect()
}

fn whitelist_object(value: &Value, fields: &[&str]) -> serde_json::Map<String, Value> {
    let mut result = serde_json::Map::new();
    for field in fields {
        if let Some(value) = value.get(field) {
            result.insert((*field).to_owned(), value.clone());
        }
    }
    result
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn string_field_any(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| string_field(value, field))
}

fn evidence_item(
    target: &SshTarget,
    observed_at: &str,
    spec: EvidenceSpec<'_>,
    metadata: Value,
) -> EvidenceItem {
    let kind_name = serde_json::to_value(&spec.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    EvidenceItem {
        external_id: stable_external_id(&kind_name, spec.source_value),
        kind: spec.kind,
        source: format!("ssh:{}:{}", target.host_id, spec.action),
        observed_at: observed_at.to_owned(),
        freshness: Freshness::Fresh,
        sha256: spec.sha256,
        redaction_state: spec.redaction_state,
        metadata,
    }
}

fn stable_external_id(kind: &str, value: &str) -> String {
    let digest = Sha256::digest(format!("{kind}\0{value}").as_bytes());
    format!("{kind}:{}", hex_digest(&digest)[..24].to_owned())
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_project_root(root: &str) -> bool {
    root.starts_with('/')
        && root != "/"
        && root.len() <= 4096
        && !root.contains(['\0', '\n', '\r'])
        && !root.split('/').any(|segment| segment == "..")
}

fn document_list_command(root: &str) -> Result<String, String> {
    if !valid_project_root(root) {
        return Err("invalid Compose working directory".to_owned());
    }
    Ok(format!(
        "LC_ALL=C find -- {} -maxdepth 5 -type d \\( -name .git -o -name node_modules -o -name target \\) -prune -o -type f \\( -iname 'README.md' -o -iname 'README.*' -o -iname 'PROJECT.md' -o -iname 'PROJECT.*' -o -name 'AGENTS.md' -o -path '*/docs/*.md' -o -name 'docker-compose.yml' -o -name 'compose.yml' \\) -print",
        shell_quote(root)
    ))
}

fn document_read_command(path: &str) -> Result<String, String> {
    if !valid_project_root(path) {
        return Err("invalid document path".to_owned());
    }
    Ok(format!(
        "LC_ALL=C sh -c 'size=$(wc -c < \"$1\"); hash=$(sha256sum \"$1\" | cut -d\" \" -f1); printf \"%s\\n%s\\n\" \"$size\" \"$hash\"; head -c {} -- \"$1\"' sh {}",
        MAX_DOCUMENT_BYTES + 1,
        shell_quote(path)
    ))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn allowed_document_path(root: &str, path: &str) -> Option<String> {
    if !valid_project_root(root) || !valid_project_root(path) {
        return None;
    }
    let prefix = root.trim_end_matches('/');
    let relative = path.strip_prefix(prefix)?.strip_prefix('/')?;
    if relative.is_empty() || relative.len() > 4096 {
        return None;
    }
    let lower = relative.to_ascii_lowercase();
    let segments = lower.split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| {
        matches!(*segment, ".git" | "node_modules" | "target" | ".env")
            || segment.contains("secret")
            || segment.ends_with(".key")
            || segment.ends_with(".pem")
    }) {
        return None;
    }
    let file = segments.last()?;
    let allowed = file == &"agents.md"
        || file == &"docker-compose.yml"
        || file == &"compose.yml"
        || file.starts_with("readme.")
        || file.starts_with("project.")
        || (segments.contains(&"docs") && file.ends_with(".md"));
    allowed.then(|| relative.to_owned())
}

fn redact_excerpt(value: &str) -> (String, usize) {
    static PRIVATE_KEY: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let private_key = PRIVATE_KEY.get_or_init(|| {
        Regex::new(r"(?s)-----BEGIN [^-\n]*PRIVATE KEY-----.*?-----END [^-\n]*PRIVATE KEY-----")
            .expect("valid private key regex")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(r"(?im)\b(api[_-]?key|token|secret|password)\b\s*[:=]\s*[^\s,;]+")
            .expect("valid assignment regex")
    });
    let token = TOKEN.get_or_init(|| {
        Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{12,}|ghp_[A-Za-z0-9]{12,})\b")
            .expect("valid token regex")
    });
    let mut redactions = private_key.find_iter(value).count();
    let mut result = private_key
        .replace_all(value, "[REDACTED_PRIVATE_KEY]")
        .into_owned();
    result = assignment
        .replace_all(&result, |captures: &regex::Captures<'_>| {
            redactions += 1;
            format!("{}=[REDACTED]", &captures[1])
        })
        .into_owned();
    redactions += token.find_iter(&result).count();
    result = token.replace_all(&result, "[REDACTED_TOKEN]").into_owned();
    let compact = result.split_whitespace().collect::<Vec<_>>().join(" ");
    (compact.chars().take(512).collect(), redactions)
}

fn classify_failure(action: &str, error: SshError, audits: &[CommandAudit]) -> DiscoveryFailure {
    let lower = error.summary.to_ascii_lowercase();
    let (state, code) = match error.failure {
        SshFailure::Unreachable => (DiscoveryRunState::SshUnreachable, "SSH_UNREACHABLE"),
        SshFailure::Authentication => (DiscoveryRunState::SshAuthFailed, "SSH_AUTH_FAILED"),
        SshFailure::HostKey => (DiscoveryRunState::EvidenceConflict, "HOST_KEY_UNVERIFIED"),
        SshFailure::Timeout => (DiscoveryRunState::DiscoveryTimeout, "DISCOVERY_TIMEOUT"),
        SshFailure::OutputLimit => (DiscoveryRunState::DiscoveryTimeout, "OUTPUT_LIMIT_EXCEEDED"),
        SshFailure::Process if action.starts_with("document_") => (
            DiscoveryRunState::DocumentReadFailed,
            "DOCUMENT_READ_FAILED",
        ),
        SshFailure::Process if action == "systemd_units" => {
            if lower.contains("permission denied") || lower.contains("access denied") {
                (
                    DiscoveryRunState::PermissionDenied,
                    "SYSTEMD_PERMISSION_DENIED",
                )
            } else {
                (
                    DiscoveryRunState::DiscoveryUnavailable,
                    "SYSTEMD_UNAVAILABLE",
                )
            }
        }
        SshFailure::Process if action == "compose_ls" => {
            if lower.contains("permission denied") {
                (
                    DiscoveryRunState::DockerPermissionDenied,
                    "DOCKER_PERMISSION_DENIED",
                )
            } else {
                (DiscoveryRunState::ComposeUnavailable, "COMPOSE_UNAVAILABLE")
            }
        }
        SshFailure::Process
            if action.starts_with("docker_")
                || matches!(action, "containers" | "images" | "networks" | "volumes") =>
        {
            if lower.contains("permission denied") {
                (
                    DiscoveryRunState::DockerPermissionDenied,
                    "DOCKER_PERMISSION_DENIED",
                )
            } else {
                (DiscoveryRunState::DockerUnavailable, "DOCKER_UNAVAILABLE")
            }
        }
        SshFailure::Process => (DiscoveryRunState::PermissionDenied, "PERMISSION_DENIED"),
    };
    DiscoveryFailure {
        state,
        code,
        summary: error.summary,
        audits: audits.to_vec(),
    }
}

fn parse_failure(summary: String, audits: &[CommandAudit]) -> DiscoveryFailure {
    DiscoveryFailure {
        state: DiscoveryRunState::EvidenceConflict,
        code: "EVIDENCE_INVALID",
        summary,
        audits: audits.to_vec(),
    }
}

fn document_failure(summary: String, audits: &[CommandAudit]) -> DiscoveryFailure {
    DiscoveryFailure {
        state: DiscoveryRunState::DocumentReadFailed,
        code: "DOCUMENT_READ_FAILED",
        summary,
        audits: audits.to_vec(),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_allowlist_rejects_secret_and_parent_paths() {
        assert_eq!(
            allowed_document_path("/srv/app", "/srv/app/docs/design.md"),
            Some("docs/design.md".to_owned())
        );
        assert!(allowed_document_path("/srv/app", "/srv/app/.env").is_none());
        assert!(allowed_document_path("/srv/app", "/srv/app/secret-notes.md").is_none());
        assert!(allowed_document_path("/srv/app", "/srv/other/README.md").is_none());
        assert!(document_list_command("/").is_err());
    }

    #[test]
    fn document_excerpt_redacts_secret_shapes() {
        let (excerpt, count) = redact_excerpt(
            "# Demo\nAPI_KEY=super-secret-value\ntoken: fixture_token_abcdefghijklmnopqrstuvwxyz\n",
        );
        assert!(count >= 2);
        assert!(!excerpt.contains("super-secret-value"));
        assert!(!excerpt.contains("fixture_token_abcdefghijklmnopqrstuvwxyz"));
        assert!(excerpt.contains("[REDACTED]"));
    }

    #[test]
    fn docker_parser_drops_unlisted_fields() {
        let target = SshTarget {
            host_id: "host".to_owned(),
            address: "127.0.0.1".to_owned(),
            port: 22,
            user: "fixture".to_owned(),
            credential: crate::ssh::SshCredential::PrivateKey("key".into()),
        };
        let items = parse_simple_docker_items(
            r#"{"id":"image-1","repository":"app","env":"SECRET=bad"}"#,
            &target,
            "2026-08-11T00:00:00Z",
            EvidenceKind::Image,
            "image",
            &["id", "repository"],
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].metadata.get("env").is_none());
    }

    #[test]
    fn systemd_parser_returns_stable_service_evidence_and_redacts_descriptions() {
        let target = SshTarget {
            host_id: "host".to_owned(),
            address: "127.0.0.1".to_owned(),
            port: 22,
            user: "fixture".to_owned(),
            credential: crate::ssh::SshCredential::PrivateKey("key".into()),
        };
        let output = concat!(
            "ssh.service loaded active running OpenBSD Secure Shell server\n",
            "fixture.service loaded inactive dead API_KEY=must-not-pass\n",
            "0 loaded units listed.\n",
        );
        let items = parse_systemd_units(output, &target, "2026-08-14T00:00:00Z")
            .expect("valid systemd rows");

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, EvidenceKind::SystemdUnit);
        assert_eq!(items[0].metadata["unit"], "ssh.service");
        assert_eq!(items[0].metadata["active"], "active");
        assert_eq!(items[0].metadata["sub"], "running");
        assert_eq!(items[0].source, "ssh:host:systemd_units");
        assert_eq!(
            items[0].external_id,
            stable_external_id("systemd_unit", "ssh.service")
        );
        assert!(
            !items[1].metadata["description"]
                .as_str()
                .unwrap_or_default()
                .contains("must-not-pass")
        );
    }

    #[test]
    fn required_discovery_failures_have_stable_states_and_codes() {
        let cases = [
            (
                "linux_identity",
                SshFailure::Unreachable,
                "connection refused",
                DiscoveryRunState::SshUnreachable,
                "SSH_UNREACHABLE",
            ),
            (
                "linux_identity",
                SshFailure::Authentication,
                "public key rejected",
                DiscoveryRunState::SshAuthFailed,
                "SSH_AUTH_FAILED",
            ),
            (
                "linux_identity",
                SshFailure::Timeout,
                "deadline exceeded",
                DiscoveryRunState::DiscoveryTimeout,
                "DISCOVERY_TIMEOUT",
            ),
            (
                "linux_identity",
                SshFailure::Process,
                "operation not permitted",
                DiscoveryRunState::PermissionDenied,
                "PERMISSION_DENIED",
            ),
            (
                "docker_info",
                SshFailure::Process,
                "permission denied while opening the Docker socket",
                DiscoveryRunState::DockerPermissionDenied,
                "DOCKER_PERMISSION_DENIED",
            ),
            (
                "docker_info",
                SshFailure::Process,
                "Docker daemon unavailable",
                DiscoveryRunState::DockerUnavailable,
                "DOCKER_UNAVAILABLE",
            ),
            (
                "compose_ls",
                SshFailure::Process,
                "Compose command unavailable",
                DiscoveryRunState::ComposeUnavailable,
                "COMPOSE_UNAVAILABLE",
            ),
            (
                "compose_ls",
                SshFailure::Process,
                "permission denied while opening the Docker socket",
                DiscoveryRunState::DockerPermissionDenied,
                "DOCKER_PERMISSION_DENIED",
            ),
            (
                "document_read",
                SshFailure::Process,
                "document read failed",
                DiscoveryRunState::DocumentReadFailed,
                "DOCUMENT_READ_FAILED",
            ),
            (
                "systemd_units",
                SshFailure::Process,
                "System has not been booted with systemd as init system",
                DiscoveryRunState::DiscoveryUnavailable,
                "SYSTEMD_UNAVAILABLE",
            ),
            (
                "systemd_units",
                SshFailure::Process,
                "Access denied",
                DiscoveryRunState::PermissionDenied,
                "SYSTEMD_PERMISSION_DENIED",
            ),
        ];

        for (action, kind, summary, expected_state, expected_code) in cases {
            let failure = classify_failure(
                action,
                SshError {
                    failure: kind,
                    summary: summary.to_owned(),
                    exit_code: Some(1),
                    output_bytes: 0,
                    auth_transport: None,
                },
                &[],
            );
            assert_eq!(failure.state, expected_state, "action={action}");
            assert_eq!(failure.code, expected_code, "action={action}");
        }
    }

    #[test]
    fn compose_parser_keeps_topology_fields_and_drops_secret_sections() {
        let target = SshTarget {
            host_id: "host".to_owned(),
            address: "127.0.0.1".to_owned(),
            port: 22,
            user: "fixture".to_owned(),
            credential: crate::ssh::SshCredential::PrivateKey("key".into()),
        };
        let compose = r#"name: demo
services:
  api:
    image: demo/api:1
    ports:
      - "8080:80"
    networks: [frontend]
    volumes:
      - source: data
        target: /data
    labels:
      app.role: api
      private.token: must-not-pass
    environment:
      API_TOKEN: must-not-pass
    command: ["run", "--token", "must-not-pass"]
networks:
  frontend: {}
volumes:
  data: {}
secrets:
  production_key:
    file: ./secret.pem
"#;
        let output = format!("{}\n{}\n{}", compose.len(), "c".repeat(64), compose);
        let item = parse_document(&output, &target, "compose.yml", "2026-08-11T00:00:00Z").unwrap();
        let serialized = serde_json::to_string(&item.metadata).unwrap();
        assert_eq!(item.metadata["compose"]["parse_state"], "parsed");
        assert_eq!(item.metadata["compose"]["services"][0]["name"], "api");
        assert_eq!(
            item.metadata["compose"]["services"][0]["volume_targets"][0],
            "/data"
        );
        assert!(!serialized.contains("must-not-pass"));
        assert!(!serialized.contains("production_key"));
        assert!(item.metadata.get("summary_excerpt").is_none());
    }
}
