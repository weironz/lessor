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
    /// 客户端自报的 option 60。现场一屏 MAC 谁也认不出哪台是哪台，
    /// 而这个字段常常直接写着厂商和阶段 —— 尤其是分不清"这台是固件在
    /// 要地址还是系统装好了在要地址"的时候。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_class: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    /// 处理了一个报文
    Packet(PacketEvent),
    /// 租约表发生变化，前端应重新拉取
    LeasesChanged,
    /// 作用域发生变化，前端应重新拉取
    ScopesChanged,
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

/// 租约存储的两种形态。
///
/// 现场默认内存（关了就走，不留痕）；常驻用 sqlite。用枚举而不是
/// `Box<dyn LeaseStore>` 是因为分派点只有一个、后端数量有限，
/// 枚举更直白，也避免了 trait object 对未来 async 后端的约束。
pub enum Store {
    Memory(MemoryStore),
    Sqlite(crate::sqlite::SqliteStore),
}

/// 存储 + 冲突探测结果。
///
/// `LeaseStore` 的 `is_occupied_elsewhere` 需要探测数据，但探测不属于
/// 存储的职责 —— 用这层包装把两者拼起来，core 因此不必知道 discovery
/// 的存在。
pub struct StoreWithProbe {
    pub store: Store,
    pub occupied: crate::conflict::Occupied,
}

impl LeaseStore for StoreWithProbe {
    fn get_by_ip(&self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        self.store.get_by_ip(scope, ip)
    }
    fn get_by_client(&self, scope: ScopeId, client: &lessor_core::ClientId) -> Option<Lease> {
        self.store.get_by_client(scope, client)
    }
    fn insert(&mut self, lease: Lease) {
        self.store.insert(lease);
    }
    fn try_claim(&mut self, lease: Lease, now: UnixTime) -> bool {
        self.store.try_claim(lease, now)
    }
    fn remove(&mut self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        self.store.remove(scope, ip)
    }
    fn all(&self) -> Vec<Lease> {
        self.store.all()
    }
    fn reap(&mut self, now: UnixTime) -> usize {
        self.store.reap(now)
    }
    fn clear_scope(&mut self, scope: ScopeId) -> usize {
        self.store.clear_scope(scope)
    }
    fn usable_by(
        &self,
        scope: ScopeId,
        ip: Ipv4Addr,
        client: &lessor_core::ClientId,
        now: UnixTime,
    ) -> bool {
        self.store.usable_by(scope, ip, client, now)
    }
    fn is_occupied_elsewhere(&self, ip: Ipv4Addr) -> bool {
        self.occupied.is_taken(ip)
    }
}

impl LeaseStore for Store {
    fn get_by_ip(&self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        match self {
            Self::Memory(s) => s.get_by_ip(scope, ip),
            Self::Sqlite(s) => s.get_by_ip(scope, ip),
        }
    }
    fn get_by_client(&self, scope: ScopeId, client: &lessor_core::ClientId) -> Option<Lease> {
        match self {
            Self::Memory(s) => s.get_by_client(scope, client),
            Self::Sqlite(s) => s.get_by_client(scope, client),
        }
    }
    fn insert(&mut self, lease: Lease) {
        match self {
            Self::Memory(s) => s.insert(lease),
            Self::Sqlite(s) => s.insert(lease),
        }
    }
    fn try_claim(&mut self, lease: Lease, now: UnixTime) -> bool {
        match self {
            Self::Memory(s) => s.try_claim(lease, now),
            Self::Sqlite(s) => s.try_claim(lease, now),
        }
    }
    fn remove(&mut self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        match self {
            Self::Memory(s) => s.remove(scope, ip),
            Self::Sqlite(s) => s.remove(scope, ip),
        }
    }
    fn all(&self) -> Vec<Lease> {
        match self {
            Self::Memory(s) => s.all(),
            Self::Sqlite(s) => s.all(),
        }
    }
    fn reap(&mut self, now: UnixTime) -> usize {
        match self {
            Self::Memory(s) => s.reap(now),
            Self::Sqlite(s) => s.reap(now),
        }
    }
    fn clear_scope(&mut self, scope: ScopeId) -> usize {
        match self {
            Self::Memory(s) => s.clear_scope(scope),
            Self::Sqlite(s) => s.clear_scope(scope),
        }
    }
    fn usable_by(
        &self,
        scope: ScopeId,
        ip: Ipv4Addr,
        client: &lessor_core::ClientId,
        now: UnixTime,
    ) -> bool {
        match self {
            Self::Memory(s) => s.usable_by(scope, ip, client, now),
            Self::Sqlite(s) => s.usable_by(scope, ip, client, now),
        }
    }
}

