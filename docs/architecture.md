# 架构

跨平台 DHCP 服务器。**前后端一体**：一个二进制里装着协议引擎、HTTP 接口和 Web 界面，
三个平台同一套代码，普通权限就能跑。

| | |
| --- | --- |
| 语言 | Rust 2024（`rust-version = 1.85`） |
| 构成 | 4 个 crate + 桌面外壳 + 前端，核心约 4.7k 行 |
| 测试 | 129 个，另有 5 种真实客户端验证 |

## 整体形状

一句话：**协议逻辑是纯函数，IO 全在外圈。**

DHCP 的难点不在收发包，而在状态机 —— 租约什么时候算过期、并发 DISCOVER 要不要占位、
收到 DECLINE 之后地址隔离多久。这些规则一旦和 socket、时钟、日志缠在一起，就没法确定性地测。

所以 `lessor-core` 不碰 IO、不用 async、不读时钟：时间由调用方传进来，租约存储是一个 trait。
每条 RFC 2131 规则都能单独构造场景验证，而不是靠"起个服务发个包看看"。

三条贯穿全局的取舍：

1. **不需要特权。** 整个项目唯一需要提权的操作（改网卡地址）被物理拆成了另一个程序。
2. **配置面向接口，不面向文件。** 界面上加个地址池立即生效，不重载进程。
3. **平台差异显式建模。** 三个平台收包绑定方式不同，这不是风格问题而是行为问题，
   写在类型和启动日志里。

## 分层

依赖单向：外圈依赖内圈，内圈对外圈一无所知。

```
┌─ ui/src-tauri ────────────── 桌面外壳（Tauri 2）
│    界面容器。先连已有服务；连不上才拉起自带的 lessord 作为子进程
└──────────────┬──────────────
               │ HTTP / WebSocket
┌─ ui/ ────────┴────────────── 前端（Svelte 5 · Vite 6 · 663 行）
│    作用域 / 租约 / 实时日志 / 设备发现。接口地址全是相对路径
└──────────────┬──────────────
               │ 构建产物由 rust-embed 打进二进制
┌─ crates/lessord ─┴────────── 服务进程（tokio · axum · 1304 行）
│    UDP 67 收发循环与 HTTP 接口同进程。socket / 时钟 / 日志 / 事件广播
│    dhcp · api · state · config · ui · main
└──────────────┬──────────────
               │ 传入报文 + 当前时间
┌─ crates/lessor-core ─┴────── 协议核心（无 IO · 无 async · 2052 行）
│    地址池、租约状态机、报文决策。三平台共用
│    addr · scope · lease · store · server
└─────────────────────────────

┌─ crates/lessor-net ───────── 平台层（704 行）
│    网卡枚举纯 Rust 三平台共用；地址配置各走各的系统命令
│    附带 lessor-netcfg 可执行程序
└─────────────────────────────
┌─ crates/discovery ────────── 设备发现（682 行）
│    找已经配了静态 IP、不会来要地址的机器
│    rmcp · neighbor · lib
└─────────────────────────────
```

**为什么核心不做 IO**：租约过期、DECLINE 隔离、并发 OFFER 占位这些最容易写错的地方，
测试里都能精确构造 —— 传一个 `now=100`，再传一个 `now=3700`，不用等一小时。

## 一个报文的旅程

一台 BMC 插上网线之后，从广播到拿到地址：

1. **收包**（`lessord::dhcp`）—— 每个监听器一个 tokio 任务，持有**一个**收发共用的 socket。
2. **解码**（`dhcproto`）—— 解不出来就记 debug 日志丢弃。网络上什么都有，
   不能因为一个畸形包退出循环。
3. **选作用域**（`ServerConfig::select_scope`）—— 直连按收包网卡的本机地址选，
   经中继按 `giaddr` 选。两个隔离网段都用 `192.168.1.0/24` 也不会串。
4. **决策**（`lessor_core::handle`）—— 纯函数。返回 `Reply`（回什么、发到哪、哪个作用域分配的）、
   `Handled`（处理了但不用回，如 RELEASE）或 `Drop`（附带原因）。
5. **落库与广播**（`lessord::state`）—— 租约写进 `LeaseStore`，同时把处理结果推给所有
   WebSocket 订阅者。慢客户端只丢事件，永远不拖住 DHCP 主循环。
6. **发包**（同一个 socket）—— 源端口必须是 67，见 [pxe-source-port.md](pxe-source-port.md)。

不应答时的 `DropReason` 会一路带到界面上 ——"这台机器插上了却没反应"能直接回答是池满了、
作用域禁用了，还是它选了别的服务器。

## 技术选型

