# Network Atlas Linux Docker Compose 部署手册

本目录是正式部署的唯一操作入口。正式环境为 **Linux APP_HOST + Docker Engine + Docker Compose v2**；Windows、PM2 和 `Caddyfile.local` 仅用于开发或发布烟测，不属于生产架构。

## 1. 先区分两台主机

- `APP_HOST`：运行 Network Atlas、Caddy、SQLite 和秘密存储的 Linux 主机。
- `TARGET_HOST`：Network Atlas 通过 SSH 访问的 Linux 服务器。

`TARGET_HOST` 不要求安装 Docker。网络端口可达、SSH 认证成功并能读取 Linux 身份，就表示 HOST 已连接；Docker/Compose 是之后的附加发现，缺失或权限不足不得归类为 SSH 失败。

## 2. 正式拓扑

```text
Browser --HTTPS--> Caddy :80/:443
                       |
                       +--Docker internal network--> Rust/Axum :8787
                                                        |
                                                        +--SQLite + FileSecretStore
                                                        +--OpenSSH (read-only)--> TARGET_HOST
```

Compose 中的 `app` 不发布宿主端口，只在内部网络暴露 `8787`。Caddy 负责公开 HTTPS、证书续期和 SSE 反向代理。

## 3. 上线前检查

APP_HOST 必须具备：

- 受支持的 Linux、Docker Engine 和 `docker compose` v2；
- 可写的持久磁盘，以及独立的站外加密备份位置；
- 解析到 APP_HOST 的真实域名；
- 入站 TCP `80/443`，需要 HTTP/3 时同时开放 UDP `443`；
- 未被其他服务占用的 `80/443`；
- Bash、curl、tar、sha256sum 和 GNU install，供发布脚本使用。

准备输入：

- `DOMAIN`：真实域名，例如 `atlas.example.com`；
- `ALLOWED_ORIGIN`：与域名完全一致的 HTTPS Origin，例如 `https://atlas.example.com`；
- `OWNER_USERNAME`：单一所有者账号；
- 所有者密码：只在交互式脚本中输入，不写入 `.env` 或命令历史；
- 不可变的应用镜像标识或当前源码 revision。正式发布应记录 Git revision 和 image digest。

SSH 密码、SSH 私钥和模型 Key 均在登录后通过产品界面保存到服务端秘密目录，不放入 `.env`、日志或 Git。

周期资源采集的进程级上限由 `MONITOR_MAX_CONCURRENCY` 控制，默认保守取 `1`；
调度检查频率由 `MONITOR_TICK_SECONDS` 控制，默认 `5` 秒。它们不是每台 HOST 的
采集周期：HOST interval、jitter 和 freshness 窗口通过产品 API 逐条持久化，而且
不会自动创建或启用。提高并发或缩短 interval 前，应先核对 TARGET_HOST 数量、
每日 SSH 次数和 SQLite 增量。

历史 compactor 与 SSH scheduler 相互独立：它只处理 APP_HOST 本地 SQLite，进程启动时
立即运行一次，之后按 `MONITOR_COMPACTOR_TICK_SECONDS` 周期运行。每轮分别最多处理
`MONITOR_COMPACTOR_PARTITIONS_PER_TICK` 个 UTC hour 分区和相同数量的 UTC day 分区；
增大批量或缩短周期会增加 APP_HOST 的 SQLite 读写、CPU、WAL 和备份压力，不会增加
TARGET_HOST 的 SSH 会话。

