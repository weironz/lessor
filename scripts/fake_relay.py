"""假装成一台 DHCP 中继代理，验证 lessord 的跨网段路径。

真实中继（路由器上的 ip helper-address）做的事：客户端在自己网段广播，
路由器收到后把 giaddr 填成**自己在那个网段上的地址**，然后单播给 DHCP
服务器；服务器按 giaddr 选作用域、单播回 giaddr:67，中继再放回线上。

这里模拟的就是中继那一侧：
  - 从 192.168.233.1:6767 发一个 giaddr=192.168.233.1 的 DISCOVER
    给 192.168.73.1:6767（lessord 只在 73 段有监听器）
  - 期望 lessord 按 giaddr 选中 192.168.233.0/24 那个作用域，
    从它的池里分地址，并把应答单播回 192.168.233.1:6767
"""
import socket
import struct
import sys

MAGIC = b"\x63\x82\x53\x63"
SERVER = ("192.168.73.1", 6767)
RELAY_IP = "192.168.233.1"
RELAY_PORT = 6767  # 中继收应答用的就是服务端口，不是客户端端口
MAC = bytes.fromhex("ac1f6b8e0042")
XID = 0x5A1AD000


def build(msg_type, requested=None, server_id=None):
    pkt = struct.pack(
        "!BBBBIHHI", 1, 1, 6, 1, XID, 0, 0x0000, 0  # hops=1，flags 不置广播
    )
    pkt += socket.inet_aton("0.0.0.0")          # yiaddr
    pkt += socket.inet_aton("0.0.0.0")          # siaddr
    pkt += socket.inet_aton(RELAY_IP)           # giaddr —— 中继的标志
    pkt += MAC.ljust(16, b"\x00") + bytes(64) + bytes(128) + MAGIC
    pkt += bytes([53, 1, msg_type])
    pkt += bytes([55, 4, 1, 3, 6, 51])
    if requested:
        pkt += bytes([50, 4]) + socket.inet_aton(requested)
    if server_id:
        pkt += bytes([54, 4]) + socket.inet_aton(server_id)
    return pkt + b"\xff"


def parse(data):
    yiaddr = socket.inet_ntoa(data[16:20])
    giaddr = socket.inet_ntoa(data[24:28])
    opts, i = {}, 240
    while i < len(data) and data[i] != 0xFF:
        if data[i] == 0:
            i += 1
            continue
        ln = data[i + 1]
        opts[data[i]] = data[i + 2 : i + 2 + ln]
        i += 2 + ln
    return yiaddr, giaddr, opts


def main():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((RELAY_IP, RELAY_PORT))
    s.settimeout(5)
    ok = True

    for label, pkt, want in [
        ("DISCOVER", build(1), 2),
        (None, None, None),
    ]:
        if label is None:
            break
        s.sendto(pkt, SERVER)
        try:
            data, frm = s.recvfrom(2048)
        except socket.timeout:
            print(f"!! {label} 没有收到应答")
            return 1
        yiaddr, giaddr, o = parse(data)
        mt = o.get(53, b"\x00")[0]
        sid = socket.inet_ntoa(o[54]) if 54 in o else "-"
        print(f"{label} -> 应答类型={mt} yiaddr={yiaddr} giaddr={giaddr} server-id={sid}")
        print(f"   来自 {frm}（中继应当在服务端口收到单播）")
        if mt != want:
            print(f"!! 应答类型不对，期望 {want}")
            ok = False
        if not yiaddr.startswith("192.168.233."):
            print(f"!! 地址不是从被中继网段的池里分的：{yiaddr}")
            ok = False
        if giaddr != RELAY_IP:
            print(f"!! giaddr 没有原样带回：{giaddr}")
            ok = False
        if 1 in o:
            print(f"   掩码={socket.inet_ntoa(o[1])}", end="")
        if 3 in o:
            print(f" 网关={socket.inet_ntoa(o[3])}", end="")
        if 6 in o:
            print(f" DNS={socket.inet_ntoa(o[6])}", end="")
        print()

        # 接着走 REQUEST，确认完整握手都走中继路径
        s.sendto(build(3, requested=yiaddr, server_id=sid), SERVER)
        try:
            data, frm = s.recvfrom(2048)
        except socket.timeout:
            print("!! REQUEST 没有收到应答")
            return 1
        y2, g2, o2 = parse(data)
        mt2 = o2.get(53, b"\x00")[0]
        print(f"REQUEST  -> 应答类型={mt2} yiaddr={y2} giaddr={g2}")
        if mt2 != 5:
            print("!! 不是 ACK")
            ok = False
        if y2 != yiaddr:
            print(f"!! ACK 给的地址和 OFFER 不一致：{y2} != {yiaddr}")
            ok = False

    print("\n中继路径通过" if ok else "\n中继路径有问题")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
