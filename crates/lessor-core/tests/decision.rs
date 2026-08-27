//! 报文决策的行为测试 —— 覆盖 RFC 2131 §4.3 里每条容易写错的规则。

use std::net::Ipv4Addr;

use dhcproto::v4::{DhcpOption, Flags, Message, MessageType, Opcode, OptionCode};

use lessor_core::addr::Range;
use lessor_core::lease::{Lease, LeaseState};
use lessor_core::scope::Reservation;
use lessor_core::{ClientId, LeaseTable, MacAddr, ReplyDest, Scope, ServerConfig, handle};

const SERVER: Ipv4Addr = Ipv4Addr::new(192, 168, 88, 1);

fn ip(d: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 168, 88, d)
}

fn mac(n: u8) -> [u8; 6] {
    [0xac, 0x1f, 0x6b, 0, 0, n]
}

fn cfg() -> ServerConfig {
    let mut scope = Scope::new("lab", Ipv4Addr::new(192, 168, 88, 0), 24);
    scope.pools = vec![Range::new(ip(10), ip(12)).unwrap()];
    scope.router = Some(SERVER);
    scope.dns = vec![Ipv4Addr::new(223, 5, 5, 5)];
    scope.lease_secs = 3600;
    scope.offer_secs = 30;
    scope.decline_quarantine_secs = 600;
    ServerConfig {
        server_id: SERVER,
        scope,
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

// ---------- DISCOVER ----------

#[test]
fn discover_gets_offer_with_scope_options() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let out = handle(&c, &mut t, &req(MessageType::Discover, 1), 100);
    let r = out.reply.expect("应当回 OFFER");

    assert_eq!(msg_type(&r.msg), MessageType::Offer);
    assert_eq!(r.msg.yiaddr(), ip(10));
    assert_eq!(r.msg.opcode(), Opcode::BootReply);
    assert_eq!(r.msg.xid(), 0xDEAD_BEEF, "xid 必须原样带回");
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
    let (c, mut t) = (cfg(), LeaseTable::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), 100);

    let l = t.get_by_ip(ip(10)).expect("OFFER 应当占位");
    assert_eq!(l.state, LeaseState::Offered);
    assert_eq!(l.expires_at, 130, "占位只保留 offer_secs，不是整个租期");
}

#[test]
fn concurrent_discovers_get_different_addresses() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let a = handle(&c, &mut t, &req(MessageType::Discover, 1), 0).reply.unwrap();
    let b = handle(&c, &mut t, &req(MessageType::Discover, 2), 0).reply.unwrap();
    assert_ne!(a.msg.yiaddr(), b.msg.yiaddr(), "占位机制必须防止重复分配");
}

#[test]
fn pool_exhaustion_is_silent_not_nak() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    for i in 1..=3 {
        handle(&c, &mut t, &req(MessageType::Discover, i), 0);
    }
    let out = handle(&c, &mut t, &req(MessageType::Discover, 9), 0);
    assert!(out.reply.is_none(), "池满时应沉默，让客户端去问别的服务器");
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
    let (c, mut t) = (cfg(), LeaseTable::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), 0);
    let out = handle(&c, &mut t, &request_selecting(1, ip(10), SERVER), 0);
    let r = out.reply.expect("应当回 ACK");

    assert_eq!(msg_type(&r.msg), MessageType::Ack);
    assert_eq!(r.msg.yiaddr(), ip(10));
    let l = t.get_by_ip(ip(10)).unwrap();
    assert_eq!(l.state, LeaseState::Bound);
    assert_eq!(l.expires_at, 3600);
}

#[test]
fn request_selecting_another_server_releases_our_offer() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    handle(&c, &mut t, &req(MessageType::Discover, 1), 0);
    assert!(t.get_by_ip(ip(10)).is_some());

    let other = Ipv4Addr::new(192, 168, 88, 254);
    let out = handle(&c, &mut t, &request_selecting(1, ip(10), other), 0);

    assert!(out.reply.is_none(), "不是选我们就不该回应");
    assert!(
        t.get_by_ip(ip(10)).is_none(),
        "占位必须释放，否则地址会被白白占住"
    );
}

