"""并发压测：N 个客户端同时握手，验证没有一个地址被发给两台机器。

这是 DHCP 服务器的头号正确性属性，也是 `LeaseStore::try_claim` 原子占位
存在的全部理由。单元测试钉的是契约（他人持有占不下、过期可接手），
这里钉的是**真并发下的结果**：判断和写入之间如果有窗口，压力足够大时
就会露出来 —— 两个客户端拿到同一个地址。

**收包用一个共享 socket，不是每个客户端一个。** DHCP 服务器把应答发给
客户端端口（这里是 --client-port），完全不看请求从哪个端口来 ——
每个客户端绑自己的临时端口就一个应答都收不到（同一个坑见
docs/dhcp-conflict-detection.md）。而 200 个 socket 抢同一个端口在各平台上
行为不一。所以：一个 socket 收，按 xid 分发到各客户端的队列。

用法（先按 scripts/README.md 起好服务）：

    LESSOR_SERVER=192.168.233.1 python scripts/load_test.py 200

参数是并发客户端数。地址池不够大时后面的客户端本来就该拿不到 ——
那不是 bug，脚本分开统计。
"""

import collections
import os
import queue
import socket
import struct
import sys
import threading
import time

MAGIC = b"\x63\x82\x53\x63"
SRV_IP = os.environ.get("LESSOR_SERVER", "127.0.0.1")
SRV_PORT = int(os.environ.get("LESSOR_DHCP_PORT", 6767))
CLI_PORT = int(os.environ.get("LESSOR_CLIENT_PORT", 6768))
TIMEOUT = float(os.environ.get("LESSOR_TIMEOUT", 15))


def build(msg_type, xid, mac, requested=None, server_id=None):
    pkt = struct.pack(
        "!BBBBIHHIIII16s64s128s",
        1, 1, 6, 0, xid, 0, 0x8000, 0, 0, 0, 0,
        mac.ljust(16, b"\x00"), b"", b"",
    ) + MAGIC
    pkt += bytes([53, 1, msg_type])
    pkt += bytes([55, 3, 1, 3, 6])
    if requested:
        pkt += bytes([50, 4]) + socket.inet_aton(requested)
    if server_id:
        pkt += bytes([54, 4]) + socket.inet_aton(server_id)
    return pkt + b"\xff"


def opts_of(data):
    out, i = {}, 240
    while i < len(data) and data[i] != 0xFF:
        if data[i] == 0:
            i += 1
            continue
        ln = data[i + 1]
        out[data[i]] = data[i + 2 : i + 2 + ln]
        i += 2 + ln
    return out


