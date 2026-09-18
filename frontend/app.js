const app = document.querySelector("#app");
const toast = document.querySelector("#toast");
const dataApi = window.NetworkAtlasData;

if (!dataApi) throw new Error("NetworkAtlasData 必须先于 app.js 加载");

const projects = {
  hermes: { label: "Hermes", subtitle: "消息网关与 Agent", health: "健康", tone: "green", activity: 2, alerts: 0 },
  automation: { label: "自动化补池", subtitle: "自动化工作流", health: "健康", tone: "green", activity: 1, alerts: 0 },
  knowledge: { label: "知识整理", subtitle: "知识索引与同步", health: "待核验", tone: "amber", activity: 0, alerts: 1 },
  lab: { label: "实验项目", subtitle: "等待接入", health: "未接入", tone: "muted", activity: 0, alerts: 0 },
};

const resources = {
  "srv-01": { label: "SRV-01", type: "服务器", scope: "共享资源", health: "健康", tone: "green", projects: ["hermes", "automation"], freshness: "19 秒前", source: "HOST_REF", relation: "承载运行服务", meta: "2 项目 · 3 个工作负载" },
  "storage-a": { label: "STORAGE_A", type: "对象存储", scope: "共享资源", health: "健康", tone: "green", projects: ["hermes", "automation", "knowledge"], freshness: "43 秒前", source: "STORAGE_REF", relation: "保存产物与索引", meta: "3 项目 · 4 个绑定" },
  "index-shared": { label: "KNOWLEDGE_INDEX", type: "共享索引", scope: "共享资源", health: "待核验", tone: "amber", projects: ["knowledge", "hermes"], freshness: "8 分钟前", source: "INDEX_REF", relation: "提供查询入口", meta: "2 项目 · 1 项待核验" },
  "domain-h": { label: "DOMAIN_REF", type: "域名入口", scope: "Hermes 私有", health: "健康", tone: "green", projects: ["hermes"], freshness: "2 分钟前", source: "DOMAIN_REF", relation: "指向消息入口", meta: "项目私有" },
  "repo-h": { label: "CODE_REPO", type: "代码仓库", scope: "Hermes 私有", health: "已同步", tone: "green", projects: ["hermes"], freshness: "6 分钟前", source: "REPO_REF", relation: "构建来源", meta: "主分支已同步" },
  "knowledge-h": { label: "KNOWLEDGE_SOURCE", type: "知识源", scope: "Hermes 私有", health: "健康", tone: "green", projects: ["hermes"], freshness: "1 分钟前", source: "KNOWLEDGE_REF", relation: "查询项目知识", meta: "索引可用" },
  "token-h": { label: "TOKEN_REF", type: "凭据引用", scope: "Hermes 私有", health: "有效", tone: "green", projects: ["hermes"], freshness: "12 分钟前", source: "SECRET_REF", relation: "授权观察能力", meta: "仅显示引用状态" },
  "domain-a": { label: "DOMAIN_REF_B", type: "服务入口", scope: "自动化补池私有", health: "健康", tone: "green", projects: ["automation"], freshness: "3 分钟前", source: "DOMAIN_REF", relation: "触发工作流", meta: "项目私有" },
  "repo-a": { label: "WORKFLOW_REPO", type: "代码仓库", scope: "自动化补池私有", health: "已同步", tone: "green", projects: ["automation"], freshness: "9 分钟前", source: "REPO_REF", relation: "脚本与流程来源", meta: "主分支已同步" },
  "queue-a": { label: "QUEUE_REF", type: "任务队列", scope: "自动化补池私有", health: "健康", tone: "green", projects: ["automation"], freshness: "22 秒前", source: "QUEUE_REF", relation: "保存待执行任务", meta: "队列深度 4" },
  "token-a": { label: "TOKEN_REF_B", type: "凭据引用", scope: "自动化补池私有", health: "有效", tone: "green", projects: ["automation"], freshness: "14 分钟前", source: "SECRET_REF", relation: "授权执行能力", meta: "仅显示引用状态" },
  "domain-k": { label: "QUERY_ENDPOINT", type: "查询入口", scope: "知识整理私有", health: "健康", tone: "green", projects: ["knowledge"], freshness: "4 分钟前", source: "ENDPOINT_REF", relation: "提供查询", meta: "项目私有" },
  "repo-k": { label: "INDEX_REPO", type: "代码仓库", scope: "知识整理私有", health: "已同步", tone: "green", projects: ["knowledge"], freshness: "18 分钟前", source: "REPO_REF", relation: "索引代码来源", meta: "项目私有" },
  "vault-k": { label: "VAULT_REF", type: "知识存储", scope: "知识整理私有", health: "待核验", tone: "amber", projects: ["knowledge"], freshness: "8 分钟前", source: "VAULT_REF", relation: "保存知识文件", meta: "连接器待核验" },
  "token-k": { label: "TOKEN_REF_C", type: "凭据引用", scope: "知识整理私有", health: "有效", tone: "green", projects: ["knowledge"], freshness: "21 分钟前", source: "SECRET_REF", relation: "授权查询能力", meta: "仅显示引用状态" },
};

const projectResources = {
  hermes: { privateResources: ["domain-h", "repo-h", "knowledge-h", "token-h"], sharedResources: ["srv-01", "storage-a"] },
  automation: { privateResources: ["domain-a", "repo-a", "queue-a", "token-a"], sharedResources: ["srv-01", "storage-a"] },
  knowledge: { privateResources: ["domain-k", "repo-k", "vault-k", "token-k"], sharedResources: ["storage-a", "index-shared"] },
  lab: { privateResources: [], sharedResources: [] },
};

const worldNodes = [
  { id: "coordinator", label: "业务统筹 Agent", subtitle: "全局协调与监控", kind: "coordinator", x: 395, y: 246, width: 220, height: 104, status: "已同步", tone: "green", activity: 4, alerts: 1, updated: "刚刚", summary: "系统内置的跨项目协调角色，负责整理已有项目、维护全局投影并解释异常。", facts: [["项目", "4"], ["健康", "2"], ["需关注", "1"], ["未接入", "1"]] },
  { id: "hermes", label: "Hermes", subtitle: "消息网关与 Agent", kind: "project", x: 72, y: 104, width: 215, height: 100, status: "健康", tone: "green", activity: 2, alerts: 0, updated: "3 分钟前", summary: "首个登记项目。项目内部的流程、资源、代码与项目 Agent 进入项目后再展开。", facts: [["活动", "2"], ["异常", "0"], ["资源", "6"], ["共享", "2"]] },
  { id: "automation", label: "自动化补池", subtitle: "自动化工作流", kind: "project", x: 715, y: 104, width: 215, height: 100, status: "健康", tone: "green", activity: 1, alerts: 0, updated: "7 分钟前", summary: "用于补齐自动化能力的项目投影；当前所有状态都来自本地样本。", facts: [["活动", "1"], ["异常", "0"], ["资源", "6"], ["共享", "2"]] },
  { id: "knowledge", label: "知识整理", subtitle: "知识索引与同步", kind: "project", x: 92, y: 414, width: 215, height: 100, status: "待核验", tone: "amber", activity: 0, alerts: 1, updated: "8 分钟前", summary: "知识索引连接器等待核验；当前不把等待回执显示为正常。", facts: [["活动", "0"], ["异常", "1"], ["资源", "6"], ["共享", "2"]] },
  { id: "lab", label: "实验项目", subtitle: "等待接入", kind: "project", x: 702, y: 414, width: 215, height: 100, status: "未接入", tone: "muted", activity: 0, alerts: 0, updated: "尚无数据", summary: "项目容器已预留，但尚未有可核验的观察入口。", facts: [["活动", "0"], ["异常", "-"], ["资源", "0"], ["入口", "待登记"]] },
];

const worldEdges = [
  { id: "hermes-sync", from: "hermes", to: "coordinator", tone: "sync", animated: true, label: "状态同步" },
  { id: "automation-sync", from: "automation", to: "coordinator", tone: "sync", animated: true, label: "状态同步" },
  { id: "knowledge-alert", from: "knowledge", to: "coordinator", tone: "attention", animated: true, label: "待核验" },
  { id: "lab-link", from: "lab", to: "coordinator", tone: "dashed", animated: false, label: "待接入" },
];

const agentOperations = [
  { time: "10:42", project: "hermes", title: "同步项目状态", target: "Hermes", result: "已验证", tone: "green" },
  { time: "10:40", project: "automation", title: "更新监控绑定", target: "自动化补池", result: "仅本地", tone: "local" },
  { time: "10:36", project: "knowledge", title: "请求索引核验", target: "KNOWLEDGE_INDEX", result: "待回执", tone: "pending" },
  { time: "10:31", project: "hermes", title: "记录流程草稿", target: "消息处理流程", result: "待确认", tone: "pending" },
  { time: "10:26", project: "lab", title: "检查接入入口", target: "实验项目", result: "失败", tone: "failed" },
];

// 快捷动作只表达当前业务统筹 Agent 可调用的观察/准备请求；有外部副作用的动作留到确认流。
function quickActionsForTarget(target) {
  if (target.kind === "project") {
    const project = projects[target.id];
    return [
      { id: "sync-project", icon: "refresh-cw", label: `同步 ${project.label} 状态`, mode: "observe", modeLabel: "只读", hint: "健康 · 活动 · 新鲜度", target: project.label, result: "本地请求", tone: "local", reply: `已为 ${project.label} 生成状态同步请求；当前页面只更新本地投影，外部回执仍需适配器返回。` },
      { id: "check-dependencies", icon: "scan-search", label: "核验外部依赖", mode: "observe", modeLabel: "只读", hint: "服务器 · 域名 · 知识库", target: `${project.label} 外部上下文`, result: project.id === "knowledge" ? "待回执" : "已整理", tone: project.id === "knowledge" ? "pending" : "local", reply: `${project.label} 的服务器、域名、代码和知识库仍按项目边界展开；我已整理核验范围，不把资源伪装成流程步骤。` },
      { id: "summarize-run", icon: "list-checks", label: "汇总最近运行", mode: "observe", modeLabel: "只读", hint: "步骤 · 耗时 · 外部回执", target: `${project.label} 运行轨迹`, result: projectRunTimelines[project.id]?.length ? "已整理" : "暂无数据", tone: projectRunTimelines[project.id]?.length ? "local" : "pending", reply: projectRunTimelines[project.id]?.length ? `最近一次 ${project.label} 运行轨迹已按步骤、耗时和回执整理；当前运行事实保持只读。` : `${project.label} 尚未捕获运行轨迹，我不会借用其他项目的实例数据。` },
      { id: "draft-brief", icon: "file-text", label: "生成项目简报", mode: "prepare", modeLabel: "草稿", hint: "状态 · 风险 · 下一步", target: `${project.label} 状态简报`, result: "已生成", tone: "local", reply: `已生成 ${project.label} 的本地状态简报草稿，包含健康、活动、异常和待核验项；尚未向外部系统发起写操作。` },
    ];
  }
  return [
    { id: "sync-global", icon: "refresh-cw", label: "同步项目状态", mode: "observe", modeLabel: "只读", hint: "4 个项目 · 健康与活动", target: "4 个项目", result: "本地请求", tone: "local", reply: "已为 4 个项目生成状态同步请求；知识整理仍标记为待核验，实验项目保持未接入。" },
    { id: "attention-summary", icon: "triangle-alert", label: "汇总需关注项目", mode: "observe", modeLabel: "只读", hint: "异常 · 待核验 · 未接入", target: "需关注清单", result: "已整理", tone: "local", reply: "当前需关注 1 个对象：知识整理的索引连接器待核验；实验项目仍等待接入入口。" },
    { id: "check-shared", icon: "share-2", label: "核验共享资源", mode: "observe", modeLabel: "只读", hint: "服务器 · 存储 · 共享索引", target: "共享资源集合", result: "待回执", tone: "pending", reply: "已整理共享资源核验范围：SRV-01、STORAGE_A 与 KNOWLEDGE_INDEX；KNOWLEDGE_INDEX 仍等待外部回执。" },
    { id: "draft-global-brief", icon: "file-bar-chart", label: "生成全局简报", mode: "prepare", modeLabel: "草稿", hint: "项目状态 · 资源影响 · 风险", target: "全局状态简报", result: "已生成", tone: "local", reply: "已生成全局状态简报草稿，覆盖 4 个项目、共享资源影响和待核验事项；结果仅写入本地会话。" },
  ];
}

// 项目运行轨迹是“这一次怎样跑”的事实层；外部依赖仍保持在流程之外。
const projectRunTimelines = {
  hermes: [
    { id: "trace-1", node: "trigger", time: "10:42:01", title: "消息触发", detail: "收到合法消息", duration: "18 ms", receipt: "connector#msg-884", state: "完成", tone: "green" },
    { id: "trace-2", node: "understand", time: "10:42:02", title: "理解请求", detail: "识别为状态查询", duration: "420 ms", receipt: "intent=inspect", state: "完成", tone: "green" },
    { id: "trace-3", node: "route", time: "10:42:03", title: "路由决策", detail: "命中低影响路径", duration: "31 ms", receipt: "branch=execute", state: "完成", tone: "green" },
    { id: "trace-4", node: "approval", time: "10:42:03", title: "人工确认", detail: "本次条件未命中", duration: "0 ms", receipt: "skipped", state: "跳过", tone: "muted" },
    { id: "trace-5", node: "execute", time: "10:42:03", title: "调用能力", detail: "请求已发出", duration: "1.8 s", receipt: "srv-01 / 202", state: "完成", tone: "green" },
    { id: "trace-6", node: "result", time: "10:42:05", title: "验证结果", detail: "等待外部回执", duration: "12 s", receipt: "pending", state: "当前", tone: "amber" },
  ],
};

const coordinatorInitialChat = [
  { role: "agent", text: "已同步 4 个项目。你可以问我某个项目的状态、资源影响或最近一次运行。", time: "现在" },
  { role: "agent", text: "当前有 1 个对象需要关注：知识整理的索引连接器待核验。", time: "10:42" },
];

const workflowBase = [
  { id: "trigger", kind: "TRIGGER", title: "消息触发", subtitle: "收到合法消息", x: 44, y: 260, width: 148, height: 74 },
  { id: "understand", kind: "AGENT", title: "理解请求", subtitle: "识别意图与项目", x: 220, y: 260, width: 148, height: 74 },
  { id: "route", kind: "CONDITION", title: "路由决策", subtitle: "低影响 / 高影响", x: 397, y: 260, width: 148, height: 74 },
  { id: "approval", kind: "APPROVAL", title: "人工确认", subtitle: "高影响动作进入", x: 585, y: 94, width: 150, height: 74 },
  { id: "execute", kind: "ADAPTER", title: "调用能力", subtitle: "读取或发起请求", x: 585, y: 410, width: 150, height: 74 },
  { id: "result", kind: "VERIFY", title: "验证结果", subtitle: "重读外部状态", x: 806, y: 260, width: 150, height: 74 },
];

const workflowSteps = {
  trigger: { definition: ["入口条件", "收到合法消息", "连接器声明"], run: ["10:42:01", "18 ms", "已接收"] },
  understand: { definition: ["Agent 步骤", "识别意图与项目", "失败进入人工确认"], run: ["10:42:02", "420 ms", "识别为状态查询"] },
  route: { definition: ["条件分支", "低影响自动执行", "高影响进入审批"], run: ["10:42:03", "31 ms", "命中自动路径"] },
  approval: { definition: ["控制闸门", "仅高影响动作进入", "确认后继续"], run: ["本次跳过", "0 ms", "条件未命中"] },
  execute: { definition: ["适配器动作", "读取状态或发起请求", "等待外部回执"], run: ["10:42:03", "1.8 s", "请求已发出"] },
  result: { definition: ["完成条件", "重新读取外部状态", "失败进入恢复路径"], run: ["当前步骤", "已等待 12 s", "等待外部回执"] },
};

const runStates = {
  "run-028": { label: "RUN-028", version: "v3", status: "等待回执", elapsed: "14.3 s", path: ["trigger", "understand", "route", "execute", "result"], current: "result", skipped: ["approval"] },
  "run-027": { label: "RUN-027", version: "v3", status: "已验证", elapsed: "8.7 s", path: ["trigger", "understand", "route", "execute", "result"], current: null, skipped: ["approval"] },
};

const paletteKinds = {
  task: { label: "任务", icon: "square-check-big", kind: "TASK" },
  condition: { label: "条件", icon: "git-branch", kind: "CONDITION" },
  approval: { label: "确认", icon: "badge-check", kind: "APPROVAL" },
  result: { label: "结果", icon: "circle-check", kind: "VERIFY" },
};

const workflowNames = {
  hermes: "消息处理流程",
  automation: "补池执行流程",
  knowledge: "索引同步流程",
  lab: "接入准备流程",
};

const monitorTerminalStates = new Set([
  "succeeded",
  "partial",
  "failed",
  "timed_out",
  "skipped_overlap",
  "interrupted",
]);

const hashParams = new URLSearchParams(location.hash.slice(1));
const hashView = hashParams.get("view");
const hashProject = hashParams.get("project");
const hashScope = hashParams.get("scope");
const initialScope = hashScope === "project" || hashView === "workflow" ? "project" : "global";
const localFixtureResponses = createLocalFixtureResponses();
const primaryDataSource = dataApi.createDataSource({
  location: window.location,
  mockResponses: localFixtureResponses,
});
const state = {
  scope: initialScope,
  view: initialScope === "project"
    ? (hashView === "resource" ? "resource" : "workflow")
    : (hashView === "resource" ? "resource" : hashView === "hosts" ? "hosts" : "world"),
  activeProject: hashProject || "hermes",
  selectedWorldId: "coordinator",
  operationProject: "all",
  operationLog: agentOperations.map((operation) => ({ ...operation })),
  worldPan: { x: 0, y: 0 },
  worldZoom: 1,
  workflowMode: "run",
  workflowVersion: 3,
  draftChanges: 0,
  workflowNodes: cloneNodes(workflowBase),
  runSnapshot: cloneNodes(workflowBase),
  selectedFlowNode: "route",
  selectedRun: "run-028",
  flowPan: { x: 0, y: 0 },
  flowZoom: 1,
  resourceLens: initialScope === "project" ? "project" : "shared",
  resourceProject: hashProject || "hermes",
  selectedResource: "srv-01",
  resourcePan: { x: 0, y: 0 },
  resourceZoom: 1,
  contextOpen: false,
  coordinatorChat: coordinatorInitialChat.map((message) => ({ ...message })),
  selectedTrace: "trace-6",
  navigation: { projects: 4, healthy: 2, attention: 1, unassigned: 1 },
  features: {
    global_world: true,
    project_resources: true,
    global_resources: false,
    project_workflow: false,
    agent_assistance: false,
  },
  dataClient: primaryDataSource,
  auth: {
    checked: false,
    enabled: false,
    authenticated: false,
    username: null,
    expiresAt: null,
    busy: false,
    error: null,
  },
  events: {
    source: null,
    connected: false,
    lastEventId: null,
    refreshTimer: null,
  },
  m4: {
    modal: false,
    busy: false,
    error: null,
    pendingDelete: null,
  },
  projectSnapshots: {},
  hosts: [],
  hostsView: null,
  contractMeta: {
    globalWorld: null,
    globalHosts: null,
  },
  selectedHostId: null,
  dataSource: {
    transport: primaryDataSource.transport,
    kind: "fixture",
    status: "stale",
    freshness: "stale",
    label: primaryDataSource.label,
    generatedAt: null,
    revision: 0,
    loading: false,
    fallback: false,
    error: null,
  },
  m2: {
    draft: null,
    workspaceByHost: {},
    restoreToken: 0,
    selectedNodeId: null,
    setup: null,
    modal: null,
    busy: false,
  },
  monitoring: {
    byHost: {},
    busyHostId: null,
    restoreToken: 0,
    errorByHost: {},
  },
  m3: {
    model: null,
    modelTest: null,
    session: null,
    diff: null,
    busy: false,
    error: null,
  },
  drag: null,
  panDrag: null,
  renderDeferred: false,
  suppressCanvasClick: false,
};
const m0RefreshCoordinator = dataApi.createRouteRefreshCoordinator(() => m0RouteKey(currentM0Route()));

function cloneNodes(nodes) {
  return nodes.map((node) => ({ ...node }));
}

function createLocalFixtureResponses() {
  const generatedAt = new Date().toISOString();
  const sourceRefs = ["fixture:browser-m0"];
  const meta = () => ({
    request_id: `browser-fixture-${Math.random().toString(16).slice(2)}`,
    revision: 1,
    generated_at: generatedAt,
    freshness: "fresh",
    data_source: { kind: "fixture", status: "fresh", label: "本地浏览器 Fixture" },
  });
  const healthFor = (value) => ({
    label: value.health || value.status || "Fixture",
    tone: value.tone || "muted",
    activity: value.activity || 0,
    alerts: value.alerts || 0,
    updated: value.updated || "固定样本",
  });
  const contractNode = (node) => ({
    id: node.id,
    kind: node.kind === "coordinator" ? "workspace" : "project",
    label: node.label,
    subtitle: node.subtitle,
    state: "fixture",
    source_refs: sourceRefs,
    observed_at: generatedAt,
    position: { x: node.x, y: node.y },
    width: node.width,
    height: node.height,
    summary: node.summary,
    facts: (node.facts || []).map(([label, value]) => ({ label, value })),
    project_id: node.kind === "project" ? node.id : undefined,
    health: healthFor(node),
  });
  const graphKind = (resource) => {
    const label = `${resource.type} ${resource.label}`.toLowerCase();
    if (label.includes("服务器")) return "host";
    if (label.includes("存储") || label.includes("vault")) return "volume";
    if (label.includes("repo") || label.includes("仓库") || label.includes("知识源")) return "document";
    if (label.includes("域名") || label.includes("入口") || label.includes("endpoint")) return "port";
    if (label.includes("网络")) return "network";
    return "service";
  };
  const projectResourceResponses = {};
  const resourcePositions = [
    { x: 285, y: 112 },
    { x: 552, y: 112 },
    { x: 285, y: 398 },
    { x: 552, y: 398 },
    { x: 36, y: 254 },
    { x: 810, y: 254 },
  ];

  Object.entries(projects).forEach(([projectId, project]) => {
    const profile = projectResources[projectId] || { privateResources: [], sharedResources: [] };
    const resourceIds = profile.privateResources.concat(profile.sharedResources);
    const projectNode = {
      id: projectId,
      kind: "project",
      label: project.label,
      subtitle: project.subtitle,
      state: "fixture",
      source_refs: sourceRefs,
      observed_at: generatedAt,
      position: { x: 408, y: 243 },
      width: 184,
      height: 78,
      summary: "本地视觉基线 Fixture",
      facts: [{ label: "来源", value: "本地浏览器 Fixture" }],
      project_id: projectId,
      health: healthFor(project),
    };
    const nodes = [projectNode, ...resourceIds.map((resourceId, index) => {
      const resource = resources[resourceId];
      const position = resourcePositions[index] || { x: 420 + (index % 3) * 170, y: 90 + Math.floor(index / 3) * 150 };
      return {
        id: resourceId,
        kind: graphKind(resource),
        label: resource.label,
        subtitle: resource.meta,
        state: "fixture",
        source_refs: sourceRefs,
        observed_at: generatedAt,
        position,
        width: index >= 4 ? 154 : 154,
        height: index >= 4 ? 76 : 72,
        summary: resource.relation,
        facts: [
          { label: "类型", value: resource.type },
          { label: "范围", value: resource.scope },
          { label: "来源", value: resource.source },
        ],
        project_id: projectId,
        health: healthFor(resource),
      };
    })];
    projectResourceResponses[projectId] = {
      data: {
        focus: { kind: "project", id: projectId },
        nodes,
        edges: resourceIds.map((resourceId) => ({
          id: `${projectId}-uses-${resourceId}`,
          from: projectId,
          to: resourceId,
          kind: "depends_on",
          label: "使用",
          state: "fixture",
          source_refs: sourceRefs,
        })),
        layout: { layout_id: `layout-browser-${projectId}`, scope: `project:${projectId}`, revision: 1 },
      },
      meta: meta(),
    };
  });

  return {
    bootstrap: {
      data: {
        workspace_id: "workspace",
        projects: Object.entries(projects).map(([projectId, project]) => ({
          project_id: projectId,
          label: project.label,
          subtitle: project.subtitle,
          state: "fixture",
          health: project.health,
          tone: project.tone,
          activity: project.activity,
          alerts: project.alerts,
        })),
        hosts: [{ host_id: "browser-fixture", label: "BROWSER_FIXTURE", state: "fixture" }],
        navigation: { projects: Object.keys(projects).length, healthy: 2, attention: 1, unassigned: 1 },
        features: { global_world: true, project_resources: true, global_resources: false, project_workflow: false, agent_assistance: false },
      },
      meta: meta(),
    },
    globalWorld: {
      data: {
        focus: { kind: "global", id: "workspace" },
        nodes: worldNodes.map(contractNode),
        edges: worldEdges.map((edge) => ({
          id: edge.id,
          from: edge.from,
          to: edge.to,
          kind: "contains",
          label: edge.label,
          state: "fixture",
          source_refs: sourceRefs,
        })),
        layout: { layout_id: "layout-browser-global", scope: "global", revision: 1 },
      },
      meta: meta(),
    },
    projectResources: projectResourceResponses,
  };
}

async function initializeApplication() {
  try {
    const session = await state.dataClient.getAuthSession();
    applyAuthSession(session.data);
    renderApp();
    await refreshM0Data({ initial: true });
    startEventStream();
  } catch (error) {
    const record = dataApi.errorRecord(error);
    state.auth = {
      ...state.auth,
      checked: true,
      enabled: state.dataClient.transport === "http",
      authenticated: false,
      busy: false,
      error: record,
    };
    state.dataSource = { ...state.dataSource, loading: false };
    renderApp();
  }
}

function applyAuthSession(session) {
  state.auth = {
    checked: true,
    enabled: Boolean(session.enabled),
    authenticated: Boolean(session.authenticated),
    username: session.username || "owner",
    expiresAt: session.expires_at || null,
    busy: false,
    error: null,
  };
}

function expireOwnerSession(error = null) {
  stopEventStream();
  state.auth = {
    ...state.auth,
    checked: true,
    enabled: true,
    authenticated: false,
    busy: false,
    error: error ? dataApi.errorRecord(error) : null,
  };
  state.m4 = { modal: false, busy: false, error: null, pendingDelete: null };
}

async function submitOwnerLogin(form) {
  const values = Object.fromEntries(new FormData(form).entries());
  const username = String(values.username || "").trim();
  const password = String(values.password || "");
  if (!username || !password) {
    state.auth.error = { code: "LOGIN_FIELDS_REQUIRED", message: "请输入所有者名称和密码" };
    renderApp();
    return;
  }
  state.auth = { ...state.auth, busy: true, error: null };
  renderApp();
  try {
    const session = await state.dataClient.login(username, password);
    applyAuthSession(session.data);
    await refreshM0Data({ initial: true });
    startEventStream();
  } catch (error) {
    state.auth = { ...state.auth, busy: false, error: dataApi.errorRecord(error) };
    renderApp();
  }
}

async function logoutOwner() {
  if (state.auth.busy) return;
  state.auth = { ...state.auth, busy: true, error: null };
  renderApp();
  try {
    await state.dataClient.logout();
  } catch (error) {
    if (error.status !== 401) {
      state.auth = { ...state.auth, busy: false, error: dataApi.errorRecord(error) };
      renderApp();
      return;
    }
  }
  expireOwnerSession();
  renderApp();
}

function startEventStream() {
  stopEventStream();
  if (state.dataClient.transport !== "http" || !state.auth.authenticated || typeof window.EventSource !== "function") return;
  const url = state.dataClient.eventStreamUrl();
  if (!url) return;
  const source = new window.EventSource(url, { withCredentials: true });
  state.events.source = source;
  source.onopen = () => {
    if (state.events.source !== source) return;
    state.events.connected = true;
    renderApp();
  };
  ["host.connection.changed", "discovery.run.changed", "projection.changed", "onboarding.changed"].forEach((kind) => {
    source.addEventListener(kind, (event) => scheduleEventRefresh(event));
  });
  source.addEventListener("stream.reset", (event) => scheduleEventRefresh(event, true));
  source.onerror = () => {
    if (state.events.source !== source) return;
    state.events.connected = false;
    renderApp();
  };
}

function stopEventStream() {
  if (state.events.source) state.events.source.close();
  if (state.events.refreshTimer) window.clearTimeout(state.events.refreshTimer);
  state.events = { source: null, connected: false, lastEventId: state.events.lastEventId, refreshTimer: null };
}

function scheduleEventRefresh(event, reset = false) {
  state.events.lastEventId = event.lastEventId || state.events.lastEventId;
  if (state.events.refreshTimer) window.clearTimeout(state.events.refreshTimer);
  state.events.refreshTimer = window.setTimeout(async () => {
    state.events.refreshTimer = null;
    if (state.dataSource.loading) {
      scheduleEventRefresh(event, reset);
      return;
    }
    await refreshM0Data({ event: true });
    if (reset) showToast("事件游标已更新 · 快照已重新读取");
  }, 180);
}

function currentM4ScopeId(scopeKind) {
  if (scopeKind === "workspace") return "workspace-default";
  if (scopeKind === "host") return state.m2.draft?.host_id || state.m2.setup?.hostId || null;
  if (scopeKind === "project") return state.scope === "project" ? state.activeProject : Object.keys(projects)[0] || null;
  return null;
}

async function exportM4Data(scopeKind) {
  if (state.m4.busy) return;
  const scopeId = currentM4ScopeId(scopeKind);
  if (!scopeId) return;
  state.m4 = { ...state.m4, busy: true, error: null };
  renderApp();
  try {
    const response = scopeKind === "workspace"
      ? await state.dataClient.exportWorkspace()
      : scopeKind === "host"
        ? await state.dataClient.exportHost(scopeId)
        : await state.dataClient.exportProject(scopeId);
    downloadJson(response.data, `network-atlas-${scopeKind}-${safeFilename(scopeId)}.json`);
    state.m4 = { ...state.m4, busy: false, error: null };
    renderApp();
    showToast(`${scopeKind === "workspace" ? "工作区" : scopeKind === "host" ? "HOST" : "项目"}导出已生成`);
  } catch (error) {
    if (error.status === 401) {
      expireOwnerSession(error);
    } else {
      state.m4 = { ...state.m4, busy: false, error: dataApi.errorRecord(error) };
    }
    renderApp();
  }
}