| 位置 | 选了什么 | 为什么 |
| --- | --- | --- |
| 报文编解码 | `dhcproto 0.14` | 只用它做字节层。分配优先级、租约生命周期、应答决策是本项目自己实现的 |
| 异步运行时 | `tokio 1` | UDP 循环、HTTP 接口、租约回收三个任务同进程 |
| HTTP / WS | `axum 0.8` | 与 tokio 同栈，WebSocket 直接可用 |
| socket 控制 | `socket2 0.5` | 需要 `SO_REUSEADDR`、`SO_BROADCAST`、`SO_BINDTODEVICE`，标准库给不了 |
| 网卡枚举 | `network-interface 2` | 三平台统一，纯 Rust，不用为此引入 C 依赖 |
| 界面嵌入 | `rust-embed 8` | 部署时只有一个可执行文件，没有"忘了拷 static 目录"这种事 |
| 前端 | Svelte 5 · Vite 6 | runes 够用且产物小 —— gzip 后 19 KB，要打进二进制的东西不能大 |
| 桌面 | Tauri 2 | 系统 webview，不打包浏览器内核；外壳只有百来行 |
| 前端工具链 | bun 1.4 | 装依赖和构建都快，CI 里少一层 node + 包管理器的组合 |
| 错误 | `thiserror` / `anyhow` | 库里用前者给出可判别的变体，二进制里用后者带上下文 |

### 没有选的

