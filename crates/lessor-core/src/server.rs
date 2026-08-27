//! 报文决策 —— 收到一个 DHCP 请求，决定回什么。
//!
//! 这里是整个服务端的大脑，实现 RFC 2131 §4.3。函数是纯的：
//! 输入报文 + 当前租约表 + 时间，输出应答（或"不回"），副作用只有对租约表的修改。
//! 没有 socket、没有时钟、没有日志 IO —— 因此每条规则都能单独测试。

use std::net::Ipv4Addr;

use dhcproto::v4::{DhcpOption, Message, MessageType, OptionCode, Opcode};

use crate::addr::{ClientId, MacAddr};
use crate::lease::{Lease, LeaseState, UnixTime};
use crate::scope::Scope;
use crate::table::{AllocSource, LeaseTable, allocate};

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
}

/// 一次处理的结果，便于上层记录发生了什么。
#[derive(Clone, Debug)]
pub struct Outcome {
    pub reply: Option<Reply>,
    pub note: &'static str,
}

impl Outcome {
    fn silent(note: &'static str) -> Self {
        Self { reply: None, note }
    }

    fn with(reply: Reply, note: &'static str) -> Self {
        Self {
            reply: Some(reply),
            note,
        }
    }
}

pub struct ServerConfig {
    /// 本服务端在该网卡上的地址，用作 option 54（server identifier）
    pub server_id: Ipv4Addr,
    pub scope: Scope,
}

/// 取客户端标识：有 option 61 就用它，否则回退到 chaddr（RFC 2131 §4.2）。
pub fn client_id_of(msg: &Message) -> ClientId {
    if let Some(DhcpOption::ClientIdentifier(raw)) = msg.opts().get(OptionCode::ClientIdentifier)
        && !raw.is_empty()
    {
        return ClientId::Opt61(raw.clone());
    }
    match MacAddr::from_slice(msg.chaddr()) {
        Some(mac) => ClientId::Mac(mac),
        // chaddr 不足 6 字节（非以太网），只能拿原始字节当标识
        None => ClientId::Opt61(msg.chaddr().to_vec()),
    }
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
    m.opts_mut()
        .insert(DhcpOption::ServerIdentifier(server_id));
    m
}

/// 按作用域配置填入网络参数。只填客户端问了的（PRL），外加几个必发的。
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
        if let Some(f) = &boot.filename {
            opts.insert(DhcpOption::BootfileName(f.clone().into_bytes()));
        }
        if let Some(sn) = &boot.server_name {
            opts.insert(DhcpOption::TFTPServerName(sn.clone().into_bytes()));
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

    let _ = req; // PRL 过滤留待后续按需实现，先全发
}

/// 处理一个请求。会就地更新租约表。
pub fn handle(
    cfg: &ServerConfig,
    table: &mut LeaseTable,
    req: &Message,
    now: UnixTime,
) -> Outcome {
    if req.opcode() != Opcode::BootRequest {
        return Outcome::silent("不是 BootRequest，忽略");
    }
    let Some(kind) = req.opts().msg_type() else {
        return Outcome::silent("没有 option 53，不是合法的 DHCP 报文");
    };

    match kind {
        MessageType::Discover => on_discover(cfg, table, req, now),
        MessageType::Request => on_request(cfg, table, req, now),
        MessageType::Decline => on_decline(cfg, table, req, now),
        MessageType::Release => on_release(cfg, table, req, now),
        MessageType::Inform => on_inform(cfg, req),
        _ => Outcome::silent("服务端不处理的报文类型"),
    }
}

fn on_discover(
    cfg: &ServerConfig,
    table: &mut LeaseTable,
    req: &Message,
    now: UnixTime,
) -> Outcome {
    let client = client_id_of(req);
    let Some(alloc) = allocate(&cfg.scope, table, &client, requested_ip(req), now) else {
        // 池子满了。不回 NAK —— DISCOVER 阶段沉默是 RFC 的要求，
        // 让客户端去问别的服务器。
        return Outcome::silent("地址池已耗尽");
    };

    let lease_secs = requested_lease_secs(req, &cfg.scope);

    // 用短超时占位，防止并发的 DISCOVER 拿到同一个地址。
    // 客户端不跟进 REQUEST 的话，很快就会被回收。
    table.insert(Lease {
        ip: alloc.ip,
        client,
        state: LeaseState::Offered,
        expires_at: now + u64::from(cfg.scope.offer_secs),
        hostname: hostname(req),
        created_at: now,
    });

    let mut msg = base_reply(req, MessageType::Offer, cfg.server_id);
    msg.set_yiaddr(alloc.ip);
    apply_scope_options(&mut msg, req, &cfg.scope, lease_secs);

    Outcome::with(
        Reply {
            msg,
            dest: dest_for(req, alloc.ip),
        },
        match alloc.source {
            AllocSource::Reservation => "OFFER（静态保留）",
            AllocSource::Existing => "OFFER（沿用原地址）",
            AllocSource::Requested => "OFFER（满足请求地址）",
            AllocSource::Pool => "OFFER（池分配）",
        },
    )
}