function queueM4Delete(scopeKind, scopeId) {
  if (!scopeId) return;
  const label = scopeKind === "workspace"
    ? "整个工作区"
    : scopeKind === "host"
      ? `HOST · ${scopeId}`
      : `项目 · ${projects[scopeId]?.label || scopeId}`;
  // 删除入口既可从“所有者与数据”打开，也可直接从服务器操作区打开。
  state.m4 = { ...state.m4, modal: true, error: null, pendingDelete: { scopeKind, scopeId, label } };
  renderApp();
}

async function submitM4Delete(form) {
  const values = Object.fromEntries(new FormData(form).entries());
  if (String(values.confirmation || "").trim() !== "删除") {
    state.m4.error = { code: "DELETE_CONFIRMATION_REQUIRED", message: "请输入“删除”后再继续" };
    renderApp();
    return;
  }
  const scopeKind = String(values.scope_kind || "");
  const scopeId = String(values.scope_id || "");
  state.m4 = { ...state.m4, busy: true, error: null };
  renderApp();
  try {
    const key = dataApi.createIdempotencyKey(`delete-${scopeKind}`);
    const response = scopeKind === "workspace"
      ? await state.dataClient.deleteWorkspace(key)
      : scopeKind === "host"
        ? await state.dataClient.deleteHost(scopeId, key)
        : await state.dataClient.deleteProject(scopeId, key);
    const backupRef = response.data.backup_ref;
    if (scopeKind === "workspace") {
      expireOwnerSession();
      renderApp();
      showToast(`工作区已删除 · ${backupRef}`);
      return;
    }
    if (scopeKind === "host") {
      state.m2.draft = null;
      state.m2.setup = null;
      state.m2.modal = null;
      state.selectedHostId = null;
      state.selectedWorldId = null;
    }
    state.m4 = { modal: false, busy: false, error: null, pendingDelete: null };
    await refreshM0Data({ initial: true });
    showToast(`本地${scopeKind === "host" ? " HOST" : "项目"}数据已删除 · ${backupRef}`);
  } catch (error) {
    if (error.status === 401) expireOwnerSession(error);
    else state.m4 = { ...state.m4, busy: false, error: dataApi.errorRecord(error) };
    renderApp();
  }
}

function downloadJson(value, filename) {
  const blob = new Blob([JSON.stringify(value, null, 2)], { type: "application/json;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 0);
}

function safeFilename(value) {
  return String(value).replace(/[^a-zA-Z0-9._-]+/g, "-").slice(0, 96) || "export";
}

function currentM0Route() {
  return {
    scope: state.scope,
    view: state.view,
    projectId: state.scope === "project" ? state.activeProject : null,
  };
}

function m0RouteKey(route) {
  return JSON.stringify([route.scope, route.view, route.projectId || null]);
}

async function readM0Bundle(source, route = currentM0Route()) {
  return dataApi.readM0RouteBundle(source, route);
}

function applyM0Bundle(bundle) {
  state.contractMeta = {
    globalWorld: bundle.globalWorld?.meta || null,
    globalHosts: bundle.hostsView?.meta || null,
  };
  applyBootstrap(bundle.bootstrap.data);
  applyHosts(bundle.hosts?.data || []);
  state.hostsView = bundle.hostsView?.data || null;
  applyHostMonitoringFromAssets(state.hostsView);
  state.m2.workspaceByHost = dataApi.hostWorkspacePointersFromView(state.hostsView);
  if (bundle.globalWorld) {
    applyGlobalSnapshot(bundle.globalWorld.data);
  } else if (state.dataClient.transport === "http") {
    worldNodes.splice(0, worldNodes.length);
    worldEdges.splice(0, worldEdges.length);
    worldNodesInitial.splice(0, worldNodesInitial.length);
    state.selectedWorldId = null;
  }
  clearContractResourceState();
  Object.entries(bundle.projectResources).forEach(([projectId, response]) => applyProjectSnapshot(projectId, response.data));
}

function applyHostMonitoringFromAssets(hostsView) {
  (hostsView?.hosts || []).forEach((asset) => {
    const hostId = asset?.host?.host_id;
    if (!hostId) return;
    const previous = state.monitoring.byHost[hostId] || {};
    const assetSnapshot = asset.current_snapshot;
    const hasSnapshotField = Object.prototype.hasOwnProperty.call(asset, "current_snapshot");
    const fullSnapshot = assetSnapshot?.cpu && assetSnapshot?.memory && assetSnapshot?.load ? assetSnapshot : null;
    state.monitoring.byHost[hostId] = {
      latestRun: asset.latest_monitor_run || null,
      currentSnapshot: fullSnapshot || (!hasSnapshotField ? previous.currentSnapshot || null : null),
      currentSnapshotSummary: {
        freshness: asset.monitor_freshness || assetSnapshot?.freshness || previous.currentSnapshotSummary?.freshness || "unknown",
        unknownCount: asset.monitor_unknown_count ?? assetSnapshot?.unknown_count ?? previous.currentSnapshotSummary?.unknownCount ?? null,
        observedAt: assetSnapshot?.observed_at || asset.last_observed_at || previous.currentSnapshotSummary?.observedAt || null,
        runId: assetSnapshot?.run_id || previous.currentSnapshotSummary?.runId || null,
      },
      meta: previous.meta || null,
    };
  });
}

function applyHosts(hosts) {
  state.hosts = hosts.map((host) => ({ ...host }));
  if (!state.hosts.some((host) => host.host_id === state.selectedHostId)) {
    state.selectedHostId = state.hosts[0]?.host_id || null;
  }
  if (state.scope === "global" && state.view === "hosts" && state.selectedHostId) {
    state.selectedWorldId = state.selectedHostId;
  }
}

function clearContractResourceState() {
  Object.keys(resources).forEach((id) => {
    if (state.dataClient.transport === "http" || resources[id]._contractManaged) delete resources[id];
  });
  Object.keys(projectResources).forEach((id) => delete projectResources[id]);
  state.projectSnapshots = {};
}

function applyBootstrap(bootstrap) {
  const nextProjects = Object.fromEntries(bootstrap.projects.map((project) => [project.project_id, {
    label: project.label,
    subtitle: project.subtitle,
    health: project.health,
    tone: project.tone,
    activity: project.activity,
    alerts: project.alerts,
    state: project.state,
  }]));
  Object.keys(projects).forEach((id) => delete projects[id]);
  Object.assign(projects, nextProjects);
  state.navigation = { ...bootstrap.navigation };
  state.features = { ...state.features, ...bootstrap.features };
  if (!projects[state.activeProject]) state.activeProject = Object.keys(projects)[0];
  if (!projects[state.resourceProject]) state.resourceProject = state.activeProject;
}

function applyGlobalSnapshot(snapshot) {
  const nextNodes = snapshot.nodes.map((node) => {
    const project = projects[node.project_id || node.id];
    const health = project || node.health || {};
    return {
      id: node.id,
      label: node.label,
      subtitle: node.subtitle || graphKindLabel(node.kind),
      kind: node.kind === "workspace" ? "coordinator" : node.kind,
      x: node.position.x,
      y: node.position.y,
      width: node.width,
      height: node.height,
      status: health.health || health.label || projectionStateLabel(node.state),
      tone: health.tone || projectionTone(node.state),
      activity: health.activity || 0,
      alerts: health.alerts || 0,
      updated: health.updated || formatObservedAt(node.observed_at),
      summary: node.summary || "当前节点来自统一图快照契约。",
      facts: (node.facts || []).map((fact) => [fact.label, fact.value]),
      sourceRefs: node.source_refs || [],
      originalLabel: (node.facts || []).find((fact) => fact.label === "原始名称")?.value || node.label,
      observedAt: node.observed_at,
      projectId: node.project_id || null,
      projectionState: node.state,
    };
  });
  const initialNodes = cloneNodes(nextNodes);
  const nextEdges = snapshot.edges.map((edge) => {
    const projectId = edge.from === "coordinator" ? edge.to : edge.from;
    const project = projects[projectId];
    const tone = state.dataSource.kind !== "real"
      ? (project?.alerts ? "attention" : project?.tone === "muted" ? "dashed" : "sync")
      : edge.state === "archived"
        ? "dashed"
        : edge.kind === "depends_on" || edge.kind === "connects_to"
          ? "sync"
          : "resource-usage";
    return {
      id: edge.id,
      from: edge.from,
      to: edge.to,
      tone,
      animated: state.dataSource.kind !== "real" && tone !== "dashed" && state.dataSource.status !== "unavailable",
      label: edge.label,
      state: edge.state,
      sourceRefs: edge.source_refs || [],
    };
  });
  worldNodes.splice(0, worldNodes.length, ...nextNodes);
  worldEdges.splice(0, worldEdges.length, ...nextEdges);
  // Keep the server-provided baseline separate from the local canvas layout;
  // the Reset control must be able to return to the latest snapshot.
  worldNodesInitial.splice(0, worldNodesInitial.length, ...initialNodes);
  if (!worldNodes.some((node) => node.id === state.selectedWorldId)) state.selectedWorldId = worldNodes[0]?.id;
}

function applyProjectionProjects(snapshot) {
  const projectionProjects = snapshot.nodes.filter((node) => node.kind === "project");
  const nextProjects = Object.fromEntries(projectionProjects.map((node) => [node.id, {
    label: node.label,
    subtitle: node.subtitle || "本地项目投影",
    health: node.health?.label || projectionStateLabel(node.state),
    tone: node.health?.tone || projectionTone(node.state),
    activity: snapshot.nodes.filter((candidate) => candidate.project_id === node.id).length,
    alerts: 0,
    state: node.state,
  }]));
  Object.keys(projects).forEach((id) => delete projects[id]);
  Object.assign(projects, nextProjects);
  state.navigation = {
    projects: projectionProjects.length,
    healthy: projectionProjects.filter((node) => node.state === "confirmed").length,
    attention: projectionProjects.filter((node) => node.state !== "confirmed").length,
    unassigned: snapshot.nodes.filter((node) => !["host", "project"].includes(node.kind) && !node.project_id).length,
  };
  if (!projects[state.activeProject]) state.activeProject = Object.keys(projects)[0] || null;
  if (!projects[state.resourceProject]) state.resourceProject = state.activeProject;
}

function applyM2Draft(response, { applySnapshot = true } = {}) {
  const draft = response.data;
  if (state.m2.draft?.draft_id !== draft.draft_id) {
    state.m3.session = null;
    state.m3.diff = null;
    state.m3.error = null;
  }
  state.m2.draft = draft;
  if (!(draft.nodes || []).some((node) => node.id === state.m2.selectedNodeId)) {
    state.m2.selectedNodeId = (draft.nodes || []).find((node) => !["host", "workspace"].includes(node.kind))?.id
      || (draft.nodes || []).find((node) => node.kind === "host")?.id
      || draft.nodes?.[0]?.id
      || null;
  }
  state.m2.setup = state.m2.setup ? { ...state.m2.setup, draftId: draft.draft_id } : null;
  if (!applySnapshot) return;
  state.dataSource = {
    ...state.dataSource,
    kind: response.meta?.data_source?.kind || "real",
    status: response.meta?.data_source?.status || "fresh",
    freshness: response.meta?.freshness || "fresh",
    label: response.meta?.data_source?.label || "SSH · Linux · 确定性投影",
    generatedAt: response.meta?.generated_at || new Date().toISOString(),
    revision: draft.revision,
    fallback: false,
    error: null,
  };
  applyProjectionProjects(draft);
  applyProjectionResourceSnapshots(draft);
  applyGlobalSnapshot(draft);
}

function applyM2Version(response) {
  const version = response.data;
  const prior = state.m2.draft || {};
  state.m2.draft = {
    ...prior,
    ...version,
    state: "confirmed",
    pending_changes: 0,
    updated_at: version.confirmed_at,
  };
  state.dataSource = {
    ...state.dataSource,
    kind: response.meta?.data_source?.kind || "real",
    status: response.meta?.data_source?.status || "fresh",
    freshness: response.meta?.freshness || "fresh",
    label: response.meta?.data_source?.label || "SSH · Linux · 确定性投影",
    generatedAt: response.meta?.generated_at || new Date().toISOString(),
    revision: version.revision,
    fallback: false,
    error: null,
  };
  applyProjectionProjects(version);
  applyProjectionResourceSnapshots(version);
  applyGlobalSnapshot(version);
}

function applyProjectionResourceSnapshots(snapshot) {
  clearContractResourceState();
  snapshot.nodes.filter((node) => node.kind === "project").forEach((project) => {
    applyProjectSnapshot(project.id, dataApi.projectSnapshotFromProjection(snapshot, project.id));
  });
}

function applyProjectSnapshot(projectId, snapshot) {
  state.projectSnapshots[projectId] = snapshot;
  const privateResources = [];
  const sharedResources = [];
  snapshot.nodes.filter((node) => node.kind !== "project" || node.id !== projectId).forEach((node) => {
    const facts = Object.fromEntries((node.facts || []).map((fact) => [fact.label, fact.value]));
    const shared = facts["范围"] === "共享资源";
    const current = resources[node.id];
    const projectIds = Array.from(new Set([...(current?._contractManaged ? current.projects : []), node.project_id || projectId]));
    resources[node.id] = {
      label: node.label,
      type: facts["类型"] || graphKindLabel(node.kind),
      scope: shared || projectIds.length > 1 ? "共享资源" : `${projects[projectId]?.label || projectId} 私有`,
      health: node.health?.label || projectionStateLabel(node.state),
      tone: node.health?.tone || projectionTone(node.state),
      projects: projectIds,
      freshness: formatObservedAt(node.observed_at),
      source: node.source_refs?.[0] || facts["来源"] || "UNKNOWN_SOURCE",
      relation: node.summary || "来自图快照契约",
      meta: node.subtitle || graphKindLabel(node.kind),
      kind: node.kind,
      _contractManaged: true,
    };
    (shared ? sharedResources : privateResources).push(node.id);
  });
  projectResources[projectId] = { privateResources, sharedResources };
}

async function refreshM0Data(options = {}) {
  const route = currentM0Route();
  const routeKey = m0RouteKey(route);
  state.dataSource = { ...state.dataSource, loading: true, error: null, routeKey };
  renderApp();
  return m0RefreshCoordinator.request(routeKey, async (request) => {
    try {
      const bundle = await readM0Bundle(state.dataClient, route);
      applyM0Bundle(bundle);
      if (!request.isCurrent()) return;
      if (bundle.routeError) throw bundle.routeError;
      const meta = route.scope === "global" && route.view === "hosts"
        ? bundle.hostsView?.meta || bundle.bootstrap.meta
        : route.scope === "project" && route.view === "resource"
          ? bundle.projectResources[route.projectId]?.meta || bundle.bootstrap.meta
          : bundle.globalWorld?.meta || bundle.bootstrap.meta;
      await restoreM2Projection(bundle);
      if (!request.isCurrent()) return;
      if (route.scope === "global" && route.view === "hosts" && state.selectedHostId) {
        await loadHostMonitoring(state.selectedHostId, { render: false });
      }
      if (!request.isCurrent()) return;
      await refreshM3Model();
      if (!request.isCurrent()) return;
      state.dataSource = {
        transport: state.dataClient.transport,
        kind: meta.data_source.kind,
        status: meta.data_source.status,
        freshness: meta.freshness,
        label: meta.data_source.label,
        generatedAt: meta.generated_at,
        revision: meta.revision,
        loading: false,
        fallback: false,
        error: null,
        routeKey,
      };
      if (!options.initial) showToast(`投影已刷新 · ${meta.data_source.kind} · ${meta.freshness}`);
    } catch (error) {
      if (!request.isCurrent()) return;
      const record = dataApi.errorRecord(error);
      if (state.dataClient.transport === "http" && error?.status === 401) {
        expireOwnerSession(error);
        state.dataSource = { ...state.dataSource, loading: false, routeKey };
        renderApp();
        return;
      }
      if (state.dataClient.transport === "http") {
        state.dataSource = {
          ...state.dataSource,
          transport: "http",
          kind: "real",
          status: "unavailable",
          freshness: "unavailable",
          label: "真实数据 API 暂不可用",
          loading: false,
          fallback: false,
          error: record,
          routeKey,
        };
      } else {
        state.dataSource = {
          ...state.dataSource,
          status: "unavailable",
          freshness: "unavailable",
          loading: false,
          error: record,
          routeKey,
        };
      }
      if (!options.initial) showToast(`刷新失败 · ${record.code}`);
    }
    if (request.isCurrent()) renderApp();
  });
}

async function restoreM2Projection(bundle) {
  if (state.dataClient.transport !== "http" || bundle.hostsView?.meta?.data_source?.kind !== "real") {
    state.m2.draft = null;
    return;
  }
  const hostId = state.selectedHostId || bundle.hostsView.data.hosts?.[0]?.host?.host_id || null;
  await restoreM2ProjectionForHost(hostId);
}

async function restoreM2ProjectionForHost(hostId) {
  const token = ++state.m2.restoreToken;
  state.m2.draft = null;
  state.m2.selectedNodeId = null;
  state.m3.session = null;
  state.m3.diff = null;
  state.m3.error = null;
  const workspace = hostId ? state.m2.workspaceByHost[hostId] : null;
  if (!workspace) return;
  let run = null;
  if (workspace.latestDiscoveryRunId) {
    try {
      run = await state.dataClient.getDiscoveryRun(workspace.latestDiscoveryRunId);
    } catch {
      // A missing run summary does not invalidate an independently stored draft pointer.
    }
  }
  if (token !== state.m2.restoreToken || hostId !== state.selectedHostId) return;
  if (!workspace.latestProjectionDraftId) return;
  try {
    const draft = await state.dataClient.getProjectionDraft(workspace.latestProjectionDraftId);
    if (token !== state.m2.restoreToken || hostId !== state.selectedHostId) return;
    applyM2Draft(draft, { applySnapshot: false });
  } catch {
    // Host inventory and controls remain usable without optional projection metadata.
    return;
  }
  if (run?.data?.draft_id === workspace.latestProjectionDraftId) await loadM3ForRun(run.data);
  if (token !== state.m2.restoreToken || hostId !== state.selectedHostId) return;
}

function dataSourceForCurrentView() {
  if (state.dataSource.loading || state.dataSource.error) return state.dataSource;
  const meta = state.scope === "global" && state.view === "hosts"
    ? state.contractMeta.globalHosts
    : state.contractMeta.globalWorld;
  if (!meta?.data_source) return state.dataSource;
  return {
    ...state.dataSource,
    kind: meta.data_source.kind,
    status: meta.data_source.status,
    freshness: meta.freshness,
    label: meta.data_source.label,
    generatedAt: meta.generated_at,
    revision: meta.revision,
  };
}

function dataSourcePresentation() {
  const source = dataSourceForCurrentView();
  if (source.loading) return {
    tone: "loading",
    dotClass: "",
    label: "正在读取投影",
    shortLabel: "DataSource · loading",
    statusLabel: "loading",
    freshnessLabel: "loading",
    detail: "正在读取统一 API 契约",
  };
  if (!currentViewUsesContractData()) return {
    tone: "unavailable",
    dotClass: "amber",
    label: "本地视觉 Fixture",
    shortLabel: "本地 · Fixture · unavailable",
    statusLabel: "unavailable",
    freshnessLabel: "unavailable",
    detail: "该保留视图尚未接入 MVP-1 真实数据契约",
  };
  return dataApi.describeDataSource(source);
}

function renderDataSourceNotice() {
  const source = state.dataSource;
  if (!currentViewUsesContractData()) return `<div class="data-source-notice" role="status"><i data-lucide="construction"></i><span><b>${state.dataClient.transport === "http" ? "当前视图尚无可用的真实读模型" : "当前视图是保留的视觉 Fixture"}，数据状态为 unavailable</b><small>${state.scope === "global" && state.view === "resource" ? "全局资源的兼容推导不代表已确认的真实共享关系。" : "该视图尚未接入当前可用的数据契约。"}</small></span></div>`;
  if (!source.error) return "";
  return `<div class="data-source-notice" role="status"><i data-lucide="circle-alert"></i><span><b>真实数据 API unavailable；保留上次成功读取的页面状态</b><small>${escapeHtml(source.error.code)} · ${escapeHtml(source.error.message)}${source.error.requestId ? ` · ${escapeHtml(source.error.requestId)}` : ""}</small></span></div>`;
}

function currentViewUsesContractData() {
  if (state.scope === "global" && state.view === "world") return state.features.global_world && Boolean(state.contractMeta.globalWorld);
  if (state.scope === "global" && state.view === "hosts") return Boolean(state.contractMeta.globalHosts);
  if (state.scope === "global" && state.view === "resource") return state.features.global_resources;
  if (state.scope === "project" && state.view === "resource") return state.features.project_resources;
  return false;
}

function graphKindLabel(kind) {
  return ({
    workspace: "工作区",
    host: "服务器",
    task: "任务",
    project: "项目",
    compose_project: "Compose 项目",
    service: "服务",
    container: "容器",
    image: "镜像",
    network: "网络",
    volume: "卷",
    port: "端口",
    document: "文档",
    unknown: "未知对象",
  })[kind] || kind;
}

function projectionStateLabel(value) {
  return ({ fixture: "Fixture", discovered: "已发现", draft: "草稿", confirmed: "已确认", stale: "已过期", unavailable: "不可用", archived: "已归档" })[value] || value;
}

function projectionTone(value) {
  if (value === "stale") return "amber";
  if (value === "unavailable" || value === "archived") return "muted";
  if (value === "confirmed" || value === "discovered") return "green";
  return "muted";
}

function formatObservedAt(value) {
  if (!value) return "时间未知";
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return value;
  return new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(date);
}

function escapeHtml(value) {
  return String(value).replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]);
}

function activateIcons() {
  if (window.lucide) window.lucide.createIcons({ attrs: { "stroke-width": 1.7 } });
}

function viewMeta() {
  if (state.scope === "project") {
    const project = projects[state.activeProject];
    if (!project) return { crumb: "项目资源", action: "连接 Linux HOST", actionIcon: "server" };
    if (state.view === "resource") return { crumb: `${project.label} / 项目资源`, action: "登记项目资源", actionIcon: "plus" };
    return state.workflowMode === "edit"
      ? { crumb: `${project.label} / 流程编辑`, action: "保存草稿", actionIcon: "save" }
      : { crumb: `${project.label} / 运行`, action: "编辑流程", actionIcon: "pencil-line" };
  }
  if (state.scope === "global" && state.view === "world") {
    return { crumb: "全局业务网", action: state.m3.model ? "业务统筹 Agent 设置" : "配置业务统筹 Agent", actionIcon: "bot" };
  }
  if (state.scope === "global" && state.view === "hosts") {
    return { crumb: "服务器", action: selectedHost() ? "重试连接 / 扫描" : "登记 Linux HOST", actionIcon: selectedHost() ? "refresh-cw" : "server" };
  }
  return state.view === "resource"
    ? { crumb: "全局资源", action: "登记共享资源", actionIcon: "plus" }
    : { crumb: "全局业务网", action: "登记项目", actionIcon: "folder-plus" };
}

function renderAuthGate() {
  const checking = !state.auth.checked;
  const error = state.auth.error;
  return `
    <main class="auth-gate">
      <section class="auth-panel" aria-live="polite">
        <div class="auth-brand"><span class="brand-mark" aria-hidden="true"><i></i><i></i><i></i></span><span><strong>Network Atlas</strong><small>个人数字业务网</small></span></div>
        <div class="auth-copy"><span class="view-kicker">OWNER SESSION</span><h1>${checking ? "正在确认所有者会话" : "登录你的业务网络"}</h1><p>${checking ? "正在读取同源会话状态，不会先展示业务数据。" : "单一所有者入口用于保护项目图、HOST 扫描、模型配置和本地投影。"}</p></div>
        ${checking ? `<div class="auth-checking"><i data-lucide="loader-circle"></i><span>检查会话</span></div>` : `
          <form class="auth-form" data-owner-login>
            <label><span>所有者</span><input name="username" autocomplete="username" maxlength="128" value="${escapeHtml(state.auth.username || "")}" required></label>
            <label><span>密码</span><input name="password" type="password" autocomplete="current-password" maxlength="1024" required></label>
            ${error ? `<div class="auth-error"><i data-lucide="circle-alert"></i><span><b>${escapeHtml(error.code || "LOGIN_FAILED")}</b>${escapeHtml(error.message || "登录失败")}</span></div>` : ""}
            <button class="command-button emphasis auth-submit" type="submit" ${state.auth.busy ? "disabled" : ""}><i data-lucide="${state.auth.busy ? "loader-circle" : "log-in"}"></i><span>${state.auth.busy ? "正在登录" : "登录"}</span></button>
          </form>`}
        <div class="auth-boundary"><span><i data-lucide="shield-check"></i>HTTPS 入口</span><span><i data-lucide="key-round"></i>秘密仅在服务端</span><span><i data-lucide="scan-search"></i>TARGET_HOST 只读</span></div>
      </section>
    </main>`;
}

function renderM4DataModal() {
  if (!state.m4.modal) return "";
  const hostId = state.selectedHostId || state.m2.draft?.host_id || state.m2.setup?.hostId || null;
  const projectId = state.scope === "project" ? state.activeProject : Object.keys(projects)[0] || null;
  const pending = state.m4.pendingDelete;
  return `<div class="m2-modal-scrim" role="presentation"><section class="m2-dialog m4-data-dialog" role="dialog" aria-modal="true" aria-label="所有者与数据">
    <div class="m2-dialog-head"><div><span>OWNER · DATA</span><h2>所有者与本地数据</h2></div><button class="icon-button" data-m4-action="close" aria-label="关闭"><i data-lucide="x"></i></button></div>
    <div class="m4-session-summary"><span class="status-dot live"></span><span><b>${escapeHtml(state.auth.username || "owner")}</b><small>${state.auth.enabled ? `会话到期 ${escapeHtml(formatObservedAt(state.auth.expiresAt))}` : "本机开发会话"}</small></span><button class="text-button" data-m4-action="logout">退出登录</button></div>
    ${state.m4.error ? `<div class="m2-inline-error"><b>${escapeHtml(state.m4.error.code)}</b><span>${escapeHtml(state.m4.error.message)}</span></div>` : ""}
    <div class="m4-data-list">
      <article><span><i data-lucide="database"></i></span><div><b>整个工作区</b><small>投影、HOST、扫描摘要、Agent 建议与审计摘要</small></div><button class="command-button" data-m4-export="workspace"><i data-lucide="download"></i>导出</button><button class="text-button danger" data-m4-delete="workspace" data-scope-id="workspace-default">删除</button></article>
      <article class="${hostId ? "" : "disabled"}"><span><i data-lucide="server"></i></span><div><b>当前 HOST</b><small>${escapeHtml(hostId || "尚未登记 HOST")}</small></div><button class="command-button" data-m4-export="host" ${hostId ? "" : "disabled"}><i data-lucide="download"></i>导出</button><button class="text-button danger" data-m4-delete="host" data-scope-id="${escapeHtml(hostId || "")}" ${hostId ? "" : "disabled"}>删除</button></article>
      <article class="${projectId ? "" : "disabled"}"><span><i data-lucide="folder-tree"></i></span><div><b>当前项目</b><small>${escapeHtml(projects[projectId]?.label || projectId || "暂无项目")}</small></div><button class="command-button" data-m4-export="project" ${projectId ? "" : "disabled"}><i data-lucide="download"></i>导出</button><button class="text-button danger" data-m4-delete="project" data-scope-id="${escapeHtml(projectId || "")}" ${projectId ? "" : "disabled"}>删除</button></article>
    </div>
    ${pending ? `<form class="m4-delete-confirm" data-m4-delete-form><input type="hidden" name="scope_kind" value="${escapeHtml(pending.scopeKind)}"><input type="hidden" name="scope_id" value="${escapeHtml(pending.scopeId)}"><div><span>删除确认</span><b>${escapeHtml(pending.label)}</b><small>执行前会创建受限备份；该操作只删除 APP_HOST 本地数据，不触碰 TARGET_HOST。</small></div><label><span>输入“删除”继续</span><input name="confirmation" autocomplete="off" required></label><div><button class="text-button" type="button" data-m4-action="cancel-delete">取消</button><button class="command-button danger" type="submit" ${state.m4.busy ? "disabled" : ""}><i data-lucide="trash-2"></i>${state.m4.busy ? "正在备份并删除" : "确认删除"}</button></div></form>` : ""}
    <div class="m2-dialog-actions"><span>导出不含 SSH 密码、私钥、模型 Key、会话令牌或原始敏感输出。</span><button class="command-button" data-m4-action="close">完成</button></div>
  </section></div>`;
}

function renderApp() {
  // A network/SSE refresh may arrive while a pointer is held. Keep the
  // current SVG alive until the gesture ends; otherwise pointer capture is
  // detached from the node and the next frame appears to jump.
  if (state.drag || state.panDrag) {
    state.renderDeferred = true;
    return;
  }
  state.renderDeferred = false;
  if (!state.auth.checked || !state.auth.authenticated) {
    app.innerHTML = renderAuthGate();
    app.querySelector("[data-owner-login]")?.addEventListener("submit", (event) => {
      event.preventDefault();
      void submitOwnerLogin(event.currentTarget);
    });
    activateIcons();
    return;
  }
  const meta = viewMeta();
  const source = dataSourcePresentation();
  const projectCount = Object.keys(projects).length;
  app.innerHTML = `
    <div class="app-shell">
      <aside class="sidebar" aria-label="主导航">
        <div class="brand">
          <span class="brand-mark" aria-hidden="true"><i></i><i></i><i></i></span>
          <span><strong>Network Atlas</strong><small>个人数字业务网</small></span>
        </div>
        <div class="sync-status"><span class="status-dot ${source.dotClass}"></span><span><b>${escapeHtml(source.label)}</b><small>${projectCount} 个项目 · ${escapeHtml(source.statusLabel)}</small></span></div>
        <nav class="nav" aria-label="视图导航">
          ${navButton("world", "network", "全局业务网", "1")}
          ${navButton("resource", "boxes", "全局资源", "2")}
          ${navButton("hosts", "server", "服务器", "3")}
        </nav>
        <div class="sidebar-rule"></div>
        <section class="side-section">
          <div class="side-heading"><span>项目投影</span><span>${projectCount}</span></div>
          <div class="sidebar-projects">${Object.entries(projects).map(([id, project]) => sideProject(id, project)).join("")}</div>
        </section>
        <div class="sidebar-fill"></div>
        <button class="knowledge-button" id="knowledge-button"><i data-lucide="book-open"></i><span><b>知识入口</b><small>Obsidian · GBrain · 自定义</small></span><i data-lucide="plus"></i></button>
        <div class="sidebar-footer"><span class="status-dot ${source.dotClass}"></span><span>数据状态 <strong>${escapeHtml(source.freshnessLabel)}</strong></span></div>
      </aside>
      <main class="main">
        <header class="topbar">
          <div class="crumbs"><span>个人数字业务网</span><i data-lucide="chevron-right"></i><span class="crumb-current">${meta.crumb}</span></div>
          <div class="topbar-actions">
            <span class="quiet-status data-source-chip ${source.tone}" title="${escapeHtml(source.detail)}"><span class="status-dot ${source.dotClass}"></span>${escapeHtml(source.shortLabel)}</span>
            <button class="command-button" id="primary-action"><i data-lucide="${meta.actionIcon}"></i><span>${meta.action}</span></button>
            <button class="icon-button" id="refresh-button" data-tooltip="刷新投影" aria-label="刷新投影" ${state.dataSource.loading ? "disabled" : ""}><i data-lucide="refresh-cw"></i></button>
            <button class="owner-button" id="owner-button" aria-label="所有者与数据"><span class="status-dot ${state.events.connected ? "live" : ""}"></span><span>${escapeHtml(state.auth.username || "owner")}</span><i data-lucide="chevron-down"></i></button>
          </div>
        </header>
        <section class="workspace" id="workspace">${renderDataSourceNotice()}${renderCurrentView()}</section>
      </main>
    </div>${renderM2Modal()}${renderM4DataModal()}`;
  bindAppEvents();
  activateIcons();
  updateHash();
}

