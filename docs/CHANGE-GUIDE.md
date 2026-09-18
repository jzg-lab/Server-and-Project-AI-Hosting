# Network Atlas 改动指南

## 1. 选择正确的修改层

| 需求 | 首先查看 | 常见改动 |
|---|---|---|
| 产品范围或优先级 | `01`、`03`、`04` | 先改规格，再改实现 |
| API 字段、状态或路由 | `08`、`09`、`openapi/openapi.json` | Rust 契约、路由、测试、重新导出类型 |
| SSH/发现 | `10`、`13`、`backend/src/ssh.rs`、`discovery.rs` | 保持只读、脱敏、错误分类 |
| 监控/历史/健康 | `14`、`backend/src/monitoring*.rs`、对应迁移 | 先定义 freshness、成本和恢复语义 |
| 业务/资源目录 | `15`、`backend/src/catalog.rs`、`catalog_api.rs` | 保持事实与用户确认关系分离 |
| Project Agent | `16`、`backend/src/project_agent.rs` | 只允许 typed read-only、明确 ProjectTarget 范围 |
| UI 视觉/交互 | `02`、`06`、`frontend/app.js`、`styles.css` | 增量修改，不重做视觉基线 |
| 正式发布 | `12`、`deploy/README.md`、`deploy/` | Linux APP_HOST、Compose v2、Caddy |

## 2. 数据流

```text
TARGET_HOST
  -> SSH connection readiness
  -> fixed read-only providers
  -> immutable evidence / observation
  -> deterministic candidate and graph draft
  -> user edits and confirms
  -> local projection / catalog read models
  -> frontend canvas and resource views
```

Agent 建议只能插入在“事实已结构化、草稿已存在”之后，不能替代事实采集或用户确认。

## 3. API / 数据库变更规则

1. 先确定这是新增字段、状态变化、兼容扩展还是破坏性变化。
2. SQLite 只用新的增量迁移；不要修改已应用迁移来“修正历史”。
3. API 响应要保持现有 JSON 语义和错误包络；能兼容就不要静默迁移旧 `Project` / `ProjectionDraft`。
4. 写接口维持幂等键、版本/条件更新和审计语义。
5. 按以下顺序导出 OpenAPI 和前端类型：

   ```powershell
   cargo run -p network-atlas -- --export-openapi openapi/openapi.json
   npm run generate:types
   ```

6. 给行为测试补成功、失败、重试、过期和恢复路径。

## 4. 前端变更规则

- 真实页面使用 `HttpDataSource`；Mock 只能由 `file:` 或显式 `?data=mock` 触发。
- 页面必须分别显示连接结果、发现结果、新鲜度和业务健康。
- Hash 深链接和刷新恢复是既有行为，涉及导航必须回归。
- 不要把视觉原型的按钮误接成外部控制能力。
- 文字、控件、画布在桌面和移动视口都不能重叠或溢出。

## 5. 验证决策树

- 仅文档：`git diff --check`，检查链接和路径。
- 前端逻辑：`node --check`、`npm run test:frontend`。
- Rust/API/迁移：`cargo fmt`、`cargo check`、相关 `cargo test`、必要时 Clippy。目录映射：H4=`backend/tests/h4_catalog.rs`，H5=`backend/tests/h5_business.rs`，H6=`backend/tests/h6_project_agent.rs`，监控=`backend/tests/monitoring_*.rs`，M0-M4 按对应脚本和测试文件执行。
- OpenAPI：执行 `cargo run -p network-atlas -- --export-openapi openapi/openapi.json`，再运行 `npm run generate:types`。
- M0-M4 行为：运行对应 `scripts/verify-m*.ps1`。
- 真实 HOST/UI：运行真实验收脚本并记录脱敏结果；不能用单元测试替代。
- 部署文档/脚本：Compose 解析、Shell 语法、`git diff --check`；没有 Linux Docker 实机时只能报告静态通过。
- API/数据库/前端契约变化：必须重新导出 OpenAPI、生成类型，并补浏览器或同源 API 验收；只跑编译不算完成。

## 6. Git 与协作规则

- 修改前、提交前都检查 `git status`。
- 不使用 `git reset --hard`、`git checkout --` 或批量删除来清理现场。
- 其他 Agent 的分支或工作树必须先同步、理解并整合，不覆盖其改动。
- 本地完整工作区不配置 Git remote；公开交接快照已发布到 `jzg-lab/Server-and-Project-AI-Hosting`，但不能把本地未推送历史或 `artifacts/` 目录说成公开备份。
- 一个提交应表达一个可解释的阶段或改动，提交说明要能让后续 Agent 快速判断影响范围。

## 7. 文档同步规则

- 范围变化：更新 `01/03/04`，再同步 `05/08/09/10/13`。
- 当前实现变化：更新 `PROJECT-STATUS.md` 和相关规格；若当前工作区保留详细实施账本，再同步 `work/MVP-1-PROGRESS.md`。
- 修改前先查询 Obsidian 的 `Network Atlas` 记录；不可用时明确记录限制。新增长期结论写入仓库 `docs/`，Obsidian 运行时可用时再登记框架性结论。
- 不能把“计划”“愿景”“视觉保留”写成“已实现”。
