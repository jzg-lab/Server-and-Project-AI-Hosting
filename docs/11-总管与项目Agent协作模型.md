# 业务统筹 Agent 与项目 Agent 协作模型

- **文档状态**：长期责任/协作愿景与语义参考；H6 的最小只读绑定和 typed adapter 已落地，本文其余责任契约、委托、知识治理和行动语义仍不参与当前验收
- **版本**：0.4
- **日期**：2026-08-12
- **适用范围**：单一所有者/超级个体，以及未来的小型工作室
- **关联文档**：`00-文档索引.md`、`01-需求文档.md`、`03-目标文档.md`、`04-MVP决策记录.md`、`09-Rust后端架构与前后端契约决策.md`、`10-HOST-SSH初始化与Agent配置规范.md`

> 本文记录“业务统筹 Agent 面向用户、项目 Agent 面向项目”的长期假设。阅读前提是当前 MVP-1 已经证明真实数据可视化管理有价值。本文中的角色、消息、知识、脉搏和责任对象均不得反向成为 MVP-1 的前置依赖。

## 0. 文档边界与当前 MVP-1 的关系

| 层级 | 定义 | 本文是否约束 |
|---|---|---|
| 当前产品 MVP-1 | SSH/Linux 事实进入现有画布，用户可人工整理、确认、保存和重扫比较；Agent 仅做可选辅助 | 否，以 `docs/01/03/04/08/09/10` 为准 |
| Post-MVP 单项目监督 | 项目目标、责任定义、范围化观察、Pulse/异常/提案和用户确认 | 是，作为后续候选 |
| 长期产品愿景 | 两层 Agent、知识、委托、核验、受控行动和跨项目协调 | 是，作为长期语义参考 |

当前产品仍以可视化管理为主语，SSH 是首个真实数据入口。H6 已建立 `ProjectAgent` 的逻辑绑定、固定 `read_only` capability、五个 typed tool 和调用审计；尚未建立 `ResponsibilityContract`、完整 `ProjectPulse`、知识授权、委托消息或受控行动基础设施。当前 UI 中已有业务统筹 Agent 元素，也不等于这些长期语义已实现。

当前能力映射：

```text
已锁定：SSH 账号密码（默认）或 SSH Key / Linux TARGET_HOST / 首次指纹界面确认 / 固定只读发现
当前 MVP-1：Observation → Deterministic Visual Draft → Manual Management → Optional Agent Suggestion → User Confirmation
Post-MVP 监督：ResponsibilityContract → Scoped Observation → ProjectPulse/Exception → User Confirmation
长期行动：Execute → Receipt → Re-observe → Verify → Cross-project Coordination
```

## 1. Post-MVP 产品假设

如果 MVP-1 证明用户需要持续监督而不只是可视化整理，产品可以演进为**分层协作的 Agent 网络**：

```text
Workspace / Principal（用户）
│
├── ExecutiveStewardAgent（业务统筹 Agent）
│   ├── UserPrivateMemory（用户私有记忆）
│   ├── GlobalPolicy / AuthorityPolicy（全局策略与授权）
│   └── SharedKnowledgeSpace（共同知识库）
│
└── Projects[]
    ├── ProjectCharter（项目目标、边界和成功条件）
    ├── ProjectStewardAgent（项目 Agent）
    ├── ProjectPrivateKnowledgeSpace（项目私有知识库）
    ├── Commitments / Plans / Exceptions
    ├── Capabilities / Resources
    └── ActionRuns / Receipts / Verifications
```

核心表达：

> **业务统筹 Agent 懂用户，项目 Agent 懂项目；业务统筹 Agent 管理用户注意力和跨项目关系，项目 Agent 管理项目的日常状态、行动和异常。**

`Agent` 是逻辑身份、责任、记忆和权限边界，不等于必须运行一个独立模型进程。进入该阶段后可以共用同一模型服务和运行时，通过 `agent_id`、`project_scope`、`memory_scope`、`tool_policy` 和 `trace_id` 隔离。

### 1.1 未来“托管”的最小含义

