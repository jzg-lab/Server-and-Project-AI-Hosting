const assert = require("node:assert/strict");
const test = require("node:test");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");

const {
  DataSourceError,
  HttpDataSource,
  MockDataSource,
  createRouteRefreshCoordinator,
  describeDataSource,
  discoveryRequestForConnection,
  discoveryRunOutcome,
  fitGraphToViewport,
  hostWorkspacePointersFromView,
  projectSnapshotFromProjection,
  readM0RouteBundle,
  selectDataSourceMode,
} = require("../data-source.js");

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function runRapidRouteSwitch({ from, to, firstBundle, secondBundle }) {
  let currentRoute = from;
  const state = { routeKey: from, loading: false, error: null, status: "idle" };
  const appliedModels = [];
  const first = deferred();
  const second = deferred();
  const coordinator = createRouteRefreshCoordinator(() => currentRoute);
  const refresh = (routeKey, gate) => {
    currentRoute = routeKey;
    Object.assign(state, { routeKey, loading: true, error: null });
    return coordinator.request(routeKey, async (request) => {
      const bundle = await gate.promise;
      appliedModels.push(...bundle.successModels);
      if (!request.isCurrent()) return;
      Object.assign(state, {
        routeKey,
        loading: false,
        error: bundle.routeError,
        status: bundle.routeError ? "unavailable" : "ready",
      });
    });
  };

  const firstDrain = refresh(from, first);
  const secondDrain = refresh(to, second);
  first.resolve(firstBundle);
  await new Promise((resolve) => setImmediate(resolve));
  const afterFirst = { ...state };
  second.resolve(secondBundle);
  await Promise.all([firstDrain, secondDrain]);
  return { afterFirst, final: { ...state }, appliedModels };
}

function envelope(data = {}) {
  return {
    data,
    meta: {
      request_id: "request-1",
      revision: 1,
      generated_at: "2026-08-11T00:00:00Z",
      freshness: "fresh",
      data_source: { kind: "fixture", status: "fresh", label: "Fixture" },
    },
  };
}

test("global business and server assets stay on separate routes", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  assert.match(appSource, /aria-label="业务统筹 Agent 配置"/);
  assert.match(appSource, /配置 URL \/ Key \/ 模型/);
  assert.match(appSource, /data-m3-action="model"/);
  assert.match(appSource, /const businessKinds = new Set\(\["coordinator", "task", "project"\]\)/);
  assert.match(appSource, /businessKinds\.has\(node\.kind\)/);
  assert.match(appSource, /业务统筹 Agent 直接统筹业务任务/);
  assert.match(appSource, /连接检查和扫描属于系统作业，不会冒充业务统筹任务/);
  assert.match(appSource, /if \(state\.view === "hosts"\) return renderHostsView\(\);/);
  assert.match(appSource, /<h1>服务器资产<\/h1>/);
  assert.match(appSource, /"全局资源与共享影响"/);
  assert.match(appSource, /aria-label="服务器操作"/);
  assert.match(appSource, /更新登录凭据/);
  assert.match(appSource, /SSH 账号密码/);
  assert.match(appSource, /先把服务器登进去/);
  assert.match(appSource, /系统不会自动扫描本机或 APP_HOST 的一堆私钥/);
  assert.match(appSource, /renderSshCredentialFields\("ssh_password"\)/);
  assert.match(appSource, /改用账号密码登录/);
  assert.match(appSource, /type="password" maxlength="4096"/);
  assert.match(appSource, /createPasswordSecretRef/);
  assert.match(appSource, /编辑地址 \/ 端口/);
  assert.match(appSource, /data-m4-delete="host"/);
  assert.match(appSource, /删除仅移除本地登记、扫描结果、投影和凭据引用/);
  assert.match(appSource, /这一步只确认服务器身份，不校验 SSH 用户或密码/);
  assert.match(appSource, /SSH 登录凭据被服务器拒绝/);
  assert.match(appSource, /DOCKER_PERMISSION_DENIED/);
  assert.match(appSource, /服务器连接已经成功；附加扫描未完成/);
  assert.match(appSource, /DOCKER_UNAVAILABLE/);
  assert.match(appSource, /docker_unavailable/);
  assert.match(appSource, /auth_transport/);
  assert.match(appSource, /SSH 登录成功/);
  assert.doesNotMatch(appSource, /host\.credential_ref/);
  assert.doesNotMatch(appSource, /function renderRealInfrastructureView\(/);
  assert.doesNotMatch(appSource, /infrastructure-canvas/);
  assert.doesNotMatch(appSource, /edge\.kind === "observes"/);
});

