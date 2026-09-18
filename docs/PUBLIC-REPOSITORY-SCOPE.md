# 公开仓库交接范围

> 最后核对：2026-09-18

本项目的本地工作区包含完整开发历史、阶段补丁、浏览器截图、真实 HOST 验收记录和临时运行日志。它们服务于本地审计，不适合进入公开 GitHub 仓库。公开交接快照已发布到 `jzg-lab/Server-and-Project-AI-Hosting` 的 `main`，提交为 `f4a5ae5c48bc5468b0d3676063e1752c7515b2e3`。

## GitHub 公开版包含

- Rust/Axum 后端、原生前端、SQLite 增量迁移和 OpenAPI 契约；
- 产品规格、架构决策、Agent 入场手册、当前状态和改动指南；
- 脱敏后的行为测试、验证脚本和 Linux Docker Compose/Caddy 部署文件；
- 能让新 Agent 在隔离数据库中启动和验证的最小完整源码快照。

## GitHub 公开版不包含

- `artifacts/` 和 `work/` 下的本地验收截图、运行日志、源码压缩包、补丁归档和真实环境导出；
- 真实 TARGET_HOST 地址、主机状态、连接结果、凭据引用或会话材料；
- 本地数据库、秘密目录、备份、`.env`、SSH 私钥和任何运行时秘密；
- 未经整合的其他 Agent 工作树或设计分支。

## 分支事实

公开版以 `codex/real-host-acceptance` 的 H1-H6 当前实现为基础。`codex/host-monitoring-alert-visibility` 和 `codex/host-interaction-concepts-20260815` 仍是未整合分支；它们不能被描述为公开版已交付能力。

## 接手方式

新 Agent 进入公开仓库后，先阅读根目录 `AGENTS.md`，再按 `docs/00-文档索引.md` 的“新 Agent 入口”阅读。公开仓库没有真实 HOST 数据，真实连接验收必须由维护者在受控环境中单独执行。
