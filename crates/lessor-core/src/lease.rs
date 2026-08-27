//! 租约与其状态机。

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addr::ClientId;

/// Unix 时间戳（秒）。核心逻辑不读时钟 —— 时间一律由调用方传入，
/// 这样状态机可以被确定性地测试。
pub type UnixTime = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LeaseState {
    /// 已 OFFER，等客户端 REQUEST。短暂占位，避免并发分配同一地址。
    Offered,
    /// 已 ACK，客户端正在使用。
    Bound,
    /// 客户端 RELEASE 或租约过期，地址可回收。
    Free,
    /// 客户端 DECLINE —— 说明地址在网上已被别人占用，需要隔离一段时间。
    Declined,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lease {
    pub ip: Ipv4Addr,
    pub client: ClientId,
    pub state: LeaseState,
    /// 到期时间。`Offered` 用短超时，`Bound` 用作用域配置的租期。
    pub expires_at: UnixTime,
    pub hostname: Option<String>,
    /// 首次分配时间，仅用于展示。
    pub created_at: UnixTime,
}

impl Lease {
    pub fn is_expired(&self, now: UnixTime) -> bool {
        now >= self.expires_at
    }

    /// 该地址此刻是否可以分配给**别的**客户端。
    pub fn is_available_for_others(&self, now: UnixTime) -> bool {
        match self.state {
            LeaseState::Free => true,
            // Declined 的地址即使"过期"也不能立刻复用，隔离期由调用方通过
            // expires_at 控制。
            LeaseState::Declined => self.is_expired(now),
            LeaseState::Offered | LeaseState::Bound => self.is_expired(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addr::MacAddr;

    fn lease(state: LeaseState, expires_at: UnixTime) -> Lease {
        Lease {
            ip: Ipv4Addr::new(10, 0, 0, 5),
            client: ClientId::Mac(MacAddr([0, 1, 2, 3, 4, 5])),
            state,
            expires_at,
            hostname: None,
            created_at: 0,
        }
    }

    #[test]
    fn bound_lease_blocks_others_until_expiry() {
        let l = lease(LeaseState::Bound, 100);
        assert!(!l.is_available_for_others(99));
        assert!(l.is_available_for_others(100));
    }

    #[test]
    fn free_lease_is_immediately_reusable() {
        assert!(lease(LeaseState::Free, u64::MAX).is_available_for_others(0));
    }

    #[test]
    fn declined_lease_is_quarantined() {
        let l = lease(LeaseState::Declined, 3600);
        assert!(!l.is_available_for_others(0), "隔离期内不能复用");
        assert!(l.is_available_for_others(3600), "隔离期满可复用");
    }
}