function navButton(view, icon, label, key) {
  const active = state.scope === "global" && state.view === view;
  return `<button class="nav-button ${active ? "active" : ""}" data-view="${view}" aria-current="${active ? "page" : "false"}"><i data-lucide="${icon}"></i><span>${label}</span><kbd>${key}</kbd></button>`;
}

function sideProject(id, project) {
  const active = state.scope === "project" && state.activeProject === id;
  const selectedGlobal = state.scope === "global" && state.view === "world" && state.selectedWorldId === id;
  const showWorkflow = state.dataSource.kind !== "real";
  return `<div class="side-project-wrap"><button class="side-project ${active || selectedGlobal ? "active" : ""}" data-side-project="${id}"><span class="status-dot ${project.tone === "green" ? "green" : project.tone === "amber" ? "amber" : ""}"></span><b>${escapeHtml(project.label)}</b><small>${project.alerts ? `${project.alerts} 异常` : `${project.activity} 活动`}</small></button>${active ? `<div class="project-subnav">${showWorkflow ? `<button class="${state.view === "workflow" ? "active" : ""}" data-project-view="workflow"><i data-lucide="activity"></i>运行与流程<kbd>4</kbd></button>` : ""}<button class="${state.view === "resource" ? "active" : ""}" data-project-view="resource"><i data-lucide="boxes"></i>项目资源<kbd>5</kbd></button></div>` : ""}</div>`;
}

function renderCurrentView() {
  if (state.scope === "project" && !projects[state.activeProject]) {
    return `<div class="view-header"><div><div class="view-kicker">PROJECT RESOURCES</div><h1>正在读取项目投影</h1></div></div>`;
  }
  if (state.scope === "project") return state.view === "resource" ? renderResourceView() : renderWorkflowView();
  if (state.view === "hosts") return renderHostsView();
  return state.view === "resource" ? renderResourceView() : renderWorldView();
}

function projectScopeBar() {
  const project = projects[state.activeProject];
  if (!project) return "";
  const tone = project.tone === "green" ? "green" : project.tone === "amber" ? "amber" : "";
  const workflow = state.dataSource.kind !== "real" ? `<button class="${state.view === "workflow" ? "active" : ""}" data-project-view="workflow"><i data-lucide="activity"></i><span>运行与流程</span><kbd>4</kbd></button>` : "";
  return `<div class="project-scope-bar"><div class="project-identity"><span class="status-dot ${tone}"></span><span><b>${escapeHtml(project.label)}</b><small>${escapeHtml(project.subtitle)}</small></span></div><div class="project-local-nav" aria-label="项目导航">${workflow}<button class="${state.view === "resource" ? "active" : ""}" data-project-view="resource"><i data-lucide="boxes"></i><span>项目资源</span><kbd>5</kbd></button></div></div>`;
}

function renderWorldView() {
  const selected = selectedWorldNode();
  const counts = state.navigation;
  const real = state.dataSource.kind === "real";
  const businessKinds = new Set(["coordinator", "task", "project"]);
  const worldBusinessNodes = real ? worldNodes.filter((node) => businessKinds.has(node.kind)) : worldNodes;
  const worldBusinessIds = new Set(worldBusinessNodes.map((node) => node.id));
  const worldBusinessEdges = real ? worldEdges.filter((edge) => worldBusinessIds.has(edge.from) && worldBusinessIds.has(edge.to)) : worldEdges;
  const taskCount = worldBusinessNodes.filter((node) => node.kind === "task").length;
  const projectCount = worldBusinessNodes.filter((node) => node.kind === "project").length;
  const emptyRealTaskProjection = real && taskCount === 0;
  const agentStatus = state.m3.model ? "已配置" : "未配置";
  return `
    <div class="view-header">
      <div><div class="view-kicker">GLOBAL BUSINESS TASK NETWORK</div><h1>${emptyRealTaskProjection ? "业务任务尚未建立" : real ? "业务任务统筹" : "业务统筹 Agent 与业务任务"}</h1><p>${emptyRealTaskProjection ? "业务统筹 Agent 的直接对象是业务任务；项目是任务作用域，服务器是任务依赖的资源。当前没有业务任务实体，因此不绘制虚构连线。" : real ? "这里表达 Agent → 任务 → 项目作用域；服务器及 SSH 连接细节只在服务器页出现。" : "顶层表达业务任务及其项目作用域；项目内部资源按需进入项目资源视图。"}</p></div>
      <div class="world-header-actions"><button class="agent-config-button ${state.m3.model ? "configured" : ""}" data-m3-action="model" type="button"><i data-lucide="bot"></i><span><small>业务统筹 Agent</small><b>${agentStatus}</b></span><i data-lucide="settings-2"></i></button><div class="header-metrics"><div class="metric"><span>任务</span><b>${taskCount}</b></div><div class="metric"><span>项目</span><b class="mint">${projectCount}</b></div><div class="metric"><span>需关注</span><b class="amber">${counts.attention}</b></div></div></div>
    </div>
    <div class="global-overview-layout">
      <div class="global-canvas-column">
        <div class="canvas-stage" id="world-stage">
          <div class="canvas-topline"><span class="canvas-badge"><span class="status-dot ${real ? "live" : "live"}"></span>${real ? "任务统筹投影" : "业务任务投影"} · ${worldBusinessNodes.length} 个实体</span><span class="canvas-legend">${real ? `<span class="legend-entry"><i class="legend-line live"></i>Agent → 任务</span><span class="legend-entry"><i class="legend-line blue"></i>任务 → 项目作用域</span>` : `<span class="legend-entry"><i class="legend-line blue"></i>任务作用域</span><span class="legend-entry"><i class="legend-line live"></i>状态同步</span><span class="legend-entry"><i class="legend-line amber"></i>待关注</span>`}</span></div>
           ${renderWorldSvg(worldBusinessNodes, worldBusinessEdges)}
           ${canvasTools("world")}
           <div class="canvas-footer-note"><i data-lucide="move"></i><span>布局只改变本地投影；状态流只沿已声明的实际关系移动。</span></div>
         </div>
          ${real ? "" : renderCoordinatorChat()}
       </div>
       <aside class="global-command-rail" aria-label="业务统筹 Agent 工作台">
          ${real ? `${renderGlobalAgentConfig()}${emptyRealTaskProjection ? `<div class="real-evidence-empty"><i data-lucide="list-todo"></i><div><b>尚无业务任务</b><span>连接检查和扫描属于系统作业，不会冒充业务统筹任务。</span></div></div>` : ""}${renderBusinessProjectionSummary(selected)}` : `${renderGlobalFocusStrip(selected)}${renderOperations()}${renderQuickActions()}`}
       </aside>
     </div>`;
}

function renderGlobalAgentConfig() {
  const model = state.m3.model;
  return `<section class="global-agent-config rail-section" aria-label="业务统筹 Agent 配置">
    <div class="global-agent-head"><div class="section-label"><i data-lucide="bot"></i><span>业务统筹 Agent</span></div><span class="agent-config-state ${model ? "configured" : ""}">${model ? "已配置" : "未配置"}</span></div>
    <div class="global-agent-body"><strong>${escapeHtml(model?.model || "连接你的 OpenAI 兼容服务")}</strong><small>${escapeHtml(model?.base_url || "填写 URL、Key 和模型后，Agent 才能参与扫描后的配置建议。")}</small></div>
    <button class="command-button global-agent-button" data-m3-action="model" type="button"><i data-lucide="settings-2"></i><span>${model ? "修改 URL / Key / 模型" : "配置 URL / Key / 模型"}</span></button>
  </section>`;
}

function hostStatusLabel(value) {
  return ({
    host_registered: "已登记",
    fingerprint_fetching: "正在读取指纹",
    host_key_unverified: "待确认指纹",
    host_key_verified: "指纹已确认",
    host_key_changed: "指纹已变化",
    connection_checking: "正在连接",
    connection_ready: "连接已就绪",
    docker_unavailable: "连接已就绪",
    docker_permission_denied: "连接已就绪",
    discovery_running: "正在扫描",
    evidence_ready: "证据已就绪",
    discovery_complete: "发现已完成",
    discovery_partial: "部分发现",
    discovery_unavailable: "发现不可用",
    failed: "连接失败",
  })[value] || value || "状态未知";
}

function selectedHost() {
  return state.hosts.find((host) => host.host_id === state.selectedHostId) || state.hosts[0] || null;
}

function hostConnectionAvailable(host) {
  if (!host) return false;
  return [
    "connection_ready",
    "evidence_ready",
    "docker_unavailable",
    "docker_permission_denied",
    "discovery_complete",
    "discovery_partial",
    "discovery_unavailable",
    "ssh_ready",
    "linux_ready",
  ].includes(host.status) || (Array.isArray(host.capabilities) && host.capabilities.includes("ssh"));
}

function normalizedHostConnectionState(host) {
  if (hostConnectionAvailable(host)) return "connection_ready";
  return host?.status || "host_registered";
}

function providerStatusLabel(value) {
  return ({
    ready: "可用",
    permission_denied: "权限不足",
    unavailable: "不可用",
    timed_out: "超时",
    failed: "失败",
  })[value] || "状态未知";
}

function providerKindLabel(value) {
  return ({ docker: "Docker", compose: "Docker Compose", systemd: "systemd 服务" })[value] || value;
}

function providerStatusTone(value) {
  if (value === "ready") return "green";
  if (["permission_denied", "failed"].includes(value)) return "red";
  return "amber";
}

function discoveryStatusLabel(value) {
  return value ? hostStatusLabel(value) : "尚未扫描";
}

function discoveryStatusTone(value) {
  if (["evidence_ready", "discovery_complete"].includes(value)) return "green";
  if (["discovery_partial", "discovery_unavailable"].includes(value)) return "amber";
  return "muted";
}

function evidenceFreshnessLabel(value) {
  return ({
    fresh: "fresh · 新鲜",
    stale: "stale · 已过期",
    unavailable: "unavailable · 未知",
  })[value] || "unavailable · 未知";
}

function evidenceFreshnessTone(value) {
  if (value === "fresh") return "green";
  if (value === "stale") return "amber";
  return "muted";
}

function providerWarningSummary(provider) {
  const warnings = Array.isArray(provider?.warnings) ? provider.warnings : [];
  if (!warnings.length) return null;
  const warning = warnings[0] || {};
  const compact = (value, limit) => {
    const normalized = String(value || "").replace(/\s+/g, " ").trim();
    return normalized.length > limit ? `${normalized.slice(0, limit - 1)}…` : normalized;
  };
  return {
    code: compact(warning.code || "PROVIDER_WARNING", 64),
    summary: compact(warning.summary || "Provider 返回了需关注的告警", 180),
    remaining: Math.max(0, warnings.length - 1),
  };
}