test("server assets keep projection editing and Agent suggestions below host details", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const stylesSource = readFileSync(join(__dirname, "..", "styles.css"), "utf8");
  assert.match(appSource, /function selectHostContext\(hostId, \{ open = true \} = \{\}\)/);
  assert.match(appSource, /restoreM2ProjectionForHost\(state\.selectedHostId\)/);
  assert.match(appSource, /function projectionWorkbenchNodes\(\)/);
  assert.match(appSource, /function renderProjectionNodeBrowser\(nodes, selectedNode\)/);
  assert.match(appSource, /data-m2-node-select=/);
  assert.match(appSource, /data-m2-action="assign"/);
  assert.match(appSource, /\$\{renderM3Agent\(\)\}/);
  const detailOrder = appSource.indexOf('aria-label="服务器详情"');
  const projectionOrder = appSource.indexOf('${renderProjectionWorkbench(selectedM2Node())}');
  assert.ok(detailOrder >= 0 && projectionOrder > detailOrder, "projection workbench follows the selected host detail");
  assert.match(stylesSource, /\.hosts-projection-workbench \{ grid-column: 1 \/ -1;/);
  assert.doesNotMatch(appSource, /function layoutInfrastructureNodes\(/);
  assert.doesNotMatch(appSource, /infrastructureLayoutPositions/);
});

test("host connection failures use the red visual state without recoloring generic attention", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const stylesSource = readFileSync(join(__dirname, "..", "styles.css"), "utf8");

  // The real-host surfaces must use red; generic project/workflow attention remains amber.
  assert.match(appSource, /failed \? "red"/);
  assert.match(appSource, /<b class="red">\$\{summary\.connection_failed_count \|\| 0\}<\/b>/);
  assert.match(appSource, /stateTone === "red"/);
  assert.match(appSource, /host-failure/);
  assert.match(stylesSource, /\.metric b\.red \{ color: var\(--red\); \}/);
  assert.match(stylesSource, /\.graph-node\.host-failure \.node-surface/);
  assert.match(stylesSource, /\.host-failure-detail \{[^}]*rgba\(238, 146, 136/);
  assert.match(stylesSource, /\.host-failure-detail span \{[^}]*var\(--red\)/);
  assert.match(stylesSource, /\.graph-node\.alert \.node-surface[^}]*rgba\(239, 189, 114/);
});

test("SSH-ready hosts start provider aggregation without changing routes", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  assert.match(appSource, /function hostConnectionAvailable\(host\)/);
  assert.match(appSource, /host\.capabilities\.includes\("ssh"\)/);
  assert.match(appSource, /function connectionReadyForDiscovery\(connection\)/);
  assert.match(appSource, /dataApi\.discoveryRequestForConnection\(connection\)/);
  assert.match(appSource, /createDiscoveryRun\(hostId, dataApi\.createIdempotencyKey\("scan"\), request\)/);
  assert.match(appSource, /Docker Compose/);
  assert.match(appSource, /systemd 服务/);
  assert.match(appSource, /"发现不可用 · SSH 连接仍保持就绪"/);
  assert.match(appSource, /function enterGlobalView\(view\)[\s\S]*state\.scope = "global";[\s\S]*state\.view = view;/);
  assert.match(appSource, /enterGlobalView\("hosts"\)/);
  assert.doesNotMatch(appSource, /function connectionHasDiscoveryCapabilities\(/);
});

test("data source selection is deterministic", () => {
  assert.equal(selectDataSourceMode({ protocol: "file:", search: "" }), "mock");
  assert.equal(selectDataSourceMode({ protocol: "https:", search: "?data=mock" }), "mock");
  assert.equal(selectDataSourceMode({ protocol: "https:", search: "" }), "http");
});

test("discovery request asks every provider to perform its own availability check", () => {
  assert.deepEqual(discoveryRequestForConnection({
    data: { capabilities: ["ssh", "linux", "ssh"] },
  }), {
    provider_kinds: ["docker", "compose", "systemd"],
    root_refs: [],
    requested_capabilities: ["read_only"],
  });
});

test("connection capability hints do not bypass any discovery provider", () => {
  assert.deepEqual(discoveryRequestForConnection({
    capabilities: ["ssh", "docker_read", "compose_read"],
  }), {
    provider_kinds: ["docker", "compose", "systemd"],
    root_refs: [],
    requested_capabilities: ["read_only"],
  });
});

test("discovery run outcomes keep partial and unavailable distinct from failure", () => {
  assert.equal(discoveryRunOutcome({ state: "accepted" }), "pending");
  assert.equal(discoveryRunOutcome({ state: "running" }), "pending");
  assert.equal(discoveryRunOutcome({ state: "evidence_ready" }), "complete");
  assert.equal(discoveryRunOutcome({ state: "discovery_complete" }), "complete");
  assert.equal(discoveryRunOutcome({ state: "discovery_partial" }), "partial");
  assert.equal(discoveryRunOutcome({ state: "discovery_unavailable" }), "unavailable");
  assert.equal(discoveryRunOutcome({ state: "ssh_auth_failed" }), "failed");
});

test("MockDataSource returns isolated fixture copies", async () => {
  const original = envelope({ projects: [{ project_id: "alpha" }] });
  const source = new MockDataSource({
    bootstrap: original,
    globalWorld: envelope({ nodes: [], edges: [] }),
    projectResources: { alpha: envelope({ focus: { id: "alpha" } }) },
  });

  const first = await source.getBootstrap();
  first.data.projects[0].project_id = "changed";
  const second = await source.getBootstrap();
  assert.equal(second.data.projects[0].project_id, "alpha");
  assert.equal((await source.getProjectResources("alpha")).data.focus.id, "alpha");
});

test("HttpDataSource uses the three M0 contract routes", async () => {
  const paths = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test/",
    fetchImpl: async (url) => {
      paths.push(url);
      return { ok: true, status: 200, json: async () => envelope({}) };
    },
  });

  await source.getBootstrap();
  await source.getGlobalWorld();
  await source.getProjectResources("project/a");
  assert.deepEqual(paths, [
    "https://example.test/api/v1/bootstrap",
    "https://example.test/api/v1/views/global/world",
    "https://example.test/api/v1/projects/project%2Fa/views/resources",
  ]);
});

