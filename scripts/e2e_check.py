"""lessord 端到端检查：WebSocket 事件流 + 静态保留 + 撤销租约。"""
import asyncio
import json
import socket
import struct
import subprocess
import sys
import urllib.request

import websockets

API = "http://127.0.0.1:8099"
WS = "ws://127.0.0.1:8099/api/events"
SRV_PORT, CLI_PORT = 6767, 6768
MAGIC = b"\x63\x82\x53\x63"
XID = 0xCAFEBABE
# 与 --reservation 里配的 MAC 一致，应当拿到 192.168.73.219
MAC = bytes.fromhex("ac1f6b8e0099")


def api(path, method="GET"):
    req = urllib.request.Request(API + path, method=method)
    with urllib.request.urlopen(req, timeout=5) as r:
        body = r.read().decode()
        return r.status, (json.loads(body) if body else None)


def build(msg_type, requested=None, server_id=None):
    pkt = struct.pack(
        "!BBBBIHHIIII16s64s128s",
        1, 1, 6, 0, XID, 0, 0x8000, 0, 0, 0, 0,
        MAC.ljust(16, b"\x00"), b"", b"",
    ) + MAGIC
    pkt += bytes([53, 1, msg_type])
    pkt += bytes([12, 6]) + b"bmc-99"          # option 12 主机名
    if requested:
        pkt += bytes([50, 4]) + socket.inet_aton(requested)
    if server_id:
        pkt += bytes([54, 4]) + socket.inet_aton(server_id)
    pkt += b"\xff"
    return pkt


def parse_opts(d):
    o, i = {}, 0
    while i < len(d):
        if d[i] == 255:
            break
        if d[i] == 0:
            i += 1
            continue
        o[d[i]] = d[i + 2:i + 2 + d[i + 1]]
        i += 2 + d[i + 1]
    return o


def handshake():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.bind(("0.0.0.0", CLI_PORT))
    s.settimeout(0.5)
    offered = server_id = None
    import time
    for mtype, want in ((1, 2), (3, 5)):
        s.sendto(build(mtype, offered, server_id), ("127.0.0.1", SRV_PORT))
        deadline = time.time() + 5
        while time.time() < deadline:
            try:
                data, _ = s.recvfrom(2048)
            except socket.timeout:
                continue
            if len(data) < 240 or struct.unpack("!I", data[4:8])[0] != XID:
                continue
            o = parse_opts(data[240:])
            if o.get(53, b"\x00")[0] != want:
                continue
            offered = socket.inet_ntoa(data[16:20])
            server_id = socket.inet_ntoa(o[54]) if 54 in o else None
            break
        else:
            return None
    s.close()
    return offered


async def main():
    events = []
    async with websockets.connect(WS) as ws:
        print("WebSocket 已连接")

        # 握手在线程里跑，避免阻塞事件循环
        ip = await asyncio.to_thread(handshake)
        if ip is None:
            print("!! 握手失败")
            return 1
        print(f"握手完成，拿到 {ip}")

        # 收 2 秒事件
        try:
            async with asyncio.timeout(2):
                while True:
                    events.append(json.loads(await ws.recv()))
        except (TimeoutError, asyncio.TimeoutError):
            pass

    print(f"\n收到 {len(events)} 条事件：")
    for e in events:
        if e["kind"] == "packet":
            p = e["Packet"] if "Packet" in e else e
            print(
                f"  {p.get('request','?'):9} -> {p.get('result','?'):7} "
                f"{p.get('ip','-'):16} {p.get('detail','') or ''}"
            )
        else:
            print(f"  {e['kind']}")

    ok = True

    if ip != "192.168.73.219":
        print(f"\n!! 静态保留没生效：期望 192.168.73.219，实际 {ip}")
        ok = False
    else:
        print("\n静态保留生效 ✓")

    _, leases = api("/api/leases")
    if len(leases) != 1 or leases[0]["hostname"] != "bmc-99":
        print(f"!! 租约不对: {leases}")
        ok = False
    else:
        print(f"租约已记录 ✓  主机名={leases[0]['hostname']} 状态={leases[0]['state']}")

    status, _ = api(f"/api/leases/1/{ip}", method="DELETE")
    _, leases = api("/api/leases")
    if status != 204 or leases:
        print(f"!! 撤销失败 status={status} 剩余={leases}")
        ok = False
    else:
        print("撤销租约 ✓")

    try:
        api("/api/leases/1/192.168.73.199", method="DELETE")
        print("!! 撤销不存在的租约应返回 404")
        ok = False
    except urllib.error.HTTPError as e:
        if e.code == 404:
            print("撤销不存在的租约返回 404 ✓")
        else:
            print(f"!! 期望 404，实际 {e.code}")
            ok = False

    print("\n全部通过" if ok else "\n有失败项")
    return 0 if ok else 1


sys.exit(asyncio.run(main()))
