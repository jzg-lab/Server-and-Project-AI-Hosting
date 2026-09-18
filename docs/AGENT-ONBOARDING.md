# Network Atlas Agent 入场手册

> 目标：让一个完全不了解本项目的新 Agent，在不依赖聊天上下文的情况下，能够理解产品边界、定位代码、运行验证，并安全地修改一个具体细节。
>
> 最后核对：2026-09-18。每次接手任务都必须重新检查 Git 状态、本文档和当前实现。

## 1. 先记住一句话

Network Atlas 是一个**把 Linux HOST 上的可观察事实转换为可追溯、可编辑、可确认的项目网络图**的可视化管理工具。

当前系统的写入主要发生在 APP_HOST 本地数据库和本地投影中；MVP-1 不修改 TARGET_HOST，不执行 Docker、Compose、服务启停或其他服务器控制动作。

## 2. 入场顺序

按以下顺序阅读，不要一开始就通读全部愿景文档：

1. 根目录 [AGENTS.md](../AGENTS.md)：仓库级规则、Obsidian 优先级、并行 Agent 和 Git 约束。
2. [文档索引](./00-文档索引.md)：找到产品规格和决策记录。
3. 本文档：知道如何继续工作。
4. [当前状态与进度](./PROJECT-STATUS.md)：知道现在已经完成什么、下一步是什么。
5. [README](../README.md)：知道本地启动、环境变量和阶段验证入口。
6. [画布与系统设计](./02-画布编辑与系统设计文档.md)：理解前端行为和事实/投影分层。
7. [前后端 API 与数据架构](./08-前后端API与数据架构草案.md)：理解 API、SQLite、SSE 和来源语义。
8. 与当前任务直接相关的规格：`10`（HOST/SSH）、`13`（发现/服务器页）、`14`（监控）、`15`（业务/资源）、`16`（项目 Agent）。
9. [部署手册](../deploy/README.md)：只有涉及正式部署、Compose、Caddy 或备份时才进入。

修改前先查询 Obsidian 中的 `Network Atlas` 记录。若 Obsidian 未运行或 CLI 不可用，必须在交接说明中注明本次未完成核对；形成长期可复用结论后再登记。

## 3. 事实、投影、建议必须分开

| 层 | 含义 | 能否由用户修改 |
|---|---|---|
| 事实证据 | 从 TARGET_HOST 只读发现的 HOST、Docker/Compose、systemd 等观察结果 | 不能直接修改 |
| 确定性草稿 | 根据事实生成的图节点、边和候选归属 | 可以编辑草稿 |
| 用户决定 | 更名、归类、关系、布局、忽略、归档、确认 | 可以修改，并应留审计依据 |
| Agent 建议 | 带证据引用的建议、问题和可编辑补丁 | 不能自动发布 |

任何修改都必须先回答：这是在改事实、草稿、确认投影，还是建议？如果回答不清楚，不要直接改代码。

## 4. 项目地图

```text
frontend/
  index.html              同源页面入口
  app.js                  画布、导航、交互和读模型组合
  data-source.js          MockDataSource / HttpDataSource
  styles.css              主视觉基线
  view-lenses.*           全局资源视图
  workflow-resource-options.html
  workflow-resource-options.js
  workflow-resource-options.css
                           保留的流程/资源视觉原型
  generated/api.d.ts      从 OpenAPI 生成，不手工维护

backend/
  src/api.rs              Axum 路由、应用状态、OpenAPI 注册
  src/contracts.rs        API/领域契约
  src/storage.rs          SQLite 连接、迁移、持久化基础设施
  src/ssh.rs               SSH 连接与只读命令边界
  src/discovery.rs         发现 Provider 与证据采集
  src/projection.rs        事实到图草稿/投影的确定性映射
  src/catalog.rs           Deployment、TechnicalProject、ProjectTarget
  src/monitoring*.rs       资源观测、调度、历史、rollup、健康策略
  src/project_agent.rs     TechnicalProject 范围内的 typed read-only Agent
  src/auth.rs              单一所有者认证、Cookie、Origin/CSRF
  src/secrets.rs           SecretRef 与受限文件秘密存储
  migrations/              0001 起的 SQLite 增量迁移
  tests/                   按 M0-M4、H4-H6 和监控能力分组的行为测试

openapi/openapi.json       API 权威导出物
scripts/                   M0-M4、真实 HOST 和部署验证脚本
deploy/                    Linux APP_HOST + Docker Compose + Caddy 正式入口
artifacts/                 验收记录、截图、补丁和回滚材料
work/                      进度账本与工作材料
docs/                      产品共识、规格、决策和交接文档
```