test("HttpDataSource reads the dedicated global server asset view", async () => {
  const paths = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url) => {
      paths.push(url);
      return { ok: true, status: 200, json: async () => envelope({ hosts: [] }) };
    },
  });

  const response = await source.getGlobalHosts();
  assert.deepEqual(response.data.hosts, []);
  assert.deepEqual(paths, ["https://example.test/api/v1/views/global/hosts"]);
});

test("HttpDataSource carries the host monitoring read and manual run contracts", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test/",
    fetchImpl: async (url, init) => {
      calls.push({ url, init, body: init.body ? JSON.parse(init.body) : null });
      return {
        ok: true,
        status: init.method === "POST" ? 202 : 200,
        json: async () => envelope(init.method === "POST"
          ? { run_id: "run/1", host_id: "host/one", profile: "host_resource_v1", state: "queued" }
          : { host_id: "host/one", latest_run: null, current_snapshot: null }),
      };
    },
  });

  await source.getHostMonitoring("host/one");
  source.csrfToken = "csrf-token";
  await source.createMonitorRun("host/one", "monitor-1");
  await source.getMonitorRun("run/1");

  assert.deepEqual(calls.map((call) => call.url), [
    "https://example.test/api/v1/hosts/host%2Fone/monitoring",
    "https://example.test/api/v1/hosts/host%2Fone/monitor-runs",
    "https://example.test/api/v1/monitor-runs/run%2F1",
  ]);
  assert.equal(calls[0].init.method, "GET");
  assert.equal(calls[1].init.method, "POST");
  assert.equal(calls[1].init.headers["idempotency-key"], "monitor-1");
  assert.equal(calls[1].init.headers["x-csrf-token"], "csrf-token");
  assert.deepEqual(calls[1].body, { profile: "host_resource_v1" });
  assert.equal(calls[2].init.method, "GET");
});

test("MockDataSource keeps monitoring empty until a fixture explicitly supplies a snapshot", async () => {
  const source = new MockDataSource({ bootstrap: envelope({ hosts: [] }) });
  assert.deepEqual((await source.getHostMonitoring("host-1")).data, {
    host_id: "host-1",
    latest_run: null,
    current_snapshot: null,
    monitor_freshness: "unknown",
  });
});

test("host monitoring keeps a failed latest run beside the previous valid snapshot", async () => {
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async () => ({
      ok: true,
      status: 200,
      json: async () => envelope({
        host_id: "host-1",
        latest_run: { run_id: "run-failed", state: "failed", failure_code: "SSH_TIMEOUT" },
        current_snapshot: { run_id: "run-valid", freshness: "stale", observed_at: "2026-08-15T00:00:00Z" },
      }),
    }),
  });

  const response = await source.getHostMonitoring("host-1");
  assert.equal(response.data.latest_run.state, "failed");
  assert.equal(response.data.latest_run.failure_code, "SSH_TIMEOUT");
  assert.equal(response.data.current_snapshot.run_id, "run-valid");
  assert.equal(response.data.current_snapshot.freshness, "stale");
});

test("hosts cold start ignores global world and project resource failures", async () => {
  const calls = [];
  const source = {
    async getBootstrap() {
      calls.push("bootstrap");
      throw new Error("bootstrap unavailable");
    },
    async getGlobalHosts() {
      calls.push("global-hosts");
      return envelope({
        hosts: [{
          host: { host_id: "host-1", display_name: "HOST 1" },
          latest_discovery_run_id: "run-1",
          latest_projection_draft_id: "draft-1",
        }],
      });
    },
    async getGlobalWorld() {
      calls.push("global-world");
      throw new Error("global world unavailable");
    },
    async getHosts() {
      calls.push("hosts");
      throw new Error("host list unavailable");
    },
    async getProjectResources() {
      calls.push("project-resources");
      throw new Error("project resources unavailable");
    },
  };

  const bundle = await readM0RouteBundle(source, { scope: "global", view: "hosts" });

  assert.deepEqual(calls, ["bootstrap", "global-world", "hosts", "global-hosts"]);
  assert.equal(bundle.globalWorld, null);
  assert.equal(bundle.hosts.data[0].host_id, "host-1");
  assert.equal(bundle.hostsView.data.hosts[0].latest_projection_draft_id, "draft-1");
  assert.equal(bundle.bootstrap.data.projects.length, 0);
  assert.equal(bundle.bootstrap.meta, bundle.hostsView.meta);
  assert.deepEqual(bundle.projectResources, {});
});

