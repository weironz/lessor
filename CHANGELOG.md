# Changelog

本文件按版本记录变化，面向使用者，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

分工：这里写"某个版本变了什么"；里程碑级的验收证据（怎么证明做完了）在
[ROADMAP-done.md](ROADMAP-done.md)；未来计划在 [ROADMAP.md](ROADMAP.md)。

## [Unreleased]

### Added

- 界面上新建作用域（`POST /api/scopes`），建完自动在对应网卡起监听器 ——
  不必带 `--listen/--pool` 启动，服务先跑起来、配置在应用里做
- `--serve-empty`：不带作用域启动（零作用域时不应答任何 DHCP 请求）
- 写操作守卫：`Host` 必须是 IP 字面量（防 DNS rebinding）；
  配了 `--token`（或 `LESSOR_TOKEN`）时写操作需带 `Authorization: Bearer`
- 作用域改名 / 改池 / 启停 / 删除（`PATCH`、`DELETE /api/scopes/{id}`），
  删除时连带清掉该作用域的租约
- 静态保留增删（`/api/scopes/{id}/reservations`）—— 现场把 BMC 钉死到
  规划地址靠它；加保留时会顶掉压在该地址上的他人动态租约
- `--open`：启动后自动打开浏览器指向本机控制台
- `--lease-db <路径>`：租约落到 sqlite 文件，重启不丢。不给则仍只在内存里
  （现场临时用正合适）。损坏的库文件会明确报错拒绝，不会被静默清空
- `--install-service` / `--uninstall-service`：注册成 systemd 单元或 Windows
  服务，开机自启、崩溃自拉起。非特权身份下给出可操作的报错
- 给了 `--config` 时，界面上的配置改动会写回文件（先写临时文件再原子改名），
  常驻服务重启后配置还在
- `/metrics`：Prometheus 文本格式，含报文/OFFER/ACK/NAK/丢弃计数、
  运行时长、各作用域的容量与占用

### Changed

- 地址分配改为**原子占位**（`LeaseStore::try_claim`）：挑中即占、占不到换
  下一个候选。原来"先判断再写入"之间的窗口在多实例共享存储下会把同一个 IP
  发给两台机器 —— 这是后续多实例高可用的前提
- **桌面端双击直接进界面** —— 不再先显示"连不上 lessord"的表单页。
  没有作用域时由界面自己用"开始发地址"的空状态卡片处理

进行中的工作见 [ROADMAP.md](ROADMAP.md)。

## [0.0.2] - 2026-08-28

### Added

- 桌面端 attach-first：启动先探测 lessord，已在跑就只当客户端；
  没在跑时回退页给出"启动本机实例"表单 —— 选网卡、自动建议地址池、
  一键拉起随包自带的 lessord（Tauri sidecar），就绪后自动进入界面
- 强杀外壳（任务管理器/崩溃）时子进程由 Windows Job Object 兜底回收，
  不留无人管的 DHCP 服务器
- NSIS 安装程序随包自带 `lessord.exe`，装完即用，无需先手工起服务

### Fixed

- 正常关窗时子进程残留：只监听 `Exit` 不够，关最后一个窗口先到的是
  `ExitRequested`，两个都处理后实测归零
- 回退页脚本因缺失 `#cmdline` 元素抛异常整段静默失效（表单不显示）；
  补元素并加缺失即报错的守卫
- 回退页轮询探测在桌面壳里永不成功：管理接口刻意不发 CORS 头，
  探测改为 `no-cors` 模式（只判连通，跳转不受 CORS 约束）

## [0.0.1] - 2026-08-28

首个发布。

### Added

- **DHCPv4 服务端**：作用域 / 地址池 / 排除区间 / 静态保留 / 租约状态机，
  RFC 2131 决策层为纯函数（42 条协议行为测试）；option 61 的 `01+MAC`
  形式归一化，按 MAC 配的保留对真实客户端生效
- **三平台收发**：Linux `SO_BINDTODEVICE`、Windows 绑具体网卡地址
  （实测受限广播会投递）、macOS 无法隔离时如实告警；应答源端口恒为 67
  （PXE 固件的硬性要求）；收发共用单 socket
- **免特权运行**：Windows 直接跑；Linux `setcap cap_net_bind_service+ep`
  一次后普通用户运行。唯一需要特权的改网卡地址操作拆成独立的
  `lessor-netcfg`（权限不足退出码 77，与参数错误 2 区分）
- **网络引导**：按 option 60/77 区分 PXE 固件 / UEFI HTTP Boot / iPXE /
  普通客户端，各发各的引导目标（`--boot-file` / `--http-boot-url` /
  `--ipxe-url`）；iPXE 先于 option 60 判定，杜绝无限自举；
  option 60 与 43 成对下发
- **内嵌 Web 界面**（rust-embed，单二进制）：作用域、租约（可撤销）、
  实时报文日志（WebSocket，含未应答原因）、设备发现
- **设备发现**：RMCP Presence Ping（UDP 623）+ 邻居表，找已配静态 IP、
  不来要地址的机器；解析与系统语言无关
- **桌面端**：Tauri 2 外壳加载同一份界面（本版本为纯客户端，
  `LESSOR_URL` 可指向远程实例）
- **发布渠道**：Docker Hub（`willdockerhub/lessord`）与阿里云 ACR
  （`registry.cn-shenzhen.aliyuncs.com/willspace/lessord`）镜像；
  NSIS 桌面安装程序；Linux / Windows 命令行压缩包

[Unreleased]: https://github.com/weironz/lessor/compare/v0.0.2...HEAD
[0.0.2]: https://github.com/weironz/lessor/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/weironz/lessor/releases/tag/v0.0.1
