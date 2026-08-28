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
use axum::routing::{delete, get, patch, post};
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
        .route("/api/scopes", post(post_scope))
        .route("/api/scopes/{id}", patch(patch_scope).delete(delete_scope))
        .route("/api/scopes/{id}/reservations", post(post_reservation))
        .route(
            "/api/scopes/{id}/reservations/{client}",
            delete(delete_reservation),
        )
        .route("/api/events", get(events))
        // 写操作的守卫。放在路由之后、fallback 之前 ——
        // 前端资源和只读接口不受影响。
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
        // 前端资源打包在二进制里，任何未匹配的路径都交给它 ——
        // 这样单页应用的前端路由才不会 404
        .fallback(serve_ui)
}

async fn healthz() -> &'static str {
    "ok"
}

// ---------- 写操作守卫 ----------

/// 拦住两类风险，只作用于写操作（GET 不受影响）：
///
/// 1. **DNS rebinding**：浏览器可以被诱导把某个恶意域名解析到 127.0.0.1，
///    然后用页面脚本打本机的管理接口。校验 Host 头只允许 IP 字面量形式，
///    域名一律拒绝 —— 我们从不需要通过域名访问自己。
/// 2. **越权**：配了 `--token` 时写操作必须带 `Authorization: Bearer <token>`。
///
/// 只读接口不设防是有意的：默认只监听 127.0.0.1，读到的也只是本机网络状态。
async fn guard(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let is_write = !matches!(req.method(), &axum::http::Method::GET);
    if !is_write {
        return next.run(req).await;
    }

    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // 去掉端口后必须是 IP 字面量（IPv6 的 [::1] 也放行）
    let hostname = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let hostname = hostname.trim_start_matches('[').trim_end_matches(']');
    if !hostname.is_empty() && hostname.parse::<std::net::IpAddr>().is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: "Host 必须是 IP 字面量 —— 拒绝可能的 DNS rebinding".into(),
            }),
        )
            .into_response();
    }

    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !state.authorize(presented) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "需要 Authorization: Bearer <token>".into(),
            }),
        )
            .into_response();
    }

    next.run(req).await
}

#[derive(Serialize)]
struct ApiError {
    error: String,
}

// ---------- 新建作用域 ----------

/// 界面上建作用域用的最小参数集。
///
/// 子网由 `serverIp` 和 `prefix` 推出 —— 让人填网段容易和监听器对不上，
/// 而对不上就是"收得到请求也发不出应答"。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewScope {
    name: Option<String>,
    server_ip: Ipv4Addr,
    prefix: u8,
    pool_start: Ipv4Addr,
    pool_end: Ipv4Addr,
    router: Option<Ipv4Addr>,
    #[serde(default)]
    dns: Vec<Ipv4Addr>,
    lease_secs: Option<u32>,
}

async fn post_scope(State(state): State<AppState>, Json(req): Json<NewScope>) -> Response {
    if req.prefix == 0 || req.prefix > 32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "前缀长度要在 1-32 之间".into(),
            }),
        )
            .into_response();
    }
    let Some(pool) = lessor_core::addr::Range::new(req.pool_start, req.pool_end) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "地址池起止顺序不对".into(),
            }),
        )
            .into_response();
    };

    let mask = u32::MAX
        .checked_shl(32 - u32::from(req.prefix))
        .unwrap_or(0);
    let subnet = Ipv4Addr::from(u32::from(req.server_ip) & mask);

    let mut scope = lessor_core::Scope::new(
        0,
        req.name.unwrap_or_else(|| "新建".into()),
        subnet,
        req.prefix,
    );
    scope.pools = vec![pool];
    scope.router = req.router;
    scope.dns = req.dns;
    if let Some(secs) = req.lease_secs {
        scope.lease_secs = secs;
        scope.offer_secs = 30.min(secs);
    }

    match state.add_scope(scope).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(problems) => (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: problems.join("；"),
            }),
        )
            .into_response(),
    }
}

