//! 报文决策 —— 收到一个 DHCP 请求，决定回什么。
//!
//! 这里是整个服务端的大脑，实现 RFC 2131 §4.3。函数是纯的：
//! 输入报文 + 当前租约 + 时间，输出应答（或"不回"），副作用只有对存储的修改。
//! 没有 socket、没有时钟、没有日志 IO —— 因此每条规则都能单独测试。

use std::net::Ipv4Addr;

use dhcproto::v4::{DhcpOption, Message, MessageType, Opcode, OptionCode};

use crate::addr::{ClientId, MacAddr};
use crate::lease::{Lease, LeaseState, UnixTime};
use crate::scope::{BootClient, Scope, ScopeId};
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
    /// 按三个线索依次判断客户端在哪个网段：
    ///
    /// 1. **`giaddr`** —— 经中继来的报文，那是客户端所在网段的网关地址。
    /// 2. **`ciaddr`** —— 客户端已经持有的地址。**续租时只有这一条线索**：
    ///    RENEW 是客户端直接单播给服务器的，不经过中继，所以没有 `giaddr`。
    ///    漏了这一层的话，被中继网段的续租会退到第 3 条、选中收包监听器
    ///    所在的那个**别的**网段，然后因为"请求的地址不属于本子网"被 NAK ——
    ///    一个还在正常工作的客户端会被迫丢掉租约重来。
    /// 3. **收包监听器的本机地址** —— 直连的常规情况。
    pub fn select_scope(&self, req: &Message, ctx: &RecvCtx) -> Result<&Scope, DropReason> {
        let key = if !req.giaddr().is_unspecified() {
            req.giaddr()
        } else if !req.ciaddr().is_unspecified() {
            req.ciaddr()
        } else {
            ctx.server_ip
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

/// BOOTP `file` 字段的容量。超过就只能走 option 67。
const BOOTP_FILE_LEN: usize = 128;

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

/// option 77（user class）里有没有 `iPXE`。
///
/// RFC 3004 规定 option 77 是"长度前缀串"的列表，但 **iPXE 直接发裸字符串**。
/// 现实里两种都会遇到（有的中继或代理会替它重新封装），所以两种都认。
fn user_class_has_ipxe(raw: &[u8]) -> bool {
    const IPXE: &[u8] = b"iPXE";

    if raw.eq_ignore_ascii_case(IPXE) {
        return true;
    }

    let mut i = 0;
    while i < raw.len() {
        let n = raw[i] as usize;
        // 长度为 0 或越界，说明这不是长度前缀格式，别再往下猜
        if n == 0 || i + 1 + n > raw.len() {
            return false;
        }
        if raw[i + 1..i + 1 + n].eq_ignore_ascii_case(IPXE) {
            return true;
        }
        i += 1 + n;
    }
    false
}

/// 客户端自报的引导方式。
///
/// 判定顺序很重要，`BootClient::Ipxe` 的文档里写了为什么。
pub fn boot_client_of(req: &Message) -> BootClient {
    if let Some(DhcpOption::UserClass(raw)) = req.opts().get(OptionCode::UserClass)
        && user_class_has_ipxe(raw)
    {
        return BootClient::Ipxe;
    }

    match vendor_class(req) {
        Some(v) if v.starts_with("PXEClient") => BootClient::Pxe,
        // UEFI 规范里 HTTP Boot 客户端自报 `HTTPClient`，后面同样跟
        // `:Arch:xxxxx:UNDI:yyyzzz`
        Some(v) if v.starts_with("HTTPClient") => BootClient::HttpBoot,
        _ => BootClient::Plain,
    }
}

/// 按作用域配置填入网络参数。
fn apply_scope_options(msg: &mut Message, req: &Message, scope: &Scope, lease_secs: u32) {
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
        let client = boot_client_of(req);

        if let Some(f) = boot.file_for(client) {
            msg.opts_mut()
                .insert(DhcpOption::BootfileName(f.as_bytes().to_vec()));

            // 同时写进 BOOTP 的 file 字段 —— 部分固件只读这里，不看 option 67。
            //
            // 但这个字段只有 128 字节，而 HTTP Boot 和 iPXE 的 URL 经常更长，
            // dhcproto 的 set_fname_str 超长会直接 panic（打崩整个收发循环）。
            // 放不下就只发 option 67：认这个字段的都是老式 TFTP 固件，
            // 它们的文件名本来就短，装不下的场景也用不着它。
            if f.len() <= BOOTP_FILE_LEN {
                msg.set_fname_str(f);
            }
        }
        if let Some(sn) = &boot.server_name {
            msg.opts_mut()
                .insert(DhcpOption::TFTPServerName(sn.clone().into_bytes()));
        }
        if let Some(ns) = boot.next_server {
            msg.set_siaddr(ns);
        }

        // UEFI 规范要求 HTTP Boot 的应答里带 option 60 = "HTTPClient"，
        // 固件靠它确认这个 URL 是给自己的；不回就不会去取。
        // 只在真的给了它 URL 时才声明 —— 空口声明和 PXEClient 那边一样有害。
        if client == BootClient::HttpBoot && boot.file_for(client).is_some() {
            msg.opts_mut()
                .insert(DhcpOption::ClassIdentifier(b"HTTPClient".to_vec()));
        }

        // option 60 = "PXEClient" 只在同时给了 PXE 厂商选项（option 43）时才回。
        //
        // 应答里出现 "PXEClient" 是在声明"我提供 PXE 引导服务"，固件据此
        // 转去 option 43 里找引导服务器列表 / 菜单。只声明却不给 43，固件
        // 会认为这是个残缺的 PXE 服务：它接受地址，却不去 TFTP 拉引导文件。
        //
        // 拿 VMware UEFI 固件实测的三组对照：
        //
        // | option 60 | option 43 | 结果                    |
        // |-----------|-----------|-------------------------|
        // | 无        | 无        | 正常引导到 GRUB         |
        // | 有        | 有        | 正常引导                |
        // | 有        | 无        | 拿到 ACK 后什么都不做   |
        //
        // 不做 PXE 菜单时，siaddr + BOOTP file 字段就够固件直接去拉引导
        // 文件了。isc-dhcp 和 dnsmasq 默认也是这个行为 —— MAAS 生成的
        // dhcpd.conf 里，UEFI（arch 00:07）那一支同样只给 filename。
        //
        // 另：别把"固件完全不接受 OFFER、一直重发 DISCOVER"算到这一项头上，
        // 那个症状的原因是应答的**源端口**不是 67，见 lessord 的 `socket_for`。
        if client == BootClient::Pxe && scope.has_pxe_vendor_options() {
            msg.opts_mut()
                .insert(DhcpOption::ClassIdentifier(b"PXEClient".to_vec()));
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
    let lease_secs = requested_lease_secs(req, scope);

    // 挑地址的同时就占下来（短超时占位），防止并发的 DISCOVER 拿到同一个。
    // 客户端不跟进 REQUEST 的话，很快就会被回收。
    let hostname = hostname(req);
    let vendor_class = vendor_class(req);
    let Some(alloc) = allocate(scope, store, client, requested_ip(req), ctx.now, |ip| {
        Lease {
            ip,
            client: client.clone(),
            scope_id: scope.id,
            state: LeaseState::Offered,
            expires_at: ctx.now + u64::from(scope.offer_secs),
            hostname: hostname.clone(),
            vendor_class: vendor_class.clone(),
            created_at: ctx.now,
            last_seen: ctx.now,
        }
    }) else {
        return Outcome::Drop(DropReason::PoolExhausted);
    };

    let mut msg = base_reply(req, MessageType::Offer, ctx.server_ip);
    msg.set_yiaddr(alloc.ip);
    apply_scope_options(&mut msg, req, scope, lease_secs);

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
    let claimed =
        requested_ip(req).or_else(|| (!req.ciaddr().is_unspecified()).then(|| req.ciaddr()));
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

    let lease_secs = requested_lease_secs(req, scope);
    // 续租要保留最初的分配时间，界面上才看得出这台机器跟了多久
    let created_at = store
        .get_by_client(scope.id, client)
        .map_or(ctx.now, |l| l.created_at);

    let hostname = hostname(req);
    let vendor_class = vendor_class(req);
    let Some(alloc) = allocate(scope, store, client, Some(claimed), ctx.now, |ip| Lease {
        ip,
        client: client.clone(),
        scope_id: scope.id,
        state: LeaseState::Bound,
        expires_at: ctx.now + u64::from(lease_secs),
        hostname: hostname.clone(),
        vendor_class: vendor_class.clone(),
        created_at,
        last_seen: ctx.now,
    }) else {
        return nak(scope.id, req, ctx.server_ip, "无可用地址");
    };
    if alloc.ip != claimed {
        // 我们能给的和它要的不是同一个 —— 地址被别人占了，或者管理员改了保留
        return nak(scope.id, req, ctx.server_ip, "该地址已不属于此客户端");
    }

    let mut msg = base_reply(req, MessageType::Ack, ctx.server_ip);
    msg.set_yiaddr(alloc.ip).set_ciaddr(req.ciaddr());
    apply_scope_options(&mut msg, req, scope, lease_secs);

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
    apply_scope_options(&mut msg, req, scope, scope.lease_secs);
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
