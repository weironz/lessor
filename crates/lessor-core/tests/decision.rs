//! 报文决策的行为测试 —— 覆盖 RFC 2131 §4.3 里每条容易写错的规则。

use std::net::Ipv4Addr;

use dhcproto::v4::{DhcpOption, Flags, Message, MessageType, Opcode, OptionCode};

use lessor_core::addr::Range;
use lessor_core::lease::{Lease, LeaseState};
use lessor_core::scope::Reservation;
use lessor_core::{
    ClientId, DropReason, LeaseStore, MacAddr, MemoryStore, RecvCtx, ReplyDest, Scope, ScopeId,
    ServerConfig, handle,
};

const SERVER: Ipv4Addr = Ipv4Addr::new(192, 168, 88, 1);
const SID: ScopeId = ScopeId(1);

fn ip(d: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 168, 88, d)
}

fn mac(n: u8) -> [u8; 6] {
    [0xac, 0x1f, 0x6b, 0, 0, n]
}

fn lab_scope() -> Scope {
    let mut s = Scope::new(1, "lab", Ipv4Addr::new(192, 168, 88, 0), 24);
    s.pools = vec![Range::new(ip(10), ip(12)).unwrap()];
    s.router = Some(SERVER);
    s.dns = vec![Ipv4Addr::new(223, 5, 5, 5)];
    s.lease_secs = 3600;
    s.offer_secs = 30;
    s.decline_quarantine_secs = 600;
    s
}

fn cfg() -> ServerConfig {
    ServerConfig::new(vec![lab_scope()])
}

fn ctx(now: u64) -> RecvCtx {
    RecvCtx {
        now,
        server_ip: SERVER,
    }
}

/// 构造一个客户端请求。
fn req(kind: MessageType, client: u8) -> Message {
    let mut m = Message::default();
    m.set_opcode(Opcode::BootRequest)
        .set_xid(0xDEAD_BEEF)
        .set_flags(Flags::default().set_broadcast())
        .set_chaddr(&mac(client));
    m.opts_mut().insert(DhcpOption::MessageType(kind));
    m
}

fn msg_type(m: &Message) -> MessageType {
    m.opts().msg_type().expect("应答必须带 option 53")
}

fn opt_u32(m: &Message, code: OptionCode) -> Option<u32> {
    match m.opts().get(code) {
        Some(DhcpOption::AddressLeaseTime(v)) => Some(*v),
        Some(DhcpOption::Renewal(v)) => Some(*v),
        Some(DhcpOption::Rebinding(v)) => Some(*v),
        _ => None,
    }
}

fn bound(ip_addr: Ipv4Addr, client: u8, expires: u64) -> Lease {
    Lease {
        ip: ip_addr,
        client: ClientId::Mac(MacAddr(mac(client))),
        scope_id: SID,
        state: LeaseState::Bound,
        expires_at: expires,
        hostname: None,
        vendor_class: None,
        created_at: 0,
        last_seen: 0,
    }
}

// ---------- DISCOVER ----------

#[test]
fn discover_gets_offer_with_scope_options() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let out = handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(100));
    let r = out.reply().expect("应当回 OFFER");

    assert_eq!(msg_type(&r.msg), MessageType::Offer);
    assert_eq!(r.msg.yiaddr(), ip(10));
    assert_eq!(r.msg.opcode(), Opcode::BootReply);
    assert_eq!(r.msg.xid(), 0xDEAD_BEEF, "xid 必须原样带回");
    assert_eq!(r.scope_id, SID);
    assert_eq!(
        r.msg.opts().get(OptionCode::SubnetMask),
        Some(&DhcpOption::SubnetMask(Ipv4Addr::new(255, 255, 255, 0)))
    );
    assert_eq!(
        r.msg.opts().get(OptionCode::Router),
        Some(&DhcpOption::Router(vec![SERVER]))
    );
    assert_eq!(opt_u32(&r.msg, OptionCode::AddressLeaseTime), Some(3600));
    assert_eq!(opt_u32(&r.msg, OptionCode::Renewal), Some(1800), "T1 = 一半");
    assert_eq!(opt_u32(&r.msg, OptionCode::Rebinding), Some(3150), "T2 = 7/8");
}

