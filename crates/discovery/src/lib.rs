//! 发现已配置静态 IP 的设备。
//!
//! DHCP 只能看见来要地址的机器。上架时遇到的另一半情况是：设备已经被
//! 配过静态地址 —— 二手机器、别人配过的、或者你自己上次配完忘了 ——
//! 它不会发 DHCP 请求，等多久都等不到。
//!
//! 这里用三种互补的办法，都不需要抓包驱动、不需要 raw socket：
//!
//! 1. **RMCP Presence Ping**（UDP 623）—— IPMI 自带的发现机制。
//!    最准，能直接确认"这是一台 BMC"。
//! 2. **UDP 探测 + 邻居表** —— 往候选地址发个 UDP 包逼操作系统做 ARP，
//!    再读邻居表。绕开了 raw socket，三平台通用。
//! 3. **被动读邻居表** —— 设备只要在网上说过话就会被系统记下。
//!
//! 三者的共同前提是"和设备在同一个二层"，正是机房里直连网线的场景。

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use lessor_core::MacAddr;
use lessor_net::Ipv4Cidr;
use serde::Serialize;
use tokio::net::UdpSocket;
use tracing::debug;

pub mod neighbor;
pub mod rmcp;

pub use neighbor::Neighbor;

/// 是怎么发现这台设备的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Method {
    /// 回应了 IPMI 的 RMCP Presence Ping
    Rmcp,
    /// UDP 探测之后出现在邻居表里
    Probed,
    /// 本来就在邻居表里 —— 它主动说过话
    Neighbor,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub ip: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<MacAddr>,
    /// 所有发现到它的途径
    pub via: Vec<Method>,
    /// 回应了 RMCP 且声明支持 IPMI —— 基本可以确定是 BMC
    pub is_bmc: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// 在哪个网段上找。通常就是本机某块网卡的地址。
    pub on: Ipv4Cidr,
    /// 每一轮等待应答的时间
    pub wait: Duration,
    /// 是否逐个 UDP 探测整个网段。/24 是 254 个包，很快；
    /// 网段更大时代价成比例上升，所以给个开关。
    pub sweep: bool,
    /// 网段大于这个地址数就不做逐个探测，避免误用在大网段上
    pub sweep_limit: u32,
}

impl Options {
    pub fn new(on: Ipv4Cidr) -> Self {
        Self {
            on,
            wait: Duration::from_millis(1500),
            sweep: true,
            sweep_limit: 1024,
        }
    }
}

fn hosts(cidr: Ipv4Cidr) -> Vec<Ipv4Addr> {
    let mask = if cidr.prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX
            .checked_shl(32 - u32::from(cidr.prefix))
            .unwrap_or(0)
    };
    let net = u32::from(cidr.addr) & mask;
    let bcast = net | !mask;
    if bcast <= net + 1 {
        return Vec::new();
    }
    ((net + 1)..bcast).map(Ipv4Addr::from).collect()
}

fn broadcast_of(cidr: Ipv4Cidr) -> Ipv4Addr {
    let mask = if cidr.prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX
            .checked_shl(32 - u32::from(cidr.prefix))
            .unwrap_or(0)
    };
    Ipv4Addr::from((u32::from(cidr.addr) & mask) | !mask)
}

/// 往每个候选地址发一个 UDP 包，逼操作系统去做 ARP。
///
/// 端口选 9（discard）—— 对方不回也没关系，我们要的只是链路层的
/// 那次 ARP 交互，之后去邻居表里捡结果。
async fn provoke(targets: &[Ipv4Addr], bind: Ipv4Addr) -> std::io::Result<()> {
    let sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(bind), 0)).await?;
    for t in targets {
        let _ = sock.send_to(b"", SocketAddr::new(IpAddr::V4(*t), 9)).await;
    }
    Ok(())
}

