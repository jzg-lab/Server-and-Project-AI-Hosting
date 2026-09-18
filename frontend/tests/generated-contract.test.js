const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const test = require("node:test");
const { join } = require("node:path");

test("generated frontend contract contains the MVP-1 API schemas", () => {
  const declaration = readFileSync(join(__dirname, "..", "generated", "api.d.ts"), "utf8");
  for (const schema of [
    "BootstrapResponse",
    "GraphSnapshotResponse",
    "GraphHealth",
    "DataSourceDescriptor",
    "ApiErrorResponse",
    "HostResponse",
    "GlobalHostsViewResponse",
    "DiscoveryProviderCoverage",
    "ConnectionTestResponse",
    "DiscoveryRunResponse",
    "DiscoveryEvidenceResponse",
    "ProjectionDraftResponse",
    "ProjectionVersionResponse",
    "LayoutResponse",
    "IgnoreRuleCreateRequest",
    "ModelProviderResponse",
    "OnboardingSessionResponse",
    "DiscoveryDiffResponse",
    "AgentProposal",
    "AuthSessionResponse",
    "LoginRequest",
    "DataExportResponse",
    "DeletionResponse",
    "MetricHistoryRollupStatistics",
    "MonitoringHistoryMaintenanceState",
    "MonitoringHistoryMaintenanceStatus",
    "MonitoringHistoryRetentionPolicy",
    "MonitoringSchedulerStatus",
  ]) {
    assert.match(declaration, new RegExp(`\\b${schema}:`), `${schema} must be generated from OpenAPI`);
  }
  assert.match(declaration, /freshness:/);
  assert.match(declaration, /data_source:/);
  assert.match(declaration, /ssh_password/);
  assert.match(declaration, /credential_kind:/);
  assert.match(declaration, /"\/api\/v1\/hosts\/{host_id}\/connection-tests":/);
  assert.match(declaration, /"\/api\/v1\/views\/global\/hosts":/);
  assert.match(declaration, /"\/api\/v1\/discovery-runs\/{run_id}\/evidence":/);
  assert.match(declaration, /"\/api\/v1\/projection-drafts\/{draft_id}":/);
  assert.match(declaration, /"\/api\/v1\/layouts\/{layout_id}":/);
  assert.match(declaration, /"\/api\/v1\/ignore-rules":/);
  assert.match(declaration, /"\/api\/v1\/model-provider":/);
  assert.match(declaration, /"\/api\/v1\/onboarding-sessions":/);
  assert.match(declaration, /"\/api\/v1\/onboarding-sessions\/{session_id}\/messages":/);
  assert.match(declaration, /"\/api\/v1\/discovery-runs\/{run_id}\/diff":/);
  assert.match(declaration, /"\/api\/v1\/discovery-runs\/{run_id}\/proposal":/);
  assert.match(declaration, /"\/api\/v1\/auth\/login":/);
  assert.match(declaration, /"\/api\/v1\/auth\/session":/);
  assert.match(declaration, /"\/api\/v1\/events\/stream":/);
  assert.match(declaration, /"\/api\/v1\/exports\/workspace":/);
  assert.match(declaration, /"\/api\/v1\/workspace":/);
  assert.match(declaration, /rollup\?: null \| components\["schemas"\]\["MetricHistoryRollupStatistics"\]/);
  assert.match(declaration, /history_maintenance: components\["schemas"\]\["MonitoringHistoryMaintenanceStatus"\]/);
  assert.match(declaration, /bucket_width_seconds: number/);
  assert.match(declaration, /retention: components\["schemas"\]\["MonitoringHistoryRetentionPolicy"\]/);
});