#[test]
fn request_for_foreign_subnet_gets_nak() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let m = request_selecting(1, Ipv4Addr::new(10, 9, 9, 9), SERVER);
    let r = handle(&c, &mut t, &m, 0).reply.expect("应当回 NAK");

    assert_eq!(msg_type(&r.msg), MessageType::Nak);
    assert!(r.msg.yiaddr().is_unspecified(), "NAK 的 yiaddr 必须为 0");
    assert_eq!(r.dest, ReplyDest::Broadcast, "NAK 必须广播");
}

#[test]
fn request_for_someone_elses_address_gets_nak() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    t.insert(Lease {
        ip: ip(10),
        client: ClientId::Mac(MacAddr(mac(1))),
        state: LeaseState::Bound,
        expires_at: 9999,
        hostname: None,
        created_at: 0,
    });
    let r = handle(&c, &mut t, &request_selecting(2, ip(10), SERVER), 0)
        .reply
        .expect("应当回 NAK");
    assert_eq!(msg_type(&r.msg), MessageType::Nak);
}

#[test]
fn renewing_via_ciaddr_gets_ack_and_unicast() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    t.insert(Lease {
        ip: ip(11),
        client: ClientId::Mac(MacAddr(mac(1))),
        state: LeaseState::Bound,
        expires_at: 1000,
        hostname: None,
        created_at: 0,
    });

    // RENEWING：没有 option 50 和 54，地址放在 ciaddr
    let mut m = req(MessageType::Request, 1);
    m.set_ciaddr(ip(11)).set_flags(Flags::default());

    let r = handle(&c, &mut t, &m, 500).reply.expect("应当回 ACK");
    assert_eq!(msg_type(&r.msg), MessageType::Ack);
    assert_eq!(r.msg.yiaddr(), ip(11));
    assert_eq!(r.dest, ReplyDest::Unicast(ip(11)), "续租应单播回客户端");
    assert_eq!(t.get_by_ip(ip(11)).unwrap().expires_at, 500 + 3600);
}

#[test]
fn request_without_address_information_gets_nak() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let r = handle(&c, &mut t, &req(MessageType::Request, 1), 0)
        .reply
        .expect("应当回 NAK");
    assert_eq!(msg_type(&r.msg), MessageType::Nak);
}

#[test]
fn client_requested_lease_is_capped_by_scope() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let mut m = request_selecting(1, ip(10), SERVER);
    m.opts_mut().insert(DhcpOption::AddressLeaseTime(86400));
    let r = handle(&c, &mut t, &m, 0).reply.unwrap();
    assert_eq!(
        opt_u32(&r.msg, OptionCode::AddressLeaseTime),
        Some(3600),
        "客户端不能要到超过作用域上限的租期"
    );
}

// ---------- DECLINE / RELEASE ----------

#[test]
fn decline_quarantines_the_address() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let mut m = req(MessageType::Decline, 1);
    m.opts_mut().insert(DhcpOption::RequestedIpAddress(ip(10)));

    let out = handle(&c, &mut t, &m, 100);
    assert!(out.reply.is_none(), "DECLINE 不需要回应");

    let l = t.get_by_ip(ip(10)).unwrap();
    assert_eq!(l.state, LeaseState::Declined);
    assert_eq!(l.expires_at, 700, "隔离到 now + decline_quarantine_secs");

    // 隔离期内不能再发出去
    let r = handle(&c, &mut t, &req(MessageType::Discover, 2), 100)
        .reply
        .unwrap();
    assert_eq!(r.msg.yiaddr(), ip(11));
}

