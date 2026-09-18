# Network Atlas

Network Atlas 从可视化管理切入：先把 HOST 上可观察的部署与资源转换为可追溯、可编辑的业务网与项目资源图，再增加多 Provider 发现、人工归属、可选 Agent 辅助和互联网发布能力。

当前实现已完成 **MVP-1 / M4（可视化管理闭环的互联网发布门槛）**：

- 全局业务网与项目资源图通过统一 `DataSource` 读取 Rust API；
- 用户可在现有画布登记 Linux HOST、确认主机指纹，并通过系统 OpenSSH 执行固定只读发现；
- Docker、Compose、容器、镜像、网络、卷、端口和白名单项目文档证据会自动生成确定性图草稿，不依赖模型；
- 真实投影支持更名、拖动布局、新建项目、重新归类、添加关系、归档/恢复和确认发布；项目的合并/拆分用重新归类与取消归类表达；
- 事实证据保持不可变，用户操作只修改本地投影；节点和关系保留来源引用与观察时间；
- SSH 密码和私钥只保存为服务端受限秘密文件，数据库只保存引用及幂等校验值；原始凭据和敏感文档内容不会进入 API 或日志；
- 刷新页面或重新打开动态项目深链接后，草稿、确认版本和布局仍保持一致；
- 可选 Agent 使用最小 OpenAI 兼容 `URL / Key / Model` 配置，只生成带证据的建议、问题与可编辑补丁；模型不可用不影响确定性画布；
- 同一 HOST 重扫会区分新增、变化、消失、冲突和未变化，并保留已确认投影与人工决定；
- 公网形态使用单一所有者登录、受保护业务 API/SSE、严格 Origin/CSRF、限流和安全 Cookie，不建设多用户或 RBAC；
- SSE 只推送已提交变化摘要，前端收到后重读 REST；支持持久游标、`Last-Event-ID`、heartbeat 和游标过期重置；
- 工作区、HOST 和项目支持不含秘密正文的 JSON 导出与本地删除；删除前必须生成并验证 SQLite/秘密备份；
- 真实数据模式下，全局资源用于服务器登记、SSH 连接、Key、重连与扫描；项目运行与流程仍保留视觉原型并明确显示 `本地 · Fixture · unavailable`；
- `file:` 或显式 `?data=mock` 才进入视觉 Fixture；HTTP 页面在 API 暂不可用时保留上次成功状态并明确报错，不再回退成假项目。启用所有者认证后，匿名访问只显示登录门。

真实服务器登记与扫描事实保持分层：服务器地址、SSH 端口、SSH 用户和登录凭据是可编辑的本地连接配置；修改这些字段或登录凭据后会清除旧连接结果，并重新核对主机指纹。首版默认使用 SSH 账号密码，也支持 SSH 私钥和单密码 Keyboard-Interactive/PAM。Docker/Compose 项目名、服务名和容器名，以及后续 systemd/PM2/进程 Provider 的结果，都是只读扫描事实；服务器别名、项目显示名和服务显示名可自定义，项目绑定只改变本地归属关系。已登记服务器但尚无成功扫描时，全局资源显示真实 HOST 与失败原因，项目数保持为 0；业务统筹 Agent 只面向真实业务任务，不与 HOST 连线。

## 本地启动

需要 Rust 1.94+ 与 Node.js 20+。

```powershell
npm ci
npm run generate:types
cargo run -p network-atlas
```

打开 <http://127.0.0.1:8787/>。服务默认把 SQLite 数据写入 `data/network-atlas.db`，并由同一 Rust 进程提供前端与 API。

可选环境变量：