/// 在指定网段上找设备。
pub async fn scan(opts: Options) -> Vec<Device> {
    let mut found: BTreeMap<Ipv4Addr, Device> = BTreeMap::new();

    let mut add =
        |ip: Ipv4Addr, mac: Option<MacAddr>, via: Method, bmc: bool, note: Option<String>| {
            let e = found.entry(ip).or_insert_with(|| Device {
                ip,
                mac: None,
                via: Vec::new(),
                is_bmc: false,
                note: None,
            });
            if e.mac.is_none() {
                e.mac = mac;
            }
            if !e.via.contains(&via) {
                e.via.push(via);
            }
            e.is_bmc |= bmc;
            if e.note.is_none() {
                e.note = note;
            }
        };

    // 先记下已经在表里的 —— 这些是设备主动说过话留下的
    let before: Vec<Neighbor> = neighbor::neighbors();
    for n in &before {
        if n.ip != opts.on.addr {
            add(n.ip, Some(n.mac), Method::Neighbor, false, None);
        }
    }

    // RMCP：既广播也逐个单播。不少 BMC 只认单播。
    let mut rmcp_targets = vec![broadcast_of(opts.on)];
    let all = hosts(opts.on);
    let small_enough = (all.len() as u32) <= opts.sweep_limit;
    if opts.sweep && small_enough {
        rmcp_targets.extend(all.iter().copied().filter(|h| *h != opts.on.addr));
    }
    match rmcp::sweep(&rmcp_targets, opts.on.addr, opts.wait).await {
        Ok(rs) => {
            for r in rs {
                add(
                    r.addr,
                    None,
                    Method::Rmcp,
                    r.pong.supports_ipmi,
                    Some(if r.pong.supports_ipmi {
                        "回应 IPMI/RMCP".to_owned()
                    } else {
                        format!("回应 RMCP（OEM {:#x}）", r.pong.oem_iana)
                    }),
                );
            }
        }
        Err(e) => debug!(error = %e, "RMCP 探测失败"),
    }

    // UDP 探测整个网段，然后看邻居表多出了谁
    if opts.sweep && small_enough {
        let targets: Vec<Ipv4Addr> = all.into_iter().filter(|h| *h != opts.on.addr).collect();
        if let Err(e) = provoke(&targets, opts.on.addr).await {
            debug!(error = %e, "UDP 探测失败");
        }
        tokio::time::sleep(opts.wait).await;
        for n in neighbor::neighbors() {
            if n.ip == opts.on.addr {
                continue;
            }
            let known = before.iter().any(|b| b.ip == n.ip);
            add(
                n.ip,
                Some(n.mac),
                if known {
                    Method::Neighbor
                } else {
                    Method::Probed
                },
                false,
                None,
            );
        }
    } else if opts.sweep {
        debug!(
            hosts = hosts(opts.on).len(),
            limit = opts.sweep_limit,
            "网段过大，跳过逐个探测"
        );
    }

    // BMC 排前面，其余按地址排
    let mut out: Vec<Device> = found.into_values().collect();
    out.sort_by_key(|d| (!d.is_bmc, u32::from(d.ip)));
    out
}

// ---------- 给冲突探测用的原语 ----------

/// 探一批地址，返回其中已经有人应答的（以及是谁）。
///
/// 复用"发 UDP 逼系统做 ARP、再读邻居表"这套 —— 不需要 raw socket，
/// 三个平台通用，也不需要任何特权。
///
/// 探不到不等于地址空闲：设备可能关着机、或防火墙不理 ARP 之外的包。
/// 所以调用方应当把结果当作"已知被占"的白名单，而不是"未列出即空闲"
/// 的证明。
pub async fn probe_occupied(
    targets: &[Ipv4Addr],
    bind: Ipv4Addr,
) -> Vec<(Ipv4Addr, Option<MacAddr>)> {
    if targets.is_empty() {
        return Vec::new();
    }
    let _ = provoke(targets, bind).await;
    // 给系统一点时间完成 ARP 交互并写进邻居表
    tokio::time::sleep(Duration::from_millis(600)).await;

    let table = neighbor::neighbors();
    targets
        .iter()
        .filter_map(|ip| {
            table
                .iter()
                .find(|n| n.ip == *ip)
                .map(|n| (*ip, Some(n.mac)))
        })
        .collect()
}

