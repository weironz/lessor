# 应答的源端口必须是 67

**症状**：服务端日志一行行"已应答"，客户端却一直重发 DISCOVER，永远拿不到地址。
只有 PXE 固件会这样，普通操作系统一切正常。

## 现象

```
INFO lessord::dhcp: 已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50
INFO lessord::dhcp: 已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50
INFO lessord::dhcp: 已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50
INFO lessord::dhcp: 已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50
```

注意**一个 REQUEST 都没有**。DISCOVER 的间隔是 4s、8s、16s、32s ——
PXE 固件的标准退避，说明它压根没收下我们的 OFFER。

把 OFFER 的字节逐个拆开看，是完全合规的：

```
siaddr = 192.168.233.1   yiaddr = 192.168.233.50
file   = bootx64.efi                      # BOOTP file 字段
选项：1(掩码) 3(网关) 6(DNS) 51(租期) 53(OFFER) 54(server-id)
      58(T1) 59(T2) 60(PXEClient) 66 67(bootx64.efi)   结尾 0xff
```

报文没问题。问题在**报文之外**。

## 原因

RFC 2131 §4.1：

> DHCP messages from a server to a client are sent to the 'DHCP client' port (68),
> and messages from a client to a server are sent to the 'DHCP server' port (67).

服务端必须**从 67 端口发**。lessor 早期版本用了两个 socket：收包的绑 67，
发包的绑本机地址加端口 **0** —— 于是源端口是内核分配的临时端口。

这个 bug 能藏这么久，是因为常见客户端都不校验源端口：

| 客户端 | 校验源端口 | 结果 |
| --- | --- | --- |
| busybox `udhcpc` | 否 | 正常拿到地址 |
| `systemd-networkd` / `dhclient` | 否 | 正常拿到地址 |
| 自己写的测试脚本 | 否 | 正常 |
| **VMware UEFI PXE 固件** | **是** | **静默丢弃** |

固件不会报错，也不会在屏幕上说什么 —— 它只是当没收到，继续退避重发。

## 修法

收发**共用一个** socket，绑在服务端口上（`crates/lessord/src/dhcp.rs` 的 `socket_for`）。

不要用"两个 socket 都绑 67"来绕过。那样在 Linux 上会引入更隐蔽的 bug：
内核把单播包投给绑了**具体地址**的那个 socket，而收包 socket 绑的是通配地址
`0.0.0.0`（Linux 上必须如此才能收到 `255.255.255.255`）—— 于是 RENEWING
阶段客户端单播过来的续租 REQUEST 会落到只写不读的那个 socket 上，静默丢失。
客户端续不上租，到 T2 才靠广播重来，表现为"租约偶尔会断一下"。

一个 socket 同时满足两个约束：源端口是 67，且绑定决定了广播从哪块网卡出去。

## 回归测试

`crates/lessord/src/dhcp.rs` 里两条：

- `replies_come_from_the_server_port` —— 断言 socket 的本地端口就是服务端口
- `one_socket_per_listener` —— 不带 `SO_REUSEADDR` 再绑一次必须失败，
  以此保证没人把它又拆回两个

## 自己验一遍

跑一台 UEFI PXE 虚拟机即可，见 [debugging-pxe.md](debugging-pxe.md)。
普通 Linux 客户端**验不出这一条** —— 这正是它当初漏网的原因。
