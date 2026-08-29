# lessor 的本地开发编排。`just --list` 看全部。
#
# Recipe 都是 POSIX，跑在 bash 下。Windows 上**必须是 Git Bash** ——
# PATH 里的 `bash` 是 C:\WINDOWS\system32\bash.exe（WSL 启动器），
# 而 WSL 里没有这边的工具链（cargo / bun / docker 都是 Windows 侧的），
# 文件系统视角也不一样（/mnt/d/...）。`windows-shell` 把它钉死。
set shell := ["bash", "-uc"]
set windows-shell := ["C:/Program Files/Git/bin/bash.exe", "-uc"]

# sidecar 的文件名必须带 target triple —— Tauri 按这个规则找。
# 不写死，免得换平台时静默找不到。
triple := `rustc -vV | sed -n 's/^host: //p'`
exe    := if os() == "windows" { ".exe" } else { "" }

# ---------------------------------------------------------------- 日常

[doc("构建本地栈：前端 → lessord → sidecar → 桌面壳")]
dev: kill
    # 顺序是硬性的，不是习惯问题。
    #
    # 1. 前端必须先构建：ui/dist 是 lessord 的**编译期**依赖
    #    （rust-embed 在宏展开时就要读它）。
    cd ui && bun install --frozen-lockfile && bun run build
    # 2. lessord。crates/lessord/build.rs 声明了对 ui/dist 的依赖，
    #    所以前端改了这里会重编 —— 那条 build.rs 是补上去的，之前
    #    cargo 只盯 .rs 文件，改完前端 `cargo build` 跑几十秒（时间花在
    #    依赖上）起来的却还是旧界面。
    cargo build --release -p lessord
    # 3. **把 lessord 拷进 sidecar 目录**。这一步最容易忘，忘了的症状和
    #    上面那个一模一样：桌面壳拉起的是 binaries/ 里的旧副本，你对着
    #    旧界面调半天。实测踩过。
    mkdir -p ui/src-tauri/binaries
    cp target/release/lessord{{exe}} ui/src-tauri/binaries/lessord-{{triple}}{{exe}}
    # 4. 桌面壳
    cd ui/src-tauri && cargo build
    @echo ""
    # 反引号在 just 里是命令替换，写进提示语会被真的执行掉 —— 踩过：
    # 这句原来写成 `just app`，结果每次 just dev 末尾都把桌面端拉起来。
    @echo "好了。just app 起桌面端；just serve 只起服务用浏览器看。"

[doc("拉起本地桌面端（已经在跑的先杀掉）")]
app: kill
    ./ui/src-tauri/target/debug/lessor-desktop{{exe}}

[doc("只起 lessord，用浏览器看界面（不碰桌面壳）")]
serve addr="192.168.233.1" pool="192.168.233.100-192.168.233.110": kill
    @echo "界面 http://127.0.0.1:8099  DHCP 在 6767 端口（非标准，不会打扰真实客户端）"
    ./target/release/lessord{{exe}} --listen {{addr}} --prefix 24 --pool {{pool}} \
        --dhcp-port 6767 --client-port 6768 --http 127.0.0.1:8099 --open

# 杀干净比看着干净重要。三件事都要办：
#
#   - lessor-desktop：开发版和**装好的那个**进程名是同一个。
#   - lessord：这条最关键。桌面壳启动时先探 127.0.0.1:8080，探到就
#     **attach 上去当客户端**、不再拉起自己的 sidecar —— 于是留着一个旧
#     lessord 不杀，`just app` 起来看到的是那个旧进程里嵌的**旧界面**，
#     而你以为自己在看新的。这一条实际坑了我很久。
#   - 文件占用：进程活着时 target 下的 exe 覆盖不了，重编会报
#     `PermissionDenied: 拒绝访问`，而错误里完全看不出是谁占着。
#
# 末尾的 `exit 0` 是必须的：`Get-Process -ErrorAction SilentlyContinue`
# 没匹配到时只压住了错误**输出**，$? 仍然是 false，powershell -Command
# 会把它变成退出码 1 —— 于是最常见的情况（什么都没跑）反而让 recipe 失败，
# 终端上一个字都没有。
[doc("停掉本机所有 lessor 进程（桌面端、服务、装好的那份）")]
kill:
    @if command -v powershell >/dev/null 2>&1; then \
        powershell -NoProfile -Command 'foreach ($p in (Get-Process lessor-desktop,lessord,lessor -ErrorAction SilentlyContinue)) { Write-Host "==> 停掉 $($p.ProcessName) (PID $($p.Id))"; Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue; if (-not $p.WaitForExit(5000)) { Write-Host "    PID $($p.Id) 没能在 5 秒内退出" } }; exit 0'; \
    else \
        pkill -f 'lessor-desktop|lessord' || true; \
    fi

