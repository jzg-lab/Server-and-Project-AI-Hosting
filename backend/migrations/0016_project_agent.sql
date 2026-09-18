-- H6: logical Project Agent bindings and an auditable typed read-only tool
-- boundary.  This is deliberately separate from the M3 onboarding Agent.

-- The event ledger predates the Project Agent event kind and uses a closed
-- SQLite CHECK constraint. Rebuild it without dropping existing cursors.
ALTER TABLE change_events RENAME TO change_events_h6_old;
CREATE TABLE change_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'host.connection.changed',
        'discovery.run.changed',
        'projection.changed',
        'onboarding.changed',
        'project_agent.changed',
        'monitor.schedule.changed',
        'monitor.run.changed'
    )),
    subject_ref TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 0,
    summary_json TEXT NOT NULL DEFAULT '{}',
    committed_at TEXT NOT NULL
);
INSERT INTO change_events(cursor, workspace_id, kind, subject_ref, revision, summary_json, committed_at)
SELECT cursor, workspace_id, kind, subject_ref, revision, summary_json, committed_at
FROM change_events_h6_old;
DROP TABLE change_events_h6_old;
CREATE INDEX change_events_workspace_cursor_idx ON change_events(workspace_id, cursor);

CREATE TABLE project_agents (
    project_agent_id TEXT PRIMARY KEY CHECK (length(project_agent_id) = 36),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    technical_project_id TEXT NOT NULL REFERENCES technical_projects(technical_project_id) ON DELETE CASCADE,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 160),
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled')),
    capabilities_json TEXT NOT NULL CHECK (
        json_valid(capabilities_json)
        AND json_type(capabilities_json) = 'array'
        AND json_array_length(capabilities_json) = 1
        AND json_extract(capabilities_json, '$[0]') = 'read_only'
    ),
    tool_names_json TEXT NOT NULL CHECK (
        json_valid(tool_names_json)
        AND json_type(tool_names_json) = 'array'
        AND json_array_length(tool_names_json) = 5
        AND json_extract(tool_names_json, '$[0]') = 'list_project_targets'
        AND json_extract(tool_names_json, '$[1]') = 'read_deployment_observation'
        AND json_extract(tool_names_json, '$[2]') = 'read_host_capabilities'
        AND json_extract(tool_names_json, '$[3]') = 'read_service_status'
        AND json_extract(tool_names_json, '$[4]') = 'read_recent_diff'
    ),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_by TEXT NOT NULL CHECK (length(created_by) BETWEEN 1 AND 128),
    created_at TEXT NOT NULL,
    updated_by TEXT NOT NULL CHECK (length(updated_by) BETWEEN 1 AND 128),
    updated_at TEXT NOT NULL,
    UNIQUE(technical_project_id)
);

CREATE INDEX project_agents_workspace_state_idx
ON project_agents(workspace_id, state, updated_at DESC);

CREATE TABLE project_agent_tool_calls (
    tool_call_id TEXT PRIMARY KEY CHECK (length(tool_call_id) = 36),
    project_agent_id TEXT NOT NULL REFERENCES project_agents(project_agent_id) ON DELETE CASCADE,
    technical_project_id TEXT NOT NULL REFERENCES technical_projects(technical_project_id) ON DELETE CASCADE,
    project_target_id TEXT REFERENCES project_targets(project_target_id) ON DELETE SET NULL,
    tool_name TEXT NOT NULL CHECK (
        tool_name IN (
            'list_project_targets',
            'read_deployment_observation',
            'read_host_capabilities',
            'read_service_status',
            'read_recent_diff'
        )
    ),
    request_json TEXT NOT NULL CHECK (json_valid(request_json) AND json_type(request_json) = 'object'),
    result_json TEXT NOT NULL CHECK (json_valid(result_json) AND json_type(result_json) = 'object'),
    evidence_refs_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(evidence_refs_json) AND json_type(evidence_refs_json) = 'array'),
    observed_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX project_agent_tool_calls_scope_idx
ON project_agent_tool_calls(project_agent_id, created_at DESC);

CREATE INDEX project_agent_tool_calls_target_idx
ON project_agent_tool_calls(project_target_id, created_at DESC);
