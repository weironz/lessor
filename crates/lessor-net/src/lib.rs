//! 平台层：网卡枚举与地址配置。
//!
//! 枚举用纯 Rust 实现，三平台共用。地址配置本质上是特权操作，各平台的
//! 做法差别很大，分别放在 [`platform`] 的子模块里 —— 调用方只看到统一的
//! [`set_static`] 和 [`restore_dhcp`]。

use std::net::Ipv4Addr;

use lessor_core::MacAddr;
use serde::Serialize;

pub mod platform;

pub use platform::{restore_dhcp, set_static};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("枚举网卡失败: {0}")]
    Enumerate(String),
    #[error("找不到网卡 {0}")]
    NoSuchInterface(String),
    #[error("配置网卡 {iface} 失败: {detail}")]
    Configure { iface: String, detail: String },
    #[error("当前平台不支持该操作")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, NetError>;

/// 网卡上的一个 IPv4 地址。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv4Cidr {
    pub addr: Ipv4Addr,
    pub prefix: u8,
}

impl Ipv4Cidr {
    pub fn network(&self) -> Ipv4Addr {
        let mask = if self.prefix >= 32 {
            u32::MAX
        } else {
            u32::MAX.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0)
        };
        Ipv4Addr::from(u32::from(self.addr) & mask)
    }
}

impl std::fmt::Display for Ipv4Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// 一块网卡。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Interface {
    /// 系统里的名字。Linux 上是 `ens33` 这种，Windows 上可能是中文的"以太网"。
    pub name: String,
    pub index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<MacAddr>,
    pub ipv4: Vec<Ipv4Cidr>,
    pub is_loopback: bool,
    /// 有 IPv4 地址就当作可用 —— 跨平台拿链路状态不可靠，
    /// 而对本工具来说"能不能在上面发地址"才是真正关心的事。
    pub has_address: bool,
}

impl Interface {
    /// 适合在这块网卡上跑 DHCP 服务吗。
    pub fn is_servable(&self) -> bool {
        !self.is_loopback && self.has_address
    }

    /// 挑一个本机地址作为该网卡上的 server-identifier。
    pub fn primary_ipv4(&self) -> Option<Ipv4Cidr> {
        self.ipv4.first().copied()
    }
}

fn netmask_to_prefix(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

/// 列出本机所有网卡。
pub fn interfaces() -> Result<Vec<Interface>> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};

    let raw = NetworkInterface::show().map_err(|e| NetError::Enumerate(e.to_string()))?;

    // 同一块网卡可能有多条记录（每个地址一条），按名字合并
    let mut out: Vec<Interface> = Vec::new();
    for ni in raw {
        let entry = match out.iter_mut().find(|i| i.name == ni.name) {
            Some(e) => e,
            None => {
                out.push(Interface {
                    name: ni.name.clone(),
                    index: Some(ni.index),
                    mac: ni.mac_addr.as_deref().and_then(|m| m.parse().ok()),
                    ipv4: Vec::new(),
                    is_loopback: false,
                    has_address: false,
                });
                out.last_mut().expect("刚推入")
            }
        };
        if entry.mac.is_none() {
            entry.mac = ni.mac_addr.as_deref().and_then(|m| m.parse().ok());
        }
        for addr in &ni.addr {
            if let network_interface::Addr::V4(v4) = addr {
                if v4.ip.is_loopback() {
                    entry.is_loopback = true;
                }
                let prefix = v4.netmask.map_or(32, netmask_to_prefix);
                let cidr = Ipv4Cidr {
                    addr: v4.ip,
                    prefix,
                };
                if !entry.ipv4.contains(&cidr) {
                    entry.ipv4.push(cidr);
                }
            }
        }
        entry.has_address = !entry.ipv4.is_empty();
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// 按名字取一块网卡。
pub fn interface(name: &str) -> Result<Interface> {
    interfaces()?
        .into_iter()
        .find(|i| i.name == name)
        .ok_or_else(|| NetError::NoSuchInterface(name.to_owned()))
}

/// 找出拥有指定地址的网卡 —— 配置里只给了 `server_ip` 时用它反查网卡名。
pub fn interface_with_address(addr: Ipv4Addr) -> Result<Option<Interface>> {
    Ok(interfaces()?
        .into_iter()
        .find(|i| i.ipv4.iter().any(|c| c.addr == addr)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_from_netmask() {
        assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 0)), 24);
        assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 252)), 30);
        assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 0, 0, 0)), 8);
        assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 255)), 32);
    }

    #[test]
    fn cidr_network_and_display() {
        let c = Ipv4Cidr {
            addr: Ipv4Addr::new(192, 168, 88, 37),
            prefix: 24,
        };
        assert_eq!(c.network(), Ipv4Addr::new(192, 168, 88, 0));
        assert_eq!(c.to_string(), "192.168.88.37/24");
    }

    #[test]
    fn enumeration_finds_a_loopback() {
        // 任何机器上都该有环回口 —— 这条断言能证明枚举确实在工作
        let ifs = interfaces().expect("枚举网卡不应失败");
        assert!(!ifs.is_empty());
        assert!(
            ifs.iter().any(|i| i.is_loopback),
            "应当能枚举到环回口，实际拿到: {:?}",
            ifs.iter().map(|i| &i.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn loopback_is_not_servable() {
        for i in interfaces().unwrap() {
            if i.is_loopback {
                assert!(!i.is_servable(), "环回口不该被当作可服务网卡");
            }
        }
    }

    #[test]
    fn addresses_are_not_duplicated() {
        for i in interfaces().unwrap() {
            let mut seen = i.ipv4.clone();
            seen.sort_by_key(|c| (u32::from(c.addr), c.prefix));
            seen.dedup();
            assert_eq!(seen.len(), i.ipv4.len(), "网卡 {} 的地址有重复", i.name);
        }
    }
}
