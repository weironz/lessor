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
        // 监听器可以为空，但仅当也没有作用域时 —— 那是"服务先起来、
        // 网卡和地址池都等界面上选"的形态（--serve-empty）。
        // 有作用域却没有监听器则是真的配错了：收得到请求也发不出应答。
        if self.listeners.is_empty() && !self.scopes.is_empty() {
            bail!("配了作用域却没有监听器 —— 收得到请求也发不出应答");
        }
        // 作用域可以为空 —— 桌面端/界面优先的用法是先把服务起起来，
        // 再在界面上建作用域。零作用域时不应答任何请求（NoMatchingScope），
        // 这是安全的：宁可不发地址，也不能乱发。

        let mut problems = Vec::new();

        for s in &self.scopes {
            for e in s.validate() {
                problems.push(format!("作用域 {}（{}）: {e}", s.name, s.id));
            }
        }

        // 每个作用域都该有一个本机地址落在它的网段里，否则收到请求也发不出应答。
        //
        // 例外是经中继服务的网段：本机在它上面本来就没有地址，应答单播回
        // giaddr 由中继放回线上。所以这类作用域必须显式标 viaRelay ——
        // 不能一刀切放开，否则"配漏了监听器"这个最常见的配置错误就没人拦了。
        for s in &self.scopes {
            if s.via_relay {
                continue;
            }
            if !self.listeners.iter().any(|l| s.contains(l.server_ip)) {
                problems.push(format!(
                    "作用域 {}（{}/{}）没有对应的监听器 —— \
                     需要一个 server_ip 落在这个网段内；\
                     如果这个网段是经 DHCP 中继服务的，请标上 viaRelay",
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
        // 没给本机地址就整个空着起 —— 网卡和地址池都在界面上选
        let Some(server_ip) = o.server_ip else {
            let cfg = Self {
                listeners: Vec::new(),
                scopes: Vec::new(),
            };
            cfg.check()?;
            return Ok(cfg);
        };
        let subnet = Ipv4Addr::from(u32::from(server_ip) & mask);

        let mut scope = Scope::new(1, "quick", subnet, o.prefix);
        scope.pools = o.pool.into_iter().collect();
        scope.router = o.router;
        scope.dns = o.dns;
        scope.lease_secs = o.lease_secs;
        scope.reservations = o.reservations;
        scope.boot = o.boot;
        scope.extra_options = o.extra_options;
        // 临时场景通常希望地址尽快回收
        scope.offer_secs = 30.min(o.lease_secs);

        let cfg = Self {
            listeners: vec![Listener {
                server_ip,
                iface: o.iface,
            }],
            // 没给地址池就不建作用域 —— 监听器照常起，等界面上建
            scopes: if scope.pools.is_empty() {
                Vec::new()
            } else {
                vec![scope]
            },
        };
        cfg.check()?;
        Ok(cfg)
    }
}

/// 不写配置文件时，用命令行参数直接描述一个单网段。
#[derive(Debug)]
pub struct Quick {
    /// 本机在该网段上的地址，子网由它和前缀推出。
    /// None 表示先不建监听器（`--serve-empty`），等界面上选网卡。
    pub server_ip: Option<Ipv4Addr>,
    pub prefix: u8,
    pub pool: Option<Range>,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub lease_secs: u32,
    pub iface: Option<String>,
    pub reservations: Vec<Reservation>,
    /// PXE / UEFI HTTP Boot 参数。不给的话不下发引导选项。
    pub boot: Option<lessor_core::BootConfig>,
    /// 额外的原始 DHCP 选项，本结构没有专门字段的都走这里。
    pub extra_options: Vec<(u8, Vec<u8>)>,
}

/// 命令行里 `192.168.88.10-192.168.88.20` 形式的区间。
pub fn parse_range(s: &str) -> Result<Range> {
    let (a, b) = s
        .split_once('-')
        .with_context(|| format!("地址区间要写成 起始-结束，收到的是 {s}"))?;
    let start: Ipv4Addr = a
        .trim()
        .parse()
        .with_context(|| format!("起始地址 {a} 不合法"))?;
    let end: Ipv4Addr = b
        .trim()
        .parse()
        .with_context(|| format!("结束地址 {b} 不合法"))?;
    Range::new(start, end).with_context(|| format!("起始地址不能大于结束地址：{s}"))
}

/// 命令行里的原始选项，形如 `43=060108ff`（编号=十六进制值）。
///
/// 有它就不用为每个冷门选项加一个参数 —— DHCP 选项有一百多个，
/// 专门字段只覆盖常用的那些。
pub fn parse_option(s: &str) -> Result<(u8, Vec<u8>)> {
    let (code, hex) = s
        .split_once('=')
        .with_context(|| format!("选项格式应为 编号=十六进制，收到 {s}"))?;
    let code: u8 = code
        .trim()
        .parse()
        .with_context(|| format!("选项编号 {code} 不合法（0-255）"))?;
    let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if hex.len() % 2 != 0 {
        bail!("选项 {code} 的十六进制值长度必须是偶数：{hex}");
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<u8>, _>>()
        .with_context(|| format!("选项 {code} 的值不是合法十六进制：{hex}"))?;
    Ok((code, bytes))
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
        ip: ip
            .trim()
            .parse()
            .with_context(|| format!("IP {ip} 不合法"))?,
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
            server_ip: Some(ip(192, 168, 88, 1)),
            prefix: 24,
            pool: Range::new(ip(192, 168, 88, 10), ip(192, 168, 88, 20)),
            router: None,
            dns: vec![],
            lease_secs: 3600,
            iface: None,
            reservations: vec![],
            boot: None,
            extra_options: vec![],
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
        // 报错要顺带指出中继这条路，否则跨网段部署的人会卡在这里
        assert!(err.contains("viaRelay"), "实际: {err}");
    }

    #[test]
    fn a_relayed_scope_may_have_no_listener_of_its_own() {
        // 经中继服务的网段，本机在它上面本来就没有地址 —— 那不是配漏了。
        // 标了 viaRelay 就该放行，否则跨网段根本配不出来。
        let mut c = quick();
        let mut relayed = lessor_core::Scope::new(2, "branch", ip(10, 20, 30, 0), 24);
        relayed.pools = vec![Range::new(ip(10, 20, 30, 100), ip(10, 20, 30, 200)).unwrap()];
        relayed.via_relay = true;
        c.scopes.push(relayed);
        assert!(c.check().is_ok(), "{:?}", c.check().unwrap_err());
    }

    #[test]
    fn a_relayed_scope_still_needs_someone_to_receive_packets() {
        // viaRelay 免掉的是"本网段要有监听器"，不是"一个监听器都不用有" ——
        // 中继会把报文单播到我们某个监听器上，没有监听器就谁也收不到。
        // 这条由既有的"配了作用域却没有监听器"兜住，这里钉住 viaRelay
        // 没有把它一并绕过去。
        let mut c = quick();
        c.scopes[0].via_relay = true;
        c.listeners.clear();
        let err = c.check().unwrap_err().to_string();
        assert!(err.contains("没有监听器"), "实际: {err}");
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
    fn option_parses_from_hex() {
        assert_eq!(
            parse_option("43=060108ff").unwrap(),
            (43, vec![0x06, 0x01, 0x08, 0xff])
        );
        assert_eq!(parse_option("252=").unwrap(), (252, vec![]));
        assert!(parse_option("43=abc").is_err(), "奇数长度应拒绝");
        assert!(parse_option("999=ff").is_err(), "编号超出 u8 应拒绝");
        assert!(parse_option("43=zz").is_err(), "非十六进制应拒绝");
        assert!(parse_option("43").is_err(), "缺少等号应拒绝");
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

    /// `--serve-empty` 的形态：没有地址池时不建作用域，但监听器照常起。
    ///
    /// 这是"双击直接进界面"的前提 —— 服务先跑起来，作用域在界面上建。
    /// 零作用域时不应答任何请求（NoMatchingScope），所以是安全的。
    #[test]
    fn quick_without_pool_yields_a_listener_and_no_scope() {
        let c = Config::from_quick(Quick {
            server_ip: Some(ip(192, 168, 88, 1)),
            prefix: 24,
            pool: None,
            router: None,
            dns: Vec::new(),
            lease_secs: 3600,
            iface: None,
            reservations: Vec::new(),
            boot: None,
            extra_options: Vec::new(),
        })
        .expect("没有地址池也应当能起服务");

        assert!(c.scopes.is_empty(), "没给池就不该凭空造一个作用域");
        assert_eq!(
            c.listeners.len(),
            1,
            "监听器照常起，否则界面建完作用域也没人收包"
        );
    }

    /// 空作用域的配置必须能过自检 —— 否则 `--serve-empty` 起不来。
    #[test]
    fn empty_scopes_pass_the_check() {
        let c = Config {
            listeners: vec![Listener {
                server_ip: ip(192, 168, 88, 1),
                iface: None,
            }],
            scopes: Vec::new(),
        };
        assert!(c.check().is_ok(), "零作用域是合法状态，不是配置错误");
    }
}
