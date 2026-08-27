"""临时 TFTP 服务器，只为验证 PXE 链路走不走得通。只读，只实现 RRQ。

lessor 本身**不提供 TFTP**，它只负责用 DHCP 把 next-server 和引导文件名
告诉客户端。这个脚本是用来验证那条信息确实被固件用上了 ——
看到 RRQ 进来，就证明 DHCP 那一段完全成立。

    python scripts/tftpd.py <根目录> [绑定地址]

支持 blksize/tsize 选项协商 —— UEFI 固件基本都会用 blksize，
不支持的话 512 字节一块拉 1MB 的 bootx64.efi 会慢到超时。

生产环境别用它：没有任何访问控制，也没做路径穿越之外的加固。
"""
import os
import socket
import struct
import sys

ROOT = sys.argv[1]
BIND = sys.argv[2] if len(sys.argv) > 2 else "192.168.233.1"

RRQ, DATA, ACK, ERROR, OACK = 1, 3, 4, 5, 6

srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((BIND, 69))
print(f"TFTP 就绪 {BIND}:69  根目录={ROOT}", flush=True)

while True:
    pkt, peer = srv.recvfrom(2048)
    op = struct.unpack("!H", pkt[:2])[0]
    if op != RRQ:
        continue

    parts = pkt[2:].split(b"\0")
    name = parts[0].decode("ascii", "replace")
    opts = {}
    rest = [p for p in parts[1:] if p]
    for i in range(1, len(rest) - 1, 2):
        opts[rest[i].decode().lower()] = rest[i + 1].decode()

    path = os.path.join(ROOT, os.path.basename(name))
    print(f"RRQ {peer[0]} -> {name}  opts={opts}", flush=True)

    # 每个传输一个新 socket，源端口是随机的 TID —— TFTP 就是这么设计的
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind((BIND, 0))
    s.settimeout(5)

    if not os.path.isfile(path):
        s.sendto(struct.pack("!HH", ERROR, 1) + b"not found\0", peer)
        print("  -> 文件不存在", flush=True)
        s.close()
        continue

    data = open(path, "rb").read()
    blksize = 512
    ack = {}
    if "blksize" in opts:
        blksize = max(8, min(int(opts["blksize"]), 8192))
        ack["blksize"] = str(blksize)
    if "tsize" in opts:
        ack["tsize"] = str(len(data))

    if ack:
        payload = b"".join(k.encode() + b"\0" + v.encode() + b"\0" for k, v in ack.items())
        s.sendto(struct.pack("!H", OACK) + payload, peer)
        try:
            r, _ = s.recvfrom(1024)
        except socket.timeout:
            print("  -> OACK 无应答", flush=True)
            s.close()
            continue

    total = (len(data) + blksize) // blksize
    ok = True
    for n in range(1, total + 1):
        chunk = data[(n - 1) * blksize: n * blksize]
        # 块号 16 位回绕，大文件必须处理，否则超过 32MB 就错乱
        s.sendto(struct.pack("!HH", DATA, n & 0xFFFF) + chunk, peer)
        try:
            r, _ = s.recvfrom(1024)
            if struct.unpack("!H", r[:2])[0] != ACK:
                ok = False
                break
        except socket.timeout:
            ok = False
            break
    print(f"  -> {'发完' if ok else '中断'} {len(data)} 字节，blksize={blksize}", flush=True)
    s.close()