test("current world failure preserves the independent global hosts response", async () => {
  const worldError = new Error("global world unavailable");
  const source = {
    async getBootstrap() {
      return envelope({ projects: [], hosts: [], navigation: {}, features: { global_world: true } });
    },
    async getGlobalWorld() {
      throw worldError;
    },
    async getHosts() {
      return envelope([]);
    },
    async getGlobalHosts() {
      return envelope({
        hosts: [{
          host: { host_id: "host-1", display_name: "HOST 1" },
          latest_discovery_run_id: "run-1",
          latest_projection_draft_id: "draft-1",
        }],
      });
    },
  };

  const bundle = await readM0RouteBundle(source, { scope: "global", view: "world" });

  assert.equal(bundle.globalWorld, null);
  assert.equal(bundle.hostsView.data.hosts[0].host.host_id, "host-1");
  assert.equal(bundle.routeError, worldError);
  assert.equal(bundle.errors.globalWorld, worldError);
});

test("rapid hosts to world switch ignores the stale hosts error and refreshes world", async () => {
  const hostsError = new Error("global hosts unavailable");
  const result = await runRapidRouteSwitch({
    from: "global/hosts",
    to: "global/world",
    firstBundle: { successModels: ["world:first"], routeError: hostsError },
    secondBundle: { successModels: ["world:second"], routeError: null },
  });

  assert.deepEqual(result.appliedModels, ["world:first", "world:second"]);
  assert.deepEqual(result.afterFirst, {
    routeKey: "global/world",
    loading: true,
    error: null,
    status: "idle",
  });
  assert.deepEqual(result.final, {
    routeKey: "global/world",
    loading: false,
    error: null,
    status: "ready",
  });
});

test("rapid world to hosts switch lets the current hosts failure decide status", async () => {
  const hostsError = new Error("global hosts unavailable");
  const result = await runRapidRouteSwitch({
    from: "global/world",
    to: "global/hosts",
    firstBundle: { successModels: ["world:first"], routeError: null },
    secondBundle: { successModels: ["world:second"], routeError: hostsError },
  });

  assert.deepEqual(result.appliedModels, ["world:first", "world:second"]);
  assert.deepEqual(result.afterFirst, {
    routeKey: "global/hosts",
    loading: true,
    error: null,
    status: "idle",
  });
  assert.equal(result.final.routeKey, "global/hosts");
  assert.equal(result.final.loading, false);
  assert.equal(result.final.error, hostsError);
  assert.equal(result.final.status, "unavailable");
});

test("global world accepts a graph with no HOST nodes when server assets are unavailable", async () => {
  const source = {
    async getBootstrap() {
      return envelope({ projects: [], hosts: [], navigation: {}, features: { global_world: true } });
    },
    async getGlobalWorld() {
      return envelope({ nodes: [{ id: "business-1", kind: "project" }], edges: [] });
    },
    async getHosts() {
      return envelope([]);
    },
    async getGlobalHosts() {
      throw new Error("server assets unavailable");
    },
  };

  const bundle = await readM0RouteBundle(source, { scope: "global", view: "world" });

  assert.deepEqual(bundle.globalWorld.data.nodes.map((node) => node.id), ["business-1"]);
  assert.equal(bundle.hostsView, null);
  assert.deepEqual(bundle.hosts.data, []);
});

test("HttpDataSource sends all providers and the read-only capability", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init, body: JSON.parse(init.body) });
      return { ok: true, status: 202, json: async () => envelope({ run_id: "run-1", state: "accepted" }) };
    },
  });
  const request = discoveryRequestForConnection({ capabilities: ["ssh", "linux"] });

  await source.createDiscoveryRun("host/one", "scan-1", request);

  assert.equal(calls[0].url, "https://example.test/api/v1/hosts/host%2Fone/discovery-runs");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers["idempotency-key"], "scan-1");
  assert.deepEqual(calls[0].body, {
    provider_kinds: ["docker", "compose", "systemd"],
    root_refs: [],
    requested_capabilities: ["read_only"],
  });
});

