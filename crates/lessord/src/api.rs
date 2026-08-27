//! HTTP / WebSocket 接口。
//!
//! 界面是纯客户端，不带任何特权 —— 网络相关的事全在这个进程里做完，
//! 前端只通过这套接口读状态、下命令。Web 端和 Tauri 桌面端加载的是同一份 UI。

use std::net::Ipv4Addr;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get};
use axum::Router;
use lessor_core::{Lease, ScopeId};
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use crate::config::Listener;
use crate::state::{AppState, ScopeStatus, now};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/state", get(get_state))
        .route("/api/leases", get(get_leases))
        .route("/api/leases/{scope_id}/{ip}", delete(revoke_lease))
        .route("/api/events", get(events))
        .route("/", get(index))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn index() -> impl IntoResponse {
    // 前端还没接进来。先让访问者知道服务是活的、接口在哪。
    (
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        concat!(
            "lessor ",
            env!("CARGO_PKG_VERSION"),
            "\n\n",
            "界面尚未构建。当前可用的接口：\n",
            "  GET    /healthz\n",
            "  GET    /api/state\n",
            "  GET    /api/leases\n",
            "  DELETE /api/leases/{scope_id}/{ip}\n",
            "  GET    /api/events   (WebSocket)\n",
        ),
    )
}

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
                    let notice = serde_json::json!({
                        "kind": "lagged",
                        "skipped": n,
                    });
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
