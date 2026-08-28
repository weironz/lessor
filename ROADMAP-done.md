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
