# 现场装机：内置 TFTP / HTTP 引导文件服务

lessor 一开始只做一件事：用 DHCP 告诉客户端**去哪儿**拿引导文件。文件本身
得由外部的 dnsmasq / MAAS / 一个小 Python 脚本提供。

这在实际部署里是个多余的台阶。"带一台笔记本进机房、插上网线就开机装"这种
形态，装不下"顺手再搭一套文件服务器"。所以加了 `--boot-dir`：

```bash
lessord --listen 192.168.88.1 --prefix 24 \
        --pool 192.168.88.100-192.168.88.199 \
        --router 192.168.88.1 \
        --boot-dir /srv/boot
```

一条命令起三样东西：

| 服务 | 默认地址 | 作用 |
| --- | --- | --- |
| DHCP | `--dhcp-port`，默认 67 | 发地址 + 下发引导目标 |
| TFTP | 第一个监听器地址:69 | PXE 固件拉 `ipxe.efi` / `undionly.kpxe` |
| HTTP | 第一个监听器地址:8081 | UEFI HTTP Boot 与 iPXE 脚本 |

TFTP 和 HTTP 都在第一次 DHCP 监听器地址上，因为 `next-server` 指向的就是它。

## 目录里放什么

程序**只认四个文件名**，放到 `--boot-dir` 指的那一层：

```
/srv/boot/
├── ipxe.efi          # UEFI 网卡的 iPXE 二进制（推荐入口）
├── undionly.kpxe     # Legacy BIOS 网卡的 iPXE 二进制
├── bootx64.efi       # shim（GRUB 的 UEFI 入口），没有 iPXE 时才用
└── boot.ipxe         # iPXE 脚本 —— 决定"装成什么"
```

启动时会扫一遍目录并打日志：

```
INFO lessord: 引导目录里找到这些文件 files=["ipxe.efi", "bootx64.efi", "boot.ipxe"]
INFO lessord: 网络引导文件服务已配置；引导目标指向本机 filled=1 tftp=192.168.88.1:69 http=192.168.88.1:8081
INFO lessord: 网络引导已配置 scope=机房本段 file=Some("ipxe.efi") next_server=Some(192.168.88.1)
              http_url=Some("http://192.168.88.1:8081/bootx64.efi") ipxe_url=Some("http://192.168.88.1:8081/boot.ipxe")
```

**推出来的引导目标只填进"还没配引导"的作用域。** 手写过 `boot` 的作用域
原样不动 —— 显式配置优先于自动推导，这是唯一说得通的顺序。

`--config` 里手写的作用域和命令行快捷参数拼出来的作用域都适用。

## 推出来的目标是怎么选的

```
filename   = ipxe.efi > undionly.kpxe > bootx64.efi    （第一个存在的）
serverName = 监听器地址                                 （option 66）
nextServer = 监听器地址                                 （siaddr）
httpUrl    = http://<监听器>:<端口>/bootx64.efi         （仅当 bootx64.efi 存在）
ipxeUrl    = http://<监听器>:<端口>/boot.ipxe           （仅当 boot.ipxe 存在）
```

两个"仅当文件存在"是刻意的：指向一个不存在的文件，固件的表现是**一直等到
超时**，比干脆不下发更难排查。日志里的 `files=[...]` 就是给人对账用的。

下发到不同客户端时用的字段不一样，见 `docs/pxe-client-identification.md`：

| 客户端（option 60 / 77） | 拿到的引导目标 |
| --- | --- |
| `PXEClient`（PXE 固件） | `filename`（option 67）+ `siaddr` |
| `HTTPClient`（UEFI HTTP Boot） | `httpUrl`，并回 option 60 = `HTTPClient` |
| iPXE（option 77 含 `iPXE`） | `ipxeUrl` |
| 其它（`Plain`） | `filename` |

**iPXE 必须给脚本。** 它起来之后会再问一次 DHCP，此时它是 iPXE 而不是固件，
如果还是拿到 `ipxe.efi`，它就把自己又拉一遍 —— 无限自举，现象是"屏幕一直
闪同一个画面"。所以配了 iPXE 二进制就一定要配 `boot.ipxe`，否则启动时会有
告警：

```
WARN lessord: scope=机房本段 引导文件 ipxe.efi 看起来就是 iPXE 自己，却没有配 ipxeUrl —— iPXE 起来后会再拿到它，无限自举。给它一个脚本 URL。
```

## 一个能用的 boot.ipxe

