//! 各平台的地址配置。
//!
//! 这是本项目里唯一必须区分平台的地方。三家的差异不只是命令不同 ——
//! "还原成 DHCP" 在 Linux 上甚至没有统一答案，取决于谁在管这块网卡。
//!
//! 都需要特权（Windows 管理员 / Unix root）。

use std::net::Ipv4Addr;
use std::process::Command;

use tracing::debug;

use crate::{NetError, Result};

/// 从命令输出里认出"权限不足"。
///
/// 这些工具都不用退出码区分错误类型，只能看文本。各家措辞不同，
/// 而且 Windows 上还是本地化的 —— 中文系统上是"需要提升"，
/// 所以两种语言的关键词都要认。
fn looks_like_permission_error(text: &str) -> bool {
    const NEEDLES: &[&str] = &[
        // Windows（英文 / 中文）
        "requires elevation",
        "Access is denied",
        "需要提升",
        "拒绝访问",
        "以管理员",
        // Unix
        "Operation not permitted",
        "Permission denied",
        "must be root",
        "not permitted",
    ];
    NEEDLES.iter().any(|n| text.contains(n))
}

/// 跑一条命令，失败时把 stderr 带进错误里 —— 排查时没有这个几乎没法定位。
fn run(iface: &str, program: &str, args: &[&str]) -> Result<String> {
    debug!(program, ?args, "执行");
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| NetError::Configure {
            iface: iface.to_owned(),
            detail: format!("无法执行 {program}: {e}"),
        })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }

    // netsh 之类的工具会把错误写到 stdout 而不是 stderr，两边都要看
    let err = String::from_utf8_lossy(&out.stderr);
    let msg = if err.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        err.into_owned()
    };
    let msg = msg.trim();

    if looks_like_permission_error(msg) {
        return Err(NetError::NeedsPrivilege {
            iface: iface.to_owned(),
        });
    }
    Err(NetError::Configure {
        iface: iface.to_owned(),
        detail: msg.to_owned(),
    })
}

/// 把网卡设成静态地址。
pub fn set_static(iface: &str, addr: Ipv4Addr, prefix: u8) -> Result<()> {
    imp::set_static(iface, addr, prefix)
}

/// 把网卡还原成自动获取。
pub fn restore_dhcp(iface: &str) -> Result<()> {
    imp::restore_dhcp(iface)
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;

    fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
        let bits = if prefix >= 32 {
            u32::MAX
        } else {
            u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
        };
        Ipv4Addr::from(bits)
    }

    pub fn set_static(iface: &str, addr: Ipv4Addr, prefix: u8) -> Result<()> {
        // netsh 比 PowerShell 快得多，而且不受执行策略影响。
        // 它是幂等的：同一地址重复设置不会报错。
        run(
            iface,
            "netsh",
            &[
                "interface",
                "ipv4",
                "set",
                "address",
                &format!("name={iface}"),
                "source=static",
                &format!("address={addr}"),
                &format!("mask={}", prefix_to_mask(prefix)),
            ],
        )?;
        Ok(())
    }

    pub fn restore_dhcp(iface: &str) -> Result<()> {
        run(
            iface,
            "netsh",
            &[
                "interface",
                "ipv4",
                "set",
                "address",
                &format!("name={iface}"),
                "source=dhcp",
            ],
        )?;
        // DNS 也一并还原，否则会留着上一次的静态设置
        let _ = run(
            iface,
            "netsh",
            &[
                "interface",
                "ipv4",
                "set",
                "dnsservers",
                &format!("name={iface}"),
                "source=dhcp",
            ],
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub fn set_static(iface: &str, addr: Ipv4Addr, prefix: u8) -> Result<()> {
        // 先清掉已有地址，否则会叠加上去，之后 DHCP 应答的源地址就不确定了
        let _ = run(iface, "ip", &["addr", "flush", "dev", iface]);
        run(
            iface,
            "ip",
            &["addr", "add", &format!("{addr}/{prefix}"), "dev", iface],
        )?;
        run(iface, "ip", &["link", "set", iface, "up"])?;
        Ok(())
    }

    pub fn restore_dhcp(iface: &str) -> Result<()> {
        let _ = run(iface, "ip", &["addr", "flush", "dev", iface]);

        // "还原成 DHCP" 取决于谁在管这块网卡，按可能性依次尝试。
        // 救援盘之类的环境可能什么都没有，那清掉地址就算完成。
        for (prog, args) in [
            ("nmcli", vec!["device", "reapply", iface]),
            ("networkctl", vec!["reconfigure", iface]),
            ("dhclient", vec!["-1", iface]),
        ] {
            if run(iface, prog, &args).is_ok() {
                debug!(prog, "已交回给网络管理器");
                return Ok(());
            }
        }
        debug!("没找到网络管理器，仅清除了地址");
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
        let bits = if prefix >= 32 {
            u32::MAX
        } else {
            u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
        };
        Ipv4Addr::from(bits)
    }

    pub fn set_static(iface: &str, addr: Ipv4Addr, prefix: u8) -> Result<()> {
        run(
            iface,
            "ifconfig",
            &[
                iface,
                "inet",
                &addr.to_string(),
                "netmask",
                &prefix_to_mask(prefix).to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn restore_dhcp(iface: &str) -> Result<()> {
        // ifconfig 设的地址不会被 networksetup 记住，直接让 DHCP 客户端重来
        run(iface, "ipconfig", &["set", iface, "DHCP"])?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    pub fn set_static(_iface: &str, _addr: Ipv4Addr, _prefix: u8) -> Result<()> {
        Err(NetError::Unsupported)
    }

    pub fn restore_dhcp(_iface: &str) -> Result<()> {
        Err(NetError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_errors_are_recognised_in_both_languages() {
        // Windows 的 netsh 输出是本地化的，中英文都得认 ——
        // 只认英文的话，中文系统上权限问题会被当成参数错误
        for text in [
            "The requested operation requires elevation.",
            "请求的操作需要提升。",
            "拒绝访问。",
            "Access is denied.",
            "ip: Operation not permitted",
            "RTNETLINK answers: Permission denied",
        ] {
            assert!(
                looks_like_permission_error(text),
                "应识别为权限问题: {text}"
            );
        }
    }

    #[test]
    fn ordinary_failures_are_not_mistaken_for_permission_errors() {
        for text in [
            "Element not found.",
            "找不到元素。",
            "Cannot find device \"eth9\"",
            "",
        ] {
            assert!(
                !looks_like_permission_error(text),
                "不该识别为权限问题: {text}"
            );
        }
    }

    #[test]
    fn errors_are_actionable() {
        // 用一个必然不存在的网卡名。非管理员身份下，netsh 会先撞上权限；
        // 管理员身份下才会走到"网卡不存在"。两种都要给得出下一步。
        let err = set_static("不存在的网卡xyz", Ipv4Addr::new(10, 254, 254, 1), 24)
            .expect_err("不存在的网卡不该配置成功");

        match &err {
            NetError::NeedsPrivilege { iface } => {
                assert_eq!(iface, "不存在的网卡xyz");
                assert!(err.is_privilege());
                assert!(err.hint().is_some(), "权限错误必须给出下一步");
            }
            NetError::Configure { iface, .. } => {
                assert_eq!(iface, "不存在的网卡xyz");
                assert!(!err.is_privilege());
            }
            NetError::Unsupported => {}
            other => panic!("意料之外的错误: {other:?}"),
        }
    }
}
