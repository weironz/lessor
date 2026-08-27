//! lessor 的服务进程。
//!
//! DHCP 引擎和 HTTP 接口在同一个进程里：引擎必须特权运行（要绑 UDP 67），
//! 顺带开一个 HTTP 端口几乎是免费的。界面则是普通权限的纯客户端。

mod api;
mod config;
mod dhcp;
mod state;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
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
}

impl Cli {
    fn into_config(self) -> Result<(Config, Ports, SocketAddr, u64)> {
        let ports = Ports {
            server: self.dhcp_port,
            client: self.client_port,
        };

        let cfg = match &self.config {
            Some(path) => Config::load(path)?,
            None => {
                let listen = self.listen.context(
                    "没给 --config 时必须给 --listen（本机在该网段上的地址）",
                )?;
                let pool = self
                    .pool
                    .as_deref()
                    .context("没给 --config 时必须给 --pool，形如 192.168.88.10-192.168.88.20")?;
                let reservations = self
                    .reservations
                    .iter()
                    .map(|r| config::parse_reservation(r))
                    .collect::<Result<Vec<_>>>()?;
                Config::from_quick(config::Quick {
                    server_ip: listen,
                    prefix: self.prefix,
                    pool: config::parse_range(pool)?,
                    router: self.router,
                    dns: self.dns.clone(),
                    lease_secs: self.lease_secs,
                    iface: self.iface.clone(),
                    reservations,
                })?
            }
        };
        Ok((cfg, ports, self.http, self.reap_secs))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("LESSOR_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let (cfg, ports, http_addr, reap_secs) = Cli::parse().into_config()?;

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

    let state = AppState::new(cfg.clone());

    let mut tasks = tokio::task::JoinSet::new();

    for listener in cfg.listeners {
        let st = state.clone();
        tasks.spawn(async move {
            if let Err(e) = dhcp::serve(st, listener, ports).await {
                error!(error = %e, "监听器退出");
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