## 5. 关键术语与主语

- `APP_HOST`：运行 Network Atlas、Rust/Axum、SQLite、秘密存储和 Caddy 的主机。
- `TARGET_HOST`：应用通过 SSH 访问的目标 Linux 主机。
- `HOST connection_ready`：端口可达、SSH 认证成功、能够读取 Linux 身份。
- `Provider discovery`：连接成功后的附加发现；Docker 不可用不能把 SSH 连接降级为失败。
- `Deployment`：用户确认后的稳定部署实例。
- `TechnicalProject`：一个项目 Agent 的运维责任范围。
- `ProjectTarget`：TechnicalProject 指向具体 HOST/Deployment 的显式绑定。
- `Business`：用户定义的业务语义边界，可跨多个 TechnicalProject。
- `SecretRef`：秘密正文的引用；数据库、API、日志和文档不得出现密码、私钥或 Token。

## 6. 常用开发命令

在仓库根目录执行：

```powershell
npm ci
npm run generate:types
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

前端静态检查：

```powershell
npm run check:frontend
npm run test:frontend
```

新 Agent 默认必须使用隔离端口和隔离数据库，不要直接打开已有的 `data/network-atlas.db`：

```powershell
$env:NETWORK_ATLAS_BIND = "127.0.0.1:18787"
$env:NETWORK_ATLAS_DATABASE_URL = "sqlite://data/dev-$PID.db?mode=rwc"
$env:NETWORK_ATLAS_DATA_DIR = "data/dev-$PID"
cargo run -p network-atlas
```

打开 `http://127.0.0.1:18787/`。开发进程启动时会执行迁移、恢复中断任务并启动 scheduler/compactor，因此隔离数据库不是可选优化。只有明确要检查已有本地数据时，才使用默认 `data/network-atlas.db`。

阶段验证优先使用 README 中的统一脚本：`verify-m0.ps1` 到 `verify-m4.ps1`。`verify-real-host-acceptance.ps1` 是对正在运行实例执行的 live HTTP 投影回归门槛：它要求 `bootstrap.meta.data_source.kind=real`，校验预先准备的主机投影和 fixture 隔离，并同时运行 Rust、前端和 diff 检查；它不是浏览器 UI 验收，也不是可脱离前置数据运行的固定历史数据集。运行前必须阅读脚本参数和前置数据。真实 HOST 产品验收必须通过 UI 完成，不能用直连 SSH 或后端日志替代。

## 7. 修改前后检查清单

修改前：

- 执行 `git status --short --branch`，保留他人的未提交改动。
- 阅读本任务对应规格和最近提交。
- 确认是否涉及 API JSON、OpenAPI、SQLite 迁移、SecretRef、URL/Hash 状态或 UI 行为。
- 若是部署文档，确认 APP_HOST/TARGET_HOST 没有混用。

修改后：

- 代码改动补行为测试；契约变化重新导出 OpenAPI 和前端类型。
- 运行与改动风险匹配的最小验证，必要时运行统一阶段脚本。
- 先对本次改动文件执行 `git diff --check -- <changed-files>`；工作树本来就不干净时，再单独审计全局 `git diff --check` 的既有失败，不要把无关历史材料的告警归到本次改动。
- 更新 [当前状态与进度](./PROJECT-STATUS.md) 或对应规格，不把已完成事实写入愿景文档。
- 检查秘密没有进入日志、测试产物、文档、Git 或回复。
- 最后再次执行 `git status`，清楚区分本次改动与既有未跟踪材料。

## 8. 不可越过的边界

不要：

- 把 Fixture 当成真实数据展示。
- 用模型或规则猜测用户没有确认的项目归属。
- 让 Agent 直接执行任意 Shell、写操作或跨项目读取。
- 修改 TARGET_HOST 的 SSH 配置、Docker、Compose、服务或项目文件。
- 修改 `deploy/.env`、秘密目录、数据库或备份来“方便测试”。
- 删除、清理或覆盖用户已有数据、验收工件和其他 Agent 的改动。
- 为未来愿景预建微服务、插件平台、图数据库、通用工作流或复杂权限系统。

## 9. 任务完成标准

一个可交接的改动必须同时具备：实现位置明确、边界没有扩大、测试或验证可复现、文档入口已更新、Git 状态可解释。若远程仓库存在，完成验证后按项目约定推送；当前仓库已知没有配置 remote，不能声称已上传 GitHub。