| 变量 | 默认值 | 作用 |
|---|---|---|
| `NETWORK_ATLAS_BIND` | `127.0.0.1:8787` | 本地监听地址 |
| `NETWORK_ATLAS_DATABASE_URL` | `sqlite://data/network-atlas.db?mode=rwc` | SQLite 连接地址 |
| `NETWORK_ATLAS_DATA_DIR` | `data` | SQLite 之外的秘密、SSH 状态与备份目录 |
| `NETWORK_ATLAS_AUTH_MODE` | `auto` | Loopback 无配置时关闭认证；公网监听必须使用 `required` |
| `NETWORK_ATLAS_OWNER_USERNAME` | 无 | 单一所有者用户名；认证启用时必填 |
| `NETWORK_ATLAS_OWNER_PASSWORD_HASH_FILE` | 无 | Argon2id 密码哈希文件；认证启用时必填 |
| `NETWORK_ATLAS_ALLOWED_ORIGIN` | 无 | 与公开入口完全一致的 HTTPS Origin；认证启用时必填 |
| `NETWORK_ATLAS_COOKIE_SECURE` | `true` | 公网会话必须保持 Secure Cookie |
| `NETWORK_ATLAS_MONITOR_MAX_CONCURRENCY` | `1` | 全局同时执行的只读发现/资源观测上限，允许 `1..32` |
| `NETWORK_ATLAS_MONITOR_TICK_SECONDS` | `5` | 持久化 interval 调度器检查到期任务的频率，允许 `1..60` 秒 |
| `NETWORK_ATLAS_MONITOR_COMPACTOR_TICK_SECONDS` | `3600` | 本地 SQLite 历史 compactor 周期，允许 `60..86400` 秒；启动时会立即执行一次 catch-up |
| `NETWORK_ATLAS_MONITOR_COMPACTOR_PARTITIONS_PER_TICK` | `32` | 每轮每种 resolution 最多处理的 UTC 分区数，允许 `1..1024` |
| `NETWORK_ATLAS_MONITOR_RETENTION_ENABLED` | `false` | 是否执行破坏性 retention；只有显式设为 `true` 才删除已被安全 rollup 覆盖的历史行 |
| `NETWORK_ATLAS_MONITOR_RETENTION_DELETE_BATCH_SIZE` | `5000` | retention 每轮最多删除的 raw sample 数，允许 `1..100000` |
| `NETWORK_ATLAS_MONITOR_RAW_OBSERVED_RETENTION_DAYS` | `7` | observed raw 保留天数，允许 `1..365` |
| `NETWORK_ATLAS_MONITOR_RAW_NON_OBSERVED_RETENTION_DAYS` | `30` | 非 observed raw 保留天数，允许 `raw observed..730` |
| `NETWORK_ATLAS_MONITOR_HOUR_RETENTION_DAYS` | `90` | hour rollup 保留天数，允许 `raw non-observed..3650` |
| `NETWORK_ATLAS_MONITOR_DAY_RETENTION_DAYS` | `365` | day rollup 保留天数，允许 `hour..7300` |

浏览器同源访问时默认使用 `HttpDataSource`。使用 `?data=mock` 或直接以 `file:` 打开页面时使用 `MockDataSource`。

监控调度不会自动创建。登记 HOST 后需通过
`POST /api/v1/hosts/{host_id}/monitor-schedules` 明确保存 interval、jitter 与
`stale_after_seconds`；创建 schedule 不会立即建立 SSH 连接，立即采集继续使用
`POST /api/v1/hosts/{host_id}/monitor-runs`。schema 11 起保存 typed raw 指标，不从旧
current 快照回填；schema 12 增加 UTC hour/day rollup、周期 compactor、维护状态与
`raw/hour/day/auto` 查询。接口默认返回 HOST 聚合，可用 typed
subject/metric/sample filters 下钻，并用双字段 cursor 分页；累计 counter 在 SQLite
中保持整数，响应另带精确十进制字符串。`auto` 按查询跨度选择单一实际 resolution，
不会把不同精度拼成一条伪精度曲线。

compactor 只读写 APP_HOST 上的 SQLite，不连接 TARGET_HOST；它在进程启动时立即检查一次，
之后按配置周期处理最多 `partitions_per_tick` 个 hour 分区和同样数量的 day 分区。rollup
始终运行，但 retention 默认关闭：`NETWORK_ATLAS_MONITOR_RETENTION_ENABLED=false` 时
不自动删除 raw/hour/day 历史。即使显式启用，仍会在闭合分区积压追平前暂停删除，避免先删掉
后续 counter bucket 所需的边界样本。启用前应先验证备份恢复、SQLite 增长与查询需求；关闭只能
停止后续删除，不能恢复已经删除的数据，也不保证 SQLite 文件立即缩小。schema 13 已加入
版本化 HOST 健康策略、run admission policy pin、同一 run 的 typed health evaluation，以及
24h/7d/30d UTC 健康时间带；schedule-version gap/coverage 会区分 explicit unknown 与
缺少 evaluation 的 due slot。ProjectTarget/service/port、Prometheus、告警和受控操作仍后置。

