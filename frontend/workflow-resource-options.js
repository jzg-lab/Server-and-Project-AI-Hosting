const initialResourceOption = location.hash.match(/resource=(project|shared|reverse)/)?.[1] || "project";
const state = {
  page: location.hash.includes("page=resource") ? "resource" : "workflow",
  workflowLens: location.hash.includes("lens=run") ? "run" : "definition",
  selectedStep: "route",
  workflowDetailOpen: false,
  resourceOption: initialResourceOption,
  project: location.hash.match(/project=(hermes|automation|knowledge|all)/)?.[1] || (initialResourceOption === "project" ? "hermes" : "all"),
  selectedResource: location.hash.match(/resourceId=(srv-01|storage-a|index-shared)/)?.[1] || "srv-01",
};

const workflowSteps = {
  trigger: {
    title: "消息触发",
    definition: ["入口条件", "收到合法消息", "连接器声明"],
    run: ["10:42:01", "18 ms", "已接收"],
  },
  understand: {
    title: "理解请求",
    definition: ["Agent 步骤", "识别意图与项目", "失败进入人工确认"],
    run: ["10:42:02", "420 ms", "识别为状态查询"],
  },
  route: {
    title: "路由决策",
    definition: ["条件分支", "低影响自动执行", "高影响进入审批"],
    run: ["10:42:03", "31 ms", "命中自动路径"],
  },
  approval: {
    title: "人工确认",
    definition: ["控制闸门", "仅高影响动作进入", "确认后继续"],
    run: ["本次跳过", "0 ms", "条件未命中"],
  },
  execute: {
    title: "调用能力",
    definition: ["适配器动作", "读取状态或发起请求", "等待外部回执"],
    run: ["10:42:03", "1.8 s", "请求已发出"],
  },
  result: {
    title: "验证结果",
    definition: ["完成条件", "重新读取外部状态", "失败进入恢复路径"],
    run: ["当前步骤", "已等待 12 s", "等待外部回执"],
  },
};

const projects = {
  hermes: {
    label: "Hermes",
    subtitle: "消息网关与 Agent",
    privateResources: ["domain-h", "repo-h", "knowledge-h", "token-h"],
    sharedResources: ["srv-01", "storage-a"],
  },
  automation: {
    label: "自动化补池",
    subtitle: "自动化工作流",
    privateResources: ["domain-a", "repo-a", "queue-a", "token-a"],
    sharedResources: ["srv-01", "storage-a"],
  },
  knowledge: {
    label: "知识整理",
    subtitle: "知识索引与同步",
    privateResources: ["domain-k", "repo-k", "vault-k", "token-k"],
    sharedResources: ["storage-a", "index-shared"],
  },
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
  "repo-a": { label: "WORKFLOW_REPO", type: "代码仓库", scope: "自动化补池私有", health: "已同步", tone: "green", projects: ["automation"], freshness: "9 分钟前", source: "REPO_REF", relation: "脚本与流程来源", meta: "项目私有" },
  "queue-a": { label: "QUEUE_REF", type: "任务队列", scope: "自动化补池私有", health: "健康", tone: "green", projects: ["automation"], freshness: "22 秒前", source: "QUEUE_REF", relation: "保存待执行任务", meta: "队列深度 4" },
  "token-a": { label: "TOKEN_REF_B", type: "凭据引用", scope: "自动化补池私有", health: "有效", tone: "green", projects: ["automation"], freshness: "14 分钟前", source: "SECRET_REF", relation: "授权执行能力", meta: "仅显示引用状态" },
  "domain-k": { label: "QUERY_ENDPOINT", type: "查询入口", scope: "知识整理私有", health: "健康", tone: "green", projects: ["knowledge"], freshness: "4 分钟前", source: "ENDPOINT_REF", relation: "提供查询", meta: "项目私有" },
  "repo-k": { label: "INDEX_REPO", type: "代码仓库", scope: "知识整理私有", health: "已同步", tone: "green", projects: ["knowledge"], freshness: "18 分钟前", source: "REPO_REF", relation: "索引代码来源", meta: "项目私有" },
  "vault-k": { label: "VAULT_REF", type: "知识存储", scope: "知识整理私有", health: "待核验", tone: "amber", projects: ["knowledge"], freshness: "8 分钟前", source: "VAULT_REF", relation: "保存知识文件", meta: "连接器待核验" },
  "token-k": { label: "TOKEN_REF_C", type: "凭据引用", scope: "知识整理私有", health: "有效", tone: "green", projects: ["knowledge"], freshness: "21 分钟前", source: "SECRET_REF", relation: "授权查询能力", meta: "仅显示引用状态" },
};

