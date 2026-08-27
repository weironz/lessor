//! 客户端标识与 IPv4 地址区间。

use core::fmt;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

/// 以太网 MAC 地址。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; 6] = bytes.get(..6)?.try_into().ok()?;
        Some(Self(arr))
    }

    /// 厂商 OUI，用于识别虚拟网卡等。
    pub fn oui(&self) -> [u8; 3] {
        [self.0[0], self.0[1], self.0[2]]
    }

    pub fn is_locally_administered(&self) -> bool {
        self.0[0] & 0b0000_0010 != 0
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl fmt::Debug for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MacAddr({self})")
    }
}

/// 客户端身份。
///
/// RFC 2131 规定：客户端若送了 option 61（client identifier），服务端必须用它
/// 而不是 chaddr 来索引租约 —— 否则同一台机器换了网卡就会拿到不同的租约，
/// 而 PXE 引导过程中固件和操作系统用的标识往往不同。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ClientId {
    /// option 61 的原始字节
    Opt61(Vec<u8>),
    /// 回退到硬件地址
    Mac(MacAddr),
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mac(m) => write!(f, "{m}"),
            Self::Opt61(raw) => {
                write!(f, "id:")?;
                for b in raw {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }
    }
}

/// 闭区间的 IPv4 地址范围。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Range {
    pub start: Ipv4Addr,
    pub end: Ipv4Addr,
}

impl Range {
    /// `start` 必须不大于 `end`，否则返回 `None`。
    pub fn new(start: Ipv4Addr, end: Ipv4Addr) -> Option<Self> {
        (u32::from(start) <= u32::from(end)).then_some(Self { start, end })
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let n = u32::from(ip);
        n >= u32::from(self.start) && n <= u32::from(self.end)
    }

    pub fn len(&self) -> u64 {
        u64::from(u32::from(self.end) - u32::from(self.start)) + 1
    }

    pub fn is_empty(&self) -> bool {
        false // 闭区间至少含一个地址
    }

    pub fn iter(&self) -> impl Iterator<Item = Ipv4Addr> + use<> {
        (u32::from(self.start)..=u32::from(self.end)).map(Ipv4Addr::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_display_and_oui() {
        let m = MacAddr([0xac, 0x1f, 0x6b, 0x8e, 0x00, 0x01]);
        assert_eq!(m.to_string(), "ac:1f:6b:8e:00:01");
        assert_eq!(m.oui(), [0xac, 0x1f, 0x6b]);
        assert!(!m.is_locally_administered());
        assert!(MacAddr([0x02, 0, 0, 0, 0, 1]).is_locally_administered());
    }

    #[test]
    fn range_rejects_inverted() {
        let a = Ipv4Addr::new(192, 168, 1, 10);
        let b = Ipv4Addr::new(192, 168, 1, 20);
        assert!(Range::new(a, b).is_some());
        assert!(Range::new(b, a).is_none());
    }

    #[test]
    fn range_contains_and_len() {
        let r = Range::new(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(10, 0, 0, 9)).unwrap();
        assert_eq!(r.len(), 5);
        assert!(r.contains(Ipv4Addr::new(10, 0, 0, 5)));
        assert!(r.contains(Ipv4Addr::new(10, 0, 0, 9)));
        assert!(!r.contains(Ipv4Addr::new(10, 0, 0, 10)));
        assert_eq!(r.iter().count(), 5);
    }

    #[test]
    fn range_spanning_octet_boundary() {
        let r = Range::new(Ipv4Addr::new(10, 0, 0, 254), Ipv4Addr::new(10, 0, 1, 2)).unwrap();
        assert_eq!(r.len(), 5);
        assert!(r.contains(Ipv4Addr::new(10, 0, 1, 0)));
    }
}