test("global hosts has an independent route and keeps connection separate from discovery", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  assert.match(appSource, /navButton\("hosts", "server"/);
  assert.match(appSource, /function renderHostsView\(\)/);
  assert.match(appSource, /if \(state\.view === "hosts"\) return renderHostsView\(\);/);
  assert.match(appSource, /connection_state/);
  assert.match(appSource, /discovery_state/);
  assert.match(appSource, /provider_coverage/);
  assert.match(appSource, /deployment_count/);
  assert.match(appSource, /project_count/);
  assert.match(appSource, /function normalizedHostConnectionState\(host\)/);
  assert.match(appSource, /"discovery_complete", "discovery_partial", "discovery_unavailable"/);
  assert.match(appSource, /ready: "可用"/);
  assert.match(appSource, /permission_denied: "权限不足"/);
  assert.match(appSource, /timed_out: "超时"/);
  assert.match(appSource, /action: "登记共享资源"/);
  assert.match(appSource, /共享资源与关系登记入口已预留/);
  const renderAppBody = appSource.slice(appSource.indexOf("function renderApp()"), appSource.indexOf("function navButton("));
  assert.doesNotMatch(renderAppBody, /retrySelectedHostConnection\(|openM2HostSetup\(/);
  const primaryActionBody = appSource.slice(appSource.indexOf("function handlePrimaryAction()"), appSource.indexOf("function bindM2Events()"));
  assert.match(primaryActionBody, /state\.view === "hosts"[\s\S]*retrySelectedHostConnection\(\)[\s\S]*openM2HostSetup/);
  assert.match(primaryActionBody, /state\.view === "resource"[\s\S]*共享资源与关系登记入口已预留/);
});

test("server assets expose evidence freshness, unknown counts, and provider evidence summaries", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const stylesSource = readFileSync(join(__dirname, "..", "styles.css"), "utf8");
  const hostsBody = appSource.slice(appSource.indexOf("function renderHostsView()"), appSource.indexOf("function resourceLensButton("));

  assert.match(appSource, /fresh: "fresh · 新鲜"/);
  assert.match(appSource, /stale: "stale · 已过期"/);
  assert.match(appSource, /unavailable: "unavailable · 未知"/);
  assert.match(hostsBody, /summary\.stale_evidence_count/);
  assert.match(hostsBody, /summary\.unknown_count/);
  assert.match(hostsBody, /stale_evidence_count: 0,\s*unknown_count: state\.hosts\.length,/);
  assert.match(hostsBody, /evidenceFreshnessLabel\(candidate\.freshness\)/);
  assert.match(hostsBody, /evidenceFreshnessLabel\(asset\.freshness\)/);
  assert.match(hostsBody, /providerWarningSummary\(provider\)/);
  assert.match(hostsBody, /provider\.evidence_refs/);
  assert.match(hostsBody, /formatObservedAt\(provider\.observed_at\)/);
  assert.match(stylesSource, /\.provider-warning \{/);
  assert.match(stylesSource, /\.asset-host-freshness\.muted \{/);
});

test("server detail separates monitoring state from connection and discovery state", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const hostsBody = appSource.slice(appSource.indexOf("function renderHostsView()"), appSource.indexOf("function resourceLensButton("));
  const monitoringBody = appSource.slice(appSource.indexOf("function renderHostMonitoring("), appSource.indexOf("function hostAssetFor("));

  assert.match(appSource, /loadHostMonitoring\(state\.selectedHostId/);
  assert.match(appSource, /createMonitorRun\(hostId, dataApi\.createIdempotencyKey\("monitor"\)\)/);
  assert.match(appSource, /const monitorTerminalStates = new Set\(\[[\s\S]*skipped_overlap[\s\S]*interrupted/);
  assert.match(appSource, /data-monitor-action="collect-once"/);
  assert.match(appSource, /data-monitor-action\]/);
  assert.match(hostsBody, /SSH 连接/);
  assert.match(hostsBody, /最近发现/);
  assert.match(hostsBody, /最近资源采集/);
  assert.match(hostsBody, /当前快照/);
  assert.match(monitoringBody, /尚未采集/);
  assert.match(monitoringBody, /当前资源状态未知/);
  assert.match(appSource, /最新采集失败/);
  assert.match(appSource, /上一次有效快照/);
  assert.match(appSource, /latestRun\.run_id[\s\S]*snapshot\.run_id/);
  assert.doesNotMatch(monitoringBody, /\|\| 0/);
});

test("monitoring refresh and host selection remain read-only until the explicit collect button", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const refreshBody = appSource.slice(appSource.indexOf("async function refreshM0Data("), appSource.indexOf("async function restoreM2Projection("));
  const bindBody = appSource.slice(appSource.indexOf("function bindM2Events("), appSource.indexOf("function openM2HostSetup("));
  assert.match(refreshBody, /loadHostMonitoring\(state\.selectedHostId, \{ render: false \}\)/);
  assert.match(bindBody, /void loadHostMonitoring\(host\.host_id\)/);
  assert.match(bindBody, /startManualMonitorRun\(button\.dataset\.hostId\)/);
  assert.doesNotMatch(refreshBody, /createMonitorRun/);
  assert.doesNotMatch(bindBody, /createMonitorRun/);
});

test("server assets use the global hosts metadata for the top data-source state", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const applyBody = appSource.slice(appSource.indexOf("function applyM0Bundle("), appSource.indexOf("function applyHosts("));
  const sourceBody = appSource.slice(appSource.indexOf("function dataSourceForCurrentView("), appSource.indexOf("function dataSourcePresentation("));
  const refreshBody = appSource.slice(appSource.indexOf("async function refreshM0Data("), appSource.indexOf("async function restoreM2Projection("));

  assert.match(appSource, /contractMeta: \{\s*globalWorld: null,\s*globalHosts: null,/);
  assert.match(applyBody, /globalWorld: bundle\.globalWorld\?\.meta \|\| null/);
  assert.match(applyBody, /globalHosts: bundle\.hostsView\?\.meta \|\| null/);
  assert.match(sourceBody, /state\.view === "hosts"[\s\S]*state\.contractMeta\.globalHosts[\s\S]*state\.contractMeta\.globalWorld/);
  assert.match(sourceBody, /freshness: meta\.freshness/);
  assert.match(refreshBody, /route\.view === "hosts"[\s\S]*bundle\.hostsView\?\.meta[\s\S]*bundle\.globalWorld\?\.meta/);
  assert.match(appSource, /function dataSourcePresentation\(\) \{\s*const source = dataSourceForCurrentView\(\);/);
});

test("global resources remain unavailable until their dedicated real read model exists", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const contractBody = appSource.slice(appSource.indexOf("function currentViewUsesContractData("), appSource.indexOf("function graphKindLabel("));
  assert.match(contractBody, /state\.view === "resource"\) return state\.features\.global_resources/);
  assert.doesNotMatch(contractBody, /state\.view === "resource"[^\n]*state\.dataSource\.kind === "real"/);
  assert.match(appSource, /兼容推导不代表已确认的真实共享关系/);
});

test("global and project keyboard shortcuts are mutually exclusive", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const shortcutBody = appSource.slice(
    appSource.indexOf('document.addEventListener("keydown"'),
    appSource.indexOf("renderApp();\nvoid initializeApplication();"),
  );

  assert.match(appSource, /data-project-view="workflow"[\s\S]{0,120}<kbd>4<\/kbd>/);
  assert.match(appSource, /data-project-view="resource"[\s\S]{0,120}<kbd>5<\/kbd>/);
  assert.match(shortcutBody, /event\.key === "1" \|\| event\.key === "2" \|\| event\.key === "3"[\s\S]*renderApp\(\);\s*return;/);
  assert.match(shortcutBody, /event\.key === "4" \|\| event\.key === "5"/);
  assert.match(shortcutBody, /event\.key === "4" && state\.dataSource\.kind === "real"\) return;/);
  assert.match(shortcutBody, /event\.key === "4" \? "workflow" : "resource"/);
  assert.doesNotMatch(shortcutBody, /event\.key === "3" \|\| event\.key === "4"/);
});

