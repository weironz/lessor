# 手工验证脚本

`lessord` 还没有集成测试（要真实 socket），这两个脚本用来在改完之后
快速确认端到端没坏。都用高位端口，不需要管理员权限。

先起服务：

```bash
cargo run -p lessord -- \
  --listen 192.168.73.1 --prefix 24 --pool 192.168.73.210-192.168.73.220 \
  --router 192.168.73.1 --dns 223.5.5.5 \
  --reservation "ac:1f:6b:8e:00:99=192.168.73.219=bmc-99" \
  --dhcp-port 6767 --client-port 6768 --http 127.0.0.1:8099
```

`--listen` 要换成本机真实存在的一个地址 —— 发送 socket 要绑在它上面。

## fake_client.py

模拟一台 BMC 走完整握手 DISCOVER→OFFER→REQUEST→ACK，
打印拿到的地址、掩码、网关、DNS、租期。

```bash
python scripts/fake_client.py
```

## e2e_check.py

连上 WebSocket 看事件流，验证静态保留、租约记录、撤销租约、
以及撤销不存在的租约返回 404。

```bash
uv run --with websockets python scripts/e2e_check.py
```

Windows 控制台若报编码错误，加 `PYTHONIOENCODING=utf-8`。
