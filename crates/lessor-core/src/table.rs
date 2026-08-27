//! 租约表与地址分配。

use std::collections::HashMap;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addr::ClientId;
use crate::lease::{Lease, LeaseState, UnixTime};
use crate::scope::Scope;

/// 内存中的租约表，按 IP 和客户端双索引。
///
/// 持久化不在这里 —— 上层把整张表序列化即可，核心不碰 IO。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LeaseTable {
    by_ip: HashMap<Ipv4Addr, Lease>,
    by_client: HashMap<ClientId, Ipv4Addr>,
}

impl LeaseTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_by_ip(&self, ip: Ipv4Addr) -> Option<&Lease> {
        self.by_ip.get(&ip)
    }

    pub fn get_by_client(&self, client: &ClientId) -> Option<&Lease> {
        self.by_client.get(client).and_then(|ip| self.by_ip.get(ip))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Lease> {
        self.by_ip.values()
    }

    pub fn len(&self) -> usize {
        self.by_ip.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_ip.is_empty()
    }

    /// 写入租约。若该客户端此前绑在别的地址上，旧记录会被解绑，
    /// 避免一个客户端在表里留下多条活跃租约。
    pub fn insert(&mut self, lease: Lease) {
        if let Some(prev_ip) = self.by_client.get(&lease.client).copied()
            && prev_ip != lease.ip
        {
            self.by_ip.remove(&prev_ip);
        }
        self.by_client.insert(lease.client.clone(), lease.ip);
        self.by_ip.insert(lease.ip, lease);
    }

    pub fn remove_ip(&mut self, ip: Ipv4Addr) -> Option<Lease> {
        let lease = self.by_ip.remove(&ip)?;
        if self.by_client.get(&lease.client) == Some(&ip) {
            self.by_client.remove(&lease.client);
        }
        Some(lease)
    }

    /// 清掉已过期且不再需要保留的记录，返回清理条数。
    pub fn reap(&mut self, now: UnixTime) -> usize {
        let stale: Vec<Ipv4Addr> = self
            .by_ip
            .values()
            .filter(|l| {
                // Declined 要留到隔离期满，否则会被立刻重新分配出去
                l.is_expired(now) && l.state != LeaseState::Declined
            })
            .map(|l| l.ip)
            .collect();
        for ip in &stale {
            self.remove_ip(*ip);
        }
        stale.len()
    }

    /// 该地址此刻能否给这个客户端用。
    fn usable_by(&self, ip: Ipv4Addr, client: &ClientId, now: UnixTime) -> bool {
        match self.by_ip.get(&ip) {
            None => true,
            Some(l) if &l.client == client => l.state != LeaseState::Declined,
            Some(l) => l.is_available_for_others(now),
        }
    }
}

/// 为什么选中了这个地址 —— 便于日志和界面展示。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllocSource {
    /// 续用该客户端原有的地址
    Existing,
    /// 命中静态保留
    Reservation,
    /// 满足了客户端请求的地址
    Requested,
    /// 从地址池里取的下一个空闲地址
    Pool,
}

#[derive(Clone, Copy, Debug)]
pub struct Allocation {
    pub ip: Ipv4Addr,
    pub source: AllocSource,
}

