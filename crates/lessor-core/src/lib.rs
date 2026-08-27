//! DHCPv4 服务端核心逻辑。
//!
//! 这个 crate 不做 IO、不用 async、不读时钟 —— 所有时间由调用方传入。
//! 目的是让分配策略和租约状态机可以被确定性地测试，并在三个平台上共用同一份实现。

pub mod addr;
pub mod lease;
pub mod scope;
pub mod server;
pub mod store;

pub use addr::{ClientId, MacAddr, ParseMacError, Range};
pub use lease::{Lease, LeaseState, UnixTime};
pub use scope::{BootConfig, Reservation, Scope, ScopeError, ScopeId};
pub use server::{DropReason, Outcome, RecvCtx, Reply, ReplyDest, ServerConfig, handle};
pub use store::{AllocSource, Allocation, LeaseStore, MemoryStore, allocate};
