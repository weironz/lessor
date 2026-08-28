//! lessor 的服务进程。
//!
//! DHCP 引擎和 HTTP 接口在同一个进程里：引擎必须特权运行（要绑 UDP 67），
//! 顺带开一个 HTTP 端口几乎是免费的。界面则是普通权限的纯客户端。

mod api;
mod config;
mod dhcp;
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

    /// 管理接口的访问令牌。给了就强制校验（写操作必须带），
    /// 不给则只有本机能用（默认只听 127.0.0.1）。
    #[arg(long, value_name = "TOKEN", env = "LESSOR_TOKEN")]
    token: Option<String>,
}

impl Cli {
    fn into_config(self) -> Result<(Config, Ports, SocketAddr, u64, Option<String>, bool)> {
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
                        "没给 --config 时必须给 --listen（本机在该网段上的地址），                         或者用 --serve-empty 先起服务再在界面上选网卡"
                    ),
                };
                // --serve-empty 时允许没有地址池：先把服务和监听器起起来，
                // 作用域在界面上建
                let pool = match (self.pool.as_deref(), self.serve_empty) {
                    (Some(p), _) => Some(config::parse_range(p)?),
                    (None, true) => None,
                    (None, false) => bail!(
                        "没给 --config 时必须给 --pool（形如 192.168.88.10-192.168.88.20），                         或者用 --serve-empty 先起服务再在界面上建作用域"
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
        Ok((
            cfg,
            ports,
            self.http,
            self.reap_secs,
            self.token.clone(),
            self.open,
        ))
    }
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

    let (cfg, ports, http_addr, reap_secs, token, open) = Cli::parse().into_config()?;

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

    let state = AppState::new(cfg.clone())
        .with_token(token)
        .with_listener_spawner(new_listener_tx);

    let mut tasks = tokio::task::JoinSet::new();

    let spawn_listener =
        |tasks: &mut tokio::task::JoinSet<()>, st: AppState, listener: Listener| {
            tasks.spawn(async move {
                if let Err(e) = dhcp::serve(st, listener, ports).await {
                    error!(error = %e, "监听器退出");
                }
            });
        };

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
                extra.spawn(async move {
                    if let Err(e) = dhcp::serve(st2, l, ports).await {
                        error!(error = %e, "监听器退出");
                    }
                });
            }
        });
    }

    {
        let st = state.clone();
        tasks.spawn(async move { dhcp::reaper(st, reap_secs).await });
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

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("收到中断，退出"),
        _ = tasks.join_next() => error!("有任务提前退出"),
    }
    Ok(())
}
