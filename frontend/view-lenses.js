const optionMeta = {
  a: {
    name: "A · 稳定骨架",
    focus: "空间连续性",
    rationale: "总览、运行、控制共用项目位置，用户不需要每切一页就重新认图。",
    tradeoff: "项目内部细节需要点击后展开",
  },
  b: {
    name: "B · 项目聚焦",
    focus: "单项目因果链",
    rationale: "把选中项目移到中心并展开内部路径，特别适合追查一次任务或配置一个项目。",
    tradeoff: "跨项目态势退到画布边缘",
  },
  c: {
    name: "C · 环形态势",
    focus: "统筹全局感",
    rationale: "用同心范围表达时间或权限边界，最快看出活动密度和访问范围。",
    tradeoff: "精确关系不如矩形网络直接",
  },
};

const viewMeta = {
  run: {
    kicker: "RUN / LIVE LENS",
    title: "所有项目都在被看护，活动只叠加在真实路径上",
    description: "区分“持续监控”和“此刻有执行”：安静的项目仍在运行，只有真实事件获得方向、路径和时间。",
    summary: [
      ["监控覆盖", "4 / 4"],
      ["活动执行", "3"],
      ["需关注", "1", "attention"],
    ],
    mode: "实时态势",
    legend: [
      ["持续监控", "normal"],
      ["真实活动", "active"],
      ["异常路径", "alert"],
    ],
    hint: "点击项目查看图形回应；运行页保持只读。",
  },
  control: {
    kicker: "CONTROL / BOUNDARY LENS",
    title: "只突出可改变的边界，以及实际与期望之间的差异",
    description: "控制不是第二张运行图：它显示访问范围、能力闸门、变更草稿和外部回执，未验证前不把期望画成事实。",
    summary: [
      ["实际一致", "11 / 12"],
      ["待确认草稿", "2"],
      ["边界漂移", "1", "attention"],
    ],
    mode: "边界与差异",
    legend: [
      ["实际关系", "actual"],
      ["期望关系", "ghost"],
      ["明确禁用", "denied"],
    ],
    hint: "选择边界后才出现控制；所有改动先进入草稿。",
  },
};

const projectDetails = {
  Hermes: {
    run: ["2 个执行", "最近 12 秒", "消息同步路径清晰"],
    control: ["4 项能力", "1 个共享组", "无待发布外部变更"],
  },
  自动化补池: {
    run: ["1 个执行", "最近 41 秒", "持续监控，当前无异常"],
    control: ["3 项能力", "1 个共享组", "暂停请求等待回执"],
  },
  知识整理: {
    run: ["监控中", "最近 2 分钟", "连接器核验失败"],
    control: ["2 项能力", "知识共享", "新增观察关系待确认"],
  },
  实验项目: {
    run: ["监控中", "最近 6 分钟", "低活动但在线"],
    control: ["2 项能力", "项目私有", "共享访问已禁用"],
  },
};

const operations = [
  { time: "10:42", action: "刷新项目状态", object: "读取健康与新鲜度", project: "Hermes", result: "已验证", state: "verified", icon: "↻" },
  { time: "10:39", action: "更新项目投影", object: "调整知识入口的画布位置", project: "知识整理", result: "仅本地", state: "local", icon: "◇" },
  { time: "10:36", action: "生成共享访问调整草稿", object: "运维共享 → Hermes", project: "Hermes", result: "待确认", state: "draft", icon: "±" },
  { time: "10:31", action: "发起暂停请求", object: "执行 run-028", project: "自动化补池", result: "待回执", state: "pending", icon: "Ⅱ" },
  { time: "10:28", action: "核验知识连接器", object: "Obsidian 索引入口", project: "知识整理", result: "失败", state: "failed", icon: "!" },
  { time: "10:22", action: "重排全局画布", object: "保存项目节点位置", project: "全局", result: "仅本地", state: "local", icon: "⌖" },
  { time: "10:14", action: "恢复共享观察", object: "运维共享 → 实验项目", project: "实验项目", result: "已验证", state: "verified", icon: "✓" },
];

