//! 租约存储与地址分配。
//!
//! 存储是 trait —— 内存实现在本 crate，sqlite 之类的持久化实现放上层，
//! 分配逻辑不需要跟着改。
//!
//! 键是 `(ScopeId, Ipv4Addr)` 而不是裸 IP：多网卡场景下两个隔离网段
//! 完全可能都用 `192.168.1.0/24`，同一个地址在不同作用域里是不同的东西。

use std::collections::HashMap;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addr::ClientId;
use crate::lease::{Lease, LeaseState, UnixTime};
use crate::scope::{Scope, ScopeId};

/// 租约存储。
///
/// 查询返回**克隆**而不是引用：sqlite / PostgreSQL 这类后端每次查询都产生
/// 新值，没有可借出去的东西。`Lease` 是个小结构体，克隆的代价远低于
/// 让整个后端实现不出来。
pub trait LeaseStore {
    fn get_by_ip(&self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease>;
    fn get_by_client(&self, scope: ScopeId, client: &ClientId) -> Option<Lease>;
    /// 写入或覆盖。实现必须维护好双向索引的一致性。
    fn insert(&mut self, lease: Lease);

    /// **原子占位**：地址此刻能给这个客户端用就占下并返回 `true`，
    /// 已被别人占着就返回 `false`，调用方换下一个候选。
    ///
    /// 为什么不是"先 `usable_by` 判断、再 `insert`"：那两步之间存在窗口。
    /// 单进程内存实现下窗口为零（`&mut self` 独占），但共享数据库的多实例
    /// 部署里，两个实例会同时判定同一个地址可用、各自发出去 —— 同一个 IP
    /// 给了两台机器。所以判断与占用必须是一步，由存储层保证原子性
    /// （sqlite / PostgreSQL 上就是带唯一约束的条件插入）。
    ///
    /// 这个契约是 v1.0 多实例 HA 的前提，各后端共用同一份语义。
    fn try_claim(&mut self, lease: Lease, now: UnixTime) -> bool {
        // 默认实现给独占访问的后端用（`&mut self` 期间没有别人能插进来）。
        // 共享存储必须重写成一条带条件的原子写。
        if self.usable_by(lease.scope_id, lease.ip, &lease.client, now) {
            self.insert(lease);
            true
        } else {
            false
        }
    }
    fn remove(&mut self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease>;
    /// 全部租约，按 (作用域, IP) 排序，便于界面稳定展示。
    fn all(&self) -> Vec<Lease>;
    /// 清掉已过期且不再需要保留的记录，返回清理条数。
    fn reap(&mut self, now: UnixTime) -> usize;

    /// 这个地址在网络上是否已被别人静态占用（而不是被我们发出去的）。
    ///
    /// 默认 `false`：不做探测的部署行为不变。上层接了冲突探测之后重写它，
    /// 分配就会自动绕开那些地址 —— 把地址发给一台已经用着它的机器，
    /// 结果是两台机器地址冲突，比池子少几个地址糟得多。
    fn is_occupied_elsewhere(&self, _ip: Ipv4Addr) -> bool {
        false
    }

    /// 删掉某个作用域的全部租约，返回条数。
    ///
    /// 删作用域时必须一并清掉 —— 留着会让"已用"统计和界面出现
    /// 指向不存在作用域的幽灵记录。
    fn clear_scope(&mut self, scope: ScopeId) -> usize;

    /// 该地址此刻能否给这个客户端用。默认实现对所有存储都适用。
    fn usable_by(&self, scope: ScopeId, ip: Ipv4Addr, client: &ClientId, now: UnixTime) -> bool {
        match self.get_by_ip(scope, ip) {
            None => true,
            Some(l) if &l.client == client => l.state != LeaseState::Declined,
            Some(l) => l.is_available_for_others(now),
        }
    }
}

/// 内存实现，双向索引。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MemoryStore {
    leases: HashMap<(ScopeId, Ipv4Addr), Lease>,
    /// (作用域, 客户端) → 该客户端在这个作用域里持有的地址
    index: HashMap<(ScopeId, ClientId), Ipv4Addr>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.leases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }

