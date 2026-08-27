# 怎么对着真固件排查 PXE

lessor 里两个最难找的 bug（[源端口](pxe-source-port.md)、
[option 60/43](pxe-option-60-and-43.md)）有同一个特点：
**普通 DHCP 客户端全部通过，只有固件不干活**。

固件基本不打印任何东西，服务端日志又显示一切正常 —— 光靠这两头看不出问题。
下面是这次真正把问题逼出来的几件工具和一条思路，外加搭台子时会踩的几个坑。

## 一、先分清"包没到"和"包到了但不认"

这一步最重要，方向错了后面全是白工。

在**同一张虚拟网络**上放一个普通 Linux 客户端：给 PXE 那台虚拟机挂上
Ubuntu live-server ISO 从光驱启动，装机界面起来的过程中 systemd-networkd
会自己去要地址。

- Linux 拿到了地址 → 广播能送达，服务端发包路径没问题，**是固件不认**
- Linux 也拿不到 → 问题在发包侧（绑定、网卡、广播），跟固件无关

这次是前者，于是排查范围一下子从"整条链路"收窄到"我们的报文哪里不合固件的口味"。

顺带一提，VMware 自带的 `vnetsniffer.exe` 需要管理员权限，非提权下**静默失败**，
什么都不输出 —— 别把它的空结果当成"网络上没有包"。

## 二、看固件的控制台

固件阶段 guest 里没有 VMware Tools，`vmrun captureScreen` 会报
`Anonymous guest operations are not allowed`。走 VNC：

```
RemoteDisplay.vnc.enabled = "TRUE"
RemoteDisplay.vnc.port = "5902"
```

```bash
python scripts/vm_console.py 5902 shot.png
```

固件说的每一句都值钱，几条常见的：

| 屏幕上 | 含义 |
| --- | --- |
| `EFI Network... No Media.` | 链路还没起来。冷启动第一次经常这样，第二次才是真的 |
| `EFI Network...` 停住 | 正在 DHCP。配合服务端日志看是没应答还是应答被丢 |
| `Fetching Netboot Image` | **DHCP 这一段已经成立**，开始走 TFTP 了 |
| `Unable to fetch TFTP image: TFTP Error` | 地址和文件名都拿到了，是 TFTP 那头的问题 |
| 直接回 Boot Manager | 固件压根没打算网络引导 —— 往 option 60/43 上查 |
| `error: you need to load the kernel first.` | GRUB 已经起来了，DHCP/TFTP 都没问题 —— 是网卡型号，见下 |

`Fetching Netboot Image` 出没出现，是区分 DHCP 问题和 TFTP 问题的分水岭。

## 三、把报文逐字节摊开

```bash
LESSOR_LOG=lessord=trace lessord --listen ... --boot-file bootx64.efi
```

trace 级会打出收到的请求（解码后）和发出的应答（解码后 **+ 原始字节**）。
原始字节不能省：编码环节出问题时，"解码后长什么样"和"线上到底是什么"是两回事。

拿这个可以确认 siaddr、BOOTP `file` 字段、option 60/66/67、`0xff` 结尾
都在该在的位置。**这次它反而是排除项** —— 报文完全合规，所以问题一定在
报文之外（源端口）。能干脆地排除掉一大片，本身就值。

## 四、找一个已知能用的参照

同一份固件既然能被别的 DHCP 服务器引导起来，那两边的差异就是答案所在。

手边正好有一套 MAAS，直接读它生成的配置：

```bash
grep -vE '^\s*#|^\s*$' /var/snap/maas/common/maas/dhcpd.conf
```

一眼看到 UEFI（`arch = 00:07`）那一支只给了 `filename`，既没有 option 60
也没有 option 66 —— 而我们两个都发。这条线索直接指向了第二个 bug。

没有 MAAS 的话，`dnsmasq --dhcp-boot=...` 或 isc-dhcp 起一个也行，
关键是**有一个已知能引导的参照物拿来对**。

## 五、让 TFTP 也真的跑起来

DHCP 通了只是一半。lessor 不提供 TFTP，但要验证它下发的 `next-server` 和
文件名确实被用上了，就得有人应答：

