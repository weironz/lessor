//! 现场自检：为什么"起来了但没反应"，以及什么时候可以自己退出。
//!
//! 现场最常见的故障形态是**服务一切正常，日志里一条请求都没有**。
//! 从服务端看，"防火墙把入站包吃掉了"和"网段上暂时没有客户端"长得
//! 一模一样 —— 都是安安静静。这个模块不去猜是哪一种，而是把这件事本身
//! 说出来，并按可能性排序给出该查什么。
//!
//! 不做主动探测去"确认"是防火墙：Windows 防火墙的规则查询要走 PowerShell，
//! 慢且容易受策略限制，而且查到"没有放行规则"也不等于就是它挡的
//! （出站规则、第三方安全软件、交换机端口隔离都会导致同样的现象）。
//! 与其给一个可能错的结论，不如给一份对的排查顺序。

use std::time::Duration;

use tracing::{info, warn};

use crate::state::{AppState, Counters};

/// 起来多久还一个包都没有，就该说话了。
///
/// 60 秒足够一台正在装机的机器发出第一个 DISCOVER；再短会在正常的
/// 空闲网段上误报，把人往错的方向带。
const QUIET_FIRST: Duration = Duration::from_secs(60);

/// 之后每隔多久重复一次。日志会滚，只说一次的话人接手时已经看不到了。
const QUIET_REPEAT: Duration = Duration::from_secs(600);

/// "一个请求都没收到"时该查什么，按可能性排序。
///
/// 平台不同，第一嫌疑人不同 —— Windows 上默认防火墙确实会拦掉入站 UDP 67，
/// 这是现场第一大坑；Linux 上更常见的是网卡绑错或者 nftables。
pub fn quiet_hint() -> &'static str {
    if cfg!(windows) {
        concat!(
            "按这个顺序查：\n",
            "  1. 防火墙。Windows 默认拦掉未放行程序的入站 UDP，这是现场第一大坑。\n",
            "     查：Get-NetFirewallPortFilter | ? LocalPort -eq 67\n",
            // 一行写完，不折行。PowerShell 的续行符是反引号不是反斜杠，
            // 提示里写错了的话人照抄过去只会得到一个语法错误
            "     放行（需要管理员）：New-NetFirewallRule -DisplayName lessord \
             -Direction Inbound -Protocol UDP -LocalPort 67 -Action Allow\n",
            "  2. 网卡选错了。--listen 给的地址必须是本机在目标网段上的地址。\n",
            "     查：Get-NetIPAddress -AddressFamily IPv4\n",
            "  3. 线不通或交换机做了端口隔离 —— 换台机器在同网段发个包试试。\n",
            "  4. 网段上确实还没有客户端在要地址。这也很正常。",
        )
    } else {
        concat!(
            "按这个顺序查：\n",
            "  1. 网卡绑错了。--iface / --listen 要对上目标网段。\n",
            "     查：ip -4 addr\n",
            "  2. 防火墙。查：nft list ruleset 或 iptables -L -n -v\n",
            "  3. 线不通或交换机做了端口隔离 —— 换台机器在同网段发个包试试。\n",
            "  4. 网段上确实还没有客户端在要地址。这也很正常。",
        )
    }
}

/// 一句话说清此刻的收包状况，给界面和 `/api/state` 用。
///
/// `None` 表示没什么可说的（正常在收包）。
pub fn quiet_note(state: &AppState) -> Option<String> {
    let c = &state.counters;
    if Counters::get(&c.packets) > 0 {
        return None;
    }
    let secs = crate::state::now().saturating_sub(state.started_at);
    (secs >= QUIET_FIRST.as_secs()).then(|| {
        format!("已监听 {secs} 秒，一个 DHCP 请求都没收到 —— 可能是防火墙拦掉了入站包，也可能网段上暂时没有客户端")
    })
}

