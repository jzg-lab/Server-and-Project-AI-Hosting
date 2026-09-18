/**
 * @typedef {import("./generated/api").components["schemas"]["BootstrapResponse"]} BootstrapResponse
 * @typedef {import("./generated/api").components["schemas"]["GraphSnapshotResponse"]} GraphSnapshotResponse
 */

(function initializeNetworkAtlasData(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  if (root) root.NetworkAtlasData = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function createNetworkAtlasDataApi() {
  "use strict";

  class DataSourceError extends Error {
    constructor(code, message, options = {}) {
      super(message, options.cause ? { cause: options.cause } : undefined);
      this.name = "DataSourceError";
      this.code = code;
      this.status = options.status ?? null;
      this.requestId = options.requestId ?? null;
      this.details = options.details ?? {};
    }
  }

  class MockDataSource {
    /**
     * @param {{bootstrap: BootstrapResponse, globalWorld: GraphSnapshotResponse, projectResources: Record<string, GraphSnapshotResponse>}} responses
     */
    constructor(responses) {
      this.transport = "mock";
      this.label = "本地 Fixture";
      this.responses = responses || {};
    }

    async getBootstrap() {
      return this.#read("bootstrap");
    }

    async getGlobalWorld() {
      return this.#read("globalWorld");
    }

    async getProjectResources(projectId) {
      const response = this.responses.projectResources?.[projectId];
      if (!response) {
        throw new DataSourceError("MOCK_PROJECT_NOT_FOUND", `本地 Fixture 中不存在项目 ${projectId}`, {
          status: 404,
          details: { project_id: projectId },
        });
      }
      return cloneValue(response);
    }

    async createSecretRef() { return this.#unsupported(); }
    async createHost() { return this.#unsupported(); }
    async getHosts() {
      return {
        data: cloneValue(this.responses.bootstrap?.data?.hosts || []),
        meta: cloneValue(this.responses.bootstrap?.meta),
      };
    }
    async getGlobalHosts() {
      const hosts = cloneValue(this.responses.bootstrap?.data?.hosts || []).map((host) => ({
        host: {
          host_id: host.host_id,
          display_name: host.label,
          address: host.address || null,
          port: host.port || null,
          ssh_user: host.ssh_user || "",
          credential_kind: host.credential_kind || "ssh_key",
          host_key_state: "verified",
          transport: "fixture",
          os: "linux",
          status: host.status || "connection_ready",
          created_at: host.created_at || null,
          last_checked_at: host.last_checked_at || null,
        },
        connection_state: host.status || "connection_ready",
        discovery_state: host.status === "evidence_ready" ? "evidence_ready" : null,
        provider_coverage: [],
        deployment_count: 0,
        project_count: 0,
        last_observed_at: host.last_checked_at || null,
        freshness: "stale",
        attention_count: host.last_error_code ? 1 : 0,
        latest_discovery_run_id: host.latest_discovery_run_id || null,
        latest_projection_draft_id: host.latest_projection_draft_id || null,
      }));
      return {
        data: {
          host_count: hosts.length,
          connection_ready_count: hosts.filter((host) => ["connection_ready", "evidence_ready"].includes(host.connection_state)).length,
          connection_failed_count: hosts.filter((host) => ["failed", "host_key_changed"].includes(host.connection_state)).length,
          discovery_partial_count: hosts.filter((host) => ["discovery_partial", "discovery_unavailable"].includes(host.discovery_state)).length,
          stale_evidence_count: hosts.filter((host) => host.freshness === "stale").length,
          unknown_count: hosts.filter((host) => host.freshness === "unavailable").length,
          hosts,
        },
        meta: cloneValue(this.responses.bootstrap?.meta),
      };
    }
    async getHostMonitoring(hostId) {
      const response = this.responses.hostMonitoring?.[hostId];
      if (response) return cloneValue(response);
      return {
        data: { host_id: hostId, latest_run: null, current_snapshot: null, monitor_freshness: "unknown" },
        meta: cloneValue(this.responses.bootstrap?.meta),
      };
    }
    async createMonitorRun() { return this.#unsupported(); }
    async getMonitorRun() { return this.#unsupported(); }
    async updateHost() { return this.#unsupported(); }
    async createConnectionTest() { return this.#unsupported(); }
    async confirmHostKey() { return this.#unsupported(); }
    async createDiscoveryRun() { return this.#unsupported(); }
    async getDiscoveryRun() { return this.#unsupported(); }
    async getEvidence() { return this.#unsupported(); }
    async getProjectionDraft() { return this.#unsupported(); }
    async updateProjectionDraft() { return this.#unsupported(); }
    async confirmProjection() { return this.#unsupported(); }
    async updateLayout() { return this.#unsupported(); }
    async createIgnoreRule() { return this.#unsupported(); }
    async createModelSecretRef() { return this.#unsupported(); }
    async getModelProvider() { return this.#unsupported(); }
    async putModelProvider() { return this.#unsupported(); }
    async testModelProvider() { return this.#unsupported(); }
    async getDiscoveryDiff() { return this.#unsupported(); }
    async getDiscoveryProposal() { return this.#unsupported(); }
    async createOnboardingSession() { return this.#unsupported(); }
    async getOnboardingSession() { return this.#unsupported(); }
    async sendOnboardingMessage() { return this.#unsupported(); }
    async login() { return this.#unsupported(); }
    async logout() { return this.#unsupported(); }
    async exportWorkspace() { return this.#unsupported(); }
    async exportHost() { return this.#unsupported(); }
    async exportProject() { return this.#unsupported(); }
    async deleteWorkspace() { return this.#unsupported(); }
    async deleteHost() { return this.#unsupported(); }
    async deleteProject() { return this.#unsupported(); }

    async getAuthSession() {
      const meta = cloneValue(this.responses.bootstrap?.meta || {
        request_id: "mock-auth",
        revision: 1,
        generated_at: new Date().toISOString(),
        freshness: "fresh",
        data_source: { kind: "fixture", status: "fresh", label: "本地 Fixture" },
      });
      return {
        data: {
          enabled: false,
          authenticated: true,
          owner_id: "owner-local",
          username: "owner",
          expires_at: null,
          csrf_token: null,
        },
        meta,
      };
    }

    eventStreamUrl() { return null; }

    #read(key) {
      const response = this.responses[key];
      if (!response) throw new DataSourceError("MOCK_RESPONSE_MISSING", `本地 Fixture 缺少 ${key}`);
      return cloneValue(response);
    }

    #unsupported() {
      throw new DataSourceError("M2_HTTP_REQUIRED", "HOST 初始化和投影编辑需要连接同源 API", { status: 409 });
    }
  }

  class HttpDataSource {
    constructor(options = {}) {
      this.transport = "http";
      this.label = "同源 HTTP API";
      this.baseUrl = String(options.baseUrl || "").replace(/\/$/, "");
      this.csrfToken = null;
      const fetchImpl = options.fetchImpl || globalThis.fetch;
      if (typeof fetchImpl !== "function") {
        throw new DataSourceError("FETCH_UNAVAILABLE", "当前运行环境没有可用的 Fetch 实现");
      }
      this.fetchImpl = options.fetchImpl ? fetchImpl : fetchImpl.bind(globalThis);
    }

    getBootstrap() {
      return this.#request("/api/v1/bootstrap");
    }

    async getAuthSession() {
      return this.#captureSession(await this.#request("/api/v1/auth/session"));
    }

    async login(username, password) {
      return this.#captureSession(await this.#request("/api/v1/auth/login", {
        method: "POST",
        body: { username, password },
      }));
    }

    async logout() {
      const response = await this.#request("/api/v1/auth/logout", { method: "POST", body: {} });
      this.csrfToken = null;
      return response;
    }

    eventStreamUrl() {
      return `${this.baseUrl}/api/v1/events/stream`;
    }

    getGlobalWorld() {
      return this.#request("/api/v1/views/global/world");
    }

    getGlobalHosts() {
      return this.#request("/api/v1/views/global/hosts");
    }

    getHostMonitoring(hostId) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}/monitoring`);
    }

    createMonitorRun(hostId, idempotencyKey) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}/monitor-runs`, {
        method: "POST",
        body: { profile: "host_resource_v1" },
        idempotencyKey,
      });
    }

    getMonitorRun(runId) {
      return this.#request(`/api/v1/monitor-runs/${encodeURIComponent(runId)}`);
    }

    getProjectResources(projectId) {
      return this.#request(`/api/v1/projects/${encodeURIComponent(projectId)}/views/resources`);
    }

    createSecretRef(privateKey, idempotencyKey) {
      return this.#request("/api/v1/secret-refs", {
        method: "POST",
        body: { kind: "ssh_key", private_key: privateKey },
        idempotencyKey,
      });
    }

    createPasswordSecretRef(password, idempotencyKey) {
      return this.#request("/api/v1/secret-refs", {
        method: "POST",
        body: { kind: "ssh_password", password },
        idempotencyKey,
      });
    }

    createHost(host, idempotencyKey) {
      return this.#request("/api/v1/hosts", { method: "POST", body: host, idempotencyKey });
    }

    getHosts() {
      return this.#request("/api/v1/hosts");
    }

    updateHost(hostId, changes, idempotencyKey) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}`, {
        method: "PATCH",
        body: typeof changes === "string" ? { display_name: changes } : changes,
        idempotencyKey,
      });
    }

    createConnectionTest(hostId, idempotencyKey) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}/connection-tests`, {
        method: "POST",
        idempotencyKey,
      });
    }

    confirmHostKey(hostId, fingerprint, idempotencyKey) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}/host-key-confirmations`, {
        method: "POST",
        body: { fingerprint },
        idempotencyKey,
      });
    }

    createDiscoveryRun(hostId, idempotencyKey, request = {}) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}/discovery-runs`, {
        method: "POST",
        body: request,
        idempotencyKey,
      });
    }

    getDiscoveryRun(runId) {
      return this.#request(`/api/v1/discovery-runs/${encodeURIComponent(runId)}`);
    }

    getEvidence(runId) {
      return this.#request(`/api/v1/discovery-runs/${encodeURIComponent(runId)}/evidence`);
    }

    getProjectionDraft(draftId) {
      return this.#request(`/api/v1/projection-drafts/${encodeURIComponent(draftId)}`);
    }

    updateProjectionDraft(draftId, revision, operations, idempotencyKey) {
      return this.#request(`/api/v1/projection-drafts/${encodeURIComponent(draftId)}`, {
        method: "PATCH",
        body: { base_revision: revision, operations },
        ifMatch: revision,
        idempotencyKey,
      });
    }

    confirmProjection(draftId, revision, idempotencyKey) {
      return this.#request(`/api/v1/projection-drafts/${encodeURIComponent(draftId)}/confirm`, {
        method: "POST",
        body: { base_revision: revision },
        ifMatch: revision,
        idempotencyKey,
      });
    }

    updateLayout(layoutId, revision, positions, idempotencyKey) {
      return this.#request(`/api/v1/layouts/${encodeURIComponent(layoutId)}`, {
        method: "PATCH",
        body: { base_revision: revision, positions },
        ifMatch: revision,
        idempotencyKey,
      });
    }

    createIgnoreRule(draftId, nodeId, action, revision, idempotencyKey) {
      return this.#request("/api/v1/ignore-rules", {
        method: "POST",
        body: { draft_id: draftId, node_id: nodeId, action, base_revision: revision },
        ifMatch: revision,
        idempotencyKey,
      });
    }

    createModelSecretRef(apiKey, idempotencyKey) {
      return this.#request("/api/v1/secret-refs", {
        method: "POST",
        body: { kind: "model_key", api_key: apiKey },
        idempotencyKey,
      });
    }

    getModelProvider() {
      return this.#request("/api/v1/model-provider");
    }

    putModelProvider(config, idempotencyKey) {
      return this.#request("/api/v1/model-provider", {
        method: "PUT",
        body: config,
        idempotencyKey,
      });
    }

    testModelProvider(idempotencyKey) {
      return this.#request("/api/v1/model-provider/test", {
        method: "POST",
        idempotencyKey,
      });
    }

    getDiscoveryDiff(runId) {
      return this.#request(`/api/v1/discovery-runs/${encodeURIComponent(runId)}/diff`);
    }

    getDiscoveryProposal(runId) {
      return this.#request(`/api/v1/discovery-runs/${encodeURIComponent(runId)}/proposal`);
    }

    createOnboardingSession(draftId, idempotencyKey) {
      return this.#request("/api/v1/onboarding-sessions", {
        method: "POST",
        body: { draft_id: draftId },
        idempotencyKey,
      });
    }

    getOnboardingSession(sessionId) {
      return this.#request(`/api/v1/onboarding-sessions/${encodeURIComponent(sessionId)}`);
    }

    sendOnboardingMessage(sessionId, message, idempotencyKey) {
      return this.#request(`/api/v1/onboarding-sessions/${encodeURIComponent(sessionId)}/messages`, {
        method: "POST",
        body: message,
        idempotencyKey,
      });
    }

    exportWorkspace() {
      return this.#request("/api/v1/exports/workspace");
    }

    exportHost(hostId) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}/export`);
    }

    exportProject(projectId) {
      return this.#request(`/api/v1/projects/${encodeURIComponent(projectId)}/export`);
    }

    async deleteWorkspace(idempotencyKey) {
      const response = await this.#request("/api/v1/workspace", {
        method: "DELETE",
        idempotencyKey,
        confirmDelete: "workspace:workspace-default",
      });
      this.csrfToken = null;
      return response;
    }

    deleteHost(hostId, idempotencyKey) {
      return this.#request(`/api/v1/hosts/${encodeURIComponent(hostId)}`, {
        method: "DELETE",
        idempotencyKey,
        confirmDelete: `host:${hostId}`,
      });
    }

    deleteProject(projectId, idempotencyKey) {
      return this.#request(`/api/v1/projects/${encodeURIComponent(projectId)}`, {
        method: "DELETE",
        idempotencyKey,
        confirmDelete: `project:${projectId}`,
      });
    }

    async #request(path, options = {}) {
      const headers = { accept: "application/json" };
      if (options.body !== undefined) headers["content-type"] = "application/json";
      if (options.idempotencyKey) headers["idempotency-key"] = options.idempotencyKey;
      if (Number.isInteger(options.ifMatch) && options.ifMatch >= 0) headers["if-match"] = `revision-${options.ifMatch}`;
      if (options.confirmDelete) headers["x-confirm-delete"] = options.confirmDelete;
      const method = options.method || "GET";
      if (!["GET", "HEAD", "OPTIONS"].includes(method) && this.csrfToken) {
        headers["x-csrf-token"] = this.csrfToken;
      }
      let response;
      try {
        response = await this.fetchImpl(`${this.baseUrl}${path}`, {
          method,
          headers,
          body: options.body === undefined ? undefined : JSON.stringify(options.body),
          cache: "no-store",
          credentials: "same-origin",
        });
      } catch (cause) {
        throw new DataSourceError("NETWORK_ERROR", "连接本地投影 API 失败", { cause });
      }

      let payload;
      try {
        payload = await response.json();
      } catch (cause) {
        throw new DataSourceError("INVALID_JSON", "投影 API 返回了无效 JSON", {
          cause,
          status: response.status,
        });
      }

      if (!response.ok) {
        const error = payload?.error || {};
        if (response.status === 401) this.csrfToken = null;
        throw new DataSourceError(error.code || `HTTP_${response.status}`, error.message || "投影 API 请求失败", {
          status: response.status,
          requestId: error.request_id,
          details: error.details,
        });
      }

      return assertEnvelope(payload, path);
    }

    #captureSession(response) {
      this.csrfToken = response?.data?.csrf_token || null;
      return response;
    }
  }

  function assertEnvelope(payload, path = "response") {
    if (!payload || typeof payload !== "object" || !("data" in payload) || !payload.meta) {
      throw new DataSourceError("INVALID_ENVELOPE", `${path} 缺少 data/meta 响应包络`);
    }
    const source = payload.meta.data_source;
    if (!source || !source.kind || !source.status || !payload.meta.freshness) {
      throw new DataSourceError("INVALID_META", `${path} 缺少数据来源或新鲜度字段`);
    }
    return payload;
  }

  function mergeHostRecords(bootstrapHosts = [], directHosts = [], hostsView = null) {
    const records = new Map((directHosts || []).map((host) => [host.host_id, { ...host }]));
    (bootstrapHosts || []).forEach((host) => {
      const current = records.get(host.host_id) || {};
      records.set(host.host_id, {
        ...current,
        host_id: host.host_id,
        display_name: current.display_name || host.label,
        address: current.address || host.address,
        port: current.port || host.port,
        status: current.status || host.status,
        last_checked_at: current.last_checked_at || host.last_checked_at,
        last_error_code: current.last_error_code || host.last_error_code,
        last_error_summary: current.last_error_summary || host.last_error_summary,
      });
    });
    (hostsView?.hosts || []).forEach((asset) => {
      const host = asset?.host;
      if (host?.host_id) records.set(host.host_id, { ...(records.get(host.host_id) || {}), ...host });
    });
    return Array.from(records.values());
  }

  function createRouteRefreshCoordinator(getCurrentRouteKey) {
    let nextToken = 0;
    let latestToken = 0;
    let active = null;
    let pending = null;
    let drainPromise = null;

    const drain = async () => {
      try {
        while (pending) {
          const request = pending;
          pending = null;
          active = request;
          await request.run({
            token: request.token,
            routeKey: request.routeKey,
            isCurrent: () => active?.token === request.token
              && latestToken === request.token
              && getCurrentRouteKey() === request.routeKey,
          });
        }
      } finally {
        active = null;
        drainPromise = null;
      }
    };

    return {
      request(routeKey, run) {
        const token = ++nextToken;
        latestToken = token;
        pending = { token, routeKey, run };
        if (!drainPromise) drainPromise = drain();
        return drainPromise;
      },
    };
  }

  async function readM0RouteBundle(source, route = {}) {
    const read = async (operation, path) => {
      if (!operation) return { response: null, error: null };
      try {
        return { response: assertEnvelope(await operation(), path), error: null };
      } catch (error) {
        return { response: null, error };
      }
    };
    const [bootstrapResult, worldResult, directHostsResult, hostsViewResult] = await Promise.all([
      read(() => source.getBootstrap(), "bootstrap"),
      read(() => source.getGlobalWorld(), "global world"),
      read(source.getHosts ? () => source.getHosts() : null, "hosts"),
      read(source.getGlobalHosts ? () => source.getGlobalHosts() : null, "global hosts"),
    ]);
    const globalWorld = worldResult.response;
    const hostsView = hostsViewResult.response;
    const projectResources = {};
    const projectErrors = {};
    if (bootstrapResult.response && source.getProjectResources) {
      const projectResults = await Promise.all(bootstrapResult.response.data.projects.map(async (project) => [
        project.project_id,
        await read(() => source.getProjectResources(project.project_id), `project ${project.project_id}`),
      ]));
      projectResults.forEach(([projectId, result]) => {
        if (result.response) projectResources[projectId] = result.response;
        else if (result.error) projectErrors[projectId] = result.error;
      });
    }
    const hostsRoute = route.scope === "global" && route.view === "hosts";
    const worldRoute = route.scope === "global" && route.view !== "hosts";
    const projectRoute = route.scope === "project" && route.view === "resource";
    const routeError = hostsRoute && !hostsView
      ? hostsViewResult.error || new DataSourceError("GLOBAL_HOSTS_UNAVAILABLE", "服务器资产读模型不可用")
      : worldRoute && !globalWorld
        ? worldResult.error || new DataSourceError("GLOBAL_WORLD_UNAVAILABLE", "全局投影读模型不可用")
        : projectRoute && route.projectId && !projectResources[route.projectId]
          ? projectErrors[route.projectId]
        || bootstrapResult.error
        || new DataSourceError("PROJECT_RESOURCES_UNAVAILABLE", `项目 ${route.projectId} 资源读模型不可用`)
          : null;
    const fallbackMeta = hostsView?.meta || globalWorld?.meta || directHostsResult.response?.meta;
    const projectionProjects = (globalWorld?.data?.nodes || []).filter((node) => node.kind === "project");
    const bootstrap = bootstrapResult.response || {
      data: {
        workspace_id: "workspace",
        projects: projectionProjects.map((node) => ({
          project_id: node.id,
          label: node.label,
          subtitle: node.subtitle || "本地项目投影",
          state: node.state,
          health: node.health?.label || node.state,
          tone: node.health?.tone || "muted",
          activity: node.health?.activity || 0,
          alerts: node.health?.alerts || 0,
        })),
        hosts: [],
        navigation: {
          projects: projectionProjects.length,
          healthy: projectionProjects.filter((node) => node.state === "confirmed").length,
          attention: projectionProjects.filter((node) => node.state !== "confirmed").length,
          unassigned: hostsView?.data?.host_count || 0,
        },
        features: {
          global_world: Boolean(globalWorld),
          project_resources: Object.keys(projectResources).length > 0,
          global_resources: false,
          project_workflow: false,
          agent_assistance: false,
        },
      },
      meta: fallbackMeta,
    };
    return {
      bootstrap,
      globalWorld,
      hosts: {
        data: mergeHostRecords(bootstrap.data.hosts, directHostsResult.response?.data || [], hostsView?.data),
        meta: directHostsResult.response?.meta || hostsView?.meta || bootstrap.meta,
      },
      hostsView,
      projectResources,
      routeError,
      errors: {
        bootstrap: bootstrapResult.error,
        globalWorld: worldResult.error,
        hosts: directHostsResult.error,
        globalHosts: hostsViewResult.error,
        projectResources: projectErrors,
      },
    };
  }

  function selectDataSourceMode(locationLike = {}) {
    const search = new URLSearchParams(locationLike.search || "");
    if (locationLike.protocol === "file:" || search.get("data") === "mock") return "mock";
    return "http";
  }

  function createDataSource(options = {}) {
    const mode = options.mode || selectDataSourceMode(options.location || {});
    if (mode === "mock") return new MockDataSource(options.mockResponses);
    return new HttpDataSource({ baseUrl: options.baseUrl, fetchImpl: options.fetchImpl });
  }

  function errorRecord(error) {
    return {
      code: error?.code || "UNKNOWN_ERROR",
      message: error?.message || "未知数据源错误",
      status: error?.status ?? null,
      requestId: error?.requestId ?? null,
    };
  }

  function describeDataSource(source) {
    const kindLabel = source.kind === "real" ? "真实数据" : "Fixture";
    const transportLabel = source.transport === "http" ? "HTTP" : "本地";
    const statusLabel = source.status || source.freshness || "unavailable";
    const failed = statusLabel === "unavailable";
    return {
      tone: failed ? "unavailable" : source.freshness === "stale" ? "stale" : "fresh",
      dotClass: failed ? "amber" : source.freshness === "fresh" ? "live" : "",
      label: source.fallback ? source.label : `${transportLabel} · ${kindLabel}`,
      shortLabel: `${transportLabel} · ${kindLabel} · ${statusLabel}`,
      statusLabel,
      freshnessLabel: source.freshness || statusLabel,
      detail: source.error
        ? `${source.error.code}: ${source.error.message}${source.error.requestId ? ` · request ${source.error.requestId}` : ""}`
        : `${source.label} · revision ${source.revision}${source.generatedAt ? ` · ${source.generatedAt}` : ""}`,
    };
  }

  function cloneValue(value) {
    if (typeof structuredClone === "function") return structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function createIdempotencyKey(prefix = "m2") {
    const suffix = globalThis.crypto?.randomUUID?.()
      || `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
    return `${prefix}-${suffix}`;
  }

  function discoveryRequestForConnection() {
    // The connection probe proves SSH/Linux access only. Each discovery adapter
    // owns its own availability check, so every read-only provider is requested.
    return {
      provider_kinds: ["docker", "compose", "systemd"],
      root_refs: [],
      requested_capabilities: ["read_only"],
    };
  }

  function discoveryRunOutcome(run) {
    const state = (run?.data || run || {}).state;
    if (["accepted", "running"].includes(state)) return "pending";
    if (["evidence_ready", "discovery_complete"].includes(state)) return "complete";
    if (state === "discovery_partial") return "partial";
    if (state === "discovery_unavailable") return "unavailable";
    return "failed";
  }

  function projectSnapshotFromProjection(snapshot, projectId) {
    const nodes = (snapshot?.nodes || []).filter((node) => (
      (node.kind === "project" && node.id === projectId)
      || (node.kind !== "project" && node.project_id === projectId)
    ));
    const nodeIds = new Set(nodes.map((node) => node.id));
    return {
      ...snapshot,
      focus: { kind: "project", id: projectId },
      nodes,
      edges: (snapshot?.edges || []).filter((edge) => nodeIds.has(edge.from) && nodeIds.has(edge.to)),
      layout: snapshot?.layout ? { ...snapshot.layout, scope: `project:${projectId}` } : snapshot?.layout,
    };
  }

  function hostWorkspacePointersFromView(hostsView) {
    return Object.fromEntries((hostsView?.hosts || []).flatMap((asset) => {
      const hostId = asset?.host?.host_id;
      if (!hostId) return [];
      return [[hostId, {
        latestDiscoveryRunId: asset.latest_discovery_run_id || null,
        latestProjectionDraftId: asset.latest_projection_draft_id || null,
      }]];
    }));
  }

  function fitGraphToViewport(nodes, viewport) {
    if (!Array.isArray(nodes) || nodes.length === 0) return { x: 0, y: 0, scale: 1 };
    const minX = Math.min(...nodes.map((node) => node.position.x));
    const minY = Math.min(...nodes.map((node) => node.position.y));
    const maxX = Math.max(...nodes.map((node) => node.position.x + node.width));
    const maxY = Math.max(...nodes.map((node) => node.position.y + node.height));
    const width = Math.max(1, maxX - minX);
    const height = Math.max(1, maxY - minY);
    const scale = Math.min(1, viewport.width / width, viewport.height / height);
    return {
      x: viewport.x + (viewport.width - width * scale) / 2 - minX * scale,
      y: viewport.y + (viewport.height - height * scale) / 2 - minY * scale,
      scale,
    };
  }

  return {
    DataSourceError,
    MockDataSource,
    HttpDataSource,
    assertEnvelope,
    selectDataSourceMode,
    createDataSource,
    errorRecord,
    describeDataSource,
    createIdempotencyKey,
    createRouteRefreshCoordinator,
    readM0RouteBundle,
    discoveryRequestForConnection,
    discoveryRunOutcome,
    projectSnapshotFromProjection,
    hostWorkspacePointersFromView,
    fitGraphToViewport,
  };
});