```bash
python scripts/tftpd.py ./tftproot 192.168.88.1
```

引导文件从任何一台 MAAS 上抓即可：

```
/var/snap/maas/common/maas/image-storage/bootloaders/uefi/amd64/bootx64.efi
/var/snap/maas/common/maas/image-storage/bootloaders/uefi/amd64/grubx64.efi
```

看到这样的输出，整条链路就算走通了：

```
RRQ 192.168.88.50 -> bootx64.efi   opts={'tsize': '0', 'blksize': '1468'}
  -> 发完 960472 字节，blksize=1468
RRQ 192.168.88.50 -> grubx64.efi   opts={'blksize': '512'}
  -> 发完 2291592 字节，blksize=512
RRQ 192.168.88.50 -> /grub/grub.cfg
```

`bootx64.efi` 是 shim，它会接着去要 `grubx64.efi`，然后 GRUB 找自己的配置。
到 GRUB 提示符就说明 DHCP 这一侧再没有可挑剔的了。

## 验证 UEFI HTTP Boot

VMware Workstation 的启动菜单里只有 "EFI Network"，看起来不支持 HTTP Boot ——
其实支持，只是默认关着。vmx 里加一行，然后删掉 nvram（它记着旧的启动项）：

```
networkBootProtocol = "httpv4"
```

开了之后同一台虚拟机的固件换了身份，option 60 从 `PXEClient:Arch:00007`
变成 `HTTPClient:Arch:00016`，走的也不再是 TFTP：

```bash
python -m http.server 80 --bind 192.168.233.1 &   # 根目录放一个真的 .efi

lessord --listen 192.168.233.1 --prefix 24 \
        --pool 192.168.233.50-192.168.233.60 \
        --http-boot-url http://192.168.233.1/bootx64.efi
```

跑通的样子：HTTP 日志里出现 `GET /bootx64.efi 200`。没有的话，先确认
应答里带了 `option 60 = HTTPClient` —— UEFI 规范要求有这一项，
不回它固件就不会去取（`LESSOR_LOG=lessord=trace` 能看到实际字节）。

把 `ipxe.efi` 放在 HTTP 根目录当 `bootx64.efi`，还能顺手把下一段一起验了：
固件 HTTP 拉起 iPXE，iPXE 再走它自己的 DHCP。

## 验证 iPXE 链式引导

这条链比 PXE 长一环，也更容易配错（配错的典型结果是无限自举）。
搭一遍只需要三个进程：

```bash
# 1. 取官方构建（注意路径按架构分目录，不是根目录）
curl -o tftproot/ipxe.efi https://boot.ipxe.org/x86_64-efi/ipxe.efi

# 2. 一个 iPXE 脚本，放在 HTTP 上
printf '#!ipxe\necho LESSOR-OK filename=${filename} ip=${net0/ip}\nsleep 90\n' \
    > httproot/boot.ipxe
python -m http.server 80 --bind 192.168.233.1 &
python scripts/tftpd.py ./tftproot 192.168.233.1 &

# 3. 固件拿 ipxe.efi，iPXE 拿脚本
lessord --listen 192.168.233.1 --prefix 24 \
        --pool 192.168.233.50-192.168.233.60 \
        --next-server 192.168.233.1 \
        --boot-file ipxe.efi \
        --ipxe-url  http://192.168.233.1/boot.ipxe
```

跑通的样子：lessord 日志里出现**两轮** DISCOVER/ACK（先固件后 iPXE），
TFTP 日志里有 `ipxe.efi`，HTTP 日志里有 `GET /boot.ipxe 200`，
虚拟机屏幕上 iPXE 打出 `Filename: http://.../boot.ipxe`。

如果屏幕上显示的是 `Filename: ipxe.efi`，那就是自举了 —— 说明 option 77
没被识别，检查 `--ipxe-url` 配没配。

写脚本时注意两点：

- **必须是 LF**。CRLF 的话 `#!ipxe` 匹配不上，iPXE 直接拒绝执行。
- **一条命令失败，整个脚本就中断退出**，屏幕一闪而过回到启动菜单。
  排查时先用最简的 ASCII 脚本（一行 `echo` 加一个 `sleep`）确认链路，
  再往里加内容。带中文的脚本在这次实测里就没跑起来。

