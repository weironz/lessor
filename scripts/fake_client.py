"""模拟一台 BMC 走完整的 DHCP 握手：DISCOVER→OFFER→REQUEST→ACK。

按 xid + 报文类型匹配，忽略重复包。REQUEST 会带上 option 50/54，
和真实客户端在 SELECTING 阶段的行为一致。
"""
import socket
import struct
import sys
import time

MAGIC = b"\x63\x82\x53\x63"
MAC = bytes.fromhex("ac1f6b8e0001")
XID = 0xDEADBEEF
SRV_PORT, CLI_PORT = 6767, 6768
VENDOR = b"PXEClient:Arch:00007:UNDI:003016"   # option 60


def build(msg_type, requested=None, server_id=None):
    pkt = struct.pack(
        "!BBBBIHHIIII16s64s128s",
        1, 1, 6, 0, XID, 0, 0x8000, 0, 0, 0, 0,
        MAC.ljust(16, b"\x00"), b"", b"",
    ) + MAGIC
    pkt += bytes([53, 1, msg_type])
    pkt += bytes([55, 4, 1, 3, 6, 51])                 # parameter request list
    pkt += bytes([60, len(VENDOR)]) + VENDOR
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


s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
s.bind(("0.0.0.0", CLI_PORT))
s.settimeout(0.5)

offered = None
server_id = None

for name, mtype, want in (("DISCOVER", 1, 2), ("REQUEST", 3, 5)):
    s.sendto(build(mtype, offered, server_id), ("127.0.0.1", SRV_PORT))
    print(f"-> 发出 {name}" + (f"  (请求 {offered})" if offered else ""))

    deadline, hit = time.time() + 5, None
    while time.time() < deadline:
        try:
            data, _ = s.recvfrom(2048)
        except socket.timeout:
            continue
        if len(data) < 240 or struct.unpack("!I", data[4:8])[0] != XID:
            continue
        o = parse_opts(data[240:])
        got = o.get(53, b"\x00")[0]
        if got != want:
            if got == 6:
                print(f"   !! 收到 NAK: {o.get(56, b'').decode('utf-8', 'replace')}")
                sys.exit(1)
            continue                                    # 重复的旧包，跳过
        hit, offered = o, socket.inet_ntoa(data[16:20])
        server_id = socket.inet_ntoa(o[54]) if 54 in o else None
        break

    if hit is None:
        print("   !! 5 秒内没等到期望的回应")
        sys.exit(1)

    label = {2: "OFFER", 5: "ACK"}[want]
    print(
        f"<- 收到 {label}  yiaddr={offered}"
        f"  mask={socket.inet_ntoa(hit[1])}"
        f"  gw={socket.inet_ntoa(hit[3])}"
        f"  dns={socket.inet_ntoa(hit[6]) if 6 in hit else '-'}"
        f"  lease={struct.unpack('!I', hit[51])[0]}s"
        f"  server={server_id}"
    )
    time.sleep(0.3)

print(f"\n握手完成：BMC 拿到 {offered}")