没有基于 [dora](https://github.com/bluecatengineering/dora) 改 —— 它全仓库搜不到 `windows`
或 `target_os`，依赖 `unix-udp-sock` 和 `pnet`，没有 Windows 支持，而三平台正是本项目存在的理由。

没有用数据库。`LeaseStore` 是个 trait，当前实现是内存版；要持久化就在上层加一个 sqlite 实现，
核心不用动。

没有做前后端分离。界面是服务的一部分，跟着服务走 —— 见下。

## 前后端一体

前端构建产物由 `rust-embed` 打进 `lessord`，**运行时只有一个进程**。

这么定的理由：lessor 的典型场景是"机房上架、插上网线、临时发个地址"，
现场要的是一个文件拷过去就能跑，不是一套编排。多一个进程就多一份配置、
多一个端口、多一种"两边版本不一致"的故障模式。

代价也说清楚：界面和服务不能各自伸缩，也不能只升级界面。
对这个体量的工具来说，这个代价比分离的复杂度便宜。

**构建顺序因此是固定的**：`ui/dist` 必须先存在，否则 `lessord` 编译不过 ——
rust-embed 在编译期就要读它，报的是 `folder ... does not exist`。

```bash
cd ui && bun install && bun run build   # 先这个
cargo build --release                   # 再这个
```

桌面端是同一份界面的外壳，不重复实现任何东西。它的启动策略是
**先连、连不上才自带拉起**（attach-first）：

1. 启动先探测 lessord。**已经在跑就只当客户端**（装成了系统服务、
   或跑在机房另一台机器上，`LESSOR_URL` 可指过去）—— 标准服务器场景
   完全不受影响，桌面端不碰别人的进程。
2. 没在跑 → 回退页给一个"启动本机实例"的表单，选网卡后把随包自带的
   lessord 作为子进程拉起。

所以"自包含"不是架构层面的：架构上服务始终独立，桌面端只是多了一个便利的
启动器。拉起的实例明确是**临时的现场实例**，不冒充系统服务 —— 这和当前
"租约只在内存里"的边界也是一致的。

子进程的清理有两道保险，因为留下一个没人管、还在发地址的 DHCP 服务器
比服务没起来更麻烦：

| 退出方式 | 靠什么清理 |
| --- | --- |
| 正常关窗 | `RunEvent::ExitRequested` / `Exit` 里显式带走 |
| 强杀外壳（任务管理器、崩溃） | Windows Job Object 的 `KILL_ON_JOB_CLOSE`，由内核保证 |

只监听 `Exit` 是不够的 —— 关掉最后一个窗口时先到的是 `ExitRequested`，
实测那样会残留。两条都验过。

### 接口

```
GET    /api/state                   作用域、容量、已用、监听器
GET    /api/leases                  全部租约
DELETE /api/leases/{scope_id}/{ip}  撤销一条租约
GET    /api/interfaces              本机网卡
POST   /api/discover                扫描静态 IP 设备
GET    /api/events                  WebSocket，逐报文推送处理结果
GET    /healthz
```

默认只听 `127.0.0.1:8080` —— 这是管理接口，不该暴露到网络上。

## 平台差异

全项目最不能"抽象掉"的地方。服务进程若不能把收包限制在指定网卡上，
就会应答**本机所有网卡**收到的 DHCP 请求 —— 一台同时连着生产网的笔记本
会就此变成流氓 DHCP 服务器。这不是理论风险：开发过程中它真的去应答了
另一个网段上的 MAAS 服务器。

| 平台 | 收包绑定 | 能否隔离 | 地址配置 |
| --- | --- | --- | --- |
| Linux | `0.0.0.0:67` + `SO_BINDTODEVICE` | 要给 `--iface`，否则不能 | `ip` |
| Windows | 直接绑本机地址 `:67` | 能 —— 实测受限广播会被投递 | `netsh` |
| macOS / BSD | 只能 `0.0.0.0:67` | **不能**，没有等价原语 | `ifconfig` |

启动日志里的 `isolated` 字段如实反映当前监听器的情况，做不到隔离时告警。
这个判断**不能只看平台** —— Linux 上没给网卡名时同样做不到。

**收发必须共用一个 socket。** 拆成两个都绑 67 的话，Linux 上单播包会被投给绑了具体地址的
那一个；收包 socket 绑的是通配地址，于是 RENEWING 阶段的续租请求会落到只写不读的那个
socket 上，静默丢失。

## 特权边界

`lessord` **不需要管理员 / root**，这是设计出来的，不是碰巧。

| 平台 | 怎么做到 |
| --- | --- |
| Windows | 直接跑。「端口小于 1024 需要特权」是 Unix 的约定，Windows 从来没有采纳 |
| Linux | 安装时 `setcap cap_net_bind_service+ep` 一次，之后普通用户运行 |
| 容器 | 以 root 运行容器即可，或同样用 `setcap` |

整个项目唯一需要特权的是**修改网卡地址**，拆成独立的 `lessor-netcfg`：
自动化流程里改地址是一次性前置步骤，DHCP 服务是长期运行的，两者的权限需求本就不同。
权限不足时退出码是 **77**，与参数错误（2）分开 —— 脚本据此决定"换个身份重来"
而不是"参数写错了"。

容器里有个陷阱：`docker run --cap-add NET_BIND_SERVICE --user 1000` **不生效**。
`--cap-add` 只放进 bounding set，非 root 进程的有效集里拿不到，必须走 file capability
或 ambient。依据见 [`docker/setcaptest.sh`](../docker/setcaptest.sh)。

## 网络引导

装机网段是主要用途之一。从 option 60 和 option 77 把客户端分成四类，各发各的引导目标：

| 类别 | 判据 | 发什么 |
| --- | --- | --- |
| `Ipxe` | option 77 为 `iPXE` | `--ipxe-url` 引导脚本 |
| `Pxe` | option 60 以 `PXEClient` 开头 | `--boot-file` TFTP 文件名 |
| `HttpBoot` | option 60 以 `HTTPClient` 开头 | `--http-boot-url` 完整 URL |
| `Plain` | 都没有 | `--boot-file` |

**判定顺序不能反** —— iPXE 同时带着两个身份，只按 option 60 判会导致无限自举。
细节与实测数据见 [引导客户端识别](pxe-client-identification.md)。

## 测试策略

129 个测试分三档，覆盖三种不同的失败方式。

| 档次 | 怎么测 | 能抓到什么 |
| --- | --- | --- |
| 协议行为（42 条） | 构造报文与时间，直接调 `handle()` | RFC 2131 的每条规则：NAK 条件、占位、隔离、续租、中继 |
| 单元（其余） | 各 crate 内部 | 地址运算、配置校验、socket 绑定端口、CJK 对齐 |
| 真实客户端 | docker + 虚拟机，走真的线 | 规范之外的怪癖 |

五种真实客户端：

| 客户端 | 验到哪一步 |
| --- | --- |
| busybox `udhcpc` | 完整握手，Linux `SO_BINDTODEVICE` 隔离路径 |
| `systemd-networkd` | 完整握手，暴露了 DUID 形式的 option 61 |
| VMware UEFI 固件（PXE） | DHCP → TFTP → shim → GRUB 提示符 |
| VMware UEFI 固件（HTTP Boot） | DHCP → HTTP `GET 200` → 执行 |
| iPXE 2.0.0+ | 链式引导，取到 HTTP 上的脚本并执行 |

这一档不是锦上添花。**三个真 bug 只有它能抓到**：option 61 的 `01+MAC` 形式让按 MAC
配的保留全部失效；应答源端口不是 67 被 PXE 固件静默丢弃；声明 option 60 却不给 option 43
让固件拿到地址后不引导。三条在单元测试里全是绿的。排查过程见
[怎么对着真固件排查 PXE](debugging-pxe.md)。

## 边界与欠账

| 项 | 状态 |
| --- | --- |
| 租约持久化 | 只有内存实现。`LeaseStore` trait 留好了位置，sqlite 未做 |
| DHCPv6 | 未做，当前只处理 IPv4 |
| 按架构分发引导文件 | 未做。需要读 option 93，目前只匹配 option 60 的字符串前缀 |
| iPXE 的 option 77 路由 | 只认 `iPXE`，不支持自定义 user class 分流 |
| WebSocket 事件 | 不带 `vendor_class`，实时日志里看不到设备类型 |
| macOS 收包隔离 | 做不到，启动时告警。这是平台限制，不是欠账 |
| 桌面端自带实例 | 仅 Windows 验证过强杀兜底（Job Object）；Linux/macOS 上强杀外壳会残留子进程 |
| 成熟度 | 早期开发中。协议路径已被真实客户端验证，但没有生产环境运行记录 |