const state = {
  option: location.hash.match(/option=([abc])/)?.[1] || "a",
  view: location.hash.match(/view=(run|control)/)?.[1] || "run",
  project: "Hermes",
  operationProject: "all",
  focusOpen: false,
};

const canvas = document.querySelector("#visual-canvas");
const focusCard = document.querySelector("#focus-card");
const toast = document.querySelector("#toast");

function edge(id, d, tone = "muted") {
  return `<path id="${id}" class="svg-edge ${tone}" d="${d}"></path>`;
}

function signal(pathId, tone = "") {
  return `<circle r="4" class="moving-signal ${tone}" aria-hidden="true"><animateMotion dur="3.1s" repeatCount="indefinite"><mpath href="#${pathId}"></mpath></animateMotion></circle>`;
}

function node({ x, y, w = 170, h = 76, project, kicker = "PROJECT", title, meta, status = "监控中", tone = "", className = "" }) {
  const selected = project === state.project ? "is-selected" : "";
  return `<g class="svg-node ${className} ${selected}" data-project="${project}" role="button" tabindex="0" aria-label="查看 ${project}">
    <rect class="node-box" x="${x}" y="${y}" width="${w}" height="${h}" rx="8"></rect>
    <text class="node-kicker" x="${x + 14}" y="${y + 18}">${kicker}</text>
    <text class="node-title" x="${x + 14}" y="${y + 40}">${title}</text>
    <text class="node-meta" x="${x + 14}" y="${y + 58}">${meta}</text>
    <text class="node-status ${tone}" x="${x + w - 13}" y="${y + 18}" text-anchor="end">${status}</text>
  </g>`;
}

function coordinatorNode(label, meta, status) {
  return `<g class="svg-node coordinator" role="img" aria-label="${label}">
    <rect class="node-box" x="390" y="224" width="220" height="104" rx="10"></rect>
    <text class="node-kicker" x="409" y="247">BUILT-IN COORDINATOR</text>
    <text class="node-title" x="409" y="274">${label}</text>
    <line class="node-divider" x1="409" x2="591" y1="287" y2="287"></line>
    <text class="node-meta" x="409" y="306">${meta}</text>
    <text class="node-status green" x="591" y="247" text-anchor="end">${status}</text>
  </g>`;
}

function renderA(view) {
  const projects = [
    { x: 70, y: 86, project: "Hermes", title: "Hermes", meta: view === "run" ? "2 个执行 · 12 秒前" : "实际一致 · 4 项能力", status: view === "run" ? "有活动" : "已核验", tone: "green" },
    { x: 760, y: 86, project: "自动化补池", title: "自动化补池", meta: view === "run" ? "1 个执行 · 41 秒前" : "暂停请求 · 等待回执", status: view === "run" ? "有活动" : "待回执", tone: view === "run" ? "green" : "amber", className: view === "control" ? "attention" : "" },
    { x: 760, y: 410, project: "知识整理", title: "知识整理", meta: view === "run" ? "连接器核验失败" : "新增观察关系 · 草稿", status: view === "run" ? "需关注" : "待确认", tone: "amber", className: "attention" },
    { x: 70, y: 410, project: "实验项目", title: "实验项目", meta: view === "run" ? "低活动 · 仍在监控" : "运维共享 · 已禁用", status: view === "run" ? "监控中" : "已禁用", tone: view === "run" ? "green" : "red", className: view === "control" ? "denied" : "" },
  ];

  if (view === "run") {
    const edges = [
      edge("a-hermes", "M240 124 C330 124 330 250 390 268", "active"),
      edge("a-auto", "M760 124 C670 124 670 250 610 268", "active"),
      edge("a-knowledge", "M760 448 C670 448 670 312 610 302", "alert"),
      edge("a-lab", "M240 448 C330 448 330 312 390 302", "muted"),
    ].join("");
    return `${edges}${signal("a-hermes")}${signal("a-auto")}${signal("a-knowledge", "amber")}${coordinatorNode("业务统筹 Agent", "汇总所有项目，不替代项目执行", "4 / 4 监控中")}${projects.map(node).join("")}`;
  }

  return `<rect class="boundary actual-boundary" x="348" y="181" width="304" height="190" rx="24"></rect>
    <text class="scope-label" x="365" y="203">已发布的本地边界</text>
    ${edge("a-control-hermes", "M240 124 C330 124 330 250 390 268", "actual")}
    ${edge("a-control-auto", "M760 124 C670 124 670 250 610 268", "actual")}
    ${edge("a-control-knowledge", "M760 448 C684 448 685 326 610 304", "ghost")}
    ${edge("a-control-lab", "M240 448 C330 448 330 312 390 302", "denied")}
    <g transform="translate(682 351)"><rect class="gate-body allow" width="83" height="25" rx="5"></rect><text class="gate-label" x="41" y="16" text-anchor="middle">观察 · 草稿</text></g>
    <g transform="translate(224 354)"><rect class="gate-body deny" width="83" height="25" rx="5"></rect><text class="gate-label" x="41" y="16" text-anchor="middle">共享 · 禁用</text></g>
    ${coordinatorNode("边界协调器", "2 个草稿 · 1 个待回执", "实际 / 期望")}${projects.map(node).join("")}`;
}