const resourceOptionMeta = {
  project: {
    mode: "项目资源舱",
    summary: [["当前项目", "Hermes"], ["私有资源", "4"], ["共享资源", "2"], ["需关注", "0"]],
    rationale: "项目边界最清楚",
    copy: "日常查看先选项目，资源按计算、入口、代码、数据与凭据分布。共享实体位于项目边界外，并标出还有哪些项目在使用。",
    note: "推荐作为默认入口",
  },
  shared: {
    mode: "共享资源集合",
    summary: [["共享资源", "3"], ["连接项目", "4"], ["使用关系", "7"], ["需关注", "1"]],
    rationale: "跨项目盘点最清楚",
    copy: "每个共享资源只出现一次，项目围绕共享集合分布。适合检查共享覆盖、重复依赖和项目级禁用。",
    note: "作为汇总入口",
  },
  reverse: {
    mode: "资源影响反查",
    summary: [["选中资源", "SRV-01"], ["关联项目", "2"], ["工作负载", "3"], ["影响级别", "中"]],
    rationale: "故障影响范围最清楚",
    copy: "共享资源位于中心，向外展开使用它的项目和工作负载。适合回答“一台服务器出问题会影响谁”。",
    note: "点击共享资源后进入",
  },
};

const workflowCanvas = document.querySelector("#workflow-canvas");
const resourceCanvas = document.querySelector("#resource-canvas");
const workflowDetail = document.querySelector("#workflow-detail");
const resourceInspector = document.querySelector("#resource-inspector");
const toast = document.querySelector("#toast");

function graphEdge(id, d, tone = "") {
  return `<path id="${id}" class="graph-edge ${tone}" d="${d}"></path>`;
}

function movingDot(pathId, tone = "") {
  return `<circle r="4" class="moving-dot ${tone}" aria-hidden="true"><animateMotion dur="2.8s" repeatCount="indefinite"><mpath href="#${pathId}"></mpath></animateMotion></circle>`;
}

function edgeLabel(x, y, text) {
  const width = Math.max(54, text.length * 9 + 12);
  return `<g transform="translate(${x - width / 2} ${y - 10})"><rect class="edge-label-bg" width="${width}" height="20" rx="5"></rect><text class="edge-label" x="${width / 2}" y="13" text-anchor="middle">${text}</text></g>`;
}

function workflowNode({ id, x, y, title, kicker, meta, stateText, tone = "", className = "", w = 146, h = 74 }) {
  const selected = id === state.selectedStep ? "is-selected" : "";
  return `<g class="graph-node ${className} ${selected}" data-step="${id}" role="button" tabindex="0" aria-label="查看步骤 ${title}">
    <rect class="node-box" x="${x}" y="${y}" width="${w}" height="${h}" rx="8"></rect>
    <text class="node-kicker" x="${x + 13}" y="${y + 18}">${kicker}</text>
    <text class="node-title" x="${x + 13}" y="${y + 40}">${title}</text>
    <text class="node-meta" x="${x + 13}" y="${y + 59}">${meta}</text>
    <text class="node-state ${tone}" x="${x + w - 12}" y="${y + 18}" text-anchor="end">${stateText}</text>
  </g>`;
}

