//! 注册成系统服务。
//!
//! 常驻形态需要开机自启、崩溃自拉起，这两件事交给系统的服务管理器做 ——
//! 自己写守护进程逻辑既不可靠也没必要。
//!
//! 这里只生成配置并调用系统命令，**不常驻、不接管服务生命周期**：
//! `lessord --install-service` 装完就退出，之后由 systemd / SCM 拉起。

use std::process::Command;

use anyhow::{Context, Result, bail};

/// 生成 systemd unit 的内容。
///
/// `Restart=on-failure` 而不是 `always`：配置错误导致的启动失败应当停下来
/// 让人看见，无限重启只会把日志刷满而问题还在。
#[cfg(target_os = "linux")]
fn systemd_unit(exe: &std::path::Path, args: &str) -> String {
    format!(
        "[Unit]\n\
         Description=lessor DHCP 服务\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} {args}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # 服务本身不需要 root：安装时给二进制设一次 capability 即可\n\
         # （见 README 的「不需要特权」）\n\
         AmbientCapabilities=CAP_NET_BIND_SERVICE\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe = exe.display(),
    )
}

/// 把自己注册成系统服务。`args` 是服务启动时要带的参数。
pub fn install(args: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("拿不到自己的可执行文件路径")?;
    let joined = args.join(" ");

    #[cfg(target_os = "linux")]
    {
        let unit_path = std::path::Path::new("/etc/systemd/system/lessord.service");
        std::fs::write(unit_path, systemd_unit(&exe, &joined))
            .with_context(|| format!("写不了 {} —— 注册服务需要 root", unit_path.display()))?;
        run("systemctl", &["daemon-reload"])?;
        run("systemctl", &["enable", "lessord"])?;
        run("systemctl", &["start", "lessord"])?;
        println!("已注册为 systemd 服务并启动。");
        println!("  状态：systemctl status lessord");
        println!("  日志：journalctl -u lessord -f");
        println!("  卸载：lessord --uninstall-service");
        return Ok(());
    }

    #[cfg(windows)]
    {
        // sc.exe 的 binPath= 后面必须有空格，且整条命令行要作为一个参数传。
        // 可执行文件路径可能含空格，所以内层再加一层引号。
        let bin = format!("\"{}\" {}", exe.display(), joined);
        run(
            "sc.exe",
            &[
                "create",
                "lessord",
                "binPath=",
                &bin,
                "start=",
                "auto",
                "DisplayName=",
                "lessor DHCP 服务",
            ],
        )?;
        // 崩溃后自动重启：5 秒、10 秒、之后每 30 秒
        let _ = run(
            "sc.exe",
            &[
                "failure",
                "lessord",
                "reset=",
                "86400",
                "actions=",
                "restart/5000/restart/10000/restart/30000",
            ],
        );
        run("sc.exe", &["start", "lessord"])?;
        println!("已注册为 Windows 服务并启动。");
        println!("  状态：sc query lessord");
        println!("  卸载：lessord --uninstall-service");
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (exe, joined);
        bail!("当前平台还不支持注册系统服务，请手工用进程管理器托管");
    }
}

/// 注销系统服务。
pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // 停和 disable 失败不致命 —— 服务可能本来就没在跑
        let _ = run("systemctl", &["stop", "lessord"]);
        let _ = run("systemctl", &["disable", "lessord"]);
        let unit_path = std::path::Path::new("/etc/systemd/system/lessord.service");
        if unit_path.exists() {
            std::fs::remove_file(unit_path)
                .with_context(|| format!("删不掉 {}", unit_path.display()))?;
        }
        let _ = run("systemctl", &["daemon-reload"]);
        println!("已注销 systemd 服务。");
        return Ok(());
    }

    #[cfg(windows)]
    {
        let _ = run("sc.exe", &["stop", "lessord"]);
        run("sc.exe", &["delete", "lessord"])?;
        println!("已注销 Windows 服务。");
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    bail!("当前平台没有可注销的服务");
}

/// 从命令输出里认出"权限不足"。
///
/// 各家措辞不同，Windows 上还是本地化的（中文系统是"拒绝访问"），
/// 所以两种语言的关键词都要认 —— 和 lessor-net 里那份是同一个思路。
fn looks_like_permission_error(text: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "Access is denied",
        "OpenSCManager",
        "拒绝访问",
        "requires elevation",
        "需要提升",
        "Permission denied",
        "must be root",
        "Interactive authentication required",
    ];
    NEEDLES.iter().any(|n| text.contains(n))
}

/// 注册服务需要什么身份 —— 各平台叫法不同，报错时用当地的说法。
fn privilege_hint() -> &'static str {
    if cfg!(windows) {
        concat!(
            "注册系统服务需要管理员权限。
",
            "请以管理员身份打开终端后重试。
",
            "注意 lessord 本身跑起来不需要管理员 —— 只有注册服务这一步需要。",
        )
    } else {
        concat!(
            "注册系统服务需要 root 权限。
",
            "请用 sudo 重试。
",
            "注意 lessord 本身跑起来不需要 root —— 只有注册服务这一步需要。",
        )
    }
}

/// 跑一条系统命令，失败时把它自己的输出带出来 —— 没有这个几乎没法定位。
///
/// 权限不足单独识别：那是最常见的失败，而且和"参数写错了"要采取的行动
/// 完全不同（换个身份重来，而不是改命令）。
#[allow(dead_code)]
fn run(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("执行不了 {program} —— 系统上有这个命令吗？"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let msg = if err.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        err.into_owned()
    };
    let msg = msg.trim();

    if looks_like_permission_error(msg) {
        bail!("{}", privilege_hint());
    }
    bail!("{program} 执行失败：{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_errors_are_recognised_in_both_languages() {
        // Windows 的 sc.exe 输出是本地化的，只认英文的话中文系统上
        // 权限问题会被当成普通失败，给出的建议就没用了
        for text in [
            "[SC] OpenSCManager 失败 5:

拒绝访问。",
            "[SC] OpenSCManager FAILED 5:

Access is denied.",
            "Failed to enable unit: Interactive authentication required",
            "systemctl: Permission denied",
        ] {
            assert!(
                looks_like_permission_error(text),
                "应识别为权限问题: {text}"
            );
        }
    }

    #[test]
    fn ordinary_failures_are_not_mistaken_for_permission_errors() {
        // 这些要照原样报出来 —— 提示"用管理员重试"只会误导
        for text in [
            "[SC] CreateService 失败 1073: 指定的服务已存在。",
            "Failed to start lessord.service: Unit not found.",
            "",
        ] {
            assert!(
                !looks_like_permission_error(text),
                "不该识别为权限问题: {text}"
            );
        }
    }
}