#[test]
fn offer_is_a_short_lived_placeholder() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(100));

    let l = t.get_by_ip(SID, ip(10)).expect("OFFER 应当占位");
    assert_eq!(l.state, LeaseState::Offered);
    assert_eq!(l.expires_at, 130, "占位只保留 offer_secs，不是整个租期");
}

#[test]
fn concurrent_discovers_get_different_addresses() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let a = handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(0));
    let b = handle(&c, &mut t, &req(MessageType::Discover, 2), ctx(0));
    assert_ne!(
        a.reply().unwrap().msg.yiaddr(),
        b.reply().unwrap().msg.yiaddr(),
        "占位机制必须防止重复分配"
    );
}

#[test]
fn pool_exhaustion_is_silent_not_nak() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    for i in 1..=3 {
        handle(&c, &mut t, &req(MessageType::Discover, i), ctx(0));
    }
    let out = handle(&c, &mut t, &req(MessageType::Discover, 9), ctx(0));
    assert_eq!(
        out.drop_reason(),
        Some(DropReason::PoolExhausted),
        "池满时应沉默，让客户端去问别的服务器"
    );
}

// ---------- REQUEST ----------

fn request_selecting(client: u8, want: Ipv4Addr, server: Ipv4Addr) -> Message {
    let mut m = req(MessageType::Request, client);
    m.opts_mut().insert(DhcpOption::RequestedIpAddress(want));
    m.opts_mut().insert(DhcpOption::ServerIdentifier(server));
    m
}

#[test]
fn request_selecting_us_gets_ack_and_binds() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(0));
    let out = handle(&c, &mut t, &request_selecting(1, ip(10), SERVER), ctx(0));
    let r = out.reply().expect("应当回 ACK");

    assert_eq!(msg_type(&r.msg), MessageType::Ack);
    assert_eq!(r.msg.yiaddr(), ip(10));
    let l = t.get_by_ip(SID, ip(10)).unwrap();
    assert_eq!(l.state, LeaseState::Bound);
    assert_eq!(l.expires_at, 3600);
}

#[test]
fn request_selecting_another_server_releases_our_offer() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(0));
    assert!(t.get_by_ip(SID, ip(10)).is_some());

    let other = Ipv4Addr::new(192, 168, 88, 254);
    let out = handle(&c, &mut t, &request_selecting(1, ip(10), other), ctx(0));

    assert_eq!(out.drop_reason(), Some(DropReason::ChoseAnotherServer));
    assert!(
        t.get_by_ip(SID, ip(10)).is_none(),
        "占位必须释放，否则地址会被白白占住"
    );
}

#[test]
fn request_for_foreign_subnet_gets_nak() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    // 地址不在本子网，但报文本身是从本网段收到的
    let m = request_selecting(1, Ipv4Addr::new(192, 168, 88, 250), SERVER);
    let _ = m;
    let mut m = req(MessageType::Request, 1);
    m.opts_mut()
        .insert(DhcpOption::RequestedIpAddress(Ipv4Addr::new(10, 9, 9, 9)));
    m.opts_mut().insert(DhcpOption::ServerIdentifier(SERVER));

    let out = handle(&c, &mut t, &m, ctx(0));
    let r = out.reply().expect("应当回 NAK");
    assert_eq!(msg_type(&r.msg), MessageType::Nak);
    assert!(r.msg.yiaddr().is_unspecified(), "NAK 的 yiaddr 必须为 0");
    assert_eq!(r.dest, ReplyDest::Broadcast, "NAK 必须广播");
}

#[test]
fn request_for_someone_elses_address_gets_nak() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    t.insert(bound(ip(10), 1, 9999));
    let out = handle(&c, &mut t, &request_selecting(2, ip(10), SERVER), ctx(0));
    assert_eq!(msg_type(&out.reply().expect("应当回 NAK").msg), MessageType::Nak);
}

