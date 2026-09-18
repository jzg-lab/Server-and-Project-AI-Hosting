# HOST / SSH 初始化与 Agent 配置规范

- **状态**：可视化优先的 MVP-1 接入规范已确认（SSH 账号密码默认、SSH Key 兼容、Linux 与首次指纹确认方式均已锁定）
- **版本**：1.5
- **日期**：2026-08-12
- **适用范围**：Linux 服务器远程部署、单一所有者、SSH 只读发现、Docker/Compose 及后续多 Provider、项目文档和 Agent 配置会话
- **关联文档**：
  - [产品需求文档](./01-需求文档.md)
  - [最小领域模型与权限语义](./05-最小领域模型与权限语义.md)
  - [前后端 API 与数据架构草案](./08-前后端API与数据架构草案.md)
  - [Rust 后端架构与前后端契约决策](./09-Rust后端架构与前后端契约决策.md)

> 文档角色：本文是当前产品 MVP-1 的 SSH/发现规范性核心文档。它先保证真实事实能生成可编辑图草稿，再规定可选 Agent 辅助；不承担长期责任托管定义。

> 本文把四层分开：扫描器产生证据；确定性映射器产生基础图草稿；Agent 产生可选建议；用户确认后才发布本地投影。

## 1. MVP-1 目标

用户通过互联网登录部署在 APP_HOST（Linux）上的应用，登记一个 TARGET_HOST（Linux）。SSH 是 MVP-1 的必经接入路径：系统执行固定只读发现，读取 Docker/Compose 与有限范围的项目文档，先用确定性规则形成可编辑项目网络图；Agent 可在其上生成归类建议和问题。用户确认或修改后发布本地投影。模型未配置或失败时，基础图与人工管理仍然可用。

## 1.1 已锁定的核心边界

| 项目 | MVP-1 决策 | 直接影响 |
|---|---|---|
| 远程接入 | 只采用 SSH | API 必须先完成连接检查，再创建发现任务 |
| 目标平台 | 只采用 Linux | 发现命令、权限检查和验收样本按 Linux 编写 |
| 接入方向 | APP_HOST 发起到 TARGET_HOST | SSH 密码或私钥只由服务端凭据存储管理，浏览器不直接连目标机 |
| 失败门槛 | SSH/指纹/Linux 基线失败停止该轮；单个 Provider 失败保留其他结果并标记覆盖范围 | 前端展示可修复原因和重试入口，不生成“已发现”状态 |

MVP-1 不执行：

- 任意 SSH 命令；
- Docker、服务器或项目配置修改；
- 容器、镜像、网络或卷删除；
- 暂停、取消、改向和其他外部控制；
- 全盘扫描和完整日志搬运。

## 2. 角色边界

```text
SSH 发现器：读取事实，不解释业务归属
文档提取器：读取白名单文档，做大小限制和脱敏
确定性映射器：把明确身份和关系转换成基础图草稿，不猜测不确定归属
辅助 Agent：归纳证据、提出建议、生成问题和补丁
用户：回答问题、确认/编辑/拒绝补丁
投影服务：保存版本、应用确认后的本地变更
```

Agent 不直接写入正式项目、关系或权限表。所有 Agent 输出都先进入 `proposal` 或 `projection_draft`。

确定性映射器与 Agent 的依赖方向固定为：

```text
Evidence → Deterministic Draft → Manual Review/Confirmation
                              ↘ Agent Suggestion（可选增强）
```

不得实现为 `Evidence → Agent → Draft` 的强依赖链。

## 3. 初始化状态机

```text
host_registered
  → fingerprint_fetching
  → host_key_unverified
  → host_key_verified
  → connection_checking
  → connection_ready
  → discovery_running
  → provider_results_ready
  ├─ evidence_ready（旧兼容） | discovery_complete | discovery_partial
  │   → draft_ready
  │   → needs_user_review
  │   → confirmed | edited | rejected | archived
  │   → projection_published
  └─ discovery_unavailable（无草稿、无 Agent 会话）
```

Agent 增强是从 `draft_ready` 开始的并行子状态：

```text
draft_ready
  → proposal_generating
  → proposal_ready | needs_user_input | model_unavailable | proposal_invalid
```

`model_unavailable` 和 `proposal_invalid` 不改变 `draft_ready`，用户仍可人工编辑和发布。

