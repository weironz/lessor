//! HTTP / WebSocket 接口。
//!
//! 界面是纯客户端，不带任何特权 —— 网络相关的事全在这个进程里做完，
//! 前端只通过这套接口读状态、下命令。Web 端和 Tauri 桌面端加载的是同一份 UI。

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, patch, post};
use lessor_core::{Lease, ScopeId};
use lessor_net::Ipv4Cidr;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use crate::config::{DiscoveryMode, DiscoverySettings, Listener};
use crate::state::{AppState, DiscoveryTargetError, ScopeStatus, now};
use crate::ui::Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/api/state", get(get_state))
        .route("/api/leases", get(get_leases))
        .route("/api/leases/{scope_id}/{ip}", delete(revoke_lease))
        .route("/api/interfaces", get(get_interfaces))
        .route("/api/settings", patch(patch_settings))
        .route("/api/discover", post(post_discover))
        .route("/api/v1/devices", get(get_devices))
        .route("/api/v1/scopes/{id}/discover", post(post_scope_discover))
        .route("/api/v1/scopes/{id}/devices", get(get_scope_devices))
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

/// Prometheus 抓取端点。和管理接口同一个监听地址 ——
/// 默认只听 127.0.0.1，要被 Prometheus 抓就得显式 `--http 0.0.0.0:8080`。
async fn metrics(State(state): State<AppState>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics().await,
    )
        .into_response()
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
/// 直连作用域的子网由 `serverIp` 和 `prefix` 推出；经中继的作用域则必须
/// 显式给 `subnet`，因为该网段没有本机网卡可供推导。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewScope {
    name: Option<String>,
    /// 本机用于接收中继单播、并作为 option 54 下发的监听地址。
    server_ip: Ipv4Addr,
    /// 仅 `viaRelay` 时使用的远端网段地址。
    subnet: Option<Ipv4Addr>,
    prefix: u8,
    pool_start: Ipv4Addr,
    pool_end: Ipv4Addr,
    router: Option<Ipv4Addr>,
    #[serde(default)]
    dns: Vec<Ipv4Addr>,
    lease_secs: Option<u32>,
    #[serde(default)]
    via_relay: bool,
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
    let subnet = Ipv4Addr::from(
        u32::from(if req.via_relay {
            let Some(subnet) = req.subnet else {
                return bad_request("经 DHCP 中继的作用域需要填写远端网段");
            };
            subnet
        } else {
            req.server_ip
        }) & mask,
    );

    let mut scope = lessor_core::Scope::new(
        0,
        req.name.unwrap_or_else(|| "新建".into()),
        subnet,
        req.prefix,
    );
    scope.pools = vec![pool];
    scope.router = req.router;
    scope.dns = req.dns;
    scope.via_relay = req.via_relay;
    if let Some(secs) = req.lease_secs {
        scope.lease_secs = secs;
        scope.offer_secs = 30.min(secs);
    }

    match state.add_scope(scope, req.server_ip).await {
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
    discovery: DiscoverySettings,
    /// 写操作是否需要令牌。界面据此决定要不要提示输入。
    auth_required: bool,
    /// "监听中但一个请求都没收到"时的提示；正常收包时为 `None`。
    ///
    /// 只写日志不够 —— 现场装机的人盯着的是界面，不是终端。
    quiet_note: Option<String>,
    /// 上面那条成立时该按什么顺序排查。界面直接展开给人看。
    quiet_hint: Option<&'static str>,
}

async fn get_state(State(st): State<AppState>) -> Json<StateResponse> {
    let quiet_note = crate::health::quiet_note(&st);
    Json(StateResponse {
        version: env!("CARGO_PKG_VERSION"),
        started_at: st.started_at,
        uptime_secs: now().saturating_sub(st.started_at),
        scopes: st.scope_status().await,
        listeners: st.listeners().await,
        discovery: st.discovery_settings().await,
        auth_required: st.auth_enabled(),
        quiet_hint: quiet_note.is_some().then(crate::health::quiet_hint),
        quiet_note,
    })
}