#[test]
fn renewing_via_ciaddr_gets_ack_and_unicast() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    t.insert(bound(ip(11), 1, 1000));

    // RENEWING：没有 option 50 和 54，地址放在 ciaddr
    let mut m = req(MessageType::Request, 1);
    m.set_ciaddr(ip(11)).set_flags(Flags::default());

    let out = handle(&c, &mut t, &m, ctx(500));
    let r = out.reply().expect("应当回 ACK");
    assert_eq!(msg_type(&r.msg), MessageType::Ack);
    assert_eq!(r.msg.yiaddr(), ip(11));
    assert_eq!(r.dest, ReplyDest::Unicast(ip(11)), "续租应单播回客户端");
    assert_eq!(t.get_by_ip(SID, ip(11)).unwrap().expires_at, 500 + 3600);
}

#[test]
fn renewal_preserves_the_original_created_at() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut l = bound(ip(11), 1, 1000);
    l.created_at = 42;
    t.insert(l);

    let mut m = req(MessageType::Request, 1);
    m.set_ciaddr(ip(11));
    handle(&c, &mut t, &m, ctx(500));

    let l = t.get_by_ip(SID, ip(11)).unwrap();
    assert_eq!(l.created_at, 42, "首次分配时间不该被续租重置");
    assert_eq!(l.last_seen, 500);
}

#[test]
fn request_without_address_information_gets_nak() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let out = handle(&c, &mut t, &req(MessageType::Request, 1), ctx(0));
    assert_eq!(msg_type(&out.reply().expect("应当回 NAK").msg), MessageType::Nak);
}

#[test]
fn client_requested_lease_is_capped_by_scope() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = request_selecting(1, ip(10), SERVER);
    m.opts_mut().insert(DhcpOption::AddressLeaseTime(86400));
    let out = handle(&c, &mut t, &m, ctx(0));
    assert_eq!(
        opt_u32(&out.reply().unwrap().msg, OptionCode::AddressLeaseTime),
        Some(3600),
        "客户端不能要到超过作用域上限的租期"
    );
}

// ---------- DECLINE / RELEASE ----------

#[test]
fn decline_quarantines_the_address() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = req(MessageType::Decline, 1);
    m.opts_mut().insert(DhcpOption::RequestedIpAddress(ip(10)));

    let out = handle(&c, &mut t, &m, ctx(100));
    assert!(out.reply().is_none(), "DECLINE 不需要回应");

    let l = t.get_by_ip(SID, ip(10)).unwrap();
    assert_eq!(l.state, LeaseState::Declined);
    assert_eq!(l.expires_at, 700, "隔离到 now + decline_quarantine_secs");

    // 隔离期内不能再发出去
    let out = handle(&c, &mut t, &req(MessageType::Discover, 2), ctx(100));
    assert_eq!(out.reply().unwrap().msg.yiaddr(), ip(11));
}

#[test]
fn release_frees_the_lease() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    t.insert(bound(ip(10), 1, 9999));

    let mut m = req(MessageType::Release, 1);
    m.set_ciaddr(ip(10));
    handle(&c, &mut t, &m, ctx(0));
    assert!(t.get_by_ip(SID, ip(10)).is_none());
}

#[test]
fn release_from_wrong_client_is_ignored() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    t.insert(bound(ip(10), 1, 9999));

    // 客户端 2 试图归还客户端 1 的地址
    let mut m = req(MessageType::Release, 2);
    m.set_ciaddr(ip(10));
    let out = handle(&c, &mut t, &m, ctx(0));

    assert_eq!(out.drop_reason(), Some(DropReason::ReleaseNotOwned));
    assert!(
        t.get_by_ip(SID, ip(10)).is_some(),
        "伪造的 RELEASE 不能清掉别人的租约"
    );
}

// ---------- INFORM ----------

#[test]
fn inform_returns_options_without_a_lease() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = req(MessageType::Inform, 1);
    m.set_ciaddr(Ipv4Addr::new(192, 168, 88, 77));

    let out = handle(&c, &mut t, &m, ctx(0));
    let r = out.reply().expect("应当回 ACK");
    assert_eq!(msg_type(&r.msg), MessageType::Ack);
    assert!(r.msg.yiaddr().is_unspecified(), "INFORM 不分配地址");
    assert!(
        r.msg.opts().get(OptionCode::AddressLeaseTime).is_none(),
        "INFORM 的应答不能带租期"
    );
    assert!(r.msg.opts().get(OptionCode::SubnetMask).is_some());
    assert!(t.is_empty(), "INFORM 不应产生租约");
}

