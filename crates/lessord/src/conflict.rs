//! 地址冲突探测与同段其他 DHCP 服务器告警。
//!
//! 兑现两条安全红线：OFFER 前确认候选地址没被别人静态占用；
//! 发现同网段还有别的 DHCP 服务器时告警，而不是闷头抢答。
//!
//! **设计要点：探测不在握手路径上。** DHCP 客户端等不起一次 ARP 往返
//! （几百毫秒），而且探测失败也不该让客户端拿不到地址。所以做法是
//! 后台持续探测、结果进缓存，分配时只查缓存 —— 查缓存是纳秒级的。
//! 代价是刚启动的头一轮还没有数据，那时按"没冲突"放行；这比让每个
//! 客户端多等半秒划算，何况静态占用本来就该在网段规划时避免。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::warn;

/// 一条探测结果的有效期。太短会让探测太频繁，太长会让刚下线的设备
/// 长期占着地址不放。
const TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
struct Seen {
    at: Instant,
    /// 谁占着 —— 记下来是为了在事件里说清楚，光说"被占了"没法排查
    mac: Option<lessor_core::MacAddr>,
}

/// 已知被静态占用的地址。
///
/// 由后台任务填充，分配路径只读。
#[derive(Clone, Default)]
pub struct Occupied(Arc<RwLock<HashMap<Ipv4Addr, Seen>>>);

impl Occupied {
    /// 这个地址此刻是否已知被别人占着。
    ///
    /// 只查缓存，不做任何 IO —— 这是它能放在分配路径上的前提。
    pub fn is_taken(&self, ip: Ipv4Addr) -> bool {
        let Ok(g) = self.0.read() else {
            // 锁中毒时宁可放行：拒绝分配会让整个服务停摆，
            // 而误发一个地址至多引起一次冲突，客户端会 DECLINE
            return false;
        };
        g.get(&ip).is_some_and(|s| s.at.elapsed() < TTL)
    }

    /// 探测挡掉了哪些地址，给界面一句人话。
    ///
    /// 池满时只说"地址池已耗尽"没法排查 —— 得让人知道是不是有一批地址
    /// 被别人静态占着。
    pub fn blocked_summary(&self) -> String {
        let Ok(g) = self.0.read() else {
            return String::new();
        };
        let mut live: Vec<_> = g.iter().filter(|(_, s)| s.at.elapsed() < TTL).collect();
        if live.is_empty() {
            return String::new();
        }
        // HashMap 的迭代顺序每次都不同，不排的话同一件事每次措辞都变，
        // 日志里没法比对
        live.sort_by_key(|(ip, _)| **ip);
        let sample = live
            .iter()
            .take(3)
            .map(|(ip, s)| match s.mac {
                Some(m) => format!("{ip} 被 {m} 占用"),
                None => format!("{ip} 已被占用"),
            })
            .collect::<Vec<_>>()
            .join("、");
        if live.len() > 3 {
            format!("探测到 {} 个地址被静态占用：{sample} 等", live.len())
        } else {
            format!("探测到 {sample}")
        }
    }

    fn record(&self, ip: Ipv4Addr, mac: Option<lessor_core::MacAddr>) {
        if let Ok(mut g) = self.0.write() {
            g.insert(
                ip,
                Seen {
                    at: Instant::now(),
                    mac,
                },
            );
        }
    }

    fn forget_stale(&self) {
        if let Ok(mut g) = self.0.write() {
            g.retain(|_, s| s.at.elapsed() < TTL);
        }
    }

    #[cfg(test)]
    fn mark_for_test(&self, ip: Ipv4Addr) {
        self.record(ip, None);
    }
}

/// 后台探测循环：定期扫一遍作用域里还没发出去的地址，
/// 把已经有人应答的记下来。
///
/// 扫描的是"池里尚未分配"的地址 —— 已经发出去的由客户端自己负责，
/// 它们冲突的话会走 DECLINE 流程。
pub async fn sweeper(state: crate::state::AppState, occupied: Occupied, every: Duration) {
    // 缓存为空的这段时间是分配的盲区 —— 会照发不误。所以首轮不等满一个
    // 周期，只避开启动那几秒（那时监听器还没起来、作用域可能还没配）。
    tokio::time::sleep(Duration::from_secs(3)).await;
    // interval 的第一拍是立刻返回的，所以这里就是"睡够 3 秒就开扫"
    let mut tick = tokio::time::interval(every);

    loop {
        tick.tick().await;
        occupied.forget_stale();

        let (scopes, listeners) = state.scopes_and_listeners().await;
        for scope in &scopes {
            if !scope.enabled {
                continue;
            }
            let Some(bind) = listeners
                .iter()
                .find(|l| scope.contains(l.server_ip))
                .map(|l| l.server_ip)
            else {
                continue;
            };

            // 只探还没分出去的地址。一轮最多探 256 个，网段大时分批轮转，
            // 免得一次灌太多 ARP 出去把交换机惹毛。
            let leased = state.leased_ips(scope.id).await;
            let candidates: Vec<Ipv4Addr> = scope
                .poolable_addrs()
                .filter(|ip| !leased.contains(ip) && *ip != bind)
                .take(256)
                .collect();
            if candidates.is_empty() {
                continue;
            }

            for (ip, mac) in discovery::probe_occupied(&candidates, bind).await {
                occupied.record(ip, mac);
            }
        }
    }
}