/// 拼一个最小的 DHCPDISCOVER，用来探同网段还有没有别的 DHCP 服务器。
///
/// 只带 option 53，不带 option 55 —— 我们不关心对方会给什么参数，
/// 只关心它会不会应答。
pub fn dhcp_discover(xid: u32, mac: [u8; 6]) -> Vec<u8> {
    let mut p = Vec::with_capacity(300);
    p.push(1); // op: BOOTREQUEST
    p.push(1); // htype: 以太网
    p.push(6); // hlen
    p.push(0); // hops
    p.extend_from_slice(&xid.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // secs
    p.extend_from_slice(&0x8000u16.to_be_bytes()); // flags: 要求广播应答
    p.extend_from_slice(&[0; 16]); // ciaddr/yiaddr/siaddr/giaddr
    p.extend_from_slice(&mac);
    p.extend_from_slice(&[0; 10]); // chaddr 补齐 16 字节
    p.extend_from_slice(&[0; 192]); // sname + file
    p.extend_from_slice(&[0x63, 0x82, 0x53, 0x63]); // magic cookie
    p.extend_from_slice(&[53, 1, 1]); // option 53 = DISCOVER
    p.push(0xff);
    p
}

/// 这个包是不是对我们那次探测的应答（OFFER 或 ACK，且 xid 对得上）。
pub fn is_dhcp_reply_to(buf: &[u8], xid: u32) -> bool {
    // 头部 240 字节 + 至少一个选项
    if buf.len() < 241 || buf[0] != 2 {
        return false;
    }
    if u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != xid {
        return false;
    }
    if buf[236..240] != [0x63, 0x82, 0x53, 0x63] {
        return false;
    }
    // 找 option 53，看是不是 OFFER(2) / ACK(5)
    let mut i = 240;
    while i < buf.len() && buf[i] != 0xff {
        if buf[i] == 0 {
            i += 1;
            continue;
        }
        let Some(&len) = buf.get(i + 1) else { break };
        let len = len as usize;
        if buf[i] == 53 {
            return matches!(buf.get(i + 2), Some(2 | 5));
        }
        i += 2 + len;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(a: u8, b: u8, c: u8, d: u8, p: u8) -> Ipv4Cidr {
        Ipv4Cidr {
            addr: Ipv4Addr::new(a, b, c, d),
            prefix: p,
        }
    }

    #[test]
    fn hosts_excludes_network_and_broadcast() {
        let h = hosts(cidr(192, 168, 88, 1, 24));
        assert_eq!(h.len(), 254);
        assert_eq!(h[0], Ipv4Addr::new(192, 168, 88, 1));
        assert_eq!(h[253], Ipv4Addr::new(192, 168, 88, 254));
        assert!(!h.contains(&Ipv4Addr::new(192, 168, 88, 0)));
        assert!(!h.contains(&Ipv4Addr::new(192, 168, 88, 255)));
    }

    #[test]
    fn broadcast_is_computed_from_the_prefix() {
        assert_eq!(
            broadcast_of(cidr(192, 168, 88, 37, 24)),
            Ipv4Addr::new(192, 168, 88, 255)
        );
        assert_eq!(
            broadcast_of(cidr(10, 0, 0, 5, 30)),
            Ipv4Addr::new(10, 0, 0, 7)
        );
    }

    #[test]
    fn tiny_subnets_have_no_hosts_to_scan() {
        // /31 和 /32 上没有可枚举的主机位，不该产生无意义的探测
        assert!(hosts(cidr(10, 0, 0, 1, 31)).is_empty());
        assert!(hosts(cidr(10, 0, 0, 1, 32)).is_empty());
    }

    #[test]
    fn a_large_subnet_exceeds_the_sweep_limit() {
        let o = Options::new(cidr(10, 0, 0, 1, 16));
        assert!(
            hosts(o.on).len() as u32 > o.sweep_limit,
            "/16 应当超过逐个探测的上限，避免误用"
        );
        let small = Options::new(cidr(10, 0, 0, 1, 24));
        assert!((hosts(small.on).len() as u32) <= small.sweep_limit);
    }
}