fn nak(cfg: &ServerConfig, req: &Message, why: &'static str) -> Outcome {
    let mut msg = base_reply(req, MessageType::Nak, cfg.server_id);
    msg.set_yiaddr(Ipv4Addr::UNSPECIFIED);
    msg.opts_mut()
        .insert(DhcpOption::Message(why.to_string()));
    // NAK 必须广播 —— 客户端此刻的地址是错的，单播到不了
    let dest = if req.giaddr().is_unspecified() {
        ReplyDest::Broadcast
    } else {
        ReplyDest::Relay(req.giaddr())
    };
    Outcome::with(Reply { msg, dest }, "NAK")
}

fn on_request(
    cfg: &ServerConfig,
    table: &mut LeaseTable,
    req: &Message,
    now: UnixTime,
) -> Outcome {
    let client = client_id_of(req);
    let requested = requested_ip(req);
    let sid = server_ident(req);

    // SELECTING：客户端在多个 OFFER 里选了一个。不是选我们就闭嘴，
    // 同时把我们之前的 OFFER 占位释放掉。
    if let Some(chosen) = sid
        && chosen != cfg.server_id
    {
        if let Some(l) = table.get_by_client(&client)
            && l.state == LeaseState::Offered
        {
            let ip = l.ip;
            table.remove_ip(ip);
        }
        return Outcome::silent("客户端选了别的服务器");
    }

    // 客户端认为自己该拿的地址：
    // SELECTING / INIT-REBOOT 放在 option 50，RENEWING / REBINDING 放在 ciaddr。
    let claimed = requested.or_else(|| {
        (!req.ciaddr().is_unspecified()).then(|| req.ciaddr())
    });
    let Some(claimed) = claimed else {
        return nak(cfg, req, "REQUEST 里既没有 option 50 也没有 ciaddr");
    };

    if !cfg.scope.contains(claimed) {
        // 客户端换网段了（比如笔记本从别的办公室回来），必须 NAK 让它重来
        return nak(cfg, req, "请求的地址不属于本子网");
    }

    let Some(alloc) = allocate(&cfg.scope, table, &client, Some(claimed), now) else {
        return nak(cfg, req, "无可用地址");
    };
    if alloc.ip != claimed {
        // 我们能给的和它要的不是同一个 —— 地址被别人占了，或者管理员改了保留
        return nak(cfg, req, "该地址已不属于此客户端");
    }

    let lease_secs = requested_lease_secs(req, &cfg.scope);
    table.insert(Lease {
        ip: alloc.ip,
        client,
        state: LeaseState::Bound,
        expires_at: now + u64::from(lease_secs),
        hostname: hostname(req),
        created_at: now,
    });

    let mut msg = base_reply(req, MessageType::Ack, cfg.server_id);
    msg.set_yiaddr(alloc.ip).set_ciaddr(req.ciaddr());
    apply_scope_options(&mut msg, req, &cfg.scope, lease_secs);

    Outcome::with(
        Reply {
            msg,
            dest: dest_for(req, alloc.ip),
        },
        "ACK",
    )
}

fn on_decline(
    cfg: &ServerConfig,
    table: &mut LeaseTable,
    req: &Message,
    now: UnixTime,
) -> Outcome {
    // 客户端 ARP 探测发现地址已被占用。把它隔离一段时间，别再发出去。
    let Some(bad) = requested_ip(req) else {
        return Outcome::silent("DECLINE 没带 option 50");
    };
    let client = client_id_of(req);
    table.insert(Lease {
        ip: bad,
        client,
        state: LeaseState::Declined,
        expires_at: now + u64::from(cfg.scope.decline_quarantine_secs),
        hostname: None,
        created_at: now,
    });
    Outcome::silent("地址被客户端 DECLINE，已隔离")
}

fn on_release(
    _cfg: &ServerConfig,
    table: &mut LeaseTable,
    req: &Message,
    _now: UnixTime,
) -> Outcome {
    // RELEASE 用 ciaddr 指明要归还的地址
    let ip = req.ciaddr();
    if ip.is_unspecified() {
        return Outcome::silent("RELEASE 没带 ciaddr");
    }
    let client = client_id_of(req);
    // 只允许归还自己的租约，避免被伪造报文清掉别人的
    if let Some(l) = table.get_by_ip(ip)
        && l.client == client
    {
        table.remove_ip(ip);
        return Outcome::silent("租约已释放");
    }
    Outcome::silent("RELEASE 的地址不属于该客户端，忽略")
}

fn on_inform(cfg: &ServerConfig, req: &Message) -> Outcome {
    // 客户端自己有地址，只想要网络参数。回 ACK 但不分配、不建租约。
    let mut msg = base_reply(req, MessageType::Ack, cfg.server_id);
    msg.set_yiaddr(Ipv4Addr::UNSPECIFIED)
        .set_ciaddr(req.ciaddr());
    apply_scope_options(&mut msg, req, &cfg.scope, cfg.scope.lease_secs);
    // INFORM 的应答不能带租期
    msg.opts_mut().remove(OptionCode::AddressLeaseTime);
    msg.opts_mut().remove(OptionCode::Renewal);
    msg.opts_mut().remove(OptionCode::Rebinding);

    let dest = if !req.ciaddr().is_unspecified() {
        ReplyDest::Unicast(req.ciaddr())
    } else {
        ReplyDest::Broadcast
    };
    Outcome::with(Reply { msg, dest }, "ACK（INFORM，不分配地址）")
}