“把项目交给 AI 托管”不是让 AI 等用户发消息，也不是多加一个聊天入口，而是把一段**持续责任**交给项目 Agent。最小闭环由五类对象组成：

| 对象 | 回答的问题 |
|---|---|
| `ProjectCharter` | 项目为什么存在、边界是什么、怎样算成功 |
| `ResponsibilityContract` | 项目 Agent 对什么结果负责、多久检查一次、能做什么、何时升级 |
| `Commitment` | 当前承诺在何时交付什么结果 |
| `Evidence` | 项目 Agent 的判断依据是什么、何时采集、是否仍然新鲜 |
| `Exception` | 哪个条件偏离、影响什么、谁需要决定、下一次检查是什么时候 |

完整责任托管最终执行 `Observe → Interpret → Plan → Act/Request → Verify → Report/Escalate`。Post-MVP 的第一个监督阶段可采用只读闭环 `Observe → Interpret → Propose/Request → Report/Escalate`；它可以发现异常和提出下一步，但外部执行仍后置。没有 `ResponsibilityContract` 时，项目 Agent 只处于问答/提案模式；只有动作回执、重新观察和验证一致后，才形成 `VerifiedOutcome`。

未来第一版 `ResponsibilityContract` 只需覆盖托管闭环所需的最小字段：

```text
responsibility_id / owner_agent_id / project_id
goal_refs[] / success_conditions[]
observation_plan / check_interval / stale_after
allowed_capabilities[] / approval_policy
escalation_policy / handback_policy
version / expires_at / revoked_at?
```

### 1.2 运维项目目标边界

项目 Agent 面向一个 TechnicalProject，读取该项目的全部 ProjectTarget、Deployment 观察、相关 HOST 能力和共享资源引用。它不直接把 HOST 当作业务项目，也不通过项目名称猜测服务目标；观察调用必须经声明的 typed adapter，并落到具体 ProjectTarget。

CodeWorkspace 不属于本协作模型的核心对象。代码开发、分支、测试和提交由专用开发 Agent 管理；项目 Agent 只消费部署版本、运行状态和变更差异等运维证据。

## 2. 角色边界

| 角色 | 主要对象 | 允许做的事 | 默认不做的事 |
|---|---|---|---|
| 用户/Principal | 自己、全部项目 | 设定目标、价值取舍、授权/撤权、处理高影响例外 | 不必逐项盯日常状态 |
| ExecutiveStewardAgent | 用户与项目组合 | 自然对话、跨项目摘要、优先级、冲突仲裁、路由、升级、管理全局记忆与策略 | 默认不绕过项目 Agent 直接操作项目内部工具 |
| ProjectStewardAgent | 单个 TechnicalProject | 理解项目边界、ProjectTarget、Deployment 观察、文档和运行手册；持续观察；维护下一步；在授权内调用 typed adapter；核验并上报 | 默认不读取其他项目私有知识，不扩大自己的权限 |
| Workflow/Tool | 单个能力 | 执行确定性检查或动作，返回回执 | 不承担项目理解和最终责任 |
| Shared Knowledge 审核（未来首版） | 共同知识 | 项目 Agent 提案；业务统筹 Agent 检查来源与适用范围；Principal 发布或拒绝 | 不把一个项目的临时推断直接推广为全局事实 |

项目 Agent 是该项目的第一责任 Agent；它可以调用短生命周期的专业 Agent，但最终责任、回执和异常仍归项目 Agent。

项目 Agent 的有效权限必须取以下交集，而不是继承业务统筹 Agent 的全部能力：

```text
全局授权
∩ ResponsibilityContract
∩ 项目策略
∩ ProjectTarget.capabilities
∩ 当前已核验 Capability
∩ 本次审批结果
```

项目 Agent 不得自行扩大权限、修改自己的责任契约、绕过审批或把更高权限继续转授给专业 Agent。业务统筹 Agent 可以暂停或撤回委托，但默认不代替项目 Agent 执行项目日常动作。

未来首个治理版本不设独立的“共同知识管理员”Agent。项目 Agent 只能提交 `KnowledgeChangeProposal`，业务统筹 Agent 负责整理来源、冲突和适用范围，Principal 负责发布或拒绝；以后只对低风险、规则明确的类别开放自动发布。

