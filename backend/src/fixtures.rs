use chrono::{SecondsFormat, Utc};
use uuid::Uuid;

use crate::contracts::*;

const OBSERVED_AT: &str = "2026-08-10T00:00:00Z";
const FIXTURE_SOURCE: &str = "fixture:m0-contract";

pub fn bootstrap() -> BootstrapResponse {
    let projects = project_summaries();
    BootstrapResponse {
        data: BootstrapData {
            workspace_id: "workspace".to_owned(),
            hosts: vec![HostSummary {
                host_id: "fixture-host".to_owned(),
                label: "FIXTURE_HOST".to_owned(),
                state: ProjectionState::Fixture,
                address: None,
                port: None,
                status: None,
                last_checked_at: None,
                last_error_code: None,
                last_error_summary: None,
            }],
            navigation: NavigationCounts {
                projects: projects.len() as u32,
                healthy: 2,
                attention: 1,
                unassigned: 1,
            },
            features: FeatureAvailability {
                global_world: true,
                project_resources: true,
                global_resources: false,
                project_workflow: false,
                agent_assistance: false,
            },
            projects,
        },
        meta: meta(1),
    }
}

pub fn global_world() -> GraphSnapshotResponse {
    let workspace = node(
        "coordinator",
        GraphNodeKind::Workspace,
        "业务统筹 Agent",
        "M0 Fixture · 仅保留现有视觉",
        395.0,
        246.0,
        220.0,
        104.0,
        None,
        vec![fact("项目", "4"), fact("数据源", "Fixture")],
    );
    let project_nodes = [
        ("hermes", "Hermes", "消息网关与 Agent", 72.0, 104.0),
        ("automation", "自动化补池", "自动化工作流", 715.0, 104.0),
        ("knowledge", "知识整理", "知识索引与同步", 92.0, 414.0),
        ("lab", "实验项目", "等待接入", 702.0, 414.0),
    ]
    .into_iter()
    .map(|(id, label, subtitle, x, y)| {
        node(
            id,
            GraphNodeKind::Project,
            label,
            subtitle,
            x,
            y,
            215.0,
            100.0,
            Some(id),
            vec![fact("来源", "M0 Fixture"), fact("状态", "待真实扫描")],
        )
    });
    let mut nodes = vec![workspace];
    nodes.extend(project_nodes);
    let edges = ["hermes", "automation", "knowledge", "lab"]
        .into_iter()
        .map(|project_id| GraphEdge {
            id: format!("workspace-contains-{project_id}"),
            from: "coordinator".to_owned(),
            to: project_id.to_owned(),
            kind: GraphRelationKind::Contains,
            label: "Fixture 投影".to_owned(),
            state: ProjectionState::Fixture,
            source_refs: vec![FIXTURE_SOURCE.to_owned()],
        })
        .collect();

    GraphSnapshotResponse {
        data: GraphSnapshot {
            focus: GraphFocus {
                kind: GraphScopeKind::Global,
                id: "workspace".to_owned(),
            },
            nodes,
            edges,
            layout: CanvasLayout {
                layout_id: "layout-global-fixture".to_owned(),
                scope: "global".to_owned(),
                revision: 1,
            },
        },
        meta: meta(1),
    }
}

