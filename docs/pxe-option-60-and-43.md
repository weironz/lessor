# option 60 与 option 43：要么都给，要么都不给

**症状**：客户端顺利拿到地址（DISCOVER→OFFER→REQUEST→ACK 全都有），
然后就停在那儿 —— 一个 TFTP 请求都不发，最后掉进 Boot Manager。

## 现象

DHCP 这一段看不出任何毛病：

```
INFO lessord::dhcp: 已应答 client=00:0c:29:78:c4:fa request=DISCOVER reply=OFFER ip=192.168.233.50
INFO lessord::dhcp: 已应答 client=00:0c:29:78:c4:fa request=REQUEST  reply=ACK   ip=192.168.233.50
```

TFTP 服务器那边一片安静。虚拟机屏幕上连 `Fetching Netboot Image` 都没出现，
直接回到启动菜单 —— 也就是说固件**根本没打算网络引导**。

## 原因

应答里带 option 60 = `PXEClient`，在 PXE 规范里不是"随手回个厂商标识"，
而是一句声明：**我是一台 PXE 引导服务器**。固件收到这句声明之后，
会转去 option 43（vendor-specific）里读引导服务器列表 / 引导菜单，
按 PXE 的发现流程走下去。

只声明 60 却不给 43，固件面对的就是一个残缺的 PXE 服务：它接受了地址，
但拿不到该去哪儿取引导文件的信息，于是什么都不做。它**不会**退回去读
siaddr 和 BOOTP `file` 字段 —— 那是另一条路径，声明了 PXE 就不走了。

## 实测对照

VMware Workstation 的 UEFI 固件（vmxnet3 网卡），其余参数完全相同，
只改这两个选项：

| option 60 | option 43 | 结果 |
| --- | --- | --- |
| 无 | 无 | 正常引导，TFTP 拉到 `bootx64.efi`，最终进 GRUB |
| 有 | 有 | 正常引导 |
| **有** | **无** | **拿到 ACK 后什么都不做** |

第三行就是 bug。前两行都能走完 shim → GRUB 全程。

## 别人怎么做

isc-dhcp、dnsmasq 的默认行为都是**不声明 option 60**，只给
`next-server`（siaddr）加 `filename`（BOOTP file 字段）。

MAAS 生成的 `dhcpd.conf` 也一样，UEFI（`arch = 00:07`）那一支只有一行：

```
} elsif option arch = 00:07 {
    filename "bootx64.efi";
}
```

它甚至连 option 66 都不发。只有 HTTP Boot 的那几个 arch（`00:10`、`00:13`）
才会 `option vendor-class-identifier "HTTPClient"` —— 那是真的要声明服务类型。

## 修法

只在作用域配了 option 43 时才声明 option 60
（`crates/lessor-core/src/server.rs`，判据是 `Scope::has_pxe_vendor_options()`）。

普通网络引导什么都不用配：

```bash
lessord --listen 192.168.88.1 --prefix 24 --pool 192.168.88.10-192.168.88.50 \
        --next-server 192.168.88.1 --boot-file bootx64.efi
```

要做 PXE 引导菜单时，把 option 43 一起给上，option 60 会自动带出去：

```bash
lessord ... --boot-file bootx64.efi --option 43=060108ff
```

`060108ff` 是 PXE 规范的 `PXE_DISCOVERY_CONTROL`：
选项号 6、长度 1、值 8（bit 3 = 跳过引导服务器发现，直接用报文里的 filename），
`ff` 收尾。

## 一个教训

排查中途有一组测试让人误以为 option 60 无关 —— 那次测试自己带了
`--option 43`，两个变量搅在一起了。**改一个变量，测一次**，把三种组合
都跑出来才能定论。

## 回归测试

`crates/lessor-core/tests/decision.rs`：

- `pxe_offer_echoes_option_60_only_with_vendor_options` —— 两种组合各断言一次
- `non_pxe_clients_never_get_option_60` —— 没发 option 60 的普通客户端不该收到
