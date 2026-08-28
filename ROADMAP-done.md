# 路线图 · 已完成

从 [ROADMAP.md](ROADMAP.md) 移入，带完成日期与验收证据。

## M0 · 协议核心 —— 2026-08-27

纯函数决策层（无 IO / 无 async / 不读时钟）、地址池与租约状态机、
option 61 归一化（`01+MAC` 形式不归一会让按 MAC 的静态保留全部失效）、
`DropReason`、配置校验、`LeaseStore` trait。

**证据**：42 条协议行为测试（构造报文与时间直接调 `handle()`）；
busybox udhcpc 与 systemd-networkd 真实握手通过，后者暴露的 DUID 形式
option 61 已处理并文档化。

## M1 · 三平台收发隔离与特权模型 —— 2026-08-27

单 socket 收发（应答源端口必须是 67）；Linux `SO_BINDTODEVICE`、
Windows 绑具体地址（实测受限广播会投递）、macOS 如实告警不能隔离；
lessord 全平台免特权，唯一特权操作拆成 `lessor-netcfg`（权限不足退出码 77）。

**证据**：回归测试 `replies_come_from_the_server_port`、`one_socket_per_listener`；
`docker/setcaptest.sh` 逐组合实证 capability 模型
（`--cap-add` + `--user` 不生效、`SO_BINDTODEVICE` 只需 `cap_net_bind_service`）。

## M2 · 网络引导三客户端 —— 2026-08-27

按 option 60/77 分四类各发各的引导目标；iPXE 先于 option 60 判定（防无限自举）；
option 60/43 成对下发；BOOTP file 字段 128 字节上限处理（超长只发 option 67，
修掉 dhcproto `set_fname_str` 的 panic）。

**证据**：真固件全链路 —— PXE：DHCP → TFTP → shim → GRUB 提示符；
HTTP Boot（`networkBootProtocol="httpv4"`）：`GET /bootx64.efi 200` → 执行；
iPXE 2.0.0+：固件拉 `ipxe.efi` → 取 HTTP 脚本执行。三个只有真固件才能暴露的
bug（源端口、60/43、option 61）各有复盘文档于 `docs/`。

## M3 · 一体化 UI、桌面端 attach-first、发布流水线 —— 2026-08-28

Svelte 前端经 rust-embed 打进 lessord（gzip 19 KB）；桌面端 attach-first
（已在跑只当客户端；没在跑拉起 sidecar，正常关窗显式回收 + Windows Job Object
内核兜底强杀场景）；CI + Release 流水线（版本一致性校验、产物齐全性校验）。

**证据**：v0.0.1 / v0.0.2 已发布 —— NSIS 安装程序（含 sidecar）、双平台 CLI
压缩包、镜像推 Docker Hub 与阿里云 ACR；干净 Win11 静默安装后用 CDP 驱动
真实 GUI 走通"选网卡 → 启动 → 进界面"，attach / 关窗回收 / 强杀回收
三条路径实测；129 个测试，clippy / fmt 全绿。

## M4 · 运行时配置与认证 —— 2026-08-28

作用域全生命周期（建 / 改 / 删 / 启停）与静态保留增删，全部运行时生效不重启；
建作用域时自动在对应网卡起监听器；`--serve-empty` 零作用域启动；
`--open` 启动即开浏览器。

**认证与守卫**（安全红线第 6 条）：写操作校验 `Host` 必须是 IP 字面量
（防 DNS rebinding）；配了 `--token` / `LESSOR_TOKEN` 时写操作必须带
`Authorization: Bearer`。只读接口不设防是有意的 —— 默认只听 127.0.0.1。

**证据**：HTTP 层实测 401（无 token）/ 403（Host 为域名）/ 201（正常建）/
200（只读免 token）；非法改动（池跑到网段外）被拒且作用域完好 ——
改动在副本上验证通过才落回。真实 GUI 用 CDP 走通完整生命周期：
选网卡建作用域 → 加保留 → 禁用 → 启用 → 删除 → 回到空状态。
删作用域连带清租约与客户端索引，两条回归测试钉住（133 个测试）。

## M5 · 租约持久化 —— 2026-08-28

`--lease-db` 指向一个 sqlite 文件即可持久化（rusqlite bundled，自带 SQLite
源码编译，不依赖目标机器的系统库）；不给则仍是内存，现场形态关了不留痕。

**最重要的不是 sqlite 本身，是分配语义**：`LeaseStore` 新增 `try_claim`
原子占位（能用就占下、已被占就返回 false），`allocate` 改为"挑中即占，
占不到换下一个候选"。原来的"先判断再写入"在共享存储的多实例部署里存在
窗口 —— 两个实例会同时判定同一地址可用、各自发出去。sqlite 后端把条件
下推到 SQL 的 `WHERE`，判断与写入是一条语句。这是 v1.0 共享 PostgreSQL
多实例 HA 的前提，也是唯一事后追改会很贵的点。

`LeaseStore` 的查询从返回 `&Lease` 改为返回 `Lease` —— sqlite/PG 每次查询
都产生新值，没有可借出去的东西；克隆的代价远低于让后端实现不出来。

**证据**：kill -9 后重启，日志报 `leases=1`，同客户端 REQUEST 拿回原
`192.168.233.100` 且全程无 NAK；损坏的库文件启动时明确报错拒绝且文件
未被清空；不给 `--lease-db` 时不产生任何落盘。137 个测试，其中四条钉住
原子占位契约（他人持有占不下且不动原租约、同客户端续租刷新、过期可接手、
分配跳过占不下的候选）。

## M6 · 常驻化 —— 2026-08-28

`--install-service` / `--uninstall-service` 一条命令注册成 systemd 单元或
Windows 服务（开机自启、崩溃自拉起，重启退避 5s/10s/30s）；`--config`
成为一等公民 —— **界面上的每次改动都写回配置文件**，否则常驻服务重启后
界面改的东西就没了；`/metrics` 暴露 Prometheus 文本格式指标。

配置写回用"先写临时文件再原子改名"：直接覆盖的话，写到一半掉电会留下
半个 JSON，下次启动直接起不来。

**证据**：起一个带 `--config` + `--lease-db` 的实例，通过 API 建作用域后
配置文件立刻出现对应的 listeners 与 scopes；握手后 `/metrics` 的
packets/offers/acks 与 scope_used 全部随之变化；kill -9 重启后作用域、
监听器、租约三者一并恢复（`载入作用域 scope=lab` + `leases=1`）。
非管理员身份注册服务给出可操作的报错（"注册系统服务需要管理员权限…
注意 lessord 本身跑起来不需要管理员"）且系统里不留半个服务；
两条测试钉住中英文权限识别。139 个测试。

**未验收项**：周级长稳（soak）按定义要跑几周日历时间，留到有常驻环境时补。