test("HttpDataSource sends M1 and M2 write preconditions", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init });
      return { ok: true, status: 200, json: async () => envelope({ draft_id: "draft-1", revision: 2 }) };
    },
  });

  await source.createSecretRef("PRIVATE_KEY_FIXTURE", "secret-1");
  await source.updateProjectionDraft("draft-1", 2, [{ op: "rename", node_id: "node-1", label: "Renamed" }], "draft-1");
  await source.updateLayout("layout-draft-1", 3, [{ node_id: "node-1", position: { x: 1, y: 2 } }], "layout-1");

  assert.equal(calls[0].init.headers["idempotency-key"], "secret-1");
  assert.equal(calls[1].init.headers["if-match"], "revision-2");
  assert.equal(calls[1].init.headers["idempotency-key"], "draft-1");
  assert.equal(calls[1].init.method, "PATCH");
  assert.equal(calls[2].init.headers["if-match"], "revision-3");
  assert.match(calls[2].url, /layouts\/layout-draft-1$/);
});

test("HttpDataSource stores SSH passwords through the password-only secret contract", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init, body: JSON.parse(init.body) });
      return { ok: true, status: 201, json: async () => envelope({ credential_ref: "secret://ssh-password/ID" }) };
    },
  });

  await source.createPasswordSecretRef("PASSWORD_FIXTURE", "password-secret-1");

  assert.equal(calls[0].url, "https://example.test/api/v1/secret-refs");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers["idempotency-key"], "password-secret-1");
  assert.deepEqual(calls[0].body, { kind: "ssh_password", password: "PASSWORD_FIXTURE" });
});

test("HttpDataSource updates a server alias without changing its address", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init, body: JSON.parse(init.body) });
      return { ok: true, status: 200, json: async () => envelope({}) };
    },
  });

  await source.updateHost("host/one", "生产节点 K", "host-alias-1");

  assert.equal(calls[0].url, "https://example.test/api/v1/hosts/host%2Fone");
  assert.equal(calls[0].init.method, "PATCH");
  assert.equal(calls[0].init.headers["idempotency-key"], "host-alias-1");
  assert.deepEqual(calls[0].body, { display_name: "生产节点 K" });
});

test("HttpDataSource sends editable SSH connection fields and a replacement credential reference", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init, body: JSON.parse(init.body) });
      return { ok: true, status: 200, json: async () => envelope({}) };
    },
  });
  const changes = {
    display_name: "边缘服务器",
    address: "edge.example",
    port: 2222,
    ssh_user: "deploy",
    credential_ref: "secret://ssh/NEW",
  };
  await source.updateHost("host-1", changes, "host-connection-1");
  assert.deepEqual(calls[0].body, changes);
  assert.equal(calls[0].init.headers["idempotency-key"], "host-connection-1");
});

