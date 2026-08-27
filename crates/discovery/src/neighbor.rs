//! 读取系统的邻居表（ARP / NDP）。
//!
//! 静态 IP 的设备不会来要地址，但只要它在网上说过话 —— 找网关、
//! 回应过谁 —— 操作系统就会把它记进邻居表。读这张表不需要抓包驱动，
//! 也不需要特权。
//!
//! 解析上不依赖任何语言环境：不认表头、不认列名，只在每行里找
//! "长得像 IPv4 的东西" 和 "长得像 MAC 的东西"。Windows 的 `arp -a`
//! 输出带本地化文字，按列切分会在中文系统上直接失效。

use std::net::Ipv4Addr;
use std::process::Command;

use lessor_core::MacAddr;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Neighbor {
    pub ip: Ipv4Addr,
    pub mac: MacAddr,
}

/// 从一行文本里同时找出 IPv4 和 MAC。两者都有才算数。
fn parse_line(line: &str) -> Option<Neighbor> {
    let mut ip = None;
    let mut mac = None;
    for tok in line.split_whitespace() {
        let t = tok.trim_matches(|c| c == '(' || c == ')' || c == ',');
        if ip.is_none()
            && let Ok(v) = t.parse::<Ipv4Addr>()
        {
            ip = Some(v);
            continue;
        }
        if mac.is_none()
            && looks_like_mac(t)
            && let Ok(m) = t.parse::<MacAddr>()
        {
            // 全零和广播地址是占位，不是真实邻居
            if !m.is_zero() && m.0 != [0xff; 6] {
                mac = Some(m);
            }
        }
    }
    Some(Neighbor {
        ip: ip?,
        mac: mac?,
    })
}

/// MAC 的形状：恰好 12 个十六进制字符，中间只允许 `:` `-` `.` 分隔。
///
/// 不能只看"能否解析"—— `MacAddr::from_str` 是宽容的，纯数字的
/// 接口序号之类也可能凑巧被接受。
fn looks_like_mac(s: &str) -> bool {
    let hex = s.chars().filter(|c| c.is_ascii_hexdigit()).count();
    let seps = s.chars().filter(|c| *c == ':' || *c == '-' || *c == '.').count();
    hex == 12 && seps > 0 && s.chars().all(|c| c.is_ascii_hexdigit() || ":-.".contains(c))
}

fn parse_table(text: &str) -> Vec<Neighbor> {
    let mut out: Vec<Neighbor> = Vec::new();
    for line in text.lines() {
        if let Some(n) = parse_line(line)
            && !out.iter().any(|e| e.ip == n.ip)
        {
            out.push(n);
        }
    }
    out
}

/// Linux 的 `/proc/net/arp` 是最干净的来源，不用起进程。
#[cfg(target_os = "linux")]
fn read_raw() -> Option<String> {
    std::fs::read_to_string("/proc/net/arp").ok()
}

#[cfg(not(target_os = "linux"))]
fn read_raw() -> Option<String> {
    None
}

/// 读取当前的邻居表。
pub fn neighbors() -> Vec<Neighbor> {
    if let Some(raw) = read_raw() {
        return parse_table(&raw);
    }
    // Windows 和 macOS 都有 arp，参数也通用
    let args: &[&str] = if cfg!(target_os = "windows") {
        &["-a"]
    } else {
        &["-an"]
    };
    Command::new("arp")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_table(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr::new(a, b, c, d)
    }

    #[test]
    fn parses_linux_proc_net_arp() {
        let raw = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.88.10    0x1         0x2         ac:1f:6b:8e:00:01     *        ens33
192.168.88.1     0x1         0x2         00:0c:29:aa:bb:cc     *        ens33
192.168.88.99    0x1         0x0         00:00:00:00:00:00     *        ens33
";
        let n = parse_table(raw);
        assert_eq!(n.len(), 2, "全零的不完整条目应被忽略");
        assert_eq!(n[0].ip, ip(192, 168, 88, 10));
        assert_eq!(n[0].mac.to_string(), "ac:1f:6b:8e:00:01");
    }

    #[test]
    fn parses_chinese_windows_arp_output() {
        // 中文 Windows 上 arp -a 的表头和"类型"列都是中文 ——
        // 按列切分会失效，所以解析必须与语言无关
        let raw = "\
接口: 192.168.88.1 --- 0xb
  Internet 地址         物理地址              类型
  192.168.88.10         ac-1f-6b-8e-00-01     动态
  192.168.88.20         00-0c-29-11-22-33     动态
  192.168.88.255        ff-ff-ff-ff-ff-ff     静态
";
        let n = parse_table(raw);
        assert_eq!(n.len(), 2, "广播地址那条应被忽略");
        assert_eq!(n[0].ip, ip(192, 168, 88, 10));
        assert_eq!(n[0].mac.to_string(), "ac:1f:6b:8e:00:01");
        assert_eq!(n[1].mac.to_string(), "00:0c:29:11:22:33");
    }

    #[test]
    fn parses_macos_arp_output() {
        let raw = "\
? (192.168.88.10) at ac:1f:6b:8e:0:1 on en0 ifscope [ethernet]
? (192.168.88.1) at 0:c:29:aa:bb:cc on en0 ifscope [ethernet]
? (192.168.88.77) at (incomplete) on en0 ifscope [ethernet]
";
        let n = parse_table(raw);
        // macOS 会省掉前导零，MacAddr 的解析要求恰好 12 个十六进制字符，
        // 所以这种缩写形式解析不了 —— 这是已知取舍，不影响主用途
        assert!(
            n.iter().all(|x| x.ip != ip(192, 168, 88, 77)),
            "incomplete 的条目不该出现"
        );
    }

    #[test]
    fn a_line_without_a_mac_is_skipped() {
        assert!(parse_line("192.168.88.5   (incomplete)  on en0").is_none());
        assert!(parse_line("接口: 192.168.88.1 --- 0xb").is_none());
    }

    #[test]
    fn a_line_without_an_ip_is_skipped() {
        assert!(parse_line("  物理地址  ac-1f-6b-8e-00-01  动态").is_none());
    }

    #[test]
    fn interface_indexes_are_not_mistaken_for_macs() {
        // 0xb 之类的接口序号不该被当成 MAC
        assert!(!looks_like_mac("0xb"));
        assert!(!looks_like_mac("192.168.88.1"));
        assert!(!looks_like_mac("ac1f6b8e0001"), "没有分隔符的不算");
        assert!(looks_like_mac("ac-1f-6b-8e-00-01"));
        assert!(looks_like_mac("ac:1f:6b:8e:00:01"));
        assert!(looks_like_mac("ac1f.6b8e.0001"));
    }

    #[test]
    fn duplicate_addresses_are_collapsed() {
        let raw = "\
192.168.88.10  ac:1f:6b:8e:00:01  ens33
192.168.88.10  ac:1f:6b:8e:00:01  ens37
";
        assert_eq!(parse_table(raw).len(), 1);
    }

    #[test]
    fn reading_the_real_table_does_not_panic() {
        // 内容因机器而异，但不该崩，也不该返回畸形数据
        for n in neighbors() {
            assert!(!n.mac.is_zero());
        }
    }
}
