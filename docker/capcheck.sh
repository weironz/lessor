#!/bin/sh
# 验证 Linux 上能不能用非 root 身份绑 67 端口。
#
# 跑法（阈值必须显式设成 1024，见下）：
#   docker build -t lessor-cap -f Dockerfile .
#   docker run --rm --entrypoint sh \
#       --sysctl net.ipv4.ip_unprivileged_port_start=1024 \
#       -v "$PWD/capcheck.sh":/capcheck.sh:ro lessor-cap /capcheck.sh
#
# --entrypoint sh 是必须的：镜像的 ENTRYPOINT 是 lessord 本身，
# 不覆盖的话脚本路径会被当成 lessord 的参数，直接被参数解析拒掉。
#
# 为什么要显式设阈值：Docker Desktop 的内核把
# ip_unprivileged_port_start 设成了 0，也就是谁都能绑 67 —— 不设的话
# 这个脚本恒为"成功"，测不出任何东西。1024 才是发行版的通常默认值。

THRESHOLD=$(cat /proc/sys/net/ipv4/ip_unprivileged_port_start 2>/dev/null)
echo "  ip_unprivileged_port_start = $THRESHOLD"
[ "$THRESHOLD" = "0" ] && echo "  !! 阈值为 0，本次结果说明不了问题，见文件头"

IP=$(hostname -i | awk '{print $1}')
NET=$(echo "$IP" | cut -d. -f1-3)

# 必须先复制出来再跑。二进制若是从宿主机 bind-mount 进来的，
# Docker Desktop 的文件共享层会让 ld.so 直接断言失败（段错误），
# 那是挂载的问题，不是权限问题 —— 会把结论带偏。
cp /usr/local/bin/lessord /tmp/lessord 2>/dev/null || cp "$(command -v lessord)" /tmp/lessord

su -s /bin/sh -c "timeout 3 /tmp/lessord --listen $IP --prefix 16     --pool $NET.200-$NET.210 --iface eth0 --http 127.0.0.1:8080" nobody >/tmp/o 2>&1

sed -e 's/\[[0-9;]*m//g' /tmp/o > /tmp/plain

if grep -q "开始监听" /tmp/plain; then
    echo "  nobody 身份绑 67 + SO_BINDTODEVICE: 成功"
else
    echo "  nobody 身份绑 67: 失败"
    sed 's/^/      /' /tmp/plain | tail -5
fi
