//! 租约与其状态机。

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addr::ClientId;
use crate::scope::ScopeId;

/// Unix 时间戳（秒）。核心逻辑不读时钟 —— 时间一律由调用方传入，
/// 这样状态机可以被确定性地测试。
pub type UnixTime = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub ip: Ipv4Addr,
    pub client: ClientId,
    /// 归属的作用域。多网卡场景下同一个 IP 可能在不同作用域里有不同含义，
    /// 租约必须记住自己是谁发的。
    pub scope_id: ScopeId,
    pub state: LeaseState,
    /// 到期时间。`Offered` 用短超时，`Bound` 用作用域配置的租期。
    pub expires_at: UnixTime,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hostname: Option<String>,
    /// option 60（vendor class identifier）。PXE 客户端会填
    /// `PXEClient:Arch:00007:...`，据此可以区分固件与操作系统阶段。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vendor_class: Option<String>,
    /// 首次分配时间，仅用于展示。
    pub created_at: UnixTime,
    /// 最后一次收到该客户端报文的时刻 —— 界面上用来判断"还活着吗"。
    pub last_seen: UnixTime,
}

impl Lease {
    pub fn is_expired(&self, now: UnixTime) -> bool {
        now >= self.expires_at
    }

    /// 正在被客户端使用（已分配且未过期）。
    pub fn is_active(&self, now: UnixTime) -> bool {
        matches!(self.state, LeaseState::Offered | LeaseState::Bound) && !self.is_expired(now)
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

    /// 是否是 PXE 客户端 —— 用于把引导选项只发给需要的机器。
    pub fn is_pxe(&self) -> bool {
        self.vendor_class
            .as_deref()
            .is_some_and(|v| v.starts_with("PXEClient"))
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
            scope_id: ScopeId(1),
            state,
            expires_at,
            hostname: None,
            vendor_class: None,
            created_at: 0,
            last_seen: 0,
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

    #[test]
    fn is_active_tracks_state_and_expiry() {
        assert!(lease(LeaseState::Bound, 100).is_active(50));
        assert!(!lease(LeaseState::Bound, 100).is_active(100));
        assert!(!lease(LeaseState::Free, u64::MAX).is_active(0));
        assert!(!lease(LeaseState::Declined, u64::MAX).is_active(0));
    }

    #[test]
    fn pxe_clients_are_recognised_by_vendor_class() {
        let mut l = lease(LeaseState::Bound, 100);
        assert!(!l.is_pxe());
        l.vendor_class = Some("PXEClient:Arch:00007:UNDI:003016".into());
        assert!(l.is_pxe());
        l.vendor_class = Some("udhcp 1.36".into());
        assert!(!l.is_pxe());
    }

    #[test]
    fn lease_roundtrips_through_json() {
        let mut l = lease(LeaseState::Bound, 100);
        l.hostname = Some("bmc-01".into());
        l.vendor_class = Some("PXEClient:Arch:00007".into());
        let j = serde_json::to_string(&l).unwrap();
        assert!(j.contains(r#""00:01:02:03:04:05""#), "MAC 应是可读字符串");
        assert!(j.contains(r#""state":"bound""#));
        assert!(j.contains(r#""scopeId""#), "字段名应为 camelCase");
        assert_eq!(serde_json::from_str::<Lease>(&j).unwrap(), l);
    }
}
