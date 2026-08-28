//! lessor 的服务进程。
//!
//! DHCP 引擎和 HTTP 接口在同一个进程里：引擎必须特权运行（要绑 UDP 67），
//! 顺带开一个 HTTP 端口几乎是免费的。界面则是普通权限的纯客户端。

mod api;
mod capture;
mod config;
mod conflict;
mod dhcp;
mod health;
mod service;
mod sqlite;
mod state;
mod ui;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, Listener};
use crate::dhcp::Ports;
use crate::state::AppState;

#[derive(Parser, Debug)]
#[command(name = "lessord", version, about = "lessor 的 DHCP 服务进程")]
struct Cli {
    /// 配置文件（JSON）。不给的话就用下面的快捷参数拼一个单网段配置。
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// 本机在该网段上的地址，同时用作 server-identifier
    #[arg(long, value_name = "IP")]
    listen: Option<Ipv4Addr>,

    /// 子网前缀长度
    #[arg(long, default_value_t = 24, value_name = "LEN")]
    prefix: u8,

    /// 地址池，形如 192.168.88.10-192.168.88.20
    #[arg(long, value_name = "起始-结束")]
    pool: Option<String>,

    /// 下发给客户端的网关
    #[arg(long, value_name = "IP")]
    router: Option<Ipv4Addr>,

    /// 下发给客户端的 DNS，可重复
    #[arg(long, value_name = "IP")]
    dns: Vec<Ipv4Addr>,

    /// 租期（秒）
    #[arg(long, default_value_t = 3600, value_name = "秒")]
    lease_secs: u32,

    /// 静态保留，形如 MAC=IP 或 MAC=IP=主机名，可重复
    #[arg(long = "reservation", value_name = "MAC=IP[=主机名]")]
    reservations: Vec<String>,

    /// 引导文件名（option 67）。给 PXE 固件和未自报身份的客户端。
    #[arg(long, value_name = "文件名")]
    boot_file: Option<String>,

    /// 引导文件所在的服务器地址，填进 siaddr
    #[arg(long, value_name = "IP")]
    next_server: Option<Ipv4Addr>,

    /// TFTP 服务器名（option 66）
    #[arg(long, value_name = "名称")]
    tftp_server: Option<String>,

    /// 给 UEFI HTTP Boot 客户端的引导 URL。它不走 TFTP，必须是完整 URL。
    #[arg(long, value_name = "URL")]
    http_boot_url: Option<String>,

    /// 给已经跑起来的 iPXE 的引导脚本 URL。不配的话 iPXE 会拿到和固件
    /// 一样的 --boot-file —— 那通常就是 iPXE 自己，会无限自举。
    #[arg(long, value_name = "URL")]
    ipxe_url: Option<String>,

    /// 额外的原始 DHCP 选项，形如 43=060108ff（编号=十六进制），可重复
    #[arg(long = "option", value_name = "编号=十六进制")]
    options: Vec<String>,

    /// 绑定到指定网卡。仅 Linux 生效，多网卡时靠它区分作用域。
    #[arg(long, value_name = "网卡名")]
    iface: Option<String>,

    /// HTTP 接口监听地址。默认只听本机 —— 这是管理接口，不该暴露到网络上。
    #[arg(long, default_value = "127.0.0.1:8080", value_name = "地址:端口")]
    http: SocketAddr,

    /// DHCP 服务端口。改成高位端口可以免特权运行，便于测试。
    #[arg(long, default_value_t = 67, value_name = "端口")]
    dhcp_port: u16,

    /// DHCP 客户端端口，与上面配套使用。
    #[arg(long, default_value_t = 68, value_name = "端口")]
    client_port: u16,

    /// 过期租约的回收间隔（秒）
    #[arg(long, default_value_t = 30, value_name = "秒")]
    reap_secs: u64,

    /// 不带作用域启动，等着在界面上建。桌面端与"界面优先"的用法走这条 ——
    /// 零作用域时不应答任何请求，安全。
    #[arg(long)]
    serve_empty: bool,

    /// 启动后自动打开浏览器指向本机控制台。
    /// 想要"双击即用"又不装桌面端时走这条。
    #[arg(long)]
    open: bool,

    /// 租约落到这个 sqlite 文件，重启不丢。常驻部署应当给上。
    /// 不给则租约只在内存里 —— 现场临时用正合适，关了不留痕。
    #[arg(long, value_name = "路径")]
    lease_db: Option<PathBuf>,

    /// 注册成系统服务（systemd / Windows 服务）后退出。
    /// 其余参数会被记进服务的启动命令行。
    #[arg(long)]
    install_service: bool,

