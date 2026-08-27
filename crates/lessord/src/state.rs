//! 共享状态与事件流。

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lessor_core::{
    AllocSource, DropReason, Lease, LeaseStore, MemoryStore, Outcome, ScopeId, ServerConfig,
    UnixTime,
};
use serde::Serialize;
use tokio::sync::{RwLock, broadcast};

use crate::config::{Config, Listener};

pub fn now() -> UnixTime {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 一次报文处理的结果。界面上的实时日志就是这个流。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PacketEvent {
    pub at: UnixTime,
    /// 客户端标识的可读形式
    pub client: String,
    /// 收到的报文类型：DISCOVER / REQUEST / …
    pub request: String,
    /// 我们做了什么：OFFER / ACK / NAK / DROP / HANDLED
    pub result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<ScopeId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<Ipv4Addr>,
    /// 丢弃原因，或地址是怎么选出来的
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    /// 处理了一个报文
    Packet(PacketEvent),
    /// 租约表发生变化，前端应重新拉取
    LeasesChanged,
    /// 定期清理回收了若干条过期租约
    Reaped { count: usize },
}

/// 报文类型的可读名，如 DISCOVER。
pub fn request_label(req: &dhcproto::v4::Message) -> String {
    req.opts()
        .msg_type()
        .map_or_else(|| "?".to_owned(), |t| format!("{t:?}").to_uppercase())
}

/// 应答类型的可读名，如 OFFER / ACK / NAK。
pub fn reply_label(msg: &dhcproto::v4::Message) -> &'static str {
    match msg.opts().msg_type() {
        Some(dhcproto::v4::MessageType::Offer) => "OFFER",
        Some(dhcproto::v4::MessageType::Ack) => "ACK",
        Some(dhcproto::v4::MessageType::Nak) => "NAK",
        _ => "REPLY",
    }
}

/// NAK 里带的拒绝原因（option 56）。被拒绝时这是最该展示的信息 ——
/// 光看到 NAK 不知道为什么，等于没有线索。
pub fn reject_reason(msg: &dhcproto::v4::Message) -> Option<String> {
    match msg.opts().get(dhcproto::v4::OptionCode::Message) {
        Some(dhcproto::v4::DhcpOption::Message(m)) if !m.is_empty() => Some(m.clone()),
        _ => None,
    }
}

/// 客户端标识的可读形式。
pub fn client_label(req: &dhcproto::v4::Message) -> String {
    lessor_core::server::client_id_of(req).map_or_else(|| "?".to_owned(), |c| c.to_string())
}

pub fn drop_reason_text(r: DropReason) -> &'static str {
    match r {
        DropReason::NotBootRequest => "不是 BootRequest",
        DropReason::NoMessageType => "缺少 option 53",
        DropReason::UnsupportedType => "服务端不处理该报文类型",
        DropReason::UnidentifiableClient => "既无有效 MAC 也无 option 61",
        DropReason::NoMatchingScope => "该网段没有配置作用域",
        DropReason::ScopeDisabled => "作用域已禁用",
        DropReason::PoolExhausted => "地址池已耗尽",
        DropReason::ChoseAnotherServer => "客户端选了别的服务器",
        DropReason::DeclineWithoutAddress => "DECLINE 缺少 option 50",
        DropReason::ReleaseWithoutAddress => "RELEASE 缺少 ciaddr",
        DropReason::ReleaseNotOwned => "RELEASE 的地址不属于该客户端",
    }
}

pub fn alloc_source_text(s: AllocSource) -> &'static str {
    match s {
        AllocSource::Existing => "沿用原地址",
        AllocSource::Reservation => "静态保留",
        AllocSource::Requested => "满足客户端请求",
        AllocSource::Pool => "池分配",
    }
}

struct Inner {
    server: ServerConfig,
    listeners: Vec<Listener>,
    store: MemoryStore,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<RwLock<Inner>>,
    events: broadcast::Sender<Event>,
    pub started_at: UnixTime,
}

