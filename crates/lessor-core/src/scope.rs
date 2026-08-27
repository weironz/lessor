//! 作用域 —— 一个被管理的子网及其下发的配置。

use core::fmt;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addr::{ClientId, Range};

/// 作用域的稳定标识。名字可以改，租约引用的是这个。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ScopeId(pub u32);

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scope#{}", self.0)
    }
}

/// 静态保留：把某个客户端固定绑到某个地址。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    pub client: ClientId,
    pub ip: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hostname: Option<String>,
}

/// 客户端自报的网络引导方式。
///
/// 三种客户端要的东西完全不同：PXE 固件要 TFTP 上的文件名，
/// HTTP Boot 固件要一个完整 URL，已经跑起来的 iPXE 要一个引导脚本。
/// 发错了轻则不引导，重则无限自举（见 [`BootClient::Ipxe`]）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BootClient {
    /// 没有自报任何引导身份 —— 普通客户端，或不发 option 60 的老固件
    #[default]
    Plain,
    /// PXE 固件。option 60 以 `PXEClient` 开头
    Pxe,
    /// UEFI HTTP Boot 固件。option 60 以 `HTTPClient` 开头
    HttpBoot,
    /// 已经被引导起来的 iPXE。option 77（user class）为 `iPXE`
    ///
    /// **必须先于 option 60 判定**：iPXE 自己也发 `PXEClient:Arch:...`，
    /// 只看 option 60 会把它当成固件，于是又把 `ipxe.efi` 发回去 ——
    /// 它加载完再来问，再被发一次，无限自举。
    Ipxe,
}

/// 网络引导参数（PXE / iPXE / UEFI HTTP Boot）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootConfig {
    /// option 66 —— TFTP 服务器名
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub server_name: Option<String>,
    /// option 67 —— 引导文件名。默认值，给 PXE 固件和没自报身份的客户端。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub filename: Option<String>,
    /// siaddr 字段里的下一跳服务器
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next_server: Option<Ipv4Addr>,
    /// 给 UEFI HTTP Boot 客户端的引导 URL。必须是完整 URL，
    /// 它不会去 TFTP 取文件。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub http_url: Option<String>,
    /// 给已经跑起来的 iPXE 的引导脚本，通常是 `http://.../boot.ipxe`。
    /// 不配的话 iPXE 会拿到和固件一样的 [`BootConfig::filename`]，
    /// 那通常就是它自己 —— 无限自举。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ipxe_url: Option<String>,
}

impl BootConfig {
    /// 该给这类客户端发什么。`None` 表示没有它能用的东西，
    /// 那就干脆一个引导选项都不发 —— 发一个它用不了的比不发更糟。
    pub fn file_for(&self, client: BootClient) -> Option<&str> {
        match client {
            // 已经是 iPXE 了，要的是脚本。没配就退回默认值：
            // 行为和加这个特性之前一致，但很可能就是自举的那种配法。
            BootClient::Ipxe => self.ipxe_url.as_deref().or(self.filename.as_deref()),

            // HTTP Boot 固件只认 URL。没配专门的 URL 时，只有默认值
            // 本身就是 URL 才能用 —— 把 TFTP 文件名发给它没有意义，
            // 它会拿去当 URL 解析然后失败。
            BootClient::HttpBoot => self
                .http_url
                .as_deref()
                .or_else(|| self.filename.as_deref().filter(|f| looks_like_url(f))),

            BootClient::Pxe | BootClient::Plain => self.filename.as_deref(),
        }
    }