    /// 某个作用域里已占用的地址数，用于容量展示。
    pub fn used_in(&self, scope: ScopeId, now: UnixTime) -> u64 {
        self.leases
            .values()
            .filter(|l| l.scope_id == scope && l.is_active(now))
            .count() as u64
    }
}

impl LeaseStore for MemoryStore {
    fn get_by_ip(&self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        self.leases.get(&(scope, ip)).cloned()
    }

    fn get_by_client(&self, scope: ScopeId, client: &ClientId) -> Option<Lease> {
        let ip = self.index.get(&(scope, client.clone()))?;
        self.leases.get(&(scope, *ip)).cloned()
    }

    fn insert(&mut self, lease: Lease) {
        let scope = lease.scope_id;

        // 这个客户端原先绑在别的地址上 —— 摘掉旧地址，否则它会一直占着
        if let Some(prev_ip) = self.index.get(&(scope, lease.client.clone())).copied()
            && prev_ip != lease.ip
        {
            self.leases.remove(&(scope, prev_ip));
        }

        // 这个地址原先属于别的客户端 —— 摘掉那个客户端的索引，
        // 否则它的索引会指向一条已经不属于它的租约
        if let Some(old) = self.leases.get(&(scope, lease.ip))
            && old.client != lease.client
        {
            let stale = old.client.clone();
            self.index.remove(&(scope, stale));
        }

        self.index.insert((scope, lease.client.clone()), lease.ip);
        self.leases.insert((scope, lease.ip), lease);
    }

    fn remove(&mut self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        let lease = self.leases.remove(&(scope, ip))?;
        let key = (scope, lease.client.clone());
        if self.index.get(&key) == Some(&ip) {
            self.index.remove(&key);
        }
        Some(lease)
    }

    fn all(&self) -> Vec<Lease> {
        let mut v: Vec<Lease> = self.leases.values().cloned().collect();
        v.sort_by_key(|l| (l.scope_id, l.ip));
        v
    }

    fn clear_scope(&mut self, scope: ScopeId) -> usize {
        let before = self.leases.len();
        self.leases.retain(|(s, _), _| *s != scope);
        self.index.retain(|(s, _), _| *s != scope);
        before - self.leases.len()
    }

