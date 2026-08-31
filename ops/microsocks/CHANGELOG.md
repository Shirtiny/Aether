# Aether MicroSocks 变更说明

## `aether-microsocks-1.0.5-aether1` — 2026-08-31

### 为什么必须使用这个版本

`netcup-ipv6` 使用 Debian MicroSocks `1.0.5-1` 提供 SOCKS5 服务。原版把
每个 SOCKS5 协议阶段交给单次 `recv()` 解析，错误地假设一次客户端写入会
对应一次完整服务端读取。

Codex WebSocket 客户端会把问候包分两次写入：

```text
05 01
00
```

当 MicroSocks 第一次只收到 `05 01` 时，会在认证方法字节尚未到达前返回
`05 ff`。Aether 日志中的典型特征是：

```text
error_type=codex_ws_handshake_proxy_connect_error
proxy_connect_reason=no_acceptable_auth_method
```

这个故障发生在代理隧道建立以及连接 ChatGPT 之前，不是账号认证错误，也
不是 Netcup IPv6 路由中断。其他代理实现会按字段长度继续读取，因此没有
触发同一问题。

**不要直接把服务改回 `/usr/bin/microsocks`，否则会重新引入该故障。**

### 修改内容

补丁文件：

```text
patches/microsocks-1.0.5-aether-framing.patch
```

主要修改：

- 新增 `recv_full()`，循环读取指定长度；
- 完整读取认证方法问候包；
- 完整读取用户名/密码认证包；
- 按 IPv4、IPv6、域名三种地址类型完整读取 CONNECT 请求；
- 新增 `send_full()`，处理小型 SOCKS5 响应的部分写入；
- 保留原有无认证模式、IPv6 出口绑定和线程模型。

### 版本与校验和

基础版本：

```text
Debian microsocks 1.0.5-1
```

原版系统二进制：

```text
02ba0e0ceadbbceccf2dfa287bab4dfb635cecc94503703a4494b0387ce9357d  /usr/bin/microsocks
```

当前仓库修复版：

```text
087fdf19221feaee85252dcb169ce6743ab255ed4762a4258f7f71f798657b87  bin/linux-amd64/aether-microsocks-1.0.5-aether1
```

源码补丁：

```text
d838300ce368c3f9fd15d5859af571aec4a5eabf8ec56f32692188ed4a2efae0  patches/microsocks-1.0.5-aether-framing.patch
```

### 上线前后验证

- 原版拆分问候返回 `05 ff`，合并问候返回 `05 00`；
- 修复版在 0、0.1、1、5、20 毫秒分片延迟下 250/250 返回 `05 00`；
- 50 个顺序及 500 个并发分片 CONNECT 测试通过；
- 40 个分片用户名/密码认证测试通过；
- Netcup IPv6 出口验证通过；
- 切换后真实流量中未再观察到 `no_acceptable_auth_method`。

测试没有调用 ChatGPT，也没有使用账号凭据。

### 未来升级或替换的验收条件

以后可以升级 MicroSocks，但不能只依据版本号判断是否安全。候选版本必须：

1. 不依赖单次 `recv()` 获得完整 SOCKS5 帧；
2. 通过 `tests/framing_regression.py`；
3. 验证指定 IPv6 源地址绑定和实际出口；
4. 更新本文件中的版本、二进制哈希和补丁状态；
5. 获得线上更新授权后再重启服务，并复查 Aether 日志。

### 运行位置

systemd 直接执行仓库里的固定文件：

```text
/opt/stacks/aether/ops/microsocks/bin/linux-amd64/aether-microsocks-1.0.5-aether1
```

宿主机专属的监听地址、端口和出口 IPv6 保存在：

```text
/etc/default/aether-ipv6-proxy
```

换宿主机和启动步骤见 `README.md`。
