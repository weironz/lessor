//! UDP 收发循环。
//!
//! 每个监听器一个任务，持有**一个**收发共用的 socket，见 [`socket_for`]。
//!
//! 怎么绑取决于平台。两个目标：只看见目标网卡上的包（做不到隔离的话，
//! 本进程会应答本机所有网卡上的 DHCP 请求，等于在别人的网络里放了一个
//! 流氓 DHCP 服务器），以及应答从 67 端口、从这块网卡发出去。

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use anyhow::{Context, Result};
use dhcproto::{Decodable, Decoder, Encodable, Encoder, v4::Message};
use lessor_core::{Outcome, ReplyDest};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{debug, info, trace, warn};

use crate::config::Listener;
use crate::state::AppState;

/// DHCP 报文最大 576 字节是 RFC 的保守下限，实际带 PXE 选项时会更大。
const BUF: usize = 2048;

#[derive(Clone, Copy, Debug)]
pub struct Ports {
    pub server: u16,
    pub client: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            server: 67,
            client: 68,
        }
    }
}

/// 这个监听器的收包能不能只看见目标网卡上的流量。
///
/// 直接关系到安全：做不到隔离时，服务进程会应答**本机所有网卡**上的
/// DHCP 请求 —— 一台连着生产网的笔记本会就此变成一个流氓 DHCP 服务器。
///
/// 注意 Linux 上的隔离依赖 `SO_BINDTODEVICE`，**必须配了网卡名才成立**；
/// 没配就和 macOS 一样是通配绑定。这个函数不能只看平台。
pub fn rx_is_isolated(listener: &Listener) -> bool {
    if cfg!(target_os = "windows") {
        true // 绑本机地址即天然只收该网卡
    } else if cfg!(target_os = "linux") {
        listener.iface.is_some()
    } else {
        false
    }
}

/// 该监听器的 socket，收发共用。
///
/// **收发必须是同一个 socket**，不能拆成两个：
///
/// - 应答的源端口必须是 67。RFC 2131 规定服务端从 67 发往客户端的 68。
///   dhclient、udhcpc 这类宽松的客户端不校验源端口，**PXE 固件会** ——
///   源端口不对的 OFFER 被静默丢弃，现象是服务端日志里"已应答"，
///   客户端却一直重发 DISCOVER。实测 VMware 的 UEFI PXE 正是如此。
/// - 而两个 socket 绑同一个端口是有害的：Linux 上单播包会被投给绑了
///   具体地址的那一个。收包 socket 绑的是通配地址，于是续租（RENEWING
///   阶段的单播 REQUEST）会落到发送 socket 上，而它从来不读 —— 静默丢包。
///
/// 绑法仍然按平台分，见下面的注释。
fn socket_for(port: u16, server_ip: Ipv4Addr, iface: Option<&str>) -> Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    sock.set_broadcast(true)?;

    // 三个平台各有各的做法，差别不是风格问题而是行为问题：
    //
    // Linux —— 绑到具体地址收不到 255.255.255.255，必须绑通配地址，
    //   再用 SO_BINDTODEVICE 把 socket 钉在网卡上换取隔离。
    //
    // Windows —— 实测受限广播**会**被投递给绑定了具体网卡地址的 socket，
    //   所以直接绑本机地址即可，天然只收这块网卡的包。
    //   （别"顺手"改回通配：那会让本进程应答所有网卡上的 DHCP 请求。）
    //
    // macOS / BSD —— 与 Linux 一样收不到，却又没有 SO_BINDTODEVICE，
    //   只能绑通配且无法隔离。启动时会告警。
    #[cfg(target_os = "linux")]
    if let Some(name) = iface {
        sock.bind_device(Some(name.as_bytes()))
            .with_context(|| format!("绑定网卡 {name} 失败"))?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = iface;

    let bind_ip = if cfg!(target_os = "windows") {
        server_ip
    } else {
        Ipv4Addr::UNSPECIFIED
    };

    let addr: SocketAddr = SocketAddrV4::new(bind_ip, port).into();
    sock.bind(&addr.into())
        .with_context(|| format!("绑定 {bind_ip}:{port} 失败 —— {}", bind_failure_hint(port)))?;
    sock.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(sock.into())?)
}

