//! 客户端标识与 IPv4 地址区间。

use core::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 以太网 MAC 地址。
///
/// 序列化成 `"ac:1f:6b:8e:00:01"` 而不是字节数组 —— 这个类型会直接出现在
/// 给前端的 JSON 里，字节数组既不可读也不便于在界面上搜索比对。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const ZERO: Self = Self([0; 6]);

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; 6] = bytes.get(..6)?.try_into().ok()?;
        Some(Self(arr))
    }

    /// 厂商 OUI，用于识别虚拟网卡等。
    pub fn oui(&self) -> [u8; 3] {
        [self.0[0], self.0[1], self.0[2]]
    }

    /// 全零 —— 不是一个有效的客户端硬件地址。
    pub fn is_zero(&self) -> bool {
        self.0 == [0; 6]
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("不是合法的 MAC 地址: {0}")]
pub struct ParseMacError(pub String);

impl FromStr for MacAddr {
    type Err = ParseMacError;

    /// 宽容地接受 `aa:bb:cc:dd:ee:ff`、`aa-bb-cc-dd-ee-ff`、`aabb.ccdd.eeff`
    /// 和裸十六进制 —— 各家 BMC 和交换机的输出格式不统一。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex: Vec<char> = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.len() != 12 {
            return Err(ParseMacError(s.to_owned()));
        }
        let mut out = [0u8; 6];
        for (i, byte) in out.iter_mut().enumerate() {
            let pair: String = hex[i * 2..i * 2 + 2].iter().collect();
            *byte = u8::from_str_radix(&pair, 16).map_err(|_| ParseMacError(s.to_owned()))?;
        }
        Ok(Self(out))
    }
}

impl Serialize for MacAddr {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MacAddr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// 客户端身份。
///
/// RFC 2131 规定：客户端若送了 option 61（client identifier），服务端必须用它
/// 而不是 chaddr 来索引租约 —— 否则同一台机器换了网卡就会拿到不同的租约，
/// 而 PXE 引导过程中固件和操作系统用的标识往往不同。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum ClientId {
    /// option 61 的原始字节，序列化为十六进制串
    Opt61(#[serde(with = "hex_bytes")] Vec<u8>),
    /// 回退到硬件地址
    Mac(MacAddr),
}

impl ClientId {
    /// 按 RFC 2131 的优先级构造：有 option 61 就用它，否则用 MAC。
    pub fn from_parts(opt61: Option<&[u8]>, mac: MacAddr) -> Self {
        match opt61 {
            Some(raw) if !raw.is_empty() => Self::Opt61(raw.to_vec()),
            _ => Self::Mac(mac),
        }
    }
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

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() % 2 != 0 {
            return Err(serde::de::Error::custom("十六进制串长度必须是偶数"));
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(serde::de::Error::custom))
            .collect()
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

    /// 两个区间是否相交。
    pub fn overlaps(&self, other: &Self) -> bool {
        u32::from(self.start) <= u32::from(other.end)
            && u32::from(other.start) <= u32::from(self.end)
    }

    pub fn iter(&self) -> impl Iterator<Item = Ipv4Addr> + use<> {
        (u32::from(self.start)..=u32::from(self.end)).map(Ipv4Addr::from)
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
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
        assert!(MacAddr::ZERO.is_zero());
    }

    #[test]
    fn mac_parses_common_formats() {
        let want = MacAddr([0xac, 0x1f, 0x6b, 0x8e, 0x00, 0x01]);
        for s in [
            "ac:1f:6b:8e:00:01",
            "AC-1F-6B-8E-00-01",
            "ac1f.6b8e.0001",
            "ac1f6b8e0001",
        ] {
            assert_eq!(s.parse::<MacAddr>().unwrap(), want, "解析失败: {s}");
        }
    }

    #[test]
    fn mac_rejects_wrong_length() {
        assert!("ac:1f:6b:8e:00".parse::<MacAddr>().is_err());
        assert!("ac:1f:6b:8e:00:01:02".parse::<MacAddr>().is_err());
        assert!("".parse::<MacAddr>().is_err());
        assert!("zz:zz:zz:zz:zz:zz".parse::<MacAddr>().is_err());
    }

    #[test]
    fn mac_json_is_a_readable_string() {
        let m = MacAddr([0xac, 0x1f, 0x6b, 0x8e, 0x00, 0x01]);
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(j, r#""ac:1f:6b:8e:00:01""#, "前端要能直接读");
        assert_eq!(serde_json::from_str::<MacAddr>(&j).unwrap(), m);
    }

    #[test]
    fn client_id_roundtrips_through_json() {
        for id in [
            ClientId::Mac(MacAddr([1, 2, 3, 4, 5, 6])),
            ClientId::Opt61(vec![0xff, 0xde, 0xad]),
        ] {
            let j = serde_json::to_string(&id).unwrap();
            assert_eq!(serde_json::from_str::<ClientId>(&j).unwrap(), id);
        }
    }

    #[test]
    fn client_id_prefers_option_61() {
        let mac = MacAddr([1, 2, 3, 4, 5, 6]);
        assert_eq!(
            ClientId::from_parts(Some(&[9, 9]), mac),
            ClientId::Opt61(vec![9, 9])
        );
        // 空的 option 61 视同没有
        assert_eq!(ClientId::from_parts(Some(&[]), mac), ClientId::Mac(mac));
        assert_eq!(ClientId::from_parts(None, mac), ClientId::Mac(mac));
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

    #[test]
    fn range_overlap_detection() {
        let a = Range::new(Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 10)).unwrap();
        let touching = Range::new(Ipv4Addr::new(10, 0, 0, 10), Ipv4Addr::new(10, 0, 0, 20)).unwrap();
        let apart = Range::new(Ipv4Addr::new(10, 0, 0, 11), Ipv4Addr::new(10, 0, 0, 20)).unwrap();
        assert!(a.overlaps(&touching), "共用一个端点也算相交");
        assert!(!a.overlaps(&apart));
    }
}