脚本是 iPXE 自己的语言，不是 shell。最小可用版本：

```
#!ipxe
dhcp
chain http://192.168.88.1:8081/grubx64.efi
```

或者直接进发行版的网络安装器：

```
#!ipxe
dhcp
kernel http://192.168.88.1:8081/vmlinuz initrd=initrd.img inst.repo=http://192.168.88.1:8081/repo
initrd http://192.168.88.1:8081/initrd.img
boot
```

想让不同机器装不同的系统，用 `${mac}` 分支即可 —— 那是 iPXE 的变量，lessor
不参与：

```
#!ipxe
dhcp
iseq ${mac} ac:1f:6b:8e:00:42 && chain http://192.168.88.1:8081/role-a.ipxe ||
chain http://192.168.88.1:8081/role-b.ipxe
```

## TFTP 的实现边界

- **只实现读（RRQ）**。装机不需要客户端往服务器写任何东西，WRQ 直接拒。
- 支持 `blksize`（8–8192，超出会被夹住）与 `tsize` 选项协商。上限卡在 8192
  是因为再大就容易触发 IP 分片，固件侧丢包反而更严重。
- 每个请求开一个新的 TID socket，主 socket 留在 69 上接新请求 —— TFTP 本来
  就是这么设计的，共用 socket 会把并发传输搅在一起。
- 每块等 ACK、超时 3 秒重传、最多三次。现场网线丢包是常态。
- 文件名可以带子目录（`efi/boot/bootx64.efi`），但不能越出 `--boot-dir`。

`--tftp-port` 可以换高位端口免特权：Linux 上 69 是特权端口，非 root 要么
`setcap cap_net_bind_service+ep lessord`，要么换端口 —— 但换了端口固件找不到
（TFTP 地址是固定的 69），只适合测试。

## HTTP 的边界

- 就是**静态文件**：没有目录索引、没有 `/api`、没有写操作、没有认证守卫。
- 只读，且必须公开可达 —— 引导文件本来就得让网段上的固件取到。所以它和
  管理接口（默认只听 `127.0.0.1:8080`，写操作有守卫）**分开监听**，
  不要图省事把两者合成一个端口。
- 路径穿越挡了两层：`..` / 绝对路径 / 反斜杠在解析阶段就拒，解析出来的真实
  路径再要求落在 `--boot-dir` 之下 —— 光挡 `..` 挡不住符号链接。

## 它不做什么

明确划在外面，免得期待错位：

- **不生成 GRUB 菜单 / iPXE 脚本**。`boot.ipxe` 是你写的，lessor 只负责发出去。
- **不管内核和 initrd 从哪来**。目录里有什么发什么。
- **不做装机内容层**：没有 kickstart / preseed / autoinstall 模板渲染，
  没有按机器角色分发配置。那是 MAAS / Foreman 那一层的事。
- **不跟踪装机进度**。装完之后机器上是什么系统，lessor 不知道也不关心。

一句话：**lessor 负责"开机时把引导文件递到机器手里"，剩下的交给引导文件。**

## 排障

固件的失败形态基本只有一种：**什么都不做**。按这个顺序查：

1. `lessord` 的启动日志里 `引导目录里找到这些文件` 是不是空的？
   空的话文件名不对，或是放到了子目录里。
2. 客户端拿到地址了吗？拿到而机器不动 → 引导目标的问题。
   连地址都没有 → 不是这一层的事，看 `docs/debugging-pxe.md`。
3. 手工验文件服务通不通：

   ```bash
   curl http://192.168.88.1:8081/boot.ipxe
   curl tftp://192.168.88.1/bootx64.efi -o /tmp/x && ls -l /tmp/x
   ```

   两条都通，问题在固件侧（多半是 option 60/67 的匹配，或安全启动没关）。
4. 防火墙：UDP 69、TCP 8081，以及 DHCP 的 67。现场最常见的"服务好好的
   但机器一动不动"就是这两个端口没放行。
5. 抓包对一遍：`--observe` 起第二个实例，或在客户端侧抓，看 ACK 里的
   `siaddr` / option 67 到底是不是你以为的值。

## 与 `scripts/tftpd.py` 的关系

`scripts/tftpd.py` 是加这个功能之前用的兜底 —— 调试 PXE 时装个临时 TFTP。
现在内置服务覆盖了它的用途（而且能同时给 HTTP Boot 和 iPXE 用），脚本留着
是为了"只想验 TFTP 一层"时不必动 lessor 的配置。日常部署用 `--boot-dir`。