    /// 注销系统服务后退出。
    #[arg(long)]
    uninstall_service: bool,

    /// 关掉 OFFER 前的地址冲突探测。
    ///
    /// 探测本身不需要特权也不在握手路径上，正常不该关；留这个开关是给
    /// 那些禁止主动探测的网络（有些安全策略会把 ARP 扫描当告警）。
    #[arg(long)]
    no_probe: bool,

    /// 把收到的每个报文原样存进这个文件（JSONL，含原始字节）。
    ///
    /// 给真机取证用：现场跑一遍，把文件带回来用 --replay 离线复现，
    /// 每个厂商怪癖都能钉成一条回归测试。捕获在解码之前发生 ——
    /// 解不出来的包恰恰最值钱。不需要抓包驱动，也不需要特权。
    #[arg(long, value_name = "路径")]
    capture: Option<PathBuf>,

    /// 只看不答：收包、记录、在界面上显示"本来会怎么答"，但一个字节都不发。
    ///
    /// 挂在**已经有 DHCP 的生产网段**上取证时必须开 —— 否则就是两个
    /// DHCP 抢答，机器可能装到一半失联。远程给真机 BMC 取怪癖走这条。
    #[arg(long)]
    observe: bool,

    /// 重放一个 --capture 出来的文件，逐条打印决策层的结论后退出。
    ///
    /// 走的是真正的 handle()，不是另写一套模拟。作用域取自 --config
    /// 或下面那些快捷参数。
    #[arg(long, value_name = "路径")]
    replay: Option<PathBuf>,

    /// 闲置这么多秒后自行退出。给现场临时使用：装完机走人，
    /// 不用记得回来关掉它。常驻部署不要开 —— 没人要地址不代表服务该消失。
    ///
    /// 下限 5 秒：再小的值只会在服务刚起来、客户端还没来得及发第一个
    /// DISCOVER 时就把自己关掉。
    #[arg(long, value_name = "秒", value_parser = clap::value_parser!(u64).range(5..))]
    idle_exit: Option<u64>,

    /// 管理接口的访问令牌。给了就强制校验（写操作必须带），
    /// 不给则只有本机能用（默认只听 127.0.0.1）。
    #[arg(long, value_name = "TOKEN", env = "LESSOR_TOKEN")]
    token: Option<String>,
}

/// 解析命令行之后要交给主流程的一切。
struct Started {
    cfg: Config,
    ports: Ports,
    http: SocketAddr,
    reap_secs: u64,
    token: Option<String>,
    open: bool,
    lease_db: Option<PathBuf>,
    config_path: Option<PathBuf>,
    no_probe: bool,
    idle_exit: Option<u64>,
    capture: Option<PathBuf>,
    observe: bool,
}

impl Cli {
    fn into_config(self) -> Result<Started> {
        let ports = Ports {
            server: self.dhcp_port,
            client: self.client_port,
        };

        let cfg = match &self.config {
            Some(path) => Config::load(path)?,
            None => {
                // --serve-empty 时监听器也可以先不建：网卡在界面上选
                let listen = match (self.listen, self.serve_empty) {
                    (Some(ip), _) => Some(ip),
                    (None, true) => None,
                    (None, false) => bail!(
                        "没给 --config 时必须给 --listen（本机在该网段上的地址）；或者用 --serve-empty 先起服务，再在界面上选网卡"
                    ),
                };
                // --serve-empty 时允许没有地址池：先把服务和监听器起起来，
                // 作用域在界面上建
                let pool = match (self.pool.as_deref(), self.serve_empty) {
                    (Some(p), _) => Some(config::parse_range(p)?),
                    (None, true) => None,
                    (None, false) => bail!(
                        "没给 --config 时必须给 --pool（形如 192.168.88.10-192.168.88.20）；或者用 --serve-empty 先起服务，再在界面上建作用域"
                    ),
                };
                let reservations = self
                    .reservations
                    .iter()
                    .map(|r| config::parse_reservation(r))
                    .collect::<Result<Vec<_>>>()?;
                Config::from_quick(config::Quick {
                    server_ip: listen,
                    prefix: self.prefix,
                    pool,
                    router: self.router,
                    dns: self.dns.clone(),
                    lease_secs: self.lease_secs,
                    iface: self.iface.clone(),
                    reservations,
                    boot: {
                        let boot = lessor_core::BootConfig {
                            filename: self.boot_file.clone(),
                            next_server: self.next_server,
                            server_name: self.tftp_server.clone(),
                            http_url: self.http_boot_url.clone(),
                            ipxe_url: self.ipxe_url.clone(),
                        };
                        (!boot.is_empty()).then_some(boot)
                    },
                    extra_options: self
                        .options
                        .iter()
                        .map(|o| config::parse_option(o))
                        .collect::<Result<Vec<_>>>()?,
                })?
            }
        };
        Ok(Started {
            cfg,
            ports,
            http: self.http,
            reap_secs: self.reap_secs,
            token: self.token.clone(),
            open: self.open,
            lease_db: self.lease_db.clone(),
            config_path: self.config.clone(),
            no_probe: self.no_probe,
            idle_exit: self.idle_exit,
            capture: self.capture.clone(),
            observe: self.observe,
        })
    }
}

