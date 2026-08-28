//! sqlite 租约存储。
//!
//! 常驻形态默认开；现场形态默认仍是内存（关了就走，不留痕）。
//!
//! **设计要点：写透，不做内存缓存。** 缓存会让 `try_claim` 的原子性失效 ——
//! 那正是 v1.0 多实例共享 PostgreSQL 的地基。读路径靠 sqlite 自己的页缓存，
//! 现场几十到几千条租约的规模，这点开销无所谓；把正确性换性能不划算。

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use lessor_core::lease::{Lease, LeaseState, UnixTime};
use lessor_core::{ClientId, LeaseStore, ScopeId};
use rusqlite::{Connection, OptionalExtension, params};

/// `rusqlite::Connection` 内部用 `RefCell`，不是 `Sync`；而 axum 的
/// handler 要求跨 await 的状态是 `Send`。用 `Mutex` 包起来 ——
/// 同时也正好把并发写串行化，这是 `try_claim` 的原子性在单进程内的保证。
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// 打开（必要时创建）一个租约库。
    ///
    /// 损坏的文件在这里就会报错退出，而不是被静默清空 —— 悄悄丢掉整个
    /// 机房的租约比起不来更糟。
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("打不开租约库 {}", path.display()))?;

        // WAL：读写不互相阻塞。DHCP 的写很碎（每次握手两次），
        // 而界面在不停地读。
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("租约库无法启用 WAL —— 文件可能已损坏或不可写")?;
        // NORMAL 在 WAL 下已经能扛住进程崩溃，只有掉电才可能丢最后几条。
        // 对租约这个数据来说，拿它换掉每次握手一次 fsync 是划算的。
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS leases (
                 scope_id     INTEGER NOT NULL,
                 ip           TEXT    NOT NULL,
                 client       TEXT    NOT NULL,
                 state        TEXT    NOT NULL,
                 expires_at   INTEGER NOT NULL,
                 hostname     TEXT,
                 vendor_class TEXT,
                 created_at   INTEGER NOT NULL,
                 last_seen    INTEGER NOT NULL,
                 PRIMARY KEY (scope_id, ip)
             );
             -- 按客户端反查要走索引，否则每次分配都全表扫
             CREATE INDEX IF NOT EXISTS leases_by_client
                 ON leases (scope_id, client);",
        )
        .context("租约库结构不对 —— 可能是别的程序的文件")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 取连接。锁中毒说明别的线程带着锁 panic 了 —— 继续用是安全的，
    /// sqlite 自己的事务不会因此半途而废。
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn row_to_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
        let ip: String = row.get("ip")?;
        let client: String = row.get("client")?;
        let state: String = row.get("state")?;
        Ok(Lease {
            ip: ip.parse().unwrap_or(Ipv4Addr::UNSPECIFIED),
            client: parse_client(&client),
            scope_id: ScopeId(row.get::<_, i64>("scope_id")? as u32),
            state: parse_state(&state),
            expires_at: row.get::<_, i64>("expires_at")? as UnixTime,
            hostname: row.get("hostname")?,
            vendor_class: row.get("vendor_class")?,
            created_at: row.get::<_, i64>("created_at")? as UnixTime,
            last_seen: row.get::<_, i64>("last_seen")? as UnixTime,
        })
    }
}

/// 客户端标识的文本形式。`ClientId` 的 `Display`/`FromStr` 已经是稳定的
/// 往返形式（MAC 或 `opt61:` 前缀的十六进制），直接复用。
fn client_text(c: &ClientId) -> String {
    c.to_string()
}

fn parse_client(s: &str) -> ClientId {
    if let Some(hex) = s.strip_prefix("opt61:") {
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect();
        if let Some(b) = bytes.filter(|b| !b.is_empty()) {
            return ClientId::Opt61(b);
        }
    }
    s.parse()
        .map(ClientId::Mac)
        // 存进去的都是我们自己写的，解不出来说明文件被外部改过。
        // 退回一个原样保留的标识，至少不丢数据。
        .unwrap_or_else(|_| ClientId::Opt61(s.as_bytes().to_vec()))
}

fn state_text(s: LeaseState) -> &'static str {
    match s {
        LeaseState::Offered => "offered",
        LeaseState::Bound => "bound",
        LeaseState::Free => "free",
        LeaseState::Declined => "declined",
    }
}

fn parse_state(s: &str) -> LeaseState {
    match s {
        "offered" => LeaseState::Offered,
        "free" => LeaseState::Free,
        "declined" => LeaseState::Declined,
        _ => LeaseState::Bound,
    }
}

impl LeaseStore for SqliteStore {
    fn get_by_ip(&self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        self.lock()
            .query_row(
                "SELECT * FROM leases WHERE scope_id = ?1 AND ip = ?2",
                params![scope.0 as i64, ip.to_string()],
                Self::row_to_lease,
            )
            .optional()
            .ok()
            .flatten()
    }

    fn get_by_client(&self, scope: ScopeId, client: &ClientId) -> Option<Lease> {
        self.lock()
            .query_row(
                "SELECT * FROM leases WHERE scope_id = ?1 AND client = ?2",
                params![scope.0 as i64, client_text(client)],
                Self::row_to_lease,
            )
            .optional()
            .ok()
            .flatten()
    }