# ---------------------------------------------------------------- 检查

[doc("本机该跑的全套：fmt + clippy + 测试")]
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cd ui/src-tauri && cargo clippy -- -D warnings

# **本机 clippy 全绿不代表 CI 会绿。**
#
# 收发那层、服务注册那层有相当一部分代码在 #[cfg(target_os = "linux")] 里，
# 在 Windows 上根本编译不到。这个坑吃过两次：
#
#   - service.rs 两处 needless_return，只在 Linux 上成立，CI 红了两轮才发现；
#   - systemd unit 模板里用了 ASCII 双引号把 Rust 字符串截断，本机 clippy
#     全绿，差点拿着编译失败的旧二进制去做验证。
#
# CI 现在是 Windows + Linux 双矩阵，但推之前自己先过一遍更省事。
[doc("在容器里跑 Linux 侧的 fmt/clippy/测试（cfg(linux) 的代码本机编译不到）")]
linux:
    cd docker && docker compose -f compose.dev.yml run --rm linux

[doc("依赖树策略检查：已知漏洞 / 许可证 / 来源（策略在 deny.toml）")]
deny:
    cd docker && docker compose -f compose.dev.yml run --rm deny

# ---------------------------------------------------------------- 验证

# 自己拼的报文覆盖不到真实客户端的怪癖（option 61 写成 01+MAC 之类）。
# 这一条跑的是 busybox udhcpc + Linux 的 SO_BINDTODEVICE 隔离路径。
[doc("真实客户端回归：busybox udhcpc 走完整握手")]
e2e: _linux-bin
    cd docker && docker compose -f compose.dev.yml --profile regression up         --abort-on-container-exit --build
    cd docker && docker compose -f compose.dev.yml --profile regression down -v --remove-orphans

# 钉的是 DHCP 服务器的头号正确性属性：同一个地址不会发给两台机器。
# 判断结果要对照服务端 /metrics 的 drops_total —— 客户端看到的 no-offer
# 可能只是 UDP 丢包（脚本自己带重传，见 scripts/README.md）。
[doc("并发压测：N 个客户端同时握手，验证没有地址被重复分配")]
load n="200":
    @echo "先在另一个终端跑 just serve，然后回来跑这条"
    LESSOR_SERVER=192.168.233.1 python scripts/load_test.py {{n}}

[doc("验证 --install-service 的成功路径（真 systemd 容器）")]
service: _linux-bin
    cd docker && docker compose -f compose.dev.yml --profile service up -d --build systemd
    @echo "容器起好了。IP 用下面第一条查，然后跑第二条："
    @echo "  docker compose -f docker/compose.dev.yml exec systemd hostname -i"
    @echo "  docker compose -f docker/compose.dev.yml exec systemd lessord --install-service \\"
    @echo "      --listen <IP> --prefix 16 --pool <池> --iface eth0 --http 127.0.0.1:8080 --no-probe"
    @echo "完事：just service-down"

[doc("收掉 systemd 测试容器")]
service-down:
    cd docker && docker compose -f compose.dev.yml --profile service down -v

# 容器里跑的是宿主机交叉编译好的二进制 —— 镜像里不重编，省得把整个依赖树
# 拉进去（见 docker/Dockerfile 头部）。几个 Linux 侧的验证都要它，抽出来。
_linux-bin:
    cargo zigbuild -q -p lessord --release --target x86_64-unknown-linux-gnu
    cp target/x86_64-unknown-linux-gnu/release/lessord docker/lessord

# ---------------------------------------------------------------- 发布

# 版本号有四处，对不上的话发布流水线的第一个 job 就会失败 ——
# 与其让它在 CI 里失败，不如在这儿一次改对。
[doc("改版本号（四处一起改）")]
version v:
    sed -i 's/^version = "[0-9.]*"/version = "{{v}}"/' Cargo.toml ui/src-tauri/Cargo.toml
    sed -i 's/"version": "[0-9.]*"/"version": "{{v}}"/' ui/package.json ui/src-tauri/tauri.conf.json
    cargo update -w
    @grep -Hn 'version' Cargo.toml ui/package.json ui/src-tauri/Cargo.toml ui/src-tauri/tauri.conf.json | grep '{{v}}'

[doc("看最近几次 CI / 发布跑得怎么样")]
ci:
    gh run list --limit 5

# ---------------------------------------------------------------- 生产

# 这条只是本机验一下 compose 写得对不对，不部署到任何地方。
# 真部署见 docker/compose.prod.yml 头部。
[doc("校验生产 compose 文件（不启动任何东西）")]
prod-check:
    cd docker && LESSOR_TOKEN=dummy docker compose -f compose.prod.yml config >/dev/null
    @echo "compose.prod.yml 语法与变量替换都没问题"