class Bus:
    """一个共享 socket，收到的应答按 xid 投进各自的队列。"""

    def __init__(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        # 500 个客户端同时握手时，默认收包缓冲会溢出，丢的是应答而不是
        # 服务端没发 —— 那会把测试结果读成服务端的问题。放大到 8 MB。
        try:
            self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
        except OSError:
            pass
        self.sock.bind((SRV_IP, CLI_PORT))
        self.boxes = {}
        self.running = True
        threading.Thread(target=self._pump, daemon=True).start()

    def box(self, xid):
        q = queue.Queue()
        self.boxes[xid] = q
        return q

    def send(self, pkt):
        self.sock.sendto(pkt, (SRV_IP, SRV_PORT))

    def _pump(self):
        self.sock.settimeout(0.5)
        while self.running:
            try:
                data, _ = self.sock.recvfrom(2048)
            except socket.timeout:
                continue
            except OSError:
                return
            if len(data) < 241 or data[0] != 2:
                continue
            xid = struct.unpack("!I", data[4:8])[0]
            q = self.boxes.get(xid)
            if q is not None:
                q.put(data)

    def close(self):
        self.running = False
        self.sock.close()


def exchange(bus, q, pkt, want, deadline):
    """发一个包、等指定类型的应答，收不到就重发。

    **重传是必须的，不是为了好看。** 500 个 DISCOVER 同时打出去，UDP
    必然丢一部分 —— 那是数据报协议的常态，真实 DHCP 客户端正是靠
    RFC 2131 的重传退避扛过去的。不重传的话测出来的是"这条链路一次能
    塞下多少包"，而不是服务端行不行。

    退避 0.5s 起、翻倍 —— 和真实客户端一个量级。
    """
    wait = 0.5
    while time.monotonic() < deadline:
        bus.send(pkt)
        stop = min(time.monotonic() + wait, deadline)
        while True:
            left = stop - time.monotonic()
            if left <= 0:
                break
            try:
                data = q.get(timeout=left)
            except queue.Empty:
                break
            o = opts_of(data)
            if o.get(53, b"\x00")[0] == want:
                return socket.inet_ntoa(data[16:20]), o
        wait = min(wait * 2, 4.0)
    return None


def one_client(idx, bus, results, done, barrier):
    mac = bytes([0x02, 0x10, 0x20, (idx >> 16) & 0xFF, (idx >> 8) & 0xFF, idx & 0xFF])
    xid = 0x4C000000 + idx
    q = bus.box(xid)

    # 所有线程在这里对齐，尽量让 DISCOVER 同时打出去 ——
    # 错开发的话就测不到并发了
    barrier.wait()
    deadline = time.monotonic() + TIMEOUT

    try:
        offer = exchange(bus, q, build(1, xid, mac), 2, deadline)
        if offer is None:
            results[idx] = ("no-offer", None)
            return
        ip, o = offer
        sid = socket.inet_ntoa(o[54]) if 54 in o else SRV_IP

        req = build(3, xid, mac, requested=ip, server_id=sid)
        ack = exchange(bus, q, req, 5, deadline)
        if ack is None:
            results[idx] = ("no-ack", ip)
            return
        results[idx] = ("ack", ack[0])
        done[idx] = time.monotonic()
    except OSError as e:
        results[idx] = ("error", str(e))


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 100
    print(f"并发 {n} 个客户端 -> {SRV_IP}:{SRV_PORT}（收包 {SRV_IP}:{CLI_PORT}）")

    bus = Bus()
    results = [None] * n
    done = {}
    barrier = threading.Barrier(n)
    threads = [
        threading.Thread(target=one_client, args=(i, bus, results, done, barrier), daemon=True)
        for i in range(n)
    ]

    t0 = time.monotonic()
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=TIMEOUT * 2)
    elapsed = time.monotonic() - t0
    bus.close()

    kinds = collections.Counter(r[0] if r else "hung" for r in results)
    acked = {i: r[1] for i, r in enumerate(results) if r and r[0] == "ack"}

    # 核心断言：同一个地址不能出现在两个客户端上
    by_ip = collections.defaultdict(list)
    for i, ip in acked.items():
        by_ip[ip].append(i)
    dupes = {ip: who for ip, who in by_ip.items() if len(who) > 1}

    print(f"\n用时 {elapsed:.1f}s，{len(acked)} 个拿到 ACK")
    if acked and done:
        # 用"最后一个 ACK 的时刻"算，而不是总耗时 —— 有客户端拿不到
        # 地址时总耗时被超时主导，那个数字没有意义
        span = max(max(done.values()) - t0, 1e-6)
        print(f"吞吐 {len(acked) / span:.0f} 次握手/秒（{span:.2f}s 内完成）")
    for k, v in sorted(kinds.items()):
        print(f"  {k}: {v}")

    if dupes:
        print(f"\n!! 地址冲突 {len(dupes)} 处 —— 同一个地址发给了多个客户端：")
        for ip, who in list(dupes.items())[:10]:
            print(f"   {ip} -> 客户端 {who}")
        return 1

    print(f"\n{len(by_ip)} 个不同地址，没有任何一个被发给两个客户端。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