    fn insert(&mut self, lease: Lease) {
        let _ = self.lock().execute(
            "INSERT INTO leases
                 (scope_id, ip, client, state, expires_at, hostname, vendor_class, created_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (scope_id, ip) DO UPDATE SET
                 client = excluded.client,
                 state = excluded.state,
                 expires_at = excluded.expires_at,
                 hostname = excluded.hostname,
                 vendor_class = excluded.vendor_class,
                 created_at = excluded.created_at,
                 last_seen = excluded.last_seen",
            params![
                lease.scope_id.0 as i64,
                lease.ip.to_string(),
                client_text(&lease.client),
                state_text(lease.state),
                lease.expires_at as i64,
                lease.hostname,
                lease.vendor_class,
                lease.created_at as i64,
                lease.last_seen as i64,
            ],
        );
    }

    /// **一条带条件的原子写** —— 这是整个后端存在的理由。
    ///
    /// 条件全部下推到 SQL 的 `WHERE`：没有现存行、或那行是同一个客户端、
    /// 或它已经过期且不在 DECLINE 隔离期内，才允许覆盖。判断与写入在
    /// 同一条语句里，两个实例并发执行时由数据库串行化，不会同时成功。
    fn try_claim(&mut self, lease: Lease, now: UnixTime) -> bool {
        let client = client_text(&lease.client);
        let n = self.lock().execute(
                "INSERT INTO leases
                     (scope_id, ip, client, state, expires_at, hostname, vendor_class, created_at, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (scope_id, ip) DO UPDATE SET
                     client = excluded.client,
                     state = excluded.state,
                     expires_at = excluded.expires_at,
                     hostname = excluded.hostname,
                     vendor_class = excluded.vendor_class,
                     created_at = excluded.created_at,
                     last_seen = excluded.last_seen
                 WHERE leases.client = excluded.client
                    OR leases.expires_at <= ?10",
                params![
                    lease.scope_id.0 as i64,
                    lease.ip.to_string(),
                    client,
                    state_text(lease.state),
                    lease.expires_at as i64,
                    lease.hostname,
                    lease.vendor_class,
                    lease.created_at as i64,
                    lease.last_seen as i64,
                    now as i64,
                ],
            )
            .unwrap_or(0);
        n > 0
    }

    fn remove(&mut self, scope: ScopeId, ip: Ipv4Addr) -> Option<Lease> {
        let existed = self
            .lock()
            .query_row(
                "SELECT * FROM leases WHERE scope_id = ?1 AND ip = ?2",
                params![scope.0 as i64, ip.to_string()],
                Self::row_to_lease,
            )
            .optional()
            .ok()
            .flatten()?;
        let _ = self.lock().execute(
            "DELETE FROM leases WHERE scope_id = ?1 AND ip = ?2",
            params![scope.0 as i64, ip.to_string()],
        );
        Some(existed)
    }

    fn all(&self) -> Vec<Lease> {
        let conn = self.lock();
        let Ok(mut stmt) = conn.prepare("SELECT * FROM leases ORDER BY scope_id, ip") else {
            return Vec::new();
        };
        stmt.query_map([], Self::row_to_lease)
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    fn reap(&mut self, now: UnixTime) -> usize {
        // DECLINE 的记录留到隔离期满，否则刚被拒的地址会立刻被重新发出去
        self.lock()
            .execute(
                "DELETE FROM leases WHERE expires_at <= ?1",
                params![now as i64],
            )
            .unwrap_or(0)
    }

    fn clear_scope(&mut self, scope: ScopeId) -> usize {
        self.lock()
            .execute(
                "DELETE FROM leases WHERE scope_id = ?1",
                params![scope.0 as i64],
            )
            .unwrap_or(0)
    }

    fn usable_by(&self, scope: ScopeId, ip: Ipv4Addr, client: &ClientId, now: UnixTime) -> bool {
        self.lock()
            .query_row(
                "SELECT client, state, expires_at FROM leases
                 WHERE scope_id = ?1 AND ip = ?2",
                params![scope.0 as i64, ip.to_string()],
                |row| {
                    let holder: String = row.get(0)?;
                    let state: String = row.get(1)?;
                    let expires: i64 = row.get(2)?;
                    Ok(if holder == client_text(client) {
                        parse_state(&state) != LeaseState::Declined
                    } else {
                        expires as UnixTime <= now
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
            // 没有记录就是空闲的
            .unwrap_or(true)
    }
}

impl SqliteStore {
    /// 库里现有多少条租约。启动时报一句，让人知道恢复了什么。
    pub fn count(&self) -> usize {
        self.lock()
            .query_row("SELECT COUNT(*) FROM leases", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    /// 该作用域里此刻还占着地址的条数。
    pub fn used_in(&self, scope: ScopeId, now: UnixTime) -> u64 {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM leases
                 WHERE scope_id = ?1 AND expires_at > ?2 AND state != 'free'",
                params![scope.0 as i64, now as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64
    }
}