## 3. 对话与委托路由

### 3.1 默认路由

```text
用户自由聊天/个人问题
    → ExecutiveStewardAgent

用户询问单个项目
    → ExecutiveStewardAgent
    → ProjectStewardAgent(project_id)
    → 业务统筹 Agent 按用户语境综合回复

用户询问跨项目问题
    → 业务统筹 Agent 拆分多个 project-scoped 查询
    → 汇总、比较、识别冲突

ProjectStewardAgent 发现异常
    → ProjectPulse / Exception
    → ExecutiveStewardAgent
    → 过滤、排序并决定是否打断用户
```

全局入口始终由业务统筹 Agent 对用户负责：涉及单个项目时，业务统筹 Agent 查询对应项目 Agent，再按用户语境综合回复。进入项目页面后，输入框默认由该项目 Agent 直接回复；项目 Agent 同时把需要跨项目协调、授权变化或用户决定的治理事件同步给业务统筹 Agent，业务统筹 Agent 不再重复回复。界面必须显示 `speaking_agent`、项目作用域、引用的知识范围和最近核验时间。

### 3.2 结构化委托信封

Agent 之间不以无约束的自然语言互相转发任务，统一使用可追踪的 `DelegationEnvelope`：

```text
message_id
schema_version
trace_id / causation_id
from_agent_id / to_agent_id
project_id?
responsibility_id?
message_kind = command | event | query | response
intent
expected_outcome
operation_class = observe | query | draft | execute
reversibility = reversible | compensatable | irreversible | unknown
approval_policy = preauthorized | required | forbidden
deadline?
expires_at?
constraints[]
authority_ref
requested_knowledge_scopes[]
knowledge_grant_refs[]
verification_contract
context_refs[]
idempotency_key
```

对照 Kubernetes/Argo CD 的 `spec/status` 语义：委托信封承载不可变的 `DelegationSpec` 引用，项目 Agent 的 `ProjectPulse` 承载可观察的 `ProjectReport` 摘要；两者都必须带版本或 revision，不能把“期望”与“实际”写进同一个自由文本状态。

项目 Agent 的结构化上报类型至少包括：

```text
ProjectPulse
ProgressEvent
DecisionRequest
ProjectException
ActionReceipt
VerifiedOutcome
KnowledgeChangeProposal
```

跨项目协作默认经业务统筹 Agent 或受控事件通道完成；项目 Agent 之间不自由群聊，避免循环委托和责任不清。

消息采用“至少一次投递 + 幂等处理”的现实语义。服务端 Dispatcher 校验真实的 Agent 身份、项目作用域、有效 Grant 和 Authority，不信任消息载荷自报权限；`requested_knowledge_scopes` 只是请求，最终可见范围由 Dispatcher 取权限交集后计算。`acknowledged`、`executed`、`receipt_received` 和 `verified` 必须是不同状态。重复、过期、取消、处理器崩溃和结果未知都要留下可恢复记录。

项目内部异常使用 `ProjectException`，由对应项目 Agent 持有关闭责任；跨项目资源冲突或组合级风险使用 `PortfolioException`，由业务统筹 Agent 持有。两者都必须带 `owner_agent_id`、`dedupe_key`、`status`、`evidence_refs` 和 `resolution_ref`。业务统筹 Agent 可以聚合或升级项目异常，但不能复制出第二个无主的同类异常。

## 4. 项目脉搏与状态

`ProjectPulse` 是项目 Agent 面向业务统筹 Agent 发布的、带证据截止点和新鲜度的项目级派生摘要；它不是原始事实、动作命令或 Agent 进程心跳。每个项目 Agent 周期性或事件触发地产生 Pulse，业务统筹 Agent 只需先接收摘要、异常和已核验结果：

