# lessor

跨平台 DHCP 服务器，带 Web 和桌面两套界面。用 Rust 写的。

> **lessor** — 出租人，授予租约的一方。DHCP 服务器干的正是这件事：
> 把地址租给客户端，到期回收。

## 为什么又造一个

现有的选择在这个场景下都不合适：

| | 问题 |
|---|---|
| ISC DHCP | 2022 年 EOL，配置是自定义语法的文本文件，改配置要重载进程 |
| Kea | 成熟，但面向长期运行的基础设施，不适合"插上网线临时发个地址"的现场场景 |
| dnsmasq | 轻量好用，但只有 Unix 版 |
| [dora](https://github.com/bluecatengineering/dora) | Rust 写的，但**全仓库搜不到 `windows` 或 `target_os`**，依赖 `unix-udp-sock` 和 `pnet`，没有 Windows 支持 |

`lessor` 的定位：**三平台、单文件、有界面、配置改完立即生效**。

典型用途：

- 机房上架时给 BMC 临时发地址，改完正式 IP 就撤
- 装机网段的 PXE / UEFI HTTP Boot
- 隔离网络、实验环境里需要一个能随手起停的 DHCP

## 状态

早期开发中，**还不能用**。

- [x] `lessor-core` —— 地址池、租约状态机、报文决策
- [x] `lessord` —— tokio UDP 循环 + axum HTTP/WebSocket
- [x] `lessor-net` —— 网卡枚举与地址配置
- [x] `discovery` —— 发现已配置静态 IP 的设备
- [x] Web UI —— Svelte + Vite，打包进二进制
- [ ] 桌面端 —— Tauri 套同一份 UI

共 112 个测试。已用 busybox 的 udhcpc（docker）和 VMware 的 UEFI 固件
两种真实客户端验证过。

### 收包隔离 —— 三个平台不一样

服务进程若不能把收包限制在指定网卡上，就会应答**本机所有网卡**收到的
DHCP 请求 —— 一台同时连着生产网的笔记本会就此变成流氓 DHCP 服务器。
这不是理论风险：开发过程中它真的去应答了另一个网段上的 MAAS 服务器。

| 平台 | 做法 | 能否隔离 |
| --- | --- | --- |
| Linux | 绑 `0.0.0.0` + `SO_BINDTODEVICE` | 需要 `--iface`，否则不能 |
| Windows | **直接绑本机地址** —— 实测受限广播会被投递 | 能 |
| macOS / BSD | 只能绑 `0.0.0.0`，没有等价原语 | **不能** |

启动日志里的 `isolated` 字段如实反映当前监听器的情况；做不到隔离时会告警。

### 跑一个试试

```bash
cargo run -p lessord --   --listen 192.168.88.1 --prefix 24 --pool 192.168.88.10-192.168.88.20   --router 192.168.88.1 --dns 223.5.5.5
```

`--listen` 是本机在该网段上的地址，子网由它和 `--prefix` 推出。
绑 UDP 67 需要管理员权限；加 `--dhcp-port 6767 --client-port 6768`
可以在高位端口免特权跑，便于验证。

HTTP 接口默认只听 `127.0.0.1:8080` —— 这是管理接口，不该暴露到网络上。

```
GET    /api/state              作用域、容量、已用、监听器
GET    /api/leases             全部租约
DELETE /api/leases/{scope}/{ip}  撤销一条租约
GET    /api/events             WebSocket，实时推送每个报文的处理结果
```

界面（作用域、租约、实时日志、发现设备）由同一个进程提供，打开
`http://127.0.0.1:8080/` 即可。

### 验证

```bash
# 真实 DHCP 客户端（busybox udhcpc），跑在 docker 里
cargo zigbuild -p lessord --release --target x86_64-unknown-linux-gnu
cp target/x86_64-unknown-linux-gnu/release/lessord docker/
docker compose -f docker/compose.yml up --abort-on-container-exit --build
```

`scripts/` 下另有两个手工脚本：`fake_client.py` 走一遍完整握手，
`e2e_check.py` 连 WebSocket 并检查保留、撤销等行为。

### core 已具备

- **多作用域** —— 直连按收包网卡地址选，经中继按 `giaddr` 选；租约带 `scope_id`，
  两个隔离网段可以都用 `192.168.1.0/24` 而互不干扰
- **存储抽象** —— `LeaseStore` trait，`MemoryStore` 是内存实现，sqlite 放上层
- **配置校验** —— 池越界、区间重叠、保留冲突、网关不在子网内等一次全部列出
- **丢弃原因** —— 不应答时给出 `DropReason`，界面上能回答"为什么这台机器插上没反应"
- **`vendor_class`（选项 60）** —— 记录并识别 PXE 客户端
- 作用域启用/禁用；容量统计；`MacAddr` 序列化为 `"ac:1f:6b:8e:00:01"`

### 真实客户端暴露出来的问题

这几条只有拿真客户端打才会发现，单元测试想不到：

- **option 61 的 `01` + MAC 形式**。udhcpc、dhclient、多数 BMC 固件发的
  客户端标识是"硬件类型 1 + 6 字节 MAC"，与裸 MAC 指同一台设备。
  不归一的话，按 MAC 配的静态保留对这些客户端**全部失效**。
- **PXE 固件要求应答里带 option 60 = `PXEClient`**。没有它，固件会丢弃
  我们的 OFFER 一直重发 DISCOVER —— 现象是机器起不来，但服务端日志
  显示"已应答"，极难定位。引导文件名也要同时写进 BOOTP 的 `file` 字段，
  部分固件不看 option 67。
- **Windows 上绑 `0.0.0.0` 会跨网卡应答**（见上）。

### lessord 已具备

- 每个监听器一对 socket：收绑 `0.0.0.0:67`，**发绑本机地址** ——
  广播应答只从对应网卡出去，不会打扰别的网段
- Linux 上用 `SO_BINDTODEVICE` 把 socket 钉在网卡上，多网卡才真正干净；
  其它平台上配多个监听器时会给出告警
- WebSocket 推送每个报文的处理结果（含丢弃原因），慢客户端只丢事件、
  永远不会拖住 DHCP 主循环
- 无配置文件的快捷启动，含 `--reservation MAC=IP[=主机名]` 与
  `--boot-file` / `--next-server` / `--tftp-server`
- 后台定期回收过期租约

### lessor-net / discovery

网卡枚举是纯 Rust，三平台共用；地址配置各平台走各自的命令
（`netsh` / `ip` / `ifconfig`），都需要特权。

设备发现针对的是"已经配了静态 IP、不会来要地址"的机器，三种手段互补，
**都不需要抓包驱动或 raw socket**：

- **RMCP Presence Ping**（UDP 623）—— IPMI 自带的发现机制，最准，
  能直接确认"这是一台 BMC"
- **UDP 探测 + 邻居表** —— 往候选地址发包逼系统做 ARP，再读邻居表
- **被动读邻居表** —— 设备只要在网上说过话就会被系统记下

邻居表的解析**与语言环境无关**：不认表头不认列名，只在每行里找
"长得像 IPv4 的"和"长得像 MAC 的"。中文 Windows 的 `arp -a` 输出
带本地化文字，按列切分会直接失效。

## 架构

```
lessor-core/     纯逻辑，无 IO、无 async、不读时钟 —— 三平台共用
                   addr    MAC / 客户端标识 / 地址区间
                   scope   作用域：子网、地址池、保留、选项、校验
                   lease   租约与状态机
                   store   LeaseStore trait + 内存实现 + 分配算法
                   server  RFC 2131 报文决策
lessor-net/      平台层
                   windows: IP Helper API
                   linux:   rtnetlink + SO_BINDTODEVICE
                   macos:   ifconfig
discovery/       静态 IP 设备发现（IPv6 组播 / RMCP / 邻居表）
lessord/         tokio + axum，UDP 67 与 HTTP API 同进程
ui/              一套前端，Web 与桌面复用
```

**为什么前后端分离**：DHCP 要绑 UDP 67，必须特权运行。把浏览器内核也拉进特权进程是糟糕的做法，
所以 `lessord` 特权驻留只管网络，界面是普通权限的纯客户端，两者通过 HTTP/WebSocket 通信。

报文编解码用 [`dhcproto`](https://github.com/bluecatengineering/dhcproto)，
本项目实现的是它之上的服务端策略：分配优先级、租约生命周期、各类报文的应答决策。

## 设计上的取舍

**核心不做 IO，时间由调用方传入。** 这样每条 RFC 规则都能被确定性地测试 ——
租约过期、DECLINE 隔离、并发 OFFER 占位这些容易写错的地方，测试里都能精确构造。

**配置面向 API 而非文件。** 界面上加一个地址池或保留，立即生效，不需要写配置文件重启进程。

## 开发

```bash
cargo test        # 44 个测试
cargo clippy --all-targets
```

## 许可

MIT