// ---------- 改 / 删作用域、静态保留 ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopePatchReq {
    name: Option<String>,
    enabled: Option<bool>,
    pool_start: Option<Ipv4Addr>,
    pool_end: Option<Ipv4Addr>,
    /// 显式传 null 可以清掉网关，所以是双层 Option
    #[serde(default, deserialize_with = "double_option")]
    router: Option<Option<Ipv4Addr>>,
    dns: Option<Vec<Ipv4Addr>>,
    lease_secs: Option<u32>,
}

/// 区分"字段没出现"和"字段显式为 null" —— 前者是不改，后者是清空。
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

async fn patch_scope(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Json(req): Json<ScopePatchReq>,
) -> Response {
    // 地址池要么两端都给，要么都不给
    let pool = match (req.pool_start, req.pool_end) {
        (Some(a), Some(b)) => Some((a, b)),
        (None, None) => None,
        _ => {
            return bad_request("地址池要同时给起止两端");
        }
    };

    let patch = crate::state::ScopePatch {
        name: req.name,
        enabled: req.enabled,
        pool,
        router: req.router,
        dns: req.dns,
        lease_secs: req.lease_secs,
    };
    match state.patch_scope(ScopeId(id), patch).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(problems) => bad_request(&problems.join("；")),
    }
}

async fn delete_scope(State(state): State<AppState>, Path(id): Path<u32>) -> Response {
    match state.remove_scope(ScopeId(id)).await {
        Ok(dropped) => Json(serde_json::json!({ "droppedLeases": dropped })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(ApiError { error: e })).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewReservation {
    /// MAC，或 `opt61:` 前缀的原始客户端标识
    client: String,
    ip: Ipv4Addr,
    hostname: Option<String>,
}

/// 解析界面传来的客户端标识。裸 MAC 是常态，option 61 形式留给
/// systemd-networkd 那类发 DUID 的客户端。
fn parse_client(s: &str) -> Option<lessor_core::ClientId> {
    if let Some(hex) = s.strip_prefix("opt61:") {
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect();
        return bytes
            .filter(|b| !b.is_empty())
            .map(lessor_core::ClientId::Opt61);
    }
    s.parse().ok().map(lessor_core::ClientId::Mac)
}

async fn post_reservation(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Json(req): Json<NewReservation>,
) -> Response {
    let Some(client) = parse_client(&req.client) else {
        return bad_request("客户端标识要写成 MAC（ac:1f:6b:…）或 opt61:<十六进制>");
    };
    let r = lessor_core::scope::Reservation {
        client,
        ip: req.ip,
        hostname: req.hostname.filter(|h| !h.is_empty()),
    };
    match state.add_reservation(ScopeId(id), r).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(problems) => bad_request(&problems.join("；")),
    }
}

async fn delete_reservation(
    State(state): State<AppState>,
    Path((id, client)): Path<(u32, String)>,
) -> Response {
    let Some(client) = parse_client(&client) else {
        return bad_request("客户端标识不合法");
    };
    match state.remove_reservation(ScopeId(id), &client).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "没有这条保留".into(),
            }),
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(ApiError { error: e })).into_response(),
    }
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: msg.to_owned(),
        }),
    )
        .into_response()
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
    /// 写操作是否需要令牌。界面据此决定要不要提示输入。
    auth_required: bool,
}

async fn get_state(State(st): State<AppState>) -> Json<StateResponse> {
    Json(StateResponse {
        version: env!("CARGO_PKG_VERSION"),
        started_at: st.started_at,
        uptime_secs: now().saturating_sub(st.started_at),
        scopes: st.scope_status().await,
        listeners: st.listeners().await,
        auth_required: st.auth_enabled(),
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
            "\n\n界面尚未构建。在 ui/ 目录执行 `bun install && bun run build`，\n",
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
