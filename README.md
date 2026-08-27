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
- 装机网段的网络引导：PXE、UEFI HTTP Boot、iPXE 链式引导
- 隔离网络、实验环境里需要一个能随手起停的 DHCP

## 状态

早期开发中，**还不能用**。

- [x] `lessor-core` —— 地址池、租约状态机、报文决策
- [x] `lessord` —— tokio UDP 循环 + axum HTTP/WebSocket
- [x] `lessor-net` —— 网卡枚举与地址配置
- [x] `discovery` —— 发现已配置静态 IP 的设备
- [x] Web UI —— Svelte + Vite，打包进二进制
- [x] 桌面端 —— Tauri 套同一份 UI

共 120 个测试。已用三种真实客户端验证过：busybox `udhcpc`（docker）、
`systemd-networkd`（Ubuntu 24.04）、VMware 的 UEFI PXE 固件 ——
最后一种一路验到 shim → GRUB 起来。

### 不需要特权

`lessord` **不需要管理员 / root**。

- **Windows** —— 直接跑。「端口小于 1024 需要特权」是 Unix 的约定，
  Windows 从来没有采纳；绑 UDP 67 普通用户就能做
- **Linux** —— 安装时给二进制设一次权限，之后普通用户运行：

  ```bash
  sudo setcap cap_net_bind_service+ep /usr/local/bin/lessord
  ```

- **容器** —— 以 root 运行容器（默认）即可，或同样用 `setcap`。
  注意 `docker run --cap-add NET_BIND_SERVICE --user 1000` **不生效** ——
  `--cap-add` 只放进 bounding set，非 root 进程的有效集里拿不到，
  必须走 file capability 或 ambient

  上面两条是实测的，不是推断：[`docker/setcaptest.sh`](docker/setcaptest.sh)
  逐个组合跑给你看，[`docker/capcheck.sh`](docker/capcheck.sh) 验证非 root 的情形。
  跑的时候要显式带上 `--sysctl net.ipv4.ip_unprivileged_port_start=1024`，
  否则 Docker Desktop 的内核把它设成 0，三种组合会全部"成功"，什么也证明不了。

  顺带测出来的一条：`SO_BINDTODEVICE` 只要 `cap_net_bind_service` 就够，
  不需要额外的 `cap_net_raw`。

整个项目里唯一需要特权的是**修改网卡地址**，它被单独拆成了
[`lessor-netcfg`](#配置网卡地址)，不在服务进程里。

### 配置网卡地址

网卡已经在目标网段上时，这一步不需要。需要的话：

```bash
lessor-netcfg list                              # 不需要特权
sudo lessor-netcfg set eth0 192.168.88.1/24     # 需要特权
lessor-netcfg restore eth0                      # 需要特权
```

权限不足时退出码是 **77**，与参数错误（2）区分开 —— 脚本可以据此决定
"换个身份重来"而不是"参数写错了"。

拆成独立程序是有意的：自动化流程里改地址是一次性的前置步骤，
DHCP 服务是长期运行的，两者的权限需求本就不同，不该绑在一起。

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
- **[引导客户端识别](docs/pxe-client-identification.md)** —— 从 option 60 / option 77
  分出 PXE 固件、UEFI HTTP Boot、iPXE 三类，各发各的引导目标；
  同一台机器在固件阶段和操作系统阶段是两条不同的记录
- 作用域启用/禁用；容量统计；`MacAddr` 序列化为 `"ac:1f:6b:8e:00:01"`

### 真实客户端暴露出来的问题

这几条只有拿真客户端打才会发现，单元测试想不到。展开的排查过程、实测数据
和复现步骤都在 [docs/](docs/)：

- **option 61 的 `01` + MAC 形式**。udhcpc、dhclient、多数 BMC 固件发的
  客户端标识是"硬件类型 1 + 6 字节 MAC"，与裸 MAC 指同一台设备。
  不归一的话，按 MAC 配的静态保留对这些客户端**全部失效**。
- **[应答的源端口必须是 67](docs/pxe-source-port.md)**。普通 DHCP 客户端
  不校验源端口，PXE 固件校验 —— 源端口不对的 OFFER 被静默丢弃，
  现象是服务端日志一行行"已应答"，客户端却一直重发 DISCOVER。
- **[option 60 与 option 43 要么都给，要么都不给](docs/pxe-option-60-and-43.md)**。
  只声明 `PXEClient` 却不给 option 43，固件会接受地址然后什么都不做，
  一个 TFTP 请求都不发。引导文件名要同时写进 BOOTP 的 `file` 字段，
  部分固件不看 option 67。
- **Windows 上绑 `0.0.0.0` 会跨网卡应答**（见上）。

后两条是拿 VMware Workstation 的 UEFI PXE 固件实测出来的，一路验证到
固件取址 → TFTP 拉 `bootx64.efi` → shim 拉 `grubx64.efi` → GRUB 起来。
三种客户端（busybox udhcpc、systemd-networkd、UEFI PXE 固件）里，
只有固件能暴露它们 —— 排查办法见 [怎么对着真固件排查 PXE](docs/debugging-pxe.md)。

### lessord 已具备

- 每个监听器**一个**收发共用的 socket，绑在服务端口上 —— 应答的源端口
  因此是 67（PXE 固件的硬性要求），广播也只从对应网卡出去
- Linux 上用 `SO_BINDTODEVICE` 把 socket 钉在网卡上，多网卡才真正干净；
  其它平台上配多个监听器时会给出告警
- WebSocket 推送每个报文的处理结果（含丢弃原因），慢客户端只丢事件、
  永远不会拖住 DHCP 主循环
- 无配置文件的快捷启动，含 `--reservation MAC=IP[=主机名]` 与
  `--boot-file` / `--next-server` / `--tftp-server`；
  另有 `--http-boot-url` 和 `--ipxe-url`，按客户端类别分别下发
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
lessor-net/      平台层：网卡枚举（纯 Rust，三平台共用）
                 + 地址配置（netsh / ip / ifconfig，唯一需要特权的地方）
                 附带 lessor-netcfg 可执行程序
discovery/       静态 IP 设备发现（IPv6 组播 / RMCP / 邻居表）
lessord/         tokio + axum，UDP 67 与 HTTP API 同进程
ui/              一套前端，Web 与桌面复用
ui/src-tauri/    桌面外壳。纯客户端，零特权 —— 加载的就是 lessord
                 提供的那份界面，因此前端只有一套代码、一套接口
```

**为什么前后端分离**：`lessord` 是长期驻留的网络进程，界面是随时开关的客户端，
两者的生命周期本来就不一样 —— 服务可以装成系统服务，也可以跑在机房另一台机器上。
分开之后桌面端只是加载同一份界面的外壳，Web 与桌面因此共用一套代码、一套接口。

报文编解码用 [`dhcproto`](https://github.com/bluecatengineering/dhcproto)，
本项目实现的是它之上的服务端策略：分配优先级、租约生命周期、各类报文的应答决策。

## 设计上的取舍

**核心不做 IO，时间由调用方传入。** 这样每条 RFC 规则都能被确定性地测试 ——
租约过期、DECLINE 隔离、并发 OFFER 占位这些容易写错的地方，测试里都能精确构造。

**配置面向 API 而非文件。** 界面上加一个地址池或保留，立即生效，不需要写配置文件重启进程。

## 开发

```bash
cargo test        # 120 个测试
cargo clippy --all-targets
```

## 许可

MIT
