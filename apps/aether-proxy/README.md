# aether-proxy

Aether Tunnel 代理节点，部署在海外 VPS 上，通过 WebSocket 隧道为 Aether 实例中转 API 流量。

Tunnel 模式下代理节点**无需对外监听端口**，仅需出站连接到 Aether 服务器。

## 安装

`aether-proxy` 会根据宿主机自动选择服务管理器：
- 常规 Linux 发行版：`systemd`
- Alpine Linux：`OpenRC`

### 下载预编译二进制

<!-- DOWNLOAD_TABLE_START -->
| Platform | Download |
|----------|----------|
| Linux x86_64 (GNU) | [aether-proxy-linux-amd64.tar.gz](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-linux-amd64.tar.gz) |
| Linux ARM64 (GNU) | [aether-proxy-linux-arm64.tar.gz](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-linux-arm64.tar.gz) |
| Linux x86_64 (musl) | [aether-proxy-linux-musl-amd64.tar.gz](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-linux-musl-amd64.tar.gz) |
| Linux ARM64 (musl) | [aether-proxy-linux-musl-arm64.tar.gz](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-linux-musl-arm64.tar.gz) |
| macOS x86_64 | [aether-proxy-macos-amd64.tar.gz](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-macos-amd64.tar.gz) |
| macOS ARM64 | [aether-proxy-macos-arm64.tar.gz](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-macos-arm64.tar.gz) |
| Windows x86_64 | [aether-proxy-windows-amd64.zip](https://github.com/fawney19/Aether/releases/download/proxy-v0.3.11/aether-proxy-windows-amd64.zip) |
<!-- DOWNLOAD_TABLE_END -->

上表展示的是最新已发布版本的下载链接。从下一次 `proxy-v*` 发布开始，表格会自动补上 `Linux x86_64 (musl)` / `Linux ARM64 (musl)` 包，供 Alpine 等 musl 系统直接使用。

## 快速开始

### 一键安装 / 添加节点

一键脚本会自动从 GitHub Releases 中筛选最新的 `proxy-v*` tag，并按当前系统下载对应制品：Linux x86_64/ARM64（GNU 或 musl）、macOS x86_64/ARM64、Windows x86_64。仓库的通用 `latest` release 可能不是 proxy 版本，因此脚本不会使用 `/releases/latest`。

脚本会安装/更新 `aether-proxy` 二进制，并把新的服务器配置追加到 `aether-proxy.toml` 的 `[[servers]]` 数组中；如果配置文件已存在，不会覆盖原有内容。检测到相同 `aether_url + node_name` 时会跳过追加。

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/fawney19/Aether/main/apps/aether-proxy/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/fawney19/Aether/main/apps/aether-proxy/install.ps1 | iex
```

也可以用环境变量非交互式执行，适合在控制台“添加代理节点”时生成命令：

```bash
curl -fsSL https://raw.githubusercontent.com/fawney19/Aether/main/apps/aether-proxy/install.sh | \
  AETHER_PROXY_AETHER_URL="https://aether.example.com" \
  AETHER_PROXY_MANAGEMENT_TOKEN="ae_xxx" \
  AETHER_PROXY_NODE_NAME="jp-proxy-01" \
  sh
```

```powershell
$env:AETHER_PROXY_AETHER_URL = "https://aether.example.com"
$env:AETHER_PROXY_MANAGEMENT_TOKEN = "ae_xxx"
$env:AETHER_PROXY_NODE_NAME = "jp-proxy-01"
irm https://raw.githubusercontent.com/fawney19/Aether/main/apps/aether-proxy/install.ps1 | iex
```

可选变量：`AETHER_PROXY_RELEASE_TAG` 固定安装某个 `proxy-v*` tag，`AETHER_PROXY_CONFIG` 指定配置文件路径，`AETHER_PROXY_INSTALL_DIR` 指定二进制安装目录。

```bash
# 1. 首次安装配置（TUI 向导，勾选 Install Service 随系统启动服务）
sudo ./aether-proxy setup

# 2. 日常管理 (勾选 Install Service 作为系统服务的情况下)
aether-proxy status          # 看状态
sudo aether-proxy logs       # 看日志

sudo aether-proxy start      # 启动服务
sudo aether-proxy stop       # 停止服务
sudo aether-proxy restart    # 重启服务

# 3. 重新配置（改完自动重启服务）
sudo aether-proxy setup

# 4. 彻底卸载
sudo aether-proxy uninstall
```

完成向导后, 配置自动保存到 `aether-proxy.toml`，如果启用了 Install Service，将自动注册并启动当前系统支持的服务（`systemd` 或 `OpenRC`）。

### 直接运行

如果不需要安装为系统服务，可以直接运行。缺少必填参数时会自动进入 setup 向导：

```bash
./aether-proxy
```

## 配置

配置按以下优先级加载（高优先级覆盖低优先级）：

1. CLI 参数
2. 环境变量（`AETHER_PROXY_*`）
3. 配置文件（`aether-proxy.toml`，或通过 `AETHER_PROXY_CONFIG` 指定路径）

### 参数一览

#### 基础配置

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--aether-url` | `AETHER_PROXY_AETHER_URL` | **必填** | Aether 服务器地址 |
| `--management-token` | `AETHER_PROXY_MANAGEMENT_TOKEN` | **必填** | 管理员 Token（`ae_xxx` 格式） |
| `--node-name` | `AETHER_PROXY_NODE_NAME` | **必填** | 节点名称标识 |
| `--public-ip` | `AETHER_PROXY_PUBLIC_IP` | 自动检测 | 公网 IP |
| `--node-region` | `AETHER_PROXY_NODE_REGION` | 自动检测 | 地区标识 |
| `--heartbeat-interval` | `AETHER_PROXY_HEARTBEAT_INTERVAL` | `5` | 心跳间隔（秒） |
| `--allowed-ports` | `AETHER_PROXY_ALLOWED_PORTS` | `80,443,8080,8443` | 允许代理的目标端口 |
| `--allow-private-targets` | `AETHER_PROXY_ALLOW_PRIVATE_TARGETS` | `true` | 允许 private/reserved 目标地址，通过后仍受 `allowed_ports` 限制；设为 `false` 可恢复严格拦截 |

#### Tunnel 连接

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--tunnel-connections` | `AETHER_PROXY_TUNNEL_CONNECTIONS` | 自动（硬件估算） | 最小连接池大小；显式设置后默认固定为该值 |
| `--tunnel-connections-max` | `AETHER_PROXY_TUNNEL_CONNECTIONS_MAX` | 自动（硬件估算） | 连接池自动扩容上限；大于 `tunnel_connections` 时启用 autoscale |
| `--tunnel-max-streams` | `AETHER_PROXY_TUNNEL_MAX_STREAMS` | 自动（硬件估算） | 单连接最大并发 stream 数 |
| `--tunnel-ping-interval-ms` | `AETHER_PROXY_TUNNEL_PING_INTERVAL_MS` | `10000` | WebSocket ping 周期（毫秒） |
| `--tunnel-connect-timeout-ms` | `AETHER_PROXY_TUNNEL_CONNECT_TIMEOUT_MS` | `3000` | tunnel 建连超时（毫秒） |
| `--tunnel-stale-timeout-ms` | `AETHER_PROXY_TUNNEL_STALE_TIMEOUT_MS` | `30000` | 无入站数据断连阈值（毫秒） |
| `--tunnel-scale-check-interval-ms` | `AETHER_PROXY_TUNNEL_SCALE_CHECK_INTERVAL_MS` | `1000` | autoscale 采样周期（毫秒） |
| `--tunnel-scale-up-threshold-percent` | `AETHER_PROXY_TUNNEL_SCALE_UP_THRESHOLD_PERCENT` | `50` | 单 tunnel 占用率超过该值时扩容 |
| `--tunnel-scale-down-threshold-percent` | `AETHER_PROXY_TUNNEL_SCALE_DOWN_THRESHOLD_PERCENT` | `35` | 单 tunnel 占用率持续低于该值时允许缩容 |
| `--tunnel-scale-down-grace-secs` | `AETHER_PROXY_TUNNEL_SCALE_DOWN_GRACE_SECS` | `15` | 低负载持续时间达到该值后才回收次级 tunnel |
| `--tunnel-tcp-keepalive-secs` | `AETHER_PROXY_TUNNEL_TCP_KEEPALIVE_SECS` | `30` | TCP keepalive 初始延迟（秒） |
| `--tunnel-tcp-nodelay` | `AETHER_PROXY_TUNNEL_TCP_NODELAY` | `true` | 禁用 Nagle 算法 |
| `--tunnel-reconnect-base-ms` | `AETHER_PROXY_TUNNEL_RECONNECT_BASE_MS` | `50` | 指数退避基础延迟（毫秒） |
| `--tunnel-reconnect-max-ms` | `AETHER_PROXY_TUNNEL_RECONNECT_MAX_MS` | `250` | 指数退避上限（毫秒） |

省略 `tunnel_connections` 时，proxy 会按设备能力自动计算一个基线值和偏单机上限的扩容上限：默认至少保留 2 条常驻 tunnel，并会更早触发扩容；如果显式设置了 `tunnel_connections` 但没有设置 `tunnel_connections_max`，则保持固定连接池，不自动扩缩。

#### 上游 HTTP 请求

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--upstream-connect-timeout-secs` | `AETHER_PROXY_UPSTREAM_CONNECT_TIMEOUT_SECS` | `30` | 上游建连超时（秒） |
| `--upstream-pool-max-idle-per-host` | `AETHER_PROXY_UPSTREAM_POOL_MAX_IDLE_PER_HOST` | `64` | 每 Host 最大空闲连接数 |
| `--upstream-pool-idle-timeout-secs` | `AETHER_PROXY_UPSTREAM_POOL_IDLE_TIMEOUT_SECS` | `300` | 连接池空闲超时（秒） |
| `--upstream-tcp-keepalive-secs` | `AETHER_PROXY_UPSTREAM_TCP_KEEPALIVE_SECS` | `60` | TCP keepalive（秒，0 关闭） |
| `--upstream-tcp-nodelay` | `AETHER_PROXY_UPSTREAM_TCP_NODELAY` | `true` | 启用 TCP_NODELAY |
| `--upstream-proxy-url` | `AETHER_PROXY_UPSTREAM_PROXY_URL` | 空 | 仅 provider 上游请求使用的出口代理 |
| `--redirect-replay-budget-bytes` | `AETHER_PROXY_REDIRECT_REPLAY_BUDGET_BYTES` | `5M` | 307/308 请求体重放的预读预算，支持 `K/M/G`，`0` 表示禁用 body replay buffering |

出口代理支持 `http://`、`socks5://`、`socks5h://`。配合 WARP sidecar 时可填写：

```toml
upstream_proxy_url = "socks5h://microwarp:1080"
```

如果需要让 Aether 管理 API 和 WebSocket tunnel 也走代理，使用 `aether_proxy_url`。

#### Aether API 客户端

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--aether-request-timeout-secs` | `AETHER_PROXY_AETHER_REQUEST_TIMEOUT_SECS` | `10` | 请求总超时（秒） |
| `--aether-connect-timeout-secs` | `AETHER_PROXY_AETHER_CONNECT_TIMEOUT_SECS` | `10` | 建连超时（秒） |
| `--aether-proxy-url` | `AETHER_PROXY_AETHER_PROXY_URL` | 空 | Aether 注册、心跳和 WebSocket tunnel 回连使用的出口代理（默认不走代理） |
| `--aether-retry-max-attempts` | `AETHER_PROXY_AETHER_RETRY_MAX_ATTEMPTS` | `3` | 最大重试次数 |

#### DNS 与安全

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--allow-private-targets` | `AETHER_PROXY_ALLOW_PRIVATE_TARGETS` | `true` | 默认允许 private/reserved 目标地址；设为 `false` 可恢复拦截，且仅影响重启后的进程 |
| `--dns-cache-ttl-secs` | `AETHER_PROXY_DNS_CACHE_TTL_SECS` | `60` | DNS 缓存 TTL（秒） |
| `--dns-cache-capacity` | `AETHER_PROXY_DNS_CACHE_CAPACITY` | `1024` | DNS 缓存容量（条目数） |

#### 日志

| 参数 | 环境变量 | 默认值 | 说明 |
|------|----------|--------|------|
| `--log-level` | `AETHER_PROXY_LOG_LEVEL` | `info` | 日志级别 |
| `--log-destination` | `AETHER_PROXY_LOG_DESTINATION` | `both` | 输出到 `stdout`、文件或两者同时输出 |
| `--log-dir` | `AETHER_PROXY_LOG_DIR` | `logs` | 文件日志目录，`file/both` 时必填 |
| `--log-rotation` | `AETHER_PROXY_LOG_ROTATION` | `daily` | 文件日志按小时或按天轮转 |
| `--log-retention-days` | `AETHER_PROXY_LOG_RETENTION_DAYS` | `7` | 文件日志保留天数 |
| `--log-max-files` | `AETHER_PROXY_LOG_MAX_FILES` | `30` | 文件日志最多保留文件数 |

### 日志落点

- 默认 `AETHER_PROXY_LOG_DESTINATION=both`，同时输出到 stdout 和 `logs/` 文件目录
- 需要只交给容器日志驱动或宿主机服务管理器时，可改成 `stdout`；setup TUI 里可用 `Save Logs to File` 开关关闭文件日志
- 文件日志固定写普通文本，并支持 `hourly/daily` 轮转；默认按天轮换、保留 7 天，最多保留 30 个文件
- 以 `systemd` 或 `OpenRC` 安装时默认会额外打开文件日志到 `/var/log/aether-proxy`
- OpenRC 安装时，`aether-proxy logs` 实际读取 `/var/log/aether-proxy/current.log` 和 `/var/log/aether-proxy/error.log`；这些文件通常需要用 `sudo aether-proxy logs` 查看

### 隧道健康上报（Heartbeat）

proxy 会在心跳 `proxy_metadata` 中主动上报隧道稳定性指标，便于后端直接入库/告警：

- `proxy_metadata.tunnel_metrics`：建连尝试/成功/失败、断开次数、累计在线时长、心跳 RTT、WebSocket 收发帧与字节等。
- `proxy_metadata.recent_tunnel_errors`：最近隧道异常事件（时间戳、类别、错误摘要，环形缓冲）。

说明：仅主连接（`conn=0`）发送 heartbeat，避免多条 tunnel 重复上报同一份全局指标。

### 多服务器配置

在 `aether-proxy.toml` 中使用 `[[servers]]` 配置 Aether 服务器。即使只有一个服务器，也必须写成一个 `[[servers]]` 条目；旧的顶层单服务器写法已不再支持。

```toml
[[servers]]
aether_url = "https://aether-1.example.com"
management_token = "ae_xxx"
node_name = "jp-proxy-01"

[[servers]]
aether_url = "https://aether-2.example.com"
management_token = "ae_yyy"
node_name = "jp-proxy-02"
```

## 发布新版本

推送 `proxy-v*` 格式的 tag，GitHub Actions 会自动：
- 编译所有平台二进制并发布到 Releases
- 更新 README 中的下载链接表格

```bash
git tag proxy-v0.2.0
git push origin proxy-v0.2.0
```
