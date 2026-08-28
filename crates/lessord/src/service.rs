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
         \n\
         # 以专用用户运行。一个对外收包的服务没有理由是 root ——\n\
         # 绑 67 端口靠下面的 capability 就够了。\n\
         User={user}\n\
         Group={user}\n\
         AmbientCapabilities=CAP_NET_BIND_SERVICE\n\
         # 上面那行给的是「拿得到」，这行限的是「最多只能有这个」\n\
         CapabilityBoundingSet=CAP_NET_BIND_SERVICE\n\
         NoNewPrivileges=yes\n\
         \n\
         # systemd 建好 {state_dir} 并归属给上面那个用户。\n\
         # 租约库和配置文件都放这儿 —— 配置写回是「同目录建临时文件再改名」，\n\
         # 需要的是父目录的写权限，所以它必须是一个我们自己拥有的目录。\n\
         StateDirectory={name}\n\
         \n\
         # 除了状态目录，整个文件系统只读；再关掉一堆用不到的东西。\n\
         ProtectSystem=strict\n\
         ProtectHome=yes\n\
         PrivateTmp=yes\n\
         PrivateDevices=yes\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectControlGroups=yes\n\
         RestrictRealtime=yes\n\
         LockPersonality=yes\n\
         SystemCallArchitectures=native\n\
         # AF_NETLINK 是枚举网卡要用的，去掉它连自己有哪些地址都看不见\n\
         RestrictAddressFamilies=AF_INET AF_INET6 AF_NETLINK AF_UNIX\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe = exe.display(),
        user = SERVICE_USER,
        name = SERVICE_USER,
        state_dir = STATE_DIR,
    )
}

/// 服务用的专用系统用户，同时也是 `StateDirectory` 的名字。
#[cfg(target_os = "linux")]
const SERVICE_USER: &str = "lessord";

/// `StateDirectory=lessord` 落在这里，属主是上面那个用户。
#[cfg(target_os = "linux")]
const STATE_DIR: &str = "/var/lib/lessord";

/// 把自己注册成系统服务。`args` 是服务启动时要带的参数。
///
/// 各平台的实现拆成独立函数，而不是在一个函数体里堆 `#[cfg]` 块。
/// 后者看着更紧凑，但每个块都得靠 `return` 收尾，于是**在只剩一个块的
/// 平台上，那个 `return` 就成了多余的**，clippy 会报 needless_return ——
/// 而在写代码的那台机器上永远看不到，因为那个分支被 cfg 掉了。
/// 拆开之后每个实现只有一个出口，各平台都干净。
pub fn install(args: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("拿不到自己的可执行文件路径")?;
    let args = absolutize_paths(args);
    #[cfg(target_os = "linux")]
    check_writable_paths(&args, STATE_DIR)?;
    install_on(&exe, &args.join(" "))?;
    // 确认它真的活着**之后**才报成功。顺序反了的话，屏幕上会先出现
    // "已注册并启动"再跟一条报错 —— 人只会记住前一句。
    verify_running()?;
    print_success();
    Ok(())
}

/// 接路径的那几个参数，注册前一律转成绝对路径。
///
/// **服务的工作目录不是你注册时所在的目录**（systemd 默认 `/`，
/// Windows 服务是 `C:\Windows\System32`）。带着 `--config ./lessor.json`
/// 去注册，装完立刻就是"No such file or directory"然后反复重启 ——
/// 实测过。而现场几乎一定会这么写，因为注册前刚在那个目录里试过一遍。
fn absolutize_paths(args: &[String]) -> Vec<String> {
    /// 值是路径的参数。`--replay` 不在其中：它是一次性动作，不会进服务。
    const PATH_FLAGS: &[&str] = &["-c", "--config", "--lease-db", "--capture"];

    let abs =
        |v: &str| std::path::absolute(v).map_or_else(|_| v.to_owned(), |p| p.display().to_string());

    let mut out = Vec::with_capacity(args.len());
    let mut take_next = false;
    for a in args {
        if take_next {
            take_next = false;
            out.push(abs(a));
            continue;
        }
        // --config=路径 这种写法也要认
        if let Some((flag, value)) = a.split_once('=')
            && PATH_FLAGS.contains(&flag)
        {
            out.push(format!("{flag}={}", abs(value)));
            continue;
        }
        take_next = PATH_FLAGS.contains(&a.as_str());
        out.push(a.clone());
    }
    out
}