/// 探测用的端口对固定是 67/68，**不跟随 `--dhcp-port`**：我们探的是
/// 别人家的服务器，它们守在标准端口上，跟我们自己听哪儿没关系。
const FOREIGN_SERVER_PORT: u16 = 67;
const FOREIGN_CLIENT_PORT: u16 = 68;

/// 建一个"以客户端身份收包"的 socket。
///
/// 单独抽出来是因为端口的选择就是这里唯一容易错、且错了会静默的地方。
fn probe_socket(bind: Ipv4Addr, client_port: u16) -> Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // 客户端端口上通常已经有系统自己的 DHCP 客户端占着；
    // 不设 REUSEADDR 就会绑不上，于是整条检查退化成"查不了"
    sock.set_reuse_address(true)?;
    sock.set_broadcast(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&std::net::SocketAddrV4::new(bind, client_port).into())
        .with_context(|| {
            format!("绑不上 {bind}:{client_port} —— 无法检查同网段是否已有 DHCP 服务器")
        })?;
    Ok(tokio::net::UdpSocket::from_std(std::net::UdpSocket::from(
        sock,
    ))?)
}

/// 同网段有没有别的 DHCP 服务器。
///
/// 做法：以客户端身份广播一个 DISCOVER，看有没有别人应答。我们自己的
/// 监听器也会收到这个包，但它来自本机地址，按 xid 和来源一起排除掉。
///
/// 为什么值得做：把 DHCP 插进一个已经有 DHCP 的网段是会出事的 ——
/// 装机网段里 MAAS 正在发地址，我们抢答会让机器装到一半失联。
/// 检测到就告警，让人自己决定，而不是替他决定。
///
/// **必须绑 68 端口收**。DHCP 服务器把应答发给客户端端口，完全不看
/// 请求是从哪个端口来的 —— 绑临时端口就一个应答都收不到，然后报
/// "本网段没有其他 DHCP"。安全检查给出假的全清信号比不做还糟，
/// 所以绑不上时返回 `Err` 让调用方如实说"没查成"。
/// （这是 M2 那个"应答源端口必须是 67"的镜像，实测见
/// `docs/pxe-source-port.md`。）
pub async fn detect_foreign_servers(bind: Ipv4Addr, wait: Duration) -> Result<Vec<Ipv4Addr>> {
    let sock = probe_socket(bind, FOREIGN_CLIENT_PORT)?;

    // 用一个不会和真实客户端撞车的 MAC（本地管理位置 1），
    // 免得别的服务器给我们真发一个地址出来占着池子
    let mac = [0x02, b'l', b'e', b's', b's', 0x01];
    let xid = 0x1e55_0001_u32;
    let probe = discovery::dhcp_discover(xid, mac);
    sock.send_to(&probe, ("255.255.255.255", FOREIGN_SERVER_PORT))
        .await
        .context("发不出探测报文")?;

    let mut found = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = vec![0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(recv) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else {
            break;
        };
        // 收到一个坏包不该让整轮探测提前收工 —— 后面可能还有真应答
        let Ok((n, from)) = recv else { continue };
        let src = match from.ip() {
            std::net::IpAddr::V4(v4) => v4,
            std::net::IpAddr::V6(_) => continue,
        };
        // 自己的应答不算
        if src == bind {
            continue;
        }
        if discovery::is_dhcp_reply_to(&buf[..n], xid) && !found.contains(&src) {
            warn!(server = %src, "同网段检测到其他 DHCP 服务器 —— 两边同时发地址会打架");
            found.push(src);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_addresses_are_not_treated_as_taken() {
        // 缓存里没有的地址必须放行 —— 否则服务刚起来（缓存为空）
        // 会拒绝分配任何地址
        let occ = Occupied::default();
        assert!(!occ.is_taken(Ipv4Addr::new(192, 168, 1, 10)));
    }

    #[test]
    fn recorded_addresses_are_taken() {
        let occ = Occupied::default();
        let ip = Ipv4Addr::new(192, 168, 1, 10);
        occ.mark_for_test(ip);
        assert!(occ.is_taken(ip));
        assert!(!occ.is_taken(Ipv4Addr::new(192, 168, 1, 11)));
    }

    #[tokio::test]
    async fn probe_listens_on_the_client_port_not_an_ephemeral_one() {
        // 这条钉的是一个已经犯过的错：DHCP 服务器把应答发到客户端端口，
        // 根本不看请求从哪个端口来。绑临时端口就一个应答都收不到，
        // 于是"本网段有没有别的 DHCP"永远答"没有" —— 一个假的全清信号。
        // 实测：同一个 DISCOVER 打到 MAAS，绑 68 收到 OFFER，绑临时端口收到 0 个。
        let sock = probe_socket(Ipv4Addr::LOCALHOST, 16068).expect("应能绑上");
        assert_eq!(sock.local_addr().unwrap().port(), 16068);
    }

    #[tokio::test]
    async fn probe_socket_coexists_with_an_existing_client_on_that_port() {
        // 真实机器的 68 端口上几乎总有系统自己的 DHCP 客户端。
        // 没有 SO_REUSEADDR 的话第二个绑不上，检查就做不成了。
        let held = probe_socket(Ipv4Addr::LOCALHOST, 16069).expect("第一个应能绑上");
        let second = probe_socket(Ipv4Addr::LOCALHOST, 16069);
        assert!(second.is_ok(), "端口已被占用时仍应能绑上");
        drop(held);
    }
}
