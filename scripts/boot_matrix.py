"""四种引导客户端各自拿到什么 —— 走真实 socket 打一遍 lessord。

单元测试是拿 dhcproto 的结构体断言的，这个脚本拿真的 UDP 报文打，
覆盖编解码那一层：option 77 的两种线上格式、option 60 的前缀匹配、
以及应答里 option 60 / option 67 / BOOTP file 字段的实际字节。

先起服务（把四种目标都配上）：

    lessord --listen 192.168.88.1 --prefix 24 \\
            --pool 192.168.88.10-192.168.88.20 \\
            --next-server 192.168.88.1 --boot-file bootx64.efi \\
            --http-boot-url http://192.168.88.1/boot.efi \\
            --ipxe-url http://192.168.88.1/boot.ipxe \\
            --dhcp-port 6767 --client-port 6768

    LESSOR_SERVER=192.168.88.1 python scripts/boot_matrix.py
"""
import os
import socket
import struct
import sys
import time

SRV_IP = os.environ.get("LESSOR_SERVER", "127.0.0.1")
SRV_PORT = int(os.environ.get("LESSOR_DHCP_PORT", 6767))
CLI_PORT = int(os.environ.get("LESSOR_CLIENT_PORT", 6768))

MAGIC = b"\x63\x82\x53\x63"
PXE_VENDOR = b"PXEClient:Arch:00007:UNDI:003000"
HTTP_VENDOR = b"HTTPClient:Arch:00016:UNDI:003000"

# (用例名, option 60, option 77, 期望的 option 67, 期望应答里的 option 60)
CASES = [
    ("普通客户端", None, None, b"bootx64.efi", None),
    ("PXE 固件", PXE_VENDOR, None, b"bootx64.efi", None),
    ("HTTP Boot 固件", HTTP_VENDOR, None, b"http://192.168.88.1/boot.efi", b"HTTPClient"),
    # iPXE 同时发 option 60 = PXEClient，只按 60 判会把它当固件 —— 无限自举
    ("iPXE（裸串）", PXE_VENDOR, b"iPXE", b"http://192.168.88.1/boot.ipxe", None),
    ("iPXE（长度前缀）", PXE_VENDOR, bytes([4]) + b"iPXE",
     b"http://192.168.88.1/boot.ipxe", None),
]


def build(xid, mac, vendor, user_class):
    pkt = struct.pack(
        "!BBBBIHHIIII16s64s128s",
        1, 1, 6, 0, xid, 0, 0x8000, 0, 0, 0, 0,
        mac.ljust(16, b"\x00"), b"", b"",
    ) + MAGIC
    pkt += bytes([53, 1, 1])                       # DISCOVER
    pkt += bytes([55, 4, 1, 3, 6, 67])             # 参数请求列表
    if vendor:
        pkt += bytes([60, len(vendor)]) + vendor
    if user_class:
        pkt += bytes([77, len(user_class)]) + user_class
    return pkt + b"\xff"


def _s(raw):
    """选项值的可读形式，没有就是 —。"""
    if raw is None:
        return "—"
    return raw.decode("ascii", "replace")


def parse_opts(data):
    o, i = {}, 240
    while i < len(data) and data[i] != 255:
        if data[i] == 0:
            i += 1
            continue
        o[data[i]] = data[i + 2:i + 2 + data[i + 1]]
        i += 2 + data[i + 1]
    return o


s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
s.bind(("0.0.0.0", CLI_PORT))
s.settimeout(0.5)

failed = 0
for n, (name, vendor, user_class, want_file, want_vendor) in enumerate(CASES):
    xid = 0xB0071000 + n
    mac = bytes.fromhex("ac1f6b8e00") + bytes([n])
    s.sendto(build(xid, mac, vendor, user_class), (SRV_IP, SRV_PORT))

    got = None
    deadline = time.time() + 3
    while time.time() < deadline:
        try:
            data, _ = s.recvfrom(2048)
        except socket.timeout:
            continue
        if len(data) >= 240 and struct.unpack("!I", data[4:8])[0] == xid:
            got = data
            break

    if got is None:
        print(f"  {name:<18} !! 3 秒内没等到应答")
        failed += 1
        continue

    o = parse_opts(got)
    file67 = o.get(67)
    vendor60 = o.get(60)
    bootp_file = got[108:236].split(b"\0")[0]

    ok = file67 == want_file and vendor60 == want_vendor
    mark = "✓" if ok else "!!"
    print(f"  {mark} {name:<18} option67={_s(file67)}  option60={_s(vendor60)}  "
          f"BOOTP.file={_s(bootp_file) or '(空)'}")
    if not ok:
        print(f"       期望 option67={_s(want_file)}  option60={_s(want_vendor)}")
        failed += 1

print()
print("全部通过" if failed == 0 else f"{failed} 项不符")
sys.exit(1 if failed else 0)
