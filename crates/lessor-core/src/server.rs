//! 报文决策 —— 收到一个 DHCP 请求，决定回什么。
//!
//! 这里是整个服务端的大脑，实现 RFC 2131 §4.3。函数是纯的：
//! 输入报文 + 当前租约 + 时间，输出应答（或"不回"），副作用只有对存储的修改。
//! 没有 socket、没有时钟、没有日志 IO —— 因此每条规则都能单独测试。

use std::net::Ipv4Addr;

use dhcproto::v4::{DhcpOption, Message, MessageType, Opcode, OptionCode};

use crate::addr::{ClientId, MacAddr};
use crate::lease::{Lease, LeaseState, UnixTime};
use crate::scope::{Scope, ScopeId};
use crate::store::{AllocSource, LeaseStore, allocate};

/// 应答该发到哪里。RFC 2131 §4.1 规定了这套优先级。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReplyDest {
    /// 经中继而来 —— 单播回中继的 67 端口
    Relay(Ipv4Addr),
    /// 客户端已有可用地址（续租）—— 单播到它的 68 端口
    Unicast(Ipv4Addr),
    /// 客户端还没有地址，且置了广播标志 —— 广播
    Broadcast,
    /// 客户端还没有地址但没置广播标志。严格按 RFC 应当单播到 yiaddr，
    /// 这需要手工写 ARP 表项；上层若做不到，退回广播也能工作。
    UnicastYiaddr(Ipv4Addr),
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub msg: Message,
    pub dest: ReplyDest,
    /// 由哪个作用域产生 —— 上层记日志和统计要用。
    pub scope_id: ScopeId,
    /// 地址是怎么选出来的。NAK 和 INFORM 没有分配动作，故为 None。
    pub alloc_source: Option<AllocSource>,
}

/// 为什么没有应答。界面上"这台机器插上了却没反应"时，这是第一手线索。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropReason {
    /// opcode 不是 BootRequest —— 可能是别的服务器的应答被我们收到了
    NotBootRequest,
    /// 缺 option 53，不是合法的 DHCP 报文
    NoMessageType,
    /// 服务端不处理的报文类型（OFFER / ACK / NAK 等）
    UnsupportedType,
    /// chaddr 全零且没有 option 61 —— 无法标识这个客户端
    UnidentifiableClient,
    /// 收包的网段（或中继地址）没有匹配的作用域
    NoMatchingScope,
    /// 匹配到的作用域被禁用了
    ScopeDisabled,
    /// 地址池耗尽。按 RFC 应当沉默，让客户端去问别的服务器
    PoolExhausted,
    /// 客户端在多个 OFFER 里选了别人
    ChoseAnotherServer,
    /// DECLINE 缺少 option 50，不知道该隔离哪个地址
    DeclineWithoutAddress,
    /// RELEASE 缺少 ciaddr
    ReleaseWithoutAddress,
    /// RELEASE 的地址不属于该客户端 —— 可能是伪造报文
    ReleaseNotOwned,
}

/// 一次处理的结果。
#[derive(Clone, Debug)]
pub enum Outcome {
    /// 要发出的应答
    Reply(Reply),
    /// 按协议处理了，但不需要回应（DECLINE / RELEASE）
    Handled(&'static str),
    /// 没有处理
    Drop(DropReason),
}

impl Outcome {
    pub fn reply(&self) -> Option<&Reply> {
        match self {
            Self::Reply(r) => Some(r),
            _ => None,
        }
    }

    pub fn drop_reason(&self) -> Option<DropReason> {
        match self {
            Self::Drop(r) => Some(*r),
            _ => None,
        }
    }
}

/// 收包上下文。时间和收包网卡的地址都由调用方注入。
#[derive(Clone, Copy, Debug)]
pub struct RecvCtx {
    pub now: UnixTime,
    /// 收到该报文的那块网卡上的本机地址。
    /// 既用来选作用域（非中继场景），也用作 option 54（server identifier）。
    pub server_ip: Ipv4Addr,
}

#[derive(Clone, Debug, Default)]
pub struct ServerConfig {
    pub scopes: Vec<Scope>,
}

impl ServerConfig {
    pub fn new(scopes: Vec<Scope>) -> Self {
        Self { scopes }
    }

    pub fn scope(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.iter().find(|s| s.id == id)
    }