function finiteMetric(value) {
  if (value === null || value === undefined || value === "") return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function formatMetricNumber(value, digits = 1) {
  const number = finiteMetric(value);
  return number === null ? "—" : number.toFixed(digits);
}

function formatMetricPercent(value, digits = 1) {
  const number = finiteMetric(value);
  return number === null ? "—" : `${number.toFixed(digits)}%`;
}

function formatMetricRatio(value, digits = 1) {
  const number = finiteMetric(value);
  return number === null ? "—" : `${(number * 100).toFixed(digits)}%`;
}

function formatMetricBytes(value) {
  const number = finiteMetric(value);
  if (number === null || number < 0) return "—";
  if (number === 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  const unitIndex = Math.min(Math.floor(Math.log(number) / Math.log(1024)), units.length - 1);
  const scaled = number / (1024 ** unitIndex);
  return `${scaled.toFixed(scaled >= 100 || unitIndex === 0 ? 0 : scaled >= 10 ? 1 : 2)} ${units[unitIndex]}`;
}

function formatMetricRate(value) {
  const formatted = formatMetricBytes(value);
  return formatted === "—" ? formatted : `${formatted}/s`;
}

function formatMetricDuration(value) {
  const seconds = finiteMetric(value);
  if (seconds === null || seconds < 0) return "—";
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days) return `${days} 天 ${hours} 小时`;
  if (hours) return `${hours} 小时 ${minutes} 分`;
  if (minutes) return `${minutes} 分 ${Math.floor(seconds % 60)} 秒`;
  return `${Math.floor(seconds)} 秒`;
}

function monitorQualityLabel(value) {
  return ({
    observed: "已观测",
    unsupported: "不支持",
    parse_failed: "解析失败",
    counter_reset: "计数器重置",
    counter_unreliable: "计数器不可靠",
    insufficient_interval: "采样间隔不足",
    permission_denied: "权限不足",
    timed_out: "超时",
  })[value] || "未知";
}

function monitorQualityTone(value) {
  if (value === "observed") return "green";
  if (["counter_reset", "counter_unreliable", "insufficient_interval", "unsupported"].includes(value)) return "amber";
  if (["parse_failed", "permission_denied", "timed_out"].includes(value)) return "red";
  return "muted";
}

function monitorRunStateLabel(value) {
  return ({
    queued: "等待采集",
    running: "正在采集",
    succeeded: "采集成功",
    partial: "部分采集",
    failed: "采集失败",
    timed_out: "采集超时",
    skipped_overlap: "跳过重叠采集",
    interrupted: "采集中断",
  })[value] || "尚未采集";
}

function monitorRunStateTone(value) {
  if (value === "succeeded") return "green";
  if (["queued", "running"].includes(value)) return "blue";
  if (["partial", "skipped_overlap"].includes(value)) return "amber";
  if (["failed", "timed_out", "interrupted"].includes(value)) return "red";
  return "muted";
}

function monitorFreshnessLabel(value) {
  return ({ fresh: "新鲜", stale: "陈旧", unknown: "未知", unavailable: "未知" })[value] || "未知";
}

function monitorFreshnessTone(value) {
  if (value === "fresh") return "green";
  if (value === "stale") return "amber";
  return "muted";
}

function hostMonitoringFor(asset) {
  const hostId = asset?.host?.host_id;
  return state.monitoring.byHost[hostId] || {
    latestRun: asset?.latest_monitor_run || null,
    currentSnapshot: asset?.current_snapshot?.cpu ? asset.current_snapshot : null,
    currentSnapshotSummary: {
      freshness: asset?.monitor_freshness || asset?.current_snapshot?.freshness || "unknown",
      unknownCount: asset?.monitor_unknown_count ?? asset?.current_snapshot?.unknown_count ?? null,
      observedAt: asset?.current_snapshot?.observed_at || null,
      runId: asset?.current_snapshot?.run_id || null,
    },
    meta: null,
  };
}

function monitoringRequestIsCurrent(hostId, token) {
  return token === state.monitoring.restoreToken
    && hostId === state.selectedHostId
    && state.scope === "global"
    && state.view === "hosts";
}

async function loadHostMonitoring(hostId, options = {}) {
  if (!hostId || state.dataClient.transport !== "http") return null;
  const token = ++state.monitoring.restoreToken;
  try {
    const response = await state.dataClient.getHostMonitoring(hostId);
    if (!monitoringRequestIsCurrent(hostId, token)) return null;
    state.monitoring.byHost[hostId] = {
      latestRun: response.data.latest_run || null,
      currentSnapshot: response.data.current_snapshot || null,
      currentSnapshotSummary: {
        freshness: response.data.current_snapshot?.freshness || response.data.monitor_freshness || "unknown",
        unknownCount: response.data.current_snapshot?.unknown_count ?? null,
        observedAt: response.data.current_snapshot?.observed_at || null,
        runId: response.data.current_snapshot?.run_id || null,
      },
      meta: response.meta || null,
    };
    delete state.monitoring.errorByHost[hostId];
    if (options.render !== false) renderApp();
    return response;
  } catch (error) {
    if (!monitoringRequestIsCurrent(hostId, token)) return null;
    if (error?.status === 401) expireOwnerSession(error);
    state.monitoring.errorByHost[hostId] = dataApi.errorRecord(error);
    if (options.render !== false) renderApp();
    return null;
  }
}

async function pollMonitorRun(runId, hostId) {
  for (let attempt = 0; attempt < 240; attempt += 1) {
    const response = await state.dataClient.getMonitorRun(runId);
    const previous = state.monitoring.byHost[hostId] || { currentSnapshot: null, meta: null };
    state.monitoring.byHost[hostId] = { ...previous, latestRun: response.data };
    if (monitorTerminalStates.has(response.data.state)) return response.data;
    if (attempt % 4 === 0 && hostId === state.selectedHostId && state.view === "hosts") renderApp();
    await new Promise((resolve) => window.setTimeout(resolve, 250));
  }
  throw new dataApi.DataSourceError("MONITOR_POLL_TIMEOUT", "等待资源采集超过 60 秒");
}

async function startManualMonitorRun(hostId) {
  if (!hostId || state.dataClient.transport !== "http" || state.monitoring.busyHostId) return;
  state.monitoring.busyHostId = hostId;
  delete state.monitoring.errorByHost[hostId];
  renderApp();
  try {
    const accepted = await state.dataClient.createMonitorRun(hostId, dataApi.createIdempotencyKey("monitor"));
    const previous = state.monitoring.byHost[hostId] || { currentSnapshot: null, meta: null };
    state.monitoring.byHost[hostId] = { ...previous, latestRun: accepted.data };
    renderApp();
    const run = await pollMonitorRun(accepted.data.run_id, hostId);
    if (hostId === state.selectedHostId && state.scope === "global" && state.view === "hosts") {
      await loadHostMonitoring(hostId, { render: false });
    }
    const message = ({
      succeeded: "资源采集完成",
      partial: "资源采集部分完成 · 未知项已保留",
      failed: `资源采集失败 · ${run.failure_code || "MONITOR_FAILED"}`,
      timed_out: "资源采集超时",
      skipped_overlap: "已有采集正在运行 · 本次已跳过",
      interrupted: "资源采集中断",
    })[run.state] || monitorRunStateLabel(run.state);
    showToast(message);
  } catch (error) {
    state.monitoring.errorByHost[hostId] = dataApi.errorRecord(error);
    showToast(`资源采集失败 · ${state.monitoring.errorByHost[hostId].code}`);
  } finally {
    if (state.monitoring.busyHostId === hostId) state.monitoring.busyHostId = null;
    renderApp();
  }
}

function connectionReadyForDiscovery(connection) {
  return ["connection_ready", "docker_unavailable", "docker_permission_denied"].includes(connection?.data?.state);
}

function selectHostContext(hostId, { open = true } = {}) {
  const host = state.hosts.find((candidate) => candidate.host_id === hostId);
  if (!host) return null;
  if (state.selectedHostId !== host.host_id) state.m2.selectedNodeId = null;
  state.selectedHostId = host.host_id;
  state.selectedWorldId = host.host_id;
  state.operationProject = host.host_id;
  state.contextOpen = open;
  state.m2.setup = {
    hostId: host.host_id,
    displayName: host.display_name,
    address: host.address,
    port: String(host.port),
    sshUser: host.ssh_user,
    credentialKind: host.credential_kind || "ssh_password",
    error: null,
  };
  return host;
}

function selectWorldNode(nodeId) {
  state.selectedWorldId = nodeId;
  if (state.hosts.some((host) => host.host_id === nodeId)) {
    selectHostContext(nodeId);
    renderApp();
    void restoreM2ProjectionForHost(state.selectedHostId).then(renderApp);
    return;
  }
  state.operationProject = nodeId === "coordinator" ? "all" : nodeId;
  state.contextOpen = true;
  renderApp();
}

function renderWorldSvg(nodes = worldNodes, edges = worldEdges) {
  const content = `${edges.map((edge) => worldEdgeMarkup(edge, nodes)).join("")}${edges.filter((edge) => edge.animated).map((edge) => signalDot(edge.id, edge.tone === "attention" ? "amber" : "")).join("")}${nodes.map(worldNodeMarkup).join("")}`;
  return graphSvg("world-canvas", "world-scene", content, state.worldPan, state.worldZoom, "业务统筹 Agent、任务与项目作用域画布");
}

function graphSvg(id, sceneId, content, pan, zoom, label) {
  return `<svg class="canvas-graph" id="${id}" viewBox="0 0 1000 590" role="img" aria-label="${label}" tabindex="0">
    <defs>
      <marker id="arrow-mint" markerWidth="10" markerHeight="10" refX="8" refY="4" orient="auto" markerUnits="strokeWidth"><path d="M0,0 L0,8 L8,4 z" fill="#79d6c0"></path></marker>
      <marker id="arrow-amber" markerWidth="10" markerHeight="10" refX="8" refY="4" orient="auto" markerUnits="strokeWidth"><path d="M0,0 L0,8 L8,4 z" fill="#efbd72"></path></marker>
      <marker id="arrow-muted" markerWidth="10" markerHeight="10" refX="8" refY="4" orient="auto" markerUnits="strokeWidth"><path d="M0,0 L0,8 L8,4 z" fill="#7d92a3"></path></marker>
    </defs>
    <g id="${sceneId}" transform="translate(${pan.x} ${pan.y}) scale(${zoom})">${content}</g>
  </svg>`;
}

function nodeAnchors(from, to) {
  const fromCenter = { x: from.x + from.width / 2, y: from.y + from.height / 2 };
  const toCenter = { x: to.x + to.width / 2, y: to.y + to.height / 2 };
  const dx = toCenter.x - fromCenter.x;
  const dy = toCenter.y - fromCenter.y;
  if (Math.abs(dx) > Math.abs(dy)) {
    return {
      from: { x: dx > 0 ? from.x + from.width : from.x, y: fromCenter.y },
      to: { x: dx > 0 ? to.x : to.x + to.width, y: toCenter.y },
    };
  }
  return {
    from: { x: fromCenter.x, y: dy > 0 ? from.y + from.height : from.y },
    to: { x: toCenter.x, y: dy > 0 ? to.y : to.y + to.height },
  };
}

function curvePath(from, to) {
  const points = nodeAnchors(from, to);
  const dx = points.to.x - points.from.x;
  const dy = points.to.y - points.from.y;
  if (Math.abs(dx) > Math.abs(dy)) {
    const bend = points.from.x + dx * 0.52;
    return `M${points.from.x} ${points.from.y} C${bend} ${points.from.y} ${bend} ${points.to.y} ${points.to.x} ${points.to.y}`;
  }
  const bend = points.from.y + dy * 0.52;
  return `M${points.from.x} ${points.from.y} C${points.from.x} ${bend} ${points.to.x} ${bend} ${points.to.x} ${points.to.y}`;
}

function worldEdgeMarkup(edge, nodes = worldNodes) {
  const from = nodes.find((node) => node.id === edge.from);
  const to = nodes.find((node) => node.id === edge.to);
  if (!from || !to) return "";
  const marker = edge.tone === "attention" ? "arrow-amber" : edge.tone === "dashed" ? "arrow-muted" : "arrow-mint";
  const mid = midpoint(from, to);
  return `<g><path id="${edge.id}" class="graph-edge ${edge.tone}" d="${curvePath(from, to)}" marker-end="url(#${marker})"></path><text class="edge-label" x="${mid.x}" y="${mid.y - 7}" text-anchor="middle">${edge.label}</text></g>`;
}

function renderBusinessProjectionSummary(node) {
  const businessNode = node && ["coordinator", "task", "project"].includes(node.kind) ? node : worldNodes.find((candidate) => candidate.kind === "coordinator") || null;
  return `<section class="projection-workbench rail-section" aria-label="业务统筹范围">
    <div class="projection-head"><div class="section-label"><i data-lucide="list-todo"></i><span>统筹范围</span></div><span class="version-badge">业务任务</span></div>
    ${businessNode ? `<div class="projection-selection"><div><span class="context-kicker">${escapeHtml(graphKindLabel(businessNode.kind).toUpperCase())}</span><strong>${escapeHtml(businessNode.label)}</strong><small>${escapeHtml(businessNode.status)}</small></div></div>` : ""}
    <div class="business-scope-note"><i data-lucide="list-todo"></i><span>业务统筹 Agent 直接统筹业务任务；项目只标记任务作用域，服务器只作为资源依赖。请到“服务器”页管理连接、密钥、重连和扫描。</span></div>
    <button class="command-button global-agent-button" data-open-host-resources type="button"><i data-lucide="server"></i><span>打开服务器资产</span></button>
  </section>`;
}

function midpoint(from, to) {
  return { x: (from.x + from.width / 2 + to.x + to.width / 2) / 2, y: (from.y + from.height / 2 + to.y + to.height / 2) / 2 };
}

function signalDot(id, tone = "") {
  return `<circle r="3.5" class="signal-dot ${tone}" aria-hidden="true"><animateMotion dur="2.8s" repeatCount="indefinite"><mpath href="#${id}"></mpath></animateMotion></circle>`;
}

function worldNodeMarkup(node) {
  const selected = state.selectedWorldId === node.id;
  const alert = node.alerts > 0 ? "alert" : "";
  const type = node.kind === "coordinator" ? "coordinator" : "";
  const hostFailure = node.kind === "host" && (node.tone === "red" || node.projectionState === "unavailable") ? "host-failure" : "";
  const stateTone = hostFailure ? "red" : node.tone;
  const icon = graphNodeIcon(node.kind);
  const compact = node.height < 86;
  const kindLabel = node.kind === "coordinator" ? "COORDINATOR" : graphKindLabel(node.kind).toUpperCase();
  const title = truncateGraphLabel(node.label, compact ? 20 : 25);
  const subtitle = truncateGraphLabel(node.subtitle, compact ? 22 : 32);
  return `<g class="graph-node ${type} ${alert} ${hostFailure} ${selected ? "selected" : ""}" id="world-node-${node.id}" data-world-node="${node.id}" role="button" tabindex="0" aria-label="查看 ${node.label}" transform="translate(${node.x} ${node.y})">
    <rect class="node-surface" width="${node.width}" height="${node.height}" rx="8"></rect>
    <rect x="13" y="14" width="25" height="25" rx="6" fill="${node.kind === "coordinator" ? "rgba(121,214,192,.15)" : "rgba(143,187,239,.13)"}" stroke="${node.kind === "coordinator" ? "#79d6c0" : "#8fbbef"}" stroke-width="1"></rect>
    <text class="node-kicker" x="48" y="24">${escapeHtml(kindLabel)}</text>
    <text class="node-state ${stateTone === "red" ? "red" : stateTone === "amber" ? "amber" : stateTone === "muted" ? "muted" : "green"}" x="${node.width - 12}" y="24" text-anchor="end">${node.status}</text>
    <text class="node-title" x="13" y="${compact ? 54 : 57}">${escapeHtml(title)}</text>
    <text class="node-subtitle" x="13" y="${compact ? 69 : 74}">${escapeHtml(subtitle)}</text>
    ${compact ? "" : `<line class="node-divider" x1="13" x2="${node.width - 13}" y1="83" y2="83"></line><text class="node-metric" x="13" y="96">${node.activity} 活动</text><text class="node-metric ${node.alerts ? "attention" : ""}" x="67" y="96">${node.alerts ? `${node.alerts} 需关注` : "无异常"}</text><text class="node-metric" x="${node.width - 13}" y="96" text-anchor="end">${node.updated}</text>`}
  </g>`;
}

function graphNodeIcon(kind) {
  return ({ coordinator: "bot", workspace: "box", task: "square-check-big", host: "server", project: "folder-kanban", compose_project: "layers-3", service: "app-window", container: "container", image: "image", network: "network", volume: "database", port: "plug", document: "file-text" })[kind] || "box";
}

function truncateGraphLabel(value, maximum) {
  const text = String(value || "");
  return text.length > maximum ? `${text.slice(0, Math.max(1, maximum - 1))}…` : text;
}

function renderGlobalFocusStrip(node) {
  const project = node.kind === "project" ? projects[node.id] : null;
  const target = project ? `${project.label} 项目` : "全局业务网";
  const status = project ? `${project.health} · ${project.activity} 个活动` : `${state.navigation.projects} 个项目 · ${state.navigation.attention} 个需关注`;
  const action = project ? { label: "进入项目运行", type: "open-workflow" } : { label: "定位最近操作", type: "focus-operations" };
  const tone = project ? (project.tone === "green" ? "green" : project.tone === "amber" ? "amber" : "") : "live";
  return `<section class="global-focus-strip rail-section" aria-label="当前焦点">
    <div class="focus-strip-head"><span class="context-kicker">当前焦点</span><span class="selection-badge"><span class="status-dot ${tone}"></span>${project ? project.health : "已同步"}</span></div>
    <div class="focus-strip-target"><i data-lucide="${project ? "folder-kanban" : "bot"}"></i><strong>${escapeHtml(target)}</strong></div>
    <div class="focus-strip-meta"><span>${escapeHtml(status)}</span><button class="text-action" data-context-action="${action.type}">${action.label}<i data-lucide="arrow-right"></i></button></div>
  </section>`;
}

function coordinatorTarget() {
  const node = selectedWorldNode();
  if (node.kind === "project") return { kind: "project", id: node.id, label: node.label };
  return { kind: "global", id: "coordinator", label: "全局业务网" };
}

function renderOperations() {
  const filtered = state.operationProject === "all" ? state.operationLog : state.operationLog.filter((item) => item.project === state.operationProject);
  const entries = filtered.slice(0, 4);
  const list = entries.length ? entries.map(operationMarkup).join("") : `<div class="operation-empty"><i data-lucide="inbox"></i><span>该项目暂无业务统筹操作</span></div>`;
  return `<section class="operations-panel rail-section" id="global-operations" aria-label="业务统筹 Agent 最近操作">
    <div class="operations-head"><div class="section-label"><i data-lucide="history"></i><span>业务统筹 Agent 最近操作</span><small>按项目筛选</small></div><div class="operation-filters">${operationFilter("all", "全部")}${Object.entries(projects).map(([id, project]) => operationFilter(id, project.label)).join("")}</div></div>
    <div class="operation-list">${list}</div>
  </section>`;
}

function renderCoordinatorChat() {
  const target = coordinatorTarget();
  const messages = state.coordinatorChat.slice(-3);
  return `<section class="coordinator-chat rail-section" aria-label="与业务统筹 Agent 对话">
    <div class="chat-head"><div class="section-label"><i data-lucide="message-circle"></i><span>与业务统筹 Agent 对话</span></div><span class="chat-live"><span class="status-dot live"></span>本地会话</span></div>
    <div class="chat-target"><span class="chat-target-label">目标</span><strong>${escapeHtml(target.label)}</strong><small>${target.kind === "project" ? "项目作用域" : "全局作用域"}</small></div>
    <form class="chat-form" data-coordinator-chat-form><label class="sr-only" for="coordinator-input">向业务统筹 Agent 提问</label><div class="chat-input-wrap"><input id="coordinator-input" name="message" autocomplete="off" placeholder="问业务统筹 Agent：${escapeHtml(target.label)} 现在怎样？" /><button class="icon-button" type="submit" data-tooltip="发送" aria-label="发送"><i data-lucide="arrow-up"></i></button></div><small>对话生成结构化请求；结果会回到最近操作。</small></form>
    <div class="chat-thread" aria-live="polite">${messages.map((message) => `<div class="chat-message ${message.role === "user" ? "user" : "agent"}"><span class="chat-avatar">${message.role === "user" ? "你" : "总"}</span><div><p>${escapeHtml(message.text)}</p><time>${escapeHtml(message.time)}</time></div></div>`).join("")}</div>
  </section>`;
}

function renderQuickActions() {
  const target = coordinatorTarget();
  const actions = quickActionsForTarget(target);
  return `<section class="quick-actions-panel rail-section" aria-label="快捷动作">
    <div class="quick-actions-head"><div class="section-label"><i data-lucide="zap"></i><span>快捷动作</span></div><small>作用于 ${escapeHtml(target.label)}</small></div>
    <div class="quick-action-grid">${actions.map(quickActionMarkup).join("")}</div>
    <div class="quick-actions-note"><i data-lucide="info"></i><span>观察动作直接生成请求；带外部副作用的动作进入预览确认。</span></div>
  </section>`;
}

function quickActionMarkup(action) {
  return `<button class="quick-action-button ${action.mode}" type="button" data-quick-action="${action.id}" aria-label="${escapeHtml(action.label)}">
    <span class="quick-action-icon"><i data-lucide="${action.icon}"></i></span><span class="quick-action-copy"><b>${escapeHtml(action.label)}</b><small>${escapeHtml(action.hint)}</small></span><span class="quick-action-mode">${escapeHtml(action.modeLabel)}</span>
  </button>`;
}

function operationFilter(id, label) {
  return `<button class="chip-button ${state.operationProject === id ? "active" : ""}" data-operation-project="${id}">${label}</button>`;
}

function operationMarkup(operation) {
  const focus = operation.focus || operation.project;
  return `<button class="operation-item" data-operation-target="${escapeHtml(focus)}"><span class="operation-time">${escapeHtml(operation.time)}</span><span class="operation-title">${escapeHtml(operation.title)}</span><span class="operation-meta"><span>${escapeHtml(operation.target)}</span><span class="operation-result ${operation.tone}">${escapeHtml(operation.result)}</span></span></button>`;
}

function selectedWorldNode() {
  return worldNodes.find((node) => node.id === state.selectedWorldId) || worldNodes[0];
}

function worldContext(node) {
  return {
    kicker: node.kind === "coordinator" ? "COORDINATOR" : graphKindLabel(node.kind).toUpperCase(),
    title: node.label,
    summary: node.summary,
    facts: node.facts,
    action: node.kind === "project" ? { label: "打开项目资源", type: "open-project-resource" } : null,
  };
}

function selectionCard(context, className = "", heading = "当前选择") {
  return `<aside class="selection-card ${className}" aria-label="${heading}">
    <div class="context-header"><div><div class="context-kicker">${context.kicker}</div><h2>${context.title}</h2></div><span class="selection-badge"><span class="status-dot live"></span>已选中</span></div>
    <p class="context-summary">${context.summary}</p>
    <div class="context-facts">${context.facts.map(([label, value]) => `<div class="context-fact"><span>${label}</span><b>${value}</b></div>`).join("")}</div>
    ${context.action ? `<div class="context-actions"><button class="text-action" data-context-action="${context.action.type}"><span>${context.action.label}</span><i data-lucide="arrow-right"></i></button></div>` : ""}
  </aside>`;
}

function activeM2Draft() {
  const draft = state.m2.draft;
  return draft?.draft_id && draft.host_id === state.selectedHostId && draft.state === "draft" ? draft : null;
}

function m2Editable() {
  return Boolean(activeM2Draft()) && state.dataClient.transport === "http" && !state.m2.busy;
}

function projectionWorkbenchNodes() {
  const draft = state.m2.draft?.host_id === state.selectedHostId ? state.m2.draft : null;
  return (draft?.nodes || []).map((node) => {
    const facts = Object.fromEntries((node.facts || []).map((fact) => [fact.label, fact.value]));
    return {
      id: node.id,
      label: node.label,
      originalLabel: facts["原始名称"] || node.label,
      kind: node.kind,
      projectId: node.project_id || null,
      observedAt: node.observed_at || null,
      projectionState: node.state,
      sourceRefs: node.source_refs || [],
    };
  });
}

function selectedM2Node() {
  const nodes = projectionWorkbenchNodes();
  return nodes.find((node) => node.id === state.m2.selectedNodeId)
    || nodes.find((node) => !["host", "workspace"].includes(node.kind))
    || nodes.find((node) => node.kind === "host")
    || nodes[0]
    || null;
}

function renderProjectionNodeBrowser(nodes, selectedNode) {
  if (!nodes.length) return "";
  const rows = nodes.map((node) => {
    const selected = node.id === selectedNode?.id;
    const scope = node.kind === "project"
      ? "项目边界"
      : node.kind === "host"
        ? "服务器根"
        : node.kind === "workspace"
          ? "工作区"
          : node.projectId
            ? `已绑定 · ${projects[node.projectId]?.label || node.projectId}`
            : "未绑定项目";
    return `<button class="projection-node-row ${selected ? "selected" : ""}" data-m2-node-select="${escapeHtml(node.id)}" type="button">
      <i data-lucide="${graphNodeIcon(node.kind)}"></i><span><b>${escapeHtml(node.label)}</b><small>${escapeHtml(graphKindLabel(node.kind))} · ${escapeHtml(scope)}</small></span><i data-lucide="chevron-right"></i>
    </button>`;
  }).join("");
  return `<div class="projection-node-browser" aria-label="扫描对象">
    <div class="projection-node-browser-head"><span>扫描对象</span><small>${nodes.length} 个</small></div>
    <div class="projection-node-list">${rows}</div>
  </div>`;
}

function m3SessionLabel(session) {
  if (!session) return "未生成";
  return ({ ready: "建议就绪", degraded: "结果无效", unavailable: "模型不可用" })[session.state] || session.state;
}

function m3ProposalStateLabel(value) {
  return ({ pending: "待确认", adopted: "已采用", modified: "修改采用", rejected: "已拒绝", undone: "已撤销" })[value] || value;
}

function proposalOperationLabel(operation) {
  return ({
    rename: "更名",
    move: "移动",
    assign_project: "归类",
    create_project: "新建项目",
    add_relation: "添加关系",
    remove_relation: "移除关系",
    archive_node: "归档",
    restore_node: "恢复",
  })[operation.op] || operation.op;
}

function renderM3Diff() {
  const diff = state.m3.diff;
  if (!diff) return "";
  const counts = diff.counts || {};
  const entries = [
    ["added", "新增", "green"],
    ["changed", "变化", "blue"],
    ["missing", "消失", "muted"],
    ["conflict", "冲突", "amber"],
    ["unchanged", "未变", "quiet"],
  ];
  return `<div class="m3-block m3-diff" aria-label="扫描差异">
    <div class="m3-block-head"><span><i data-lucide="git-compare-arrows"></i>扫描差异</span><small>${escapeHtml(diff.previous_run_id ? "再次扫描" : "首次扫描")}</small></div>
    <div class="m3-diff-grid">${entries.map(([key, label, tone]) => `<div class="${tone}"><b>${Number(counts[key] || 0)}</b><span>${label}</span></div>`).join("")}</div>
  </div>`;
}

function renderM3Proposal(proposal) {
  const operations = (proposal.patch || []).map(proposalOperationLabel).join(" · ");
  const pending = proposal.state === "pending" && m2Editable();
  const undoable = ["adopted", "modified"].includes(proposal.state) && m2Editable();
  return `<div class="m3-proposal-row">
    <div class="m3-proposal-copy"><span>${escapeHtml(m3ProposalStateLabel(proposal.state))} · ${escapeHtml(proposal.confidence)}</span><strong>${escapeHtml(proposal.title)}</strong><p>${escapeHtml(proposal.reason)}</p><small>${escapeHtml(operations || "无投影补丁")} · ${proposal.evidence_refs?.length || 0} 条证据</small></div>
    <div class="m3-row-actions">
      ${pending ? `<button class="icon-button" data-m3-action="adopt" data-proposal-id="${escapeHtml(proposal.proposal_id)}" data-tooltip="采用建议" aria-label="采用建议"><i data-lucide="check"></i></button><button class="icon-button" data-m3-action="modify" data-proposal-id="${escapeHtml(proposal.proposal_id)}" data-tooltip="修改后采用" aria-label="修改后采用"><i data-lucide="pencil-line"></i></button><button class="icon-button" data-m3-action="reject" data-proposal-id="${escapeHtml(proposal.proposal_id)}" data-tooltip="拒绝建议" aria-label="拒绝建议"><i data-lucide="x"></i></button>` : ""}
      ${undoable ? `<button class="icon-button" data-m3-action="undo" data-proposal-id="${escapeHtml(proposal.proposal_id)}" data-tooltip="撤销本次采用" aria-label="撤销本次采用"><i data-lucide="rotate-ccw"></i></button>` : ""}
    </div>
  </div>`;
}

function renderM3Question(question) {
  const control = question.kind === "choice"
    ? `<select name="value">${question.options.map((option) => `<option value="${escapeHtml(option)}">${escapeHtml(option)}</option>`).join("")}</select>`
    : question.kind === "confirm"
      ? `<label class="m3-confirm-control"><input name="value" type="checkbox" /><span>确认</span></label>`
      : `<input name="value" maxlength="4000" required />`;
  return `<form class="m3-question" data-m3-question-form data-question-id="${escapeHtml(question.question_id)}" data-question-kind="${escapeHtml(question.kind)}">
    <label><span>${escapeHtml(question.prompt)}</span>${control}</label>
    <button class="icon-button" type="submit" data-tooltip="提交回答" aria-label="提交回答"><i data-lucide="arrow-up"></i></button>
  </form>`;
}

function renderM3Agent() {
  const draft = state.m2.draft;
  if (!draft?.draft_id || draft.host_id !== state.selectedHostId || state.dataClient.transport !== "http") return "";
  const editable = Boolean(activeM2Draft());
  const session = state.m3.session;
  const pendingQuestions = session?.questions?.filter((question) => question.state === "pending") || [];
  const warnings = session?.warnings || [];
  let body = "";
  if (!state.m3.model) {
    body = `<button class="command-button m3-primary" data-m3-action="model"><i data-lucide="settings-2"></i><span>配置模型</span></button>`;
  } else if (!session && editable) {
    body = `<button class="command-button m3-primary" data-m3-action="generate" ${state.m3.busy ? "disabled" : ""}><i data-lucide="sparkles"></i><span>生成建议</span></button>`;
  } else if (!session) {
    body = `<div class="m3-warning"><i data-lucide="circle-check"></i><span>投影已确认</span></div>`;
  } else {
    body = `${session.proposals?.length ? `<div class="m3-proposal-list">${session.proposals.map(renderM3Proposal).join("")}</div>` : ""}
      ${pendingQuestions.length ? `<div class="m3-question-list">${pendingQuestions.slice(0, 3).map(renderM3Question).join("")}</div>` : ""}
      ${warnings.length ? `<div class="m3-warning"><i data-lucide="circle-alert"></i><span>${escapeHtml(warnings[0])}</span></div>` : ""}
      ${session.state !== "ready" && editable ? `<button class="command-button m3-primary" data-m3-action="generate" ${state.m3.busy ? "disabled" : ""}><i data-lucide="refresh-cw"></i><span>重新生成</span></button>` : ""}`;
  }
  return `<div class="m3-block m3-agent" aria-label="Agent 辅助">
    <div class="m3-block-head"><span><i data-lucide="sparkles"></i>Agent 建议</span><span class="m3-agent-status ${escapeHtml(session?.state || "idle")}">${escapeHtml(m3SessionLabel(session))}</span><button class="icon-button" data-m3-action="model" data-tooltip="模型设置" aria-label="模型设置"><i data-lucide="settings-2"></i></button></div>
    ${state.m3.error ? `<div class="m3-warning"><i data-lucide="circle-alert"></i><span>${escapeHtml(state.m3.error.code)} · ${escapeHtml(state.m3.error.message)}</span></div>` : ""}
    ${state.m3.busy ? `<div class="m3-loading"><i data-lucide="loader-circle"></i><span>处理中</span></div>` : body}
  </div>`;
}

function renderProjectionWorkbench(node) {
  const draft = state.m2.draft?.host_id === state.selectedHostId ? state.m2.draft : null;
  const nodes = projectionWorkbenchNodes();
  const editable = m2Editable();
  const host = selectedHost();
  const sourceRefs = node?.sourceRefs?.slice(0, 3) || [];
  const assignable = node && !["host", "workspace"].includes(node.kind);
  const selectedNodeHost = node?.kind === "host"
    ? state.hosts.find((candidate) => candidate.host_id === node.id)
    : null;
  const actionButtons = node?.kind === "host"
    ? renderHostOperations(selectedNodeHost)
    : !node || !editable ? "" : `
    <div class="projection-actions">
      <button class="command-button" data-m2-action="rename" data-node-id="${escapeHtml(node.id)}"><i data-lucide="pencil-line"></i><span>更名</span></button>
      ${assignable ? `<button class="command-button" data-m2-action="assign" data-node-id="${escapeHtml(node.id)}"><i data-lucide="folder-input"></i><span>绑定项目</span></button>` : ""}
      <button class="command-button" data-m2-action="relation" data-node-id="${escapeHtml(node.id)}"><i data-lucide="waypoints"></i><span>关系</span></button>
      <button class="icon-button" data-m2-action="${node.projectionState === "archived" ? "restore" : "archive"}" data-node-id="${escapeHtml(node.id)}" data-tooltip="${node.projectionState === "archived" ? "恢复节点" : "归档节点"}" aria-label="${node.projectionState === "archived" ? "恢复节点" : "归档节点"}"><i data-lucide="${node.projectionState === "archived" ? "rotate-ccw" : "archive"}"></i></button>
    </div>`;
  return `<section class="projection-workbench hosts-projection-workbench rail-section" aria-label="本地投影管理">
    <div class="projection-head"><div class="section-label"><i data-lucide="layers-3"></i><span>本地投影</span></div><span class="version-badge ${draft?.draft_id && draft.state === "draft" ? "draft" : ""}">${draft?.draft_id ? `${projectionStateLabel(draft.state)} · r${draft.revision}` : "尚无扫描投影"}</span></div>
    <div class="projection-state-grid"><div><span>当前服务器</span><b>${escapeHtml(host?.display_name || "未选择")}</b></div><div><span>项目</span><b>${state.navigation.projects}</b></div></div>
    ${renderM3Diff()}
    ${renderProjectionNodeBrowser(nodes, node)}
    ${node ? `<div class="projection-selection"><div><span class="context-kicker">${escapeHtml(graphKindLabel(node.kind).toUpperCase())}</span><strong>${escapeHtml(node.label)}</strong><small>${escapeHtml(node.observedAt ? formatObservedAt(node.observedAt) : "本地声明")}</small></div><div class="projection-source-list">${sourceRefs.map((source) => `<code>${escapeHtml(source)}</code>`).join("")}</div></div>` : ""}
    ${actionButtons}
    <div class="projection-footer-actions">
      <button class="command-button" data-m2-action="scan-host" ${host ? "" : "disabled"}><i data-lucide="refresh-cw"></i><span>${host ? "重试连接 / 扫描" : "选择服务器"}</span></button>
      ${editable ? `<button class="command-button" data-m2-action="create-project"><i data-lucide="folder-plus"></i><span>新建项目</span></button><button class="command-button emphasis" data-m2-action="confirm"><i data-lucide="check"></i><span>确认投影</span></button>` : ""}
    </div>
    ${renderM3Agent()}
  </section>`;
}

function hostRecommendedAction(host) {
  if (!host) return null;
  if (host.status === "host_key_changed") return { icon: "fingerprint", label: "核对新指纹", hint: "服务器主机密钥与此前记录不同", action: "retry-selected-host" };
  if (host.last_error_code === "SSH_AUTH_FAILED") {
    return { icon: "key-round", label: host.credential_kind === "ssh_key" ? "改用账号密码" : "更新账号密码", hint: "先把服务器登进去；私钥只在明确提供时使用", action: "replace-host-credential" };
  }
  if (["SSH_UNREACHABLE", "SSH_TIMEOUT"].includes(host.last_error_code)) return { icon: "network", label: "编辑地址 / 端口", hint: "SSH 网络路径或端口没有响应", action: "edit-host-connection" };
  if (["connection_ready", "evidence_ready", "discovery_complete", "discovery_partial", "discovery_unavailable"].includes(host.status)) return { icon: "scan-search", label: "重新扫描", hint: "连接可用，重新读取只读证据", action: "retry-selected-host" };
  return { icon: "rotate-ccw", label: "立即重连", hint: "重新执行指纹与 SSH 能力检查", action: "retry-selected-host" };
}

function hostFailureCopy(host) {
  if (host?.last_error_code === "SSH_AUTH_FAILED") {
    if (host.credential_kind === "ssh_key") {
      return "服务器身份已确认，但当前私钥没有通过。系统不会自动扫描本机或 APP_HOST 上的一堆私钥；如果你有账号密码，请直接改用账号密码先登录。";
    }
    return "服务器身份已确认，但 SSH 拒绝了这组账号密码。请核对 SSH 用户和密码；若使用 root，服务器可能禁止 root 密码登录，或要求多因素/多轮交互。";
  }
  return host?.last_error_summary || "连接检查未通过";
}

function renderHostOperations(host) {
  if (!host) return "";
  const recommendation = hostRecommendedAction(host);
  const connectionError = !["DOCKER_PERMISSION_DENIED", "DOCKER_UNAVAILABLE", "COMPOSE_UNAVAILABLE"].includes(host.last_error_code)
    ? host.last_error_code
    : null;
  const error = connectionError
    ? `<div class="host-failure-detail"><span>${escapeHtml(host.last_error_code)}</span><p>${escapeHtml(hostFailureCopy(host))}</p><small>${escapeHtml(host.last_checked_at ? formatObservedAt(host.last_checked_at) : "尚未检查")}</small></div>`
    : `<div class="host-failure-detail quiet"><span>${escapeHtml(hostStatusLabel(host.status))}</span><p>当前没有连接错误记录。</p><small>${escapeHtml(host.last_checked_at ? formatObservedAt(host.last_checked_at) : "尚未检查")}</small></div>`;
  return `<div class="host-operations" aria-label="服务器操作">
    ${error}
    <button class="host-primary-action" data-m2-action="${recommendation.action}" data-node-id="${escapeHtml(host.host_id)}"><i data-lucide="${recommendation.icon}"></i><span><b>${recommendation.label}</b><small>${recommendation.hint}</small></span></button>
    <div class="host-operation-grid">
      <button class="command-button" data-m2-action="retry-selected-host" data-node-id="${escapeHtml(host.host_id)}"><i data-lucide="rotate-ccw"></i><span>立即重连</span></button>
      <button class="command-button" data-m2-action="edit-host-connection" data-node-id="${escapeHtml(host.host_id)}"><i data-lucide="settings-2"></i><span>编辑连接</span></button>
      <button class="command-button" data-m2-action="replace-host-credential" data-node-id="${escapeHtml(host.host_id)}"><i data-lucide="key-round"></i><span>更新登录凭据</span></button>
      <button class="command-button danger" data-m4-delete="host" data-scope-id="${escapeHtml(host.host_id)}"><i data-lucide="trash-2"></i><span>删除服务器</span></button>
    </div>
    <small class="host-operation-note">删除仅移除本地登记、扫描结果、投影和凭据引用；不会登录、重启或修改远端服务器。</small>
  </div>`;
}

function renderM2Modal() {
  const modal = state.m2.modal;
  if (!modal) return "";
  const setup = state.m2.setup || {};
  let content = "";
  if (modal.type === "host") {
    if (modal.phase === "fingerprint") {
      const endpoint = setup.address ? `${setup.address}:${setup.port || "22"}` : "这台服务器";
      content = `<div class="m2-dialog-head"><div><span>确认服务器身份</span><h2>${escapeHtml(setup.displayName || setup.hostId || "Linux HOST")}</h2></div><button class="icon-button" data-m2-action="close-modal" data-tooltip="关闭" aria-label="关闭"><i data-lucide="x"></i></button></div><div class="m2-fingerprint-copy"><strong>这一步只确认服务器身份，不校验 SSH 用户或密码。</strong><span>系统从 ${escapeHtml(endpoint)} 读取 SSH 主机公钥的 SHA256 指纹；请与服务器控制台或服务商提供的指纹核对。</span><span>确认后，系统会固定这台服务器的身份，再发送 SSH 登录请求。</span></div><div class="m2-fingerprint"><code>${escapeHtml(setup.fingerprint || "")}</code></div><div class="m2-dialog-actions"><button class="command-button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" data-m2-action="confirm-host-key" ${state.m2.busy ? "disabled" : ""}><i data-lucide="fingerprint"></i><span>确认身份并继续登录</span></button></div>`;
    } else if (modal.phase === "scanning") {
      const connectionReady = ["connection_ready", "docker_unavailable", "docker_permission_denied"].includes(setup.connectionState)
        ? `<span class="m2-connection-ready"><i data-lucide="shield-check"></i>SSH 登录成功 · ${escapeHtml(setup.sshUser || "SSH 用户")}@${escapeHtml(setup.address || "HOST")}:${escapeHtml(setup.port || "22")}</span>`
        : "";
      const providerLabel = (setup.providerKinds || []).map((kind) => ({ docker: "Docker", compose: "Docker Compose", systemd: "systemd 服务" })[kind] || kind).join(" / ") || "已登记的发现方式";
      content = `<div class="m2-dialog-head"><div><span>HOST 扫描</span><h2>${escapeHtml(setup.displayName || setup.hostId || "Linux HOST")}</h2></div></div><div class="m2-scan-state"><i data-lucide="loader-circle"></i>${connectionReady}<strong>${escapeHtml(setup.runState === "running" ? `正在读取 ${providerLabel} 只读证据` : `等待 ${providerLabel} 扫描任务`)}</strong><small>${escapeHtml(setup.runId || "")}</small></div>`;
    } else if (modal.phase === "error") {
      const credentialRecovery = setup.hostId && setup.error?.code === "SSH_AUTH_FAILED" ? `<button class="command-button emphasis" data-m2-action="replace-host-credential" data-node-id="${escapeHtml(setup.hostId)}"><i data-lucide="key-round"></i><span>改用账号密码登录</span></button>` : "";
      content = `<div class="m2-dialog-head"><div><span>HOST 初始化</span><h2>未完成</h2></div><button class="icon-button" data-m2-action="close-modal" data-tooltip="关闭" aria-label="关闭"><i data-lucide="x"></i></button></div>${renderM2Error(setup.error)}<div class="m2-dialog-actions">${credentialRecovery}<button class="command-button" data-m2-action="retry-selected-host"><i data-lucide="rotate-ccw"></i><span>重试当前服务器</span></button><button class="command-button" data-m2-action="register-host"><i data-lucide="plus"></i><span>登记其他服务器</span></button></div>`;
    } else {
      content = `<div class="m2-dialog-head"><div><span>连接 Linux HOST</span><h2>初始化观察入口</h2></div><button class="icon-button" data-m2-action="close-modal" data-tooltip="关闭" aria-label="关闭"><i data-lucide="x"></i></button></div>${renderM2Error(setup.error)}<form class="m2-form" data-m2-form="host"><label>显示名称<input name="display_name" maxlength="120" required value="${escapeHtml(setup.displayName || "")}" /></label><div class="m2-form-row"><label>地址<input name="address" inputmode="url" required value="${escapeHtml(setup.address || "")}" /></label><label>端口<input name="port" type="number" min="1" max="65535" required value="${escapeHtml(setup.port || "22")}" /></label></div><label>SSH 用户<input name="ssh_user" required value="${escapeHtml(setup.sshUser || "")}" /></label>${renderSshCredentialFields(setup.credentialKind || "ssh_password")}<div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit" ${state.m2.busy ? "disabled" : ""}><i data-lucide="arrow-right"></i><span>检查指纹</span></button></div></form>`;
    }
  } else {
    content = renderM2EditorModal(modal);
  }
  return `<div class="m2-modal-scrim" role="presentation"><section class="m2-dialog" role="dialog" aria-modal="true">${content}</section></div>`;
}

function m2ErrorMessage(error) {
  if (error?.code === "SSH_AUTH_FAILED") {
    return "SSH 登录凭据被服务器拒绝。服务器身份已确认；如果当前走私钥，请改用账号密码先登录。系统不会自动查找本机私钥。";
  }
  if (error?.code === "DOCKER_PERMISSION_DENIED") {
    return "服务器连接已经成功；附加扫描未完成。";
  }
  if (error?.code === "DOCKER_UNAVAILABLE") {
    return "服务器连接已经成功；附加扫描未完成。";
  }
  return error?.message || "请求失败";
}

function renderM2Error(error) {
  if (!error) return "";
  return `<div class="m2-form-error"><i data-lucide="circle-alert"></i><span><b>${escapeHtml(error.code || "REQUEST_FAILED")}</b><small>${escapeHtml(m2ErrorMessage(error))}</small></span></div>`;
}

function renderSshCredentialFields(kind = "ssh_password") {
  const password = kind !== "ssh_key";
  return `<fieldset class="ssh-credential-fields">
    <legend>登录方式</legend>
    <div class="ssh-credential-options" role="radiogroup" aria-label="SSH 登录方式">
      <label><input type="radio" name="credential_kind" value="ssh_password" ${password ? "checked" : ""} /><span><b>SSH 账号密码</b><small>默认 · 先把服务器登进去</small></span></label>
      <label><input type="radio" name="credential_kind" value="ssh_key" ${password ? "" : "checked"} /><span><b>SSH 私钥</b><small>明确知道对应私钥时使用</small></span></label>
    </div>
    <label data-ssh-credential-field="ssh_password" ${password ? "" : "hidden"}>SSH 密码<input name="password" type="password" maxlength="4096" ${password ? "required" : ""} autocomplete="new-password" /></label>
    <label data-ssh-credential-field="ssh_key" ${password ? "hidden" : ""}>SSH 私钥<textarea name="private_key" rows="8" ${password ? "" : "required"} spellcheck="false" autocomplete="off"></textarea></label>
    <small class="ssh-credential-note">默认用账号密码先登录；系统不会自动扫描本机或 APP_HOST 的一堆私钥。选择私钥时只使用你明确粘贴的这一把；后续托管密钥会做成单独确认操作。</small>
  </fieldset>`;
}

function renderM3OperationEditor(operation, index) {
  const prefix = `operation_${index}`;
  if (operation.op === "rename") return `<div class="m3-operation-editor"><span>${index + 1} · 更名</span><label>名称<input name="${prefix}_label" maxlength="120" required value="${escapeHtml(operation.label || "")}" /></label></div>`;
  if (operation.op === "assign_project") return `<div class="m3-operation-editor"><span>${index + 1} · 项目归类</span><label>项目<select name="${prefix}_project_id"><option value="">未归类</option>${Object.entries(projects).map(([id, project]) => `<option value="${escapeHtml(id)}" ${operation.project_id === id ? "selected" : ""}>${escapeHtml(project.label)}</option>`).join("")}</select></label></div>`;
  if (operation.op === "create_project") return `<div class="m3-operation-editor"><span>${index + 1} · 新建项目</span><label>项目 ID<input name="${prefix}_project_id" maxlength="96" required value="${escapeHtml(operation.project_id || "")}" /></label><label>名称<input name="${prefix}_label" maxlength="120" required value="${escapeHtml(operation.label || "")}" /></label><label>说明<input name="${prefix}_subtitle" maxlength="240" value="${escapeHtml(operation.subtitle || "")}" /></label></div>`;
  if (operation.op === "add_relation") return `<div class="m3-operation-editor"><span>${index + 1} · 添加关系</span><label>目标<select name="${prefix}_to">${projectionWorkbenchNodes().filter((node) => node.id !== operation.from).map((node) => `<option value="${escapeHtml(node.id)}" ${operation.to === node.id ? "selected" : ""}>${escapeHtml(node.label)}</option>`).join("")}</select></label><label>名称<input name="${prefix}_label" maxlength="120" required value="${escapeHtml(operation.label || "")}" /></label></div>`;
  if (operation.op === "move") return `<div class="m3-operation-editor"><span>${index + 1} · 移动节点</span><div class="m2-form-row"><label>X<input name="${prefix}_x" type="number" step="1" required value="${Number(operation.position?.x || 0)}" /></label><label>Y<input name="${prefix}_y" type="number" step="1" required value="${Number(operation.position?.y || 0)}" /></label></div></div>`;
  return `<div class="m3-operation-editor compact"><span>${index + 1} · ${escapeHtml(proposalOperationLabel(operation))}</span><small>${escapeHtml(operation.node_id || operation.edge_id || "保持原建议")}</small></div>`;
}

function renderM2EditorModal(modal) {
  const close = `<button class="icon-button" data-m2-action="close-modal" data-tooltip="关闭" aria-label="关闭"><i data-lucide="x"></i></button>`;
  if (["host-alias", "host-connection", "host-credential"].includes(modal.type)) {
    const host = state.hosts.find((candidate) => candidate.host_id === modal.hostId);
    if (!host) return `<div class="m2-dialog-head"><div><span>服务器</span><h2>记录不存在</h2></div>${close}</div>`;
    if (modal.type === "host-credential") {
      return `<div class="m2-dialog-head"><div><span>服务器登录凭据</span><h2>${escapeHtml(host.display_name)}</h2></div>${close}</div><div class="host-secret-note"><i data-lucide="shield-check"></i><span>当前方式：${host.credential_kind === "ssh_password" ? "SSH 账号密码" : "SSH 私钥"}。本次更新默认改用账号密码；如果你明确知道对应私钥，也可手动切换到 SSH 私钥。旧凭据不会回显；保存后会重新核对主机指纹。</span></div><form class="m2-form" data-m2-form="host-credential">${renderSshCredentialFields("ssh_password")}<div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit"><i data-lucide="key-round"></i><span>保存并重连</span></button></div></form>`;
    }
    return `<div class="m2-dialog-head"><div><span>服务器连接信息</span><h2>${escapeHtml(host.display_name)}</h2></div>${close}</div><div class="host-secret-note"><i data-lucide="info"></i><span>修改地址、端口或 SSH 用户后，需要重新核对主机指纹。</span></div><form class="m2-form" data-m2-form="host-connection"><label>服务器别名<input name="display_name" maxlength="120" required value="${escapeHtml(host.display_name)}" /></label><div class="m2-form-row"><label>地址<input name="address" required value="${escapeHtml(host.address)}" /></label><label>端口<input name="port" type="number" min="1" max="65535" required value="${Number(host.port)}" /></label></div><label>SSH 用户<input name="ssh_user" maxlength="64" required value="${escapeHtml(host.ssh_user || "")}" /></label><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit">保存连接信息</button></div></form>`;
  }
  if (modal.type === "model") {
    const model = state.m3.model || {};
    return `<div class="m2-dialog-head"><div><span>OpenAI 兼容</span><h2>模型设置</h2></div>${close}</div>${renderM2Error(state.m3.error)}<form class="m2-form" data-m3-form="model"><label>URL<input name="base_url" type="url" required value="${escapeHtml(model.base_url || "")}" placeholder="https://HOST/v1" /></label><label>模型<input name="model" maxlength="160" required value="${escapeHtml(model.model || "")}" /></label><label>Key<input name="api_key" type="password" maxlength="16384" required autocomplete="new-password" /></label><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit" ${state.m3.busy ? "disabled" : ""}><i data-lucide="plug-zap"></i><span>保存并测试</span></button></div></form>`;
  }
  const draft = activeM2Draft();
  const workbenchNodes = projectionWorkbenchNodes();
  const node = workbenchNodes.find((candidate) => candidate.id === modal.nodeId);
  if (!draft) return `<div class="m2-dialog-head"><div><span>本地投影</span><h2>草稿不可用</h2></div>${close}</div>`;
  if (modal.type === "proposal-modify") {
    const proposal = state.m3.session?.proposals?.find((candidate) => candidate.proposal_id === modal.proposalId);
    if (!proposal) return `<div class="m2-dialog-head"><div><span>Agent 建议</span><h2>建议不可用</h2></div>${close}</div>`;
    return `<div class="m2-dialog-head"><div><span>修改后采用</span><h2>${escapeHtml(proposal.title)}</h2></div>${close}</div><form class="m2-form m3-operation-form" data-m3-form="proposal-modify" data-proposal-id="${escapeHtml(proposal.proposal_id)}">${proposal.patch.map(renderM3OperationEditor).join("")}<div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit" ${state.m3.busy ? "disabled" : ""}><i data-lucide="check"></i><span>采用修改</span></button></div></form>`;
  }
  if (modal.type === "rename") return `<div class="m2-dialog-head"><div><span>显示名称</span><h2>${escapeHtml(node?.label || "")}</h2></div>${close}</div><form class="m2-form" data-m2-form="rename"><label>扫描原始名称（只读）<input value="${escapeHtml(node?.originalLabel || node?.label || "")}" readonly /></label><label>显示名称（可修改）<input name="label" maxlength="120" required value="${escapeHtml(node?.label || "")}" /></label><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit">保存显示名称</button></div></form>`;
  if (modal.type === "assign") return `<div class="m2-dialog-head"><div><span>绑定项目 / 项目归属</span><h2>${escapeHtml(node?.label || "")}</h2></div>${close}</div><form class="m2-form" data-m2-form="assign"><label>扫描原始名称（只读）<input value="${escapeHtml(node?.originalLabel || node?.label || "")}" readonly /></label><label>项目归属<select name="project_id"><option value="">未绑定项目</option>${Object.entries(projects).map(([id, project]) => `<option value="${escapeHtml(id)}" ${node?.projectId === id ? "selected" : ""}>${escapeHtml(project.label)}</option>`).join("")}</select></label><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit">保存项目绑定</button></div></form>`;
  if (modal.type === "create-project") return `<div class="m2-dialog-head"><div><span>本地项目</span><h2>新建项目</h2></div>${close}</div><form class="m2-form" data-m2-form="create-project"><label>项目 ID<input name="project_id" pattern="[A-Za-z0-9_-]{1,96}" maxlength="96" required /></label><label>名称<input name="label" maxlength="120" required /></label><label>说明<input name="subtitle" maxlength="240" /></label><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit">创建</button></div></form>`;
  if (modal.type === "relation") return `<div class="m2-dialog-head"><div><span>本地关系</span><h2>${escapeHtml(node?.label || "")}</h2></div>${close}</div><form class="m2-form" data-m2-form="relation"><label>目标<select name="to">${workbenchNodes.filter((candidate) => candidate.id !== modal.nodeId).map((candidate) => `<option value="${escapeHtml(candidate.id)}">${escapeHtml(candidate.label)} · ${escapeHtml(graphKindLabel(candidate.kind))}</option>`).join("")}</select></label><div class="m2-form-row"><label>类型<select name="kind"><option value="contains">包含</option><option value="depends_on">依赖</option><option value="connects_to">连接</option><option value="mounts">挂载</option><option value="exposes">暴露</option><option value="documents">文档</option></select></label><label>名称<input name="label" maxlength="120" required value="本地关系" /></label></div><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit">添加</button></div></form>`;
  return `<div class="m2-dialog-head"><div><span>确认投影</span><h2>发布本地版本</h2></div>${close}</div><div class="m2-confirm-copy"><strong>${escapeHtml(draft.draft_id)}</strong><small>r${draft.revision} · ${draft.pending_changes || 0} 项本地变更</small></div><form class="m2-form" data-m2-form="confirm"><div class="m2-dialog-actions"><button class="command-button" type="button" data-m2-action="close-modal">取消</button><button class="command-button emphasis" type="submit"><i data-lucide="check"></i><span>确认</span></button></div></form>`;
}

function canvasTools(kind) {
  const scale = kind === "world" ? state.worldZoom : kind === "workflow" ? state.flowZoom : state.resourceZoom;
  return `<div class="canvas-tools" aria-label="画布工具">
    <button class="icon-button" data-canvas-control="${kind}:zoom-out" data-tooltip="缩小" aria-label="缩小"><i data-lucide="minus"></i></button>
    <span class="zoom-readout">${Math.round(scale * 100)}%</span>
    <button class="icon-button" data-canvas-control="${kind}:zoom-in" data-tooltip="放大" aria-label="放大"><i data-lucide="plus"></i></button>
    <span class="tool-divider"></span>
    <button class="icon-button" data-canvas-control="${kind}:fit" data-tooltip="适应画布" aria-label="适应画布"><i data-lucide="maximize-2"></i></button>
    <button class="icon-button" data-canvas-control="${kind}:reset" data-tooltip="重置布局" aria-label="重置布局"><i data-lucide="rotate-ccw"></i></button>
  </div>`;
}

function renderWorkflowView() {
  const editing = state.workflowMode === "edit";
  const project = projects[state.activeProject];
  const workflowName = workflowNames[state.activeProject];
  const versionLabel = editing ? (state.draftChanges ? `v${state.workflowVersion + 1} 草稿` : `v${state.workflowVersion} 已发布`) : `${runStates[state.selectedRun].label} · ${runStates[state.selectedRun].version}`;
  const selected = selectedFlowNode();
  return `
    ${projectScopeBar()}
    <div class="view-header">
      <div><div class="view-kicker">${project.label.toUpperCase()} / RUN & FLOW</div><h1>${workflowName}</h1><p>这是 ${project.label} 项目内部的运行与流程。运行事实默认呈现；需要改变结构时，再在同一张图上切换到编辑。</p></div>
      <div class="header-metrics"><div class="metric"><span>步骤</span><b>${visibleFlowNodes().length}</b></div><div class="metric"><span>版本</span><b class="mint">v${state.workflowVersion}</b></div><div class="metric"><span>运行中</span><b class="amber">1</b></div></div>
    </div>
    <div class="workflow-bar">
      <div class="workflow-ident"><span class="context-path">${project.label} / ${workflowName}</span><span class="version-badge ${state.draftChanges && editing ? "draft" : ""}">${versionLabel}</span></div>
      <div class="workflow-actions">
        <div class="segmented" aria-label="流程模式">
          <button class="segmented-button ${editing ? "active" : ""}" data-flow-mode="edit"><i data-lucide="pencil-line"></i><span>编辑</span></button>
          <button class="segmented-button ${!editing ? "active" : ""}" data-flow-mode="run"><i data-lucide="activity"></i><span>运行</span></button>
        </div>
        <button class="command-button" data-flow-action="save" ${editing ? "" : "disabled"}><i data-lucide="save"></i><span>保存草稿</span></button>
        <button class="command-button emphasis" data-flow-action="publish" ${editing ? "" : "disabled"}><i data-lucide="upload"></i><span>发布</span></button>
      </div>
    </div>
    <div class="canvas-stage" id="workflow-stage">
      <div class="canvas-topline"><span class="canvas-badge"><span class="status-dot ${editing ? "amber" : "live"}"></span>${editing ? (state.draftChanges ? "未发布流程草稿" : "可编辑流程版本") : "运行实例 · 只读事实"}</span><span class="canvas-legend">${editing ? `<span class="legend-entry"><i class="legend-line dashed"></i>定义路径</span><span class="legend-entry"><i class="legend-line amber"></i>恢复路径</span>` : `<span class="legend-entry"><i class="legend-line live"></i>实际路径</span><span class="legend-entry"><i class="legend-line"></i>未命中分支</span>`}</span></div>
      ${renderFlowPalette(editing)}
      ${renderWorkflowSvg()}
      ${canvasTools("workflow")}
      <div class="canvas-footer-note"><i data-lucide="${editing ? "mouse-pointer-2" : "lock"}"></i><span>${editing ? "拖动步骤调整草稿布局；新增节点会插入调用能力与验证之间。" : "运行模式记录这一次的路径与回执，不能改写流程结构。"}</span></div>
    </div>
    ${selectionCard(workflowContext(selected), "workflow-selection-card", "当前步骤")}
    ${renderProjectContextOrbit()}
    ${renderRunTimeline()}
    ${renderWorkflowBottom(editing)}`;
}

function renderFlowPalette(editing) {
  return `<div class="node-palette ${editing ? "" : "hidden"}" aria-label="流程节点工具箱"><div class="palette-heading">添加步骤</div>${Object.entries(paletteKinds).map(([id, kind]) => `<button class="palette-button" data-add-node="${id}"><i data-lucide="${kind.icon}"></i><span>${kind.label}</span></button>`).join("")}</div>`;
}

function visibleFlowNodes() {
  return state.workflowMode === "run" ? state.runSnapshot : state.workflowNodes;
}

function selectedFlowNode() {
  const nodes = visibleFlowNodes();
  return nodes.find((node) => node.id === state.selectedFlowNode) || nodes[0];
}

function workflowContext(node) {
  const edit = state.workflowMode === "edit";
  const data = workflowSteps[node.id] || (edit ? { definition: ["新增步骤", node.subtitle, node.draftOnly ? "尚未发布" : "已发布"] } : { run: ["未包含", "-", "RUN-028 基于 v3"] });
  const facts = edit
    ? [["类型", data.definition[0]], ["规则", data.definition[1]], ["状态", data.definition[2]], ["版本", state.draftChanges ? `v${state.workflowVersion + 1} 草稿` : `v${state.workflowVersion}`]]
    : [["开始", data.run[0]], ["耗时", data.run[1]], ["状态", data.run[2]], ["版本", runStates[state.selectedRun].version]];
  return { kicker: edit ? "FLOW STEP · EDIT" : "RUN STEP · READ ONLY", title: node.title, summary: edit ? "此处定义每一次执行都必须遵守的步骤与规则。" : "此处记录本次执行的时间、结果和外部回执。", facts, action: edit ? { label: "定位到运行事实", type: "switch-run" } : { label: "编辑流程定义", type: "switch-edit" } };
}

function projectContextResourceIds() {
  const profile = projectResources[state.activeProject] || { privateResources: [], sharedResources: [] };
  return profile.privateResources.concat(profile.sharedResources);
}

function resourceIcon(type = "") {
  const value = type.toLowerCase();
  if (value.includes("知识") || value.includes("索引")) return "book-open";
  if (value.includes("服务器") || value.includes("服务")) return "server";
  if (value.includes("代码") || value.includes("仓库")) return "git-branch";
  if (value.includes("域名") || value.includes("入口")) return "globe-2";
  if (value.includes("存储")) return "database";
  if (value.includes("凭据") || value.includes("令牌")) return "key-round";
  return "box";
}

function projectContextResourceMarkup(id) {
  const resource = resources[id];
  if (!resource) return "";
  const shared = resource.scope === "共享资源";
  return `<button class="context-resource-node ${shared ? "shared" : "private"}" data-project-context-resource="${id}" title="查看 ${escapeHtml(resource.label)}"><span class="context-resource-icon"><i data-lucide="${resourceIcon(resource.type)}"></i></span><span class="context-resource-copy"><b>${escapeHtml(resource.label)}</b><small>${escapeHtml(resource.type)}</small></span><span class="context-resource-state ${resource.tone}">${shared ? "共享" : "私有"}</span></button>`;
}

function renderProjectContextOrbit() {
  const project = projects[state.activeProject];
  const ids = projectContextResourceIds();
  const privateIds = ids.filter((id) => resources[id]?.scope !== "共享资源");
  const sharedIds = ids.filter((id) => resources[id]?.scope === "共享资源");
  return `<section class="project-context-panel" aria-label="项目外部上下文">
    <div class="subsection-head"><div><div class="section-label"><i data-lucide="orbit"></i><span>项目外部上下文</span><small>不属于流程路径的依赖</small></div><p>项目运行会调用这些已有对象；点击节点进入对应资源视图。</p></div><button class="text-action" data-project-context-resources><span>打开项目资源</span><i data-lucide="arrow-up-right"></i></button></div>
    <div class="context-orbit">
      <div class="context-orbit-side left"><div class="orbit-caption"><span class="status-dot blue"></span>项目私有</div>${privateIds.length ? privateIds.map(projectContextResourceMarkup).join("") : `<span class="orbit-empty">尚未登记私有资源</span>`}</div>
      <div class="context-orbit-center"><span class="orbit-center-kicker">PROJECT CONTEXT</span><strong>${escapeHtml(project.label)}</strong><small>${ids.length} 个外部依赖 · ${sharedIds.length} 个共享引用</small></div>
      <div class="context-orbit-side right"><div class="orbit-caption"><span class="status-dot green"></span>共享引用</div>${sharedIds.length ? sharedIds.map(projectContextResourceMarkup).join("") : `<span class="orbit-empty">没有共享引用</span>`}</div>
    </div>
  </section>`;
}

function runTimelineForProject() {
  const base = projectRunTimelines[state.activeProject];
  if (!base) return [];
  const run = runStates[state.selectedRun];
  return base.map((event) => {
    if (run.skipped.includes(event.node)) return { ...event, state: "跳过", tone: "muted", receipt: "skipped" };
    if (run.current === event.node) return { ...event, state: "当前", tone: "amber" };
    if (run.path.includes(event.node)) return { ...event, state: state.selectedRun === "run-027" && event.node === "result" ? "已验证" : "完成", tone: "green", receipt: state.selectedRun === "run-027" && event.node === "result" ? "verified" : event.receipt };
    return { ...event, state: "未执行", tone: "muted", receipt: "not-run" };
  });
}

function renderRunTimeline() {
  const events = runTimelineForProject();
  if (!events.length) {
    return `<section class="run-timeline" aria-label="项目运行轨迹"><div class="subsection-head timeline-head"><div><div class="section-label"><i data-lucide="route"></i><span>项目运行轨迹</span><small>尚无运行实例</small></div><p>当前项目还没有可核验的运行回执；接入事件源后，步骤、耗时和外部回执会在这里出现。</p></div><div class="timeline-summary"><span class="muted">待接入</span></div></div><div class="timeline-empty"><span class="timeline-empty-icon"><i data-lucide="clock-3"></i></span><strong>尚未捕获运行轨迹</strong><small>不借用其他项目的运行数据。</small></div></section>`;
  }
  const run = runStates[state.selectedRun];
  return `<section class="run-timeline" aria-label="项目运行轨迹">
    <div class="subsection-head timeline-head"><div><div class="section-label"><i data-lucide="route"></i><span>项目运行轨迹</span><small>${run.label} · ${run.version}</small></div><p>按时间还原本次执行的步骤、耗时与外部回执；跳过和等待都保留。</p></div><div class="timeline-summary"><span><b>${events.length}</b> 步</span><span><b>${run.elapsed}</b> 已用</span><span class="${run.status === "等待回执" ? "amber" : "green"}">${run.status}</span></div></div>
    <div class="timeline-track">${events.map((event, index) => `<button class="timeline-event ${event.tone} ${state.selectedTrace === event.id ? "selected" : ""}" data-run-event="${event.id}" data-run-node="${event.node}" aria-label="查看 ${event.title} 运行事实"><span class="timeline-connector ${index === events.length - 1 ? "last" : ""}"></span><span class="timeline-marker"><i data-lucide="${event.state === "跳过" ? "minus" : event.state === "当前" ? "loader-circle" : "check"}"></i></span><span class="timeline-time">${escapeHtml(event.time)}</span><strong>${escapeHtml(event.title)}</strong><small>${escapeHtml(event.detail)}</small><span class="timeline-facts"><em>${escapeHtml(event.duration)}</em><em>${escapeHtml(event.receipt)}</em></span><span class="timeline-state">${escapeHtml(event.state)}</span></button>`).join("")}</div>
  </section>`;
}

function renderWorkflowSvg() {
  const nodes = visibleFlowNodes();
  const run = state.workflowMode === "run";
  const content = `${flowBoundaries(run)}${flowEdges(nodes, run)}${nodes.map((node) => flowNodeMarkup(node, run)).join("")}`;
  return graphSvg("workflow-canvas", "workflow-scene", content, state.flowPan, state.flowZoom, "流程编辑与运行画布");
}

function flowBoundaries(run) {
  return `<rect class="boundary" x="24" y="54" width="952" height="454" rx="30"></rect><text class="boundary-label" x="47" y="80">${run ? `${runStates[state.selectedRun].label} · VERSION ${runStates[state.selectedRun].version.toUpperCase()}` : `WORKFLOW DEFINITION · ${state.draftChanges ? `DRAFT V${state.workflowVersion + 1}` : `VERSION ${state.workflowVersion}`}`}</text>`;
}

function flowEdges(nodes, run) {
  const find = (id) => nodes.find((node) => node.id === id);
  const edge = (id, fromId, toId, tone, label = "") => {
    const from = find(fromId);
    const to = find(toId);
    if (!from || !to) return "";
    const marker = tone.includes("fallback") ? "arrow-amber" : tone.includes("hidden") ? "arrow-muted" : "arrow-mint";
    const center = midpoint(from, to);
    return `<g><path id="${id}" class="graph-edge ${tone}" d="${curvePath(from, to)}" marker-end="url(#${marker})"></path>${label ? `<text class="edge-label" x="${center.x}" y="${center.y - 7}" text-anchor="middle">${label}</text>` : ""}</g>`;
  };
  const pieces = flowEdgeSpecs(nodes, run).map((item) => edge(item.id, item.from, item.to, item.tone, item.label));
  const activeRun = runStates[state.selectedRun];
  if (run && activeRun.current) pieces.push(signalDot("flow-result"));
  return pieces.join("");
}

function flowEdgeSpecs(nodes, run) {
  const draftNodes = nodes.filter((node) => node.draftOnly);
  const activeRun = runStates[state.selectedRun];
  const activeTone = (from, to) => (activeRun.path.includes(from) && activeRun.path.includes(to) ? "active-path" : "hidden");
  const branchTone = run ? "hidden" : "dashed";
  const mainTone = run ? activeTone("trigger", "understand") : "dashed";
  const specs = [
    { id: "flow-1", from: "trigger", to: "understand", tone: mainTone },
    { id: "flow-2", from: "understand", to: "route", tone: run ? activeTone("understand", "route") : "dashed" },
    { id: "flow-approval", from: "route", to: "approval", tone: branchTone, label: run ? "未命中" : "高影响" },
    { id: "flow-approval-result", from: "approval", to: "result", tone: branchTone },
    { id: "flow-execute", from: "route", to: "execute", tone: run ? activeTone("route", "execute") : "dashed", label: run ? "实际路径" : "低影响" },
  ];
  if (draftNodes.length && !run) {
    specs.push({ id: "flow-draft-start", from: "execute", to: draftNodes[0].id, tone: "dashed" });
    draftNodes.slice(1).forEach((node, index) => specs.push({ id: `flow-draft-${index}`, from: draftNodes[index].id, to: node.id, tone: "dashed" }));
    specs.push({ id: "flow-draft-end", from: draftNodes[draftNodes.length - 1].id, to: "result", tone: "dashed" });
  } else {
    specs.push({ id: "flow-result", from: "execute", to: "result", tone: run ? activeTone("execute", "result") : "dashed" });
  }
  specs.push({ id: "flow-fallback", from: "result", to: "route", tone: run ? "hidden" : "fallback", label: "恢复 / 重试" });
  return specs;
}

function flowNodeMarkup(node, run) {
  const runInfo = runStates[state.selectedRun];
  const onPath = runInfo.path.includes(node.id);
  const skipped = runInfo.skipped.includes(node.id);
  const current = runInfo.current === node.id;
  const selected = state.selectedFlowNode === node.id;
  const states = run
    ? { state: current ? "当前" : skipped ? "跳过" : onPath ? (state.selectedRun === "run-027" && node.id === "result" ? "已验证" : "完成") : "未执行", tone: current ? "" : skipped ? "muted" : onPath ? "green" : "muted", subtitle: current ? "等待回执 · 12 s" : skipped ? "本次条件未命中" : node.id === "execute" ? "1.8 s · 请求已发出" : node.subtitle }
    : { state: node.draftOnly ? "草稿" : node.kind === "CONDITION" ? "2 分支" : node.kind === "APPROVAL" ? "闸门" : "步骤", tone: node.draftOnly ? "amber" : "", subtitle: node.subtitle };
  return `<g class="graph-node flow-node ${run ? "" : "draggable"} ${skipped ? "skipped" : ""} ${current ? "current" : ""} ${node.draftOnly ? "draft-node" : ""} ${selected ? "selected" : ""}" id="flow-node-${node.id}" data-flow-node="${node.id}" role="button" tabindex="0" aria-label="查看步骤 ${node.title}" transform="translate(${node.x} ${node.y})">
    <rect class="node-surface" width="${node.width}" height="${node.height}" rx="8"></rect>
    <text class="node-kicker" x="13" y="19">${node.kind}</text>
    <text class="node-state ${states.tone}" x="${node.width - 12}" y="19" text-anchor="end">${states.state}</text>
    <text class="node-title" x="13" y="43">${escapeHtml(node.title)}</text>
    <text class="node-subtitle" x="13" y="62">${escapeHtml(states.subtitle)}</text>
  </g>`;
}

function renderWorkflowBottom(editing) {
  if (editing) {
    return `<div class="workflow-bottom"><div class="draft-footer"><span class="status-dot ${state.draftChanges ? "amber" : "green"}"></span><span>${state.draftChanges ? `${state.draftChanges} 项未发布变更` : `v${state.workflowVersion} 已发布，尚未修改`}</span></div><span class="run-footer-fact">运行实例仍固定使用 <b>v3</b></span></div>`;
  }
  const run = runStates[state.selectedRun];
  return `<div class="workflow-bottom"><span class="workflow-bottom-label">运行实例</span><div class="run-selector">${Object.entries(runStates).map(([id, item]) => `<button class="run-chip ${state.selectedRun === id ? "active" : ""}" data-run="${id}">${item.label} · ${item.status}</button>`).join("")}</div><span class="run-footer-fact">${run.label} · ${run.version} · <b>${run.elapsed}</b></span></div>`;
}

function renderResourceView() {
  const inProject = state.scope === "project";
  if (inProject) {
    state.resourceProject = state.activeProject;
    state.resourceLens = "project";
  } else if (state.resourceLens === "project") {
    state.resourceLens = "shared";
  }
  // HOST connection and discovery management lives at #scope=global&view=hosts.
  // This route remains the shared-resource collection and impact view.
  const project = projects[state.resourceProject];
  const profile = projectResources[state.resourceProject] || { privateResources: [], sharedResources: [] };
  const sharedResourceEntries = Object.entries(resources).filter(([, resource]) => resource.scope === "共享资源");
  const sharedResources = sharedResourceEntries.map(([, resource]) => resource);
  const sharedResourceIds = sharedResourceEntries.map(([id]) => id);
  const sharedProjectCount = new Set(sharedResources.flatMap((resource) => resource.projects || [])).size;
  const projectResourceIds = profile.privateResources.concat(profile.sharedResources);
  if (inProject && projectResourceIds.length && !projectResourceIds.includes(state.selectedResource)) state.selectedResource = projectResourceIds[0];
  if (inProject && !projectResourceIds.length) state.contextOpen = false;
  if (!inProject && !sharedResourceIds.includes(state.selectedResource)) state.selectedResource = sharedResourceIds[0] || null;
  const attentionCount = inProject
    ? profile.privateResources.concat(profile.sharedResources).filter((id) => resources[id]?.tone === "amber").length
    : sharedResources.filter((resource) => resource.tone === "amber").length;
  return `
    ${inProject ? projectScopeBar() : ""}
    <div class="view-header">
      <div><div class="view-kicker">${inProject ? `${project.label.toUpperCase()} / PROJECT RESOURCES` : "GLOBAL RESOURCE NETWORK"}</div><h1>${inProject ? `${project.label} 项目资源` : "全局资源与共享影响"}</h1><p>${inProject ? "只呈现当前项目的私有资源、共享引用与边界；共享实体可跳到全局层查看完整影响。" : "共享资源在全局只出现一次；从集合盘点资源，也可从单一资源反查所有使用项目与工作负载。"}</p></div>
      <div class="header-metrics">${inProject ? `<div class="metric"><span>私有</span><b>${profile.privateResources.length}</b></div><div class="metric"><span>共享引用</span><b class="mint">${profile.sharedResources.length}</b></div>` : `<div class="metric"><span>共享实体</span><b>${sharedResources.length}</b></div><div class="metric"><span>使用项目</span><b class="mint">${sharedProjectCount}</b></div>`}<div class="metric"><span>需关注</span><b class="amber">${attentionCount}</b></div></div>
    </div>
    <div class="resource-controls">
      <div class="resource-lenses">
        ${inProject ? `<span class="scope-chip"><i data-lucide="folder-kanban"></i>${project.label} 边界</span>` : `${resourceLensButton("shared", "共享集合", "全局盘点")}${resourceLensButton("reverse", "资源反查", "影响分析")}`}
      </div>
      ${inProject ? `<button class="text-action global-resource-link" data-open-global-resources><span>查看全局资源</span><i data-lucide="arrow-up-right"></i></button>` : ""}
    </div>
    <div class="canvas-stage" id="resource-stage">
      <div class="canvas-topline"><span class="canvas-badge"><span class="status-dot ${state.resourceLens === "reverse" ? "amber" : "live"}"></span>${resourceLensTitle()}</span><span class="canvas-legend">${resourceLegend()}</span></div>
      ${renderResourceSvg()}
      ${canvasTools("resource")}
      <div class="canvas-footer-note"><i data-lucide="${state.resourceLens === "reverse" ? "route" : "layers"}"></i><span>${resourceFooterCopy()}</span></div>
    </div>
    ${state.selectedResource && resources[state.selectedResource] ? selectionCard(resourceContext(), "resource-selection-card", "当前资源") : ""}
    ${renderResourceStrip()}`;
}

function monitorQualityBadge(metric) {
  const quality = metric?.quality;
  return `<span class="monitor-quality ${monitorQualityTone(quality)}">${escapeHtml(monitorQualityLabel(quality))}</span>`;
}

function monitorRunNotice(latestRun, snapshot) {
  if (!latestRun) return "";
  const stateValue = latestRun.state;
  const snapshotIsPrevious = Boolean(snapshot?.run_id && latestRun.run_id && snapshot.run_id !== latestRun.run_id);
  if (["failed", "timed_out", "interrupted"].includes(stateValue)) {
    const code = latestRun.failure_code || stateValue.toUpperCase();
    return `<div class="monitor-run-notice red"><i data-lucide="circle-alert"></i><span><b>最新采集失败 · ${escapeHtml(code)}</b>${latestRun.failure_summary ? `<small>${escapeHtml(latestRun.failure_summary)}</small>` : ""}${snapshotIsPrevious ? `<small>正在展示 ${escapeHtml(formatObservedAt(snapshot.observed_at))} 的上一次有效快照</small>` : ""}</span></div>`;
  }
  if (stateValue === "partial") {
    return `<div class="monitor-run-notice amber"><i data-lucide="circle-dashed"></i><span><b>最近一次为部分采集</b><small>不可用指标保持未知，不用 0 代替。</small></span></div>`;
  }
  if (["queued", "running"].includes(stateValue) && snapshot) {
    return `<div class="monitor-run-notice blue"><i data-lucide="loader-circle"></i><span><b>${escapeHtml(monitorRunStateLabel(stateValue))}</b><small>新快照完成前继续展示当前有效快照。</small></span></div>`;
  }
  if (stateValue === "skipped_overlap") {
    return `<div class="monitor-run-notice amber"><i data-lucide="copy-x"></i><span><b>本次采集因重叠被跳过</b><small>${snapshot ? "当前有效快照保持不变。" : "当前仍没有有效资源快照。"}</small></span></div>`;
  }
  return "";
}

function monitorRequestError(hostId) {
  const error = state.monitoring.errorByHost[hostId];
  if (!error) return "";
  return `<div class="monitor-run-notice red"><i data-lucide="wifi-off"></i><span><b>资源采集请求异常 · ${escapeHtml(error.code || "MONITOR_REQUEST_FAILED")}</b><small>${escapeHtml(error.message || "请稍后重试")}</small></span></div>`;
}

function renderFilesystemRows(filesystems) {
  if (!filesystems.length) return `<div class="monitor-list-empty">文件系统数据未知</div>`;
  return filesystems.map((filesystem) => `<div class="monitor-list-row">
    <span class="monitor-list-ident"><b>${escapeHtml(filesystem.mount || "挂载点未知")}</b></span>
    <span><small>容量</small><b>${escapeHtml(formatMetricBytes(filesystem.used_bytes))} / ${escapeHtml(formatMetricBytes(filesystem.size_bytes))}</b></span>
    <span><small>可分配占用</small><b>${escapeHtml(formatMetricRatio(filesystem.allocatable_used_ratio))}</b></span>
    <span><small>inode 占用</small><b class="${monitorQualityTone(filesystem.inode_quality)}">${escapeHtml(formatMetricRatio(filesystem.inode_used_ratio))}</b></span>
  </div>`).join("");
}

function renderDiskIoRows(disks) {
  if (!disks.length) return `<div class="monitor-list-empty">块设备速率未知</div>`;
  return disks.map((disk) => `<div class="monitor-list-row">
    <span class="monitor-list-ident"><b>${escapeHtml(disk.name || "设备未知")}</b>${monitorQualityBadge(disk)}</span>
    <span><small>读取</small><b>${escapeHtml(formatMetricRate(disk.read_bytes_per_second))}</b></span>
    <span><small>写入</small><b>${escapeHtml(formatMetricRate(disk.write_bytes_per_second))}</b></span>
    <span><small>IOPS / 利用率</small><b>${escapeHtml(formatMetricNumber(disk.iops))} / ${escapeHtml(formatMetricPercent(disk.util_percent))}</b></span>
  </div>`).join("");
}

function renderNetworkRows(interfaces) {
  if (!interfaces.length) return `<div class="monitor-list-empty">网络接口速率未知</div>`;
  return interfaces.map((network) => `<div class="monitor-list-row">
    <span class="monitor-list-ident"><b>${escapeHtml(network.name || "接口未知")}</b><small>${escapeHtml(network.operstate || "状态未知")}${finiteMetric(network.speed_mbps) === null ? "" : ` · ${escapeHtml(formatMetricNumber(network.speed_mbps, 0))} Mbps`}</small></span>
    <span><small>接收 ↓</small><b>${escapeHtml(formatMetricRate(network.rx_bytes_per_second))}</b></span>
    <span><small>发送 ↑</small><b>${escapeHtml(formatMetricRate(network.tx_bytes_per_second))}</b></span>
    <span><small>错误/丢包</small><b class="${monitorQualityTone(network.quality)}">↓ ${escapeHtml(formatMetricPercent(network.rx_error_drop_percent))} · ↑ ${escapeHtml(formatMetricPercent(network.tx_error_drop_percent))}</b></span>
  </div>`).join("");
}

function renderHostMonitoring(asset) {
  const hostId = asset?.host?.host_id;
  const monitoring = hostMonitoringFor(asset);
  const latestRun = monitoring.latestRun;
  const snapshot = monitoring.currentSnapshot;
  const busy = state.monitoring.busyHostId === hostId;
  const anotherBusy = Boolean(state.monitoring.busyHostId && !busy);
  const collectDisabled = state.dataClient.transport !== "http" || Boolean(state.monitoring.busyHostId);
  const runLabel = latestRun ? monitorRunStateLabel(latestRun.state) : "尚未采集";
  const runTone = monitorRunStateTone(latestRun?.state);
  const buttonLabel = busy ? "正在采集" : anotherBusy ? "其他服务器采集中" : "手动采集一次";
  const notices = `${monitorRunNotice(latestRun, snapshot)}${monitorRequestError(hostId)}`;
  if (!snapshot) {
    return `<section class="host-monitoring-panel empty" aria-label="当前资源快照">
      <div class="host-monitoring-head"><div><div class="section-label"><i data-lucide="gauge"></i><span>当前资源快照</span><small class="${runTone}">${escapeHtml(runLabel)}</small></div><p>连接状态与资源采集状态相互独立；只有采集返回的观测值才会显示数值。</p></div><button class="command-button emphasis" data-monitor-action="collect-once" data-host-id="${escapeHtml(hostId || "")}" ${collectDisabled ? "disabled" : ""}><i data-lucide="${busy ? "loader-circle" : "play"}"></i><span>${escapeHtml(buttonLabel)}</span></button></div>
      ${notices}
      <div class="monitor-empty-state"><i data-lucide="gauge-circle"></i><span><b>尚未采集</b><strong>当前资源状态未知</strong><small>点击“手动采集一次”读取 CPU、内存、Load、磁盘、网络、运行时长和进程摘要。</small></span></div>
    </section>`;
  }

  const cpu = snapshot.cpu || {};
  const memory = snapshot.memory || {};
  const load = snapshot.load || {};
  const uptime = snapshot.uptime || {};
  const process = snapshot.process || {};
  const totalMemory = finiteMetric(memory.total_bytes);
  const usedMemory = finiteMetric(memory.used_bytes);
  const memoryRatio = totalMemory && usedMemory !== null ? Math.max(0, Math.min(1, usedMemory / totalMemory)) : null;
  const coverage = Array.isArray(snapshot.coverage) ? snapshot.coverage : [];
  const observedFamilies = coverage.filter((item) => item.quality === "observed").length;
  return `<section class="host-monitoring-panel" aria-label="当前资源快照">
    <div class="host-monitoring-head"><div><div class="section-label"><i data-lucide="gauge"></i><span>当前资源快照</span><small class="${monitorFreshnessTone(snapshot.freshness)}">${escapeHtml(monitorFreshnessLabel(snapshot.freshness))}</small></div><p>观测 ${escapeHtml(formatObservedAt(snapshot.observed_at))} · 有效至 ${escapeHtml(formatObservedAt(snapshot.valid_until))} · ${observedFamilies}/${coverage.length || "—"} 个指标族已观测</p></div><button class="command-button emphasis" data-monitor-action="collect-once" data-host-id="${escapeHtml(hostId || "")}" ${collectDisabled ? "disabled" : ""}><i data-lucide="${busy ? "loader-circle" : "play"}"></i><span>${escapeHtml(buttonLabel)}</span></button></div>
    ${notices}
    <div class="monitor-snapshot-meta"><span><small>最近资源采集</small><b class="${runTone}">${escapeHtml(runLabel)}</b></span><span><small>快照新鲜度</small><b class="${monitorFreshnessTone(snapshot.freshness)}">${escapeHtml(monitorFreshnessLabel(snapshot.freshness))}</b></span><span><small>已记录指标</small><b>${escapeHtml(formatMetricNumber(snapshot.metric_count, 0))}</b></span><span><small>未知指标</small><b class="${finiteMetric(snapshot.unknown_count) ? "amber" : ""}">${escapeHtml(formatMetricNumber(snapshot.unknown_count, 0))}</b></span></div>
    <div class="monitor-primary-grid">
      <article class="monitor-metric-card"><div><span>CPU busy</span>${monitorQualityBadge(cpu)}</div><b>${escapeHtml(formatMetricPercent(cpu.busy_percent))}</b><small>iowait ${escapeHtml(formatMetricPercent(cpu.iowait_percent))} · steal ${escapeHtml(formatMetricPercent(cpu.steal_percent))}</small><small>${escapeHtml(formatMetricNumber(cpu.online_cpu_count, 0))} 个在线 CPU · ${escapeHtml(formatMetricNumber(cpu.window_seconds))} 秒窗口</small></article>
      <article class="monitor-metric-card"><div><span>内存</span>${monitorQualityBadge(memory)}</div><b>${escapeHtml(formatMetricBytes(memory.used_bytes))} / ${escapeHtml(formatMetricBytes(memory.total_bytes))}</b>${memoryRatio === null ? "" : `<span class="monitor-capacity" style="--monitor-ratio:${(memoryRatio * 100).toFixed(2)}%"><i></i></span>`}<small>可用 ${escapeHtml(formatMetricBytes(memory.available_bytes))} · Swap ${escapeHtml(formatMetricBytes(memory.swap_total_bytes))}</small></article>
      <article class="monitor-metric-card"><div><span>Load 1 / 5 / 15</span>${monitorQualityBadge(load)}</div><b>${escapeHtml(formatMetricNumber(load.load1, 2))} / ${escapeHtml(formatMetricNumber(load.load5, 2))} / ${escapeHtml(formatMetricNumber(load.load15, 2))}</b><small>归一化 ${escapeHtml(formatMetricNumber(load.normalized_load1, 2))} / ${escapeHtml(formatMetricNumber(load.normalized_load5, 2))} / ${escapeHtml(formatMetricNumber(load.normalized_load15, 2))}</small><small>${escapeHtml(formatMetricNumber(load.online_cpu_count, 0))} 个在线 CPU</small></article>
      <article class="monitor-metric-card"><div><span>运行时长</span>${monitorQualityBadge(uptime)}</div><b>${escapeHtml(formatMetricDuration(uptime.uptime_seconds))}</b><small>${uptime.rebooted_during_sample === true ? "采样窗口内检测到重启" : uptime.rebooted_during_sample === false ? "采样窗口内未检测到重启" : "重启状态未知"}</small></article>
      <article class="monitor-metric-card"><div><span>进程摘要</span>${monitorQualityBadge(process)}</div><b>${escapeHtml(formatMetricNumber(process.scanned, 0))} 个已扫描</b><small>运行 ${escapeHtml(formatMetricNumber(process.running, 0))} · 阻塞 ${escapeHtml(formatMetricNumber(process.blocked, 0))} · 僵尸 ${escapeHtml(formatMetricNumber(process.zombie, 0))}</small><small>${process.truncated === true ? "达到枚举上限 · 结果已截断" : process.truncated === false ? `竞态跳过 ${escapeHtml(formatMetricNumber(process.raced, 0))}` : "完整性未知"}</small></article>
    </div>
    <div class="monitor-detail-groups">
      <section><div class="monitor-group-head"><span>文件系统</span><small>${Array.isArray(snapshot.filesystems) ? snapshot.filesystems.length : 0} 个挂载</small></div>${renderFilesystemRows(Array.isArray(snapshot.filesystems) ? snapshot.filesystems : [])}</section>
      <section><div class="monitor-group-head"><span>磁盘 I/O</span><small>当前采样窗口</small></div>${renderDiskIoRows(Array.isArray(snapshot.disk_io) ? snapshot.disk_io : [])}</section>
      <section><div class="monitor-group-head"><span>网络</span><small>当前采样窗口</small></div>${renderNetworkRows(Array.isArray(snapshot.network) ? snapshot.network : [])}</section>
    </div>
  </section>`;
}

function hostAssetFor(hostId) {
  return state.hostsView?.hosts?.find((asset) => asset.host?.host_id === hostId) || null;
}

function renderHostsView() {
  const summary = state.hostsView || {
    host_count: state.hosts.length,
    connection_ready_count: state.hosts.filter(hostConnectionAvailable).length,
    connection_failed_count: state.hosts.filter((host) => ["failed", "host_key_changed"].includes(host.status)).length,
    discovery_partial_count: state.hosts.filter((host) => host.status === "discovery_partial").length,
    stale_evidence_count: 0,
    unknown_count: state.hosts.length,
    hosts: state.hosts.map((host) => ({ host, connection_state: normalizedHostConnectionState(host), discovery_state: null, provider_coverage: [], deployment_count: 0, project_count: 0, freshness: "unavailable", attention_count: 0 })),
  };
  const host = selectedHost();
  const asset = hostAssetFor(host?.host_id) || summary.hosts?.find((candidate) => candidate.host?.host_id === host?.host_id) || null;
  const rows = (summary.hosts || []).map((candidate) => {
    const item = candidate.host;
    const selected = item?.host_id === host?.host_id;
    const failed = ["failed", "host_key_changed"].includes(candidate.connection_state);
    const tone = failed ? "red" : evidenceFreshnessTone(candidate.freshness);
    const monitoring = hostMonitoringFor(candidate);
    const monitoringFreshness = monitoring.currentSnapshot?.freshness || monitoring.currentSnapshotSummary?.freshness || candidate.monitor_freshness || "unknown";
    const monitoringRun = monitoring.latestRun?.state ? monitorRunStateLabel(monitoring.latestRun.state) : "尚未采集";
    return `<button class="asset-host-row ${selected ? "selected" : ""}" data-host-select="${escapeHtml(item?.host_id || "")}" type="button">
      <span class="status-dot ${tone}"></span>
      <span class="asset-host-copy"><b>${escapeHtml(item?.display_name || item?.host_id || "HOST")}</b><code>${escapeHtml(item?.address || "地址未知")}:${Number(item?.port || 0)}</code><small>SSH ${escapeHtml(hostStatusLabel(candidate.connection_state))} · 发现 ${escapeHtml(discoveryStatusLabel(candidate.discovery_state))} · 采集 ${escapeHtml(monitoringRun)} · ${candidate.deployment_count || 0} 个部署</small><small class="asset-host-freshness ${evidenceFreshnessTone(candidate.freshness)}">${escapeHtml(evidenceFreshnessLabel(candidate.freshness))} · 资源 ${escapeHtml(monitorFreshnessLabel(monitoringFreshness))}</small></span>
      <span class="asset-host-count">${candidate.project_count || 0} 项目</span><i data-lucide="chevron-right"></i>
    </button>`;
  }).join("");
  const providerRows = (asset?.provider_coverage || []).map((provider) => {
    const warning = providerWarningSummary(provider);
    const evidenceCount = Array.isArray(provider.evidence_refs) ? provider.evidence_refs.length : 0;
    return `<div class="provider-row"><span class="provider-row-head"><i data-lucide="${provider.provider_kind === "docker" ? "container" : provider.provider_kind === "compose" ? "layers-3" : "file-search"}"></i><b>${escapeHtml(providerKindLabel(provider.provider_kind))}</b></span><span class="provider-state ${providerStatusTone(provider.status)}">${escapeHtml(providerStatusLabel(provider.status))} · ${provider.observed_count || 0}</span><div class="provider-row-meta"><span>${evidenceCount} 条证据引用</span><span>观测 ${escapeHtml(formatObservedAt(provider.observed_at))}</span></div>${warning ? `<div class="provider-warning"><code>${escapeHtml(warning.code)}</code><span>${escapeHtml(warning.summary)}</span>${warning.remaining ? `<small>另有 ${warning.remaining} 条</small>` : ""}</div>` : ""}</div>`;
  }).join("");
  const connectionFailed = ["failed", "host_key_changed"].includes(asset?.connection_state);
  const connectionTone = asset?.connection_state === "connection_ready" ? "green" : connectionFailed ? "red" : "amber";
  const freshnessTone = evidenceFreshnessTone(asset?.freshness);
  const selectedMonitoring = hostMonitoringFor(asset);
  return `<div class="view-header hosts-view-header">
      <div><div class="view-kicker">GLOBAL SERVER ASSETS</div><h1>服务器资产</h1><p>服务器是全局连接与观察对象；这里分开展示 SSH 连接、发现覆盖和部署实例，不把连接成功解释成业务健康。</p></div>
      <div class="header-metrics"><div class="metric"><span>服务器</span><b>${summary.host_count || 0}</b></div><div class="metric"><span>连接可用</span><b class="mint">${summary.connection_ready_count || 0}</b></div><div class="metric"><span>连接失败</span><b class="red">${summary.connection_failed_count || 0}</b></div><div class="metric"><span>部分发现</span><b class="amber">${summary.discovery_partial_count || 0}</b></div><div class="metric"><span>证据过期</span><b class="amber">${summary.stale_evidence_count || 0}</b></div><div class="metric"><span>证据未知</span><b class="amber">${summary.unknown_count || 0}</b></div></div>
    </div>
    <div class="hosts-asset-layout">
      <section class="hosts-asset-list rail-section" aria-label="服务器列表">
        <div class="asset-list-head"><div class="section-label"><i data-lucide="server"></i><span>服务器清单</span></div><small>${summary.stale_evidence_count || 0} 条过期 · ${summary.unknown_count || 0} 条未知</small></div>
        <div class="asset-host-list">${rows || `<div class="host-registry-empty">尚未登记服务器</div>`}</div>
        <button class="command-button host-register-button" data-m2-action="register-host"><i data-lucide="plus"></i><span>登记服务器</span></button>
      </section>
      <section class="hosts-asset-detail rail-section" aria-label="服务器详情">
        ${asset ? `<div class="asset-detail-head"><div><span class="context-kicker">HOST ASSET</span><h2>${escapeHtml(asset.host.display_name)}</h2><code>${escapeHtml(asset.host.address)}:${Number(asset.host.port)} · ${escapeHtml(asset.host.ssh_user)}</code></div><span class="selection-badge"><span class="status-dot ${connectionTone}"></span>${escapeHtml(hostStatusLabel(asset.connection_state))}</span></div>
          <div class="asset-state-summary"><div><span>SSH 连接</span><b class="${connectionTone}">${escapeHtml(hostStatusLabel(asset.connection_state))}</b></div><div><span>最近发现</span><b class="${discoveryStatusTone(asset.discovery_state)}">${escapeHtml(discoveryStatusLabel(asset.discovery_state))}</b></div><div><span>最近资源采集</span><b class="${monitorRunStateTone(selectedMonitoring.latestRun?.state)}">${escapeHtml(selectedMonitoring.latestRun ? monitorRunStateLabel(selectedMonitoring.latestRun.state) : "尚未采集")}</b></div><div><span>当前快照</span><b class="${monitorFreshnessTone(selectedMonitoring.currentSnapshot?.freshness || selectedMonitoring.currentSnapshotSummary?.freshness)}">${escapeHtml(selectedMonitoring.currentSnapshot ? monitorFreshnessLabel(selectedMonitoring.currentSnapshot.freshness) : monitorFreshnessLabel(selectedMonitoring.currentSnapshotSummary?.freshness))}</b></div></div>
          ${renderHostMonitoring(asset)}
          <div class="asset-detail-metrics"><div><span>部署实例</span><b>${asset.deployment_count || 0}</b></div><div><span>关联项目</span><b>${asset.project_count || 0}</b></div><div><span>需处理</span><b class="${asset.attention_count ? "amber" : "mint"}">${asset.attention_count || 0}</b></div><div><span>证据新鲜度</span><b class="${freshnessTone}">${escapeHtml(evidenceFreshnessLabel(asset.freshness))}</b></div><div><span>最近观测</span><b>${escapeHtml(formatObservedAt(asset.last_observed_at))}</b></div></div>
          <div class="provider-matrix"><div class="section-label"><i data-lucide="scan-search"></i><span>Provider 覆盖</span></div>${providerRows || `<div class="provider-empty">尚无发现记录</div>`}</div>
          <div class="asset-detail-actions"><button class="command-button" data-m2-action="retry-selected-host" data-node-id="${escapeHtml(asset.host.host_id)}"><i data-lucide="refresh-cw"></i><span>重连 / 只读发现</span></button><button class="command-button" data-m2-action="edit-host-connection" data-node-id="${escapeHtml(asset.host.host_id)}"><i data-lucide="settings-2"></i><span>连接设置</span></button><button class="command-button" data-m2-action="replace-host-credential" data-node-id="${escapeHtml(asset.host.host_id)}"><i data-lucide="key-round"></i><span>更新凭据</span></button></div>
          <div class="asset-boundary-note"><i data-lucide="shield-check"></i><span>当前页面只执行连接测试、证据读取和本地投影刷新；部署、重启和远程写入不从这里发起。</span></div>` : `<div class="hosts-empty-detail"><i data-lucide="server"></i><h2>选择一台服务器</h2><p>登记服务器后，这里会显示身份、Provider 覆盖、部署实例和最近观测。</p></div>`}
      </section>
      ${renderProjectionWorkbench(selectedM2Node())}
    </div>`;
}

function resourceLensButton(id, label, sublabel) {
  const symbols = { project: "", shared: "shared", reverse: "reverse" };
  return `<button class="lens-button ${state.resourceLens === id ? "active" : ""}" data-resource-lens="${id}"><span class="lens-symbol ${symbols[id]}"></span><span>${label}</span><small>${sublabel}</small></button>`;
}

function firstSharedResourceId() {
  return Object.keys(resources).find((id) => resources[id]?.scope === "共享资源") || null;
}

function resourceLensTitle() {
  return {
    project: `${projects[state.resourceProject]?.label || "项目"} · 项目资源边界`,
    shared: "共享资源集合 · 实体只出现一次",
    reverse: `${resources[state.selectedResource]?.label || "尚未选择资源"} · 影响反查`,
  }[state.resourceLens];
}

function resourceLegend() {
  if (state.resourceLens === "project") return `<span class="legend-entry"><i class="legend-line blue"></i>项目私有</span><span class="legend-entry"><i class="legend-line shared"></i>共享实体</span><span class="legend-entry"><i class="legend-line live"></i>使用关系</span>`;
  return `<span class="legend-entry"><i class="legend-line shared"></i>资源实体</span><span class="legend-entry"><i class="legend-line live"></i>项目使用</span><span class="legend-entry"><i class="legend-line"></i>工作负载</span>`;
}

function resourceFooterCopy() {
  if (state.resourceLens === "project") return "共享资源位于项目边界外；点击共享资源即可查看全部使用关系。";
  if (state.resourceLens === "shared") return "每个共享资源只显示一次，适合检查共享范围与待核验对象。";
  return "从一个资源展开全部使用项目与工作负载，确认故障或变更的影响范围。";
}

function renderResourceSvg() {
  const content = state.resourceLens === "project" ? projectResourceGraph() : state.resourceLens === "shared" ? sharedResourceGraph() : reverseResourceGraph();
  return graphSvg("resource-canvas", "resource-scene", content, state.resourcePan, state.resourceZoom, state.scope === "project" ? `${projects[state.activeProject].label} 项目资源关系画布` : "全局共享资源关系画布");
}

function resourceNodeMarkup({ id, x, y, width = 154, height = 72, kicker, title, meta, status, tone = "", scope = "private", extraClass = "" }) {
  const selected = id === state.selectedResource;
  const resource = resources[id];
  return `<g class="graph-node resource ${scope === "shared" ? "shared-resource" : "private-resource"} ${extraClass} ${selected ? "selected" : ""}" data-resource-node="${id}" role="button" tabindex="0" aria-label="查看资源 ${title}" transform="translate(${x} ${y})">
    <rect class="node-surface" width="${width}" height="${height}" rx="8"></rect>
    <text class="node-kicker" x="13" y="18">${kicker}</text>
    <text class="node-state ${tone === "amber" ? "amber" : tone === "green" ? "green" : ""}" x="${width - 12}" y="18" text-anchor="end">${status}</text>
    <text class="node-title" x="13" y="42">${title}</text>
    <text class="node-subtitle" x="13" y="60">${meta}</text>
  </g>`;
}

function resourceProjectNode(id, x, y, width = 170, height = 76, status = "已登记", tone = "green", metaOverride) {
  const project = projects[id];
  const selected = state.resourceProject === id;
  const meta = metaOverride || project.subtitle;
  return `<g class="graph-node project-node ${selected ? "selected" : ""}" data-resource-project-node="${id}" role="button" tabindex="0" aria-label="查看项目 ${project.label}" transform="translate(${x} ${y})">
    <rect class="node-surface" width="${width}" height="${height}" rx="8"></rect>
    <text class="node-kicker" x="13" y="18">PROJECT</text>
    <text class="node-state ${tone === "amber" ? "amber" : tone === "green" ? "green" : "muted"}" x="${width - 12}" y="18" text-anchor="end">${status}</text>
    <text class="node-title" x="13" y="43">${project.label}</text>
    <text class="node-subtitle" x="13" y="61">${meta}</text>
  </g>`;
}

function staticEdge(id, d, className, marker = "arrow-mint", label = "", labelX = 0, labelY = 0) {
  return `<g><path id="${id}" class="graph-edge ${className}" d="${d}" marker-end="url(#${marker})"></path>${label ? `<text class="edge-label" x="${labelX}" y="${labelY}" text-anchor="middle">${label}</text>` : ""}</g>`;
}

function projectResourceGraph() {
  const contractSnapshot = state.projectSnapshots[state.resourceProject];
  if (contractSnapshot) return contractProjectResourceGraph(contractSnapshot);
  const profile = projectResources[state.resourceProject] || { privateResources: [], sharedResources: [] };
  const project = projects[state.resourceProject];
  const projectNode = { x: 408, y: 243, width: 184, height: 78 };
  const positions = [[285, 112], [552, 112], [285, 398], [552, 398]];
  const privateNodes = profile.privateResources.map((id, index) => {
    const resource = resources[id];
    const [x, y] = positions[index];
    return resourceNodeMarkup({ id, x, y, kicker: resource.type.toUpperCase(), title: resource.label, meta: resource.meta, status: resource.health, tone: resource.tone, scope: "private" });
  }).join("");
  const privateEdges = profile.privateResources.map((id, index) => {
    const [x, y] = positions[index];
    return staticEdge(`private-edge-${index}`, curvePath(projectNode, { x, y, width: 154, height: 72 }), "resource-private", "arrow-mint");
  }).join("");
  const sharedPositions = [{ x: 36, y: 254 }, { x: 810, y: 254 }];
  const sharedNodes = profile.sharedResources.map((id, index) => {
    const resource = resources[id];
    const position = sharedPositions[index];
    const otherProjects = resource.projects.filter((projectId) => projectId !== state.resourceProject).length;
    return resourceNodeMarkup({ id, x: position.x, y: position.y, width: 154, height: 76, kicker: `SHARED · ${resource.type.toUpperCase()}`, title: resource.label, meta: `${otherProjects} 个其他项目也在使用`, status: resource.health, tone: resource.tone, scope: "shared" });
  }).join("");
  const sharedEdges = profile.sharedResources.map((id, index) => {
    const position = sharedPositions[index];
    const resourceNode = { x: position.x, y: position.y, width: 154, height: 76 };
    return staticEdge(`project-shared-${index}`, curvePath(projectNode, resourceNode), "resource-shared", "arrow-mint", "使用", midpoint(projectNode, resourceNode).x, midpoint(projectNode, resourceNode).y - 7);
  }).join("");
  const emptyState = profile.privateResources.length || profile.sharedResources.length ? "" : `<g class="resource-empty"><circle cx="500" cy="178" r="22"></circle><path d="M489 178h22M500 167v22"></path><text x="500" y="218" text-anchor="middle">尚未登记项目资源</text><text class="secondary" x="500" y="236" text-anchor="middle">登记后将在边界内外自动形成关系</text></g>`;
  return `<rect class="boundary" x="225" y="70" width="550" height="448" rx="34"></rect><text class="boundary-label" x="249" y="96">${project.label.toUpperCase()} · PROJECT RESOURCE BOUNDARY</text>
    ${sharedEdges}
    ${privateEdges}
    ${resourceProjectNode(state.resourceProject, 408, 243, 184, 78, "已选中", "green", `${profile.privateResources.length} 私有 · ${profile.sharedResources.length} 共享`)}
    ${privateNodes}
    ${sharedNodes}
    ${emptyState}`;
}

function contractProjectResourceGraph(snapshot) {
  const projectId = state.resourceProject;
  const project = projects[projectId];
  const projectNode = snapshot.nodes.find((node) => node.id === projectId && node.kind === "project")
    || snapshot.nodes.find((node) => node.kind === "project");
  const resourceNodes = snapshot.nodes.filter((node) => node !== projectNode);
  const boxes = Object.fromEntries(snapshot.nodes.map((node) => [node.id, {
    x: node.position.x,
    y: node.position.y,
    width: node.width,
    height: node.height,
  }]));
  const edgeMarkup = snapshot.edges.map((edge, index) => {
    const from = boxes[edge.from];
    const to = boxes[edge.to];
    if (!from || !to) return "";
    const target = resources[edge.to];
    const className = target?.scope === "共享资源" ? "resource-shared" : "resource-private";
    const center = midpoint(from, to);
    return staticEdge(`contract-resource-edge-${index}`, curvePath(from, to), className, "arrow-mint", edge.label, center.x, center.y - 7);
  }).join("");
  const nodeMarkup = resourceNodes.map((node) => {
    const resource = resources[node.id];
    if (!resource) return "";
    const shared = resource.scope === "共享资源";
    return resourceNodeMarkup({
      id: node.id,
      x: node.position.x,
      y: node.position.y,
      width: node.width,
      height: node.height,
      kicker: `${shared ? "SHARED · " : ""}${graphKindLabel(node.kind).toUpperCase()}`,
      title: resource.label,
      meta: resource.meta,
      status: resource.health,
      tone: resource.tone,
      scope: shared ? "shared" : "private",
    });
  }).join("");
  const profile = projectResources[projectId] || { privateResources: [], sharedResources: [] };
  const projectMarkup = projectNode
    ? resourceProjectNode(
      projectId,
      projectNode.position.x,
      projectNode.position.y,
      projectNode.width,
      projectNode.height,
      project.health,
      project.tone,
      `${profile.privateResources.length} 私有 · ${profile.sharedResources.length} 共享`,
    )
    : "";
  const fit = dataApi.fitGraphToViewport(snapshot.nodes, { x: 80, y: 110, width: 840, height: 400 });
  return `<rect class="boundary" x="54" y="62" width="892" height="480" rx="34"></rect><text class="boundary-label" x="78" y="90">${escapeHtml(project.label.toUpperCase())} · PROJECT RESOURCE CONTRACT</text><g transform="translate(${fit.x} ${fit.y}) scale(${fit.scale})">${edgeMarkup}${projectMarkup}${nodeMarkup}</g>`;
}

function sharedResourceGraph() {
  const shared = Object.entries(resources).filter(([, resource]) => resource.scope === "共享资源");
  if (!shared.length) {
    return `<g class="resource-empty"><circle cx="500" cy="245" r="22"></circle><path d="M489 245h22M500 234v22"></path><text x="500" y="285" text-anchor="middle">尚无共享资源</text><text class="secondary" x="500" y="303" text-anchor="middle">确认项目关系后，共享引用会在这里出现</text></g>`;
  }
  const resourceBoxes = Object.fromEntries(shared.map(([id], index) => [id, {
    x: 390,
    y: 105 + index * Math.min(116, 380 / Math.max(1, shared.length - 1)),
    width: 220,
    height: 78,
  }]));
  const projectIds = Array.from(new Set(shared.flatMap(([, resource]) => resource.projects || []))).filter((id) => projects[id]);
  const projectBoxes = Object.fromEntries(projectIds.map((id, index) => [id, {
    x: index % 2 === 0 ? 55 : 775,
    y: 105 + Math.floor(index / 2) * 130,
    width: 170,
    height: 78,
  }]));
  const edges = shared.flatMap(([id, resource]) => (resource.projects || []).map((projectId) => {
    const from = projectBoxes[projectId];
    const to = resourceBoxes[id];
    if (!from || !to) return "";
    return staticEdge(`shared-${projectId}-${id}`, curvePath(from, to), "resource-usage", "arrow-mint", "使用", midpoint(from, to).x, midpoint(from, to).y - 7);
  })).join("");
  const resourceMarkup = shared.map(([id, resource]) => {
    const box = resourceBoxes[id];
    return resourceNodeMarkup({
      id,
      ...box,
      kicker: `SHARED · ${String(resource.type || "RESOURCE").toUpperCase()}`,
      title: escapeHtml(resource.label),
      meta: escapeHtml(`${(resource.projects || []).length} 个项目使用`),
      status: escapeHtml(resource.health),
      tone: resource.tone,
      scope: "shared",
    });
  }).join("");
  const projectMarkup = projectIds.map((id) => {
    const box = projectBoxes[id];
    const count = shared.filter(([, resource]) => (resource.projects || []).includes(id)).length;
    return resourceProjectNode(id, box.x, box.y, box.width, box.height, projects[id].health, projects[id].tone, `${count} 项共享资源`);
  }).join("");
  return `<rect class="boundary shared" x="315" y="66" width="370" height="470" rx="34"></rect><text class="boundary-label" x="338" y="92">SHARED COLLECTION · 资源实体只出现一次</text>${edges}${resourceMarkup}${projectMarkup}`;
}

function reverseResourceConfig() {
  const resource = resources[state.selectedResource] || null;
  const projectIds = (resource?.projects || []).filter((id) => projects[id]);
  return {
    resource,
    branches: projectIds.map((projectId, index) => ({
      project: projectId,
      x: index % 2 === 0 ? 54 : 776,
      y: 90 + Math.floor(index / 2) * 150,
    })),
  };
}

function reverseResourceGraph() {
  const config = reverseResourceConfig();
  if (!config.resource) return sharedResourceGraph();
  const center = { x: 410, y: 235, width: 180, height: 90 };
  const branches = config.branches.map((branch, index) => {
    const project = { x: branch.x, y: branch.y, width: 170, height: 76 };
    const edge = staticEdge(`impact-project-${index}`, curvePath(center, project), "resource-usage", "arrow-mint", "使用", midpoint(center, project).x, midpoint(center, project).y - 7);
    return `${edge}${resourceProjectNode(branch.project, branch.x, branch.y, 170, 76, "受影响", projects[branch.project].tone)}`;
  }).join("");
  return `<circle class="boundary" cx="500" cy="280" r="220"></circle><circle class="boundary" cx="500" cy="280" r="140"></circle><text class="boundary-label" x="500" y="48" text-anchor="middle">RESOURCE IMPACT · 从单一资源反查全部使用关系</text>
    ${resourceNodeMarkup({ id: state.selectedResource, x: center.x, y: center.y, width: center.width, height: center.height, kicker: config.resource.type.toUpperCase(), title: config.resource.label, meta: config.resource.meta, status: config.resource.health, tone: config.resource.tone, scope: "shared" })}${branches}`;
}

function resourceContext() {
  const resource = resources[state.selectedResource];
  const projectNames = resource.projects.map((id) => projects[id]?.label || id).join(" / ");
  const action = resource.scope === "共享资源"
    ? (state.resourceLens === "reverse" ? { label: "返回共享集合", type: "open-shared" } : { label: "查看影响", type: "open-reverse" })
    : { label: "打开项目资源", type: "open-project-resource" };
  return { kicker: "RESOURCE", title: resource.label, summary: resource.relation, facts: [["类型", resource.type], ["范围", resource.scope], ["健康", resource.health], ["来源", resource.source], ["观察时间", resource.freshness], ["使用", projectNames]], action };
}

function renderResourceStrip() {
  const resource = resources[state.selectedResource];
  if (state.scope === "project") {
    const profile = projectResources[state.activeProject] || { privateResources: [], sharedResources: [] };
    if (!profile.privateResources.concat(profile.sharedResources).includes(state.selectedResource)) {
      return `<div class="resource-empty-strip"><span class="status-dot"></span><span>${projects[state.activeProject].label} 尚未登记资源</span></div>`;
    }
  }
  if (!resource) return "";
  const projectNames = resource.projects.map((id) => projects[id]?.label).join(" / ");
  return `<div class="resource-detail-strip"><div><span>选中资源</span><b>${resource.label}</b></div><div><span>范围</span><b>${resource.scope}</b></div><div><span>健康</span><b class="${resource.tone}">${resource.health}</b></div><div><span>使用项目</span><b>${projectNames}</b></div><div><span>最近核验</span><b>${resource.freshness}</b></div></div>`;
}

function enterGlobalView(view) {
  const routeChanged = state.scope !== "global" || state.view !== view;
  state.scope = "global";
  state.view = view;
  if (view === "resource") {
    state.resourceLens = "shared";
    if (resources[state.selectedResource]?.scope !== "共享资源") state.selectedResource = firstSharedResourceId();
  }
  state.contextOpen = false;
  renderApp();
  if (routeChanged && state.dataClient.transport === "http" && state.auth.authenticated) {
    void refreshM0Data({ initial: true });
  }
}

function bindAppEvents() {
  app.querySelectorAll("[data-view]").forEach((button) => button.addEventListener("click", () => {
    enterGlobalView(button.dataset.view);
  }));

  app.querySelectorAll("[data-side-project]").forEach((button) => button.addEventListener("click", () => {
    const id = button.dataset.sideProject;
    const view = state.dataSource.kind === "real" ? "resource" : "workflow";
    enterProject(id, view);
    if (view === "workflow") state.workflowMode = "run";
    renderApp();
  }));

  app.querySelectorAll("[data-project-view]").forEach((button) => button.addEventListener("click", () => {
    state.scope = "project";
    state.view = button.dataset.projectView;
    state.resourceProject = state.activeProject;
    if (state.view === "workflow") state.workflowMode = "run";
    if (state.view === "resource") state.resourceLens = "project";
    state.contextOpen = false;
    renderApp();
  }));

  app.querySelector("[data-open-global-resources]")?.addEventListener("click", () => {
    state.scope = "global";
    state.view = "resource";
    state.resourceLens = "shared";
    if (resources[state.selectedResource]?.scope !== "共享资源") state.selectedResource = firstSharedResourceId();
    state.contextOpen = false;
    renderApp();
  });

  app.querySelector("#knowledge-button")?.addEventListener("click", () => showToast("知识入口已保留：Obsidian、GBrain 与自定义连接器在接入阶段配置。"));
  app.querySelector("#refresh-button")?.addEventListener("click", () => { void refreshM0Data(); });
  app.querySelector("#owner-button")?.addEventListener("click", () => {
    state.m4 = { ...state.m4, modal: true, error: null, pendingDelete: null };
    renderApp();
  });
  app.querySelector("#primary-action")?.addEventListener("click", handlePrimaryAction);
  app.querySelector("[data-open-host-resources]")?.addEventListener("click", () => {
    enterGlobalView("hosts");
  });
  bindM2Events();
  app.querySelector("[data-coordinator-chat-form]")?.addEventListener("submit", handleCoordinatorChatSubmit);
  app.querySelectorAll("[data-quick-action]").forEach((button) => button.addEventListener("click", () => handleQuickAction(button.dataset.quickAction)));
  app.querySelector("[data-project-context-resources]")?.addEventListener("click", () => { enterProject(state.activeProject, "resource"); renderApp(); });
  app.querySelectorAll("[data-close-context]").forEach((button) => button.addEventListener("click", () => { state.contextOpen = false; renderApp(); }));
  app.querySelectorAll("[data-context-action]").forEach((button) => button.addEventListener("click", () => handleContextAction(button.dataset.contextAction)));
  app.querySelectorAll("[data-canvas-control]").forEach((button) => button.addEventListener("click", () => canvasControl(button.dataset.canvasControl)));
  app.querySelectorAll("[data-m4-export]").forEach((button) => button.addEventListener("click", () => { void exportM4Data(button.dataset.m4Export); }));
  app.querySelectorAll("[data-m4-delete]").forEach((button) => button.addEventListener("click", () => queueM4Delete(button.dataset.m4Delete, button.dataset.scopeId)));
  app.querySelectorAll("[data-m4-action]").forEach((button) => button.addEventListener("click", () => {
    const action = button.dataset.m4Action;
    if (action === "close" || action === "cancel-delete") {
      state.m4 = { ...state.m4, modal: action === "close" ? false : true, pendingDelete: null, error: null };
      renderApp();
    }
    if (action === "logout") void logoutOwner();
  }));
  app.querySelector("[data-m4-delete-form]")?.addEventListener("submit", (event) => {
    event.preventDefault();
    void submitM4Delete(event.currentTarget);
  });

  if (state.scope === "global" && (state.view === "world" || (state.view === "resource" && state.dataSource.kind === "real"))) bindWorldEvents();
  if (state.scope === "project" && state.view === "workflow") bindWorkflowEvents();
  if (state.view === "resource") bindResourceEvents();
}

function enterProject(id, view = "workflow") {
  if (!projects[id]) return;
  state.scope = "project";
  state.activeProject = id;
  state.resourceProject = id;
  state.view = view;
  state.resourceLens = "project";
  state.contextOpen = false;
}

function coordinatorReply(message) {
  const target = coordinatorTarget();
  const text = message.toLowerCase();
  if (target.kind === "project" && (text.includes("资源") || text.includes("服务器") || text.includes("知识"))) return `${target.label} 的外部上下文包含项目私有资源和共享引用；共享实体可跳转到全局资源反查，当前不改变资源归属。`;
  if (target.kind === "project" && (text.includes("运行") || text.includes("流程") || text.includes("状态"))) return `${target.label} 默认进入运行视图；我会按当前项目的实际轨迹读取步骤、耗时和回执，不借用其他项目实例。`;
  if (text.includes("hermes") || text.includes("消息")) return "Hermes 的 RUN-028 当前停在“验证结果”，已等待外部回执 12 秒；流程本身没有被改写。";
  if (text.includes("资源") || text.includes("服务器") || text.includes("知识")) return "我会从项目边界先展开资源，再把共享实体反查到全局资源视图；当前 KNOWLEDGE_INDEX 仍待核验。";
  if (text.includes("项目") || text.includes("状态")) return "全局有 4 个项目：2 个健康、1 个待核验、1 个尚未接入。你可以继续指定项目名称。";
  return `已记录这条询问，当前目标是${target.label}。我会先保持投影结构，再根据项目、流程或资源语境给出下一步。`;
}

function handleCoordinatorChatSubmit(event) {
  event.preventDefault();
  const input = event.currentTarget.querySelector("input[name=message]");
  const message = input?.value.trim();
  if (!message) return;
  state.coordinatorChat.push({ role: "user", text: message, time: "刚刚" });
  state.coordinatorChat.push({ role: "agent", text: coordinatorReply(message), time: "刚刚" });
  renderApp();
  window.requestAnimationFrame(() => app.querySelector("#coordinator-input")?.focus());
}

function handleQuickAction(actionId) {
  if (state.scope !== "global" || state.view !== "world") return;
  const target = coordinatorTarget();
  const action = quickActionsForTarget(target).find((candidate) => candidate.id === actionId);
  if (!action) return;
  const operation = {
    time: "刚刚",
    project: target.kind === "project" ? target.id : "all",
    focus: target.kind === "project" ? target.id : "coordinator",
    title: action.label,
    target: action.target,
    result: action.result,
    tone: action.tone,
  };
  state.operationLog.unshift(operation);
  state.operationProject = operation.project;
  state.coordinatorChat.push({ role: "user", text: `快捷动作：${action.label}`, time: "刚刚" });
  state.coordinatorChat.push({ role: "agent", text: action.reply, time: "刚刚" });
  renderApp();
  showToast(`${action.label} · ${action.result}`);
}

function handlePrimaryAction() {
  if (state.scope === "global" && state.view === "world") {
    state.m2.modal = { type: "model" };
    state.m3.error = null;
    renderApp();
    return;
  }
  if (state.scope === "global" && state.view === "hosts") {
    if (selectedHost() && state.dataClient.transport === "http") void retrySelectedHostConnection();
    else openM2HostSetup({ fresh: true });
    return;
  }
  if (state.view === "workflow" && state.workflowMode === "edit") {
    saveDraft();
    return;
  }
  if (state.view === "workflow") {
    state.workflowMode = "edit";
    state.contextOpen = false;
    renderApp();
    return;
  }
  if (state.view === "resource") {
    showToast("共享资源与关系登记入口已预留：先录入已有资源，再建立本地引用与项目关系。");
    return;
  }
  showToast("项目登记入口已预留：先录入已有项目信息，再生成本地投影。");
}

function bindM2Events() {
  app.querySelectorAll("[data-monitor-action]").forEach((button) => button.addEventListener("click", () => {
    if (button.dataset.monitorAction === "collect-once") void startManualMonitorRun(button.dataset.hostId);
  }));
  app.querySelectorAll("[data-m2-node-select]").forEach((button) => button.addEventListener("click", () => {
    state.m2.selectedNodeId = button.dataset.m2NodeSelect;
    renderApp();
  }));
  app.querySelectorAll("[data-m2-action]").forEach((button) => button.addEventListener("click", () => {
    const action = button.dataset.m2Action;
    const nodeId = button.dataset.nodeId;
    if (action === "scan-host") {
      if (selectedHost() && state.dataClient.transport === "http") void retrySelectedHostConnection();
      else openM2HostSetup();
    }
    if (action === "register-host") openM2HostSetup({ fresh: true });
    if (action === "retry-selected-host") {
      if (nodeId) { state.selectedHostId = nodeId; state.selectedWorldId = nodeId; }
      void retrySelectedHostConnection();
    }
    if (action === "edit-host-alias") {
      const hostId = nodeId || state.selectedHostId;
      if (hostId) { state.m2.modal = { type: "host-alias", hostId }; renderApp(); }
    }
    if (action === "edit-host-connection") {
      const hostId = nodeId || state.selectedHostId;
      if (hostId) { state.selectedHostId = hostId; state.selectedWorldId = hostId; state.m2.modal = { type: "host-connection", hostId }; renderApp(); }
    }
    if (action === "replace-host-credential") {
      const hostId = nodeId || state.selectedHostId;
      if (hostId) {
        const host = state.hosts.find((candidate) => candidate.host_id === hostId);
        state.selectedHostId = hostId;
        state.selectedWorldId = hostId;
        state.m2.setup = host ? {
          hostId,
          displayName: host.display_name,
          address: host.address,
          port: String(host.port),
          sshUser: host.ssh_user,
          credentialKind: "ssh_password",
          error: null,
        } : { ...(state.m2.setup || {}), hostId, credentialKind: "ssh_password", error: null };
        state.m2.modal = { type: "host-credential", hostId };
        renderApp();
      }
    }
    if (action === "close-modal") { state.m2.modal = null; state.m2.busy = false; state.m3.busy = false; state.m3.error = null; renderApp(); }
    if (action === "retry-host-setup") { state.m2.modal = { type: "host", phase: "form" }; state.m2.setup = { ...state.m2.setup, error: null }; renderApp(); }
    if (action === "confirm-host-key") void confirmM2HostKeyAndScan();
    if (["rename", "assign", "relation"].includes(action)) { state.m2.modal = { type: action, nodeId }; renderApp(); }
    if (action === "create-project") { state.m2.modal = { type: "create-project" }; renderApp(); }
    if (action === "confirm") { state.m2.modal = { type: "confirm" }; renderApp(); }
    if (action === "archive" || action === "restore") void updateM2IgnoreRule(nodeId, action);
  }));
  app.querySelectorAll("[data-host-select]").forEach((button) => button.addEventListener("click", () => {
    const host = selectHostContext(button.dataset.hostSelect);
    if (!host) return;
    renderApp();
    void loadHostMonitoring(host.host_id);
    void restoreM2ProjectionForHost(state.selectedHostId).then(renderApp);
  }));
  app.querySelectorAll("[data-m2-form]").forEach((form) => form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (form.dataset.m2Form === "host") void submitM2HostSetup(form);
    else void submitM2Editor(form);
  }));
  app.querySelectorAll(".ssh-credential-fields").forEach(bindSshCredentialFields);
  app.querySelectorAll("[data-m3-action]").forEach((button) => button.addEventListener("click", () => {
    const action = button.dataset.m3Action;
    const proposalId = button.dataset.proposalId;
    if (action === "model") { state.m3.error = null; state.m2.modal = { type: "model" }; renderApp(); }
    else if (action === "generate") void generateM3Session();
    else if (action === "modify") { state.m2.modal = { type: "proposal-modify", proposalId }; renderApp(); }
    else void mutateM3Proposal(action, proposalId);
  }));
  app.querySelectorAll("[data-m3-form]").forEach((form) => form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (form.dataset.m3Form === "model") void submitM3Model(form);
    if (form.dataset.m3Form === "proposal-modify") void submitM3ProposalModify(form);
  }));
  app.querySelectorAll("[data-m3-question-form]").forEach((form) => form.addEventListener("submit", (event) => {
    event.preventDefault();
    void submitM3Question(form);
  }));
}