impl Store {
    fn used_in(&self, scope: ScopeId, now: UnixTime) -> u64 {
        match self {
            Self::Memory(s) => s.used_in(scope, now),
            Self::Sqlite(s) => s.used_in(scope, now),
        }
    }
}

struct Inner {
    server: ServerConfig,
    listeners: Vec<Listener>,
    store: StoreWithProbe,
}

/// 运行计数。常驻部署要能回答"它到底在干活吗" ——
/// 日志会滚掉，这些数字不会。
#[derive(Debug, Default)]
pub struct Counters {
    pub packets: std::sync::atomic::AtomicU64,
    pub offers: std::sync::atomic::AtomicU64,
    pub acks: std::sync::atomic::AtomicU64,
    pub naks: std::sync::atomic::AtomicU64,
    pub drops: std::sync::atomic::AtomicU64,
    /// 最后一次收到报文的时刻（Unix 秒）。0 表示一个都没收到过。
    ///
    /// "监听中但一个请求都没有"是现场最常见的故障形态，而它和"网段上
    /// 暂时没有客户端"从服务端看长得一模一样。这个时间戳是唯一能把两者
    /// 分开的依据，也是空闲自动退出的判据。
    pub last_packet_at: std::sync::atomic::AtomicU64,
}

impl Counters {
    fn bump(c: &std::sync::atomic::AtomicU64) {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn get(c: &std::sync::atomic::AtomicU64) -> u64 {
        c.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<RwLock<Inner>>,
    events: broadcast::Sender<Event>,
    pub started_at: UnixTime,
    /// 管理接口令牌。`None` 表示未启用鉴权 —— 只在默认的
    /// 仅监听 127.0.0.1 场景下可接受。
    token: Option<Arc<str>>,
    /// 运行时新增监听器的请求出口。界面上建完作用域后，得真的有人在
    /// 那块网卡上收包，否则作用域建了也是摆设。
    new_listener: Option<tokio::sync::mpsc::UnboundedSender<Listener>>,
    /// 配置文件路径。给了的话，界面上的每次改动都会写回去 ——
    /// 常驻服务重启后配置还在，否则界面改的东西活不过一次重启。
    config_path: Option<Arc<std::path::Path>>,
    pub counters: Arc<Counters>,
    /// 已知被静态占用的地址。后台探测填充，分配路径只读 ——
    /// 查缓存是纳秒级的，不会拖慢握手。
    pub occupied: crate::conflict::Occupied,
    /// 开了 --capture 时，收到的每个包都原样存一份。给真机 BMC 取证用。
    capture: Option<Arc<crate::capture::Capture>>,
    /// 只看不答。挂在生产网段上取证时必须开 —— 那儿已经有别人在发地址了。
    observe: bool,
}

/// 作用域可改的部分。`None` 表示不动这一项。
#[derive(Debug, Default)]
pub struct ScopePatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    pub router: Option<Option<Ipv4Addr>>,
    pub dns: Option<Vec<Ipv4Addr>>,
    pub lease_secs: Option<u32>,
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
    /// 本机在该网段上的地址。经中继服务的网段这里是 `None` ——
    /// 那不是配漏了，见下面的 `via_relay`。
    pub server_ip: Option<Ipv4Addr>,
    /// 这个网段是经 DHCP 中继服务的。界面据此把"没有本机地址"标成
    /// 有意为之，而不是显示成一个说不清的空值。
    pub via_relay: bool,
}

impl AppState {
    /// 设置管理接口令牌。给了就强制校验写操作。
    #[must_use]
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.map(Into::into);
        self
    }