    /// 有没有配任何引导参数。
    pub fn is_empty(&self) -> bool {
        self.server_name.is_none()
            && self.filename.is_none()
            && self.next_server.is_none()
            && self.http_url.is_none()
            && self.ipxe_url.is_none()
    }
}

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// 配置校验发现的问题。
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ScopeError {
    #[error("前缀长度 {0} 不合法（应为 0-32）")]
    BadPrefix(u8),
    #[error("{subnet} 不是 /{prefix} 的网络地址，主机位不为零")]
    NotANetworkAddress { subnet: Ipv4Addr, prefix: u8 },
    #[error("地址池 {range} 超出子网 {subnet}/{prefix}")]
    PoolOutsideSubnet {
        range: Range,
        subnet: Ipv4Addr,
        prefix: u8,
    },
    #[error("地址池 {a} 与 {b} 相交")]
    PoolsOverlap { a: Range, b: Range },
    #[error("保留地址 {ip} 超出子网 {subnet}/{prefix}")]
    ReservationOutsideSubnet {
        ip: Ipv4Addr,
        subnet: Ipv4Addr,
        prefix: u8,
    },
    #[error("保留地址 {ip} 被 {count} 个客户端同时占用")]
    DuplicateReservation { ip: Ipv4Addr, count: usize },
    #[error("网关 {gw} 不在子网 {subnet}/{prefix} 内")]
    GatewayOutsideSubnet {
        gw: Ipv4Addr,
        subnet: Ipv4Addr,
        prefix: u8,
    },
    #[error("租期必须大于 0")]
    ZeroLease,
    #[error("OFFER 占位时长 {offer}s 不应超过租期 {lease}s")]
    OfferLongerThanLease { offer: u32, lease: u32 },
    #[error("作用域没有任何可分配地址")]
    NoUsableAddresses,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub id: ScopeId,
    pub name: String,
    /// 关掉之后该作用域不再应答任何请求，但配置和租约都保留。
    pub enabled: bool,
    /// 网络地址，如 192.168.88.0
    pub subnet: Ipv4Addr,
    /// 前缀长度，如 24
    pub prefix: u8,
    /// 可分配区间，按顺序使用
    pub pools: Vec<Range>,
    /// 从池中剔除的区间（比如中间几个地址留给交换机）
    pub exclusions: Vec<Range>,
    pub reservations: Vec<Reservation>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub router: Option<Ipv4Addr>,
    #[serde(default)]
    pub dns: Vec<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub domain: Option<String>,
    /// 正式租期
    pub lease_secs: u32,
    /// OFFER 的占位时长 —— 客户端没跟上 REQUEST 时地址要能尽快回收
    pub offer_secs: u32,
    /// 收到 DECLINE 后隔离该地址的时长
    pub decline_quarantine_secs: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub boot: Option<BootConfig>,
    /// 额外的原始选项 `(code, value)`，用于本结构没有专门字段的场景
    #[serde(default)]
    pub extra_options: Vec<(u8, Vec<u8>)>,
}

impl Scope {
    /// 一个够用的默认作用域，调用方按需覆盖字段。
    pub fn new(id: u32, name: impl Into<String>, subnet: Ipv4Addr, prefix: u8) -> Self {
        Self {
            id: ScopeId(id),
            name: name.into(),
            enabled: true,
            subnet,
            prefix,
            pools: Vec::new(),
            exclusions: Vec::new(),
            reservations: Vec::new(),
            router: None,
            dns: Vec::new(),
            domain: None,
            lease_secs: 3600,
            offer_secs: 30,
            decline_quarantine_secs: 3600,
            boot: None,
            extra_options: Vec::new(),
        }
    }

    /// 这个作用域有没有配 PXE 厂商选项（option 43）。
    ///
    /// 决定应答里要不要声明 option 60 = "PXEClient"。只声明不给 43 的话，
    /// 固件会去找根本不存在的引导服务器列表，然后什么都不做 ——
    /// 详见 `server.rs` 里填充 option 60 那段的实测对照表。
    pub fn has_pxe_vendor_options(&self) -> bool {
        const VENDOR_SPECIFIC: u8 = 43;
        self.extra_options
            .iter()
            .any(|(code, value)| *code == VENDOR_SPECIFIC && !value.is_empty())
    }