function workflowNodes(lens) {
  const isRun = lens === "run";
  return [
    workflowNode({ id: "trigger", x: 45, y: 243, title: "消息触发", kicker: "TRIGGER", meta: isRun ? "10:42:01 · 18 ms" : "收到合法消息", stateText: isRun ? "完成" : "入口", tone: isRun ? "live" : "" }),
    workflowNode({ id: "understand", x: 225, y: 243, title: "理解请求", kicker: "AGENT", meta: isRun ? "420 ms · 状态查询" : "识别意图与项目", stateText: isRun ? "完成" : "步骤", tone: isRun ? "live" : "" }),
    workflowNode({ id: "route", x: 405, y: 243, title: "路由决策", kicker: "CONDITION", meta: isRun ? "31 ms · 自动路径" : "低影响 / 高影响", stateText: isRun ? "完成" : "2 分支", tone: isRun ? "live" : "" }),
    workflowNode({ id: "approval", x: 590, y: 95, title: "人工确认", kicker: "APPROVAL", meta: isRun ? "本次条件未命中" : "高影响动作进入", stateText: isRun ? "跳过" : "闸门", className: isRun ? "inactive" : "", tone: isRun ? "" : "attention" }),
    workflowNode({ id: "execute", x: 590, y: 390, title: "调用能力", kicker: "ADAPTER", meta: isRun ? "1.8 s · 请求已发出" : "读取或发起请求", stateText: isRun ? "完成" : "动作", tone: isRun ? "live" : "" }),
    workflowNode({ id: "result", x: 805, y: 243, title: "验证结果", kicker: "VERIFY", meta: isRun ? "等待回执 · 12 s" : "重读外部状态", stateText: isRun ? "当前" : "完成条件", tone: isRun ? "current" : "", className: isRun ? "current" : "" }),
  ].join("");
}

function renderWorkflowCanvas() {
  const run = state.workflowLens === "run";
  const edgeTone = run ? "active flowing" : "expected";
  const inactiveTone = run ? "muted" : "expected";
  const markup = [
    `<rect class="boundary" x="25" y="54" width="950" height="455" rx="34"></rect>`,
    `<text class="boundary-label" x="48" y="80">${run ? "RUN INSTANCE · RUN-028" : "WORKFLOW DEFINITION · VERSION 3"}</text>`,
    graphEdge("wf-1", "M191 280 L225 280", edgeTone),
    graphEdge("wf-2", "M371 280 L405 280", edgeTone),
    graphEdge("wf-approval", "M551 262 C575 245 563 132 590 132", inactiveTone),
    graphEdge("wf-approval-result", "M736 132 C790 132 770 260 805 272", inactiveTone),
    graphEdge("wf-execute", "M551 300 C578 320 563 427 590 427", edgeTone),
    graphEdge("wf-execute-result", "M736 427 C790 427 770 298 805 287", edgeTone),
    graphEdge("wf-fallback", "M878 317 C878 505 477 535 477 317", run ? "muted" : "fallback"),
    edgeLabel(570, 205, run ? "条件未命中" : "高影响"),
    edgeLabel(568, 352, run ? "实际路径" : "低影响"),
    edgeLabel(686, 510, "恢复 / 重试"),
    run ? movingDot("wf-execute") + movingDot("wf-execute-result") : "",
    workflowNodes(state.workflowLens),
  ].join("");
  workflowCanvas.innerHTML = markup;
  workflowCanvas.querySelectorAll("[data-step]").forEach((node) => {
    const select = () => {
      state.selectedStep = node.dataset.step;
      state.workflowDetailOpen = true;
      renderWorkflowCanvas();
      renderWorkflowDetail();
    };
    node.addEventListener("click", select);
    node.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select();
      }
    });
  });
}

function renderWorkflowDetail() {
  const step = workflowSteps[state.selectedStep];
  const facts = step[state.workflowLens];
  workflowDetail.className = `floating-detail ${state.workflowDetailOpen ? "is-open" : ""}`;
  workflowDetail.innerHTML = `<div class="detail-kicker">${state.workflowLens === "run" ? "RUN STEP" : "STEP DEFINITION"}</div>
    <h3>${step.title}</h3>
    <p>${state.workflowLens === "run" ? "这里记录本次执行的时间和事实。" : "这里定义每次执行都要遵守的规则。"}</p>
    <div class="detail-facts">
      <div class="detail-fact"><span>${state.workflowLens === "run" ? "发生时间" : "步骤类型"}</span><strong>${facts[0]}</strong></div>
      <div class="detail-fact"><span>${state.workflowLens === "run" ? "耗时" : "规则"}</span><strong>${facts[1]}</strong></div>
    </div>`;
}