// ---------- 客户端标识 ----------

#[test]
fn option_61_takes_precedence_over_chaddr() {
    let (c, mut t) = (cfg(), MemoryStore::new());

    // 同一个 option 61，但 chaddr 不同 —— 应视为同一个客户端
    // （PXE 固件与操作系统的常见情形）
    let id = vec![0xff, 1, 2, 3, 4];
    let mut a = req(MessageType::Discover, 1);
    a.opts_mut().insert(DhcpOption::ClientIdentifier(id.clone()));
    let mut b = req(MessageType::Discover, 99);
    b.opts_mut().insert(DhcpOption::ClientIdentifier(id));

    let ra = handle(&c, &mut t, &a, ctx(0));
    let rb = handle(&c, &mut t, &b, ctx(0));
    assert_eq!(
        ra.reply().unwrap().msg.yiaddr(),
        rb.reply().unwrap().msg.yiaddr(),
        "同一 option 61 应拿到同一地址"
    );
    assert_eq!(t.len(), 1);
}

#[test]
fn client_with_neither_mac_nor_option_61_is_dropped() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = Message::default();
    m.set_opcode(Opcode::BootRequest).set_chaddr(&[0u8; 6]);
    m.opts_mut()
        .insert(DhcpOption::MessageType(MessageType::Discover));

    let out = handle(&c, &mut t, &m, ctx(0));
    assert_eq!(out.drop_reason(), Some(DropReason::UnidentifiableClient));
}

#[test]
fn vendor_class_is_recorded_on_the_lease() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = req(MessageType::Discover, 1);
    m.opts_mut().insert(DhcpOption::ClassIdentifier(
        b"PXEClient:Arch:00007:UNDI:003016".to_vec(),
    ));

    handle(&c, &mut t, &m, ctx(0));
    let l = t.get_by_ip(SID, ip(10)).unwrap();
    assert_eq!(
        l.vendor_class.as_deref(),
        Some("PXEClient:Arch:00007:UNDI:003016")
    );
    assert!(l.is_pxe(), "应能识别出这是 PXE 客户端");
}

// ---------- 保留 ----------

#[test]
fn reservation_is_honoured_end_to_end() {
    let mut s = lab_scope();
    s.reservations = vec![Reservation {
        client: ClientId::Mac(MacAddr(mac(7))),
        ip: ip(12),
        hostname: Some("bmc-07".into()),
    }];
    let c = ServerConfig::new(vec![s]);
    let mut t = MemoryStore::new();

    let out = handle(&c, &mut t, &req(MessageType::Discover, 7), ctx(0));
    assert_eq!(out.reply().unwrap().msg.yiaddr(), ip(12));

    let out = handle(&c, &mut t, &request_selecting(7, ip(12), SERVER), ctx(0));
    assert_eq!(msg_type(&out.reply().unwrap().msg), MessageType::Ack);
}

// ---------- 多作用域 ----------

fn two_scope_cfg() -> ServerConfig {
    let mut a = lab_scope(); // 192.168.88.0/24, id 1
    a.name = "net-a".into();

    let mut b = Scope::new(2, "net-b", Ipv4Addr::new(10, 20, 0, 0), 24);
    b.pools = vec![Range::new(Ipv4Addr::new(10, 20, 0, 50), Ipv4Addr::new(10, 20, 0, 60)).unwrap()];
    b.router = Some(Ipv4Addr::new(10, 20, 0, 1));

    ServerConfig::new(vec![a, b])
}