/// 绑定失败时该往哪儿查。
///
/// 不要笼统地说"需要管理员权限"：**Windows 上绑低端口根本不需要管理员**
/// （"端口 < 1024 需特权"是 Unix 的约定），这么写会让人白白去提权，
/// 而真正的原因通常是端口被别的 DHCP 服务占着。
fn bind_failure_hint(port: u16) -> &'static str {
    if cfg!(target_os = "windows") {
        "端口多半被占用了。常见来源：Internet Connection Sharing（网卡共享）、\
         VMware DHCP Service、其它 DHCP 工具。\
         查：Get-NetUDPEndpoint -LocalPort 67"
    } else if port < 1024 {
        "低端口需要权限或端口被占用。用 root 运行，或给二进制设权限后普通用户运行：\
         sudo setcap cap_net_bind_service+ep <lessord 路径>"
    } else {
        "端口被占用了"
    }
}

/// 报文的十六进制串，用于和抓包逐字节对照。
///
/// 客户端不认应答的时候，"解码后长什么样"和"线上到底是什么字节"是两回事 ——
/// 编码环节出问题只有后者看得出来。
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// 这个收包错误能不能忽略着接着跑。
///
/// `ConnectionReset` 是 Windows 特有的坑：给某个地址发过包之后，如果对方
/// 回了 ICMP 端口不可达，**下一次 recv 会报 WSAECONNRESET** —— 明明是 UDP，
/// 明明和这次要收的包毫无关系。把它当致命错误的话，网段上只要有一台机器
/// 关着 DHCP 客户端，监听器就会不停重启。
fn is_transient(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(e.kind(), WouldBlock | Interrupted | ConnectionReset)
}

/// 网卡还在不在。
///
/// 拔 USB 网卡和"socket 出了别的毛病"要区别对待：前者重建 socket 也没用，
/// 得等网卡回来；后者退避重试就好。
fn address_present(addr: std::net::Ipv4Addr) -> bool {
    // 枚举失败时按"还在"处理：那是我们自己看不见，不是网卡没了，
    // 报成"网卡被拔了"会把人带偏
    lessor_net::interface_with_address(addr).map_or(true, |found| found.is_some())
}

/// 隔多久看一眼地址还在不在。
///
/// 拔网卡到停止应答之间的窗口就是这个值。给到 3 秒：再密没意义
/// （人拔完插回来也要好几秒），再疏就会在窗口里发出一批
/// server-identifier 已经失效的应答。
const ADDR_POLL: Duration = Duration::from_secs(3);