`sleep` 是为了让画面停住够久，好用 `vm_console.py` 抓下来 ——
不然脚本执行完 iPXE 就退出了，截图只能拍到启动菜单。

## 测试台本身的坑（虚拟机侧）

下面三条不是 lessor 测出来的，是在**同一台 VMware 上跑 MAAS 装机实验室**时
踩的。它们和 DHCP 无关，但会让人误以为是 DHCP 出了问题 —— 记在这里，
免得下次搭台子重走一遍。

**网卡型号会决定 PXE 能不能走完。** 用 e1000 / e1000e 时，DHCP 和 TFTP
都正常，GRUB 却拉不动内核：

```
error: you need to load the kernel first.
```

HTTP 传到 250–300 KB 就断，而且**每次断的字节数还不一样**。根因是
e1000/e1000e 的 EFI UNDI 驱动太慢，触发了 GRUB 的超时。换 vmxnet3 解决。

已经排除掉的方向，省得重走：服务端 `curl` 拉同一个 URL 完整且快、
HTTP 响应头正常、关掉 TSO/GSO/GRO 无效。

真机上的对应物是 **PXE 网口的 UEFI Option ROM / 网卡固件版本**，
不是 DHCP 服务器，也不是网络配置。

**EFI NVRAM 记着上次的启动项。** 改完虚拟机配置（换网卡、改启动顺序、
开 `networkBootProtocol`）如果不清 nvram，机器就是不按新配置网启，
看起来像"改了没生效"。VMware 上直接删虚拟机目录下的 `nvram` 文件：

```bash
vmrun stop <vmx> hard
rm <vm 目录>/nvram
```

真机上的对应物是 **BIOS 里的 UEFI Boot Order 被上一次安装改写过**。
批量重装时这一步必须处理。

**换网卡型号会改变 guest 里的接口名。** e1000 是 `ens33`，vmxnet3 是
`ens192`。装机后的网络配置（netplan / curtin）别写死接口名，
用 `match: macaddress` + `set-name` 钉住。

## 几个把人带偏的坑

**同一个端口上跑了两个服务器。** `SO_REUSEADDR` 会让第二个也绑上去，
包却只投给其中一个 —— 于是"服务器明明在跑，日志却空的"。开工前先确认：

```bash
# Windows
Get-NetUDPEndpoint -LocalPort 69
# Linux
ss -ulnp 'sport = :69'
```

**二进制不能 bind-mount 进容器再执行。** Docker Desktop 的文件共享层会让
`ld.so` 断言失败直接段错误（`Inconsistency detected by ld.so`），
看着像程序崩了。先 `cp` 出来再跑，或者 `COPY` 进镜像。

**tracing 即使不在终端里也会输出 ANSI 转义。** `grep 'reply=ACK'` 匹配不到，
因为实际是 `reply\x1b[0m\x1b[2m=\x1b[0mACK`。脚本里统计前先剥掉：

```bash
sed 's/\x1b\[[0-9;]*m//g'
```

这条坑了两次，两次都表现为"结果是 0"，很容易误判成功能坏了。

**改一个变量，测一次。** 中途有一组测试让人以为 option 60 无关，
其实那次测试自己带了 `--option 43`。把三种组合都跑出来才敢下结论。

## 本次验证环境

- Windows 11，lessord **普通权限**跑在 67 端口
- VMware Workstation，UEFI 固件 + vmxnet3 网卡，VMnet1（192.168.233.0/24）
- VMware 自带的 DHCP（`VMnetDHCP` 服务）已停，避免抢答
- 五种客户端：busybox `udhcpc`（docker）、`systemd-networkd`（Ubuntu 24.04）、
  VMware UEFI 固件的 PXE 模式与 HTTP Boot 模式（`networkBootProtocol`）、
  iPXE 2.0.0+（官方 x86_64-efi 构建）
- 终点：GNU GRUB 2.06 提示符 / iPXE 执行到 HTTP 上的脚本