/// 作用域的运行时快照，给界面用。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeStatus {
    pub id: ScopeId,
    pub name: String,
    pub enabled: bool,
    pub subnet: Ipv4Addr,
    pub prefix: u8,
    pub capacity: u64,
    pub used: u64,
    pub reservations: usize,
    /// 本机在该网段上的地址
    pub server_ip: Option<Ipv4Addr>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        // 容量给足，慢速的 WebSocket 客户端掉几条事件也不该拖住 DHCP 主循环
        let (events, _) = broadcast::channel(512);
        Self {
            inner: Arc::new(RwLock::new(Inner {
                server: ServerConfig::new(cfg.scopes),
                listeners: cfg.listeners,
                store: MemoryStore::new(),
            })),
            events,
            started_at: now(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// 发事件。没有订阅者时会返回错误，那是正常情况，忽略即可。
    pub fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    /// 处理一个 DHCP 报文，返回决策结果，并把事件推给订阅者。
    pub async fn handle_packet(&self, req: &dhcproto::v4::Message, server_ip: Ipv4Addr) -> Outcome {
        let at = now();
        let mut g = self.inner.write().await;
        let ctx = lessor_core::RecvCtx { now: at, server_ip };
        // 拆开借用：配置只读、存储可写，两者是 Inner 的不同字段
        let Inner { server, store, .. } = &mut *g;
        let outcome = lessor_core::handle(server, store, req, ctx);
        drop(g);

        let request = request_label(req);
        let client = client_label(req);

        let ev = match &outcome {
            Outcome::Reply(r) => {
                let kind = reply_label(&r.msg);
                PacketEvent {
                    at,
                    client,
                    request,
                    result: kind,
                    scope_id: Some(r.scope_id),
                    ip: (!r.msg.yiaddr().is_unspecified()).then(|| r.msg.yiaddr()),
                    // NAK 用拒绝原因，其余用地址是怎么选出来的
                    detail: reject_reason(&r.msg)
                        .or_else(|| r.alloc_source.map(|s| alloc_source_text(s).to_owned())),
                }
            }
            Outcome::Handled(note) => PacketEvent {
                at,
                client,
                request,
                result: "HANDLED",
                scope_id: None,
                ip: None,
                detail: Some((*note).to_owned()),
            },
            Outcome::Drop(r) => PacketEvent {
                at,
                client,
                request,
                result: "DROP",
                scope_id: None,
                ip: None,
                detail: Some(drop_reason_text(*r).to_owned()),
            },
        };

        let changed = !matches!(outcome, Outcome::Drop(_));
        self.emit(Event::Packet(ev));
        if changed {
            self.emit(Event::LeasesChanged);
        }
        outcome
    }

    pub async fn leases(&self) -> Vec<Lease> {
        let g = self.inner.read().await;
        g.store.all().into_iter().cloned().collect()
    }

    pub async fn scope_status(&self) -> Vec<ScopeStatus> {
        let g = self.inner.read().await;
        let t = now();
        g.server
            .scopes
            .iter()
            .map(|s| ScopeStatus {
                id: s.id,
                name: s.name.clone(),
                enabled: s.enabled,
                subnet: s.subnet,
                prefix: s.prefix,
                capacity: s.capacity(),
                used: g.store.used_in(s.id, t),
                reservations: s.reservations.len(),
                server_ip: g
                    .listeners
                    .iter()
                    .find(|l| s.contains(l.server_ip))
                    .map(|l| l.server_ip),
            })
            .collect()
    }

    pub async fn listeners(&self) -> Vec<Listener> {
        self.inner.read().await.listeners.clone()
    }

    /// 手工撤销一条租约。返回是否真的删掉了。
    pub async fn revoke(&self, scope: ScopeId, ip: Ipv4Addr) -> bool {
        let removed = self.inner.write().await.store.remove(scope, ip).is_some();
        if removed {
            self.emit(Event::LeasesChanged);
        }
        removed
    }

    /// 清掉过期租约。返回清理条数。
    pub async fn reap(&self) -> usize {
        let n = self.inner.write().await.store.reap(now());
        if n > 0 {
            self.emit(Event::Reaped { count: n });
            self.emit(Event::LeasesChanged);
        }
        n
    }
}
