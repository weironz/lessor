# 手工验证脚本

`lessord` 还没有集成测试（要真实 socket），这两个脚本用来在改完之后
快速确认端到端没坏。都用高位端口 —— 不是因为低位端口需要提权
（Windows 上不需要，Linux 上给 `lessord` 设了 `cap_net_bind_service`
之后也不需要），而是为了不和机器上已有的 DHCP 服务撞车。

先起服务：

```bash
cargo run -p lessord -- \
  --listen 192.168.73.1 --prefix 24 --pool 192.168.73.210-192.168.73.220 \
  --router 192.168.73.1 --dns 223.5.5.5 \
  --reservation "ac:1f:6b:8e:00:99=192.168.73.219=bmc-99" \
  --dhcp-port 6767 --client-port 6768 --http 127.0.0.1:8099
```

`--listen` 要换成本机真实存在的一个地址 —— 发送 socket 要绑在它上面。
`lessor-netcfg list` 能列出本机网卡，不需要特权。

## 环境变量

两个脚本的默认值对应上面那条命令。换子网或换端口跑的时候用环境变量覆盖，
不要改脚本里的常量：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `LESSOR_SERVER` | `127.0.0.1` | 服务端地址，**必须和 `--listen` 一致** |
| `LESSOR_DHCP_PORT` | `6767` | 对应 `--dhcp-port` |
| `LESSOR_CLIENT_PORT` | `6768` | 对应 `--client-port` |
| `LESSOR_API` | `http://127.0.0.1:8099` | 对应 `--http` |
| `LESSOR_EXPECT_IP` | `192.168.73.219` | `e2e_check.py` 期望保留到的地址 |

`LESSOR_SERVER` 不能省成 `127.0.0.1`：Windows 上 `lessord` 只绑 `--listen`
给的那个地址（见 `crates/lessord/src/dhcp.rs` 里按平台绑定的那段），
发给环回口的包内核会直接回 ICMP 不可达，脚本会看到 `ConnectionResetError`。

## fake_client.py

模拟一台 BMC 走完整握手 DISCOVER→OFFER→REQUEST→ACK，
打印拿到的地址、掩码、网关、DNS、租期。

```bash
LESSOR_SERVER=192.168.73.1 python scripts/fake_client.py
```

## e2e_check.py

连上 WebSocket 看事件流，验证静态保留、租约记录、撤销租约、
以及撤销不存在的租约返回 404。

```bash
LESSOR_SERVER=192.168.73.1 uv run --with websockets python scripts/e2e_check.py
```

Windows 控制台若报编码错误，加 `PYTHONIOENCODING=utf-8`。

## vm_console.py

抓一帧虚拟机控制台存成 PNG。固件 / PXE 阶段 guest 里没有 VMware Tools，
`vmrun captureScreen` 用不了，只能走 VNC。先在 .vmx 里开：

```
RemoteDisplay.vnc.enabled = "TRUE"
RemoteDisplay.vnc.port = "5902"
```

```bash
python scripts/vm_console.py 5902 shot.png
```

固件屏幕上那几句话是判断它走到哪一步的唯一现场，
对照表见 [docs/debugging-pxe.md](../docs/debugging-pxe.md)。

## tftpd.py

临时 TFTP 服务器，只读，只实现 RRQ。lessor 本身不提供 TFTP ——
这个脚本是用来验证它下发的 `next-server` 和引导文件名确实被固件用上了。

```bash
python scripts/tftpd.py ./tftproot 192.168.88.1
```

没有访问控制，别在生产环境用。

## 更接近真实的那一档

`fake_client.py` 和 `e2e_check.py` 发的是自己拼的报文，覆盖不到真实客户端
的怪癖（比如 option 61 写成 `01+MAC`）。`docker/` 下有一套用 busybox
`udhcpc` 的回归，跑真实客户端 + Linux 的 `SO_BINDTODEVICE` 隔离路径：

```bash
cd docker && docker compose up --abort-on-container-exit --build
```
