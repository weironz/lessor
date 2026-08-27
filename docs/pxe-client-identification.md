# PXE 客户端识别

lessor 靠 **option 60（vendor class identifier）** 认出网络引导中的机器：
以 `PXEClient` 开头就当作 PXE 固件。

两个用处：

- **只把引导选项发给需要的机器** —— 见
  [option 60 与 option 43](pxe-option-60-and-43.md)，声明 `PXEClient` 是有代价的，
  不该发给普通客户端
- **界面上认得出这是什么设备** —— 租约表里的"设备类型"列直接显示这个字符串

## 实测：三种客户端发的 option 60

| 客户端 | option 60 |
| --- | --- |
| VMware UEFI PXE 固件 | `PXEClient:Arch:00007:UNDI:003000` |
| busybox `udhcpc` 1.37 | `udhcp 1.37.0` |
| `systemd-networkd`（Ubuntu 24.04） | **不发** |

也就是说，只有固件会自报家门。操作系统起来之后通常什么都不发 ——
这正好让"这台机器现在处在哪个阶段"变得可判断。

## 同一台机器的两副面孔

同一台虚拟机，先 PXE 再进系统，在 lessor 里是**两条互不相干的记录**：

```jsonc
// 固件阶段
{ "ip": "192.168.233.50", "client": "00:0c:29:78:c4:fa",
  "vendorClass": "PXEClient:Arch:00007:UNDI:003000" }

// 操作系统阶段（同一块网卡）
{ "ip": "192.168.233.50", "client": "opt61:ff2d1aa13300020000ab11...",
  "hostname": "ubuntu-server", "vendorClass": null }
```

两处都变了：

- **option 60 从有到无** —— 这是判断阶段的依据
- **客户端标识从裸 MAC 变成了 DUID**。`systemd-networkd` 默认按
  RFC 4361 发 option 61：`ff` + IAID + DUID，其中 `0002` 是 DUID-EN，
  `0000ab11`（43793）是 systemd 的企业号。**它和 MAC 没有任何关系**，
  所以按 MAC 配的静态保留对进了系统的 Ubuntu 不生效
  —— 要么在 guest 里配 `ClientIdentifier=mac`，要么按 DUID 配保留。

  注意这和 [option 61 的 `01`+MAC 形式](../README.md#真实客户端暴露出来的问题)
  是两码事：那种能归一回 MAC，这种不能。

## option 60 的格式

PXE 规范定义（RFC 4578 §2.3 也有）：

```
PXEClient:Arch:xxxxx:UNDI:yyyzzz
          └─┬─┘      └──┬──┘
       架构码（5 位十进制）  UNDI 版本，各 3 位
```

上面那台固件报的 `Arch:00007` + `UNDI:003000` = 架构 7、UNDI 3.0。

### 架构码

来自 RFC 4578 的 IANA 注册表，只列常见的：

| 码 | 含义 | 一般给什么引导文件 |
| --- | --- | --- |
| `00000` | Intel x86PC（传统 BIOS） | `pxelinux.0` / `lpxelinux.0` |
| `00006` | EFI IA32 | `bootia32.efi` |
| `00007` | **EFI BC**（EFI Byte Code） | `bootx64.efi` |
| `00009` | EFI x86-64 | `bootx64.efi` |
| `0000b` | EFI ARM64 | `bootaa64.efi` |
| `00010` | UEFI HTTP Boot x86-64 | HTTP URL |

`00007` 名义上是 "EFI Byte Code"，但**实际上绝大多数 x86-64 UEFI 固件都报它**，
业界一律当作"x64 UEFI"处理。MAAS 生成的 `dhcpd.conf` 里 `00:07` 和 `00:09`
都指向 `bootx64.efi`，就是这个原因。

顺带一个容易看懵的地方：`dhcproto` 把架构码 7 解码成枚举变体 `BC`，
所以 trace 日志里会看到 `ClientSystemArchitecture(BC)`。它没解析错，
只是照搬了注册表里的名字。

## 固件还会发什么

同一个 DISCOVER 里，那台 UEFI 固件还带了：

| 选项 | 内容 | lessor 目前 |
| --- | --- | --- |
| 60 | `PXEClient:Arch:00007:UNDI:003000` | **读**，原样记进租约 |
| 93 | 架构码（同上，二进制形式） | 不读 |
| 94 | UNDI 版本 `(1, 3, 0)` | 不读 |
| 97 | 机器 UUID，17 字节（首字节 0 + 16 字节 UUID） | 不读 |

option 55（参数请求列表）里它还点名要 43、66、67、97 和 128–135。

**lessor 只看 option 60 的字符串前缀**，架构码没有单独解析出来。
对当前用法够了 —— 一个作用域配一个 `--boot-file`，不按架构分发。
真要给混合架构的机房发不同引导文件（BIOS 和 UEFI 混装），就得读 option 93，
那是还没做的事。

## 记在哪，从哪看

`vendor_class` 存在租约上（`crates/lessor-core/src/lease.rs`），
OFFER 和 ACK 两条路径都会写入（`server.rs`）。

```bash
curl -s http://127.0.0.1:8080/api/leases | jq '.[] | {ip, vendorClass}'
```

- **API** —— `/api/leases` 每条租约的 `vendorClass` 字段
- **界面** —— 租约表的"设备类型"列（`ui/src/lib/Leases.svelte`）

WebSocket 的报文事件（`PacketEvent`）**不带**这个字段 —— 实时日志里看不到
设备类型，只能等租约落库后在租约表里看。想在事件流里直接认出 PXE 报文的话，
这是个待补的口子。

代码里还有 `Lease::is_pxe()`，判据和 `is_pxe_client()` 一致
（`vendor_class` 以 `PXEClient` 开头）。

## 边界

现在的识别只有"是不是 `PXEClient` 开头"这一条判据，以下都**认不出来**：

- **UEFI HTTP Boot** 的客户端自报 `HTTPClient`，不以 `PXEClient` 开头，
  所以不会被当作引导客户端。README 把 HTTP Boot 列为适用场景，
  这一块其实还没做。
- **iPXE** 通过 option 77（user class）自报 `iPXE`，lessor 不读 option 77。
  MAAS 靠它把 iPXE 客户端引到 HTTP 引导脚本上，lessor 做不到。
- **按架构分发引导文件**需要 option 93，见上。

## 相关测试

`crates/lessor-core/tests/decision.rs`：

- `vendor_class_is_recorded_on_the_lease` —— option 60 原样记录，且 `is_pxe()` 为真
- `pxe_offer_echoes_option_60_only_with_vendor_options` —— 识别结果怎么影响应答
- `non_pxe_clients_never_get_option_60` —— 没自报的客户端不会被误判