test("HttpDataSource keeps M3 model, diff, and proposal traffic behind one client", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://example.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init, body: init.body ? JSON.parse(init.body) : null });
      return { ok: true, status: 200, json: async () => envelope({}) };
    },
  });

  await source.createModelSecretRef("MODEL_KEY", "model-secret");
  await source.putModelProvider({ base_url: "https://model.test/v1", model: "MODEL", credential_ref: "secret://model/ID" }, "model-config");
  await source.testModelProvider("model-test");
  await source.getDiscoveryDiff("run/1");
  await source.getDiscoveryProposal("run/1");
  await source.createOnboardingSession("draft-1", "session-create");
  await source.getOnboardingSession("session/1");
  await source.sendOnboardingMessage("session/1", { action: "adopt", proposal_id: "proposal-1", base_revision: 2 }, "proposal-adopt");

  assert.deepEqual(calls.map((call) => call.init.method), ["POST", "PUT", "POST", "GET", "GET", "POST", "GET", "POST"]);
  assert.deepEqual(calls[0].body, { kind: "model_key", api_key: "MODEL_KEY" });
  assert.equal(calls[0].init.headers["idempotency-key"], "model-secret");
  assert.match(calls[3].url, /discovery-runs\/run%2F1\/diff$/);
  assert.match(calls[4].url, /discovery-runs\/run%2F1\/proposal$/);
  assert.deepEqual(calls[5].body, { draft_id: "draft-1" });
  assert.match(calls[6].url, /onboarding-sessions\/session%2F1$/);
  assert.equal(calls[7].init.headers["idempotency-key"], "proposal-adopt");
});

test("HttpDataSource carries the owner session, CSRF, export, and delete contracts", async () => {
  const calls = [];
  const source = new HttpDataSource({
    baseUrl: "https://atlas.test",
    fetchImpl: async (url, init) => {
      calls.push({ url, init });
      const data = url.endsWith("/auth/session")
        ? { enabled: true, authenticated: true, username: "owner", csrf_token: "csrf-token" }
        : {};
      return { ok: true, status: 200, json: async () => envelope(data) };
    },
  });

  await source.getAuthSession();
  await source.exportWorkspace();
  await source.deleteHost("host/one", "delete-host");

  assert.equal(source.eventStreamUrl(), "https://atlas.test/api/v1/events/stream");
  assert.equal(calls[0].init.credentials, "same-origin");
  assert.equal(calls[1].init.method, "GET");
  assert.equal(calls[2].init.method, "DELETE");
  assert.equal(calls[2].init.headers["x-csrf-token"], "csrf-token");
  assert.equal(calls[2].init.headers["idempotency-key"], "delete-host");
  assert.equal(calls[2].init.headers["x-confirm-delete"], "host:host/one");
  assert.match(calls[2].url, /hosts\/host%2Fone$/);
});

test("project resource snapshots contain only the selected projection boundary", () => {
  const snapshot = {
    focus: { kind: "global", id: "workspace" },
    nodes: [
      { id: "host-1", kind: "host", project_id: null },
      { id: "project-a", kind: "project", project_id: "project-a" },
      { id: "service-a", kind: "service", project_id: "project-a" },
      { id: "project-b", kind: "project", project_id: "project-b" },
      { id: "service-b", kind: "service", project_id: "project-b" },
    ],
    edges: [
      { id: "host-project", from: "host-1", to: "project-a" },
      { id: "project-service", from: "project-a", to: "service-a" },
      { id: "cross-project", from: "service-a", to: "service-b" },
    ],
    layout: { layout_id: "layout-global", scope: "global", revision: 1 },
  };

  const project = projectSnapshotFromProjection(snapshot, "project-a");
  assert.deepEqual(project.focus, { kind: "project", id: "project-a" });
  assert.deepEqual(project.nodes.map((node) => node.id), ["project-a", "service-a"]);
  assert.deepEqual(project.edges.map((edge) => edge.id), ["project-service"]);
  assert.equal(project.layout.scope, "project:project-a");
  assert.equal(snapshot.layout.scope, "global");
});

test("global hosts expose the only workspace recovery pointers", () => {
  assert.deepEqual(hostWorkspacePointersFromView({
    hosts: [
      {
        host: { host_id: "host-1", source_refs: ["discovery:wrong-run"] },
        latest_discovery_run_id: "run-42",
        latest_projection_draft_id: "draft-42",
      },
      {
        host: { host_id: "host-2" },
        latest_discovery_run_id: null,
        latest_projection_draft_id: "draft-17",
      },
      { host: null, latest_discovery_run_id: "ignored" },
    ],
  }), {
    "host-1": { latestDiscoveryRunId: "run-42", latestProjectionDraftId: "draft-42" },
    "host-2": { latestDiscoveryRunId: null, latestProjectionDraftId: "draft-17" },
  });
  assert.deepEqual(hostWorkspacePointersFromView(null), {});
});

