//! 守护进程配置。
//!
//! 作用域描述"这个网段该怎么发地址"，监听器描述"本机在这个网段上是谁"。
//! 两者分开是因为同一份作用域配置可能被不同机器复用，而本机地址是机器相关的。

use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use lessor_core::{Range, Reservation, Scope};
use serde::{Deserialize, Serialize};

/// 一个监听点：本机在某个网段上的地址，以及（可选的）绑定到哪块网卡。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listener {
    /// 本机在该网段上的地址。用作 option 54（server identifier），
    /// 也是选作用域和发包时的源地址。
    pub server_ip: Ipv4Addr,
    /// 网卡名。仅 Linux 生效（`SO_BINDTODEVICE`）—— 有了它，
    /// 每个监听器只会收到自己那块网卡上的广播，多网卡场景才真正干净。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub iface: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub listeners: Vec<Listener>,
    pub scopes: Vec<Scope>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读不到配置文件 {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&text)
            .with_context(|| format!("{} 不是合法的配置", path.display()))?;
        cfg.check()?;
        Ok(cfg)
    }

    /// 启动前的自检。作用域自身的问题交给 `Scope::validate`，
    /// 这里只管跨对象的一致性。
    pub fn check(&self) -> Result<()> {
        if self.listeners.is_empty() {
            bail!("至少要配置一个监听器");
        }
        if self.scopes.is_empty() {
            bail!("至少要配置一个作用域");
        }

        let mut problems = Vec::new();

        for s in &self.scopes {
            for e in s.validate() {
                problems.push(format!("作用域 {}（{}）: {e}", s.name, s.id));
            }
        }

        // 每个作用域都该有一个本机地址落在它的网段里，否则收到请求也发不出应答
        for s in &self.scopes {
            if !self.listeners.iter().any(|l| s.contains(l.server_ip)) {
                problems.push(format!(
                    "作用域 {}（{}/{}）没有对应的监听器 —— \
                     需要一个 server_ip 落在这个网段内",
                    s.name, s.subnet, s.prefix
                ));
            }
        }

        // 两个作用域覆盖同一个监听器地址会导致选谁不确定
        for l in &self.listeners {
            let hits: Vec<&str> = self
                .scopes
                .iter()
                .filter(|s| s.contains(l.server_ip))
                .map(|s| s.name.as_str())
                .collect();
            if hits.len() > 1 {
                problems.push(format!(
                    "监听器 {} 同时落在多个作用域里（{}），无法确定用哪个",
                    l.server_ip,
                    hits.join("、")
                ));
            }
        }

        if !problems.is_empty() {
            bail!("配置有问题：\n  - {}", problems.join("\n  - "));
        }
        Ok(())
    }

    /// 从命令行参数拼一个单网段配置 —— 临时起一个 DHCP 时不必写配置文件。
    pub fn from_quick(o: Quick) -> Result<Self> {
        let mask = if o.prefix >= 32 {
            u32::MAX
        } else {
            u32::MAX.checked_shl(32 - u32::from(o.prefix)).unwrap_or(0)
        };
        let subnet = Ipv4Addr::from(u32::from(o.server_ip) & mask);

        let mut scope = Scope::new(1, "quick", subnet, o.prefix);
        scope.pools = vec![o.pool];
        scope.router = o.router;
        scope.dns = o.dns;
        scope.lease_secs = o.lease_secs;
        scope.reservations = o.reservations;
        // 临时场景通常希望地址尽快回收
        scope.offer_secs = 30.min(o.lease_secs);

        let cfg = Self {
            listeners: vec![Listener {
                server_ip: o.server_ip,
                iface: o.iface,
            }],
            scopes: vec![scope],
        };
        cfg.check()?;
        Ok(cfg)
    }
}

/// 不写配置文件时，用命令行参数直接描述一个单网段。
#[derive(Debug)]
pub struct Quick {
    /// 本机在该网段上的地址，子网由它和前缀推出
    pub server_ip: Ipv4Addr,
    pub prefix: u8,
    pub pool: Range,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub lease_secs: u32,
    pub iface: Option<String>,
    pub reservations: Vec<Reservation>,
}

