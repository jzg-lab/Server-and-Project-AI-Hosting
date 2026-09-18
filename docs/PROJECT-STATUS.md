# Network Atlas 当前状态与进度

> 最后核对：2026-09-18
>
> 本文只记录当前工作树和实现事实，不替代产品规格。若与代码冲突，以代码、迁移、OpenAPI 和可复现验证为准；冲突必须回写文档。

## 1. 总体判断

项目已经完成 MVP-1 / M4 的主要产品闭环，并继续完成了 H1-H6 的运维目录、监控、业务资源和只读 Project Agent 增量。

当前工作不应被描述为“从零开始”或“已完成完整运维平台”。准确说法是：

- MVP-1：已完成并停止在 M4。
- H1-H6：已有实现、迁移、测试和验收工件。
- Post-MVP：Target 级持续观察、告警、Prometheus、服务/端口级完整能力和受控操作仍未完成。
- GitHub 云端：正在向 `jzg-lab/Server-and-Project-AI-Hosting` 发布脱敏交接快照；在推送命令成功前，不视为已上传。

## 2. 已完成能力

| 阶段 | 当前事实 | 主要位置 |
|---|---|---|
| M0 | Rust/Axum、SQLite、OpenAPI、统一 DataSource、真实页面最小契约 | `backend/`、`frontend/` |
| M1 | SSH 连接、指纹确认、账号密码/Key、只读发现、SecretRef、证据脱敏 | `backend/src/ssh.rs`、`discovery.rs`、`secrets.rs` |
| M2 | 事实到图草稿、人工归类/更名/关系/布局/确认/归档 | `backend/src/projection.rs`、`frontend/app.js` |
| M3 | 可选 OpenAI 兼容 Agent、带证据建议、重扫五类差异、模型失败降级 | `backend/src/model_provider.rs`、`discovery_diff.rs` |
| M4 | 单一所有者、HTTPS/Caddy、SSE、导出/删除、备份/恢复/升级/回滚 | `backend/src/auth.rs`、`deploy/` |
| H1-H3c | 资源快照、interval 调度、raw/hour/day 历史、健康策略与时间带 | `backend/src/monitoring*.rs` |
| H4-H5 | Deployment、TechnicalProject、ProjectTarget、Business、ResourceEntity 和关系读模型 | `backend/src/catalog.rs`、`catalog_api.rs` |
| H6 | TechnicalProject 绑定的五个 typed read-only Project Agent 工具与审计 | `backend/src/project_agent.rs` |

## 3. 当前代码事实

- 后端是单一 Rust workspace，crate 位于 `backend/`，由同一个进程提供 API 和前端静态文件。
- 前端是原生 HTML/CSS/JavaScript，不是 React/Vue 应用。
- SQLite 迁移当前到 `0016_project_agent.sql`，必须通过增量迁移维护兼容性。
- OpenAPI 导出物位于 `openapi/openapi.json`，前端类型位于 `frontend/generated/api.d.ts`。
- 监控 scheduler 和 compactor 随应用进程启动，但 HOST 的 interval schedule 不会自动创建。
- retention 默认关闭；开启 retention 属于破坏性数据策略，不能在没有备份/恢复依据时擅自打开。
- `global/hosts` 负责连接和扫描管理；`global/resource` 负责关系投影；服务器页不能重新复制项目资源画布。

## 4. 下一主线

下一步应由真实使用缺口驱动，优先顺序为：

1. 先补一个真实缺口 Provider，优先用户确认的根目录/项目文档或其他已证实需求。
2. 再进入 Target 级持续观察，明确采样、过期、健康、告警和存储成本。
3. 只有在上述事实稳定后，才评估告警、Prometheus 或受控操作。

不能因为 `docs/11` 存在，就提前实现跨项目协调、责任契约或自主控制。

## 5. 当前工作树

当前分支是 `codex/real-host-acceptance`，最近提交为 `f550f30`（2026-08-15）。当前没有 Git remote。工作区非干净，存在本次文档整理和既有未跟踪材料：

- H1、H2、H3a、H5、H6 验收目录；
- SSH 密码流程和项目导航截图/验收材料；
- `docs/AUTH_AUTOMATION_PLAYBOOK.md`；
- 本次新增但尚未提交的 `docs/AGENT-ONBOARDING.md`、`docs/PROJECT-STATUS.md`、`docs/CHANGE-GUIDE.md`；
- 本次修改但尚未提交的 `docs/00-文档索引.md`；
- 本次新增但尚未提交的 `docs/PUBLIC-REPOSITORY-SCOPE.md`；
- `work/live-app-current.js`、`work/served-app-audit.js`。

这些材料不能自动删除或批量加入提交。处理前应逐项判断：长期有价值的进入 `docs/` 或 `artifacts/`，临时调试材料保留或由用户明确批准清理。

## 6. H1-H6 定位表

| 阶段 | 主要实现 | 行为测试 | 相关验收材料 |
|---|---|---|---|
| H1 | `backend/src/monitoring.rs`、`monitoring_api.rs` | `backend/tests/monitoring_collection.rs` | `artifacts/h1-ui-verify-20260815/`、`artifacts/host-resource-snapshot-20260815/` |
| H2 | `backend/src/monitoring_scheduler.rs` | `backend/tests/monitoring_scheduler.rs` | `artifacts/h2-scheduler-20260815/`、`artifacts/h2-scheduler-final-20260815/` |
| H3a-c | `monitoring_history.rs`、`monitoring_rollup.rs`、`monitoring_health.rs` | `backend/tests/monitoring_history.rs`、`monitoring_retention.rs`、`monitoring_health_semantics.rs` | `artifacts/h3a-raw-history-final-20260815/` 及对应提交记录 |
| H4 | `backend/src/catalog.rs`、`api/catalog_api.rs` | `backend/tests/h4_catalog.rs` | `PROJECT-STATUS.md` 与 Git 历史；本地完整审计另见 `work/MVP-1-PROGRESS.md` |
| H5 | `backend/src/catalog.rs`、`catalog_api.rs` | `backend/tests/h5_business.rs` | `artifacts/h5-business-resources-20260815/` |
| H6 | `backend/src/project_agent.rs` | `backend/tests/h6_project_agent.rs` | `artifacts/h6-project-agent-20260815/` |

## 7. 当前验证依据

完整 M0-M4 过程和历史结果在本地工作区的 `work/MVP-1-PROGRESS.md` 及对应 `artifacts/` 目录中；公开交接快照不包含这些本地运行材料。当前文档整理本身至少应通过：

```powershell
git diff --check
```

若修改了代码，再运行对应的 Rust、前端、OpenAPI 或部署验证。文档整理不等于重新宣称全部历史验收在今天重新通过。

## 8. 交接时必须说明

每次交接至少说明：当前分支、最近提交、工作树是否干净、是否存在 Git remote、完成了哪些验证、未完成项、是否触及秘密/真实 HOST/部署边界。没有这些信息，新 Agent 不应继续扩大范围。
