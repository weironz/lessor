//! `lessorctl` 是面向 Ansible 等自动化工具的本机 API 客户端。
//!
//! DHCP socket 仍只由 `lessord` 持有：CLI 不另起一个服务端，也不复制发现
//! 逻辑。它只调用稳定的 `/api/v1` 接口，因此桌面端、CLI 和未来 MCP 能得
//! 到同一份作用域边界与 BMC 判定结果。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::Client;
use serde_json::{Value, json};

const EXIT_NOT_FOUND: u8 = 3;

#[derive(Parser)]
#[command(name = "lessorctl", version, about = "lessor 自动化命令行客户端")]
struct Cli {
    /// lessord 的本机管理接口地址。
    #[arg(long, env = "LESSOR_URL", default_value = "http://127.0.0.1:8080")]
    endpoint: String,

    /// lessord 启动时设置 --token 后所需的管理令牌。
    #[arg(long, env = "LESSOR_TOKEN", hide_env_values = true)]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 查询或等待某个作用域中的已分类设备。
    Device(DeviceArgs),
}

#[derive(Args)]
struct DeviceArgs {
    #[command(subcommand)]
    command: DeviceCommand,
}

#[derive(Subcommand)]
enum DeviceCommand {
    /// 读取最近一次发现缓存；不会触发任何网络扫描。
    List {
        /// lessor 作用域 ID（在 /api/state 的 scopes[].id 中可见）。
        #[arg(long)]
        scope: u32,

        /// 只读取该类型的设备。
        #[arg(long, value_enum)]
        kind: Option<DeviceKind>,
    },
    /// 在这个作用域上重复发现，直到找到目标类型或超时。
    Wait {
        /// lessor 作用域 ID（在 /api/state 的 scopes[].id 中可见）。
        #[arg(long)]
        scope: u32,

        /// 要等待的设备类型。BMC 仅在 RMCP/IPMI 明确确认后返回。
        #[arg(long, value_enum, default_value_t = DeviceKind::Bmc)]
        kind: DeviceKind,

        /// 总等待时长（秒）。超时以退出码 3 结束，便于 Ansible 判断“未发现”。
        #[arg(long, default_value_t = 60)]
        timeout: u64,

        /// 两轮发现之间的间隔（毫秒）。
        #[arg(long, default_value_t = 1500)]
        interval_ms: u64,

        /// 每轮发现等待 RMCP/邻居表结果的时间（毫秒）。
        #[arg(long, default_value_t = 800)]
        scan_wait_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "lower")]
enum DeviceKind {
    Bmc,
    Device,
}

impl DeviceKind {
    fn as_api(self) -> &'static str {
        match self {
            Self::Bmc => "bmc",
            Self::Device => "device",
        }
    }
}

struct ApiClient {
    http: Client,
    endpoint: String,
    token: Option<String>,
}

impl ApiClient {
    fn new(endpoint: String, token: Option<String>) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .context("无法初始化 HTTP 客户端")?,
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            token,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self
            .http
            .request(method, format!("{}{path}", self.endpoint));
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn response_json(&self, response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let body = response.text().await.context("读取 lessord 响应失败")?;
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or(body);
            bail!("lessord 返回 {status}: {detail}");
        }
        serde_json::from_str(&body).context("lessord 返回的不是有效 JSON")
    }

    async fn discover(&self, scope: u32, scan_wait_ms: u64) -> Result<()> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/api/v1/scopes/{scope}/discover"),
            )
            .json(&json!({ "sweep": true, "waitMs": scan_wait_ms }))
            .send()
            .await
            .context("无法连接 lessord；请确认服务正在运行且 --endpoint 正确")?;
        let _ = self.response_json(response).await?;
        Ok(())
    }

    async fn devices(&self, scope: u32, kind: Option<DeviceKind>) -> Result<Value> {
        let mut path = format!("/api/v1/scopes/{scope}/devices");
        if let Some(kind) = kind {
            path.push_str("?kind=");
            path.push_str(kind.as_api());
        }
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("无法连接 lessord；请确认服务正在运行且 --endpoint 正确")?;
        self.response_json(response).await
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(value).context("序列化 JSON 输出失败")?
    );
    Ok(())
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let api = ApiClient::new(cli.endpoint, cli.token)?;
    match cli.command {
        Command::Device(DeviceArgs {
            command: DeviceCommand::List { scope, kind },
        }) => {
            print_json(&api.devices(scope, kind).await?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Device(DeviceArgs {
            command:
                DeviceCommand::Wait {
                    scope,
                    kind,
                    timeout,
                    interval_ms,
                    scan_wait_ms,
                },
        }) => {
            let started = Instant::now();
            let timeout = Duration::from_secs(timeout);
            loop {
                api.discover(scope, scan_wait_ms).await?;
                let devices = api.devices(scope, Some(kind)).await?;
                if let Some(device) = devices
                    .pointer("/data/devices")
                    .and_then(Value::as_array)
                    .and_then(|devices| devices.first())
                {
                    print_json(device)?;
                    return Ok(ExitCode::SUCCESS);
                }

                if started.elapsed() >= timeout {
                    return Ok(ExitCode::from(EXIT_NOT_FOUND));
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                tokio::time::sleep(Duration::from_millis(interval_ms).min(remaining)).await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_kind_matches_the_v1_query_value() {
        assert_eq!(DeviceKind::Bmc.as_api(), "bmc");
        assert_eq!(DeviceKind::Device.as_api(), "device");
    }
}
