# 项目 Agent 只读边界规格

- **状态**：H6 已交付
- **版本**：0.1
- **日期**：2026-08-15
- **范围**：TechnicalProject 的逻辑 Project Agent 绑定、ProjectTarget 作用域、固定 typed read-only tools 和调用审计
- **前置文档**：`13-运维项目发现与服务器页面规格.md`、`15-业务创建与共享资源关系规格.md`、`11-总管与项目Agent协作模型.md`

## 1. 目标与边界

H6 把“一个项目由哪个逻辑 Agent 负责”从长期愿景变成可验证的后端边界，但不把旧的 M3 onboarding Agent 静默升级成项目责任 Agent。绑定本身不发起 SSH、不调用模型、不执行外部写操作；它只建立稳定身份和可审计的结构化观察入口。

本阶段明确不包含：任意 Shell、Docker/Compose 写入、重启/部署、秘密读取、CodeWorkspace、跨项目私有数据、Business Agent、责任契约、通知告警和受控行动。

## 2. 持久化对象

### 2.1 `project_agents`

一个活动 `TechnicalProject` 最多绑定一个逻辑 Project Agent。记录包含：

- `project_agent_id`：稳定 UUID；
- `technical_project_id`、`workspace_id`：不可越界的作用域；
- `state`：`active` 或 `disabled`；
- `capabilities`：数据库约束固定为 `["read_only"]`；
- `tool_names`：数据库约束固定为五个工具；
- 修订、创建者和更新时间。

绑定接口是幂等的：`POST /api/v1/technical-projects/{technical_project_id}/agent` 必须携带 `Idempotency-Key`。重复相同请求重放已保存响应；不同项目或不同载荷不会覆盖已有 Agent。

### 2.2 `project_agent_tool_calls`

每次成功的 typed read 调用保存 Agent、TechnicalProject、可选 ProjectTarget、工具名、受限请求、结构化结果、Evidence 引用和观测时间。失败的越界/非法工具调用不写入成功调用审计，避免把拒绝伪装成事实。

删除 TechnicalProject 时，Agent、工具调用和 ProjectTarget 通过外键级联清理；其他项目的 Agent 和审计不受影响。

## 3. API

| 方法 | 路径 | 作用 |
|---|---|---|
| `POST` | `/api/v1/technical-projects/{technical_project_id}/agent` | 绑定一个只读逻辑 Agent |
| `GET` | `/api/v1/technical-projects/{technical_project_id}/agent` | 查看绑定摘要和固定能力 |
| `POST` | `/api/v1/project-agents/{project_agent_id}/tools/{tool_name}` | 调用一个固定 typed read-only tool |

工具请求只接受 `project_target_id` 和有界 `limit` 字段；服务器不接受命令字符串、provider 名称、路径或脚本。所有目标级工具缺少 `project_target_id` 时返回 `PROJECT_TARGET_REQUIRED`。

## 4. 固定工具目录

### `list_project_targets`

项目作用域例外，不需要目标 ID，也不允许附带目标 ID。返回当前 TechnicalProject 及其全部 ProjectTarget。它不会返回其他项目的目标。

### `read_deployment_observation`

必须指向当前项目的 ProjectTarget。返回绑定 Deployment 的有限条 immutable `DeploymentObservation`，包括 observation state、provider status、observed time、metadata 和 Evidence 引用。

### `read_host_capabilities`

通过当前 ProjectTarget → Deployment → HOST 的明确路径读取 HOST 状态、Linux/SSH 基线、最近 Discovery provider coverage、最近 DiscoveryRun 和 HOST 监控新鲜度。它不返回凭据本体，也不把 HOST 提升为项目节点。

### `read_service_status`

读取当前目标 Deployment 的最新观察，分别表达 observed/missing/unknown、provider status、freshness、metadata 和 Evidence。没有观察时保持 `unavailable`，不推断“正常”。

### `read_recent_diff`

只读取当前目标所在 HOST 的最近 Discovery diff，并保留 diff item 的 Evidence/path 引用。没有 diff 时返回空结果和 `unavailable` 新鲜度。

## 5. 隔离与权限不变量

有效能力是固定交集：

```text
workspace 硬边界
∩ active Project Agent
∩ 当前 TechnicalProject
∩ 当前 ProjectTarget
∩ ProjectTarget.capabilities = read_only
```

1. 目标查询同时约束 `project_agent_id` 对应的 `technical_project_id`、`workspace_id` 和 `project_target_id`；把另一个项目的目标 ID 传入时返回统一 not-found，不泄露对方存在性。
2. `project_target_id` 之外不接受 HOST、Deployment 或外部身份作为越界查询入口；Agent 不能绕过 ProjectTarget 直接枚举 HOST。
3. 工具名是闭集；`execute_shell`、任意命令、写入、重启、部署等输入统一返回 `PROJECT_AGENT_TOOL_NOT_ALLOWED`。
4. 归档/不可读 ProjectTarget 不再提供观察；已归档项目不能创建新绑定或调用工具。
5. 结果携带 `observed_at`、`freshness` 和 `evidence_refs`；未知、缺失和过期不会被渲染成绿色健康结论。

## 6. 与其他 Agent 的边界

- M3 onboarding Agent 继续负责 Discovery 草稿建议、问题和人工确认；它不获得 `project_agents` 表中的责任身份。
- 业务统筹 Agent 仍是未来的跨业务汇总入口；H6 不给 Project Agent 跨项目读取或委托能力。
- 专用开发 Agent 管理代码；运维 Project Agent 只读部署和运行证据。

## 7. 验收证据

- `backend/tests/h6_project_agent.rs`：绑定幂等、五个工具、Evidence 引用、跨项目拒绝、缺少目标拒绝、任意工具拒绝、删除级联。
- `backend/migrations/0016_project_agent.sql`：追加迁移，保留既有表/数据，并为新增 SSE 事件扩展 `change_events` 的闭集约束。
- OpenAPI：包含三个 Project Agent 路由、请求/响应和 tagged tool result schema；生成类型与 OpenAPI 同步。

下一阶段是在不扩大本边界的前提下，为 Target 增加持续观察/健康时间带；责任契约、通知告警和受控行动必须另行建模和验收。