function renderB(view) {
  const perimeter = [
    { x: 70, y: 78, project: "自动化补池", title: "自动化补池", meta: "持续监控", w: 145, h: 64, className: "mini", status: "在线", tone: "green" },
    { x: 785, y: 78, project: "知识整理", title: "知识整理", meta: "1 项需关注", w: 145, h: 64, className: "mini attention", status: "注意", tone: "amber" },
    { x: 785, y: 424, project: "实验项目", title: "实验项目", meta: "低活动", w: 145, h: 64, className: "mini", status: "在线", tone: "green" },
  ];

  if (view === "run") {
    return `<text class="scope-label" x="58" y="42">所有项目持续监控 · 当前聚焦 Hermes</text>
      <rect class="boundary" x="235" y="67" width="530" height="420" rx="40"></rect>
      <text class="scope-label" x="257" y="93">HERMES · PROJECT BOUNDARY</text>
      ${edge("b-run-1", "M319 260 C370 260 382 184 430 184", "active")}
      ${edge("b-run-2", "M500 212 L500 275", "active")}
      ${edge("b-run-3", "M570 303 C625 303 620 382 672 382", "active")}
      ${signal("b-run-1")}${signal("b-run-2")}${signal("b-run-3")}
      ${node({ x: 245, y: 222, w: 146, h: 76, project: "Hermes", kicker: "ENTRY", title: "消息入口", meta: "收到新消息", status: "12 秒", tone: "green" })}
      ${node({ x: 430, y: 136, w: 140, h: 76, project: "Hermes", kicker: "AGENT", title: "Hermes", meta: "识别意图", status: "完成", tone: "green" })}
      ${node({ x: 430, y: 275, w: 140, h: 76, project: "Hermes", kicker: "WORKFLOW", title: "路由工作流", meta: "调用 2 个工具", status: "执行中", tone: "green" })}
      ${node({ x: 610, y: 344, w: 146, h: 76, project: "Hermes", kicker: "SERVICE", title: "消息网关", meta: "等待回执", status: "活动", tone: "green" })}
      ${perimeter.map(node).join("")}`;
  }

  return `<text class="scope-label" x="58" y="42">选择一个项目，只显示它能碰到的边界</text>
    <rect class="boundary actual-boundary" x="224" y="58" width="550" height="438" rx="42"></rect>
    <rect class="boundary expected-boundary" x="258" y="91" width="482" height="370" rx="35"></rect>
    <text class="scope-label" x="247" y="82">HERMES · 实际访问边界</text>
    <text class="scope-label amber" x="281" y="114">待发布期望边界</text>
    ${edge("b-control-1", "M500 276 C406 276 394 180 320 180", "actual")}
    ${edge("b-control-2", "M500 276 C594 276 606 180 680 180", "actual")}
    ${edge("b-control-3", "M500 276 C406 276 394 384 320 384", "ghost")}
    ${edge("b-control-4", "M500 276 C594 276 606 384 680 384", "denied")}
    ${node({ x: 416, y: 236, w: 168, h: 80, project: "Hermes", kicker: "PROJECT", title: "Hermes", meta: "边界一致率 3 / 4", status: "1 草稿", tone: "amber", className: "attention" })}
    ${node({ x: 245, y: 142, w: 150, h: 72, project: "Hermes", kicker: "PRIVATE", title: "项目 Agent", meta: "观察 / 查询", status: "允许", tone: "green" })}
    ${node({ x: 605, y: 142, w: 150, h: 72, project: "Hermes", kicker: "SHARED_A", title: "运维共享", meta: "3 项资源", status: "允许", tone: "green" })}
    ${node({ x: 245, y: 348, w: 150, h: 72, project: "Hermes", kicker: "SHARED_B", title: "知识共享", meta: "新增观察", status: "草稿", tone: "amber", className: "attention" })}
    ${node({ x: 605, y: 348, w: 150, h: 72, project: "Hermes", kicker: "CAPABILITY", title: "外部执行", meta: "未声明能力", status: "禁用", tone: "red", className: "denied" })}
    ${perimeter.map(node).join("")}`;
}

