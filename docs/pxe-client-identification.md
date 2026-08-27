# 引导客户端识别（PXE / HTTP Boot / iPXE）

lessor 从 **option 60（vendor class）** 和 **option 77（user class）**
认出客户端要哪种网络引导，分成四类（`BootClient`）：

| 类别 | 判据 | 要什么 |
| --- | --- | --- |
| `Pxe` | option 60 以 `PXEClient` 开头 | TFTP 上的文件名 |
| `HttpBoot` | option 60 以 `HTTPClient` 开头 | 一个完整 URL |
| `Ipxe` | **option 77 为 `iPXE`** | 一个 iPXE 脚本 URL |
| `Plain` | 都没有 | 默认引导文件 |

两个用处：

- **只把引导选项发给需要的机器** —— 见
  [option 60 与 option 43](pxe-option-60-and-43.md)，声明 `PXEClient` 是有代价的，
  不该发给普通客户端
- **界面上认得出这是什么设备** —— 租约表里的"设备类型"列直接显示这个字符串

## 实测：真实客户端发的是什么

| 客户端 | option 60 | option 77 |
| --- | --- | --- |
| VMware UEFI 固件（PXE 模式） | `PXEClient:Arch:00007:UNDI:003000` | 不发 |
| VMware UEFI 固件（HTTP Boot 模式） | `HTTPClient:Arch:00016:UNDI:003000` | 不发 |
| iPXE 2.0.0+（x86_64-efi） | `PXEClient:Arch:00007:UNDI:003010` | **`iPXE`**（4 字节裸串） |
| busybox `udhcpc` 1.37 | `udhcp 1.37.0` | 不发 |
| `systemd-networkd`（Ubuntu 24.04） | **不发** | 不发 |

两点值得注意：

- **只有固件会自报家门。** 操作系统起来之后通常什么都不发 ——
  这正好让"这台机器现在处在哪个阶段"变得可判断。
- **iPXE 的 option 60 和裸固件几乎一样**，只有 UNDI 版本差一位
  （`003010` vs `003000`）。指望靠 option 60 把它们分开是不行的，
  必须看 option 77。

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

来自 RFC 4578 的 IANA 注册表。**注意进制**：注册表里习惯写十六进制，
而 option 60 字符串里是**十进制五位**，两者对不上号很容易看错
（实测那台 HTTP Boot 固件报的是 `Arch:00016`，不是 `00010`）：

| 注册表值 | option 60 里的写法 | 含义 | 一般给什么 |
| --- | --- | --- | --- |
| `0x0000` | `00000` | Intel x86PC（传统 BIOS） | `pxelinux.0` / `lpxelinux.0` |
| `0x0006` | `00006` | EFI IA32 | `bootia32.efi` |
| `0x0007` | `00007` | **EFI BC**（EFI Byte Code） | `bootx64.efi` |
| `0x0009` | `00009` | EFI x86-64 | `bootx64.efi` |
| `0x000b` | `00011` | EFI ARM64 | `bootaa64.efi` |
| `0x0010` | `00016` | UEFI HTTP Boot x86-64 | HTTP URL |
| `0x0013` | `00019` | UEFI HTTP Boot ARM64 | HTTP URL |

option 93 里则是二进制的注册表值本身（两字节），不是这个十进制串。

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
| 77 | user class —— 裸固件不发，iPXE 会发 `iPXE` | **读**，用来认出 iPXE |

option 55（参数请求列表）里它还点名要 43、66、67、97 和 128–135。

**架构码没有单独解析出来** —— lessor 只匹配 option 60 的字符串前缀。
对当前用法够了：一个作用域按客户端类别配三个引导目标，不按架构细分。
真要给 BIOS 和 UEFI 混装的机房发不同引导文件，就得读 option 93。

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

代码里还有 `Lease::is_pxe()`，判据是"`vendor_class` 以 `PXEClient` 开头"。
注意它和 `boot_client_of()` 不是一回事：`is_pxe()` 只看落库的 option 60，
认不出 iPXE（那要看请求里的 option 77，租约上没存）。

## 判定顺序：iPXE 必须先判

iPXE 被固件加载起来之后再次发 DISCOVER 时，**同时带着两个身份**：

```
option 60 = PXEClient:Arch:00007:UNDI:003000    ← 继承自固件
option 77 = iPXE                                ← 它自己
```

只按 option 60 判，就会把它当成裸固件，于是又把 `ipxe.efi` 发回去 ——
它加载完再来问，再被发一次，**无限自举**。所以 `boot_client_of()` 先看
option 77。

option 77 的线上格式有两种：RFC 3004 规定是"长度前缀串"的列表，
而 iPXE 直接发裸字符串。实测 iPXE 2.0.0+ 发的是 `69 50 58 45`
（4 字节 `iPXE`，无长度前缀），但经过某些中继会被重新封装成前缀形式，
所以两种都认。

## 该给谁发什么

```bash
lessord --listen 192.168.88.1 --prefix 24 \
        --pool 192.168.88.10-192.168.88.50 \
        --next-server 192.168.88.1 \
        --boot-file     bootx64.efi \
        --http-boot-url http://192.168.88.1/boot.efi \
        --ipxe-url      http://192.168.88.1/boot.ipxe
```

- `--boot-file` —— PXE 固件和未自报身份的客户端
- `--http-boot-url` —— UEFI HTTP Boot 固件
- `--ipxe-url` —— 已经跑起来的 iPXE

只配 `--boot-file` 时的退化行为：

| 客户端 | 拿到什么 |
| --- | --- |
| `Plain` / `Pxe` | `--boot-file` |
| `Ipxe` | `--boot-file` —— 和加这个特性之前一致。**如果它就是 `ipxe.efi`，会自举** |
| `HttpBoot` | 只有 `--boot-file` 本身是 URL 时才发，否则**一个引导选项都不发** |

