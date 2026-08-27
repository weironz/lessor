"""抓一帧虚拟机控制台，存成 PNG。

固件 / PXE 阶段 guest 里没有 VMware Tools，`vmrun captureScreen` 会直接报
"Anonymous guest operations are not allowed"，所以只能走 VNC。先在 .vmx 里开：

    RemoteDisplay.vnc.enabled = "TRUE"
    RemoteDisplay.vnc.port = "5902"

然后：

    python scripts/vm_console.py 5902 shot.png

只实现够用的那部分 RFB：无认证、Raw 编码、一次全屏 FramebufferUpdate，
PNG 也是手写的 —— 不引第三方依赖，排查工具本身不该再带出装依赖的麻烦。
"""
import socket
import struct
import sys
import zlib

HOST, PORT = "127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 5902
OUT = sys.argv[2] if len(sys.argv) > 2 else "shot.png"


def recvn(s, n):
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise EOFError(f"连接断开，收到 {len(buf)}/{n}")
        buf += chunk
    return buf


s = socket.create_connection((HOST, PORT), timeout=15)
s.settimeout(15)

server_ver = recvn(s, 12)
print("服务端版本:", server_ver.decode(errors="replace").strip())
s.sendall(b"RFB 003.008\n")

n = recvn(s, 1)[0]
if n == 0:
    reason_len = struct.unpack("!I", recvn(s, 4))[0]
    raise SystemExit("握手失败: " + recvn(s, reason_len).decode(errors="replace"))
types = recvn(s, n)
print("安全类型:", list(types))
if 1 in types:
    s.sendall(bytes([1]))
else:
    raise SystemExit(f"需要认证，暂不支持: {list(types)}")

res = struct.unpack("!I", recvn(s, 4))[0]
if res != 0:
    raise SystemExit("安全握手被拒")

s.sendall(bytes([1]))  # shared

w, h = struct.unpack("!HH", recvn(s, 4))
pf = recvn(s, 16)
name_len = struct.unpack("!I", recvn(s, 4))[0]
name = recvn(s, name_len).decode(errors="replace")
bpp, depth, big_endian, true_colour = pf[0], pf[1], pf[2], pf[3]
rmax, gmax, bmax = struct.unpack("!HHH", pf[4:10])
rsh, gsh, bsh = pf[10], pf[11], pf[12]
print(f"桌面: {name}  {w}x{h}  bpp={bpp} depth={depth}")

# 只要 Raw 编码，省得实现 Hextile/Tight
s.sendall(struct.pack("!BBHi", 2, 0, 1, 0))
# FramebufferUpdateRequest，incremental=0 要整屏
s.sendall(struct.pack("!BBHHHH", 3, 0, 0, 0, w, h))

pixels = bytearray(w * h * 3)
got = 0
while got < w * h:
    msg = recvn(s, 1)[0]
    if msg != 0:
        # 忽略 SetColourMapEntries / Bell / ServerCutText
        if msg == 1:
            recvn(s, 5)
            ncolors = struct.unpack("!H", recvn(s, 2))[0]
            recvn(s, ncolors * 6)
        elif msg == 3:
            recvn(s, 3)
            ln = struct.unpack("!I", recvn(s, 4))[0]
            recvn(s, ln)
        continue
    recvn(s, 1)
    nrects = struct.unpack("!H", recvn(s, 2))[0]
    for _ in range(nrects):
        x, y, rw, rh, enc = struct.unpack("!HHHHi", recvn(s, 12))
        if enc != 0:
            raise SystemExit(f"收到非 Raw 编码 {enc}，本工具不支持")
        data = recvn(s, rw * rh * bpp // 8)
        step = bpp // 8
        for row in range(rh):
            for col in range(rw):
                off = (row * rw + col) * step
                px = int.from_bytes(data[off:off + step], "big" if big_endian else "little")
                r = (px >> rsh) & rmax
                g = (px >> gsh) & gmax
                b = (px >> bsh) & bmax
                # 归一到 8 位
                r = r * 255 // rmax
                g = g * 255 // gmax
                b = b * 255 // bmax
                d = ((y + row) * w + (x + col)) * 3
                pixels[d:d + 3] = bytes((r, g, b))
        got += rw * rh
    if got >= w * h:
        break

# 手写 PNG，免依赖
raw = b"".join(b"\x00" + bytes(pixels[y * w * 3:(y + 1) * w * 3]) for y in range(h))


def chunk(tag, data):
    return (struct.pack("!I", len(data)) + tag + data
            + struct.pack("!I", zlib.crc32(tag + data) & 0xFFFFFFFF))


png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack("!IIBBBBB", w, h, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(raw, 6))
       + chunk(b"IEND", b""))
with open(OUT, "wb") as f:
    f.write(png)
print(f"已保存 {OUT}  ({w}x{h})")