function renderWorkflowMeta() {
  const run = state.workflowLens === "run";
  document.querySelector("#workflow-summary").innerHTML = (run
    ? [["运行实例", "RUN-028", "cyan"], ["当前进度", "5 / 6", ""], ["已耗时", "14.3 s", ""], ["状态", "等待回执", "amber"], ["属性", "只读事实", "summary-spacer"]]
    : [["流程版本", "v3", "cyan"], ["状态", "已发布", ""], ["步骤", "6", ""], ["分支", "2", ""], ["属性", "可编辑蓝图", "summary-spacer"]])
    .map(([label, value, tone]) => `<span class="summary-chip ${tone || ""}">${label}<strong>${value}</strong></span>`).join("");

  const mode = document.querySelector("#workflow-mode");
  mode.className = `canvas-mode ${run ? "live" : ""}`;
  mode.innerHTML = `<i></i>${run ? "一次真实执行" : "可复用蓝图"}`;
  document.querySelector("#workflow-legend").innerHTML = run
    ? `<span><i class="legend-line active"></i>本次实际路径</span><span><i class="legend-line"></i>未命中分支</span><span><i class="legend-line fallback"></i>等待回执</span>`
    : `<span><i class="legend-line expected"></i>定义路径</span><span><i class="legend-line fallback"></i>恢复路径</span><span><i class="legend-line"></i>控制闸门</span>`;
  document.querySelector("#workflow-hint").textContent = run
    ? "运行实例只记录这一次的实际路径、耗时和回执，节点布局保持只读。"
    : "流程框架定义未来每一次执行都要遵守的步骤、分支、闸门和恢复路径。";
  document.querySelector("#semantic-contrast").innerHTML = (run
    ? [["对象", "一次执行", "RUN-028 只发生一次"], ["内容", "事实与时间", "走过哪些节点、耗时多久"], ["交互", "观察与干预", "定位阻塞、请求暂停或改向"], ["结果", "回执与验证", "外部结果回来后才结束"]]
    : [["对象", "可复用蓝图", "一个定义可以产生很多次运行"], ["内容", "步骤与规则", "触发器、分支、闸门、恢复路径"], ["交互", "编辑与发布", "先形成草稿，再比较和发布"], ["结果", "版本", "v3 定义未来怎样执行"]])
    .map(([label, title, copy]) => `<div class="contrast-card"><span>${label}</span><strong>${title}</strong><small>${copy}</small></div>`).join("");
}

