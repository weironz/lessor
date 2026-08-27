//! 网卡地址配置 —— 整个项目里唯一需要特权的操作，单独一个程序。
//!
//! 分出来是有意的：`lessord` 不需要任何特权，把改网卡的能力放进去会让
//! 整个服务背上"要提权跑"的包袱。自动化流程里，改地址是一次性的前置步骤，
//! 而 DHCP 服务是长期运行的 —— 两者的权限需求本就不同，不该绑在一起。
//!
//! 需要特权的只有 `set` 和 `restore`；`list` 普通用户就能跑。

use std::net::Ipv4Addr;
use std::process::ExitCode;

use lessor_net::{NetError, interfaces, restore_dhcp, set_static};

const USAGE: &str = "\
lessor-netcfg —— 配置网卡地址

    lessor-netcfg list
        列出本机网卡（不需要特权）

    lessor-netcfg set <网卡名> <地址>/<前缀>
        把网卡设成静态地址，例如：
        lessor-netcfg set \"以太网\" 192.168.88.1/24

    lessor-netcfg restore <网卡名>
        还原成自动获取

只有 set 和 restore 需要特权。lessord 本身不需要 —— 这是分开的原因。
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match refs.as_slice() {
        ["list"] => list(),
        ["set", iface, cidr] => set(iface, cidr),
        ["restore", iface] => restore(iface),
        [] | ["-h"] | ["--help"] | ["help"] => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误：{e}");
            if let Some(hint) = e.hint() {
                eprintln!();
                for line in hint.lines() {
                    eprintln!("  {line}");
                }
            }
            // 权限不足单独给一个退出码，脚本可以据此决定"换个身份重来"
            // 而不是把它当成参数写错。
            ExitCode::from(if e.is_privilege() { 77 } else { 1 })
        }
    }
}

/// 终端里的显示宽度。中日韩字符占两列，按字符数对齐会歪 ——
/// 本机网卡名常有中文（"以太网 2"），不处理的话这张表没法看。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let n = c as u32;
            let wide = (0x1100..=0x115F).contains(&n)      // 谚文字母
                || (0x2E80..=0xA4CF).contains(&n)          // 汉字、假名、部首
                || (0xAC00..=0xD7A3).contains(&n)          // 谚文音节
                || (0xF900..=0xFAFF).contains(&n)          // 兼容汉字
                || (0xFE30..=0xFE6F).contains(&n)          // 竖排标点
                || (0xFF00..=0xFF60).contains(&n)          // 全角
                || (0xFFE0..=0xFFE6).contains(&n);
            if wide { 2 } else { 1 }
        })
        .sum()
}

fn list() -> Result<(), NetError> {
    let ifs = interfaces()?;
    let width = ifs.iter().map(|i| display_width(&i.name)).max().unwrap_or(4);
    for i in &ifs {
        let pad = width.saturating_sub(display_width(&i.name));
        let addrs = if i.ipv4.is_empty() {
            "—".to_owned()
        } else {
            i.ipv4
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("  ")
        };
        let mark = if i.is_loopback {
            "环回"
        } else if i.is_servable() {
            "可用"
        } else {
            "无地址"
        };
        println!(
            "  {}{}  {:<6}  {}",
            i.name,
            " ".repeat(pad),
            mark,
            addrs
        );
    }
    Ok(())
}

fn parse_cidr(s: &str) -> Result<(Ipv4Addr, u8), NetError> {
    let bad = || NetError::Configure {
        iface: String::new(),
        detail: format!("地址应写成 192.168.88.1/24 的形式，收到 {s}"),
    };
    let (addr, prefix) = s.split_once('/').ok_or_else(bad)?;
    let addr: Ipv4Addr = addr.trim().parse().map_err(|_| bad())?;
    let prefix: u8 = prefix.trim().parse().map_err(|_| bad())?;
    if prefix > 32 {
        return Err(bad());
    }
    Ok((addr, prefix))
}

fn set(iface: &str, cidr: &str) -> Result<(), NetError> {
    let (addr, prefix) = parse_cidr(cidr)?;
    set_static(iface, addr, prefix)?;
    println!("已把 {iface} 设为 {addr}/{prefix}");
    println!();
    println!("接下来可以用普通权限起服务：");
    println!("  lessord --listen {addr} --prefix {prefix} --pool <起始>-<结束>");
    println!();
    println!("用完记得还原：lessor-netcfg restore \"{iface}\"");
    Ok(())
}

fn restore(iface: &str) -> Result<(), NetError> {
    restore_dhcp(iface)?;
    println!("已把 {iface} 还原为自动获取");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_names_align_by_display_width() {
        // 本机网卡名常有中文。按字符数对齐会让这张表歪掉，
        // 因为中日韩字符在终端里占两列。
        assert_eq!(display_width("Ethernet"), 8);
        assert_eq!(display_width("以太网"), 6);
        assert_eq!(display_width("以太网 2"), 8);
        assert_eq!(display_width("VMnet1"), 6);
    }

    #[test]
    fn cidr_parses_and_rejects_nonsense() {
        assert_eq!(
            parse_cidr("192.168.88.1/24").unwrap(),
            (Ipv4Addr::new(192, 168, 88, 1), 24)
        );
        assert!(parse_cidr("192.168.88.1").is_err(), "缺前缀应拒绝");
        assert!(parse_cidr("192.168.88.1/33").is_err(), "前缀越界应拒绝");
        assert!(parse_cidr("不是地址/24").is_err());
    }
}