| `.env` 变量 | 应用环境变量 | 默认值与约束 |
|---|---|---|
| `MONITOR_COMPACTOR_TICK_SECONDS` | `NETWORK_ATLAS_MONITOR_COMPACTOR_TICK_SECONDS` | `3600`，允许 `60..86400` 秒 |
| `MONITOR_COMPACTOR_PARTITIONS_PER_TICK` | `NETWORK_ATLAS_MONITOR_COMPACTOR_PARTITIONS_PER_TICK` | `32`，允许 `1..1024`，限制分别作用于 hour/day |
| `MONITOR_RETENTION_ENABLED` | `NETWORK_ATLAS_MONITOR_RETENTION_ENABLED` | `false`；只有显式 `true` 才执行删除 |
| `MONITOR_RETENTION_DELETE_BATCH_SIZE` | `NETWORK_ATLAS_MONITOR_RETENTION_DELETE_BATCH_SIZE` | `5000`，允许 `1..100000`；限制每轮 raw 删除量 |
| `MONITOR_RAW_OBSERVED_RETENTION_DAYS` | `NETWORK_ATLAS_MONITOR_RAW_OBSERVED_RETENTION_DAYS` | `7`，允许 `1..365` |
| `MONITOR_RAW_NON_OBSERVED_RETENTION_DAYS` | `NETWORK_ATLAS_MONITOR_RAW_NON_OBSERVED_RETENTION_DAYS` | `30`，必须不少于 observed raw，最多 `730` |
| `MONITOR_HOUR_RETENTION_DAYS` | `NETWORK_ATLAS_MONITOR_HOUR_RETENTION_DAYS` | `90`，必须不少于 non-observed raw，最多 `3650` |
| `MONITOR_DAY_RETENTION_DAYS` | `NETWORK_ATLAS_MONITOR_DAY_RETENTION_DAYS` | `365`，必须不少于 hour，最多 `7300` |

rollup 生成不受 `MONITOR_RETENTION_ENABLED` 控制；保持 `false` 仍会生成 hour/day
分区、维护运行账本和容量计数，但不会自动删除 raw/hour/day 行。显式启用时，raw 只有在
对应 hour 与 day 分区都已完整写入并 sealed 后才按批次删除；hour rollup 只有存在对应 day
分区后才会清理；任一闭合 hour/day 分区仍有 compaction backlog 时，本轮 retention 整体暂停，
避免删除后续 counter bucket 的边界样本。把开关重新设为 `false` 只停止后续删除，已经删除的
数据只能从先前备份恢复。SQLite `DELETE` 释放的页通常供数据库复用，不代表数据库文件立即缩小。所有配置在
APP 启动时读取，修改 `.env` 后需要重建或重启 app；生产启用 retention 前必须先执行站外
加密备份与恢复演练，并根据实际 HOST 数、样本量和查询窗口调整预算。

Compose 为 APP 配置 35 秒停止宽限期。收到 SIGTERM/CTRL+C 后，APP 会先停止领取
新的周期任务并关闭 SSE 流，再等待已经取得执行许可的只读观察结束；固定 SSH
命令超时为 15 秒，因此不得把该宽限期缩短到默认的 10 秒。

## 4. 首次部署

```bash
cd deploy
cp .env.example .env
# 编辑 .env：替换 DOMAIN、ALLOWED_ORIGIN、OWNER_USERNAME，并确认 APP_IMAGE

docker compose --env-file .env config --quiet
docker compose --env-file .env build app
./scripts/generate-owner-password.sh
test -s secrets/owner_password_hash.txt
docker compose --env-file .env up -d
docker compose --env-file .env ps
./scripts/health-check.sh
```

注意：

- `DOMAIN` 和 `ALLOWED_ORIGIN` 必须是同一真实 HTTPS Origin，不能保留示例插槽。
- `secrets/owner_password_hash.txt` 必须在 `up` 前存在；它受 `.gitignore` 保护，不进入 Git。
- `generate-owner-password.sh` 当前从 shell 环境读取 `APP_IMAGE`。若 `.env` 自定义了镜像名，先执行 `export APP_IMAGE=实际镜像名`，避免脚本使用默认镜像。
- Caddy 首次申请证书需要 DNS 已生效且公网能访问 `80/443`。

查看状态和日志：

```bash
docker compose --env-file .env ps
docker compose --env-file .env logs --tail=200 app caddy
curl -fsS https://DOMAIN/healthz
```

首次验收必须包括：真实 HTTPS、所有者登录、关键页面读取、通过 UI 保存凭据并连接至少一台 TARGET_HOST。直连 SSH 或容器日志不能替代 UI 验收。

## 5. 连接与发现验收

HOST 的两个结果必须分开检查：