/// 装完之后确认它真的还活着。
///
/// **"启动命令成功"和"服务在跑"是两回事**：`systemctl start` 只等到进程被
/// 拉起来，进程随后立刻退出它照样算成功。实测把相对路径带进去时，注册
/// 打印"已注册为 systemd 服务并启动"，而服务正在崩溃重启循环里。
///
/// 报一个假的成功比报错糟得多 —— 人看到"已启动"就走开了，等发现的时候
/// 已经是几小时之后、在别的地方。
fn verify_running() -> Result<()> {
    // 起来需要一点时间，多试几次再下结论
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if service_is_running()? {
            return Ok(());
        }
    }
    bail!("{}", not_running_hint());
}

fn not_running_hint() -> &'static str {
    if cfg!(windows) {
        concat!(
            "服务注册成功了，但它没能跑起来 —— 多半是启动参数有问题。\n",
            "看日志：Get-EventLog -LogName Application -Source lessord -Newest 20\n",
            "查状态：sc query lessord\n",
            "常见原因：给 --config / --lease-db 用了相对路径（服务的工作目录\n",
            "是 C:\\Windows\\System32，不是你注册时那个目录），或者端口被占用。",
        )
    } else {
        concat!(
            "服务注册成功了，但它没能跑起来 —— 多半是启动参数有问题。\n",
            "看日志：journalctl -u lessord -n 50 --no-pager\n",
            "常见原因：给 --config / --lease-db 用了相对路径（服务的工作目录\n",
            "是 /，不是你注册时那个目录），或者端口被占用。",
        )
    }
}

/// 服务要写的那几个文件。这些必须落在状态目录里 —— 见
/// [`check_writable_paths`]。
#[cfg(target_os = "linux")]
const WRITABLE_FLAGS: &[&str] = &["-c", "--config", "--lease-db", "--capture"];