async fn get_leases(State(st): State<AppState>) -> Json<Vec<Lease>> {
    Json(st.leases().await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatchRequest {
    discovery_mode: Option<DiscoveryMode>,
    discovery_interval_secs: Option<u64>,
}

async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<SettingsPatchRequest>,
) -> Response {
    let mut settings = state.discovery_settings().await;
    if let Some(mode) = patch.discovery_mode {
        settings.mode = mode;
    }
    if let Some(interval_secs) = patch.discovery_interval_secs {
        settings.interval_secs = interval_secs;
    }
    match state.set_discovery_settings(settings.clone()).await {
        Ok(()) => Json(serde_json::json!({ "discovery": settings })).into_response(),
        Err(error) => bad_request(&error),
    }
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

// ---------- 面向自动化的作用域设备 API ----------

/// v1 设备类型是服务端的语义判断，不让 Ansible 自己从 MAC、厂商字符串或
/// 扫描来源猜测。当前 `bmc` 仅在 RMCP/IPMI 明确确认后给出，宁可漏报也不
/// 把普通邻居表项目误当成 BMC。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DeviceKind {
    Bmc,
    Device,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum DeviceConfidence {
    Confirmed,
    Low,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeDiscoverRequest {
    /// 是否逐个探测整个网段；默认开启，超过 discovery 的安全上限时会跳过。
    #[serde(default = "yes")]
    sweep: bool,
    #[serde(default = "default_wait_ms")]
    wait_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevicesQuery {
    kind: Option<DeviceKind>,
    scope_id: Option<ScopeId>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceRecord {
    ip: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none")]
    mac: Option<lessor_core::MacAddr>,
    kind: DeviceKind,
    confidence: DeviceConfidence,
    sources: Vec<discovery::Method>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    first_seen: u64,
    last_seen: u64,
    /// 如果设备地址是本服务在该作用域下发的，则把 DHCP 租约一并关联。
    #[serde(skip_serializing_if = "Option::is_none")]
    lease: Option<Lease>,
    /// True only for the address currently bound by this scope's DHCP service. Discovery history
    /// intentionally survives a rescan, so consumers must not mistake an old IP for the device's
    /// current DHCP address.
    active_lease: bool,
    /// The one address callers should use for this MAC: an active lease wins; otherwise the
    /// newest confirmed discovery survives daemon restarts where a client has not renewed yet.
    current_address: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeDevices {
    scope: ScopeReference,
    scanned_at: u64,
    devices: Vec<DeviceRecord>,
}

/// 发现记录的稳定归属是 scopeId；其他字段从当前作用域实时投影，改名或
/// 调整显示信息不会让缓存保留过期副本。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeReference {
    id: ScopeId,
    name: String,
    subnet: Ipv4Addr,
    prefix: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_ip: Option<Ipv4Addr>,
}

#[derive(Serialize)]
struct Data<T> {
    data: T,
}

async fn device_collection(
    state: &AppState,
    scope_id: ScopeId,
    kind: Option<DeviceKind>,
) -> Option<ScopeDevices> {
    let scope = state
        .scope_status()
        .await
        .into_iter()
        .find(|scope| scope.id == scope_id)?;
    let (discovery, leases) = state.discovered_devices(scope_id).await?;
    let current = now();
    let mut newest_by_identity: HashMap<String, (bool, u64, Ipv4Addr)> = HashMap::new();
    for cached in &discovery.devices {
        let active_lease = leases
            .iter()
            .any(|lease| lease.ip == cached.device.ip && lease.is_active(current));
        let identity = cached
            .device
            .mac
            .map(|mac| mac.to_string())
            .unwrap_or_else(|| format!("ip:{}", cached.device.ip));
        let replace = newest_by_identity
            .get(&identity)
            .is_none_or(|(was_active, last_seen, _)| {
                (active_lease && !was_active)
                    || (active_lease == *was_active && cached.last_seen > *last_seen)
            });
        if replace {
            newest_by_identity.insert(identity, (active_lease, cached.last_seen, cached.device.ip));
        }
    }
    let current_addresses: HashSet<Ipv4Addr> = newest_by_identity
        .into_values()
        .map(|(_, _, ip)| ip)
        .collect();
    let devices = discovery
        .devices
        .into_iter()
        .map(|cached| {
            let device_kind = if cached.device.is_bmc {
                DeviceKind::Bmc
            } else {
                DeviceKind::Device
            };
            (cached, device_kind)
        })
        .filter(|(_, device_kind)| kind.is_none_or(|wanted| wanted == *device_kind))
        .map(|(cached, device_kind)| {
            let lease = leases
                .iter()
                .find(|lease| lease.ip == cached.device.ip && lease.is_active(current))
                .cloned();
            DeviceRecord {
                ip: cached.device.ip,
                mac: cached.device.mac,
                kind: device_kind,
                confidence: if cached.device.is_bmc {
                    DeviceConfidence::Confirmed
                } else {
                    DeviceConfidence::Low
                },
                sources: cached.device.via,
                note: cached.device.note,
                first_seen: cached.first_seen,
                last_seen: cached.last_seen,
                active_lease: lease.is_some(),
                current_address: current_addresses.contains(&cached.device.ip),
                lease,
            }
        })
        .collect();
    Some(ScopeDevices {
        scope: ScopeReference {
            id: scope.id,
            name: scope.name,
            subnet: scope.subnet,
            prefix: scope.prefix,
            server_ip: scope.server_ip,
        },
        scanned_at: discovery.scanned_at,
        devices,
    })
}

/// 聚合作用域设备缓存。GET 永远不发包；没有缓存的作用域不会出现在结果中，
/// 调用方可结合 /api/state 区分“尚未扫描”和“扫描为空”。
async fn get_devices(State(state): State<AppState>, Query(query): Query<DevicesQuery>) -> Response {
    let scope_ids: Vec<ScopeId> = match query.scope_id {
        Some(id) => vec![id],
        None => state
            .scope_status()
            .await
            .into_iter()
            .map(|scope| scope.id)
            .collect(),
    };
    let mut collections = Vec::new();
    for scope_id in scope_ids {
        if let Some(devices) = device_collection(&state, scope_id, query.kind).await {
            collections.push(devices);
        }
    }
    Json(Data { data: collections }).into_response()
}

async fn get_scope_devices(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Query(query): Query<DevicesQuery>,
) -> Response {
    let scope_id = ScopeId(id);
    match device_collection(&state, scope_id, query.kind).await {
        Some(devices) => Json(Data { data: devices }).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "这个作用域尚未执行设备发现".into(),
            }),
        )
            .into_response(),
    }
}

async fn post_scope_discover(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Json(req): Json<ScopeDiscoverRequest>,
) -> Response {
    if state.discovery_settings().await.mode == DiscoveryMode::Disabled {
        return (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "设备发现已在设置中禁用".into(),
            }),
        )
            .into_response();
    }
    let scope_id = ScopeId(id);
    let target = match state.discovery_target(scope_id).await {
        Ok(target) => target,
        Err(DiscoveryTargetError::NotFound(_)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: format!("没有 {scope_id} 这个作用域"),
                }),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::CONFLICT,
                Json(ApiError {
                    error: error.to_string(),
                }),
            )
                .into_response();
        }
    };

    let mut options = discovery::Options::new(target);
    options.sweep = req.sweep;
    options.wait = Duration::from_millis(req.wait_ms.clamp(200, 10_000));
    let devices = discovery::scan(options).await;
    state.record_discovery(scope_id, devices).await;

    match device_collection(&state, scope_id, None).await {
        Some(devices) => Json(Data { data: devices }).into_response(),
        // 作用域可能在扫描期间被删除；不要把旧扫描结果泄露给已经不存在的作用域。
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("作用域 {scope_id} 在发现期间已删除"),
            }),
        )
            .into_response(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_scope() -> AppState {
        let scope = lessor_core::Scope::new(1, "BMC direct", Ipv4Addr::new(192, 168, 1, 0), 24);
        AppState::new(crate::config::Config {
            listeners: Vec::new(),
            scopes: vec![scope],
            discovery: crate::config::DiscoverySettings::default(),
        })
    }

    #[tokio::test]
    async fn bmc_filter_returns_only_rmcp_confirmed_devices() {
        let state = state_with_scope();
        let scope = ScopeId(1);
        state
            .record_discovery(
                scope,
                vec![
                    discovery::Device {
                        ip: Ipv4Addr::new(192, 168, 1, 101),
                        mac: None,
                        via: vec![discovery::Method::Rmcp],
                        is_bmc: true,
                        note: Some("回应 IPMI/RMCP".into()),
                    },
                    discovery::Device {
                        ip: Ipv4Addr::new(192, 168, 1, 102),
                        mac: None,
                        via: vec![discovery::Method::Probed],
                        is_bmc: false,
                        note: None,
                    },
                ],
            )
            .await;

        let result = device_collection(&state, scope, Some(DeviceKind::Bmc))
            .await
            .expect("已缓存的发现结果应可读取");
        assert_eq!(result.devices.len(), 1);
        assert_eq!(result.devices[0].ip, Ipv4Addr::new(192, 168, 1, 101));
        assert_eq!(result.devices[0].kind, DeviceKind::Bmc);
        assert!(matches!(
            result.devices[0].confidence,
            DeviceConfidence::Confirmed
        ));
    }
}