#[test]
fn release_frees_the_lease() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    t.insert(Lease {
        ip: ip(10),
        client: ClientId::Mac(MacAddr(mac(1))),
        state: LeaseState::Bound,
        expires_at: 9999,
        hostname: None,
        created_at: 0,
    });

    let mut m = req(MessageType::Release, 1);
    m.set_ciaddr(ip(10));
    handle(&c, &mut t, &m, 0);
    assert!(t.get_by_ip(ip(10)).is_none());
}

#[test]
fn release_from_wrong_client_is_ignored() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    t.insert(Lease {
        ip: ip(10),
        client: ClientId::Mac(MacAddr(mac(1))),
        state: LeaseState::Bound,
        expires_at: 9999,
        hostname: None,
        created_at: 0,
    });

    // 客户端 2 试图归还客户端 1 的地址
    let mut m = req(MessageType::Release, 2);
    m.set_ciaddr(ip(10));
    handle(&c, &mut t, &m, 0);
    assert!(
        t.get_by_ip(ip(10)).is_some(),
        "伪造的 RELEASE 不能清掉别人的租约"
    );
}

// ---------- INFORM ----------

#[test]
fn inform_returns_options_without_a_lease() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let mut m = req(MessageType::Inform, 1);
    m.set_ciaddr(Ipv4Addr::new(192, 168, 88, 77));

    let r = handle(&c, &mut t, &m, 0).reply.expect("应当回 ACK");
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
    let (c, mut t) = (cfg(), LeaseTable::new());

    // 同一个 option 61，但 chaddr 不同 —— 应视为同一个客户端（PXE 固件与操作系统的常见情形）
    let id = vec![0xff, 1, 2, 3, 4];
    let mut a = req(MessageType::Discover, 1);
    a.opts_mut().insert(DhcpOption::ClientIdentifier(id.clone()));
    let mut b = req(MessageType::Discover, 99);
    b.opts_mut().insert(DhcpOption::ClientIdentifier(id));

    let ra = handle(&c, &mut t, &a, 0).reply.unwrap();
    let rb = handle(&c, &mut t, &b, 0).reply.unwrap();
    assert_eq!(ra.msg.yiaddr(), rb.msg.yiaddr(), "同一 option 61 应拿到同一地址");
    assert_eq!(t.len(), 1);
}

// ---------- 保留 ----------

#[test]
fn reservation_is_honoured_end_to_end() {
    let mut c = cfg();
    c.scope.reservations = vec![Reservation {
        client: ClientId::Mac(MacAddr(mac(7))),
        ip: ip(12),
        hostname: Some("bmc-07".into()),
    }];
    let mut t = LeaseTable::new();

    let r = handle(&c, &mut t, &req(MessageType::Discover, 7), 0)
        .reply
        .unwrap();
    assert_eq!(r.msg.yiaddr(), ip(12));

    let r = handle(&c, &mut t, &request_selecting(7, ip(12), SERVER), 0)
        .reply
        .unwrap();
    assert_eq!(msg_type(&r.msg), MessageType::Ack);
}

// ---------- 中继 ----------

#[test]
fn relayed_request_replies_to_the_relay() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let relay = Ipv4Addr::new(192, 168, 88, 200);
    let mut m = req(MessageType::Discover, 1);
    m.set_giaddr(relay);

    let r = handle(&c, &mut t, &m, 0).reply.unwrap();
    assert_eq!(r.dest, ReplyDest::Relay(relay));
    assert_eq!(r.msg.giaddr(), relay, "giaddr 必须原样带回");
}

// ---------- 畸形报文 ----------

#[test]
fn message_without_option_53_is_ignored() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let mut m = Message::default();
    m.set_opcode(Opcode::BootRequest).set_chaddr(&mac(1));
    assert!(handle(&c, &mut t, &m, 0).reply.is_none());
}

#[test]
fn bootreply_from_a_rogue_is_ignored() {
    let (c, mut t) = (cfg(), LeaseTable::new());
    let mut m = req(MessageType::Discover, 1);
    m.set_opcode(Opcode::BootReply);
    assert!(handle(&c, &mut t, &m, 0).reply.is_none());
}