失败状态至少包括：

```text
ssh_unreachable
ssh_auth_failed
permission_denied
docker_permission_denied
docker_unavailable
compose_unavailable
discovery_timeout
document_read_failed
model_unavailable
evidence_conflict
```

失败状态需要保存原因、来源、时间和下一步建议；失败不产生“健康”或“已配置”状态。

## 4. SSH 连接契约

### 4.1 HOST 登记字段

```text
host_id
display_name
address
port
ssh_user
credential_ref
host_key_fingerprint
host_key_state = unverified | verified | changed
transport = ssh
os = linux
status
created_at
last_checked_at
```

MVP-1 默认使用 SSH 账号密码，也支持用户明确提供的 SSH Key，凭据只保存 `credential_ref` 和类型；密码、私钥和口令不进入 SQLite、OpenAPI 响应、SSE、日志或 Agent 上下文。密码请求的幂等记录只保存 Argon2id 验证值。账号密码登录先尝试标准 SSH Password Authentication，若服务端提示 `keyboard-interactive`，自动处理单个不回显密码提示；多因素/MFA、多轮问题和 SSH Agent 后置。系统不读取用户电脑的 `~/.ssh`，也不枚举 APP_HOST 上的私钥集合；选择 SSH Key 时只使用用户明确粘贴/提交的那一把。主机指纹必须处于 `verified` 才能开始发现；指纹变化进入 `host_key_changed`，暂停后续发现并要求用户复核。

HOST 查询、更新和连接测试响应只返回 `credential_kind`，不返回绑定的 `credential_ref`。连接测试响应同时返回脱敏的 `port`、`ssh_user` 与 `auth_transport`（`russh_client` / `openssh_askpass` / `openssh_key`），用于确认前端提交、凭据类型和实际 SSH 执行分支。旧版幂等响应没有这些字段时，服务端从当前 HOST 元数据补齐；历史上未记录的 `auth_transport` 保持未知，不能据此猜测执行分支。

**账号密码优先原则**：登录服务器是后续管理和可视化的基础。首次接入默认走账号密码；如果私钥认证失败且用户有账号密码，界面必须引导用户改用账号密码先登录，不得让“找不到旧私钥”阻断接入。已存在于目标机 `authorized_keys` 的旧公钥不能与新生成私钥配对；新私钥只能通过后续显式安装新公钥来启用。

**用户可见的两步连接语义**：

1. **检查指纹**只读取 SSH 主机公钥的 SHA256 指纹，用来确认“连接的是哪台服务器”；这一步不验证 SSH 用户名、密码或私钥。
2. 用户确认指纹后，服务端把候选主机密钥写入该 HOST 专属的受控 `known_hosts`，再执行 SSH 认证和 Linux 身份检查。Docker/Compose 只在后续发现任务中检查。

因此，`SSH_AUTH_FAILED` 表示网络和主机身份阶段通常已经通过，但目标 SSH 服务拒绝了登录凭据。界面应优先提示核对 SSH 用户/密码，并提醒 `root` 可能被禁止密码登录、服务器可能只接受私钥，或需要多因素/MFA/多轮交互。不得把该错误解释成“指纹错误”。

如果失败发生在 SSH Key 模式，界面优先解释为“这把私钥未通过”，并提供“改用账号密码登录”。系统不得自动遍历本机或 APP_HOST 私钥，因为无法从服务端公钥反推出私钥位置，且盲试多把私钥会触发目标 SSH 的认证次数限制。

第三方客户端可连接时，也不能直接推断“账号密码可登录”。验收时必须核对第三方客户端实际认证方式：账号密码、Keyboard-Interactive、用户密钥、Agent、跳板或保存会话组合。若客户端会话显示密码为空但用户密钥存在，应按 SSH Key 路径导入/粘贴对应私钥；这不是账号密码链路失败。

连接检查的成功定义是“SSH 已认证且 Linux 身份命令成功执行”。Docker/Compose 只属于后续发现任务，不参与服务器连接成功/失败或画布连接统计；若 `docker version` 或 `docker compose ls` 返回 Docker socket `permission denied`，发现结果使用 `DOCKER_PERMISSION_DENIED`；若命令不存在或服务不可用，使用 `DOCKER_UNAVAILABLE` 或 `COMPOSE_UNAVAILABLE`。这些结果不得归类为 `SSH_AUTH_FAILED`，也不得把已接入 HOST 降级为“SSH 连接失败”；界面始终保留服务器操作入口。