    /// 写操作是否放行。没配令牌时一律放行（默认只听环回口）。
    pub fn authorize(&self, presented: Option<&str>) -> bool {
        match &self.token {
            None => true,
            // 比较长度相同的字节，避免因提前返回泄露长度信息
            Some(t) => presented.is_some_and(|p| {
                p.len() == t.len()
                    && p.bytes()
                        .zip(t.bytes())
                        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                        == 0
            }),
        }
    }

    pub fn auth_enabled(&self) -> bool {
        self.token.is_some()
    }

    /// 只看不答。
    #[must_use]
    pub fn with_observe(mut self, observe: bool) -> Self {
        self.observe = observe;
        self
    }

    /// 是不是只看不答。
    pub fn observing(&self) -> bool {
        self.observe
    }

    /// 开启报文捕获。
    #[must_use]
    pub fn with_capture(mut self, cap: Option<crate::capture::Capture>) -> Self {
        self.capture = cap.map(Arc::new);
        self
    }

    /// 正在捕获的话给出句柄。
    pub fn capture(&self) -> Option<&crate::capture::Capture> {
        self.capture.as_deref()
    }

    /// 换用 sqlite 存储。常驻形态走这条 —— 重启不丢租约。
    #[must_use]
    pub fn with_store(self, store: Store) -> Self {
        // 构造期独占，不会有别的持有者
        if let Ok(mut g) = self.inner.try_write() {
            g.store.store = store;
        }
        self
    }

