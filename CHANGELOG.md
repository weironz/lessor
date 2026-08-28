# Changelog

本文件按版本记录变化，面向使用者，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

分工：这里写"某个版本变了什么"；里程碑级的验收证据（怎么证明做完了）在
[ROADMAP-done.md](ROADMAP-done.md)；未来计划在 [ROADMAP.md](ROADMAP.md)。

## [Unreleased]

### Added

- **跨网段服务（DHCP 中继）**：作用域标上 `viaRelay` 就能服务一个本机没有
  地址的网段 —— 由路由器上的 `ip helper-address` 把请求转过来。企业里多网段
  DHCP 基本都这么跑。配置示例与坑见 [docs/dhcp-relay.md](docs/dhcp-relay.md)
- 界面上经中继的作用域，"本机地址"一栏显示"经 DHCP 中继"而不是空值
- 应答原样带回 option 82（中继代理信息，RFC 3046 的 MUST）。交换机靠它把
  应答送回客户端所在的物理端口，不带回去有的中继代理会直接丢掉 ——
  现象是"服务端一行行已应答、客户端一个都收不到"，且只在真交换机上出现

- CI 增加依赖树策略检查（`cargo-deny`）：已知漏洞、许可证、依赖来源。
  策略在 [deny.toml](deny.toml)，每条都写了理由
- 发布物增加 SBOM（SPDX，含 Rust 与前端两侧依赖），供客户的安全扫描使用

- **systemd 服务不再以 root 运行**：专用 `lessord` 用户 + 只保留
  `CAP_NET_BIND_SERVICE` + 一组 systemd 沙箱。租约库与配置文件放
  `/var/lib/lessord`（由 systemd 建好并归属给服务用户）——
  配置写回需要父目录写权限，所以这些文件必须在服务自己拥有的目录里；
  不在的话注册时会明确拒绝并说清怎么放

### Fixed

- **`--install-service` 带相对路径必然坏掉。** 服务的工作目录不是你注册时
  那个目录，`--config ./lessor.json` 装完立刻找不到文件然后反复重启。
  现在 `--config` / `--lease-db` / `--capture` 会在注册时展开成绝对路径
- **`--install-service` 会报假的成功。** 启动命令成功不等于服务在跑 ——
  进程被拉起后立刻退出也算成功，于是打印"已注册并启动"而服务正在崩溃循环里。
  现在装完会确认服务真的活着，没活成就返回非零并给出排查指引
- **升级 `dhcproto` 0.14 → 0.15，修掉一个已知漏洞**：0.14 传递依赖的
  `hickory-proto` 0.25.2 存在 RUSTSEC-2026-0119（DNS 名字压缩的 O(n²) 导致
  CPU 耗尽）。0.15 带的是修复版 0.26.1。这是 CI 接入 `cargo-deny` 后
  第一次扫描就查出来的 —— 此前三个版本对依赖漏洞状况完全无感
- **经中继网段的客户端续租会被误 NAK。** 续租是客户端直接单播给服务器的，
  不经过中继，报文里没有 `giaddr` 只有 `ciaddr`；原来判断客户端在哪个网段时
  漏了 `ciaddr` 这一层，会退回用收包监听器的地址 —— 那在另一个网段里，
  于是选错作用域并回 NAK，把一个正在正常工作的客户端逼得丢掉租约重来
- 配置校验原来拒绝一切"没有本机地址"的作用域，导致跨网段配置根本写不出来。
  现在认 `viaRelay`；不标仍然拒绝（忘配监听器是最常见的配置错误），
  但报错会指出中继这条路

## [0.0.3] - 2026-08-28

这一版把服务从"能发地址"推到"能放着不管"：运行时改配置、租约落盘、
注册成系统服务、不发别人占着的地址、现场出问题时自己说清楚该查什么。

仍是 0.0.x —— `v0.1` 的门槛是真机 BMC 验证（ROADMAP 的 M9），还没过。
现有的客户端矩阵是 VMware 固件 + Linux 客户端 + 真 iPXE，没有任何一台真 BMC。

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
- **地址冲突探测**：后台持续扫描地址池，被别人静态占用的地址不再发出去。
  探测不在握手路径上（走缓存，不拖慢分配），也不需要特权。
  `--no-probe` 可关（给禁止主动探测的网络）
- **同网段其他 DHCP 服务器告警**：启动时探一次，发现了就告警而不是闷头
  抢答 —— 旁挂到 MAAS 装机网段上时这条能救命。检查做不成时会明说
  "没能进行"，不会伪装成"未发现"
- 池满的事件会说清是谁占着（"地址池已耗尽（探测到 6 个地址被静态占用：
  192.168.73.5 被 00:0c:29:… 占用 …）"）—— 池满经常是配置问题不是容量问题
- **"监听中但一个请求都没收到"会主动报出来**，日志和界面上都给按可能性
  排好序的排查清单（Windows 上第一条是防火墙），每条都带能直接照抄的命令
- **网卡热插拔自动恢复**：网卡被拔掉或禁用时停止应答并说明原因，插回来后
  3 秒内自动重新监听，不用重启服务
- `--idle-exit <秒>`：闲置这么久就自行退出，给"装完机走人"用。
  常驻部署不要开
- 现场 runbook（[docs/field-runbook.md](docs/field-runbook.md)）—— 从插网线
  到走人，含需要管理员的三处分别是什么，以及远程（Tailscale/VPN）该怎么做
- `--capture <路径>`：把收到的每个报文原样存成 JSONL（含原始字节）。
  捕获发生在解码之前 —— 解不出来的包最值钱。不需要抓包驱动，也不需要特权
- `--replay <路径>`：离线重放捕获文件，走真正的决策层逐条给出结论，
  无法解码的单独列出并带上原始字节
- `--observe`：只看不答。收包、记录、界面上显示"本来会怎么答"，但一个字节
  都不发。挂在已经有 DHCP 的生产网段上取证时必须开

### Changed

- 地址分配改为**原子占位**（`LeaseStore::try_claim`）：挑中即占、占不到换
  下一个候选。原来"先判断再写入"之间的窗口在多实例共享存储下会把同一个 IP
  发给两台机器 —— 这是后续多实例高可用的前提
- **桌面端双击直接进界面** —— 不再先显示"连不上 lessord"的表单页。
  没有作用域时由界面自己用"开始发地址"的空状态卡片处理

### Fixed

- 自己发出去的"同网段还有没有别的 DHCP"探测包会广播回自己的监听器，
  被当成一个真客户端应答 —— 每次启动白占池里一个地址直到 OFFER 过期，
  界面上还多一条根本不存在的客户端记录
- 几处报错文案中间夹着一长串空格（多行字符串少了续行符），读起来像断了：
  没给 `--listen` / `--pool` 时的提示、以及绑定端口失败时的提示
- 收包出错时只是记一条日志然后接着循环 —— socket 已经废了却停不下来，
  会变成刷屏的死循环。同时不再把 Windows 的 `WSAECONNRESET` 当成致命错误
  （网段上有机器没开 DHCP 客户端就会触发它，和本次收包无关）
- 任务提前结束时退出码是 0，systemd 的 `Restart=on-failure` 和 Windows
  服务的失败重启都因此不会拉起它。现在返回非零
- 收到无法解析的报文只在 debug 级留一行且不带原始字节 —— 那恰恰是最需要
  看见的东西。改为 warn 并带上完整的十六进制

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

[Unreleased]: https://github.com/weironz/lessor/compare/v0.0.3...HEAD
[0.0.3]: https://github.com/weironz/lessor/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/weironz/lessor/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/weironz/lessor/releases/tag/v0.0.1
