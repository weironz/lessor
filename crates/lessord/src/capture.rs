//! 把收到的报文原样存下来，以及事后把它们重新喂回决策层。
//!
//! **为什么需要这个。** 真机 BMC 的怪癖藏在原始字节里 —— option 61 用
//! `01+MAC` 而不是裸 MAC、厂商自己塞的私有选项、长度字段和实际内容对不上，
//! 这类事情看解码后的结构是看不出来的，因为能解码出来的已经是被我们
//! 理解过一遍的样子了。而**解不出来的那些包最有价值**，偏偏它们连结构
//! 都没有。所以捕获发生在解码之前。
//!
//! **为什么不用 pcap。** 抓包要装驱动（Windows 上是 npcap），而现场那台
//! 笔记本很可能连管理员都没有。我们自己在收包点存一份，什么都不用装 ——
//! 这和整个项目"不需要特权"的取向是一致的。
//!
//! 用法是一条闭环：现场 `--capture bmc.jsonl` 跑一遍，把文件带回来，
//! `--replay bmc.jsonl` 就能离线看我们的决策层怎么处理这些真包，
//! 每个怪癖对应一条回归测试。

use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use dhcproto::{Decodable, Decoder, v4::Message};
use serde::{Deserialize, Serialize};

/// 捕获文件里的一行。
///
/// JSONL 而不是二进制：现场的人可能需要肉眼扫一眼，也可能要用 jq 挑出
/// 某一台机器的包发给我。可读性在这里比紧凑更值钱 —— 一次上架撑死几百个包。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Captured {
    /// 收到的时刻（Unix 秒）
    pub at: u64,
    /// 来源地址:端口
    pub from: String,
    /// 收在哪个监听器上 —— 多网段时用来区分
    pub listener: String,
    /// 原始字节的十六进制
    pub hex: String,
}

/// 一个只往里追加的捕获文件。
pub struct Capture(Mutex<BufWriter<std::fs::File>>);