/// 命令行里 `192.168.88.10-192.168.88.20` 形式的区间。
pub fn parse_range(s: &str) -> Result<Range> {
    let (a, b) = s
        .split_once('-')
        .with_context(|| format!("地址区间要写成 起始-结束，收到的是 {s}"))?;
    let start: Ipv4Addr = a.trim().parse().with_context(|| format!("起始地址 {a} 不合法"))?;
    let end: Ipv4Addr = b.trim().parse().with_context(|| format!("结束地址 {b} 不合法"))?;
    Range::new(start, end).with_context(|| format!("起始地址不能大于结束地址：{s}"))
}

/// 让 `Reservation` 能从 `MAC=IP` 或 `MAC=IP=主机名` 解析。
pub fn parse_reservation(s: &str) -> Result<Reservation> {
    let mut parts = s.split('=');
    let mac = parts
        .next()
        .with_context(|| format!("保留项格式应为 MAC=IP[=主机名]，收到 {s}"))?;
    let ip = parts
        .next()
        .with_context(|| format!("保留项缺少 IP：{s}"))?;
    Ok(Reservation {
        client: lessor_core::ClientId::Mac(mac.trim().parse()?),
        ip: ip.trim().parse().with_context(|| format!("IP {ip} 不合法"))?,
        hostname: parts.next().map(|h| h.trim().to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr::new(a, b, c, d)
    }

    fn quick() -> Config {
        Config::from_quick(Quick {
            server_ip: ip(192, 168, 88, 1),
            prefix: 24,
            pool: Range::new(ip(192, 168, 88, 10), ip(192, 168, 88, 20)).unwrap(),
            router: None,
            dns: vec![],
            lease_secs: 3600,
            iface: None,
            reservations: vec![],
        })
        .unwrap()
    }

    #[test]
    fn quick_config_derives_the_subnet_from_the_server_address() {
        let c = quick();
        assert_eq!(c.scopes[0].subnet, ip(192, 168, 88, 0));
        assert_eq!(c.scopes[0].prefix, 24);
        assert_eq!(c.listeners[0].server_ip, ip(192, 168, 88, 1));
    }

    #[test]
    fn a_scope_without_a_listener_is_rejected() {
        let mut c = quick();
        c.listeners[0].server_ip = ip(10, 0, 0, 1);
        let err = c.check().unwrap_err().to_string();
        assert!(err.contains("没有对应的监听器"), "实际: {err}");
    }

    #[test]
    fn overlapping_scopes_on_one_listener_are_rejected() {
        let mut c = quick();
        let mut dup = c.scopes[0].clone();
        dup.id = lessor_core::ScopeId(2);
        dup.name = "另一个".into();
        c.scopes.push(dup);
        let err = c.check().unwrap_err().to_string();
        assert!(err.contains("同时落在多个作用域"), "实际: {err}");
    }

    #[test]
    fn scope_validation_errors_surface_in_check() {
        let mut c = quick();
        c.scopes[0].router = Some(ip(10, 9, 9, 1)); // 网关不在子网内
        let err = c.check().unwrap_err().to_string();
        assert!(err.contains("网关"), "实际: {err}");
    }

    #[test]
    fn range_parses_and_rejects_inverted() {
        let r = parse_range("192.168.88.10-192.168.88.20").unwrap();
        assert_eq!(r.start, ip(192, 168, 88, 10));
        assert_eq!(r.end, ip(192, 168, 88, 20));
        assert!(parse_range("192.168.88.20-192.168.88.10").is_err());
        assert!(parse_range("192.168.88.10").is_err());
    }

    #[test]
    fn reservation_parses_with_and_without_hostname() {
        let r = parse_reservation("ac:1f:6b:8e:00:01=192.168.88.50").unwrap();
        assert_eq!(r.ip, ip(192, 168, 88, 50));
        assert!(r.hostname.is_none());

        let r = parse_reservation("ac1f6b8e0001=192.168.88.51=bmc-01").unwrap();
        assert_eq!(r.hostname.as_deref(), Some("bmc-01"));
    }

    #[test]
    fn config_roundtrips_through_json() {
        let c = quick();
        let j = serde_json::to_string_pretty(&c).unwrap();
        let back: Config = serde_json::from_str(&j).unwrap();
        assert!(back.check().is_ok());
        assert_eq!(back.scopes[0].subnet, c.scopes[0].subnet);
    }
}