function openM2HostSetup(options = {}) {
  if (state.dataClient.transport !== "http") {
    showToast("连接 HOST 需要同源 HTTP API。");
    return;
  }
  state.m2.setup = options.fresh
    ? { error: null, port: "22", credentialKind: "ssh_password" }
    : { ...(state.m2.setup || {}), error: null, port: state.m2.setup?.port || "22", credentialKind: state.m2.setup?.credentialKind || "ssh_password" };
  state.m2.modal = { type: "host", phase: "form" };
  renderApp();
}

function bindSshCredentialFields(fieldset) {
  const sync = () => {
    const kind = fieldset.querySelector('input[name="credential_kind"]:checked')?.value || "ssh_password";
    fieldset.querySelectorAll("[data-ssh-credential-field]").forEach((field) => {
      const active = field.dataset.sshCredentialField === kind;
      field.hidden = !active;
      field.querySelectorAll("input, textarea").forEach((input) => { input.required = active; });
    });
    if (state.m2.modal?.type === "host") state.m2.setup = { ...(state.m2.setup || {}), credentialKind: kind };
  };
  fieldset.querySelectorAll('input[name="credential_kind"]').forEach((input) => input.addEventListener("change", sync));
  sync();
}

async function createSshCredential(values, idempotencyPrefix) {
  const credentialKind = values.credential_kind === "ssh_key" ? "ssh_key" : "ssh_password";
  if (credentialKind === "ssh_key") {
    const privateKey = String(values.private_key || "");
    if (!privateKey.trim()) throw invalidM2Input("请填写 SSH 私钥");
    return state.dataClient.createSecretRef(privateKey, dataApi.createIdempotencyKey(`${idempotencyPrefix}-key`));
  }
  const password = String(values.password || "");
  if (!password) throw invalidM2Input("请填写 SSH 密码");
  return state.dataClient.createPasswordSecretRef(password, dataApi.createIdempotencyKey(`${idempotencyPrefix}-password`));
}