function renderWorkflow() {
  document.querySelectorAll("[data-workflow-lens]").forEach((button) => {
    const active = button.dataset.workflowLens === state.workflowLens;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  renderWorkflowMeta();
  renderWorkflowCanvas();
  renderWorkflowDetail();
}

function resourceNode({ id, x, y, w = 150, h = 72, kicker, title, meta, status, tone = "", scope = "private", className = "" }) {
  const selected = id === state.selectedResource ? "is-selected" : "";
  return `<g class="graph-node resource ${scope === "shared" ? "shared-resource" : "private-resource"} ${className} ${selected}" data-resource="${id}" role="button" tabindex="0" aria-label="查看资源 ${title}">
    <rect class="node-box" x="${x}" y="${y}" width="${w}" height="${h}" rx="8"></rect>
    <text class="node-kicker" x="${x + 13}" y="${y + 17}">${kicker}</text>
    <text class="node-title" x="${x + 13}" y="${y + 39}">${title}</text>
    <text class="node-meta" x="${x + 13}" y="${y + 57}">${meta}</text>
    <text class="node-state ${tone}" x="${x + w - 12}" y="${y + 17}" text-anchor="end">${status}</text>
  </g>`;
}

function projectNode({ id, x, y, title, meta, status = "已登记", tone = "live", w = 170, h = 78, className = "" }) {
  const selected = state.project === id ? "is-selected" : "";
  return `<g class="graph-node project ${className} ${selected}" data-project-node="${id}" role="button" tabindex="0" aria-label="查看项目 ${title}">
    <rect class="node-box" x="${x}" y="${y}" width="${w}" height="${h}" rx="9"></rect>
    <text class="node-kicker" x="${x + 14}" y="${y + 18}">PROJECT</text>
    <text class="node-title" x="${x + 14}" y="${y + 42}">${title}</text>
    <text class="node-meta" x="${x + 14}" y="${y + 61}">${meta}</text>
    <text class="node-state ${tone}" x="${x + w - 12}" y="${y + 18}" text-anchor="end">${status}</text>
  </g>`;
}

function selectedProject() {
  return projects[state.project === "all" ? "hermes" : state.project];
}

function renderProjectResources() {
  const profile = selectedProject();
  const ids = profile.privateResources;
  const positions = [[295,110],[555,110],[295,397],[555,397]];
  const privateNodes = ids.map((id, index) => {
    const resource = resources[id];
    const [x,y] = positions[index];
    return resourceNode({ id, x, y, kicker: resource.type.toUpperCase(), title: resource.label, meta: resource.meta, status: resource.health, tone: resource.tone === "green" ? "live" : "attention", scope: "private" });
  }).join("");
  const privateEdges = positions.map(([x,y], index) => {
    const endX = x < 450 ? x + 150 : x;
    const endY = y + 36;
    const startX = x < 450 ? 415 : 585;
    const startY = y < 250 ? 245 : 315;
    return graphEdge(`private-${index}`, `M${startX} ${startY} C${(startX+endX)/2} ${startY} ${(startX+endX)/2} ${endY} ${endX} ${endY}`, "private");
  }).join("");
  const otherProject = state.project === "automation" ? "hermes" : "automation";
  const knowledgeProject = state.project === "knowledge" ? "hermes" : "knowledge";
  const activeProjectId = state.project === "all" ? "hermes" : state.project;
  return `<rect class="boundary" x="235" y="72" width="530" height="440" rx="36"></rect>
    <text class="boundary-label" x="258" y="97">${profile.label.toUpperCase()} · PROJECT RESOURCE BOUNDARY</text>
    ${graphEdge("project-srv", "M405 278 C310 278 300 292 195 292", "shared")}
    ${graphEdge("project-store", "M595 278 C690 278 700 292 805 292", "shared")}
    ${graphEdge("srv-other", "M115 256 C115 205 128 182 128 156", "usage")}
    ${graphEdge("store-other", "M880 256 C880 205 870 182 870 156", "usage")}
    ${privateEdges}
    ${edgeLabel(278,270,"承载于")}${edgeLabel(720,270,"写入")}
    ${projectNode({ id: activeProjectId, x: 405, y: 237, w: 190, h: 82, title: profile.label, meta: `${profile.privateResources.length} 私有 · ${profile.sharedResources.length} 共享`, status: "已选中" })}
    ${privateNodes}
    ${resourceNode({ id: "srv-01", x: 35, y: 256, w: 160, h: 76, kicker: "SHARED SERVER", title: "SRV-01", meta: "2 项目 · 3 工作负载", status: "共享", tone: "shared", scope: "shared" })}
    ${resourceNode({ id: "storage-a", x: 805, y: 256, w: 160, h: 76, kicker: "SHARED STORAGE", title: "STORAGE_A", meta: "3 项目 · 4 个绑定", status: "共享", tone: "shared", scope: "shared" })}
    ${projectNode({ id: otherProject, x: 48, y: 88, w: 160, h: 68, title: projects[otherProject].label, meta: "也使用 SRV-01", status: "关联", className: "mini" })}
    ${projectNode({ id: knowledgeProject, x: 792, y: 88, w: 160, h: 68, title: projects[knowledgeProject].label, meta: "也使用 STORAGE_A", status: "关联", className: "mini" })}`;
}

function relationshipTone(projectId) {
  return state.project === "all" || state.project === projectId ? "usage" : "usage dimmed";
}

function renderSharedCollection() {
  const projectNodes = [
    projectNode({ id: "hermes", x: 55, y: 100, title: "Hermes", meta: "2 项共享资源" }),
    projectNode({ id: "automation", x: 55, y: 402, title: "自动化补池", meta: "2 项共享资源" }),
    projectNode({ id: "knowledge", x: 775, y: 100, title: "知识整理", meta: "2 项共享资源", status: "1 待核验", tone: "attention" }),
    projectNode({ id: "lab", x: 775, y: 402, title: "实验项目", meta: "共享观察已禁用", status: "禁用", tone: "attention" }),
  ].join("");
  return `<rect class="boundary shared-zone" x="315" y="74" width="370" height="430" rx="36"></rect>
    <text class="boundary-label green" x="338" y="100">SHARED RESOURCE COLLECTION · 实体只出现一次</text>
    ${graphEdge("sc-srv-h", "M390 184 C290 184 260 139 225 139", relationshipTone("hermes"))}
    ${graphEdge("sc-srv-a", "M390 184 C290 240 260 441 225 441", relationshipTone("automation"))}
    ${graphEdge("sc-store-h", "M390 326 C295 305 265 160 225 150", relationshipTone("hermes"))}
    ${graphEdge("sc-store-a", "M390 326 C295 360 265 430 225 441", relationshipTone("automation"))}
    ${graphEdge("sc-store-k", "M610 326 C700 310 730 166 775 150", relationshipTone("knowledge"))}
    ${graphEdge("sc-index-k", "M610 430 C700 390 730 166 775 150", relationshipTone("knowledge"))}
    ${graphEdge("sc-index-h", "M390 430 C300 360 260 165 225 150", relationshipTone("hermes"))}
    ${resourceNode({ id: "srv-01", x: 390, y: 145, w: 220, h: 78, kicker: "SHARED_A · SERVER", title: "SRV-01", meta: "Hermes / 自动化补池", status: "健康", tone: "shared", scope: "shared" })}
    ${resourceNode({ id: "storage-a", x: 390, y: 287, w: 220, h: 78, kicker: "SHARED_A · STORAGE", title: "STORAGE_A", meta: "3 个项目使用", status: "健康", tone: "shared", scope: "shared" })}
    ${resourceNode({ id: "index-shared", x: 390, y: 391, w: 220, h: 78, kicker: "SHARED_B · INDEX", title: "KNOWLEDGE_INDEX", meta: "2 个项目 · 1 待核验", status: "待核验", tone: "attention", scope: "shared", className: "attention" })}
    ${projectNodes}`;
}

function reverseConfig() {
  if (state.selectedResource === "storage-a") {
    return {
      resource: resources["storage-a"],
      groups: [
        { project: "hermes", x: 55, y: 82, workloads: [{ x: 270, y: 120, name: "消息产物", relation: "写入" }] },
        { project: "automation", x: 55, y: 410, workloads: [{ x: 270, y: 360, name: "执行产物", relation: "写入" }] },
        { project: "knowledge", x: 775, y: 82, workloads: [{ x: 605, y: 120, name: "索引文件", relation: "读取 / 写入" }] },
      ],
    };
  }
  if (state.selectedResource === "index-shared") {
    return {
      resource: resources["index-shared"],
      groups: [
        { project: "hermes", x: 55, y: 205, workloads: [{ x: 270, y: 220, name: "问答检索", relation: "查询" }] },
        { project: "knowledge", x: 775, y: 205, workloads: [{ x: 605, y: 220, name: "索引同步", relation: "维护" }] },
      ],
    };
  }
  return {
    resource: resources["srv-01"],
    groups: [
      { project: "hermes", x: 55, y: 205, workloads: [{ x: 270, y: 120, name: "消息网关", relation: "部署于" }, { x: 270, y: 350, name: "健康检查", relation: "运行于" }] },
      { project: "automation", x: 775, y: 205, workloads: [{ x: 605, y: 220, name: "执行服务", relation: "部署于" }] },
    ],
  };
}

function renderReverseLookup() {
  const config = reverseConfig();
  const center = { x: 410, y: 235 };
  let workloadIndex = 0;
  const branches = config.groups.map((group) => {
    const project = projects[group.project];
    const projectCenterX = group.x + 85;
    const projectCenterY = group.y + 39;
    const dim = state.project !== "all" && state.project !== group.project ? " dimmed" : "";
    const workloads = group.workloads.map((workload) => {
      const index = workloadIndex++;
      const workCenterX = workload.x + 65;
      const workCenterY = workload.y + 32;
      return `${graphEdge(`impact-work-${index}`, `M${center.x + 90} ${center.y + 45} C${(center.x+90+workCenterX)/2} ${center.y+45} ${(center.x+90+workCenterX)/2} ${workCenterY} ${workCenterX} ${workCenterY}`, `usage${dim}`)}
        ${graphEdge(`impact-project-${index}`, `M${workCenterX} ${workCenterY} L${projectCenterX} ${projectCenterY}`, `shared${dim}`)}
        ${edgeLabel((workCenterX+projectCenterX)/2,(workCenterY+projectCenterY)/2,workload.relation)}
        ${resourceNode({ id: `work-${index}`, x: workload.x, y: workload.y, w: 130, h: 64, kicker: "WORKLOAD", title: workload.name, meta: workload.relation, status: "绑定", tone: "current", scope: "private", className: dim ? "inactive" : "" })}`;
    }).join("");
    return `${workloads}${projectNode({ id: group.project, x: group.x, y: group.y, title: project.label, meta: project.subtitle, status: "受影响", className: dim ? "inactive" : "" })}`;
  }).join("");
  return `<circle class="impact-ring outer" cx="500" cy="280" r="220"></circle><circle class="impact-ring" cx="500" cy="280" r="140"></circle>
    <text class="boundary-label" x="500" y="48" text-anchor="middle">RESOURCE IMPACT · 从单一资源反查所有使用关系</text>
    ${resourceNode({ id: state.selectedResource, x: center.x, y: center.y, w: 180, h: 90, kicker: config.resource.type.toUpperCase(), title: config.resource.label, meta: config.resource.meta, status: config.resource.health, tone: config.resource.tone === "green" ? "shared" : "attention", scope: "shared" })}
    ${branches}`;
}

function resourceGraphMarkup() {
  if (state.resourceOption === "shared") return renderSharedCollection();
  if (state.resourceOption === "reverse") return renderReverseLookup();
  return renderProjectResources();
}

function attachResourceInteractions() {
  resourceCanvas.querySelectorAll("[data-resource]").forEach((node) => {
    const select = () => {
      const id = node.dataset.resource;
      if (!resources[id]) return;
      state.selectedResource = id;
      if (resources[id].scope === "共享资源" && state.resourceOption !== "reverse") {
        state.resourceOption = "reverse";
        state.project = "all";
        showToast(`已从 ${resources[id].label} 进入资源反查，显示全部使用项目。`);
      }
      renderResource();
    };
    node.addEventListener("click", select);
    node.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select();
      }
    });
  });
  resourceCanvas.querySelectorAll("[data-project-node]").forEach((node) => {
    const select = () => {
      if (!projects[node.dataset.projectNode]) return;
      state.project = node.dataset.projectNode;
      if (state.resourceOption === "shared") renderResource();
    };
    node.addEventListener("click", select);
  });
}