```text
project_id
project_agent_id
pulse_revision
reported_at
evidence_cutoff
freshness
observation_scope[]
coverage {
  observed_dimensions[]
  unknown_dimensions[]
  unavailable_dimensions[]
}
conditions[] { type, status, observed_at, evidence_refs[] }
commitment_counts { on_track, at_risk, overdue, verified }
exception_counts { open, decision_required }
top_attention_refs[]
decision_request_refs[]
last_verified_outcome_refs[]
next_check_at
cursor
narrative?
```

Pulse 的 `conditions`、计数和引用必须由结构化 Commitment、Observation、Action 和 Verification 状态确定性聚合；LLM 只能补充 `narrative`，不能自由改写健康结论。所有结论必须落在 `observation_scope` 内；缺少证据的业务维度进入 `unknown_dimensions`，SSH/Docker 正常不能被提升为“完整项目正常”。Pulse 只传覆盖摘要、数量、Top-N 注意项引用和增量游标，不复制整个项目数据库。

另设三个独立信号：

```text
RuntimeHeartbeat          共享模型运行时/执行器是否存活
ProjectAgentCycleState    该逻辑项目 Agent 最近一次周期是否按计划成功、租约是否有效
ObservationFreshness      项目真实状态多久没有重新核验
```

多个逻辑项目 Agent 可以共享一个运行时，所以不得为每个项目伪造独立进程心跳。顶层可在组合视图中并排呈现 Pulse、运行时、项目周期和证据新鲜度，但不得把它们合并成一个“综合健康分”。没有收到新 Pulse 时显示 `unknown/stale`，不自动推断项目失败或健康；未变化 Pulse 可按 revision/cursor 压缩，避免周期噪音。

运维项目还要单独保留：

- HOSTHeartbeat：SSH/Linux 观察入口是否可达；
- DeploymentObservationFreshness：某个 Deployment 最近一次证据距今多久；
- ProjectAgentCycleState：项目 Agent 最近一次周期是否完成；
- BusinessProjectHealth：只在业务维度有明确 Observation 时派生。

HOSTHeartbeat 或 Docker 健康检查正常时，不直接生成 BusinessProjectHealth=healthy；缺少证据的维度保持 unknown。

必须正交显示：

```text
ProjectHealth         仅在 observation_scope 覆盖范围内由领域 Conditions 确定性派生
RuntimeAvailability   来自共享 RuntimeHeartbeat 的运行时可用性
AgentCycleState       逻辑项目 Agent 的最近周期、调度租约和失败状态
EvidenceFreshness     来自 ObservationFreshness 的证据新鲜度
AuthorityState        当前 Agent 能做什么
CommitmentState       承诺是否正常、风险、逾期或已履约
```

“项目 Agent 报告正常”不能替代来源、时间和证据。项目内部出现报告过期、连续失败或回执未知时更新对应 `ProjectException`；业务统筹 Agent 发现跨项目资源冲突时生成 `PortfolioException`。

## 5. 知识空间与最小权限

### 5.1 五类空间

1. **UserPrivateMemory**：经确认的用户偏好、长期目标、沟通习惯和全局禁区。业务统筹 Agent 持有；项目 Agent 只接收与当前项目相关的投影。完整会话先属于 SessionContext，不自动成为长期记忆。
2. **SharedKnowledgeSpace**：品牌规范、通用 SOP、公司词汇、公共模板和经批准的共享事实。项目 Agent 按授权引用。
3. **ProjectPrivateKnowledgeSpace**：项目文档、已确认决策、运行手册、利益相关者和带来源的稳定记忆引用。对应项目 Agent 默认可读；新增或覆盖稳定记忆必须先 `propose`，再由规则或用户确认发布。
4. **OperationalEvidenceStore**：Observation、Declaration、Inference、ActionRun、Receipt、Verification 的唯一追加式记录。项目 Agent 可以追加观察和推断，但不能把推断覆盖为已确认事实。
5. **SecretStore**：Token、密码、私钥只以 `SecretRef` 和能力状态出现，秘密正文不进入任何知识库。

用户记忆按 `SessionContext → ProposedPreference → ConfirmedPreference` 升级。长期记忆必须记录 `source`、`scope`、`version`、`confirmed_at` 和 `expires_at/delete_ref`；项目 Agent 只得到满足当前责任所需的偏好投影，不能读取完整私人对话。