async function retrySelectedHostConnection() {
  const host = selectedHost();
  if (!host) {
    openM2HostSetup({ fresh: true });
    return;
  }
  state.m2.setup = {
    hostId: host.host_id,
    displayName: host.display_name,
    address: host.address,
    port: String(host.port),
    sshUser: host.ssh_user,
    credentialKind: host.credential_kind || "ssh_password",
    error: null,
  };
  state.m2.busy = true;
  state.m2.modal = { type: "host", phase: "scanning" };
  renderApp();
  try {
    const connection = await state.dataClient.createConnectionTest(host.host_id, dataApi.createIdempotencyKey("connect"));
    state.m2.setup = { ...state.m2.setup, error: null };
    if (["host_key_unverified", "host_key_changed"].includes(connection.data.state)) {
      state.m2.setup.fingerprint = connection.data.candidate_fingerprint;
      state.m2.modal = { type: "host", phase: "fingerprint" };
      state.m2.busy = false;
      renderApp();
      return;
    }
    if (connectionReadyForDiscovery(connection)) {
      state.m2.setup = { ...state.m2.setup, connectionState: connection.data.state, authTransport: connection.data.auth_transport || null };
      await startM2Discovery(host.host_id, connection);
      return;
    }
    throw new dataApi.DataSourceError(
      connection.data.error_code || "CONNECTION_NOT_READY",
      connection.data.error_summary || "HOST 连接检查未通过",
      { details: connection.data },
    );
  } catch (error) {
    try { await refreshM0Data({ initial: true }); } catch (_) { /* visible error remains primary */ }
    setM2HostError(error);
  }
}