### 4.2 首版 SSH 实现默认值

```text
APP_HOST Rust 服务
  → 系统 OpenSSH（一次性进程）
  → TARGET_HOST 固定发现器
  → 版本化 JSON 证据包
```

### 4.2.1 运行平台验收（Windows 开发机 / Linux APP_HOST）

当前开发机为 Windows，但产品实际部署在 Linux APP_HOST。平台边界必须单独验收：

1. Windows 验收覆盖浏览器、Windows OpenSSH、Windows `network-atlas.exe` askpass 分支和真实 HOST 连接；它不能替代 Linux 发布验收。
2. Linux 发布验收覆盖 Linux Rust release 二进制、Linux `ssh` / `ssh-keyscan`、Linux 文件权限、容器内 `current_exe` askpass 路径和 `/healthz`。部署镜像必须带 `openssh-client`。
3. 两个平台都必须验证“主机指纹已确认 → 密码认证 → `uname`/Linux 检查”，并将 Docker/Compose 能力分类作为独立发现任务验收。Linux 端至少保留一条脱敏的 `connection_ready` 结果。
4. 验收记录分为 `windows-dev` 与 `linux-release` 两段，记录命令、脱敏输入、字面输出和退出状态；禁止记录密码、私钥或完整环境变量。

实现上，密码模式将当前应用二进制设置为 `SSH_ASKPASS`，并设置 `SSH_ASKPASS_REQUIRE=force`、`DISPLAY` 和一次性密码环境变量。该设计在 Windows 与 Linux 使用同一 Rust 分支；不得恢复依赖 Windows `.cmd` 或 Linux shell 临时脚本的双实现。

- 不在 TARGET_HOST 安装常驻 Agent，不写入脚本、配置或 Docker 对象。
- 连接使用严格 `known_hosts`；首次连接由服务端取得主机指纹，界面展示指纹与 HOST 信息，用户确认后写入受控 `known_hosts`；禁止关闭主机密钥校验。
- 启用 Docker/Compose Provider 时，目标 SSH 用户需已有 Docker 只读访问（docker group 或 rootless Docker）；不自动提权。Docker 权限不足只影响该 Provider，不影响 `connection_ready` 或其他发现 Provider。
- 单次连接、每条发现命令和总输出都设超时/大小上限；记录退出码和脱敏错误。
- 地址、端口、用户、根目录等只能作为结构化字段传入，服务端适配器生成命令。

### 4.3 只读原则

发现器只能调用固定的、版本化的读取动作，例如：

```text
docker version
docker info
docker ps
docker inspect
docker compose ls
docker compose ps
```

实际命令由服务端适配器固定生成，不接受来自浏览器或模型的任意命令字符串。

### 4.4 发现器输出

发现器返回版本化 JSON，不把 CLI 文本直接交给前端：

```json
{
  "protocol_version": "1",
  "discovery_id": "DISCOVERY_RUN",
  "host": {"address": "HOST", "os": "linux"},
  "connection_state": "connection_ready",
  "discovery_state": "discovery_complete",
  "providers": [],
  "deployment_candidates": [],
  "compose_projects": [],
  "containers": [],
  "images": [],
  "networks": [],
  "volumes": [],
  "document_candidates": [],
  "health_checks": [],
  "warnings": [],
  "started_at": "2026-08-10T00:00:00Z",
  "finished_at": "2026-08-10T00:00:10Z"
}
```

每项记录至少带：

```text
external_id
kind
source
observed_at
freshness
sha256（适用时）
redaction_state
metadata
```

下一阶段每个 Provider 还必须返回 status、observed_count、evidence_refs、warnings 和 observed_at。Provider 失败不覆盖已成功的连接结果；DeploymentCandidate 使用 host_id、provider_kind 和 external_id 形成稳定身份，ProjectTarget 在用户确认后建立。

## 5. Docker 与项目文档发现

### 5.1 Docker 事实

MVP-1 读取：

- Compose 项目名和工作目录；
- 容器名称、ID、镜像、状态和创建时间；
- 网络、卷和端口映射；
- Docker 标签；
- 健康检查状态；
- 资源之间的可观察引用。

