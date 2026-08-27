//! HTTP / WebSocket 接口。
//!
//! 界面是纯客户端，不带任何特权 —— 网络相关的事全在这个进程里做完，
//! 前端只通过这套接口读状态、下命令。Web 端和 Tauri 桌面端加载的是同一份 UI。

use std::net::Ipv4Addr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use lessor_core::{Lease, ScopeId};
use lessor_net::Ipv4Cidr;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use crate::config::Listener;
use crate::state::{AppState, ScopeStatus, now};
use crate::ui::Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/state", get(get_state))
        .route("/api/leases", get(get_leases))
        .route("/api/leases/{scope_id}/{ip}", delete(revoke_lease))
        .route("/api/interfaces", get(get_interfaces))
        .route("/api/discover", post(post_discover))
        .route("/api/events", get(events))
        .with_state(state)
        // 前端资源打包在二进制里，任何未匹配的路径都交给它 ——
        // 这样单页应用的前端路由才不会 404
        .fallback(serve_ui)
}

async fn healthz() -> &'static str {
    "ok"
}

// ---------- 状态 ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    version: &'static str,
    started_at: u64,
    uptime_secs: u64,
    scopes: Vec<ScopeStatus>,
    listeners: Vec<Listener>,
}

async fn get_state(State(st): State<AppState>) -> Json<StateResponse> {
    Json(StateResponse {
        version: env!("CARGO_PKG_VERSION"),
        started_at: st.started_at,
        uptime_secs: now().saturating_sub(st.started_at),
        scopes: st.scope_status().await,
        listeners: st.listeners().await,
    })
}

async fn get_leases(State(st): State<AppState>) -> Json<Vec<Lease>> {
    Json(st.leases().await)
}

async fn revoke_lease(
    State(st): State<AppState>,
    Path((scope_id, ip)): Path<(u32, Ipv4Addr)>,
) -> StatusCode {
    if st.revoke(ScopeId(scope_id), ip).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

// ---------- 网卡与发现 ----------

async fn get_interfaces() -> Response {
    match lessor_net::interfaces() {
        Ok(list) => Json(list).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverRequest {
    /// 在哪个网段上找 —— 通常填本机某块网卡的地址
    addr: Ipv4Addr,
    prefix: u8,
    /// 是否逐个探测整个网段。默认开，网段过大时服务端会自动跳过。
    #[serde(default = "yes")]
    sweep: bool,
    /// 每轮等待毫秒数
    #[serde(default = "default_wait_ms")]
    wait_ms: u64,
}

fn yes() -> bool {
    true
}

fn default_wait_ms() -> u64 {
    1500
}

async fn post_discover(Json(req): Json<DiscoverRequest>) -> Response {
    if req.prefix > 32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "前缀长度必须在 0-32 之间" })),
        )
            .into_response();
    }
    let mut opts = discovery::Options::new(Ipv4Cidr {
        addr: req.addr,
        prefix: req.prefix,
    });
    opts.sweep = req.sweep;
    // 给个上限，避免前端传一个巨大的值把请求挂死
    opts.wait = Duration::from_millis(req.wait_ms.clamp(200, 10_000));

    Json(discovery::scan(opts).await).into_response()
}

// ---------- 事件流 ----------

async fn events(ws: WebSocketUpgrade, State(st): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| pump(socket, st))
}

/// 把事件流推给一个 WebSocket 客户端，直到对端断开。
async fn pump(mut socket: WebSocket, st: AppState) {
    let mut rx = st.subscribe();
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    let Ok(text) = serde_json::to_string(&ev) else { continue };
                    if socket.send(WsMessage::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // 客户端太慢跟不上。丢掉的事件不重发 —— 保证 DHCP 主循环
                // 永远不会被一个卡住的浏览器拖住。
                Err(RecvError::Lagged(n)) => {
                    debug!(skipped = n, "WebSocket 客户端跟不上，已丢弃部分事件");
                    let notice = serde_json::json!({ "kind": "lagged", "skipped": n });
                    if socket
                        .send(WsMessage::Text(notice.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            // 对端关闭或出错时结束这个连接
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ---------- 前端资源 ----------

async fn serve_ui(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(f) = Assets::get(path) {
        return (
            [(header::CONTENT_TYPE, f.metadata.mimetype())],
            Body::from(f.data.into_owned()),
        )
            .into_response();
    }

    // 单页应用的前端路由：非资源路径一律回 index.html，由前端接管
    if let Some(f) = Assets::get("index.html") {
        return (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            Body::from(f.data.into_owned()),
        )
            .into_response();
    }

    // 没有构建过前端时给一份能用的说明，而不是空白的 404
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        concat!(
            "lessor ",
            env!("CARGO_PKG_VERSION"),
            "\n\n界面尚未构建。在 ui/ 目录执行 `pnpm install && pnpm build`，\n",
            "然后重新编译 lessord。\n\n可用接口：\n",
            "  GET    /healthz\n",
            "  GET    /api/state\n",
            "  GET    /api/leases\n",
            "  DELETE /api/leases/{scope_id}/{ip}\n",
            "  GET    /api/interfaces\n",
            "  POST   /api/discover\n",
            "  GET    /api/events   (WebSocket)\n",
        ),
    )
        .into_response()
}