function m2Error(error) {
  return dataApi.errorRecord(error);
}

function setM2HostError(error) {
  state.m2.busy = false;
  state.m2.setup = { ...(state.m2.setup || {}), error: m2Error(error) };
  state.m2.modal = { type: "host", phase: "error" };
  renderApp();
}

function invalidM2Input(message) {
  return new dataApi.DataSourceError("INVALID_INPUT", message, { status: 400 });
}

async function submitM2HostSetup(form) {
  const values = Object.fromEntries(new FormData(form).entries());
  const port = Number(values.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    setM2HostError(invalidM2Input("端口必须在 1 到 65535 之间"));
    return;
  }
  const displayName = String(values.display_name || "").trim();
  const address = String(values.address || "").trim();
  const sshUser = String(values.ssh_user || "").trim();
  const credentialKind = values.credential_kind === "ssh_key" ? "ssh_key" : "ssh_password";
  if (!displayName || !address || !sshUser) {
    setM2HostError(invalidM2Input("请完整填写 HOST、SSH 用户和登录凭据"));
    return;
  }
  state.m2.setup = { displayName, address, port: String(port), sshUser, credentialKind, error: null };
  state.m2.busy = true;
  renderApp();
  try {
    const existingHosts = await state.dataClient.getHosts();
    const existing = existingHosts.data.find((candidate) => candidate.address === address
      && candidate.port === port
      && candidate.ssh_user === sshUser);
    const secret = await createSshCredential(values, "host-secret");
    let hostId = existing?.host_id;
    if (!hostId) {
      const host = await state.dataClient.createHost({
        display_name: displayName,
        address,
        port,
        ssh_user: sshUser,
        credential_ref: secret.data.credential_ref,
      }, dataApi.createIdempotencyKey("host"));
      hostId = host.data.host_id;
    } else {
      await state.dataClient.updateHost(hostId, {
        display_name: displayName,
        credential_ref: secret.data.credential_ref,
      }, dataApi.createIdempotencyKey("host-credential-refresh"));
    }
    state.selectedHostId = hostId;
    state.selectedWorldId = hostId;
    const connection = await state.dataClient.createConnectionTest(hostId, dataApi.createIdempotencyKey("fingerprint"));
    state.m2.setup = { ...state.m2.setup, hostId, error: null };
    if (connection.data.state === "host_key_unverified") {
      state.m2.setup.fingerprint = connection.data.candidate_fingerprint;
      state.m2.modal = { type: "host", phase: "fingerprint" };
      state.m2.busy = false;
      renderApp();
      return;
    }
    if (connectionReadyForDiscovery(connection)) {
      state.m2.setup = { ...state.m2.setup, connectionState: connection.data.state, authTransport: connection.data.auth_transport || null };
      await startM2Discovery(hostId, connection);
      return;
    }
    throw new dataApi.DataSourceError(connection.data.error_code || "CONNECTION_NOT_READY", "HOST 连接检查没有进入可扫描状态", { details: connection.data });
  } catch (error) {
    setM2HostError(error);
  }
}

async function confirmM2HostKeyAndScan() {
  const setup = state.m2.setup;
  if (!setup?.hostId || !setup.fingerprint) {
    setM2HostError(invalidM2Input("缺少待确认的 HOST 指纹"));
    return;
  }
  state.m2.busy = true;
  renderApp();
  try {
    await state.dataClient.confirmHostKey(setup.hostId, setup.fingerprint, dataApi.createIdempotencyKey("host-key"));
    const connection = await state.dataClient.createConnectionTest(setup.hostId, dataApi.createIdempotencyKey("connect"));
    if (!connectionReadyForDiscovery(connection)) {
      throw new dataApi.DataSourceError(
        connection.data.error_code || "CONNECTION_NOT_READY",
        connection.data.error_summary || "HOST 认证或能力检查未通过",
        { details: connection.data },
      );
    }
    state.m2.setup = { ...state.m2.setup, connectionState: connection.data.state, authTransport: connection.data.auth_transport || null };
    await startM2Discovery(setup.hostId, connection);
  } catch (error) {
    setM2HostError(error);
  }
}

async function startM2Discovery(hostId, connection) {
  const request = dataApi.discoveryRequestForConnection(connection);
  state.selectedHostId = hostId;
  state.selectedWorldId = hostId;
  state.m2.modal = { type: "host", phase: "scanning" };
  state.m2.busy = true;
  state.m2.setup = {
    ...(state.m2.setup || {}),
    hostId,
    providerKinds: request.provider_kinds,
    requestedCapabilities: request.requested_capabilities,
  };
  renderApp();
  try {
    const accepted = await state.dataClient.createDiscoveryRun(hostId, dataApi.createIdempotencyKey("scan"), request);
    state.m2.setup = { ...state.m2.setup, hostId, runId: accepted.data.run_id, runState: accepted.data.state, error: null };
    await pollM2Discovery(accepted.data.run_id);
  } catch (error) {
    setM2HostError(error);
  }
}

async function pollM2Discovery(runId) {
  for (let attempt = 0; attempt < 180; attempt += 1) {
    const run = await state.dataClient.getDiscoveryRun(runId);
    state.m2.setup = { ...state.m2.setup, runId, runState: run.data.state };
    const outcome = dataApi.discoveryRunOutcome(run.data);
    if (["complete", "partial", "unavailable"].includes(outcome)) {
      if (run.data.draft_id) {
        const draft = await state.dataClient.getProjectionDraft(run.data.draft_id);
        applyM2Draft(draft);
        await loadM3ForRun(run.data);
      }
      state.m2.modal = null;
      state.m2.busy = false;
      await refreshM0Data({ initial: true });
      renderApp();
      const hasDraft = Boolean(run.data.draft_id);
      const message = outcome === "partial"
        ? (hasDraft ? "部分发现 · 已生成本地投影草稿" : "部分发现 · 暂无可投影对象")
        : outcome === "unavailable"
          ? "发现不可用 · SSH 连接仍保持就绪"
          : (hasDraft ? "发现完成 · 已生成本地投影草稿" : "发现完成 · 暂无可投影对象");
      showToast(message);
      return;
    }
    if (outcome === "failed") {
      throw new dataApi.DataSourceError(run.data.failure_code || "DISCOVERY_FAILED", run.data.failure_summary || "HOST 扫描未完成", { details: run.data });
    }
    if (attempt % 4 === 0) renderApp();
    await new Promise((resolve) => window.setTimeout(resolve, 250));
  }
  throw new dataApi.DataSourceError("DISCOVERY_TIMEOUT", "等待 HOST 扫描超过 45 秒");
}

async function refreshM3Model() {
  if (state.dataClient.transport !== "http") return;
  try {
    const response = await state.dataClient.getModelProvider();
    state.m3.model = response.data;
  } catch (error) {
    if (error.code === "NOT_FOUND") state.m3.model = null;
    else state.m3.error = m2Error(error);
  }
}

async function loadM3ForRun(run) {
  if (!run?.run_id || state.dataClient.transport !== "http") return;
  state.m3.diff = null;
  state.m3.session = null;
  if (run.diff_id) {
    try {
      const diff = await state.dataClient.getDiscoveryDiff(run.run_id);
      state.m3.diff = diff.data;
    } catch (error) {
      if (error.code !== "NOT_FOUND") state.m3.error = m2Error(error);
    }
  }
  try {
    const proposal = await state.dataClient.getDiscoveryProposal(run.run_id);
    state.m3.session = proposal.data;
  } catch (error) {
    if (error.code !== "NOT_FOUND") state.m3.error = m2Error(error);
  }
}

async function submitM3Model(form) {
  const values = Object.fromEntries(new FormData(form).entries());
  const baseUrl = String(values.base_url || "").trim();
  const model = String(values.model || "").trim();
  const apiKey = String(values.api_key || "").trim();
  if (!baseUrl || !model || !apiKey) {
    state.m3.error = m2Error(invalidM2Input("请完整填写 URL、模型和 Key"));
    renderApp();
    return;
  }
  state.m3.busy = true;
  state.m3.error = null;
  renderApp();
  try {
    const secret = await state.dataClient.createModelSecretRef(apiKey, dataApi.createIdempotencyKey("model-secret"));
    const configured = await state.dataClient.putModelProvider({
      base_url: baseUrl,
      model,
      credential_ref: secret.data.credential_ref,
    }, dataApi.createIdempotencyKey("model-config"));
    state.m3.model = configured.data;
    const tested = await state.dataClient.testModelProvider(dataApi.createIdempotencyKey("model-test"));
    state.m3.modelTest = tested.data;
    await refreshM0Data({ initial: true });
    if (tested.data.state !== "reachable") {
      throw new dataApi.DataSourceError(tested.data.error_code || "MODEL_TEST_FAILED", "模型连接测试未通过", { details: tested.data });
    }
    state.m2.modal = null;
    showToast("模型配置已保存 · 连接可用");
  } catch (error) {
    state.m3.error = m2Error(error);
  } finally {
    state.m3.busy = false;
    renderApp();
  }
}