### 5.2 KnowledgeGrant

```text
subject_agent_id
knowledge_space_id
project_scope
permission = search | read | propose | publish
purpose
sensitivity_ceiling
expires_at
```

全局硬性拒绝优先于项目策略；项目特例只能收紧权限，扩大权限必须由 Principal 授予。共享知识的写入默认先形成提案，经过来源、适用范围和版本检查后再发布。

知识事实冲突时并列保留来源并生成 `ProjectException/DecisionRequest`，不通过静默覆盖制造单一真相。结构化业务状态、运行证据和知识文档分别保存，不把所有内容统一塞进向量检索库。

知识引用只保存稳定 ID、版本和必要摘要，不复制全文；业务统筹 Agent 需要深入项目时，通过带目的的查询获取最小相关证据，不把所有项目原始日志装入自身记忆。

## 6. 从参考项目借鉴的机制

下表是 Post-MVP 研究依据，不是当前 MVP-1 依赖清单。只借已经被验证且由真实需求触发的机制，不同时引入图数据库、通用工作流引擎、插件市场或完整可观测平台。

| 参考项目 | 直接借鉴 | 在本项目中的位置 | 不照搬 |
|---|---|---|---|
| [Backstage](https://backstage.io/) | Provider → Processor → Stitcher；稳定实体外壳 | 各连接器只产生 Observation/Evidence；处理器做身份和引用归一化 | 不把某个 Provider 当成最终实体真相，不做插件市场 |
| [Infrahub](https://github.com/opsmill/infrahub) | Agent 会话分支、Proposed Change、Diff、人工合并 | 项目 Agent 的计划、知识提案、关系修正和业务统筹 Agent 跨项目变更 | 该阶段不引入完整图数据库和企业审批平台 |
| [Kubernetes](https://kubernetes.io/) / [Argo CD](https://github.com/argoproj/argo-cd) | `spec/status`、generation、Conditions、desired/live diff、重读核验 | `DelegationSpec` 与 `ProjectReport`；健康、授权、同步和新鲜度分轴 | 不把业务统筹 Agent 做成全局 K8s Controller，不自动 prune |
| [Kiali](https://kiali.io/) | 稳定拓扑骨架 + 时间窗口活动层、局部过滤、回放 | 保留现有全局/项目画布，加 Project Agent 状态和委托活动覆盖 | 不要求 Service Mesh/eBPF 作为前置条件 |
| [OpenTelemetry](https://opentelemetry.io/) / [Langfuse](https://langfuse.com/) | Trace/Span/Event/Link 跨 Agent、工具、批准和回执 | 用户意图为 root trace，项目委托为 child span，动作和验证继续向下 | 不保存无边界的 Prompt/日志正文 |
| [Temporal](https://github.com/temporalio/temporal) / [Kestra](https://github.com/kestra-io/kestra) | Definition/Version/Run/NodeRun、事件历史、重试、超时、heartbeat | `StewardCycle`、`ProjectAgentCycle`、`WorkflowRun`、`ActionRun` 分开 | 不自研通用工作流运行时 |
| [DataHub](https://github.com/datahub-project/datahub) / [OpenLineage](https://openlineage.io/) | 类型化身份、Aspect 原子更新、Run 与 Evidence/Lineage 分离 | 项目、人员、承诺、知识项和运行证据可分别更新和追溯 | 不把数据血缘模型原样搬入所有业务对象 |
| [Structurizr](https://docs.structurizr.com/) / [LikeC4](https://likec4.dev/) | 一个 Model，多种 View；布局不是真实 | 保留现有 UI，通过全局业务统筹 Agent 图、项目 Agent 图、运行和知识视图钻取 | 不删除当前画布，不把布局当事实 |
| [Meshery](https://github.com/meshery/meshery) | 版本化 Model、组件/关系/策略、Adapter 能力注册 | 能力目录、Agent Binding、项目子图和策略验证 | 不把画布连线自动变成执行流程 |
| Portainer / Coolify | 连接预检、Docker/Compose onboarding、错误诊断 | `docs/10` 作为技术项目 Agent 的 SSH/Docker 适配器 | 不扩成 PaaS、容器 CRUD 或高权限终端 |

## 7. Post-MVP 对现有 UI 的增量规则

现有前端原型和交互基线继续保留：

- 全局业务网与项目资源/运行视图不删除；
- 固定信息岛、拖拽连线连续反馈、运行/编辑模式和历史轨迹不删除；
- 当前画布布局不因新增 Agent 层而重排；
- 新内容以卡片、徽标、信息岛和钻取层追加。

进入对应阶段后才允许新增：

1. 全局项目卡常驻只追加一个 `Project Agent / CycleState` 徽标和一个 attention 数量；项目健康沿用现有表达，异常、审批、证据和周期详情进入固定信息岛，避免把项目卡堆成仪表盘；
2. 全局消息由业务统筹 Agent 回复，项目页消息由当前项目 Agent 回复；每条消息显示 `speaking_agent`、`project_scope` 和当前知识范围；
3. 项目页面增加项目 Agent 工作台：目标、今日检查、异常、下一动作、最近核验结果；
4. 右侧信息岛增加 `global / shared / project / session` 知识来源和更新时间；
5. 新增委托、异常、批准、已核验结果卡片，但不删除现有运行轨迹；
6. 全局默认层继续只显示业务统筹 Agent 和项目；项目卡追加 Project Agent 周期/责任状态。选中或钻取项目时，才展开 `业务统筹 Agent → 项目 Agent → 项目` 的监督/委托关系；运行依赖继续使用不同关系类型；
7. 运行时离线、项目 Agent 周期过期、证据过期或回执未知时显示明确且不同的状态，不伪造健康。

## 8. Post-MVP 实施参考

> 启动前提：当前 MVP-1 已通过真实样本证明可视化管理价值，并出现仅靠人工整理、轻量建议或重扫差异解决不了的持续监督需求。以下 F1–F5 不是当前开发队列。

### F1：单项目监督语义试验

- 只选一个真实项目，引入最小 `ProjectCharter / ResponsibilityContract`、观察范围和升级规则；
- 用现有 Observation 验证范围化 `ProjectPulse / ProjectException / Proposal`；
- 未观察到的维度保持 `unknown`，不以 Docker 健康代替业务健康；
- 继续保留当前画布，只在信息岛追加少量状态。

### F2：逻辑项目 Agent

- 为项目绑定一个逻辑 Project Agent；同一运行时可承载多个隔离身份；
- 固定回复归属、项目作用域、周期状态和最小知识命名空间；
- 业务统筹 Agent 只汇总需要用户关注的事项；
- 不建设自由互聊的 Agent 群或通用 federation。

### F3：共享知识与治理

- 在真实隔离需求出现后实现 UserPrivate、Shared、ProjectPrivate、Evidence、Secret 空间；
- 实现版本化 KnowledgeGrant、共享知识发布/撤回和必要审计；
- 先使用数据库命名空间与固定角色，不提前建设通用 ACL 语言、独立知识服务或向量平台。

### F4：受控行动与业务 Adapter

- 只选择一个具有真实权限、明确回执、重读验证和回滚/补偿方式的低影响动作；
- 对动作分别声明 `operation_class`、`reversibility` 和 `approval_policy`；
- 固定 `Observe → Plan → Check → Approval → Execute → Receipt → Re-observe → Verify`；
- 只有重新观察与预期一致后才形成 `VerifiedOutcome`。

审批绑定不可变动作载荷及其哈希；参数变化后原审批失效。每项动作带幂等键、租约和冷却时间，防止周期或重试造成重复执行。

### F5：跨项目协调

- 处理共享资源冲突、全局优先级、预算和用户决策；
- 项目 Agent 之间通过业务统筹 Agent 或受控事件协作；
- 增加暂停/撤回委托、证据最小化和依据回放。

## 9. 实际场景校验

本节只验证长期产品愿景与 F4/F5 能力，不是当前 MVP-1 的需求、数据模型或验收清单。MVP-1 只承担 SSH/Linux 事实、真实图、人工投影管理和轻量 Agent 辅助；内部任务、提醒、发布审批和外部动作均为后续假设。

### 9.1 艺人公司：一次作品发布

固定夹具：共同知识中有品牌规则“所有对外发布必须由用户批准”；项目私有知识中有未发布素材清单和合作方截止时间；截止前 24 小时仍缺一份封面文件；责任契约规定项目 Agent 每 2 小时检查一次，可创建内部跟进任务，不可直接公开发布；`ReleaseOpsAdapter` 提供排期、素材清单、内部任务及重读能力。

字面验收：

1. 项目 Agent 生成带 `evidence_refs/evidence_cutoff` 的 `at_risk` Commitment 与 Pulse；
2. 业务统筹 Agent 只收到“发布时间风险、影响和是否需要用户决定”的摘要，不收到未发布素材正文；
3. 内部跟进任务属于预授权动作，同一委托重放两次也只创建一次；收到 Receipt 后重新读取到该任务，才生成 `VerifiedOutcome`；
4. 公开发布动作被全局策略拦截并生成 `DecisionRequest`；
5. 另一个项目 Agent 检索未发布素材时返回 `deny/empty`，并留下审计记录。

这个场景验证了：业务统筹 Agent 懂用户但不代替项目 Agent；项目 Agent 懂项目但不越过知识与权限边界；“艺人公司”不再只是远期叙述，而有最小数据入口和可核验动作。

### 9.2 业务统筹 Agent“懂用户”：把偏好投影为策略

固定输入：用户对业务统筹 Agent 说“以后任何面向公众的内容都先给我确认，但内部提醒不用问我。”

1. 业务统筹 Agent 生成可见的 `ProposedPreference/PolicyChange`，用户确认后形成带版本的全局策略；
2. 项目 Agent 在下一次周期获取该策略版本；内部提醒继续按预授权执行，公开内容进入 `approval_policy=required`；
3. 用户撤回偏好后，旧授权缓存失效；审计能说明每次动作依据的是哪个 `policy_revision`；
4. 项目 Agent 只收到与当前项目有关的策略投影，不得到用户完整私人对话。

这个场景验证了：“懂用户”不是把聊天全部存进长期记忆，而是把经确认的偏好转成可追踪、可撤回、可投影的规则。

### 9.3 超级个体：两个项目争用同一注意力

固定夹具：内容运营项目和客户交付项目在同一天各请求用户 2 小时，两个 Pulse 都有新鲜证据。

1. 两个项目 Agent 只报告本项目影响；
2. 业务统筹 Agent 去重并生成一个 `PortfolioException`，依据已确认的用户优先级提出选择，只打断用户一次；
3. 用户决定被拆成两个 project-scoped 委托，两个项目只看到与自己有关的决定；
4. 一条 root trace 串起用户决定、两个子委托和各自的核验结果。

这个场景验证了：业务统筹 Agent 的核心价值是管理注意力和跨项目取舍，不是复制每个项目的全部上下文。

### 9.4 无人值守期间：活着、在工作、有新证据是三件事

共享 `RuntimeHeartbeat` 可能正常，但某个 `ProjectAgentCycleState` 已经过期，或数据连接器数小时没有产生新证据。此时 UI 必须分别显示运行时正常、项目周期过期、证据过期；`ProjectHealth` 显示未知而不是绿色。若动作已经发出但只有 Receipt、尚未重新观察到结果，状态保持 `receipt_received/unknown`，不能显示 `verified`。

恢复验收还包括：Dispatcher 在执行前崩溃时可以安全重试；执行后、回执写入前崩溃时先进入 `unknown/re-observe`，不得直接重复副作用。

这个场景验证了：系统监测的不是“AI 有没有说一切正常”，而是责任周期是否运行、证据是否新鲜、动作结果是否被重新核验。

### 9.5 场景结论

上述场景与当前产品方向相符，但要成立有三个前提：

1. 每个项目先定义可检查的目标、责任契约和异常阈值，而不是只创建一个 Agent 名称；
2. 连接器能提供真实 Observation，项目 Agent 不能靠对话记忆猜测运行状态；
3. 自动执行必须有权限、幂等、回执、重读验证和回滚，逐级扩大自主范围。

因此当前 MVP-1 先打通 `SSH Observation → Deterministic Visual Draft → Manual Management → Optional Agent Suggestion → User Confirmation`；Post-MVP 再验证 `责任契约 → 范围化观察 → Pulse/异常 → 用户确认`；受控行动阶段才加入 `Execute → Receipt → Re-observe → Verify`。后续闭环必须以真实需求为触发条件，不能反向扩大当前范围。

## 10. 分层验证边界

### 10.1 当前 MVP-1 非回归边界

- 通过 SSH 账号密码（默认）或用户明确提供的 SSH Key 连接 Linux TARGET_HOST，首次指纹由界面展示并经用户确认；账号密码登录支持标准 Password Authentication 与单密码 Keyboard-Interactive/PAM；系统不自动发现或盲试私钥，账号密码是首次接入和认证失败补救的兜底入口；多因素/MFA、多轮交互、SSH Agent 和托管公钥安装后置；
- Docker/Compose 和项目文档 Observation 带来源、时间、新鲜度和脱敏状态；
- 确定性规则无需模型即可生成可编辑图草稿；
- 用户能够人工整理并确认本地投影；Agent 只追加可选建议和问题；
- 当前 UI 的全局/项目视图和既有交互保持可用；
- 整条链路保持只读，不把外部控制动作纳入当前验收。

以上边界只用于确保未来工作不破坏当前产品；MVP-1 的正式验收仍以 `docs/01/03/04/09/10` 为准。

### 10.2 Post-MVP：单项目只读监督

- 一个项目拥有可查看、可版本化的 ProjectCharter、ResponsibilityContract 和逻辑 Project Agent；
- 项目 Agent 能从结构化状态确定性生成带 `freshness / observation_scope / coverage / unknown_dimensions / conditions / evidence_refs` 的 ProjectPulse；
- 业务统筹 Agent 能汇总注意项，并区分范围内项目状态、共享运行时、逻辑项目周期和证据新鲜度；
- 全局入口与项目页显示明确的 `speaking_agent` 和 `project_scope`，不会产生重复回复；
- 项目私有知识不会被其他项目 Agent 默认检索；
- 未授权、过期、知识冲突和无法核验的结论进入 ProjectException/PortfolioException，不显示为正常或完成；
- 用户可以确认、修改或拒绝建议，决定可追踪到责任契约和证据。

### 10.3 更后续的受控行动与业务场景

- 一次委托可从用户消息追踪到项目 Agent、工具、回执、重新观察和核验；
- 同一委托重放两次时外部动作只发生一次；只有 Receipt、重读不一致或结果未知时不显示 Verified；
- 责任契约、权限、KnowledgeGrant 或全局策略撤回后，在下一次调用前生效；
- ReleaseOps Fixture/Adapter 能验证排期、素材、内部跟进和公开发布审批，但不作为当前 MVP-1 或 F1/F2 监督阶段的前置条件。

## 11. 非目标

- 不建立多个自由互聊的黑盒 Agent 群；
- 不把所有知识库合并为单一全局上下文；
- 不要求每个 Agent 独立部署模型服务；
- 不删除当前 UI 或把画布重写成聊天应用；
- 不因增加项目 Agent 就提前引入图数据库、微服务或完整工作流引擎。

## 12. 待确认决策

以下问题全部属于 Post-MVP F1–F5，不阻塞当前 MVP-1，也不应提前产生数据库迁移或 API：

1. `ReleaseOpsAdapter` 在 P4 连接真实任务系统，还是先使用排期、素材清单和内部任务的本地受控 Fixture？
2. 第一个预授权低风险动作选“创建内部跟进任务”还是“发送内部提醒”？
3. 哪些用户偏好必须逐次显式确认，哪些低敏偏好可以批量确认？
4. 业务统筹 Agent 默认只看项目摘要；用户临时授权深入时，授权时长和最大证据范围是多少？
5. ProjectPulse、ProjectAgentCycle 和 ObservationFreshness 的周期及过期阈值如何按项目类型配置？
