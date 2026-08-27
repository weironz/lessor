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

- [x] `lessor-core` —— 地址池、租约状态机、报文决策（44 个测试）
- [ ] `lessord` —— tokio UDP 循环 + axum HTTP/WebSocket
- [ ] `lessor-net` —— 平台层：网卡枚举与地址配置
- [ ] `discovery` —— 发现已配置静态 IP 的设备
- [ ] UI —— Svelte + Vite，Tauri 套壳做桌面端

### core 已知的欠缺

这几条是"通用 DHCP 服务器"必须补上的，越早改代价越小：

- [ ] **多作用域** —— 目前 `ServerConfig` 只带一个 `Scope`，租约上也没有 scope 标识。
      多网卡、多子网、按 `giaddr` 选作用域都还做不到
- [ ] **存储抽象** —— `LeaseTable` 是具体类型，接 sqlite 持久化需要抽成 trait
- [ ] **`MacAddr` 的 JSON 表示** —— 现在序列化成字节数组，前端不可读，应实现
      `FromStr` 并序列化为 `"ac:1f:6b:8e:00:01"`
- [ ] **作用域校验** —— 池是否落在子网内、区间是否重叠、保留地址是否冲突，目前都不检查
- [ ] **`vendor_class`（选项 60）** —— PXE 客户端分类要用，租约里还没有这个字段
- [ ] 作用域启用/禁用开关；容量统计（可分配总数 / 已用）

## 架构

```
lessor-core/     纯逻辑，无 IO、无 async、不读时钟 —— 三平台共用
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