async function generateM3Session() {
  const draft = activeM2Draft();
  if (!draft) return;
  state.m3.busy = true;
  state.m3.error = null;
  renderApp();
  try {
    const response = await state.dataClient.createOnboardingSession(
      draft.draft_id,
      dataApi.createIdempotencyKey("onboarding"),
    );
    state.m3.session = response.data;
    showToast(response.data.state === "ready" ? "Agent 建议已生成" : `Agent · ${m3SessionLabel(response.data)}`);
  } catch (error) {
    state.m3.error = m2Error(error);
  } finally {
    state.m3.busy = false;
    renderApp();
  }
}

async function mutateM3Proposal(action, proposalId, operations = null) {
  const draft = activeM2Draft();
  const session = state.m3.session;
  if (!draft || !session?.session_id || !proposalId) return;
  state.m3.busy = true;
  state.m3.error = null;
  renderApp();
  try {
    const body = { action, proposal_id: proposalId };
    if (["adopt", "modify", "undo"].includes(action)) body.base_revision = draft.revision;
    if (action === "modify") body.operations = operations;
    const response = await state.dataClient.sendOnboardingMessage(
      session.session_id,
      body,
      dataApi.createIdempotencyKey(`proposal-${action}`),
    );
    state.m3.session = response.data;
    if (["adopt", "modify", "undo"].includes(action)) await refreshM2Draft();
    state.m2.modal = null;
    showToast(({ adopt: "建议已采用", modify: "修改建议已采用", reject: "建议已拒绝", undo: "采用已撤销" })[action] || "建议已更新");
  } catch (error) {
    state.m3.error = m2Error(error);
    if (["PRECONDITION_FAILED", "UNDO_HAS_INTERVENING_CHANGES"].includes(error.code)) {
      try {
        await refreshM2Draft();
        const refreshed = await state.dataClient.getOnboardingSession(session.session_id);
        state.m3.session = refreshed.data;
      } catch (_) { /* keep the original conflict */ }
    }
  } finally {
    state.m3.busy = false;
    renderApp();
  }
}

function modifiedM3Operations(form, proposal) {
  const values = Object.fromEntries(new FormData(form).entries());
  return proposal.patch.map((operation, index) => {
    const next = JSON.parse(JSON.stringify(operation));
    const prefix = `operation_${index}`;
    if (next.op === "rename") next.label = String(values[`${prefix}_label`] || "").trim();
    if (next.op === "assign_project") next.project_id = values[`${prefix}_project_id`] || null;
    if (next.op === "create_project") {
      next.project_id = String(values[`${prefix}_project_id`] || "").trim();
      next.label = String(values[`${prefix}_label`] || "").trim();
      next.subtitle = String(values[`${prefix}_subtitle`] || "").trim();
    }
    if (next.op === "add_relation") {
      next.to = values[`${prefix}_to`];
      next.label = String(values[`${prefix}_label`] || "").trim();
    }
    if (next.op === "move") {
      next.position = { x: Number(values[`${prefix}_x`]), y: Number(values[`${prefix}_y`]) };
    }
    return next;
  });
}

async function submitM3ProposalModify(form) {
  const proposalId = form.dataset.proposalId;
  const proposal = state.m3.session?.proposals?.find((candidate) => candidate.proposal_id === proposalId);
  if (!proposal) return;
  await mutateM3Proposal("modify", proposalId, modifiedM3Operations(form, proposal));
}

async function submitM3Question(form) {
  const session = state.m3.session;
  if (!session?.session_id) return;
  const kind = form.dataset.questionKind;
  const value = kind === "confirm"
    ? Boolean(form.querySelector("[name='value']")?.checked)
    : String(new FormData(form).get("value") || "").trim();
  state.m3.busy = true;
  state.m3.error = null;
  renderApp();
  try {
    const response = await state.dataClient.sendOnboardingMessage(
      session.session_id,
      { action: "answer", question_id: form.dataset.questionId, answer: { type: kind, value } },
      dataApi.createIdempotencyKey("question"),
    );
    state.m3.session = response.data;
    showToast("回答已保存");
  } catch (error) {
    state.m3.error = m2Error(error);
  } finally {
    state.m3.busy = false;
    renderApp();
  }
}

async function submitM2Editor(form) {
  const type = form.dataset.m2Form;
  const values = Object.fromEntries(new FormData(form).entries());
  const modal = state.m2.modal || {};
  if (type === "confirm") {
    await confirmM2Draft();
    return;
  }
  if (type === "host-alias" || type === "host-connection") {
    const displayName = String(values.display_name || "").trim();
    const hostId = modal.hostId;
    if (!displayName || !hostId) {
      showToast("服务器别名不能为空");
      return;
    }
    state.m2.busy = true;
    renderApp();
    try {
      const host = state.hosts.find((candidate) => candidate.host_id === hostId);
      const changes = type === "host-alias" ? { display_name: displayName } : {
        display_name: displayName,
        address: String(values.address || "").trim(),
        port: Number(values.port),
        ssh_user: String(values.ssh_user || "").trim(),
      };
      await state.dataClient.updateHost(hostId, changes, dataApi.createIdempotencyKey("host-connection"));
      await refreshM0Data({ initial: true });
      state.selectedHostId = hostId;
      state.selectedWorldId = hostId;
      state.m2.modal = null;
      showToast(type === "host-alias" ? "服务器别名已保存" : "服务器连接信息已保存 · 请重新核对指纹");
    } catch (error) {
      showToast(`别名保存失败 · ${m2Error(error).code}`);
    } finally {
      state.m2.busy = false;
      renderApp();
    }
    return;
  }
  if (type === "host-credential") {
    const hostId = modal.hostId;
    if (!hostId) { showToast("服务器记录不存在"); return; }
    state.m2.busy = true;
    renderApp();
    try {
      const secret = await createSshCredential(values, "host-credential-secret");
      await state.dataClient.updateHost(hostId, { credential_ref: secret.data.credential_ref }, dataApi.createIdempotencyKey("host-credential-update"));
      await refreshM0Data({ initial: true });
      state.selectedHostId = hostId;
      state.selectedWorldId = hostId;
      state.m2.modal = null;
      state.m2.busy = false;
      await retrySelectedHostConnection();
    } catch (error) {
      state.m2.busy = false;
      showToast(`SSH 登录凭据保存失败 · ${m2Error(error).code}`);
      renderApp();
    }
    return;
  }
  if (type === "rename") {
    await mutateM2Draft([{ op: "rename", node_id: modal.nodeId, label: String(values.label || "").trim() }], "节点已更名");
    return;
  }
  if (type === "assign") {
    await mutateM2Draft([{ op: "assign_project", node_id: modal.nodeId, project_id: values.project_id || null }], "项目绑定已保存");
    return;
  }
  if (type === "create-project") {
    await mutateM2Draft([{
      op: "create_project",
      project_id: String(values.project_id || "").trim(),
      label: String(values.label || "").trim(),
      subtitle: String(values.subtitle || "").trim(),
    }], "本地项目已创建");
    return;
  }
  if (type === "relation") {
    await mutateM2Draft([{
      op: "add_relation",
      from: modal.nodeId,
      to: values.to,
      kind: values.kind,
      label: String(values.label || "").trim(),
    }], "本地关系已保存");
  }
}

async function refreshM2Draft() {
  const draftId = state.m2.draft?.draft_id;
  if (!draftId) return;
  const draft = await state.dataClient.getProjectionDraft(draftId);
  applyM2Draft(draft);
}

async function mutateM2Draft(operations, successMessage) {
  const draft = activeM2Draft();
  if (!draft) {
    showToast("当前没有可编辑的投影草稿。");
    return;
  }
  state.m2.busy = true;
  renderApp();
  try {
    const response = await state.dataClient.updateProjectionDraft(
      draft.draft_id,
      draft.revision,
      operations,
      dataApi.createIdempotencyKey("draft"),
    );
    applyM2Draft(response);
    state.m2.modal = null;
    showToast(successMessage);
  } catch (error) {
    if (error.code === "PRECONDITION_FAILED") {
      try { await refreshM2Draft(); } catch (_) { /* the original error remains the useful result */ }
    }
    showToast(`保存失败 · ${m2Error(error).code}`);
  } finally {
    state.m2.busy = false;
    renderApp();
  }
}

async function updateM2IgnoreRule(nodeId, action) {
  const draft = activeM2Draft();
  if (!draft || !nodeId) return;
  state.m2.busy = true;
  renderApp();
  try {
    const response = await state.dataClient.createIgnoreRule(
      draft.draft_id,
      nodeId,
      action === "restore" ? "restore" : "archive",
      draft.revision,
      dataApi.createIdempotencyKey("ignore"),
    );
    applyM2Draft(response);
    showToast(action === "restore" ? "节点已恢复" : "节点已归档");
  } catch (error) {
    showToast(`保存失败 · ${m2Error(error).code}`);
  } finally {
    state.m2.busy = false;
    renderApp();
  }
}

async function confirmM2Draft() {
  const draft = activeM2Draft();
  if (!draft) return;
  state.m2.busy = true;
  renderApp();
  try {
    const response = await state.dataClient.confirmProjection(
      draft.draft_id,
      draft.revision,
      dataApi.createIdempotencyKey("confirm"),
    );
    applyM2Version(response);
    state.m2.modal = null;
    showToast("本地投影已确认");
  } catch (error) {
    showToast(`确认失败 · ${m2Error(error).code}`);
  } finally {
    state.m2.busy = false;
    renderApp();
  }
}

async function persistM2WorldLayout(node) {
  const draft = activeM2Draft();
  if (!draft || !node) return;
  try {
    await state.dataClient.updateLayout(
      draft.layout.layout_id,
      draft.revision,
      [{ node_id: node.id, position: { x: node.x, y: node.y } }],
      dataApi.createIdempotencyKey("layout"),
    );
    await refreshM2Draft();
    renderApp();
  } catch (error) {
    showToast(`布局未保存 · ${m2Error(error).code}`);
    try { await refreshM2Draft(); renderApp(); } catch (_) { /* keep the visible local position until the next refresh */ }
  }
}

function handleContextAction(action) {
  if (action === "open-workflow") {
    enterProject(state.selectedWorldId, "workflow");
    state.workflowMode = "run";
  } else if (action === "focus-operations") {
    app.querySelector("#global-operations")?.scrollIntoView({ behavior: "auto", block: "nearest" });
    showToast("已定位业务统筹 Agent 最近操作；点击记录可切换项目焦点。");
    return;
  } else if (action === "open-reverse") {
    state.scope = "global";
    state.view = "resource";
    state.resourceLens = "reverse";
    state.contextOpen = false;
  } else if (action === "open-shared") {
    state.scope = "global";
    state.view = "resource";
    state.resourceLens = "shared";
    state.contextOpen = false;
  } else if (action === "open-project-resource") {
    const projectId = resources[state.selectedResource]?.projects?.find((id) => id === state.activeProject) || resources[state.selectedResource]?.projects?.[0] || state.activeProject;
    enterProject(projectId, "resource");
    state.selectedResource = resources[state.selectedResource]?.projects?.includes(projectId) ? state.selectedResource : projectResources[projectId]?.privateResources?.[0] || null;
    state.resourceLens = "project";
    state.contextOpen = false;
  } else if (action === "switch-run") {
    state.workflowMode = "run";
    state.contextOpen = false;
  } else if (action === "switch-edit") {
    state.workflowMode = "edit";
    state.contextOpen = false;
  } else {
    state.contextOpen = false;
  }
  renderApp();
}

function canvasControl(value) {
  const [kind, command] = value.split(":");
  const prefix = canvasPrefix(kind);
  const zoomKey = `${prefix}Zoom`;
  const panKey = `${prefix}Pan`;
  if (command === "zoom-in") state[zoomKey] = Math.min(1.8, state[zoomKey] + 0.12);
  if (command === "zoom-out") state[zoomKey] = Math.max(0.58, state[zoomKey] - 0.12);
  if (command === "fit") { state[zoomKey] = 0.9; state[panKey] = { x: 42, y: 22 }; }
  if (command === "reset") {
    state[zoomKey] = 1;
    state[panKey] = { x: 0, y: 0 };
    if (kind === "world") {
      worldNodes.splice(0, worldNodes.length, ...cloneNodes(worldNodesInitial));
    }
    if (kind === "workflow" && state.workflowMode === "edit") { state.workflowNodes = cloneNodes(workflowBase); state.draftChanges = 0; }
  }
  renderApp();
}

const worldNodesInitial = cloneNodes(worldNodes);

function bindWorldEvents() {
  const svg = app.querySelector("#world-canvas");
  if (!svg) return;
  svg.querySelectorAll("[data-world-node]").forEach((element) => {
    element.addEventListener("click", (event) => {
      if (consumeCanvasNodeClickSuppression()) return;
      selectWorldNode(element.dataset.worldNode);
    });
    element.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") { event.preventDefault(); element.click(); }
    });
    element.addEventListener("pointerdown", (event) => {
      if (m2Editable()) startNodeDrag(event, "world", element.dataset.worldNode);
      else startCanvasPanFromNode(event, svg, "world");
    });
  });
  bindCanvasPanAndZoom(svg, "world");
  app.querySelectorAll("[data-operation-project]").forEach((button) => button.addEventListener("click", () => { state.operationProject = button.dataset.operationProject; renderApp(); }));
   app.querySelectorAll("[data-operation-target]").forEach((button) => button.addEventListener("click", () => { const target = button.dataset.operationTarget; state.selectedWorldId = projects[target] ? target : "coordinator"; state.operationProject = projects[target] ? target : "all"; state.contextOpen = true; renderApp(); }));
}

function bindWorkflowEvents() {
  const svg = app.querySelector("#workflow-canvas");
  app.querySelectorAll("[data-flow-mode]").forEach((button) => button.addEventListener("click", () => {
    state.workflowMode = button.dataset.flowMode;
    if (state.workflowMode === "run" && !state.runSnapshot.some((node) => node.id === state.selectedFlowNode)) state.selectedFlowNode = "route";
    state.contextOpen = false;
    renderApp();
  }));
  app.querySelectorAll("[data-flow-action]").forEach((button) => button.addEventListener("click", () => {
    if (button.dataset.flowAction === "save") saveDraft();
    if (button.dataset.flowAction === "publish") publishDraft();
  }));
  app.querySelectorAll("[data-add-node]").forEach((button) => button.addEventListener("click", () => addDraftNode(button.dataset.addNode)));
  app.querySelectorAll("[data-run]").forEach((button) => button.addEventListener("click", () => { state.selectedRun = button.dataset.run; state.contextOpen = false; renderApp(); }));
  app.querySelectorAll("[data-run-event]").forEach((button) => button.addEventListener("click", () => {
    state.selectedTrace = button.dataset.runEvent;
    state.selectedFlowNode = button.dataset.runNode;
    state.contextOpen = true;
    renderApp();
  }));
  app.querySelectorAll("[data-project-context-resource]").forEach((button) => button.addEventListener("click", () => {
    const id = button.dataset.projectContextResource;
    if (!resources[id]) return;
    state.selectedResource = id;
    if (resources[id].scope === "共享资源") {
      state.scope = "global";
      state.view = "resource";
      state.resourceLens = "reverse";
      state.contextOpen = false;
    } else {
      state.scope = "project";
      state.view = "resource";
      state.resourceProject = state.activeProject;
      state.resourceLens = "project";
      state.contextOpen = true;
    }
    renderApp();
  }));
  svg.querySelectorAll("[data-flow-node]").forEach((element) => {
    element.addEventListener("click", () => {
      if (consumeCanvasNodeClickSuppression()) return;
      state.selectedFlowNode = element.dataset.flowNode;
      state.contextOpen = true;
      renderApp();
    });
    element.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") { event.preventDefault(); element.click(); }
    });
    element.addEventListener("pointerdown", (event) => {
      if (state.workflowMode === "edit") startNodeDrag(event, "workflow", element.dataset.flowNode);
      else startCanvasPanFromNode(event, svg, "workflow");
    });
  });
  bindCanvasPanAndZoom(svg, "workflow");
}

function bindResourceEvents() {
  const canvas = app.querySelector("#resource-canvas");
  if (!canvas) return;
  app.querySelectorAll("[data-resource-lens]").forEach((button) => button.addEventListener("click", () => {
    if (state.scope !== "global") return;
    state.resourceLens = button.dataset.resourceLens;
    if (state.resourceLens === "reverse" && resources[state.selectedResource]?.scope !== "共享资源") state.selectedResource = firstSharedResourceId();
    state.contextOpen = false;
    renderApp();
  }));
  app.querySelectorAll("[data-resource-node]").forEach((element) => {
    element.addEventListener("click", () => {
      if (consumeCanvasNodeClickSuppression()) return;
      const id = element.dataset.resourceNode;
      if (!resources[id]) return;
      const host = state.hosts.find((candidate) => candidate.host_id === id)
        || (resources[id].kind === "host" ? state.hosts.find((candidate) => candidate.host_id === id) : null);
      if (host) {
        selectHostContext(host.host_id);
        enterGlobalView("hosts");
        return;
      }
      state.selectedResource = id;
      if (state.scope === "project" && resources[id].scope === "共享资源") {
        state.scope = "global";
        state.view = "resource";
        state.resourceLens = "reverse";
        state.contextOpen = false;
      } else {
        state.contextOpen = true;
        if (state.scope === "global" && resources[id].scope === "共享资源" && state.resourceLens !== "reverse") state.resourceLens = "reverse";
      }
      renderApp();
    });
    element.addEventListener("keydown", (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); element.click(); } });
    element.addEventListener("pointerdown", (event) => startCanvasPanFromNode(event, canvas, "resource"));
  });
  app.querySelectorAll("[data-resource-project-node]").forEach((element) => {
    element.addEventListener("click", () => {
      if (consumeCanvasNodeClickSuppression()) return;
      const id = element.dataset.resourceProjectNode;
      if (projectResources[id]) { enterProject(id, "resource"); renderApp(); }
    });
    element.addEventListener("pointerdown", (event) => startCanvasPanFromNode(event, canvas, "resource"));
  });
  bindCanvasPanAndZoom(canvas, "resource");
}

function saveDraft() {
  if (state.workflowMode !== "edit") return;
  showToast(state.draftChanges ? `流程草稿已保存，包含 ${state.draftChanges} 项变更。` : "当前流程没有新的草稿变更。");
}

function publishDraft() {
  if (state.workflowMode !== "edit") return;
  if (!state.draftChanges) { showToast("没有需要发布的流程变更。"); return; }
  state.workflowVersion += 1;
  state.draftChanges = 0;
  state.workflowNodes.forEach((node) => { node.draftOnly = false; });
  showToast(`流程 v${state.workflowVersion} 已发布；RUN-028 仍固定使用 v3。`);
  renderApp();
}

function addDraftNode(kindId) {
  const kind = paletteKinds[kindId];
  const draftNodes = state.workflowNodes.filter((node) => node.draftOnly);
  const offset = draftNodes.length * 28;
  const id = `draft-${Date.now()}`;
  state.workflowNodes.push({ id, kind: kind.kind, title: `新增${kind.label}`, subtitle: "待配置", x: 682 - offset, y: 448 - offset, width: 138, height: 70, draftOnly: true });
  state.selectedFlowNode = id;
  state.draftChanges += 1;
  state.contextOpen = true;
  renderApp();
}

function bindCanvasPanAndZoom(svg, kind) {
  svg.addEventListener("pointerdown", (event) => {
    if (event.target.closest("[data-world-node], [data-flow-node], [data-resource-node], [data-resource-project-node]")) return;
    startCanvasPan(event, svg, kind);
  });
  svg.addEventListener("pointermove", (event) => handlePointerMove(svg, event));
  svg.addEventListener("pointerup", (event) => endPointerInteraction(svg, event));
  svg.addEventListener("lostpointercapture", (event) => endPointerInteraction(svg, event));
  svg.addEventListener("pointercancel", () => {
    state.drag = null;
    state.panDrag = null;
    state.suppressCanvasClick = false;
    if (state.renderDeferred) renderApp();
  });
  svg.addEventListener("click", (event) => {
    if (state.suppressCanvasClick && !event.target.closest("[data-world-node], [data-flow-node], [data-resource-node], [data-resource-project-node]")) {
      state.suppressCanvasClick = false;
    }
  });
  svg.addEventListener("wheel", (event) => {
    event.preventDefault();
    const prefix = canvasPrefix(kind);
    const point = rawSvgPoint(svg, event);
    const zoomKey = `${prefix}Zoom`;
    const panKey = `${prefix}Pan`;
    const next = Math.min(1.8, Math.max(0.58, state[zoomKey] * (event.deltaY > 0 ? 0.9 : 1.1)));
    const old = state[zoomKey];
    const worldX = (point.x - state[panKey].x) / old;
    const worldY = (point.y - state[panKey].y) / old;
    state[zoomKey] = next;
    state[panKey] = { x: point.x - worldX * next, y: point.y - worldY * next };
    updateSceneTransform(kind);
    updateZoomReadout();
  }, { passive: false });
}

function startCanvasPanFromNode(event, svg, kind) {
  if (event.button !== 0) return;
  event.stopPropagation();
  startCanvasPan(event, svg, kind);
}

function startCanvasPan(event, svg, kind) {
  if (event.button !== 0 || state.drag || state.panDrag) return;
  const point = rawSvgPoint(svg, event);
  const prefix = canvasPrefix(kind);
  state.suppressCanvasClick = false;
  state.panDrag = {
    kind,
    pointerId: event.pointerId,
    startX: point.x,
    startY: point.y,
    origin: { ...state[`${prefix}Pan`] },
    moved: false,
  };
  svg.setPointerCapture(event.pointerId);
}

const nodeDragThresholdPx = 5;

function startNodeDrag(event, kind, id) {
  if (event.button !== 0) return;
  event.stopPropagation();
  // A node gesture supersedes any stale blank-canvas pan bookkeeping. Without
  // this reset, a lost pan pointerup can consume the next deliberate node tap.
  state.panDrag = null;
  state.suppressCanvasClick = false;
  const svg = event.currentTarget.closest("svg");
  const prefix = kind === "world" ? "world" : "flow";
  const point = scenePoint(svg, event, state[`${prefix}Pan`], state[`${prefix}Zoom`]);
  const collection = kind === "world" ? worldNodes : state.workflowNodes;
  const node = collection.find((item) => item.id === id);
  if (!node) return;
  state.drag = {
    kind,
    id,
    pointerId: event.pointerId,
    startClientX: event.clientX,
    startClientY: event.clientY,
    offsetX: point.x - node.x,
    offsetY: point.y - node.y,
    moved: false,
    persist: kind === "world" && m2Editable(),
  };
  svg.setPointerCapture(event.pointerId);
}

function handlePointerMove(svg, event) {
  if (state.drag && state.drag.pointerId === event.pointerId) {
    const prefix = state.drag.kind === "world" ? "world" : "flow";
    const point = scenePoint(svg, event, state[`${prefix}Pan`], state[`${prefix}Zoom`]);
    const collection = state.drag.kind === "world" ? worldNodes : state.workflowNodes;
    const node = collection.find((item) => item.id === state.drag.id);
    if (!node) return;
    if (!state.drag.moved && Math.hypot(
      event.clientX - state.drag.startClientX,
      event.clientY - state.drag.startClientY,
    ) < nodeDragThresholdPx) return;
    state.drag.moved = true;
    node.x = Math.round(point.x - state.drag.offsetX);
    node.y = Math.round(point.y - state.drag.offsetY);
    const element = app.querySelector(`#${state.drag.kind === "world" ? "world" : "flow"}-node-${CSS.escape(node.id)}`);
    if (element) element.setAttribute("transform", `translate(${node.x} ${node.y})`);
    if (state.drag.kind === "world") updateWorldEdgeGeometry();
    if (state.drag.kind === "workflow") updateFlowEdgeGeometry();
    return;
  }
  if (state.panDrag && state.panDrag.pointerId === event.pointerId) {
    const point = rawSvgPoint(svg, event);
    const prefix = canvasPrefix(state.panDrag.kind);
    const deltaX = point.x - state.panDrag.startX;
    const deltaY = point.y - state.panDrag.startY;
    if (!state.panDrag.moved && Math.hypot(deltaX, deltaY) < 3) return;
    state.panDrag.moved = true;
    state[`${prefix}Pan`] = { x: state.panDrag.origin.x + deltaX, y: state.panDrag.origin.y + deltaY };
    updateSceneTransform(state.panDrag.kind);
  }
}

function updateEdgeElement(id, from, to) {
  if (!from || !to) return;
  const path = app.querySelector(`#${CSS.escape(id)}`);
  if (!path) return;
  path.setAttribute("d", curvePath(from, to));
  const label = path.parentElement?.querySelector(".edge-label");
  if (label) {
    const center = midpoint(from, to);
    label.setAttribute("x", center.x);
    label.setAttribute("y", center.y - 7);
  }
}

function updateWorldEdgeGeometry() {
  worldEdges.forEach((edge) => updateEdgeElement(
    edge.id,
    worldNodes.find((node) => node.id === edge.from),
    worldNodes.find((node) => node.id === edge.to),
  ));
}

function updateFlowEdgeGeometry() {
  const nodes = visibleFlowNodes();
  flowEdgeSpecs(nodes, state.workflowMode === "run").forEach((edge) => updateEdgeElement(
    edge.id,
    nodes.find((node) => node.id === edge.from),
    nodes.find((node) => node.id === edge.to),
  ));
}

function endPointerInteraction(svg, event) {
  let selectWorldNodeId = null;
  let renderAfterRelease = false;
  if (state.drag && state.drag.pointerId === event.pointerId) {
    const drag = state.drag;
    const wasFlow = drag.kind === "workflow";
    const moved = drag.moved;
    const selectWorldNodeOnRelease = drag.kind === "world" && !moved;
    const movedWorldNode = drag.kind === "world"
      ? worldNodes.find((node) => node.id === drag.id)
      : null;
    state.drag = null;
    // Pointer capture does not reliably synthesize click across all browsers.
    // A short press is a selection; only a drag changes the node position.
    setTransientCanvasClickSuppression(moved || selectWorldNodeOnRelease);
    if (moved && wasFlow) state.draftChanges += 1;
    if (moved && movedWorldNode && drag.persist) void persistM2WorldLayout({ ...movedWorldNode });
    if (moved) renderAfterRelease = true;
    if (selectWorldNodeOnRelease) selectWorldNodeId = drag.id;
  }
  if (state.panDrag && state.panDrag.pointerId === event.pointerId) {
    setTransientCanvasClickSuppression(state.panDrag.moved);
    state.panDrag = null;
  }
  if (svg.hasPointerCapture(event.pointerId)) svg.releasePointerCapture(event.pointerId);
  if (selectWorldNodeId) selectWorldNode(selectWorldNodeId);
  else if (renderAfterRelease) renderApp();
  else if (state.renderDeferred && !state.drag && !state.panDrag) renderApp();
}

function setTransientCanvasClickSuppression(moved) {
  state.suppressCanvasClick = moved;
  if (!moved) return;
  // The pointer release can synthesize one click on the dragged target. Keep
  // suppression through that event, then clear it so the next deliberate
  // node click is never consumed when a browser emits no synthetic click.
  window.setTimeout(() => {
    state.suppressCanvasClick = false;
  }, 0);
}

function consumeCanvasNodeClickSuppression() {
  const moved = Boolean(
    state.suppressCanvasClick
      || state.drag?.moved
      || state.panDrag?.moved,
  );
  if (moved) {
    // A completed drag can still leave bookkeeping behind when a browser
    // misses pointerup. Consume only its synthetic click, then free the next
    // deliberate node click to select its server context.
    state.drag = null;
    state.panDrag = null;
    state.suppressCanvasClick = false;
    if (state.renderDeferred) renderApp();
    return true;
  }
  // A browser or automation path can lose pointerup while a zero-distance
  // pan is still recorded.  It is a click, not a drag, so clear that stale
  // bookkeeping and let the node select its server context.
  state.drag = null;
  state.panDrag = null;
  state.suppressCanvasClick = false;
  if (state.renderDeferred) renderApp();
  return false;
}

function rawSvgPoint(svg, event) {
  const point = svg.createSVGPoint();
  point.x = event.clientX;
  point.y = event.clientY;
  return point.matrixTransform(svg.getScreenCTM().inverse());
}

function scenePoint(svg, event, pan, zoom) {
  const point = rawSvgPoint(svg, event);
  return { x: (point.x - pan.x) / zoom, y: (point.y - pan.y) / zoom };
}

function updateSceneTransform(kind) {
  const prefix = canvasPrefix(kind);
  const sceneName = kind === "world"
    ? "world"
    : kind === "workflow"
      ? "workflow"
      : "resource";
  const scene = app.querySelector(`#${sceneName}-scene`);
  if (scene) scene.setAttribute("transform", `translate(${state[`${prefix}Pan`].x} ${state[`${prefix}Pan`].y}) scale(${state[`${prefix}Zoom`]})`);
}

function updateZoomReadout() {
  const scale = state.view === "world"
    ? state.worldZoom
    : state.view === "workflow"
      ? state.flowZoom
      : state.resourceZoom;
  const label = app.querySelector(".zoom-readout");
  if (label) label.textContent = `${Math.round(scale * 100)}%`;
}

function canvasPrefix(kind) {
  return kind === "world" ? "world" : kind === "workflow" ? "flow" : "resource";
}

function showToast(message) {
  toast.textContent = message;
  toast.classList.add("visible");
  window.clearTimeout(window.networkAtlasToastTimer);
  window.networkAtlasToastTimer = window.setTimeout(() => toast.classList.remove("visible"), 2600);
}

function updateHash() {
  const params = new URLSearchParams({ scope: state.scope });
  if (state.scope === "project") params.set("project", state.activeProject);
  params.set("view", state.view);
  history.replaceState(null, "", `#${params.toString()}`);
}

document.addEventListener("keydown", (event) => {
  if (event.target.matches("input, textarea, select")) return;
  if (event.key === "1" || event.key === "2" || event.key === "3") {
    event.preventDefault();
    enterGlobalView(event.key === "1" ? "world" : event.key === "2" ? "resource" : "hosts");
    return;
  }
  if (event.key === "4" || event.key === "5") {
    event.preventDefault();
    if (event.key === "4" && state.dataSource.kind === "real") return;
    enterProject(state.activeProject, event.key === "4" ? "workflow" : "resource");
    if (state.view === "workflow") state.workflowMode = "run";
    state.contextOpen = false;
    renderApp();
    return;
  }
  if (event.key === "Escape" && state.contextOpen) {
    state.contextOpen = false;
    renderApp();
  }
});

renderApp();
void initializeApplication();