    pub fn netmask(&self) -> Ipv4Addr {
        let bits = if self.prefix >= 32 {
            u32::MAX
        } else {
            u32::MAX
                .checked_shl(32 - u32::from(self.prefix))
                .unwrap_or(0)
        };
        Ipv4Addr::from(bits)
    }

    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.subnet) | !u32::from(self.netmask()))
    }

    /// 该地址是否属于本作用域的子网。
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask());
        u32::from(ip) & mask == u32::from(self.subnet) & mask
    }

    /// 该地址是否可被动态分配（在某个池内、且不在排除区间内）。
    pub fn is_poolable(&self, ip: Ipv4Addr) -> bool {
        self.pools.iter().any(|p| p.contains(ip))
            && !self.exclusions.iter().any(|e| e.contains(ip))
            // 网络地址和广播地址永远不发
            && ip != self.subnet
            && ip != self.broadcast()
    }

    pub fn reservation_for(&self, client: &ClientId) -> Option<&Reservation> {
        self.reservations.iter().find(|r| &r.client == client)
    }

    /// 该地址是否被某个静态保留占着（用于避免动态分配踩到保留地址）。
    pub fn is_reserved_for_other(&self, ip: Ipv4Addr, client: &ClientId) -> bool {
        self.reservations
            .iter()
            .any(|r| r.ip == ip && &r.client != client)
    }

    /// 按顺序遍历所有可分配地址。
    pub fn poolable_addrs(&self) -> impl Iterator<Item = Ipv4Addr> + '_ {
        self.pools
            .iter()
            .flat_map(Range::iter)
            .filter(|ip| self.is_poolable(*ip))
    }

    /// 可动态分配的地址总数（已扣除排除区间、网络与广播地址）。
    pub fn capacity(&self) -> u64 {
        self.poolable_addrs().count() as u64
    }

    /// 检查配置是否自洽。返回所有发现的问题，而不是遇到第一个就停 ——
    /// 界面上一次把问题列全，比让人改一个再报一个体验好。
    pub fn validate(&self) -> Vec<ScopeError> {
        let mut errs = Vec::new();

        if self.prefix > 32 {
            errs.push(ScopeError::BadPrefix(self.prefix));
            // 后面的检查都依赖掩码，前缀不合法就没法继续
            return errs;
        }

        if u32::from(self.subnet) & !u32::from(self.netmask()) != 0 {
            errs.push(ScopeError::NotANetworkAddress {
                subnet: self.subnet,
                prefix: self.prefix,
            });
        }

        for p in &self.pools {
            if !self.contains(p.start) || !self.contains(p.end) {
                errs.push(ScopeError::PoolOutsideSubnet {
                    range: *p,
                    subnet: self.subnet,
                    prefix: self.prefix,
                });
            }
        }

        for (i, a) in self.pools.iter().enumerate() {
            for b in &self.pools[i + 1..] {
                if a.overlaps(b) {
                    errs.push(ScopeError::PoolsOverlap { a: *a, b: *b });
                }
            }
        }

        for r in &self.reservations {
            if !self.contains(r.ip) {
                errs.push(ScopeError::ReservationOutsideSubnet {
                    ip: r.ip,
                    subnet: self.subnet,
                    prefix: self.prefix,
                });
            }
        }

        // 同一个地址被多个客户端保留 —— 一定是配置错误
        let mut seen: Vec<(Ipv4Addr, usize)> = Vec::new();
        for r in &self.reservations {
            match seen.iter_mut().find(|(ip, _)| *ip == r.ip) {
                Some((_, n)) => *n += 1,
                None => seen.push((r.ip, 1)),
            }
        }
        for (ip, count) in seen.into_iter().filter(|(_, n)| *n > 1) {
            errs.push(ScopeError::DuplicateReservation { ip, count });
        }

        if let Some(gw) = self.router
            && !self.contains(gw)
        {
            errs.push(ScopeError::GatewayOutsideSubnet {
                gw,
                subnet: self.subnet,
                prefix: self.prefix,
            });
        }

        if self.lease_secs == 0 {
            errs.push(ScopeError::ZeroLease);
        } else if self.offer_secs > self.lease_secs {
            errs.push(ScopeError::OfferLongerThanLease {
                offer: self.offer_secs,
                lease: self.lease_secs,
            });
        }

        // 保留地址不占池子，所以只有池为空且没有保留时才算完全不可用
        if self.capacity() == 0 && self.reservations.is_empty() {
            errs.push(ScopeError::NoUsableAddresses);
        }

        errs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addr::MacAddr;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr::new(a, b, c, d)
    }

    fn scope24() -> Scope {
        let mut s = Scope::new(1, "lab", ip(192, 168, 88, 0), 24);
        s.pools = vec![Range::new(ip(192, 168, 88, 10), ip(192, 168, 88, 20)).unwrap()];
        s
    }

    #[test]
    fn netmask_and_broadcast() {
        let s = scope24();
        assert_eq!(s.netmask(), ip(255, 255, 255, 0));
        assert_eq!(s.broadcast(), ip(192, 168, 88, 255));

        let mut s30 = Scope::new(2, "p2p", ip(10, 0, 0, 0), 30);
        s30.prefix = 30;
        assert_eq!(s30.netmask(), ip(255, 255, 255, 252));
        assert_eq!(s30.broadcast(), ip(10, 0, 0, 3));

        let s32 = Scope::new(3, "host", ip(10, 0, 0, 1), 32);
        assert_eq!(s32.netmask(), ip(255, 255, 255, 255));
    }

    #[test]
    fn contains_only_own_subnet() {
        let s = scope24();
        assert!(s.contains(ip(192, 168, 88, 1)));
        assert!(!s.contains(ip(192, 168, 89, 1)));
    }

    #[test]
    fn exclusions_are_removed_from_pool() {
        let mut s = scope24();
        s.exclusions = vec![Range::new(ip(192, 168, 88, 15), ip(192, 168, 88, 16)).unwrap()];
        let addrs: Vec<_> = s.poolable_addrs().collect();
        assert_eq!(addrs.len(), 9, "11 个减去排除的 2 个");
        assert_eq!(s.capacity(), 9);
        assert!(!addrs.contains(&ip(192, 168, 88, 15)));
        assert!(addrs.contains(&ip(192, 168, 88, 14)));
    }

    #[test]
    fn network_and_broadcast_never_offered() {
        let mut s = Scope::new(4, "full", ip(10, 0, 0, 0), 24);
        s.pools = vec![Range::new(ip(10, 0, 0, 0), ip(10, 0, 0, 255)).unwrap()];
        assert_eq!(s.capacity(), 254);
        let addrs: Vec<_> = s.poolable_addrs().collect();
        assert!(!addrs.contains(&ip(10, 0, 0, 0)));
        assert!(!addrs.contains(&ip(10, 0, 0, 255)));
    }

    // ---------- 校验 ----------

    #[test]
    fn a_sane_scope_has_no_errors() {
        let mut s = scope24();
        s.router = Some(ip(192, 168, 88, 1));
        assert_eq!(s.validate(), vec![]);
    }

    #[test]
    fn catches_pool_outside_subnet() {
        let mut s = scope24();
        s.pools = vec![Range::new(ip(10, 0, 0, 1), ip(10, 0, 0, 5)).unwrap()];
        assert!(matches!(
            s.validate().as_slice(),
            [ScopeError::PoolOutsideSubnet { .. }, ..]
        ));
    }

    #[test]
    fn catches_overlapping_pools() {
        let mut s = scope24();
        s.pools = vec![
            Range::new(ip(192, 168, 88, 10), ip(192, 168, 88, 20)).unwrap(),
            Range::new(ip(192, 168, 88, 18), ip(192, 168, 88, 30)).unwrap(),
        ];
        assert!(
            s.validate()
                .iter()
                .any(|e| matches!(e, ScopeError::PoolsOverlap { .. }))
        );
    }

    #[test]
    fn catches_duplicate_reservation() {
        let mut s = scope24();
        s.reservations = vec![
            Reservation {
                client: ClientId::Mac(MacAddr([1; 6])),
                ip: ip(192, 168, 88, 50),
                hostname: None,
            },
            Reservation {
                client: ClientId::Mac(MacAddr([2; 6])),
                ip: ip(192, 168, 88, 50),
                hostname: None,
            },
        ];
        assert!(
            s.validate()
                .iter()
                .any(|e| matches!(e, ScopeError::DuplicateReservation { count: 2, .. }))
        );
    }

    #[test]
    fn catches_gateway_outside_subnet() {
        let mut s = scope24();
        s.router = Some(ip(10, 0, 0, 1));
        assert!(
            s.validate()
                .iter()
                .any(|e| matches!(e, ScopeError::GatewayOutsideSubnet { .. }))
        );
    }

    #[test]
    fn catches_non_network_address() {
        let mut s = scope24();
        s.subnet = ip(192, 168, 88, 7); // 主机位不为零
        assert!(
            s.validate()
                .iter()
                .any(|e| matches!(e, ScopeError::NotANetworkAddress { .. }))
        );
    }

    #[test]
    fn catches_offer_longer_than_lease() {
        let mut s = scope24();
        s.lease_secs = 60;
        s.offer_secs = 120;
        assert!(
            s.validate()
                .iter()
                .any(|e| matches!(e, ScopeError::OfferLongerThanLease { .. }))
        );
    }

    #[test]
    fn catches_empty_scope() {
        let mut s = scope24();
        s.pools.clear();
        assert!(
            s.validate()
                .iter()
                .any(|e| matches!(e, ScopeError::NoUsableAddresses))
        );
    }

    #[test]
    fn reservation_only_scope_is_valid() {
        let mut s = scope24();
        s.pools.clear();
        s.reservations = vec![Reservation {
            client: ClientId::Mac(MacAddr([1; 6])),
            ip: ip(192, 168, 88, 50),
            hostname: None,
        }];
        assert_eq!(s.validate(), vec![], "只做静态保留、不设池是合法配置");
    }

    #[test]
    fn validate_reports_every_problem_at_once() {
        let mut s = scope24();
        s.pools = vec![Range::new(ip(10, 0, 0, 1), ip(10, 0, 0, 5)).unwrap()];
        s.router = Some(ip(172, 16, 0, 1));
        s.lease_secs = 0;
        assert!(s.validate().len() >= 3, "应一次列全，而不是报一个停一个");
    }
}