function renderResourceInspector() {
  const resource = resources[state.selectedResource] || resources["srv-01"];
  const projectNames = resource.projects.map((id) => projects[id]?.label || id).join(" / ");
  resourceInspector.innerHTML = `<div class="resource-identity"><span>SELECTED RESOURCE</span><h3>${resource.label}</h3><p>${resource.relation}</p></div>
    <div class="resource-fact"><span>类型</span><strong>${resource.type}</strong></div>
    <div class="resource-fact"><span>范围</span><strong>${resource.scope}</strong></div>
    <div class="resource-fact"><span>健康</span><strong class="${resource.tone}">${resource.health}</strong></div>
    <div class="resource-fact"><span>使用项目</span><strong>${projectNames}</strong></div>
    <div class="resource-fact"><span>最近核验</span><strong>${resource.freshness}</strong></div>`;
}

function renderResourceMeta() {
  const meta = resourceOptionMeta[state.resourceOption];
  const profile = selectedProject();
  const summary = meta.summary.map((item) => [...item]);
  if (state.resourceOption === "project") {
    summary[0][1] = profile.label;
    summary[1][1] = String(profile.privateResources.length);
    summary[2][1] = String(profile.sharedResources.length);
    summary[3][1] = profile.privateResources.some((id) => resources[id].tone === "amber") ? "1" : "0";
  }
  if (state.resourceOption === "reverse") {
    const resource = resources[state.selectedResource];
    summary[0][1] = resource.label;
    summary[1][1] = String(resource.projects.length);
    summary[2][1] = String(reverseConfig().groups.reduce((count, group) => count + group.workloads.length, 0));
  }
  document.querySelector("#resource-summary").innerHTML = summary.map(([label,value],index) => `<span class="summary-chip ${index === 0 ? "cyan" : index === 3 && value !== "0" ? "amber" : ""}">${label}<strong>${value}</strong></span>`).join("");
  document.querySelector("#resource-mode").innerHTML = `<i></i>${meta.mode}`;
  document.querySelector("#resource-legend").innerHTML = state.resourceOption === "project"
    ? `<span><i class="legend-line private"></i>项目私有</span><span><i class="legend-line shared"></i>共享实体</span><span><i class="legend-line usage"></i>使用关系</span>`
    : `<span><i class="legend-line shared"></i>资源实体</span><span><i class="legend-line usage"></i>项目使用</span><span><i class="legend-line"></i>工作负载</span>`;
  document.querySelector("#resource-hint").textContent = state.resourceOption === "project"
    ? "共享资源位于项目边界外；点击共享资源自动查看全部使用项目。"
    : state.resourceOption === "shared"
      ? "共享集合用于盘点；点击任一共享资源进入影响反查。"
      : "资源位于中心，所有项目和工作负载都通过真实使用关系连接。";
  document.querySelector("#option-rationale").innerHTML = `<div><span>这个方案最突出</span><strong>${meta.rationale}</strong></div><p>${meta.copy}</p><em>${meta.note}</em>`;
  const picker = document.querySelector("#resource-picker");
  picker.classList.toggle("is-visible", state.resourceOption === "reverse");
  picker.innerHTML = ["srv-01","storage-a","index-shared"].map((id) => `<button class="resource-pick ${id === state.selectedResource ? "is-active" : ""}" data-resource-pick="${id}" type="button">${resources[id].label}</button>`).join("");
  picker.querySelectorAll("[data-resource-pick]").forEach((button) => {
    button.addEventListener("click", () => {
      state.selectedResource = button.dataset.resourcePick;
      renderResource();
    });
  });
}