    /// 选出该请求归属的作用域。
    ///
    /// 经中继来的报文用 `giaddr` 判断 —— 那是客户端所在网段的网关地址；
    /// 直连的用收包网卡的本机地址。
    pub fn select_scope(&self, req: &Message, ctx: &RecvCtx) -> Result<&Scope, DropReason> {
        let key = if req.giaddr().is_unspecified() {
            ctx.server_ip
        } else {
            req.giaddr()
        };
        let matched: Vec<&Scope> = self.scopes.iter().filter(|s| s.contains(key)).collect();
        match matched.iter().find(|s| s.enabled) {
            Some(s) => Ok(s),
            None if matched.is_empty() => Err(DropReason::NoMatchingScope),
            None => Err(DropReason::ScopeDisabled),
        }
    }
}

/// 取客户端标识：有 option 61 就用它，否则回退到 chaddr（RFC 2131 §4.2）。
pub fn client_id_of(msg: &Message) -> Option<ClientId> {
    let opt61 = match msg.opts().get(OptionCode::ClientIdentifier) {
        Some(DhcpOption::ClientIdentifier(raw)) if !raw.is_empty() => Some(raw.as_slice()),
        _ => None,
    };
    let mac = MacAddr::from_slice(msg.chaddr()).unwrap_or(MacAddr::ZERO);
    // 既没有 option 61 又没有有效 MAC，无法索引租约
    if opt61.is_none() && mac.is_zero() {
        return None;
    }
    Some(ClientId::from_parts(opt61, mac))
}

fn requested_ip(msg: &Message) -> Option<Ipv4Addr> {
    match msg.opts().get(OptionCode::RequestedIpAddress) {
        Some(DhcpOption::RequestedIpAddress(ip)) => Some(*ip),
        _ => None,
    }
}

fn server_ident(msg: &Message) -> Option<Ipv4Addr> {
    match msg.opts().get(OptionCode::ServerIdentifier) {
        Some(DhcpOption::ServerIdentifier(ip)) => Some(*ip),
        _ => None,
    }
}

fn hostname(msg: &Message) -> Option<String> {
    match msg.opts().get(OptionCode::Hostname) {
        Some(DhcpOption::Hostname(h)) if !h.is_empty() => Some(h.clone()),
        _ => None,
    }
}

/// option 60。PXE 客户端会填 `PXEClient:Arch:00007:...`，
/// 据此可以区分固件阶段和操作系统阶段，也便于在界面上认出设备类型。
fn vendor_class(msg: &Message) -> Option<String> {
    match msg.opts().get(OptionCode::ClassIdentifier) {
        Some(DhcpOption::ClassIdentifier(raw)) if !raw.is_empty() => {
            Some(String::from_utf8_lossy(raw).into_owned())
        }
        _ => None,
    }
}

/// 客户端请求的租期，受作用域上限约束。
fn requested_lease_secs(msg: &Message, scope: &Scope) -> u32 {
    match msg.opts().get(OptionCode::AddressLeaseTime) {
        Some(DhcpOption::AddressLeaseTime(secs)) => (*secs).min(scope.lease_secs),
        _ => scope.lease_secs,
    }
}

fn dest_for(req: &Message, yiaddr: Ipv4Addr) -> ReplyDest {
    if !req.giaddr().is_unspecified() {
        ReplyDest::Relay(req.giaddr())
    } else if !req.ciaddr().is_unspecified() {
        ReplyDest::Unicast(req.ciaddr())
    } else if req.flags().broadcast() {
        ReplyDest::Broadcast
    } else {
        ReplyDest::UnicastYiaddr(yiaddr)
    }
}

/// 搭好应答的公共骨架 —— 头部字段照抄请求，opcode 换成 BootReply。
fn base_reply(req: &Message, kind: MessageType, server_id: Ipv4Addr) -> Message {
    let mut m = Message::default();
    m.set_opcode(Opcode::BootReply)
        .set_htype(req.htype())
        .set_xid(req.xid())
        .set_flags(req.flags())
        .set_giaddr(req.giaddr())
        .set_chaddr(req.chaddr());
    m.opts_mut().insert(DhcpOption::MessageType(kind));
    m.opts_mut().insert(DhcpOption::ServerIdentifier(server_id));
    m
}

/// 按作用域配置填入网络参数。
fn apply_scope_options(msg: &mut Message, scope: &Scope, lease_secs: u32) {
    let opts = msg.opts_mut();
    opts.insert(DhcpOption::AddressLeaseTime(lease_secs));
    // T1/T2 —— 不发的话客户端会自己按 0.5/0.875 推算，显式给出更可控
    opts.insert(DhcpOption::Renewal(lease_secs / 2));
    opts.insert(DhcpOption::Rebinding(lease_secs / 8 * 7));
    opts.insert(DhcpOption::SubnetMask(scope.netmask()));

    if let Some(gw) = scope.router {
        opts.insert(DhcpOption::Router(vec![gw]));
    }
    if !scope.dns.is_empty() {
        opts.insert(DhcpOption::DomainNameServer(scope.dns.clone()));
    }
    if let Some(d) = &scope.domain {
        opts.insert(DhcpOption::DomainName(d.clone()));
    }

    if let Some(boot) = &scope.boot {
        if let Some(f) = &boot.filename {
            msg.opts_mut()
                .insert(DhcpOption::BootfileName(f.clone().into_bytes()));
        }
        if let Some(sn) = &boot.server_name {
            msg.opts_mut()
                .insert(DhcpOption::TFTPServerName(sn.clone().into_bytes()));
        }
        if let Some(ns) = boot.next_server {
            msg.set_siaddr(ns);
        }
    }

    // 作用域里配置的原始选项，覆盖上面的默认值
    for (code, value) in &scope.extra_options {
        msg.opts_mut()
            .insert(DhcpOption::Unknown(dhcproto::v4::UnknownOption::new(
                OptionCode::Unknown(*code),
                value.clone(),
            )));
    }
}

/// 处理一个请求。会就地更新租约存储。
pub fn handle<S: LeaseStore + ?Sized>(
    cfg: &ServerConfig,
    store: &mut S,
    req: &Message,
    ctx: RecvCtx,
) -> Outcome {
    if req.opcode() != Opcode::BootRequest {
        return Outcome::Drop(DropReason::NotBootRequest);
    }
    let Some(kind) = req.opts().msg_type() else {
        return Outcome::Drop(DropReason::NoMessageType);
    };
    let Some(client) = client_id_of(req) else {
        return Outcome::Drop(DropReason::UnidentifiableClient);
    };
    let scope = match cfg.select_scope(req, &ctx) {
        Ok(s) => s,
        Err(why) => return Outcome::Drop(why),
    };

    match kind {
        MessageType::Discover => on_discover(scope, store, req, &client, ctx),
        MessageType::Request => on_request(scope, store, req, &client, ctx),
        MessageType::Decline => on_decline(scope, store, req, &client, ctx),
        MessageType::Release => on_release(scope, store, req, &client),
        MessageType::Inform => on_inform(scope, req, ctx),
        _ => Outcome::Drop(DropReason::UnsupportedType),
    }
}

fn on_discover<S: LeaseStore + ?Sized>(
    scope: &Scope,
    store: &mut S,
    req: &Message,
    client: &ClientId,
    ctx: RecvCtx,
) -> Outcome {
    let Some(alloc) = allocate(scope, store, client, requested_ip(req), ctx.now) else {
        return Outcome::Drop(DropReason::PoolExhausted);
    };

    let lease_secs = requested_lease_secs(req, scope);

    // 用短超时占位，防止并发的 DISCOVER 拿到同一个地址。
    // 客户端不跟进 REQUEST 的话，很快就会被回收。
    store.insert(Lease {
        ip: alloc.ip,
        client: client.clone(),
        scope_id: scope.id,
        state: LeaseState::Offered,
        expires_at: ctx.now + u64::from(scope.offer_secs),
        hostname: hostname(req),
        vendor_class: vendor_class(req),
        created_at: ctx.now,
        last_seen: ctx.now,
    });

    let mut msg = base_reply(req, MessageType::Offer, ctx.server_ip);
    msg.set_yiaddr(alloc.ip);
    apply_scope_options(&mut msg, scope, lease_secs);

    Outcome::Reply(Reply {
        msg,
        dest: dest_for(req, alloc.ip),
        scope_id: scope.id,
        alloc_source: Some(alloc.source),
    })
}

fn nak(scope_id: ScopeId, req: &Message, server_ip: Ipv4Addr, why: &str) -> Outcome {
    let mut msg = base_reply(req, MessageType::Nak, server_ip);
    msg.set_yiaddr(Ipv4Addr::UNSPECIFIED);
    msg.opts_mut().insert(DhcpOption::Message(why.to_string()));
    // NAK 必须广播 —— 客户端此刻的地址是错的，单播到不了
    let dest = if req.giaddr().is_unspecified() {
        ReplyDest::Broadcast
    } else {
        ReplyDest::Relay(req.giaddr())
    };
    Outcome::Reply(Reply {
        msg,
        dest,
        scope_id,
        alloc_source: None,
    })
}

fn on_request<S: LeaseStore + ?Sized>(
    scope: &Scope,
    store: &mut S,
    req: &Message,
    client: &ClientId,
    ctx: RecvCtx,
) -> Outcome {
    // SELECTING：客户端在多个 OFFER 里选了一个。不是选我们就闭嘴，
    // 同时把我们之前的 OFFER 占位释放掉。
    if let Some(chosen) = server_ident(req)
        && chosen != ctx.server_ip
    {
        if let Some(l) = store.get_by_client(scope.id, client)
            && l.state == LeaseState::Offered
        {
            let ip = l.ip;
            store.remove(scope.id, ip);
        }
        return Outcome::Drop(DropReason::ChoseAnotherServer);
    }

    // 客户端认为自己该拿的地址：
    // SELECTING / INIT-REBOOT 放在 option 50，RENEWING / REBINDING 放在 ciaddr。
    let claimed = requested_ip(req).or_else(|| {
        (!req.ciaddr().is_unspecified()).then(|| req.ciaddr())
    });
    let Some(claimed) = claimed else {
        return nak(
            scope.id,
            req,
            ctx.server_ip,
            "REQUEST 里既没有 option 50 也没有 ciaddr",
        );
    };

    if !scope.contains(claimed) {
        // 客户端换网段了（比如笔记本从别的办公室回来），必须 NAK 让它重来
        return nak(scope.id, req, ctx.server_ip, "请求的地址不属于本子网");
    }

    let Some(alloc) = allocate(scope, store, client, Some(claimed), ctx.now) else {
        return nak(scope.id, req, ctx.server_ip, "无可用地址");
    };
    if alloc.ip != claimed {
        // 我们能给的和它要的不是同一个 —— 地址被别人占了，或者管理员改了保留
        return nak(scope.id, req, ctx.server_ip, "该地址已不属于此客户端");
    }

    let lease_secs = requested_lease_secs(req, scope);
    let created_at = store
        .get_by_client(scope.id, client)
        .map_or(ctx.now, |l| l.created_at);

    store.insert(Lease {
        ip: alloc.ip,
        client: client.clone(),
        scope_id: scope.id,
        state: LeaseState::Bound,
        expires_at: ctx.now + u64::from(lease_secs),
        hostname: hostname(req),
        vendor_class: vendor_class(req),
        created_at,
        last_seen: ctx.now,
    });

    let mut msg = base_reply(req, MessageType::Ack, ctx.server_ip);
    msg.set_yiaddr(alloc.ip).set_ciaddr(req.ciaddr());
    apply_scope_options(&mut msg, scope, lease_secs);

    Outcome::Reply(Reply {
        msg,
        dest: dest_for(req, alloc.ip),
        scope_id: scope.id,
        alloc_source: Some(alloc.source),
    })
}

fn on_decline<S: LeaseStore + ?Sized>(
    scope: &Scope,
    store: &mut S,
    req: &Message,
    client: &ClientId,
    ctx: RecvCtx,
) -> Outcome {
    // 客户端 ARP 探测发现地址已被占用。把它隔离一段时间，别再发出去。
    let Some(bad) = requested_ip(req) else {
        return Outcome::Drop(DropReason::DeclineWithoutAddress);
    };
    store.insert(Lease {
        ip: bad,
        client: client.clone(),
        scope_id: scope.id,
        state: LeaseState::Declined,
        expires_at: ctx.now + u64::from(scope.decline_quarantine_secs),
        hostname: None,
        vendor_class: None,
        created_at: ctx.now,
        last_seen: ctx.now,
    });
    Outcome::Handled("地址被客户端 DECLINE，已隔离")
}

fn on_release<S: LeaseStore + ?Sized>(
    scope: &Scope,
    store: &mut S,
    req: &Message,
    client: &ClientId,
) -> Outcome {
    // RELEASE 用 ciaddr 指明要归还的地址
    let ip = req.ciaddr();
    if ip.is_unspecified() {
        return Outcome::Drop(DropReason::ReleaseWithoutAddress);
    }
    // 只允许归还自己的租约，避免被伪造报文清掉别人的
    match store.get_by_ip(scope.id, ip) {
        Some(l) if &l.client == client => {
            store.remove(scope.id, ip);
            Outcome::Handled("租约已释放")
        }
        _ => Outcome::Drop(DropReason::ReleaseNotOwned),
    }
}

fn on_inform(scope: &Scope, req: &Message, ctx: RecvCtx) -> Outcome {
    // 客户端自己有地址，只想要网络参数。回 ACK 但不分配、不建租约。
    let mut msg = base_reply(req, MessageType::Ack, ctx.server_ip);
    msg.set_yiaddr(Ipv4Addr::UNSPECIFIED)
        .set_ciaddr(req.ciaddr());
    apply_scope_options(&mut msg, scope, scope.lease_secs);
    // INFORM 的应答不能带租期
    msg.opts_mut().remove(OptionCode::AddressLeaseTime);
    msg.opts_mut().remove(OptionCode::Renewal);
    msg.opts_mut().remove(OptionCode::Rebinding);

    let dest = if req.ciaddr().is_unspecified() {
        ReplyDest::Broadcast
    } else {
        ReplyDest::Unicast(req.ciaddr())
    };
    Outcome::Reply(Reply {
        msg,
        dest,
        scope_id: scope.id,
        alloc_source: None,
    })
}