    fn reap(&mut self, now: UnixTime) -> usize {
        let stale: Vec<(ScopeId, Ipv4Addr)> = self
            .leases
            .values()
            // Declined 要留到隔离期满，否则会被立刻重新分配出去
            .filter(|l| l.is_expired(now) && l.state != LeaseState::Declined)
            .map(|l| (l.scope_id, l.ip))
            .collect();
        for (scope, ip) in &stale {
            self.remove(*scope, *ip);
        }
        stale.len()
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

/// 按 RFC 2131 §4.3.1 的优先级为客户端挑一个地址，**并当场占下它**。
///
/// 优先级：
///
/// 1. 静态保留（管理员后加的保留应当立即生效，故优先于历史租约）
/// 2. 该客户端原有的租约（即使已过期也优先给回同一个，减少地址漂移）
/// 3. 客户端明确请求的地址（前提是可用）
/// 4. 池里下一个空闲地址
///
/// 和"先挑后写"的区别不是风格：挑与占之间的窗口在共享存储的多实例部署里
/// 会让两个实例把同一个 IP 发给两台机器。所以这里每一步候选都走
/// [`LeaseStore::try_claim`]，占不到就换下一个 —— 由存储层保证原子性。
///
/// `make_lease` 把候选地址变成要写入的租约（OFFER 阶段是短占位、
/// REQUEST 阶段是正式租约），因为两条路径的状态和到期时间不同。
pub fn allocate<S, F>(
    scope: &Scope,
    store: &mut S,
    client: &ClientId,
    requested: Option<Ipv4Addr>,
    now: UnixTime,
    mut make_lease: F,
) -> Option<Allocation>
where
    S: LeaseStore + ?Sized,
    F: FnMut(Ipv4Addr) -> Lease,
{
    let sid = scope.id;

    // 静态保留优先，且不看占用情况 —— 管理员既然把地址钉给了这个客户端，
    // 就该拿到。压在上面的别人的租约由上层（加保留时）负责清掉。
    if let Some(res) = scope.reservation_for(client) {
        let ip = res.ip;
        store.try_claim(make_lease(ip), now);
        return Some(Allocation {
            ip,
            source: AllocSource::Reservation,
        });
    }

    // 续用原有地址。仍然走 try_claim —— 它对同一个客户端总是成功，
    // 但顺带把租约刷新成新的状态和到期时间。
    if let Some(existing) = store.get_by_client(sid, client)
        && existing.state != LeaseState::Declined
        && scope.is_poolable(existing.ip)
        && !scope.is_reserved_for_other(existing.ip, client)
    {
        let ip = existing.ip;
        if store.try_claim(make_lease(ip), now) {
            return Some(Allocation {
                ip,
                source: AllocSource::Existing,
            });
        }
    }

    if let Some(want) = requested
        && scope.is_poolable(want)
        && !scope.is_reserved_for_other(want, client)
        && !store.is_occupied_elsewhere(want)
        && store.try_claim(make_lease(want), now)
    {
        return Some(Allocation {
            ip: want,
            source: AllocSource::Requested,
        });
    }

    // 池里逐个试。先收集候选再逐个占 —— poolable_addrs 借着 scope，
    // 而 try_claim 要可变借 store，两者不能同时活着。
    let candidates: Vec<Ipv4Addr> = scope
        .poolable_addrs()
        .filter(|ip| !scope.is_reserved_for_other(*ip, client) && !store.is_occupied_elsewhere(*ip))
        .collect();
    for ip in candidates {
        if store.try_claim(make_lease(ip), now) {
            return Some(Allocation {
                ip,
                source: AllocSource::Pool,
            });
        }
    }
    None
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
        let mut s = Scope::new(1, "lab", Ipv4Addr::new(192, 168, 88, 0), 24);
        s.pools = vec![Range::new(ip(10), ip(12)).unwrap()];
        s
    }

    fn bound_in(sid: ScopeId, ip_addr: Ipv4Addr, c: &ClientId, expires: UnixTime) -> Lease {
        Lease {
            ip: ip_addr,
            client: c.clone(),
            scope_id: sid,
            state: LeaseState::Bound,
            expires_at: expires,
            hostname: None,
            vendor_class: None,
            created_at: 0,
            last_seen: 0,
        }
    }

    fn bound(ip_addr: Ipv4Addr, c: &ClientId, expires: UnixTime) -> Lease {
        bound_in(ScopeId(1), ip_addr, c, expires)
    }

    /// 测试里的 allocate 包装：分配的同时写一条正式租约。
    fn alloc(
        s: &Scope,
        t: &mut MemoryStore,
        c: &ClientId,
        want: Option<Ipv4Addr>,
        now: UnixTime,
    ) -> Option<Allocation> {
        allocate(s, t, c, want, now, |addr| {
            bound_in(s.id, addr, c, now + 3600)
        })
    }

    #[test]
    fn fresh_client_gets_lowest_free() {
        let a = alloc(&scope(), &mut MemoryStore::new(), &client(1), None, 0).unwrap();
        assert_eq!(a.ip, ip(10));
        assert_eq!(a.source, AllocSource::Pool);
    }

    #[test]
    fn second_client_skips_taken_address() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 1000));
        let a = alloc(&scope(), &mut t, &client(2), None, 0).unwrap();
        assert_eq!(a.ip, ip(11));
    }

    #[test]
    fn client_keeps_its_address_across_renewals() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(12), &client(1), 1000));
        let a = alloc(&scope(), &mut t, &client(1), None, 500).unwrap();
        assert_eq!(a.ip, ip(12));
        assert_eq!(a.source, AllocSource::Existing);
    }

    #[test]
    fn expired_lease_is_still_preferred_for_same_client() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(12), &client(1), 100));
        let a = alloc(&scope(), &mut t, &client(1), None, 9999).unwrap();
        assert_eq!(a.ip, ip(12), "同一客户端回来时应拿回原地址");
    }

    #[test]
    fn expired_lease_can_be_taken_by_another_client() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 100));
        let a = alloc(&scope(), &mut t, &client(2), None, 9999).unwrap();
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
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 9999));
        let a = alloc(&s, &mut t, &client(1), None, 0).unwrap();
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
        let a = alloc(&s, &mut MemoryStore::new(), &client(2), None, 0).unwrap();
        assert_eq!(a.ip, ip(11), "保留给别人的地址要跳过");
    }

    #[test]
    fn honours_requested_address_when_free() {
        let a = alloc(
            &scope(),
            &mut MemoryStore::new(),
            &client(1),
            Some(ip(12)),
            0,
        )
        .unwrap();
        assert_eq!(a.ip, ip(12));
        assert_eq!(a.source, AllocSource::Requested);
    }

    #[test]
    fn ignores_requested_address_outside_pool() {
        let a = alloc(
            &scope(),
            &mut MemoryStore::new(),
            &client(1),
            Some(ip(99)),
            0,
        )
        .unwrap();
        assert_eq!(a.ip, ip(10), "池外的请求应被忽略并回退到池分配");
    }

    #[test]
    fn declined_address_is_skipped_until_quarantine_ends() {
        let mut t = MemoryStore::new();
        let mut l = bound(ip(10), &client(9), 0);
        l.state = LeaseState::Declined;
        l.expires_at = 3600;
        t.insert(l);
        assert_eq!(
            alloc(&scope(), &mut t, &client(1), None, 0).unwrap().ip,
            ip(11),
            "隔离期内应当跳过 .10"
        );
        // 换一个客户端来验隔离期满 —— 用 client(1) 的话它已经占着 .11 了，
        // 会走"续用原有地址"直接返回，根本试不到 .10。
        assert_eq!(
            alloc(&scope(), &mut t, &client(2), None, 3600).unwrap().ip,
            ip(10),
            "隔离期满后 .10 应当重新可用"
        );
    }

    #[test]
    fn pool_exhaustion_returns_none() {
        let mut t = MemoryStore::new();
        for (i, d) in (10u8..=12).enumerate() {
            t.insert(bound(ip(d), &client(i as u8 + 1), 9999));
        }
        assert!(alloc(&scope(), &mut t, &client(99), None, 0).is_none());
    }

    // ---------- 索引一致性 ----------

    #[test]
    fn moving_a_client_releases_its_old_address() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 9999));
        t.insert(bound(ip(11), &client(1), 9999));
        assert_eq!(t.len(), 1, "同一客户端不应留下两条租约");
        assert_eq!(t.get_by_client(ScopeId(1), &client(1)).unwrap().ip, ip(11));
        assert!(t.get_by_ip(ScopeId(1), ip(10)).is_none());
    }

    #[test]
    fn taking_over_an_address_clears_the_old_owners_index() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 100));
        t.insert(bound(ip(10), &client(2), 9999)); // 过期后被别人拿走
        assert_eq!(t.len(), 1);
        assert_eq!(t.get_by_ip(ScopeId(1), ip(10)).unwrap().client, client(2));
        assert!(
            t.get_by_client(ScopeId(1), &client(1)).is_none(),
            "旧主的索引必须清掉，否则会指向一条不属于它的租约"
        );
    }

    #[test]
    fn reap_clears_expired_but_keeps_declined() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 100));
        let mut d = bound(ip(11), &client(2), 100);
        d.state = LeaseState::Declined;
        t.insert(d);
        assert_eq!(t.reap(500), 1);
        assert!(t.get_by_ip(ScopeId(1), ip(10)).is_none());
        assert!(
            t.get_by_ip(ScopeId(1), ip(11)).is_some(),
            "Declined 要留到隔离期满"
        );
    }

    // ---------- 多作用域隔离 ----------

    #[test]
    fn same_address_in_two_scopes_is_two_leases() {
        let mut t = MemoryStore::new();
        t.insert(bound_in(ScopeId(1), ip(10), &client(1), 9999));
        t.insert(bound_in(ScopeId(2), ip(10), &client(2), 9999));
        assert_eq!(t.len(), 2, "两个隔离网段可以用同一个地址");
        assert_eq!(t.get_by_ip(ScopeId(1), ip(10)).unwrap().client, client(1));
        assert_eq!(t.get_by_ip(ScopeId(2), ip(10)).unwrap().client, client(2));
    }

    #[test]
    fn a_client_can_hold_a_lease_in_each_scope() {
        let mut t = MemoryStore::new();
        t.insert(bound_in(ScopeId(1), ip(10), &client(1), 9999));
        t.insert(bound_in(ScopeId(2), ip(11), &client(1), 9999));
        assert_eq!(t.len(), 2, "笔记本在两个网段各有一条租约是正常的");
        assert_eq!(t.get_by_client(ScopeId(1), &client(1)).unwrap().ip, ip(10));
        assert_eq!(t.get_by_client(ScopeId(2), &client(1)).unwrap().ip, ip(11));
    }

    #[test]
    fn allocation_only_sees_its_own_scope() {
        let mut t = MemoryStore::new();
        // 作用域 2 里 .10 被占，不应影响作用域 1 的分配
        t.insert(bound_in(ScopeId(2), ip(10), &client(9), 9999));
        let a = alloc(&scope(), &mut t, &client(1), None, 0).unwrap();
        assert_eq!(a.ip, ip(10));
    }

    #[test]
    fn used_in_counts_only_active_leases_of_that_scope() {
        let mut t = MemoryStore::new();
        t.insert(bound_in(ScopeId(1), ip(10), &client(1), 9999));
        t.insert(bound_in(ScopeId(1), ip(11), &client(2), 100)); // 已过期
        t.insert(bound_in(ScopeId(2), ip(10), &client(3), 9999));
        assert_eq!(t.used_in(ScopeId(1), 500), 1);
        assert_eq!(t.used_in(ScopeId(2), 500), 1);
    }

    // ---------- 原子占位契约 ----------

    #[test]
    fn try_claim_refuses_an_address_someone_else_holds() {
        // 这条契约是多实例 HA 的地基：判断"能不能用"和"占下来"必须是一步。
        // 拆成两步的话，共享存储上两个实例会同时判定可用、各自发出去。
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 9999));

        assert!(
            !t.try_claim(bound(ip(10), &client(2), 9999), 0),
            "别人持有的地址必须占不下"
        );
        assert_eq!(
            t.get_by_ip(ScopeId(1), ip(10)).unwrap().client,
            client(1),
            "占用失败不该动到原来那条租约"
        );
    }

    #[test]
    fn try_claim_succeeds_for_the_same_client_and_refreshes() {
        // 同一个客户端回来续租：应当成功，并把租约刷新成新的到期时间
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 100));

        assert!(t.try_claim(bound(ip(10), &client(1), 5000), 50));
        assert_eq!(t.get_by_ip(ScopeId(1), ip(10)).unwrap().expires_at, 5000);
    }

    #[test]
    fn try_claim_takes_over_an_expired_lease() {
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(1), 100));
        // 过期之后别人可以接手
        assert!(t.try_claim(bound(ip(10), &client(2), 9999), 200));
        assert_eq!(t.get_by_ip(ScopeId(1), ip(10)).unwrap().client, client(2));
    }

    #[test]
    fn allocation_skips_addresses_that_cannot_be_claimed() {
        // 池里前两个都被占着，分配应当一路试到第三个 —— 而不是挑中就撞车
        let mut t = MemoryStore::new();
        t.insert(bound(ip(10), &client(8), 9999));
        t.insert(bound(ip(11), &client(9), 9999));

        let a = alloc(&scope(), &mut t, &client(1), None, 0).unwrap();
        assert_eq!(a.ip, ip(12));
        assert_eq!(a.source, AllocSource::Pool);
        // 而且它真的占下了，不是只挑不占
        assert_eq!(t.get_by_ip(ScopeId(1), ip(12)).unwrap().client, client(1));
    }
}