/// 按 RFC 2131 §4.3.1 的优先级为客户端挑一个地址。
///
/// 1. 静态保留（管理员后加的保留应当立即生效，故优先于历史租约）
/// 2. 该客户端原有的租约（即使已过期也优先给回同一个，减少地址漂移）
/// 3. 客户端明确请求的地址（前提是可用）
/// 4. 池里下一个空闲地址
pub fn allocate(
    scope: &Scope,
    table: &LeaseTable,
    client: &ClientId,
    requested: Option<Ipv4Addr>,
    now: UnixTime,
) -> Option<Allocation> {
    if let Some(res) = scope.reservation_for(client) {
        return Some(Allocation {
            ip: res.ip,
            source: AllocSource::Reservation,
        });
    }

    if let Some(existing) = table.get_by_client(client)
        && existing.state != LeaseState::Declined
        && scope.is_poolable(existing.ip)
        && !scope.is_reserved_for_other(existing.ip, client)
    {
        return Some(Allocation {
            ip: existing.ip,
            source: AllocSource::Existing,
        });
    }

    if let Some(want) = requested
        && scope.is_poolable(want)
        && !scope.is_reserved_for_other(want, client)
        && table.usable_by(want, client, now)
    {
        return Some(Allocation {
            ip: want,
            source: AllocSource::Requested,
        });
    }

    scope
        .poolable_addrs()
        .find(|ip| !scope.is_reserved_for_other(*ip, client) && table.usable_by(*ip, client, now))
        .map(|ip| Allocation {
            ip,
            source: AllocSource::Pool,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addr::{MacAddr, Range};
    use crate::scope::Reservation;

    fn ip(d: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 88, d)
    }

    fn client(n: u8) -> ClientId {
        ClientId::Mac(MacAddr([0xac, 0x1f, 0x6b, 0, 0, n]))
    }

    fn scope() -> Scope {
        let mut s = Scope::new("lab", Ipv4Addr::new(192, 168, 88, 0), 24);
        s.pools = vec![Range::new(ip(10), ip(12)).unwrap()];
        s
    }

    fn bound(ip_addr: Ipv4Addr, c: &ClientId, expires: UnixTime) -> Lease {
        Lease {
            ip: ip_addr,
            client: c.clone(),
            state: LeaseState::Bound,
            expires_at: expires,
            hostname: None,
            created_at: 0,
        }
    }

    #[test]
    fn fresh_client_gets_lowest_free() {
        let a = allocate(&scope(), &LeaseTable::new(), &client(1), None, 0).unwrap();
        assert_eq!(a.ip, ip(10));
        assert_eq!(a.source, AllocSource::Pool);
    }

    #[test]
    fn second_client_skips_taken_address() {
        let mut t = LeaseTable::new();
        t.insert(bound(ip(10), &client(1), 1000));
        let a = allocate(&scope(), &t, &client(2), None, 0).unwrap();
        assert_eq!(a.ip, ip(11));
    }

    #[test]
    fn client_keeps_its_address_across_renewals() {
        let mut t = LeaseTable::new();
        t.insert(bound(ip(12), &client(1), 1000));
        let a = allocate(&scope(), &t, &client(1), None, 500).unwrap();
        assert_eq!(a.ip, ip(12));
        assert_eq!(a.source, AllocSource::Existing);
    }

    #[test]
    fn expired_lease_is_still_preferred_for_same_client() {
        let mut t = LeaseTable::new();
        t.insert(bound(ip(12), &client(1), 100));
        let a = allocate(&scope(), &t, &client(1), None, 9999).unwrap();
        assert_eq!(a.ip, ip(12), "同一客户端回来时应拿回原地址");
    }

    #[test]
    fn expired_lease_can_be_taken_by_another_client() {
        let mut t = LeaseTable::new();
        t.insert(bound(ip(10), &client(1), 100));
        let a = allocate(&scope(), &t, &client(2), None, 9999).unwrap();
        assert_eq!(a.ip, ip(10));
    }

    #[test]
    fn reservation_wins_over_existing_lease() {
        let mut s = scope();
        s.reservations = vec![Reservation {
            client: client(1),
            ip: ip(12),
            hostname: None,
        }];
        let mut t = LeaseTable::new();
        t.insert(bound(ip(10), &client(1), 9999));
        let a = allocate(&s, &t, &client(1), None, 0).unwrap();
        assert_eq!(a.ip, ip(12));
        assert_eq!(a.source, AllocSource::Reservation);
    }

    #[test]
    fn others_cannot_take_a_reserved_address() {
        let mut s = scope();
        s.reservations = vec![Reservation {
            client: client(1),
            ip: ip(10),
            hostname: None,
        }];
        let a = allocate(&s, &LeaseTable::new(), &client(2), None, 0).unwrap();
        assert_eq!(a.ip, ip(11), "保留给别人的地址要跳过");
    }

    #[test]
    fn honours_requested_address_when_free() {
        let a = allocate(&scope(), &LeaseTable::new(), &client(1), Some(ip(12)), 0).unwrap();
        assert_eq!(a.ip, ip(12));
        assert_eq!(a.source, AllocSource::Requested);
    }

    #[test]
    fn ignores_requested_address_outside_pool() {
        let a = allocate(&scope(), &LeaseTable::new(), &client(1), Some(ip(99)), 0).unwrap();
        assert_eq!(a.ip, ip(10), "池外的请求应被忽略并回退到池分配");
    }

    #[test]
    fn declined_address_is_skipped_until_quarantine_ends() {
        let mut t = LeaseTable::new();
        let mut l = bound(ip(10), &client(9), 0);
        l.state = LeaseState::Declined;
        l.expires_at = 3600;
        t.insert(l);
        assert_eq!(
            allocate(&scope(), &t, &client(1), None, 0).unwrap().ip,
            ip(11)
        );
        assert_eq!(
            allocate(&scope(), &t, &client(1), None, 3600).unwrap().ip,
            ip(10)
        );
    }

    #[test]
    fn pool_exhaustion_returns_none() {
        let mut t = LeaseTable::new();
        for (i, d) in (10u8..=12).enumerate() {
            t.insert(bound(ip(d), &client(i as u8 + 1), 9999));
        }
        assert!(allocate(&scope(), &t, &client(99), None, 0).is_none());
    }

    #[test]
    fn insert_rebinds_client_to_new_address() {
        let mut t = LeaseTable::new();
        t.insert(bound(ip(10), &client(1), 9999));
        t.insert(bound(ip(11), &client(1), 9999));
        assert_eq!(t.len(), 1, "同一客户端不应留下两条租约");
        assert_eq!(t.get_by_client(&client(1)).unwrap().ip, ip(11));
        assert!(t.get_by_ip(ip(10)).is_none());
    }

    #[test]
    fn reap_clears_expired_but_keeps_declined() {
        let mut t = LeaseTable::new();
        t.insert(bound(ip(10), &client(1), 100));
        let mut d = bound(ip(11), &client(2), 100);
        d.state = LeaseState::Declined;
        t.insert(d);
        assert_eq!(t.reap(500), 1);
        assert!(t.get_by_ip(ip(10)).is_none());
        assert!(t.get_by_ip(ip(11)).is_some(), "Declined 要留到隔离期满");
    }
}
