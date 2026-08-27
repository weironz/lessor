//! 作用域 —— 一个被管理的子网及其下发的配置。

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addr::{ClientId, Range};

/// 静态保留：把某个客户端固定绑到某个地址。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reservation {
    pub client: ClientId,
    pub ip: Ipv4Addr,
    pub hostname: Option<String>,
}

/// 网络引导参数（PXE / iPXE / UEFI HTTP Boot）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BootConfig {
    /// option 66 —— TFTP 服务器名
    pub server_name: Option<String>,
    /// option 67 —— 引导文件名
    pub filename: Option<String>,
    /// siaddr 字段里的下一跳服务器
    pub next_server: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    /// 网络地址，如 192.168.88.0
    pub subnet: Ipv4Addr,
    /// 前缀长度，如 24
    pub prefix: u8,
    /// 可分配区间，按顺序使用
    pub pools: Vec<Range>,
    /// 从池中剔除的区间（比如中间几个地址留给交换机）
    pub exclusions: Vec<Range>,
    pub reservations: Vec<Reservation>,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    /// 正式租期
    pub lease_secs: u32,
    /// OFFER 的占位时长 —— 客户端没跟上 REQUEST 时地址要能尽快回收
    pub offer_secs: u32,
    /// 收到 DECLINE 后隔离该地址的时长
    pub decline_quarantine_secs: u32,
    pub boot: Option<BootConfig>,
    /// 额外的原始选项 `(code, value)`，用于本结构没有专门字段的场景
    pub extra_options: Vec<(u8, Vec<u8>)>,
}

impl Scope {
    /// 一个够用的默认作用域，调用方按需覆盖字段。
    pub fn new(name: impl Into<String>, subnet: Ipv4Addr, prefix: u8) -> Self {
        Self {
            name: name.into(),
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

    pub fn netmask(&self) -> Ipv4Addr {
        let bits = if self.prefix >= 32 {
            u32::MAX
        } else {
            u32::MAX.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr::new(a, b, c, d)
    }

    fn scope24() -> Scope {
        let mut s = Scope::new("lab", ip(192, 168, 88, 0), 24);
        s.pools = vec![Range::new(ip(192, 168, 88, 10), ip(192, 168, 88, 20)).unwrap()];
        s
    }

    #[test]
    fn netmask_and_broadcast() {
        let s = scope24();
        assert_eq!(s.netmask(), ip(255, 255, 255, 0));
        assert_eq!(s.broadcast(), ip(192, 168, 88, 255));

        let mut s30 = Scope::new("p2p", ip(10, 0, 0, 0), 30);
        s30.prefix = 30;
        assert_eq!(s30.netmask(), ip(255, 255, 255, 252));
        assert_eq!(s30.broadcast(), ip(10, 0, 0, 3));

        let s32 = Scope::new("host", ip(10, 0, 0, 1), 32);
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
        assert!(!addrs.contains(&ip(192, 168, 88, 15)));
        assert!(addrs.contains(&ip(192, 168, 88, 14)));
    }

    #[test]
    fn network_and_broadcast_never_offered() {
        let mut s = Scope::new("full", ip(10, 0, 0, 0), 24);
        s.pools = vec![Range::new(ip(10, 0, 0, 0), ip(10, 0, 0, 255)).unwrap()];
        let addrs: Vec<_> = s.poolable_addrs().collect();
        assert_eq!(addrs.len(), 254);
        assert!(!addrs.contains(&ip(10, 0, 0, 0)));
        assert!(!addrs.contains(&ip(10, 0, 0, 255)));
    }
}
