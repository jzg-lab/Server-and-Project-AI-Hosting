# Project Network Visualization Agent Rules

本文件是仓库内 Agent 的首要交接入口。开始工作前先阅读本文件，再从 `docs/00-文档索引.md` 进入对应规格。

## 工作原则

1. 遵循第一性原理和奥卡姆剃刀：先确认事实，再提出推测；没有证据时明确标为待验证，不用复杂基础设施解决尚未出现的问题。
2. 修改前先使用 Obsidian CLI 查询既有 `Network Atlas` 记录；形成可复用结论后再写入 Obsidian。仓库 `docs/` 保存可执行规格，Obsidian 保存长期记忆与研究结论。
3. 保留既有真实数据、演示数据、保存的凭据、迁移历史和兼容契约。除非用户在当前任务明确要求，不执行清空、删除或自动清理。
4. 不修改 TARGET_HOST 的 SSH 配置。当前发现与连接验证应保持只读；任何控制能力必须作为独立范围重新设计和验收。
5. 密码、私钥、Token、会话值和完整凭据不得出现在日志、文档、Git、测试产物或回复中。数据库只保存 SecretRef；秘密文件不进入 Git。
6. 修改前检查 `git status`，不要恢复或覆盖用户已有改动。若存在 Git remote，完成并验证后按项目流程推送；无 remote 时只提交本地 Git 记录并明确说明。

## 环境与部署边界

- 正式交付环境唯一入口：Linux APP_HOST + Docker Engine + Docker Compose v2，操作手册为 `deploy/README.md`。
- Windows 本地进程、PM2 或临时静态服务只用于开发和验收，不属于正式部署架构，不得写成生产依赖。
- APP_HOST 是运行 Network Atlas 的主机；TARGET_HOST 是应用通过 SSH 访问的 Linux 目标。两者不得混称。
- Rust/Axum 应用在 Compose 内部监听 `8787`；Caddy 对外提供 HTTPS。不要把应用端口直接作为正式公网入口。

## HOST 连接语义

- HOST 连接测试只回答：网络端口可达、SSH 认证成功，并能读取 Linux 身份。
- 上述条件成立即为 `connection_ready`，代表应用能够访问该 Linux 服务器。
- Docker、Compose、项目文档等属于连接后的附加发现。未安装、命令不可用或权限不足必须单独记录，不得把 HOST 降级为 SSH 认证失败。
- 前端必须分别展示连接结果和最近一次发现结果；兼容性枚举可以保留，但新代码不得重新混用两者。

## 变更与验证

- 优先保持现有 API JSON、OpenAPI schema、SQLite 表和迁移、SecretRef、URL/Hash 状态以及 UI 行为不变。
- 大文件拆分先补行为测试，再做机械移动；每次只移动一个职责边界。
- 部署文档改动至少运行 Compose 配置解析、Shell 脚本语法检查和 `git diff --check`。Docker daemon 或 Linux 实机不可用时，明确区分“静态配置通过”和“真实部署验收通过”。
- 不以直连 SSH、后端日志或单元测试代替产品 UI 的真实连接验收。