#[test]
fn scope_is_selected_by_the_receiving_interface() {
    let (c, mut t) = (two_scope_cfg(), MemoryStore::new());

    let out = handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(0));
    let r = out.reply().unwrap();
    assert_eq!(r.scope_id, ScopeId(1));
    assert_eq!(r.msg.yiaddr(), ip(10));

    // 换一块网卡收包 —— 应当落到另一个作用域
    let out = handle(
        &c,
        &mut t,
        &req(MessageType::Discover, 2),
        RecvCtx {
            now: 0,
            server_ip: Ipv4Addr::new(10, 20, 0, 1),
        },
    );
    let r = out.reply().unwrap();
    assert_eq!(r.scope_id, ScopeId(2));
    assert_eq!(r.msg.yiaddr(), Ipv4Addr::new(10, 20, 0, 50));
}

#[test]
fn relayed_request_selects_scope_by_giaddr_and_replies_to_relay() {
    let (c, mut t) = (two_scope_cfg(), MemoryStore::new());
    let relay = Ipv4Addr::new(10, 20, 0, 1);

    // 报文从 88 网段的网卡进来，但 giaddr 指向 10.20 网段
    let mut m = req(MessageType::Discover, 1);
    m.set_giaddr(relay);

    let out = handle(&c, &mut t, &m, ctx(0));
    let r = out.reply().unwrap();
    assert_eq!(r.scope_id, ScopeId(2), "跨网段时必须按 giaddr 选作用域");
    assert_eq!(r.msg.yiaddr(), Ipv4Addr::new(10, 20, 0, 50));
    assert_eq!(r.dest, ReplyDest::Relay(relay));
    assert_eq!(r.msg.giaddr(), relay, "giaddr 必须原样带回");
}

#[test]
fn request_from_an_unknown_network_is_dropped() {
    let (c, mut t) = (two_scope_cfg(), MemoryStore::new());
    let out = handle(
        &c,
        &mut t,
        &req(MessageType::Discover, 1),
        RecvCtx {
            now: 0,
            server_ip: Ipv4Addr::new(172, 16, 9, 1),
        },
    );
    assert_eq!(out.drop_reason(), Some(DropReason::NoMatchingScope));
}

#[test]
fn a_disabled_scope_serves_nobody() {
    let mut s = lab_scope();
    s.enabled = false;
    let c = ServerConfig::new(vec![s]);
    let mut t = MemoryStore::new();

    let out = handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(0));
    assert_eq!(out.drop_reason(), Some(DropReason::ScopeDisabled));
    assert!(t.is_empty(), "禁用的作用域不应产生任何租约");
}

#[test]
fn leases_carry_the_scope_that_issued_them() {
    let (c, mut t) = (two_scope_cfg(), MemoryStore::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), ctx(0));
    handle(
        &c,
        &mut t,
        &req(MessageType::Discover, 2),
        RecvCtx {
            now: 0,
            server_ip: Ipv4Addr::new(10, 20, 0, 1),
        },
    );

    assert_eq!(t.get_by_ip(ScopeId(1), ip(10)).unwrap().scope_id, ScopeId(1));
    assert_eq!(
        t.get_by_ip(ScopeId(2), Ipv4Addr::new(10, 20, 0, 50))
            .unwrap()
            .scope_id,
        ScopeId(2)
    );
    assert_eq!(t.len(), 2);
}

// ---------- 畸形报文 ----------

#[test]
fn message_without_option_53_is_ignored() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = Message::default();
    m.set_opcode(Opcode::BootRequest).set_chaddr(&mac(1));
    assert_eq!(
        handle(&c, &mut t, &m, ctx(0)).drop_reason(),
        Some(DropReason::NoMessageType)
    );
}

#[test]
fn bootreply_from_a_rogue_is_ignored() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    let mut m = req(MessageType::Discover, 1);
    m.set_opcode(Opcode::BootReply);
    assert_eq!(
        handle(&c, &mut t, &m, ctx(0)).drop_reason(),
        Some(DropReason::NotBootRequest)
    );
}

#[test]
fn server_side_message_types_are_not_processed() {
    let (c, mut t) = (cfg(), MemoryStore::new());
    for kind in [MessageType::Offer, MessageType::Ack, MessageType::Nak] {
        let out = handle(&c, &mut t, &req(kind, 1), ctx(0));
        assert_eq!(out.drop_reason(), Some(DropReason::UnsupportedType));
    }
}
