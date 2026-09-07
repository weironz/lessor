//! 已知作用域的低噪音设备刷新。
//!
//! 这里绝不执行逐 IP 扫描：那是用户明确发起的“发现”操作。后台只做
//! RMCP 广播和邻居表读取，既能更新 BMC 的最近发现时间，也不会在多个
//! /24 作用域上持续制造数百个探测包。

use std::time::Duration;

use crate::state::AppState;

const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub async fn run(state: AppState) {
    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;
        for (scope_id, target) in state.discovery_targets().await {
            let mut options = discovery::Options::new(target);
            options.sweep = false;
            options.wait = Duration::from_millis(800);
            state
                .record_discovery(scope_id, discovery::scan(options).await)
                .await;
        }
    }
}