/// 盯着"起来了但没收到任何请求"这件事。
pub async fn watch_quiet(state: AppState) {
    tokio::time::sleep(QUIET_FIRST).await;

    loop {
        if Counters::get(&state.counters.packets) > 0 {
            // 收到过就再也不用管了 —— 通路是通的，之后的安静是网段的事
            return;
        }
        let secs = crate::state::now().saturating_sub(state.started_at);
        warn!(
            listening_secs = secs,
            "监听中，但一个 DHCP 请求都没收到。\n{}",
            quiet_hint()
        );
        // 这里刻意用 sleep 而不是 interval：interval 的第一拍是立刻返回的，
        // 会让这条告警在同一毫秒内连打两遍
        tokio::time::sleep(QUIET_REPEAT).await;
    }
}

/// 闲够了就自己退出。返回即表示该退了。
///
/// 给现场临时使用：装完机走人，不用记得回来关掉它。常驻部署不该开 ——
/// 常驻服务本来就该一直在，没人要地址不代表它该消失。
pub async fn wait_until_idle(state: AppState, idle: Duration) {
    // 一秒一查足够了，这个判断本身没有任何代价
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;

        let last = Counters::get(&state.counters.last_packet_at);
        // 一个包都还没收到时，从启动时刻开始算 —— 否则"起来就没人理"
        // 这种情况会永远等下去，而那恰恰是最该自己退出的情况
        let since =
            crate::state::now().saturating_sub(if last == 0 { state.started_at } else { last });

        if since >= idle.as_secs() {
            info!(
                idle_secs = since,
                had_traffic = last != 0,
                "空闲超时，按 --idle-exit 自行退出"
            );
            return;
        }
    }
}

/// 收到中断信号。第二次按下就不再等了 —— 现场按 Ctrl-C 没反应的时候，
/// 人只会再按一次，那时它必须真的立刻死掉。
pub async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        // 装不上处理器就永远不返回，让别的退出路径接管
        std::future::pending::<()>().await;
    }
    info!("收到中断，正在退出 —— 再按一次 Ctrl-C 立即结束");

    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("再次收到中断，立即结束。");
            std::process::exit(130); // 128 + SIGINT
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hint_names_the_platforms_first_suspect() {
        let h = quiet_hint();
        // 排查顺序是这段文字的全部价值 —— 第一条必须是本平台最可能的原因
        if cfg!(windows) {
            assert!(h.contains("防火墙"), "Windows 上第一嫌疑人是防火墙");
            assert!(
                h.find("防火墙").unwrap() < h.find("网卡选错").unwrap(),
                "防火墙要排在网卡之前"
            );
        } else {
            assert!(h.contains("ip -4 addr"));
        }
        // 给了命令才叫可操作，光说"检查防火墙"等于没说
        assert!(h.contains("查："));
    }

    /// 一个不带作用域的最小状态，只用来驱动计时逻辑。
    fn bare_state() -> AppState {
        AppState::new(crate::config::Config {
            listeners: Vec::new(),
            scopes: Vec::new(),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn idle_exit_counts_from_startup_when_nothing_ever_arrived() {
        // "起来就没人理"恰恰是最该自己退出的情况。从"最后一个包"算的话
        // 这种情况永远等不到退出，因为压根没有最后一个包。
        let mut st = bare_state();
        st.started_at -= 100;
        tokio::time::timeout(
            Duration::from_secs(5),
            wait_until_idle(st, Duration::from_secs(10)),
        )
        .await
        .expect("闲置早已超时，应当立刻返回");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_exit_waits_while_traffic_is_recent() {
        // 刚收过包就不能退 —— 否则一批机器装到一半服务自己走了
        let st = bare_state();
        st.counters
            .last_packet_at
            .store(crate::state::now(), std::sync::atomic::Ordering::Relaxed);
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            wait_until_idle(st, Duration::from_secs(3600)),
        )
        .await;
        assert!(r.is_err(), "流量还新鲜时不该退出");
    }

    #[test]
    fn quiet_hint_admits_it_might_be_nothing() {
        // 不能把"网段上还没有客户端"这条正常情况漏掉，
        // 否则会让人去追一个根本不存在的故障
        assert!(quiet_hint().contains("这也很正常"));
    }
}