pub fn project_resources(project_id: &str) -> Option<GraphSnapshotResponse> {
    let project = project_summaries()
        .into_iter()
        .find(|project| project.project_id == project_id)?;
    let prefix = project.project_id.as_str();
    let project_node = node(
        prefix,
        GraphNodeKind::Project,
        &project.label,
        "项目边界 · M0 Fixture",
        418.0,
        252.0,
        170.0,
        76.0,
        Some(prefix),
        vec![fact("状态", "Fixture"), fact("来源", FIXTURE_SOURCE)],
    );
    let resources = [
        (
            "compose",
            GraphNodeKind::ComposeProject,
            "Compose Project",
            92.0,
            96.0,
        ),
        (
            "service",
            GraphNodeKind::Service,
            "API Service",
            92.0,
            396.0,
        ),
        (
            "network",
            GraphNodeKind::Network,
            "APP_NETWORK",
            742.0,
            96.0,
        ),
        ("volume", GraphNodeKind::Volume, "APP_DATA", 742.0, 396.0),
        (
            "document",
            GraphNodeKind::Document,
            "README.md",
            418.0,
            476.0,
        ),
    ];
    let mut nodes = vec![project_node];
    nodes.extend(resources.into_iter().map(|(suffix, kind, label, x, y)| {
        node(
            &format!("{prefix}-{suffix}"),
            kind,
            label,
            "待 M1 真实证据替换",
            x,
            y,
            154.0,
            72.0,
            Some(prefix),
            vec![fact("来源", FIXTURE_SOURCE), fact("新鲜度", "Fixture")],
        )
    }));

    let edges = vec![
        edge(
            prefix,
            &format!("{prefix}-compose"),
            GraphRelationKind::Contains,
            "包含",
        ),
        edge(
            &format!("{prefix}-compose"),
            &format!("{prefix}-service"),
            GraphRelationKind::Deploys,
            "部署",
        ),
        edge(
            &format!("{prefix}-service"),
            &format!("{prefix}-network"),
            GraphRelationKind::ConnectsTo,
            "连接",
        ),
        edge(
            &format!("{prefix}-service"),
            &format!("{prefix}-volume"),
            GraphRelationKind::Mounts,
            "挂载",
        ),
        edge(
            &format!("{prefix}-document"),
            prefix,
            GraphRelationKind::Documents,
            "说明",
        ),
    ];

    Some(GraphSnapshotResponse {
        data: GraphSnapshot {
            focus: GraphFocus {
                kind: GraphScopeKind::Project,
                id: prefix.to_owned(),
            },
            nodes,
            edges,
            layout: CanvasLayout {
                layout_id: format!("layout-project-{prefix}-fixture"),
                scope: format!("project:{prefix}"),
                revision: 1,
            },
        },
        meta: meta(1),
    })
}

fn project_summaries() -> Vec<ProjectSummary> {
    vec![
        project(
            "hermes",
            "Hermes",
            "消息网关与 Agent",
            "健康",
            "green",
            2,
            0,
        ),
        project(
            "automation",
            "自动化补池",
            "自动化工作流",
            "健康",
            "green",
            1,
            0,
        ),
        project(
            "knowledge",
            "知识整理",
            "知识索引与同步",
            "待核验",
            "amber",
            0,
            1,
        ),
        project("lab", "实验项目", "等待接入", "未接入", "muted", 0, 0),
    ]
}

fn project(
    project_id: &str,
    label: &str,
    subtitle: &str,
    health: &str,
    tone: &str,
    activity: u32,
    alerts: u32,
) -> ProjectSummary {
    ProjectSummary {
        project_id: project_id.to_owned(),
        label: label.to_owned(),
        subtitle: subtitle.to_owned(),
        state: ProjectionState::Fixture,
        health: health.to_owned(),
        tone: tone.to_owned(),
        activity,
        alerts,
    }
}

#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    kind: GraphNodeKind,
    label: &str,
    subtitle: &str,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    project_id: Option<&str>,
    facts: Vec<GraphFact>,
) -> GraphNode {
    GraphNode {
        id: id.to_owned(),
        kind,
        label: label.to_owned(),
        subtitle: Some(subtitle.to_owned()),
        state: ProjectionState::Fixture,
        source_refs: vec![FIXTURE_SOURCE.to_owned()],
        observed_at: Some(OBSERVED_AT.to_owned()),
        position: GraphPosition { x, y },
        width,
        height,
        summary: Some("M0 契约 Fixture；M1 将由 SSH 事实替换。".to_owned()),
        facts,
        project_id: project_id.map(str::to_owned),
        health: Some(GraphHealth {
            label: "Fixture".to_owned(),
            tone: "muted".to_owned(),
            activity: 0,
            alerts: 0,
            updated: "固定样本".to_owned(),
        }),
    }
}

fn edge(from: &str, to: &str, kind: GraphRelationKind, label: &str) -> GraphEdge {
    GraphEdge {
        id: format!("{from}-{to}"),
        from: from.to_owned(),
        to: to.to_owned(),
        kind,
        label: label.to_owned(),
        state: ProjectionState::Fixture,
        source_refs: vec![FIXTURE_SOURCE.to_owned()],
    }
}

fn fact(label: &str, value: &str) -> GraphFact {
    GraphFact {
        label: label.to_owned(),
        value: value.to_owned(),
    }
}

fn meta(revision: i64) -> ApiMeta {
    ApiMeta {
        request_id: Uuid::new_v4().to_string(),
        revision,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        freshness: Freshness::Fresh,
        data_source: DataSourceDescriptor {
            kind: DataSourceKind::Fixture,
            status: DataSourceStatus::Fresh,
            label: "Rust API Fixture".to_owned(),
        },
    }
}