    pub fn new(cfg: Config) -> Self {
        // 容量给足，慢速的 WebSocket 客户端掉几条事件也不该拖住 DHCP 主循环
        let (events, _) = broadcast::channel(512);
        let occupied = crate::conflict::Occupied::default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                server: ServerConfig::new(cfg.scopes),
                listeners: cfg.listeners,
                store: StoreWithProbe {
                    store: Store::Memory(MemoryStore::new()),
                    occupied: occupied.clone(),
                },
            })),
            events,
            started_at: now(),
            token: None,
            new_listener: None,
            config_path: None,
            counters: Arc::new(Counters::default()),
            occupied,
            capture: None,
            observe: false,
        }
    }

    /// 让配置改动写回这个文件。常驻部署应当给上。
    #[must_use]
    pub fn with_config_path(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.config_path = path.map(Into::into);
        self
    }

    /// 把当前配置写回文件。
    ///
    /// 先写临时文件再原子改名 —— 直接覆盖的话，写到一半掉电会留下
    /// 半个 JSON，下次启动直接起不来。
    async fn persist(&self) {
        let Some(path) = &self.config_path else {
            return;
        };
        let g = self.inner.read().await;
        let cfg = Config {
            listeners: g.listeners.clone(),
            scopes: g.server.scopes.clone(),
        };
        drop(g);

        let Ok(text) = serde_json::to_string_pretty(&cfg) else {
            tracing::error!("配置无法序列化，改动没有写回文件");
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, text.as_bytes())
            .and_then(|()| std::fs::rename(&tmp, path.as_ref()))
        {
            tracing::error!(path = %path.display(), error = %e,
                "配置写回失败 —— 界面上的改动重启后会丢");
        }
    }

    /// 注册运行时新增监听器的出口。`main` 持有接收端并把它们 spawn 起来。
    #[must_use]
    pub fn with_listener_spawner(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<Listener>,
    ) -> Self {
        self.new_listener = Some(tx);
        self
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

        Counters::bump(&self.counters.packets);
        self.counters
            .last_packet_at
            .store(at, std::sync::atomic::Ordering::Relaxed);
        match &outcome {
            Outcome::Reply(r) => match reply_label(&r.msg) {
                "OFFER" => Counters::bump(&self.counters.offers),
                "ACK" => Counters::bump(&self.counters.acks),
                "NAK" => Counters::bump(&self.counters.naks),
                _ => {}
            },
            Outcome::Drop(_) => Counters::bump(&self.counters.drops),
            Outcome::Handled(_) => {}
        }

        let request = request_label(req);
        let client = client_label(req);
        let vendor_class = lessor_core::server::vendor_class(req);

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
                    vendor_class,
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
                vendor_class,
            },
            Outcome::Drop(r) => PacketEvent {
                at,
                client,
                request,
                result: "DROP",
                scope_id: None,
                ip: None,
                // 池满时多半是因为一批地址被静态占用挡住了 —— 把占用者
                // 说出来，否则界面上只有"地址池已耗尽"，没法排查
                detail: Some(match r {
                    DropReason::PoolExhausted => {
                        let blocked = self.occupied.blocked_summary();
                        if blocked.is_empty() {
                            drop_reason_text(*r).to_owned()
                        } else {
                            format!("{}（{blocked}）", drop_reason_text(*r))
                        }
                    }
                    _ => drop_reason_text(*r).to_owned(),
                }),
                // 没应答的时候最需要知道是什么设备 —— 那正是要排查的那一台
                vendor_class,
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
        g.store.all()
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
                used: g.store.store.used_in(s.id, t),
                reservations: s.reservations.len(),
                server_ip: g
                    .listeners
                    .iter()
                    .find(|l| s.contains(l.server_ip))
                    .map(|l| l.server_ip),
                via_relay: s.via_relay,
            })
            .collect()
    }

    /// Prometheus 文本格式的指标。
    ///
    /// 不引 prometheus crate：就这几个计数器，手写比多一个依赖便宜，
    /// 格式也简单到不会写错。
    pub async fn metrics(&self) -> String {
        use std::fmt::Write;
        let c = &self.counters;
        let scopes = self.scope_status().await;
        let mut out = String::new();

        let counters = [
            (
                "lessor_packets_total",
                "收到并处理的 DHCP 报文数",
                Counters::get(&c.packets),
            ),
            (
                "lessor_offers_total",
                "发出的 OFFER 数",
                Counters::get(&c.offers),
            ),
            ("lessor_acks_total", "发出的 ACK 数", Counters::get(&c.acks)),
            ("lessor_naks_total", "发出的 NAK 数", Counters::get(&c.naks)),
            (
                "lessor_drops_total",
                "未应答的报文数",
                Counters::get(&c.drops),
            ),
        ];
        for (name, help, v) in counters {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} counter");
            let _ = writeln!(out, "{name} {v}");
        }

        let _ = writeln!(out, "# HELP lessor_uptime_seconds 进程已运行时长");
        let _ = writeln!(out, "# TYPE lessor_uptime_seconds gauge");
        let _ = writeln!(
            out,
            "lessor_uptime_seconds {}",
            now().saturating_sub(self.started_at)
        );

        // 按作用域的容量与占用 —— 告警最常用的就是"池要满了"
        let _ = writeln!(out, "# HELP lessor_scope_capacity 作用域可分配地址总数");
        let _ = writeln!(out, "# TYPE lessor_scope_capacity gauge");
        for s in &scopes {
            let _ = writeln!(
                out,
                "lessor_scope_capacity{{scope=\"{}\"}} {}",
                s.name.replace('"', "'"),
                s.capacity
            );
        }
        let _ = writeln!(out, "# HELP lessor_scope_used 作用域已占用地址数");
        let _ = writeln!(out, "# TYPE lessor_scope_used gauge");
        for s in &scopes {
            let _ = writeln!(
                out,
                "lessor_scope_used{{scope=\"{}\"}} {}",
                s.name.replace('"', "'"),
                s.used
            );
        }
        out
    }

    /// 作用域与监听器的快照 —— 后台探测要用。
    pub async fn scopes_and_listeners(&self) -> (Vec<lessor_core::Scope>, Vec<Listener>) {
        let g = self.inner.read().await;
        (g.server.scopes.clone(), g.listeners.clone())
    }

    /// 某个作用域里已经发出去的地址。探测只扫还没分出去的。
    pub async fn leased_ips(&self, scope: ScopeId) -> std::collections::HashSet<Ipv4Addr> {
        let g = self.inner.read().await;
        g.store
            .all()
            .into_iter()
            .filter(|l| l.scope_id == scope)
            .map(|l| l.ip)
            .collect()
    }

    pub async fn listeners(&self) -> Vec<Listener> {
        self.inner.read().await.listeners.clone()
    }

    /// 运行时新建作用域。
    ///
    /// 会先跑 `Scope::validate` 与跨对象检查（必须有落在该网段的监听器、
    /// 不能与已有作用域抢同一个监听器地址）—— 界面上填错不该把服务搞坏。
    pub async fn add_scope(&self, mut scope: lessor_core::Scope) -> Result<ScopeId, Vec<String>> {
        let mut g = self.inner.write().await;

        let mut problems: Vec<String> = scope.validate().iter().map(ToString::to_string).collect();

        // 该网段还没有监听器时自动补一个：本机在这个网段上的地址就是
        // 它的 server_ip。找不到才是真的错 —— 那说明这台机器根本不在
        // 用户填的网段里，建了作用域也收不到包。
        let mut listener_to_start = None;
        if !g.listeners.iter().any(|l| scope.contains(l.server_ip)) {
            match lessor_net::interfaces().ok().and_then(|ifs| {
                ifs.into_iter()
                    .filter(lessor_net::Interface::is_servable)
                    .find_map(|i| {
                        let cidr = i.primary_ipv4()?;
                        scope.contains(cidr.addr).then_some((i.name, cidr.addr))
                    })
            }) {
                Some((iface, server_ip)) => {
                    listener_to_start = Some(Listener {
                        server_ip,
                        // 只有 Linux 用得上网卡名（SO_BINDTODEVICE）
                        iface: cfg!(target_os = "linux").then_some(iface),
                    });
                }
                None => problems.push(format!(
                    "本机没有落在 {}/{} 里的地址 —— 建了作用域也收不到这个网段的请求",
                    scope.subnet, scope.prefix
                )),
            }
        }
        if let Some(conflict) = g.server.scopes.iter().find(|s| {
            g.listeners
                .iter()
                .any(|l| s.contains(l.server_ip) && scope.contains(l.server_ip))
        }) {
            problems.push(format!(
                "与已有作用域「{}」抢同一个监听器地址",
                conflict.name
            ));
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        // id 由服务端分配，前端不该猜
        let id = ScopeId(g.server.scopes.iter().map(|s| s.id.0).max().unwrap_or(0) + 1);
        scope.id = id;
        g.server.scopes.push(scope);
        if let Some(l) = listener_to_start.clone() {
            g.listeners.push(l);
        }
        drop(g);

        // 真正把收包任务起起来。发不出去（main 已退出）时作用域仍然建了，
        // 但收不到包 —— 这种情况只在关停途中出现。
        if let (Some(tx), Some(l)) = (&self.new_listener, listener_to_start) {
            let _ = tx.send(l);
        }

        self.persist().await;
        self.emit(Event::ScopesChanged);
        Ok(id)
    }

    /// 改一个作用域的可变部分。
    ///
    /// 网段和监听器不给改 —— 那等于换一个作用域，让人删了重建更清楚，
    /// 也免得改到一半和别的作用域抢监听器。
    pub async fn patch_scope(&self, id: ScopeId, patch: ScopePatch) -> Result<(), Vec<String>> {
        let mut g = self.inner.write().await;
        let Some(scope) = g.server.scopes.iter_mut().find(|s| s.id == id) else {
            return Err(vec![format!("没有 {id} 这个作用域")]);
        };

        // 改在副本上验证，通过了才落回去 —— 免得校验失败时留下半改的状态
        let mut next = scope.clone();
        if let Some(name) = patch.name {
            next.name = name;
        }
        if let Some(enabled) = patch.enabled {
            next.enabled = enabled;
        }
        if let Some((start, end)) = patch.pool {
            let Some(range) = lessor_core::Range::new(start, end) else {
                return Err(vec!["地址池起止顺序不对".into()]);
            };
            next.pools = vec![range];
        }
        if let Some(router) = patch.router {
            next.router = router;
        }
        if let Some(dns) = patch.dns {
            next.dns = dns;
        }
        if let Some(secs) = patch.lease_secs {
            next.lease_secs = secs;
            next.offer_secs = next.offer_secs.min(secs);
        }

        let problems: Vec<String> = next.validate().iter().map(ToString::to_string).collect();
        if !problems.is_empty() {
            return Err(problems);
        }
        *scope = next;
        drop(g);

        self.persist().await;
        self.emit(Event::ScopesChanged);
        Ok(())
    }

    /// 删一个作用域，连同它的租约。
    ///
    /// 监听器留着不动：它只是"在这块网卡上收包"，没有作用域时收到的请求
    /// 会以 NoMatchingScope 落地，界面上看得见。贸然关掉反而会让同网段
    /// 新建的作用域收不到包。
    pub async fn remove_scope(&self, id: ScopeId) -> Result<usize, String> {
        let mut g = self.inner.write().await;
        let before = g.server.scopes.len();
        g.server.scopes.retain(|s| s.id != id);
        if g.server.scopes.len() == before {
            return Err(format!("没有 {id} 这个作用域"));
        }
        let dropped = g.store.clear_scope(id);
        drop(g);

        self.persist().await;
        self.emit(Event::ScopesChanged);
        if dropped > 0 {
            self.emit(Event::LeasesChanged);
        }
        Ok(dropped)
    }

    /// 加一条静态保留。现场把 BMC 钉死到规划地址就靠它。
    pub async fn add_reservation(
        &self,
        id: ScopeId,
        r: lessor_core::scope::Reservation,
    ) -> Result<(), Vec<String>> {
        let mut g = self.inner.write().await;
        let Some(scope) = g.server.scopes.iter_mut().find(|s| s.id == id) else {
            return Err(vec![format!("没有 {id} 这个作用域")]);
        };

        let mut next = scope.clone();
        // 同一个客户端只保留一条，重复配等于改
        next.reservations.retain(|x| x.client != r.client);
        next.reservations.push(r.clone());

        let problems: Vec<String> = next.validate().iter().map(ToString::to_string).collect();
        if !problems.is_empty() {
            return Err(problems);
        }
        *scope = next;

        // 那个地址上如果压着别人的动态租约，先撤掉 —— 否则保留形同虚设，
        // 客户端会一直被那条旧租约挡着拿不到规划地址
        let evicted = g
            .store
            .get_by_ip(id, r.ip)
            .is_some_and(|l| l.client != r.client)
            .then(|| g.store.remove(id, r.ip))
            .is_some();
        drop(g);

        self.persist().await;
        self.emit(Event::ScopesChanged);
        if evicted {
            self.emit(Event::LeasesChanged);
        }
        Ok(())
    }

    /// 删一条静态保留。已经发出去的租约不动 —— 到期自然回收。
    pub async fn remove_reservation(
        &self,
        id: ScopeId,
        client: &lessor_core::ClientId,
    ) -> Result<bool, String> {
        let mut g = self.inner.write().await;
        let Some(scope) = g.server.scopes.iter_mut().find(|s| s.id == id) else {
            return Err(format!("没有 {id} 这个作用域"));
        };
        let before = scope.reservations.len();
        scope.reservations.retain(|x| &x.client != client);
        let removed = scope.reservations.len() != before;
        drop(g);

        if removed {
            self.persist().await;
            self.emit(Event::ScopesChanged);
        }
        Ok(removed)
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