function polar(cx, cy, radius, degrees) {
  const angle = ((degrees - 90) * Math.PI) / 180;
  return { x: cx + radius * Math.cos(angle), y: cy + radius * Math.sin(angle) };
}

function ringProject(project, angle, status, tone, meta) {
  const p = polar(500, 276, 212, angle);
  const selected = project === state.project ? "is-selected" : "";
  return `<g class="svg-node ${selected}" data-project="${project}" role="button" tabindex="0" transform="translate(${p.x - 76} ${p.y - 32})">
    <rect class="node-box" width="152" height="64" rx="32"></rect>
    <circle class="status-dot-svg ${tone}" cx="18" cy="20" r="4"></circle>
    <text class="node-title" x="30" y="24">${project}</text>
    <text class="node-meta" x="18" y="45">${meta}</text>
    <text class="node-status ${tone === "attention" ? "amber" : "green"}" x="135" y="45" text-anchor="end">${status}</text>
  </g>`;
}

function renderC(view) {
  const projectRings = [
    ringProject("Hermes", 315, view === "run" ? "2 执行" : "一致", "active", view === "run" ? "12 秒前" : "4 项能力"),
    ringProject("自动化补池", 45, view === "run" ? "1 执行" : "待回执", "active", view === "run" ? "41 秒前" : "1 项请求"),
    ringProject("知识整理", 135, view === "run" ? "需关注" : "1 草稿", "attention", view === "run" ? "核验失败" : "期望关系"),
    ringProject("实验项目", 225, view === "run" ? "监控中" : "已禁用", "", view === "run" ? "低活动" : "项目拒绝"),
  ].join("");

  if (view === "run") {
    return `<circle class="orbit" cx="500" cy="276" r="212"></circle>
      <circle class="orbit" cx="500" cy="276" r="142"></circle>
      <circle class="orbit active" cx="500" cy="276" r="175"></circle>
      <circle class="orbit warning" cx="500" cy="276" r="175" transform="rotate(112 500 276)"></circle>
      <text class="ring-label" x="500" y="65" text-anchor="middle">全部项目 · 持续监控环</text>
      <text class="ring-label" x="500" y="119" text-anchor="middle">最近 15 分钟活动</text>
      <g transform="translate(422 225)">
        <rect class="node-box" width="156" height="102" rx="51" fill="#102632" stroke="#5b9a9b"></rect>
        <text class="node-kicker" x="78" y="30" text-anchor="middle">NOW</text>
        <text class="node-title" x="78" y="56" text-anchor="middle">3 个活动执行</text>
        <text class="node-meta" x="78" y="76" text-anchor="middle">4 / 4 项目监控中</text>
      </g>${projectRings}`;
  }

  return `<circle class="boundary actual-boundary" cx="500" cy="276" r="220"></circle>
    <circle class="boundary expected-boundary" cx="500" cy="276" r="170"></circle>
    <circle class="orbit" cx="500" cy="276" r="112"></circle>
    <text class="ring-label" x="500" y="51" text-anchor="middle">项目访问边界</text>
    <text class="scope-label amber" x="500" y="103" text-anchor="middle">期望状态 · 2 个草稿</text>
    <text class="scope-label" x="500" y="169" text-anchor="middle">已发布实际状态</text>
    ${edge("c-control-a", "M500 276 L350 126", "actual")}
    ${edge("c-control-b", "M500 276 L650 126", "actual")}
    ${edge("c-control-c", "M500 276 L650 426", "ghost")}
    ${edge("c-control-d", "M500 276 L350 426", "denied")}
    <g transform="translate(426 233)"><rect class="gate-body" width="148" height="86" rx="43"></rect><text class="node-kicker" x="74" y="28" text-anchor="middle">POLICY CORE</text><text class="node-title" x="74" y="51" text-anchor="middle">边界协调器</text><text class="node-meta" x="74" y="69" text-anchor="middle">11 / 12 一致</text></g>
    ${projectRings}`;
}