test("host workspace restoration is read-only and independent from global world HOST nodes", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const applyBundleBody = appSource.slice(appSource.indexOf("function applyM0Bundle("), appSource.indexOf("function applyHosts("));
  const applyGlobalBody = appSource.slice(appSource.indexOf("function applyGlobalSnapshot("), appSource.indexOf("function applyProjectionProjects("));
  const restoreBody = appSource.slice(appSource.indexOf("async function restoreM2Projection("), appSource.indexOf("function dataSourceForCurrentView("));
  const refreshBody = appSource.slice(appSource.indexOf("async function refreshM0Data("), appSource.indexOf("async function restoreM2Projection("));
  const bindBody = appSource.slice(appSource.indexOf("function bindAppEvents("), appSource.indexOf("function handlePrimaryAction("));

  assert.match(applyBundleBody, /hostWorkspacePointersFromView\(state\.hostsView\)/);
  assert.doesNotMatch(applyGlobalBody, /runByHost|workspaceByHost|startsWith\("discovery:"\)/);
  assert.match(restoreBody, /bundle\.hostsView\?\.meta\?\.data_source\?\.kind/);
  assert.match(restoreBody, /latestDiscoveryRunId[\s\S]*getDiscoveryRun/);
  assert.match(restoreBody, /latestProjectionDraftId[\s\S]*getProjectionDraft/);
  assert.doesNotMatch(restoreBody, /source_refs|getGlobalWorld|createConnectionTest|createDiscoveryRun/);
  assert.doesNotMatch(refreshBody, /createConnectionTest|createDiscoveryRun/);
  assert.doesNotMatch(bindBody, /createConnectionTest|createDiscoveryRun/);
});

test("M0 refresh status is guarded by the latest route key and token", () => {
  const appSource = readFileSync(join(__dirname, "..", "app.js"), "utf8");
  const refreshBody = appSource.slice(appSource.indexOf("async function refreshM0Data("), appSource.indexOf("async function restoreM2Projection("));

  assert.match(appSource, /createRouteRefreshCoordinator\(\(\) => m0RouteKey\(currentM0Route\(\)\)\)/);
  assert.match(refreshBody, /m0RefreshCoordinator\.request\(routeKey/);
  assert.match(refreshBody, /if \(!request\.isCurrent\(\)\) return;/);
  assert.doesNotMatch(refreshBody, /if \(state\.dataSource\.loading\) return;/);
});

test("project graph fitting preserves layout while keeping nodes inside the viewport", () => {
  const nodes = [
    { position: { x: 280, y: 316 }, width: 154, height: 72 },
    { position: { x: 260, y: 538 }, width: 184, height: 84 },
  ];
  const viewport = { x: 80, y: 110, width: 840, height: 400 };
  const fit = fitGraphToViewport(nodes, viewport);
  assert.equal(fit.scale, 1);
  for (const node of nodes) {
    const x = fit.x + node.position.x * fit.scale;
    const y = fit.y + node.position.y * fit.scale;
    assert.ok(x >= viewport.x && x + node.width * fit.scale <= viewport.x + viewport.width);
    assert.ok(y >= viewport.y && y + node.height * fit.scale <= viewport.y + viewport.height);
  }
  assert.deepEqual(nodes[1].position, { x: 260, y: 538 });
});

test("HttpDataSource preserves the unified API error", async () => {
  const source = new HttpDataSource({
    fetchImpl: async () => ({
      ok: false,
      status: 404,
      json: async () => ({ error: { code: "NOT_FOUND", message: "不存在", details: { id: "missing" }, request_id: "request-404" } }),
    }),
  });

  await assert.rejects(
    source.getProjectResources("missing"),
    (error) => error instanceof DataSourceError
      && error.code === "NOT_FOUND"
      && error.status === 404
      && error.requestId === "request-404",
  );
});

test("HttpDataSource rejects a successful response without the common envelope", async () => {
  const source = new HttpDataSource({
    fetchImpl: async () => ({ ok: true, status: 200, json: async () => ({ projects: [] }) }),
  });
  await assert.rejects(source.getBootstrap(), { code: "INVALID_ENVELOPE" });
});

test("data source presentation keeps fresh, stale, and unavailable distinct", () => {
  const base = { transport: "http", label: "API", revision: 3, generatedAt: "2026-08-11T00:00:00Z", fallback: false, error: null };
  assert.deepEqual(
    describeDataSource({ ...base, kind: "real", status: "fresh", freshness: "fresh" }),
    {
      tone: "fresh",
      dotClass: "live",
      label: "HTTP · 真实数据",
      shortLabel: "HTTP · 真实数据 · fresh",
      statusLabel: "fresh",
      freshnessLabel: "fresh",
      detail: "API · revision 3 · 2026-08-11T00:00:00Z",
    },
  );
  assert.equal(describeDataSource({ ...base, kind: "real", status: "stale", freshness: "stale" }).tone, "stale");
  const unavailable = describeDataSource({
    ...base,
    kind: "fixture",
    status: "unavailable",
    freshness: "unavailable",
    fallback: true,
    label: "本地 Fixture",
    error: { code: "NETWORK_ERROR", message: "连接失败", requestId: null },
  });
  assert.equal(unavailable.shortLabel, "HTTP · Fixture · unavailable");
  assert.equal(unavailable.label, "本地 Fixture");
  assert.match(unavailable.detail, /NETWORK_ERROR/);
});