最后一行是有意的：把 `bootx64.efi` 这样的 TFTP 文件名发给 HTTP Boot 固件，
它会拿去当 URL 解析然后失败。什么都不发，至少它能去试别的启动项。

同理，option 60 = `HTTPClient` 只在真的给了 URL 时才回。UEFI 规范要求
HTTP Boot 的应答里带这一项，固件靠它确认 URL 是给自己的 —— 但空口声明
和 [`PXEClient` 那边](pxe-option-60-and-43.md)一样有害。

## BOOTP `file` 字段放不下 URL

引导文件名除了 option 67，还会写进 BOOTP 报文头的 `file` 字段
（部分老固件只读这里）。但那个字段只有 **128 字节**，而 HTTP Boot 和
iPXE 的 URL 经常更长。

`dhcproto` 的 `set_fname_str` 超长会直接 **panic**，打崩整个收发循环 ——
所以 lessor 放不下就只发 option 67。认 `file` 字段的都是老式 TFTP 固件，
它们的文件名本来就短，装不下的场景也用不着它。

## 验证到什么程度

- **PXE**：拿 VMware Workstation 的真 UEFI 固件打通全程（DHCP → TFTP →
  shim → GRUB），见 [debugging-pxe.md](debugging-pxe.md)
- **iPXE**：拿真的 iPXE 2.0.0+ 打通全程，见下面一节
- **HTTP Boot**：拿真固件打通全程，见下面一节。VMware Workstation 默认
  不开这个模式，vmx 里加一行 `networkBootProtocol = "httpv4"` 就有了
- 三类之外还有单元测试和
  [`scripts/boot_matrix.py`](../scripts/boot_matrix.py) —— 后者走真实
  UDP socket，构造真的 option 60 / option 77 组合，检查应答里的实际字节：

  ```
  ✓ 普通客户端        option67=bootx64.efi                    option60=—
  ✓ PXE 固件          option67=bootx64.efi                    option60=—
  ✓ HTTP Boot 固件    option67=http://.../boot.efi            option60=HTTPClient
  ✓ iPXE（裸串）       option67=http://.../boot.ipxe           option60=—
  ✓ iPXE（长度前缀）    option67=http://.../boot.ipxe           option60=—
  ```

### HTTP Boot 实测

vmx 里加 `networkBootProtocol = "httpv4"` 之后，同一台虚拟机的固件换了身份：

```
option 60 = HTTPClient:Arch:00016:UNDI:003000
```

lessor 认出 `HttpBoot`，回的是：

```
BOOTP.file = http://192.168.233.1/bootx64.efi
option 54  = 192.168.233.1
option 60  = HTTPClient            ← UEFI 规范要求，不回它固件不会去取
option 67  = http://192.168.233.1/bootx64.efi
```

固件随即用 HTTP 把它取走：

```
192.168.233.50 - - "GET /bootx64.efi HTTP/1.1" 200 -
```

那次实测里 HTTP 根目录下的 `bootx64.efi` 放的其实是 iPXE，所以取走之后
iPXE 就起来了，接着走下面 iPXE 那一段 —— 一次跑通了两条链。

### iPXE 链式引导实测

固件 TFTP 拉 `ipxe.efi`（官方 x86_64-efi 构建，1159680 字节）→ iPXE 起来
自己再问一次 DHCP → 拿到脚本 URL → HTTP 取走执行。lessord 这边看到两轮：

```
已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50   ← 固件
已应答 client=00:0c:29:78:c4:fa request=REQUEST  reply=ACK   ip=192.168.233.50
已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50   ← iPXE
已应答 client=00:0c:29:78:c4:fa request=REQUEST  reply=ACK   ip=192.168.233.50
```

iPXE 自己的输出证明它拿到的是脚本而不是自己：

```
net0: 00:0c:29:78:c4:fa using vmxnet3 on 0000:0b:00.0 (Ethernet) [open]
Configuring (net0 00:0c:29:78:c4:fa)...... ok
net0: 192.168.233.50/255.255.255.0 gw 192.168.233.1
Next server: 192.168.233.1
Filename: http://192.168.233.1/boot.ipxe        ← 不是 ipxe.efi，没有自举
http://192.168.233.1/boot.ipxe... ok
boot.ipxe : 91 bytes [script]
```

复现步骤见 [debugging-pxe.md](debugging-pxe.md#验证-ipxe-链式引导)。

## 还没做的

- **按架构分发引导文件**（BIOS 和 UEFI 混装的机房）需要读 option 93，
  lessor 只读 option 60 的字符串前缀，没把架构码解析出来。
- **WebSocket 事件不带 `vendor_class`**，实时日志里看不到设备类型，
  只有租约表有。

## 相关测试

`crates/lessor-core/tests/decision.rs`：

- `vendor_class_is_recorded_on_the_lease` —— option 60 原样记录，且 `is_pxe()` 为真
- `pxe_offer_echoes_option_60_only_with_vendor_options` —— 识别结果怎么影响应答
- `non_pxe_clients_never_get_option_60` —— 没自报的客户端不会被误判
- `ipxe_is_recognised_before_the_pxe_vendor_class` —— 自举那条防线
- `ipxe_user_class_is_read_in_both_wire_forms` —— option 77 的两种线上格式
- `a_pxe_firmware_without_user_class_is_not_mistaken_for_ipxe` —— 反向不误判
- `http_boot_gets_the_url_and_the_required_vendor_class`
- `http_boot_gets_nothing_when_only_a_tftp_filename_is_configured`
- `long_urls_do_not_reach_the_bootp_file_field` —— 128 字节那条 panic

外加 [`scripts/boot_matrix.py`](../scripts/boot_matrix.py)，走真实 socket。