`docker inspect` 只允许读取字段白名单：ID、名称、镜像、创建时间、状态/健康状态、端口、网络名、卷目标、Compose 标签；默认丢弃 `Config.Env`、完整启动命令、认证配置、日志和未声明标签。来源路径和标签进入 Agent 前再次执行秘密模式脱敏。

不把容器名称直接当成业务项目名称。

### 5.2 文档白名单

默认从 Compose 工作目录或用户确认的项目根目录读取：

```text
README.md / README.*
PROJECT.md / PROJECT.*
AGENTS.md
docs/**/*.md
docker-compose.yml
compose.yml
```

Compose 文件首版只做结构化解析（项目名、服务名、镜像、网络、卷、端口和标签）；不把整份 YAML、内嵌环境变量、认证配置或 `secrets` 正文交给 Agent。

默认跳过：

```text
.env
*.key
*.pem
*secret*
日志正文
node_modules
.git
target
```

每个文件设置数量、单文件大小和总大小上限。超过上限时保存路径、哈希和“未完整读取”状态，把选择权交给用户。

### 5.3 确定性图映射与 Agent 归类优先级

```text
Compose project 名称
→ Docker 标签
→ Compose 文件路径
→ 文档标题和明确项目标识
→ 无法确定则保持 unassigned
```

确定性映射器至少生成：

```text
HOST contains COMPOSE_PROJECT
COMPOSE_PROJECT deploys SERVICE/CONTAINER
SERVICE/CONTAINER connects_to NETWORK
SERVICE/CONTAINER mounts VOLUME
DOCUMENT documents COMPOSE_PROJECT（仅存在明确路径/标识时）
```

每条节点和边必须带 `source_refs[]` 与 `observed_at`。需要语义判断、存在冲突或只有模糊名称时，不继续猜测，交给 Agent 建议或用户手动处理。

Agent 归类优先级为：

```text
确定性图草稿
→ 文档中的明确说明
→ Agent 语义归纳
→ 用户采用、拒绝或修改
```

Agent 必须在建议中引用证据，并且不得覆盖确定性草稿，例如：

```json
{
  "proposal": "将 CONTAINER_A 和 CONTAINER_B 归入 PROJECT_A",
  "confidence": "medium",
  "evidence_refs": ["DOC_1", "LABEL_2", "COMPOSE_3"],
  "reason": "文档标题与 Compose project 名称一致",
  "requires_user_confirmation": true
}
```

### 5.4 下一阶段非 Docker Provider

下一阶段按以下顺序扩展，只读执行固定命令：

- systemd：unit 名、ActiveState、SubState、ExecStart 摘要、WorkingDirectory 和端口引用；
- 用户确认根目录：清单文件、文档标题、版本元数据和受限配置摘要；
- PM2：应用名、状态、解释器、工作目录和端口；
- 端口/进程：监听地址、PID、父进程、工作目录和可脱敏命令摘要。

每个结果先进入 DeploymentCandidate，不直接创建项目。用户确认 TechnicalProject 后，再物化 ProjectTarget；代码仓库只作为版本或文档证据。

## 6. Agent 辅助配置会话

### 6.1 输入

辅助 Agent 只接收：

- 结构化 Docker 事实；
- DeploymentCandidate、Deployment、TechnicalProject 和 ProjectTarget 的引用与状态；
- HOST Provider 能力、覆盖范围和证据新鲜度；
- 文档路径、哈希和脱敏摘要；
- 已确认的用户规则；
- 当前投影和历史修改摘要。

### 6.2 输出

Agent 输出必须是结构化对象：

```text
facts_used[]
proposals[]
questions[]
projection_patch
warnings[]
```

每个 `question` 至少包含：

```text
question_id
type = choice | text | confirm
prompt
options（可选）
evidence_refs[]
blocking = true | false
```

### 6.3 必须优先展示的事项

规则层先标记以下事项，Agent 再负责解释；规则层结果无需等待模型即可显示：

- SSH 或 Docker 不可用；
- 资源未归类；
- 文档和 Docker 标签矛盾；
- 一个外部对象被多个 DeploymentCandidate 同时使用；
- 文档读取权限不足；
- 疑似凭据字段出现；
- 之前被忽略的对象再次出现；
- 用户确认过的投影发生漂移。