/// 重放一个捕获文件，把结论打出来。
///
/// 无法解码的放在最后单独列 —— 那些才是要动手的地方，混在一长串
/// "正常"里会被划过去。
fn run_replay(path: &std::path::Path, scopes: Vec<lessor_core::Scope>) -> Result<()> {
    let results = capture::replay(path, scopes)?;
    if results.is_empty() {
        println!("{} 里没有记录。", path.display());
        return Ok(());
    }

    let mut odd = Vec::new();
    println!("共 {} 条：\n", results.len());
    for (line, verdict) in &results {
        match verdict {
            capture::Verdict::Decided(s) => println!("  #{line:<4} {s}"),
            capture::Verdict::Undecodable(s) => {
                println!("  #{line:<4} !! 无法解码");
                odd.push((line, s));
            }
        }
    }

    if odd.is_empty() {
        println!("\n全部能解码。");
        return Ok(());
    }
    println!("\n{} 条无法解码 —— 这些是要看的：\n", odd.len());
    for (line, s) in odd {
        println!("  #{line}: {s}\n");
    }
    Ok(())
}

/// 用系统默认程序打开一个 URL。
///
/// 不引第三方 crate：三个平台各有一条现成命令，加起来比一个依赖便宜。
fn open_browser(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    let mut cmd = if cfg!(target_os = "windows") {
        // 走 cmd 的 start；第一个空参数是窗口标题占位，省掉它会把 URL 当标题
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("LESSOR_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // 服务注册是一次性动作，装完就退，不进主循环
    let cli = Cli::parse();
    if cli.uninstall_service {
        return service::uninstall();
    }
    if cli.install_service {
        // 把除 --install-service 之外的参数原样传给服务
        let args: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| a != "--install-service")
            .collect();
        return service::install(&args);
    }

    // 重放同样是一次性动作：读文件、打结论、退出，不碰网络
    if let Some(path) = cli.replay.clone() {
        let scopes = cli.into_config()?.cfg.scopes;
        return run_replay(&path, scopes);
    }

    let Started {
        cfg,
        ports,
        http: http_addr,
        reap_secs,
        token,
        open,
        lease_db,
        config_path,
        no_probe,
        idle_exit,
        capture: capture_path,
        observe,
    } = cli.into_config()?;

    // 非 Linux 上没有把 socket 钉在网卡上的办法，多监听器时无法判断
    // 包从哪块网卡进来，可能把请求算到错误的作用域上。
    if cfg.listeners.len() > 1 && !cfg!(target_os = "linux") {
        warn!(
            "配置了 {} 个监听器，但当前平台无法把 socket 绑定到网卡。\
             直连请求可能被归到错误的作用域；经中继的请求不受影响。",
            cfg.listeners.len()
        );
    }

    for s in &cfg.scopes {
        info!(
            scope = %s.name,
            subnet = %format!("{}/{}", s.subnet, s.prefix),
            capacity = s.capacity(),
            enabled = s.enabled,
            "载入作用域"
        );
    }

    if cfg.scopes.is_empty() {
        info!("没有作用域，暂不应答任何 DHCP 请求 —— 在界面上新建一个即可开始服务");
    }

    // 界面上新建作用域时，可能需要在新网卡上起一个监听器 ——
    // 通过这个 channel 送回来 spawn。
    let (new_listener_tx, mut new_listener_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut state = AppState::new(cfg.clone())
        .with_token(token)
        .with_listener_spawner(new_listener_tx)
        .with_config_path(config_path);

    let capture = match &capture_path {
        Some(p) => {
            let c = capture::Capture::open(p)?;
            info!(file = %p.display(), "报文捕获已开启 —— 收到的每个包都会原样存下来");
            Some(c)
        }
        None => None,
    };
    state = state.with_capture(capture).with_observe(observe);
    if observe {
        warn!("只看不答模式：会收包、会记录、界面上能看到本来会怎么答，但不会往网络上发任何东西");
    }

    if let Some(path) = &lease_db {
        let store = sqlite::SqliteStore::open(path)
            .with_context(|| format!("租约库 {} 不可用", path.display()))?;
        let restored = store.count();
        state = state.with_store(state::Store::Sqlite(store));
        info!(db = %path.display(), leases = restored, "租约持久化已启用");
    }

    let mut tasks = tokio::task::JoinSet::new();

    let spawn_listener =
        |tasks: &mut tokio::task::JoinSet<()>, st: AppState, listener: Listener| {
            tasks.spawn(dhcp::serve_forever(st, listener, ports));
        };

    // 后面探测同段其他 DHCP 要用，先留一份地址
    let listen_addrs: Vec<std::net::Ipv4Addr> = cfg.listeners.iter().map(|l| l.server_ip).collect();

    for listener in cfg.listeners {
        spawn_listener(&mut tasks, state.clone(), listener);
    }

    {
        // 运行时新增的监听器
        let st = state.clone();
        let mut extra = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            while let Some(l) = new_listener_rx.recv().await {
                info!(server_ip = %l.server_ip, "界面新增了作用域，起一个监听器");
                let st2 = st.clone();
                extra.spawn(dhcp::serve_forever(st2, l, ports));
            }
        });
    }

    {
        let st = state.clone();
        tasks.spawn(async move { dhcp::reaper(st, reap_secs).await });
    }

    if !no_probe {
        // 后台持续探测地址占用。结果进缓存，分配路径只查缓存，
        // 所以不会拖慢握手。
        let st = state.clone();
        let occ = state.occupied.clone();
        tasks.spawn(async move {
            conflict::sweeper(st, occ, std::time::Duration::from_secs(60)).await;
        });

        // 启动时探一次同网段有没有别的 DHCP 服务器。
        // 这是安全红线：把 DHCP 插进已有 DHCP 的网段会让机器装到一半失联。
        for bind in listen_addrs {
            tokio::spawn(async move {
                match conflict::detect_foreign_servers(bind, std::time::Duration::from_secs(3))
                    .await
                {
                    Ok(found) if !found.is_empty() => warn!(
                        servers = ?found,
                        "本网段已有其他 DHCP 服务器。两边同时发地址会互相干扰 —— \
                         确认这是你要的，否则请停掉其中一边"
                    ),
                    Ok(_) => info!(iface = %bind, "本网段未发现其他 DHCP 服务器"),
                    // 查不成必须说出来。这是条安全检查，闷掉之后人会以为
                    // "没告警就是没冲突" —— 假的全清信号比不查更糟
                    Err(e) => warn!(iface = %bind, error = %e, "同网段 DHCP 检查没能进行"),
                }
            });
        }
    }

    {
        let app = api::router(state.clone());
        let tcp = tokio::net::TcpListener::bind(http_addr)
            .await
            .with_context(|| format!("HTTP 监听 {http_addr} 失败"))?;
        info!(addr = %http_addr, "HTTP 接口已就绪");

        if open {
            // 监听已经建好了，这时候开浏览器不会扑空。
            // 打不开不是致命错误 —— 地址已经打在日志里，人工点进去就是了。
            let url = format!("http://{http_addr}/");
            if let Err(e) = open_browser(&url) {
                warn!(%url, error = %e, "打不开浏览器，请手工访问上面的地址");
            }
        }
        tasks.spawn(async move {
            if let Err(e) = axum::serve(tcp, app).await {
                error!(error = %e, "HTTP 服务退出");
            }
        });
    }

    {
        // "起来了但一个请求都没有"是现场第一大故障形态，而它安静得
        // 和"网段上还没有客户端"一模一样。到点了主动说一句。
        //
        // 刻意不放进 tasks：收到第一个包之后它就功成身退正常返回了，
        // 而 tasks 里任何一个任务结束都被当成故障。放进去的话，
        // 服务会在"跑满一分钟且有流量"之后把自己判死。
        let st = state.clone();
        tokio::spawn(health::watch_quiet(st));
    }

    // 空闲自动退出。没给 --idle-exit 时是一个永不返回的 future，
    // 让下面的 select 结构保持一致
    let idle = async {
        match idle_exit {
            Some(secs) => {
                health::wait_until_idle(state.clone(), std::time::Duration::from_secs(secs)).await
            }
            None => std::future::pending().await,
        }
    };

    tokio::select! {
        _ = health::shutdown_signal() => Ok(()),
        _ = idle => Ok(()),
        // 任务提前结束是故障，不能报 exit 0 —— systemd 的 Restart=on-failure
        // 和 Windows 服务的失败重启都只看退出码，报 0 就没人来救了
        _ = tasks.join_next() => Err(anyhow::anyhow!(
            "有任务提前退出，服务已不完整 —— 上面的日志里有原因"
        )),
    }
}
