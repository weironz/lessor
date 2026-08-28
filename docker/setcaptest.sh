#!/bin/sh
# 逐个组合验证：非 root 身份绑 67 端口，到底需要哪种 capability 写法。
#
# 跑法（阈值必须显式设成 1024）：
#   docker build -t lessor-cap -f Dockerfile .
#   docker run --rm --entrypoint sh \
#       --cap-add NET_BIND_SERVICE --cap-add NET_RAW \
#       --sysctl net.ipv4.ip_unprivileged_port_start=1024 \
#       -v "$PWD/setcaptest.sh":/setcaptest.sh:ro lessor-cap /setcaptest.sh
#
# --entrypoint sh 是必须的：镜像的 ENTRYPOINT 是 lessord 本身，
# 不覆盖的话脚本路径会被当成 lessord 的参数。
#
# --cap-add 是故意加上的：要测的正是"docker --cap-add 加上非 root 身份
# 够不够"。答案是不够 —— --cap-add 只放进 bounding set，非 root 进程的
# effective set 里拿不到；真正管用的是文件 capability 或 ambient set。

THRESHOLD=$(cat /proc/sys/net/ipv4/ip_unprivileged_port_start 2>/dev/null)
echo "  ip_unprivileged_port_start = $THRESHOLD"
if [ "$THRESHOLD" = "0" ]; then
    echo "  !! 阈值为 0：谁都能绑 67，下面三组必然全部成功，说明不了问题。"
    echo "     加上 --sysctl net.ipv4.ip_unprivileged_port_start=1024 重跑。"
fi

command -v setcap >/dev/null 2>&1 || {
    echo "  !! 镜像里没有 setcap（libcap2-bin），装上再跑"
    exit 1
}

# 先复制出来：bind-mount 进来的二进制在 Docker Desktop 上会段错误，
# 而且 setcap 也写不进只读挂载。
cp /usr/local/bin/lessord /tmp/lessord

IP=$(hostname -i | awk '{print $1}')
NET=$(echo "$IP" | cut -d. -f1-3)

try() {
    echo "  --- $1 ---"
    setcap -r /tmp/lessord 2>/dev/null
    [ -n "$2" ] && setcap "$2" /tmp/lessord 2>/dev/null

    printf '      实际 caps: '
    getcap /tmp/lessord 2>/dev/null | grep . || echo "(无)"

    # 关键是以 nobody 身份跑 —— root 跑什么都成功，测不出东西
    su -s /bin/sh -c "timeout 3 /tmp/lessord --listen $IP --prefix 16         --pool $NET.200-$NET.210 --iface eth0 --http 127.0.0.1:8080" nobody >/tmp/o 2>&1

    # tracing 即使不在终端里也会输出 ANSI 转义，不剥掉的话
    # isolated=... 里夹着转义序列，grep 匹配不到
    sed -e 's/\[[0-9;]*m//g' /tmp/o > /tmp/plain

    if grep -q "开始监听" /tmp/plain; then
        echo "      结果: 成功  $(grep -o 'isolated=[a-z]*' /tmp/plain | head -1)"
    else
        echo "      结果: 失败"
        sed 's/^/          /' /tmp/plain | tail -3
    fi
}

try "无 file capability（只有 docker --cap-add）" ""
try "cap_net_bind_service+ep" "cap_net_bind_service+ep"
try "cap_net_bind_service,cap_net_raw+ep" "cap_net_bind_service,cap_net_raw+ep"