impl Capture {
    /// 打开（或新建）一个捕获文件，追加写。
    ///
    /// 追加而不是覆盖：现场往往要拔了插、插了拔跑好几轮，
    /// 每轮都从头写的话前面的就没了。
    pub fn open(path: &Path) -> Result<Self> {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("打不开捕获文件 {}", path.display()))?;
        Ok(Self(Mutex::new(BufWriter::new(f))))
    }

    /// 记一个包。
    ///
    /// 每条都立刻 flush。现场是 Ctrl-C 甚至直接拔电源结束的，
    /// 缓冲区里攒着的包等于没抓到 —— 而抓包这件事没有第二次机会。
    pub fn record(&self, at: u64, from: std::net::SocketAddr, listener: &str, bytes: &[u8]) {
        let line = match serde_json::to_string(&Captured {
            at,
            from: from.to_string(),
            listener: listener.to_owned(),
            hex: to_hex(bytes),
        }) {
            Ok(s) => s,
            Err(_) => return,
        };
        // 抓包失败绝不能影响发地址这件正事，所以这里所有错误都咽掉
        if let Ok(mut w) = self.0.lock() {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    // MSRV 是 1.85，is_multiple_of 要 1.87
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// 重放一条捕获记录得到的结论。
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 解码都没过去 —— 这类最值得看，说明有我们还不认识的编码方式
    Undecodable(String),
    /// 决策层处理了，这是结果的一句话描述
    Decided(String),
}

/// 把一个捕获文件重新喂给决策层，逐条给出结论。
///
/// 用真的 `handle()`，不是另写一套模拟 —— 重放的意义就在于走的是同一条路。
/// 每条记录用各自的 `at` 当"当前时间"，让租期判断和现场一致。
pub fn replay(path: &Path, scopes: Vec<lessor_core::Scope>) -> Result<Vec<(usize, Verdict)>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读不到捕获文件 {}", path.display()))?;

    let cfg = lessor_core::ServerConfig::new(scopes);
    let mut store = lessor_core::MemoryStore::default();
    let mut out = Vec::new();

    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let n = i + 1;

        let rec: Captured = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                out.push((n, Verdict::Undecodable(format!("这一行不是捕获记录：{e}"))));
                continue;
            }
        };
        let Some(bytes) = from_hex(&rec.hex) else {
            out.push((n, Verdict::Undecodable("hex 字段不是合法的十六进制".into())));
            continue;
        };

        let req = match Message::decode(&mut Decoder::new(&bytes)) {
            Ok(m) => m,
            Err(e) => {
                out.push((
                    n,
                    Verdict::Undecodable(format!("{e} —— {} 字节：{}", bytes.len(), rec.hex)),
                ));
                continue;
            }
        };

        // 监听器地址就是当时的 server_ip；解析不出来就退回一个占位地址，
        // 至少让这条记录还能跑完，而不是整个重放中断
        let server_ip = rec
            .listener
            .parse()
            .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
        let ctx = lessor_core::RecvCtx {
            now: rec.at,
            server_ip,
        };

        let verdict = match lessor_core::handle(&cfg, &mut store, &req, ctx) {
            lessor_core::Outcome::Reply(r) => format!(
                "{} → {} {}",
                crate::state::request_label(&req),
                crate::state::reply_label(&r.msg),
                r.msg.yiaddr()
            ),
            lessor_core::Outcome::Handled(note) => {
                format!("{} → {note}", crate::state::request_label(&req))
            }
            lessor_core::Outcome::Drop(why) => format!(
                "{} → 未应答（{}）",
                crate::state::request_label(&req),
                crate::state::drop_reason_text(why)
            ),
        };
        out.push((
            n,
            Verdict::Decided(format!("{} {verdict}", crate::state::client_label(&req))),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个本次测试专属的临时文件路径。
    ///
    /// 不引 tempfile：进程号加一个计数器就够唯一了，
    /// 为三个测试拖一棵依赖树不划算。
    fn temp_path() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lessor-capture-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir.join("c.jsonl")
    }

    /// 一个最小但合法的 DISCOVER。
    fn discover() -> Vec<u8> {
        let mut p = vec![1, 1, 6, 0];
        p.extend_from_slice(&0xABCD_1234u32.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0x80, 0]);
        p.extend_from_slice(&[0; 16]);
        p.extend_from_slice(&[0xac, 0x1f, 0x6b, 0x8e, 0, 1]);
        p.extend_from_slice(&[0; 10]);
        p.extend_from_slice(&[0; 192]);
        p.extend_from_slice(&[0x63, 0x82, 0x53, 0x63]);
        p.extend_from_slice(&[53, 1, 1, 0xff]);
        p
    }

    fn scope() -> lessor_core::Scope {
        crate::config::Config::from_quick(crate::config::Quick {
            server_ip: Some("192.168.88.1".parse().unwrap()),
            prefix: 24,
            pool: Some(crate::config::parse_range("192.168.88.100-192.168.88.110").unwrap()),
            router: None,
            dns: Vec::new(),
            lease_secs: 3600,
            iface: None,
            reservations: Vec::new(),
            boot: None,
            extra_options: Vec::new(),
        })
        .expect("最小配置应当合法")
        .scopes
        .remove(0)
    }

    #[test]
    fn captured_packets_replay_through_the_real_decision_layer() {
        let path = temp_path();

        let cap = Capture::open(&path).unwrap();
        cap.record(
            1_700_000_000,
            "0.0.0.0:68".parse().unwrap(),
            "192.168.88.1",
            &discover(),
        );
        drop(cap);

        let out = replay(&path, vec![scope()]).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0].1 {
            Verdict::Decided(s) => {
                assert!(s.contains("OFFER"), "应当给出 OFFER，实际是：{s}");
                assert!(s.contains("ac:1f:6b:8e:00:01"), "应当带上客户端标识：{s}");
            }
            v => panic!("应当能决策，实际是 {v:?}"),
        }
    }

    #[test]
    fn undecodable_packets_are_reported_not_skipped() {
        // 解不出来的包是这套东西存在的理由 —— 真机 BMC 的怪癖就藏在
        // 这里。悄悄跳过等于把最该看的东西丢了。
        let path = temp_path();

        let cap = Capture::open(&path).unwrap();
        cap.record(
            1_700_000_000,
            "0.0.0.0:68".parse().unwrap(),
            "192.168.88.1",
            &[1, 1, 6, 0, 0xde],
        );
        drop(cap);

        let out = replay(&path, vec![scope()]).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0].1 {
            // 原始字节必须原样带出来，否则拿到报告也没法接着查
            Verdict::Undecodable(s) => assert!(s.contains("01010600de"), "要带上原始字节：{s}"),
            v => panic!("应当报告为无法解码，实际是 {v:?}"),
        }
    }

    #[test]
    fn capture_appends_across_runs() {
        // 现场要拔了插、插了拔跑好几轮，覆盖写会把前面的丢掉
        let path = temp_path();
        for _ in 0..2 {
            let cap = Capture::open(&path).unwrap();
            cap.record(
                1_700_000_000,
                "0.0.0.0:68".parse().unwrap(),
                "192.168.88.1",
                &discover(),
            );
        }
        assert_eq!(replay(&path, vec![scope()]).unwrap().len(), 2);
    }
}