function renderCanvas() {
  const renderers = { a: renderA, b: renderB, c: renderC };
  canvas.innerHTML = renderers[state.option](state.view);
  canvas.querySelectorAll("[data-project]").forEach((element) => {
    const choose = () => selectProject(element.dataset.project);
    element.addEventListener("click", choose);
    element.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        choose();
      }
    });
  });
}

function renderFocus() {
  const detail = projectDetails[state.project]?.[state.view] || projectDetails.Hermes[state.view];
  const isControl = state.view === "control";
  focusCard.className = `focus-card ${isControl ? "control-copy" : ""} ${state.focusOpen ? "is-open" : ""}`;
  focusCard.innerHTML = `<div class="focus-kicker">SELECTED PROJECT</div>
    <h3>${state.project}</h3>
    <p>${isControl ? "只展示这个项目实际拥有、期望拥有和明确禁用的边界。" : "持续监控不等于持续有事件；活动数量只表示当前时间窗。"}</p>
    <div class="focus-facts">
      <div class="focus-fact"><span>${isControl ? "能力 / 范围" : "运行状态"}</span><strong>${detail[0]}</strong></div>
      <div class="focus-fact"><span>${isControl ? "共享 / 来源" : "最新信号"}</span><strong>${detail[1]}</strong></div>
    </div>
    <button class="focus-action" type="button"><span>${isControl ? "查看边界差异" : "查看项目路径"}</span><span>→</span></button>`;
}

function selectProject(project) {
  state.project = project;
  state.focusOpen = true;
  renderCanvas();
  renderFocus();
}

