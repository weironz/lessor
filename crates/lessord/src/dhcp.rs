//! UDP 收发循环。
//!
//! 每个监听器一个任务，各自持有一对 socket：
//!
//! - **收**：怎么绑取决于平台，见 [`rx_socket`]。目标是只看见目标网卡上的包 ——
//!   做不到隔离的话，本进程会应答本机所有网卡上的 DHCP 请求，
//!   等于在别人的网络里放了一个流氓 DHCP 服务器。
//! - **发**：绑到该监听器的本机地址。广播回应就只会从这块网卡出去，
//!   不会跑到别的网段上去打扰无关设备。

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use anyhow::{Context, Result};
use dhcproto::{Decodable, Decoder, Encodable, Encoder, v4::Message};
use lessor_core::{Outcome, ReplyDest};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

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

fn rx_socket(port: u16, server_ip: Ipv4Addr, iface: Option<&str>) -> Result<UdpSocket> {
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
    sock.bind(&addr.into()).with_context(|| {
        format!("绑定 {bind_ip}:{port} 失败（需要管理员权限，或端口被占用）")
    })?;
    sock.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(sock.into())?)
}

fn tx_socket(server_ip: Ipv4Addr) -> Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    sock.set_broadcast(true)?;
    // 绑到本机在该网段上的地址 —— 广播就只从这块网卡出去
    let addr: SocketAddr = SocketAddrV4::new(server_ip, 0).into();
    sock.bind(&addr.into())
        .with_context(|| format!("发送 socket 绑定 {server_ip} 失败"))?;
    sock.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(sock.into())?)
}

/// 跑一个监听器，直到出错。
pub async fn serve(state: AppState, listener: Listener, ports: Ports) -> Result<()> {
    let rx = rx_socket(ports.server, listener.server_ip, listener.iface.as_deref())?;
    let tx = tx_socket(listener.server_ip)?;

    info!(
        server_ip = %listener.server_ip,
        iface = listener.iface.as_deref().unwrap_or("(未绑定)"),
        port = ports.server,
        isolated = rx_is_isolated(&listener),
        "开始监听"
    );

    let mut buf = vec![0u8; BUF];
    loop {
        let (n, from) = match rx.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "收包失败");
                continue;
            }
        };

        let req = match Message::decode(&mut Decoder::new(&buf[..n])) {
            Ok(m) => m,
            Err(e) => {
                debug!(%from, error = %e, "丢弃无法解析的报文");
                continue;
            }
        };

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

        if let Err(e) = tx.send_to(&out, SocketAddr::from(target)).await {
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