## M0 验证

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify-m0.ps1
```

统一入口会执行 Rust 格式、编译、测试、Clippy、OpenAPI 导出、前端类型生成、前端检查与同源 API 烟测。视觉回归记录位于 `artifacts/m0-visual-contract-20260811/`。

完整范围与阶段门见 `docs/00-文档索引.md` 和 `docs/PROJECT-STATUS.md`。本地完整验收工件位于未随公开交接快照发布的 `artifacts/` 与 `work/` 目录。

## M1 验证

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify-m1.ps1
```

统一入口会执行 M0 回归、SQLite 重复迁移、Rust 测试与 Clippy、OpenAPI/前端契约检查、真实 OpenSSH 集成夹具、同源 M1 API 烟测和运行时泄漏扫描。M1 仍只读目标 Linux HOST，不执行服务器控制动作。

## M2 验证

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify-m2.ps1
```

统一入口会重新构建本地 OpenSSH 夹具，执行全部 M0/M1/M2 Rust 测试、Clippy 零警告、OpenAPI 与前端契约检查，并从真实 SSH 证据完成“生成草稿 → 更名/归类/关系/布局/归档恢复 → 确认 → 重读”的纵向验证。浏览器复验截图和精确结果位于 `artifacts/m2-visual-management-20260811/`。

M2 只发布本地投影，不修改目标 Linux HOST。

## M3 验证

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-m3.ps1
```

统一入口覆盖最小 OpenAI 兼容配置、模型失败降级、带证据建议与人工决定、五类重扫差异，以及 M0-M2 回归。M3 不替代确定性投影，也不执行服务器控制动作。

## M4 本地验证

需要本机可用的 Caddy 二进制，默认路径为 `target/m4-tools/release/caddy.exe`。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-m4.ps1
```

统一入口执行 M0-M4 Rust 回归、Clippy、OpenAPI Cookie 安全契约、文档路径一致性、前端检查、发布脚本语法与隔离命令流演练、Compose 解析、真实本地 HTTPS 登录/CSRF/SSE/注销烟测、秘密泄漏扫描和浏览器证据校验。精确结果与截图位于 `artifacts/m4-remote-release-20260811/`；最终完成审计位于 `artifacts/mvp1-completion-audit-20260811/`。

## Linux 互联网发布

发布包与首次部署、备份、恢复、升级和回滚命令位于 `deploy/README.md`。真实上线前必须提供域名、Linux APP_HOST、所有者密码和备份去向；Rust 只在内部网络提供服务，公网入口由 Caddy 终止 HTTPS。

辅助命令：

```powershell
# 从不写入命令历史的交互输入生成 Argon2id 哈希
Read-Host -MaskInput "Owner password" | cargo run -p network-atlas -- --hash-password

# 离线校验 SQLite 备份
cargo run -p network-atlas -- --verify-backup PATH_TO_BACKUP.db
```

MVP-1 始终只读 TARGET_HOST；Docker、项目文件和服务器服务的控制动作不在当前范围。下一阶段的运维对象、非 Docker 发现和服务器页面见 docs/13-运维项目发现与服务器页面规格.md。

## 下一阶段：运维资产与项目绑定

MVP-1 之后，系统先解决“发现部署实例、由用户组合成运维项目、让 Project Agent 绑定具体目标”：

1. Docker/Compose、systemd、PM2、端口/进程和用户确认根目录作为独立 Provider；
2. DeploymentCandidate 进入草稿，用户合并、拆分、绑定或忽略；
3. TechnicalProject 通过 ProjectTarget 绑定到具体 HOST 和 Deployment；
4. 新增 global/hosts 服务器资产与最近观测页面；
5. 先开放项目范围内的 typed read-only Agent 工具，持续监控和远程控制另行验收。
