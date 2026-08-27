//! RMCP Presence Ping —— IPMI 自带的发现机制。
//!
//! BMC 只要开着 IPMI over LAN 就会应答，**和它配的是什么 IP 无关**。
//! 这正是 DHCP 等不到设备时最有用的一招：机器已经配了静态地址，
//! 不会来要地址，但它仍然会回应这个探测。
//!
//! 报文格式见 ASF 2.0 规范 §3.2.4.2。整个请求只有 12 字节，
//! 用普通 UDP socket 就能发，不需要任何特权。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;

/// IPMI over LAN 的端口。
pub const PORT: u16 = 623;

/// ASF 的 IANA 企业号 4542。
const ASF_IANA: u32 = 0x0000_11BE;

const RMCP_VERSION_1_0: u8 = 0x06;
const RMCP_CLASS_ASF: u8 = 0x06;
/// 序号 255 表示"不需要确认"
const RMCP_SEQ_NO_ACK: u8 = 0xFF;

const ASF_PRESENCE_PING: u8 = 0x80;
const ASF_PRESENCE_PONG: u8 = 0x40;

/// 构造一个 Presence Ping。`tag` 用来把应答和请求对上。
pub fn ping(tag: u8) -> [u8; 12] {
    let iana = ASF_IANA.to_be_bytes();
    [
        RMCP_VERSION_1_0,
        0x00, // 保留
        RMCP_SEQ_NO_ACK,
        RMCP_CLASS_ASF,
        iana[0],
        iana[1],
        iana[2],
        iana[3],
        ASF_PRESENCE_PING,
        tag,
        0x00, // 保留
        0x00, // 数据长度
    ]
}

/// Presence Pong 里带的信息。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pong {
    pub tag: u8,
    /// 设备的 IANA 企业号。0x11BE 表示它只实现了 ASF，
    /// 其它值一般是厂商自己的号。
    pub oem_iana: u32,
    /// 是否支持 IPMI —— 这是判断"这是不是一台 BMC"的关键位。
    pub supports_ipmi: bool,
}

/// 解析一个可能的 Pong。不是 Pong 就返回 `None`。
pub fn parse_pong(buf: &[u8]) -> Option<Pong> {
    // RMCP 头 4 + ASF 头 8 + 至少 16 字节数据
    if buf.len() < 28 {
        return None;
    }
    if buf[0] != RMCP_VERSION_1_0 || buf[3] != RMCP_CLASS_ASF {
        return None;
    }
    if u32::from_be_bytes(buf[4..8].try_into().ok()?) != ASF_IANA {
        return None;
    }
    if buf[8] != ASF_PRESENCE_PONG {
        return None;
    }

    // 数据区从第 12 字节开始：
    //   0..4   OEM IANA
    //   4..8   OEM defined
    //   8      supported entities —— bit 7 置位表示支持 IPMI
    let data = &buf[12..];
    Some(Pong {
        tag: buf[9],
        oem_iana: u32::from_be_bytes(data[0..4].try_into().ok()?),
        supports_ipmi: data[8] & 0x80 != 0,
    })
}

/// 一次探测的结果。
#[derive(Clone, Copy, Debug)]
pub struct Responder {
    pub addr: Ipv4Addr,
    pub pong: Pong,
}

/// 向一批地址（可以包含广播地址）发 Presence Ping，收集应答。
///
/// 广播能一次问遍整个网段，但不少 BMC 只回应单播 —— 所以两种都发。
pub async fn sweep(
    targets: &[Ipv4Addr],
    bind: Ipv4Addr,
    wait: Duration,
) -> std::io::Result<Vec<Responder>> {
    let sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(bind), 0)).await?;
    sock.set_broadcast(true)?;

    let tag = 0x42;
    let pkt = ping(tag);
    for t in targets {
        // 发不出去的地址（比如路由不可达）跳过就好，不该中断整轮探测
        let _ = sock.send_to(&pkt, SocketAddr::new(IpAddr::V4(*t), PORT)).await;
    }

    let mut found: Vec<Responder> = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = [0u8; 256];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
            Err(_) => break,
            Ok(Err(_)) => continue,
            Ok(Ok((n, from))) => {
                let IpAddr::V4(addr) = from.ip() else { continue };
                let Some(pong) = parse_pong(&buf[..n]) else {
                    continue;
                };
                if pong.tag == tag && !found.iter().any(|r| r.addr == addr) {
                    found.push(Responder { addr, pong });
                }
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_matches_the_asf_layout() {
        let p = ping(0x42);
        assert_eq!(p.len(), 12);
        assert_eq!(p[0], 0x06, "RMCP 版本 1.0");
        assert_eq!(p[2], 0xFF, "序号 255 = 不需要确认");
        assert_eq!(p[3], 0x06, "消息类别 ASF");
        assert_eq!(&p[4..8], &[0x00, 0x00, 0x11, 0xBE], "IANA 4542");
        assert_eq!(p[8], 0x80, "Presence Ping");
        assert_eq!(p[9], 0x42, "标签原样带出");
        assert_eq!(p[11], 0x00, "数据长度为 0");
    }

    /// 拼一个 Pong，用于测试解析。
    fn pong_bytes(tag: u8, oem: u32, ipmi: bool) -> Vec<u8> {
        let mut v = vec![0x06, 0x00, 0xFF, 0x06];
        v.extend_from_slice(&ASF_IANA.to_be_bytes());
        v.push(ASF_PRESENCE_PONG);
        v.push(tag);
        v.push(0x00);
        v.push(16); // 数据长度
        v.extend_from_slice(&oem.to_be_bytes()); // OEM IANA
        v.extend_from_slice(&[0, 0, 0, 0]); // OEM defined
        v.push(if ipmi { 0x81 } else { 0x01 }); // supported entities
        v.extend_from_slice(&[0; 7]);
        v
    }

    #[test]
    fn parses_a_bmc_pong() {
        let p = parse_pong(&pong_bytes(0x42, 0x11BE, true)).expect("应能解析");
        assert_eq!(p.tag, 0x42);
        assert_eq!(p.oem_iana, 0x11BE);
        assert!(p.supports_ipmi, "bit 7 置位表示支持 IPMI");
    }

    #[test]
    fn recognises_a_non_ipmi_responder() {
        let p = parse_pong(&pong_bytes(1, 0, false)).unwrap();
        assert!(!p.supports_ipmi, "不置 bit 7 的设备不是 BMC");
    }

    #[test]
    fn rejects_packets_that_are_not_pongs() {
        assert!(parse_pong(&[]).is_none(), "空包");
        assert!(parse_pong(&ping(1)).is_none(), "自己发的 Ping 不是 Pong");

        let mut short = pong_bytes(1, 0, true);
        short.truncate(20);
        assert!(parse_pong(&short).is_none(), "数据区不完整");

        let mut wrong_class = pong_bytes(1, 0, true);
        wrong_class[3] = 0x07;
        assert!(parse_pong(&wrong_class).is_none(), "类别不是 ASF");

        let mut wrong_iana = pong_bytes(1, 0, true);
        wrong_iana[7] = 0xFF;
        assert!(parse_pong(&wrong_iana).is_none(), "IANA 号不对");
    }

    #[test]
    fn tag_lets_us_ignore_other_peoples_replies() {
        // 网段上可能有别的工具也在探测，标签不匹配的应答要能区分出来
        let ours = parse_pong(&pong_bytes(0x42, 0x11BE, true)).unwrap();
        let theirs = parse_pong(&pong_bytes(0x07, 0x11BE, true)).unwrap();
        assert_ne!(ours.tag, theirs.tag);
    }
}