### 6.4 用户可执行的修改

```text
确认建议
拒绝建议
修改项目名称
移动资源
合并项目
拆分项目
补充文档路径
忽略对象
归档投影
重新提问
重新扫描
```

用户修改形成新的 `projection_draft`，支持差异、确认、撤销和审计；不会删除外部 Docker 对象。

## 7. OpenAI 兼容模型配置

第一版只要求：

```text
base_url
api_key
model
```

调用协议固定为 OpenAI Chat Completions 兼容的非流式请求；首版不调用模型列表接口，不保存完整提示词和原始响应，只保存结构化结果摘要、状态和错误。

服务端存储形式：

```text
base_url       → SQLite 配置
model          → SQLite 配置
api_key        → SecretProvider，数据库只保存 secret_ref
```

如果现有 Agent 提供 OpenAI 兼容接口，可以直接作为 `ModelProvider`。如果它提供的是独立业务 API，则在后续作为单独的 AgentAdapter 接入，不和模型服务配置混为一类。

MVP-1 默认不保存完整模型上下文；只保存请求状态、摘要、错误和用户确认结果。

## 8. 最小 API 顺序

### 8.1 核心图路径（先实现）

```text
POST /api/v1/secret-refs（SSH 账号密码或 SSH Key）
POST /api/v1/hosts
POST /api/v1/hosts/{host_id}/connection-tests（取得候选指纹）
POST /api/v1/hosts/{host_id}/host-key-confirmations（用户确认）
POST /api/v1/hosts/{host_id}/connection-tests（验证 SSH 认证与 Linux 身份）
POST /api/v1/hosts/{host_id}/discovery-runs
GET  /api/v1/discovery-runs/{run_id}
GET  /api/v1/discovery-runs/{run_id}/evidence
GET  /api/v1/projection-drafts/{draft_id}
PATCH /api/v1/projection-drafts/{draft_id}
POST /api/v1/projection-drafts/{draft_id}/confirm
POST /api/v1/ignore-rules
PATCH /api/v1/layouts/{layout_id}
GET  /api/v1/views/global/world
GET  /api/v1/projects/{project_id}/views/resources
```

### 8.2 Agent 与通知增强（核心图通过后实现）

```text
POST /api/v1/secret-refs（模型 Key）
PUT  /api/v1/model-provider
POST /api/v1/model-provider/test
GET  /api/v1/discovery-runs/{run_id}/proposal
POST /api/v1/onboarding-sessions
POST /api/v1/onboarding-sessions/{session_id}/messages
GET  /api/v1/events/stream
```

所有异步发现任务都返回 `request_id`、`run_id` 和当前状态；达到 `evidence_ready`（旧兼容）、`discovery_complete` 或 `discovery_partial` 时同时返回确定性生成的 `draft_id`。`discovery_unavailable` 不返回 `draft_id`，也不进入 Agent 辅助。Provider 的 `observed_count = 0` 仍可是成功空结果，不能单凭数量判定不可用。前端可以轮询任务快照；接入 SSE 后，由事件提示重新读取，不依赖事件保存最终状态。

## 9. MVP-1 数据保留默认

- 当前事实、已确认投影、用户回答/确认、忽略规则和审计摘要：保留到用户手动删除。
- 每个 TARGET_HOST：保留最近 3 次完整脱敏扫描；更早扫描保留摘要、哈希、时间和差异。
- 原始 SSH 输出、完整文档正文和完整模型提示词/响应：只在临时目录保留，最长 24 小时，默认不进长期库。
- SSH 密码、SSH 私钥和模型 Key：只在服务端 `FileSecretStore` 的受限权限文件中保留，SQLite 只保存引用描述及密码请求的 Argon2id 幂等验证值，删除引用时同步清理。
- 提供工作区导出和“一键删除”入口；按类型/天数配置留到后续版本。

## 10. MVP-1 验收