/// 一直把这个监听器维持住：网卡拔了就等它回来，出别的错就退避重试。
///
/// 现场用 USB 网卡是常态，拔插的那一下不该让服务从此哑掉 ——
/// 尤其是无人值守跑着的时候。
///
/// **不能只等 socket 报错。** 各平台的绑法不一样，行为也不一样：
/// Windows 绑的是具体地址，地址没了 socket 就废了；Linux 绑的是
/// `0.0.0.0` + `SO_BINDTODEVICE`，**地址删掉之后 socket 照收不误**，
/// 于是监听器会继续应答，而应答里的 server-identifier 指向一个本机
/// 已经不存在的地址 —— 客户端拿着它去续租，永远等不到回音。
/// 那比干脆停掉更糟。所以这里主动盯着地址在不在，不等 socket 说话。
/// （实测：容器里 `ip addr del` 之后，socket 一声不吭。）
pub async fn serve_forever(state: AppState, listener: Listener, ports: Ports) {
    let addr = listener.server_ip;
    // 退避从 1 秒起，翻倍到 30 秒封顶。收敛慢一点没关系，
    // 重要的是别在一个起不来的网卡上把日志刷爆
    let mut backoff = Duration::from_secs(1);

    loop {
        if !address_present(addr) {
            // 说清楚是"地址没了"，而不是含糊的"监听失败" ——
            // 现场看到这句就知道该去插网卡，不用查别的
            warn!(
                server_ip = %addr,
                "本机上找不到这个地址了（网卡被拔掉或被禁用？）—— 停止应答，等它回来"
            );
            while !address_present(addr) {
                tokio::time::sleep(ADDR_POLL).await;
            }
            info!(server_ip = %addr, "地址回来了，重新开始监听");
            backoff = Duration::from_secs(1);
        }

        let ran = tokio::select! {
            r = serve(state.clone(), listener.clone(), ports) => Some(r),
            // 地址一没就立刻收手。select 结束会把 socket 一起丢掉，
            // 这正是我们要的 —— 网卡回来时要重新绑一次
            _ = wait_until_address_gone(addr) => None,
        };

        match ran {
            // 地址没了，回到循环开头去等
            None => continue,
            Some(Ok(())) => return,
            Some(Err(e)) => {
                warn!(
                    server_ip = %addr, error = %e,
                    retry_in_secs = backoff.as_secs(),
                    "监听器出错，稍后重试"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// 一直等到这个地址从本机上消失。
async fn wait_until_address_gone(addr: std::net::Ipv4Addr) {
    loop {
        tokio::time::sleep(ADDR_POLL).await;
        if !address_present(addr) {
            return;
        }
    }
}

/// 跑一个监听器，直到出错。
pub async fn serve(state: AppState, listener: Listener, ports: Ports) -> Result<()> {
    let sock = socket_for(ports.server, listener.server_ip, listener.iface.as_deref())?;

    info!(
        server_ip = %listener.server_ip,
        iface = listener.iface.as_deref().unwrap_or("(未绑定)"),
        port = ports.server,
        isolated = rx_is_isolated(&listener),
        "开始监听"
    );

    let mut buf = vec![0u8; BUF];
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) if is_transient(&e) => {
                debug!(error = %e, "收包被打断，继续");
                continue;
            }
            Err(e) => {
                // 网卡被拔掉时报的就是这一类。以前这里也只是 warn + continue，
                // socket 已经废了却接着循环 —— 会变成一个刷屏的死循环。
                // 交给上层：那里知道地址还在不在，能决定是等网卡回来还是重试。
                return Err(anyhow::Error::new(e).context("收包失败，监听器无法继续"));
            }
        };

        let req = match Message::decode(&mut Decoder::new(&buf[..n])) {
            Ok(m) => m,
            Err(e) => {
                debug!(%from, error = %e, "丢弃无法解析的报文");
                continue;
            }
        };

        // 我们自己那个"同网段还有没有别的 DHCP"的探测包也会广播到自己头上。
        // 应答它是纯浪费：白占池里一个地址直到 OFFER 过期，还在界面上留下
        // 一条根本不存在的客户端记录。别人家的服务器该答就答，那是探测生效的
        // 方式；答自己没有任何意义。
        if req.chaddr().starts_with(&crate::conflict::PROBE_MAC) {
            trace!("忽略自己的冲突探测包");
            continue;
        }

        // 客户端不认应答时，光看"已应答"那一行没用 —— 得逐字段对。
        // 放在 trace 级：RUST_LOG=lessord::dhcp=trace 打开。
        trace!(?req, "收到（解码后）");

        let outcome = state.handle_packet(&req, listener.server_ip).await;

        // 无人值守时日志是唯一的现场记录，每个报文都留一行
        match &outcome {
            Outcome::Reply(r) => info!(
                client = %crate::state::client_label(&req),
                request = %crate::state::request_label(&req),
                reply = %crate::state::reply_label(&r.msg),
                ip = %r.msg.yiaddr(),
                "已应答"
            ),
            Outcome::Handled(note) => info!(
                client = %crate::state::client_label(&req),
                request = %crate::state::request_label(&req),
                "{note}"
            ),
            Outcome::Drop(why) => debug!(
                client = %crate::state::client_label(&req),
                request = %crate::state::request_label(&req),
                reason = crate::state::drop_reason_text(*why),
                "未应答"
            ),
        }

        let Outcome::Reply(reply) = outcome else {
            continue;
        };

        let mut out = Vec::with_capacity(BUF);
        if let Err(e) = reply.msg.encode(&mut Encoder::new(&mut out)) {
            warn!(error = %e, "应答编码失败");
            continue;
        }

        trace!(msg = ?reply.msg, wire = %hex(&out), "发出（解码后 + 原始字节）");

        let target = match reply.dest {
            // 中继要发回它的服务端口
            ReplyDest::Relay(gw) => SocketAddrV4::new(gw, ports.server),
            ReplyDest::Unicast(ip) => SocketAddrV4::new(ip, ports.client),
            ReplyDest::Broadcast => SocketAddrV4::new(Ipv4Addr::BROADCAST, ports.client),
            // 严格按 RFC 该单播到 yiaddr，但那要求手工写 ARP 表项 ——
            // 跨平台做不到，退回广播。客户端按 xid 自行过滤，不会认错。
            ReplyDest::UnicastYiaddr(ip) => {
                debug!(%ip, "客户端未置广播标志，仍以广播发送");
                SocketAddrV4::new(Ipv4Addr::BROADCAST, ports.client)
            }
        };

        if let Err(e) = sock.send_to(&out, SocketAddr::from(target)).await {
            warn!(%target, error = %e, "发送应答失败");
        }
    }
}

/// 后台定期回收过期租约。
pub async fn reaper(state: AppState, every_secs: u64) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(every_secs));
    // 第一次立即触发没有意义，跳过
    tick.tick().await;
    loop {
        tick.tick().await;
        let n = state.reap().await;
        if n > 0 {
            debug!(count = n, "回收过期租约");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 应答必须从服务端口（67）发出。
    ///
    /// 这条出过事：发送曾经是另一个绑在端口 0 上的 socket，源端口就成了
    /// 临时端口。dhclient、udhcpc、自己写的测试脚本都不校验源端口，所以
    /// 一路绿灯；直到拿真的 VMware UEFI PXE 固件去打，才发现它把这样的
    /// OFFER 全丢了 —— 服务端日志"已应答"，客户端却一直重发 DISCOVER。
    #[tokio::test]
    async fn replies_come_from_the_server_port() {
        // 用高位端口，免得和机器上真的 DHCP 服务撞车
        const PORT: u16 = 6767;
        let sock = socket_for(PORT, Ipv4Addr::LOCALHOST, None).expect("socket 应能创建");
        assert_eq!(
            sock.local_addr().expect("应能取到本地地址").port(),
            PORT,
            "应答的源端口必须是服务端口，否则 PXE 固件会静默丢弃"
        );
    }

    /// 收和发是同一个 socket。
    ///
    /// 拆成两个的话，两个都得绑 67：Linux 上单播包会被投给绑了具体地址的
    /// 那一个，续租请求就此静默丢失。这里用"只有一个 socket 能绑上"来
    /// 兜住这个约束 —— 第二次绑同一地址端口必须失败。
    #[tokio::test]
    async fn one_socket_per_listener() {
        const PORT: u16 = 6769;
        let first = socket_for(PORT, Ipv4Addr::LOCALHOST, None).expect("第一个应能绑上");
        let addr = first.local_addr().expect("应能取到本地地址");

        // 不带 SO_REUSEADDR 再绑一次，占用就应当被拒
        assert!(
            std::net::UdpSocket::bind(addr).is_err(),
            "监听器应当独占服务端口，出现第二个 socket 就说明又拆开了"
        );
    }
}