/// 降权之后，服务要写的文件必须在它自己拥有的目录里。
///
/// **不是"文件可写就行"**：配置写回的做法是"在同目录建一个临时文件再改名"，
/// 需要的是**父目录**的写权限。给 `/etc` 开写权限显然不行，所以只能要求
/// 这些文件放进 `StateDirectory`。
///
/// 装之前就查，因为装完再失败的话，服务会进崩溃重启循环 ——
/// 那种状态下的报错远不如现在这条清楚。
#[cfg(target_os = "linux")]
fn check_writable_paths(args: &[String], state_dir: &str) -> Result<()> {
    let mut bad = Vec::new();
    let mut take_next = false;
    for a in args {
        let value = if take_next {
            take_next = false;
            Some(a.as_str())
        } else if let Some((flag, v)) = a.split_once('=')
            && WRITABLE_FLAGS.contains(&flag)
        {
            Some(v)
        } else {
            take_next = WRITABLE_FLAGS.contains(&a.as_str());
            None
        };
        if let Some(v) = value
            && !std::path::Path::new(v).starts_with(state_dir)
        {
            bad.push(v.to_owned());
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    bail!(
        "服务以 {user} 用户运行（不是 root），下面这些文件它写不了：\n  {list}\n\n\
         把它们放到 {dir}/ 下面再注册，例如：\n  \
         sudo mkdir -p {dir} && sudo cp <你的文件> {dir}/\n  \
         lessord --install-service --config {dir}/lessor.json ...\n\n\
         为什么必须是这个目录：配置写回是「同目录建临时文件再改名」，需要父目录\n\
         的写权限；那个目录由 systemd 建好并归属给服务用户。",
        user = SERVICE_USER,
        dir = state_dir,
        list = bad.join("\n  "),
    )
}

/// 确保服务用的系统用户存在。已经有了就什么都不做。
#[cfg(target_os = "linux")]
fn ensure_service_user() -> Result<()> {
    let exists = Command::new("id")
        .args(["-u", SERVICE_USER])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    run(
        "useradd",
        &[
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            SERVICE_USER,
        ],
    )
    .with_context(|| format!("建不了系统用户 {SERVICE_USER} —— 服务需要它来降权运行"))
}

#[cfg(target_os = "linux")]
fn install_on(exe: &std::path::Path, args: &str) -> Result<()> {
    ensure_service_user()?;
    let unit_path = std::path::Path::new("/etc/systemd/system/lessord.service");
    std::fs::write(unit_path, systemd_unit(exe, args))
        .with_context(|| format!("写不了 {} —— 注册服务需要 root", unit_path.display()))?;
    run("systemctl", &["daemon-reload"])?;
    run("systemctl", &["enable", "lessord"])?;
    run("systemctl", &["start", "lessord"])?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn print_success() {
    println!("已注册为 systemd 服务并启动。");
    println!("  状态：systemctl status lessord");
    println!("  日志：journalctl -u lessord -f");
    println!("  卸载：lessord --uninstall-service");
}

#[cfg(windows)]
fn install_on(exe: &std::path::Path, args: &str) -> Result<()> {
    // sc.exe 的 binPath= 后面必须有空格，且整条命令行要作为一个参数传。
    // 可执行文件路径可能含空格，所以内层再加一层引号。
    let bin = format!("\"{}\" {args}", exe.display());
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
    Ok(())
}

#[cfg(windows)]
fn print_success() {
    println!("已注册为 Windows 服务并启动。");
    println!("  状态：sc query lessord");
    println!("  卸载：lessord --uninstall-service");
}

#[cfg(not(any(target_os = "linux", windows)))]
fn install_on(_exe: &std::path::Path, _args: &str) -> Result<()> {
    bail!("当前平台还不支持注册系统服务，请手工用进程管理器托管")
}

#[cfg(not(any(target_os = "linux", windows)))]
fn print_success() {}

/// 注销系统服务。
pub fn uninstall() -> Result<()> {
    uninstall_on()
}

#[cfg(target_os = "linux")]
fn uninstall_on() -> Result<()> {
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
    Ok(())
}

#[cfg(windows)]
fn uninstall_on() -> Result<()> {
    let _ = run("sc.exe", &["stop", "lessord"]);
    run("sc.exe", &["delete", "lessord"])?;
    println!("已注销 Windows 服务。");
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn uninstall_on() -> Result<()> {
    bail!("当前平台没有可注销的服务")
}

/// 服务此刻是不是真在跑。
#[cfg(target_os = "linux")]
fn service_is_running() -> Result<bool> {
    // is-active 在非 active 时退出码非零，所以不能用 run()（它会当成失败）
    let out = Command::new("systemctl")
        .args(["is-active", "lessord"])
        .output()
        .context("执行不了 systemctl")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "active")
}

#[cfg(windows)]
fn service_is_running() -> Result<bool> {
    let out = Command::new("sc.exe")
        .args(["query", "lessord"])
        .output()
        .context("执行不了 sc.exe")?;
    // sc query 的标签是本地化的（中文系统上是"状态"），但状态名
    // RUNNING / STOPPED 本身不翻译。数字 4 是 SERVICE_RUNNING，
    // 两个都认一下，免得押错一边。
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.contains("RUNNING") || text.contains(": 4"))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn service_is_running() -> Result<bool> {
    Ok(true)
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
    fn path_arguments_become_absolute() {
        // 服务的工作目录不是注册时那个目录。相对路径带进去，装完立刻
        // "No such file or directory" 然后反复重启 —— 而现场几乎一定会
        // 这么写，因为注册前刚在那个目录里试过。
        let args: Vec<String> = ["--config", "./lessor.json", "--http", "127.0.0.1:8080"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let out = absolutize_paths(&args);

        assert_ne!(out[1], "./lessor.json", "相对路径必须被展开");
        assert!(std::path::Path::new(&out[1]).is_absolute());
        assert!(out[1].ends_with("lessor.json"));
        // 不接路径的参数一个字都不能动
        assert_eq!(out[2], "--http");
        assert_eq!(out[3], "127.0.0.1:8080");
    }

    #[test]
    fn equals_form_is_handled_too() {
        let args = vec!["--lease-db=leases.db".to_owned()];
        let out = absolutize_paths(&args);
        let (flag, value) = out[0].split_once('=').expect("应当保持 flag=value 形式");
        assert_eq!(flag, "--lease-db");
        assert!(std::path::Path::new(value).is_absolute(), "实际: {value}");
    }

    #[test]
    fn a_value_that_looks_like_a_flag_is_still_taken_as_the_path() {
        // --config 后面跟什么就是什么，不去猜。漏了这条的话
        // "--config --lease-db" 这种写错的命令会被悄悄改成别的意思
        let args = vec!["--config".to_owned(), "x.json".to_owned()];
        assert_eq!(absolutize_paths(&args).len(), 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn writable_paths_outside_the_state_dir_are_refused() {
        // 降权之后服务写不了别处。装之前就拦住 —— 装完再失败的话服务会进
        // 崩溃重启循环，那种状态下的报错远不如这条清楚。
        let args: Vec<String> = ["--config", "/etc/lessor.json"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let err = check_writable_paths(&args, "/var/lib/lessord")
            .expect_err("状态目录之外的可写路径必须被拒")
            .to_string();
        assert!(err.contains("/etc/lessor.json"), "要点名是哪个文件: {err}");
        assert!(err.contains("/var/lib/lessord"), "要说清该放哪儿: {err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn writable_paths_inside_the_state_dir_pass() {
        let args: Vec<String> = [
            "--config",
            "/var/lib/lessord/lessor.json",
            "--lease-db=/var/lib/lessord/leases.db",
            "--http",
            "127.0.0.1:8080",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert!(check_writable_paths(&args, "/var/lib/lessord").is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unit_drops_privileges_and_keeps_only_the_one_capability() {
        // 这条钉的是"不以 root 跑"这个承诺。少了 User= 的话，下面那行
        // capability 就是空转的 —— 项目一直讲不需要特权，unit 里却是 root，
        // 那是说法和事实对不上。
        let unit = systemd_unit(
            std::path::Path::new("/usr/local/bin/lessord"),
            "--listen 1.2.3.4",
        );
        assert!(unit.contains("User=lessord"), "必须降权运行");
        assert!(
            unit.contains("AmbientCapabilities=CAP_NET_BIND_SERVICE"),
            "否则绑不了 67"
        );
        assert!(
            unit.contains("CapabilityBoundingSet=CAP_NET_BIND_SERVICE"),
            "只给这一个，多的一概拿不到"
        );
        assert!(unit.contains("NoNewPrivileges=yes"));
        // 枚举网卡要走 netlink，限死了连自己有哪些地址都看不见
        assert!(unit.contains("AF_NETLINK"));
        assert!(unit.contains("StateDirectory=lessord"));
    }

    #[test]
    fn failure_hint_names_the_relative_path_trap() {
        // 这条提示的全部价值是让人不用自己去猜。第一嫌疑人必须写出来。
        let h = not_running_hint();
        assert!(h.contains("相对路径"), "实际: {h}");
        assert!(h.contains("工作目录"), "实际: {h}");
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