| 层次 | 成功条件 | 失败示例 |
|---|---|---|
| HOST 连接 | 端口可达、SSH 认证成功、读取 Linux 身份 | 网络不可达、host key 未确认、认证失败 |
| 附加发现 | 对应工具存在且当前 SSH 用户有读取权限 | Docker 未安装、docker.sock 权限不足、Compose 不可用 |

HOST 连接成功时状态应为 `connection_ready`。即使附加发现返回 `docker_unavailable`、`docker_permission_denied` 或 `compose_unavailable`，也仍然表示应用可以访问该服务器。

## 6. 日常操作

```bash
# 重启
docker compose --env-file .env restart

# 停止但保留卷和数据
docker compose --env-file .env stop

# 再次启动
docker compose --env-file .env up -d

# 实时日志
docker compose --env-file .env logs -f app caddy
```

不要使用 `down -v`；它会删除命名卷。部署者还应在 APP_HOST 层配置 Docker 日志轮转、磁盘余量告警和备份成功告警。

## 7. 备份

```bash
./scripts/backup.sh
```

脚本使用 SQLite 在线 `.backup`，校验数据库并归档数据卷中的 `secrets/` 与 `ssh/`，输出文件权限为 `0600`。归档包含真实服务器凭据，必须复制到访问受限的站外加密存储，并建立保留和恢复演练策略。

重要边界：Compose 的所有者登录哈希位于 APP_HOST 的 `deploy/secrets/owner_password_hash.txt`，不在应用数据卷归档中。必须将该文件作为独立秘密备份，保持 `0600` 权限；不要把它加入普通文档库或 Git。

## 8. 恢复

恢复会停止 app，并替换 `network-atlas-data` 卷中的数据库、秘密和 SSH 状态。先安排维护窗口并核对归档来源。

```bash
./scripts/restore.sh backups/network-atlas-TIMESTAMP.tar.gz
docker compose --env-file .env ps
./scripts/health-check.sh
```

恢复后还必须确认：

- `deploy/secrets/owner_password_hash.txt` 已从独立秘密备份恢复；
- 所有者可以登录；
- 关键读模型正常；
- 通过 UI 重试一台已登记 TARGET_HOST；
- APP_HOST 上的卷名仍为 `network-atlas-data`。

当前 `restore.sh` 只接受可信的本项目备份。它尚未对归档成员做路径穿越、危险符号链接和特殊文件检查，不要恢复来源不明或被第三方修改的归档。

## 9. 升级与回滚

```bash
./scripts/upgrade.sh
./scripts/rollback.sh
```

升级会先生成备份、标记旧镜像，再构建并启动新版本；基础健康检查失败时自动回滚。`.release-state` 是本地可信发布状态文件，不进入 Git，不要手工编辑或从其他主机复制。

升级后的人工门槛与首次部署相同：HTTPS、登录、关键读模型和真实 UI SSH 连接都通过后，才算发布完成。现有脚本的自动健康检查只覆盖 `/healthz` 和匿名业务 API 返回 `401`，不能替代这些验收。

## 10. 秘密与操作边界

- API、SSE、日志、Agent 上下文、文档、Git 和回复不得出现密码、私钥、模型 Key、会话 Token 或完整凭据。
- 删除和恢复只操作 APP_HOST 本地数据；TARGET_HOST 保持只读。
- 未经单独设计和验收，不修改 TARGET_HOST 的 SSH 配置，不执行服务器控制动作。
- `Caddyfile.local` 监听本地烟测端口并使用内部 CA，不会被正式 Compose 自动挂载，也不替代真实域名证书。

## 11. 已知部署改进项

以下属于后续脚本加固，不应被误报为当前已经解决：

- 让生成密码和恢复脚本可靠读取 `.env` 中的 `APP_IMAGE`；
- 恢复前严格校验 tar 成员、manifest 和 schema；
- 严格解析并保护 `.release-state`，避免直接执行文件内容；
- 为 Caddy 增加独立健康检查、资源限制和日志轮转；
- 固定基础镜像 digest，并扩展发布烟测到 TLS、登录和关键 API。