- 用 SSH 连接一个 Linux HOST；
- 发现至少一个 Compose 项目和一个容器；
- 读取至少一份白名单项目文档；
- 在未配置模型时生成一份带来源的图草稿；
- 通过真实 Rust API 在全局业务网和项目资源图显示扫描事实；
- 用户能人工更名、移动、合并/拆分、修改关系、忽略并确认投影；
- 生成一条带证据引用的 Agent 建议；
- 生成一个需要用户确认的问题；
- 用户可以编辑建议并发布本地投影；
- 模型调用失败时，已有图草稿与人工确认仍可用；
- 归档/忽略对象在下一次扫描中保持隐藏；
- 前端通过真实 Rust API 和 SSE 显示以上状态；
- 模型服务只配置 URL、Key、Model；
- 整个流程不修改目标 HOST。
- 验收结论限定为 SSH 事实、真实图、人工投影管理和 Agent 辅助；未被该 Adapter 覆盖的流程、业务承诺和责任状态不进入当前判断。

### 10.1 无扫描证据与名称分层验收

- 登记 HOST 后，即使 SSH 失败，界面仍显示服务器别名、真实 IP、端口、状态和错误码；项目、服务与容器保持为空。
- 服务器别名修改后刷新保持且不重置连接；地址、端口、SSH 用户或登录凭据修改后刷新保持，并回到待连接状态重新核对指纹。
- 每台 HOST 的详情提供立即重连、编辑连接、更换密码或 Key、失败原因和重新扫描入口；不同错误类型突出对应的修复动作。
- 每台 HOST 的详情还必须提供直接可发现的“删除服务器”入口。删除只删除 APP_HOST 本地登记、扫描证据、投影和凭据引用，执行前生成受限备份；不向 TARGET_HOST 发起删除、重启或配置修改。
- 旧 SSH 密码或 Key 不在页面或响应中回显；更换时只提交新的 SecretRef。
- Compose 项目、服务和容器节点同时保留“扫描原始名称（只读）”与“显示名称（可修改）”。
- “绑定项目”只更新本地 `project_id`；不得覆盖 Compose 项目名、服务名或容器名。
- HTTP API unavailable 时显示错误与上次成功状态，不生成 Fixture 项目。

### 10.2 业务统筹 Agent 可视化验收

- 未保存模型配置时，全局画布不显示业务统筹 Agent 节点；右侧保留配置入口。
- 保存 URL、Key、Model 后，全局画布显示唯一的“业务统筹 Agent”节点；刷新页面后仍存在。
- 节点显示模型、OpenAI 兼容 URL、Key“已保存”和最近测试状态，但响应及页面均不出现 Key 正文、`credential_ref` 或 Secret 路径。
- Agent 节点不计入 Docker/Compose 扫描证据。HOST 尚未扫描成功时，项目数保持 0，并继续显示无扫描证据提示。
- 业务统筹 Agent 直接统筹业务任务；项目是任务作用域，HOST 是任务可能依赖的资源。目标关系为 `Agent → Task → Project`，MVP-1 不绘制 `Agent → HOST`，也不以 `Agent → Project` 代替缺失的任务实体。
- 全局业务网隐藏 HOST，把服务器登记、连接、Key、重连和扫描操作放到 `global/hosts`；`global/resource` 只承担共享资源集合与影响反查。业务任务尚未形成时，业务网显示“尚无业务任务”，不得把连接检查或扫描运行显示成业务任务。
- 当前后端尚未实现业务 `Task` 持久化与图节点，本阶段保持真实空状态，不造假任务数据。

## 11. 后续扩展

- 密码认证升级为密钥认证的后续流程：

  ```text
  SSH 账号密码认证
  → 用户确认主机指纹
  → APP 本地生成独立 Ed25519 密钥对
  → 追加带唯一标识的公钥
  → 使用私钥重新验证
  → 成功后删除密码 SecretRef
  → 失败时只撤销本次新增公钥
  ```

  该流程必须由用户点击“生成托管密钥，以后免输密码”之类的明确操作触发；它会写入目标用户的 `~/.ssh/authorized_keys`，所以需要单独的公钥安装、权限、回执、重试和回滚设计，本轮不自动执行。成功前继续保留账号密码兜底；多因素/MFA、多轮 Keyboard-Interactive 和 SSH Agent 也作为后续兼容项。

- SSH Agent 和多种凭据方式；
- 第二个远程 HOST transport；
- 周期性无人值守扫描和完整历史回放；
- 外部 Agent 适配器；
- 项目 Agent、责任契约和项目脉搏；
- 控制动作、回执和补偿；
- 可配置数据保留和备份恢复。