function renderResource() {
  document.querySelectorAll("[data-resource-option]").forEach((button) => {
    const active = button.dataset.resourceOption === state.resourceOption;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  document.querySelectorAll("[data-project]").forEach((button) => {
    const active = button.dataset.project === state.project;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  renderResourceMeta();
  resourceCanvas.innerHTML = resourceGraphMarkup();
  attachResourceInteractions();
  renderResourceInspector();
  updateHash();
}

function showPage(page) {
  state.page = page;
  document.querySelectorAll("[data-page]").forEach((button) => {
    const active = button.dataset.page === page;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  document.querySelectorAll("[data-page-panel]").forEach((panel) => panel.classList.toggle("is-active", panel.dataset.pagePanel === page));
  updateHash();
}

function updateHash() {
  history.replaceState(null, "", `#page=${state.page}&lens=${state.workflowLens}&resource=${state.resourceOption}&project=${state.project}&resourceId=${state.selectedResource}`);
}

function showToast(message) {
  toast.textContent = message;
  toast.classList.add("is-visible");
  window.clearTimeout(window.prototypeToastTimer);
  window.prototypeToastTimer = window.setTimeout(() => toast.classList.remove("is-visible"), 2600);
}

document.querySelectorAll("[data-page]").forEach((button) => button.addEventListener("click", () => showPage(button.dataset.page)));
document.querySelectorAll("[data-workflow-lens]").forEach((button) => button.addEventListener("click", () => {
  state.workflowLens = button.dataset.workflowLens;
  state.workflowDetailOpen = false;
  renderWorkflow();
  updateHash();
}));
document.querySelectorAll("[data-resource-option]").forEach((button) => button.addEventListener("click", () => {
  state.resourceOption = button.dataset.resourceOption;
  if (state.resourceOption === "project" && state.project === "all") state.project = "hermes";
  if (state.resourceOption !== "project") state.project = "all";
  renderResource();
}));
document.querySelectorAll("[data-project]").forEach((button) => button.addEventListener("click", () => {
  const project = button.dataset.project;
  if (state.resourceOption === "project" && project === "all") {
    state.resourceOption = "shared";
    state.project = "all";
  } else {
    state.project = project;
  }
  renderResource();
}));
document.querySelector("#mark-button").addEventListener("click", () => {
  const choice = state.page === "workflow"
    ? `流程页：${state.workflowLens === "definition" ? "流程框架" : "运行实例"}`
    : `资源页：${{project:"A 按项目",shared:"B 共享集合",reverse:"C 资源反查"}[state.resourceOption]}`;
  showToast(`已标记 ${choice}；你直接告诉我需要保留或组合哪些部分。`);
});

renderWorkflow();
renderResource();
showPage(state.page);
