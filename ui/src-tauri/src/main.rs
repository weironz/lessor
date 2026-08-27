// 桌面外壳。
//
// 有意做成一个**纯客户端**：DHCP 引擎在 lessord 里跑，那个进程可能需要
// 特权、可能装成系统服务、也可能跑在另一台机器上。桌面端只负责显示，
// 加载的是同一份 Web 界面 —— 前端因此只有一套代码、一套接口。
//
// 把引擎塞进这个进程会带来两个问题：整个 webview 得跟着提权（很糟），
// 而且 Web 端和桌面端会分叉成两套数据层。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use tauri::{WebviewUrl, WebviewWindowBuilder};

/// 默认连本机的 lessord。可以用 `LESSOR_URL` 指向别处 ——
/// 比如机房里另一台机器上的服务。
const DEFAULT_URL: &str = "http://127.0.0.1:8080";

fn target_url() -> String {
    std::env::var("LESSOR_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned())
}

/// 探一下服务在不在。
///
/// 不用 HTTP 请求是为了不引入 HTTP 客户端依赖 —— 这里只需要知道
/// "端口有没有人听"，TCP 连一下就够了。
fn is_up(url: &str) -> bool {
    let Some(hostport) = url.split("://").nth(1) else {
        return false;
    };
    let hostport = hostport.split('/').next().unwrap_or(hostport);
    let addrs: Vec<SocketAddr> = match std::net::ToSocketAddrs::to_socket_addrs(&hostport) {
        Ok(it) => it.collect(),
        Err(_) => return false,
    };
    addrs
        .iter()
        .any(|a| TcpStream::connect_timeout(a, Duration::from_millis(400)).is_ok())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let url = target_url();

            // 服务在就直接进界面；不在就先给一个能自助的页面，
            // 而不是让用户对着 webview 的连接错误发呆。
            let start = if is_up(&url) {
                WebviewUrl::External(url.parse()?)
            } else {
                WebviewUrl::App("index.html".into())
            };

            let win = WebviewWindowBuilder::new(app, "main", start)
                .title("lessor")
                .inner_size(1180.0, 780.0)
                .min_inner_size(760.0, 520.0)
                .build()?;

            // 把目标地址传给回退页，它据此轮询并在服务起来后跳过去
            let js = format!(
                "window.__LESSOR_URL__ = {};",
                serde_json::to_string(&url).unwrap_or_else(|_| "\"\"".into())
            );
            let _ = win.eval(&js);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("桌面外壳启动失败");
}