function renderMeta() {
  const option = optionMeta[state.option];
  const view = viewMeta[state.view];
  document.querySelector("#option-name").textContent = option.name;
  document.querySelector("#option-focus").textContent = option.focus;
  document.querySelector("#option-rationale").textContent = option.rationale;
  document.querySelector("#option-tradeoff").textContent = option.tradeoff;
  document.querySelector("#choice-label").textContent = `标记方案 ${state.option.toUpperCase()}`;
  document.querySelector("#view-kicker").textContent = view.kicker;
  document.querySelector("#view-title").textContent = view.title;
  document.querySelector("#view-description").textContent = view.description;
  document.querySelector("#canvas-hint").textContent = view.hint;

  const modeBadge = document.querySelector("#mode-badge");
  modeBadge.className = `mode-badge ${state.view === "control" ? "control" : ""}`;
  modeBadge.innerHTML = `<i></i>${view.mode}`;
  document.querySelector(".canvas-shell").classList.toggle("control-mode", state.view === "control");

  document.querySelector("#signal-summary").innerHTML = view.summary
    .map(([label, value, tone = ""]) => `<div class="summary-item ${tone}"><span>${label}</span><strong>${value}</strong></div>`)
    .join("");
  document.querySelector("#toolbar-legend").innerHTML = view.legend
    .map(([label, tone]) => `<span><i class="legend-key ${tone}"></i>${label}</span>`)
    .join("");

  document.querySelectorAll(".option-tab").forEach((button) => {
    const active = button.dataset.option === state.option;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-selected", String(active));
  });
  document.querySelectorAll(".view-tab").forEach((button) => {
    const active = button.dataset.view === state.view;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-pressed", String(active));
  });
}

function renderOperations() {
  const filtered = state.operationProject === "all"
    ? operations
    : operations.filter((item) => item.project === state.operationProject);
  const visible = filtered.slice(0, 3);
  document.querySelector("#filter-count").textContent = `${filtered.length} 条记录 · 首屏显示 ${visible.length} 条`;
  document.querySelector("#operation-list").innerHTML = visible.length
    ? visible.map((item) => `<button class="operation-row" data-operation-project="${item.project}" type="button">
        <time class="operation-time">${item.time}</time>
        <span class="operation-action"><i class="operation-icon">${item.icon}</i><span><strong>${item.action}</strong><small>${item.object}</small></span></span>
        <span class="operation-project">${item.project}</span>
        <span class="operation-result ${item.state}">${item.result}</span>
        <span class="operation-arrow">›</span>
      </button>`).join("")
    : `<div class="empty-state">这个项目还没有业务统筹操作记录</div>`;

  document.querySelectorAll(".operation-row").forEach((row) => {
    row.addEventListener("click", () => {
      if (row.dataset.operationProject !== "全局") selectProject(row.dataset.operationProject);
      window.scrollTo({ top: document.querySelector(".visual-section").offsetTop - 80, behavior: "smooth" });
    });
  });
}

function updateHash() {
  history.replaceState(null, "", `#option=${state.option}&view=${state.view}`);
}

function renderAll() {
  renderMeta();
  renderCanvas();
  renderFocus();
  updateHash();
}

document.querySelectorAll(".option-tab").forEach((button) => {
  button.addEventListener("click", () => {
    state.option = button.dataset.option;
    renderAll();
  });
});

document.querySelectorAll(".view-tab").forEach((button) => {
  button.addEventListener("click", () => {
    state.view = button.dataset.view;
    renderAll();
  });
});

document.querySelectorAll(".filter-chip").forEach((button) => {
  button.addEventListener("click", () => {
    state.operationProject = button.dataset.project;
    document.querySelectorAll(".filter-chip").forEach((item) => {
      const active = item === button;
      item.classList.toggle("is-active", active);
      item.setAttribute("aria-pressed", String(active));
    });
    renderOperations();
  });
});

document.querySelector("#choice-button").addEventListener("click", () => {
  const message = `已在本页标记方案 ${state.option.toUpperCase()}；你直接告诉我字母和修改意见即可。`;
  toast.textContent = message;
  toast.classList.add("is-visible");
  window.clearTimeout(window.choiceToastTimer);
  window.choiceToastTimer = window.setTimeout(() => toast.classList.remove("is-visible"), 2600);
});

document.querySelector(".all-actions-button").addEventListener("click", () => {
  state.operationProject = "all";
  document.querySelector('[data-project="all"]').click();
  toast.textContent = "完整操作页将在方向确认后实现；当前对比稿保留最近三条。";
  toast.classList.add("is-visible");
  window.clearTimeout(window.choiceToastTimer);
  window.choiceToastTimer = window.setTimeout(() => toast.classList.remove("is-visible"), 2600);
});

renderAll();
renderOperations();
