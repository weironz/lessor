//! 现场装机的引导文件服务：内置只读 TFTP 与 HTTP 静态文件。
//!
//! lessor 原本只负责用 DHCP 告诉客户端"去哪儿拿引导文件"，文件本身得由
//! 外部的 MAAS / dnsmasq / Python 脚本提供。现场形态是"带一台笔记本、
//! 插上网线就开机装"，再要求另搭一套文件服务器就不成立。
//!
//! 边界：**只做只读分发**，目录里有什么发什么。不生成 GRUB 菜单、不管
//! 内核 initrd 从哪来、不做装机内容层 —— 那仍归 MAAS。
//!
//! 两个服务刻意与管理接口分开监听：管理接口默认只听本机且写操作有守卫，
//! 而引导文件必须能被网段上的固件取到，是公开的只读路径。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tokio::net::{TcpListener, UdpSocket};
use tracing::{info, warn};

/// `--boot-dir` 这一组服务的启动参数。
#[derive(Clone, Debug)]
pub struct BootServe {
    pub root: PathBuf,
    pub http: SocketAddr,
    pub tftp_bind: Ipv4Addr,
    pub tftp_port: u16,
    /// DHCP 监听器地址，也就是下发给客户端的 next-server。
    pub next_server: Ipv4Addr,
}

// ---------------------------------------------------------------- 引导目标

/// 目录里认得的几种引导文件。**只有确实存在的文件才会被下发** ——
/// 把固件指向一个不存在的文件，它只会一直等到超时，比不下发更难排查。
const IPXE_EFI: &str = "ipxe.efi";
const UNDI: &str = "undionly.kpxe";
const SHIM: &str = "bootx64.efi";
const IPXE_SCRIPT: &str = "boot.ipxe";

#[derive(Clone, Debug)]
pub struct BootPlan {
    pub boot: lessor_core::BootConfig,
    /// 目录里认出来的文件，启动日志里列给人看。
    pub found: Vec<&'static str>,
}

/// 按目录内容推导默认引导目标。
///
/// `advertise` 是客户端真正能访问到的 HTTP 地址（监听地址可能是
/// 0.0.0.0，那样不能写进 URL）；`next_server` 是 TFTP / siaddr 的地址。
pub fn plan(root: &Path, advertise: SocketAddr, next_server: Ipv4Addr) -> BootPlan {
    let has = |n: &str| root.join(n).is_file();
    let mut found = Vec::new();
    for n in [IPXE_EFI, UNDI, SHIM, IPXE_SCRIPT] {
        if has(n) {
            found.push(n);
        }
    }

    let url = |n: &str| format!("http://{advertise}/{n}");
    let filename = if has(IPXE_EFI) {
        Some(IPXE_EFI)
    } else if has(UNDI) {
        Some(UNDI)
    } else if has(SHIM) {
        Some(SHIM)
    } else {
        None
    };

    BootPlan {
        boot: lessor_core::BootConfig {
            server_name: Some(next_server.to_string()),
            filename: filename.map(str::to_owned),
            next_server: Some(next_server),
            http_url: has(SHIM).then(|| url(SHIM)),
            ipxe_url: has(IPXE_SCRIPT).then(|| url(IPXE_SCRIPT)),
        },
        found,
    }
}

/// 把默认引导参数补进还没配引导的作用域。已经显式配过的原样不动 ——
/// `--config` 里手写的目标优先于自动推导。
pub fn fill_defaults(scopes: &mut [lessor_core::Scope], boot: &lessor_core::BootConfig) -> usize {
    let mut filled = 0;
    for s in scopes {
        if s.boot.is_none() {
            s.boot = Some(boot.clone());
            filled += 1;
        }
    }
    filled
}

/// 监听地址是 0.0.0.0 / :: 时，URL 里得换成客户端够得着的那个地址。
pub fn advertised_addr(http: SocketAddr, next_server: Ipv4Addr) -> SocketAddr {
    match http.ip() {
        IpAddr::V4(v4) if !v4.is_unspecified() => http,
        _ => SocketAddr::new(next_server.into(), http.port()),
    }
}

// ---------------------------------------------------------------- HTTP

#[derive(Clone)]
struct HttpState {
    root: Arc<PathBuf>,
}

/// 只有静态文件，没有 /api、没有写操作、没有守卫。
pub fn http_router(root: Arc<PathBuf>) -> Router {
    Router::new()
        .fallback(serve_file)
        .with_state(HttpState { root })
}

/// 把 URL 路径转成目录内的相对路径。拒绝 `..`、绝对路径、反斜杠
/// （Windows 上 `..\` 也是穿越），空路径不给索引。
fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." || seg.contains('\\') || seg.contains('\0') {
            return None;
        }
        out.push(seg);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// 解析到真实路径，并保证它仍在根目录之下 —— 光挡住 `..` 挡不住符号链接。
async fn resolve(root: &Path, rel: PathBuf) -> Option<PathBuf> {
    let root_canon = tokio::fs::canonicalize(root).await.ok()?;
    let file_canon = tokio::fs::canonicalize(root.join(rel)).await.ok()?;
    file_canon.starts_with(&root_canon).then_some(file_canon)
}

fn mime_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "ipxe" | "cfg" | "conf" | "txt" | "json" | "yaml" | "yml" | "sh" => {
            "text/plain; charset=utf-8"
        }
        _ => "application/octet-stream",
    }
}

async fn serve_file(State(st): State<HttpState>, uri: Uri) -> Response {
    let Some(rel) = safe_relative(uri.path()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Some(path) = resolve(&st.root, rel).await else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if !path.is_file() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    ([(header::CONTENT_TYPE, mime_for(&path))], bytes).into_response()
}

pub async fn serve_http(addr: SocketAddr, root: Arc<PathBuf>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("引导文件 HTTP 监听 {addr} 失败"))?;
    info!(addr = %addr, "引导文件 HTTP 服务已就绪（只读，与管理接口分开）");
    axum::serve(listener, http_router(root))
        .await
        .context("引导文件 HTTP 服务异常退出")
}

// ---------------------------------------------------------------- TFTP

const RRQ: u16 = 1;
const DATA: u16 = 3;
const ACK: u16 = 4;
const ERROR: u16 = 5;
const OACK: u16 = 6;

/// TFTP 只实现读（RRQ）—— 装机不需要客户端往服务器写任何东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub filename: String,
    pub mode: String,
    /// 协商后的实际块大小（默认 512）
    pub blksize: usize,
    /// 客户端有没有请求 blksize。即便它请求的正好是 512，也得回 OACK 确认
    pub blksize_requested: bool,
    pub tsize: bool,
}

impl ReadRequest {
    fn needs_oack(&self) -> bool {
        self.blksize_requested || self.tsize
    }
}

/// 解析 RRQ。纯函数，报文怎么奇怪都能单独测。
pub fn parse_rrq(pkt: &[u8]) -> Result<ReadRequest, String> {
    if pkt.len() < 6 {
        return Err("报文太短".into());
    }
    if u16::from_be_bytes([pkt[0], pkt[1]]) != RRQ {
        return Err("不是 RRQ（只支持读）".into());
    }
    let fields: Vec<&[u8]> = pkt[2..].split(|b| *b == 0).collect();
    // 合法的 RRQ 以 NUL 结尾，所以最后一段必须是空的
    if fields.len() < 3 || !fields.last().is_some_and(|f| f.is_empty()) {
        return Err("RRQ 字段没有以 NUL 结尾".into());
    }
    let strings: Vec<String> = fields[..fields.len() - 1]
        .iter()
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    let filename = strings[0].clone();
    let mode = strings.get(1).cloned().unwrap_or_default();
    if !mode.eq_ignore_ascii_case("octet") {
        return Err(format!("只支持 octet 模式，收到 {mode}"));
    }

    let mut blksize = 512usize;
    let mut blksize_requested = false;
    let mut tsize = false;
    for pair in strings[2..].chunks(2) {
        if pair.len() < 2 {
            break;
        }
        match pair[0].to_ascii_lowercase().as_str() {
            "blksize" => {
                let n: usize = pair[1]
                    .parse()
                    .map_err(|_| format!("blksize 不合法：{}", pair[1]))?;
                // 上限 8192：再大容易触发 IP 分片，固件侧反而更容易丢
                blksize = n.clamp(8, 8192);
                blksize_requested = true;
            }
            "tsize" => tsize = true,
            // 不认识的选项忽略即可 —— 不协商它，客户端会按默认值处理
            _ => {}
        }
    }
    Ok(ReadRequest {
        filename,
        mode,
        blksize,
        blksize_requested,
        tsize,
    })
}

/// TFTP 文件名可以带子目录（`efi/boot/bootx64.efi`），但不能越出根目录。
pub fn safe_name(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return None;
    }
    let p = Path::new(name);
    if p.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn data_pkt(block: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + payload.len());
    v.extend_from_slice(&DATA.to_be_bytes());
    v.extend_from_slice(&block.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// 只有测试需要构造 ACK；服务端方向是解析 ACK，不是发。
#[cfg(test)]
fn ack_pkt(block: u16) -> Vec<u8> {
    [ACK.to_be_bytes(), block.to_be_bytes()].concat()
}

/// OACK 只回客户端**请求过的**选项 —— RFC 2347 不允许服务端凭空加一个，
/// 有些固件收到没要过的选项会直接放弃这次传输。
fn oack_pkt(req: &ReadRequest, size: usize) -> Vec<u8> {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if req.blksize_requested {
        pairs.push(("blksize", req.blksize.to_string()));
    }
    if req.tsize {
        pairs.push(("tsize", size.to_string()));
    }
    let mut v = OACK.to_be_bytes().to_vec();
    for (k, val) in pairs {
        v.extend_from_slice(k.as_bytes());
        v.push(0);
        v.extend_from_slice(val.as_bytes());
        v.push(0);
    }
    v
}

fn error_pkt(code: u16, msg: &str) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&ERROR.to_be_bytes());
    v.extend_from_slice(&code.to_be_bytes());
    v.extend_from_slice(msg.as_bytes());
    v.push(0);
    v
}

fn ack_block(pkt: &[u8]) -> Option<u16> {
    (pkt.len() >= 4 && u16::from_be_bytes([pkt[0], pkt[1]]) == ACK)
        .then(|| u16::from_be_bytes([pkt[2], pkt[3]]))
}

/// 发一个包、等对应块号的 ACK。超时重传三次 —— 现场网线上丢包是常态。
async fn send_await_ack(sock: &UdpSocket, pkt: &[u8], expect: u16, peer: SocketAddr) -> bool {
    for _ in 0..3 {
        if sock.send_to(pkt, peer).await.is_err() {
            return false;
        }
        let mut buf = [0u8; 512];
        match tokio::time::timeout(Duration::from_secs(3), sock.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) if from == peer && ack_block(&buf[..n]) == Some(expect) => {
                return true;
            }
            // 重复 / 乱序的 ACK：再发一次当前块，让客户端自己对齐
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(_)) => return false,
        }
    }
    false
}

/// 一次传输。每个 RRQ 用一个新的 TID socket —— TFTP 就是这么设计的，
/// 主 socket 始终留在 69 上接新请求。
async fn transfer(root: Arc<PathBuf>, bind: Ipv4Addr, req: ReadRequest, peer: SocketAddr) {
    let Ok(sock) = UdpSocket::bind((bind, 0)).await else {
        warn!(error = "无法创建传输 socket", "TFTP 传输中止");
        return;
    };
    let Some(rel) = safe_name(&req.filename) else {
        let _ = sock.send_to(&error_pkt(2, "文件名不合法"), peer).await;
        return;
    };
    let Some(path) = resolve(&root, rel).await else {
        let _ = sock.send_to(&error_pkt(1, "not found"), peer).await;
        warn!(file = %req.filename, "TFTP 找不到文件");
        return;
    };
    let Ok(data) = tokio::fs::read(&path).await else {
        let _ = sock.send_to(&error_pkt(2, "读不到文件"), peer).await;
        return;
    };

    // 协商过的选项必须先收到 ACK(0)，否则固件会把第一个 DATA 当成 OACK 的应答
    if req.needs_oack() && !send_await_ack(&sock, &oack_pkt(&req, data.len()), 0, peer).await {
        warn!(file = %req.filename, "TFTP 选项协商没有应答");
        return;
    }

    // 空文件也要发一个空 DATA，否则客户端会一直等
    let blocks = data.len().div_ceil(req.blksize).max(1);
    for i in 0..blocks {
        let start = i * req.blksize;
        let end = (start + req.blksize).min(data.len());
        // 块号是 16 位，超过 32 MB 的文件会回绕 —— 内核/initrd 常有这么大
        let block = ((i + 1) % 65536) as u16;
        if !send_await_ack(&sock, &data_pkt(block, &data[start..end]), block, peer).await {
            warn!(file = %req.filename, block, "TFTP 传输中断（ACK 超时）");
            return;
        }
    }
    info!(file = %req.filename, bytes = data.len(), blksize = req.blksize, "TFTP 发送完成");
}

pub async fn serve_tftp(bind: Ipv4Addr, port: u16, root: Arc<PathBuf>) -> Result<()> {
    let sock = UdpSocket::bind((bind, port)).await.with_context(|| {
        format!(
            "TFTP 监听 {bind}:{port} 失败 —— Linux 上 69 是特权端口，\
             给二进制 setcap cap_net_bind_service+ep，或用 --tftp-port 换高位端口"
        )
    })?;
    info!(addr = %format!("{bind}:{port}"), "TFTP 服务已就绪（只读 RRQ）");

    let mut buf = vec![0u8; 2048];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await.context("TFTP 收包失败")?;
        let pkt = buf[..n].to_vec();
        match parse_rrq(&pkt) {
            Ok(req) => {
                let r = root.clone();
                tokio::spawn(async move { transfer(r, bind, req, peer).await });
            }
            Err(e) => {
                warn!(error = %e, from = %peer, "忽略一个无法解析的 TFTP 报文");
                let _ = sock.send_to(&error_pkt(4, "非法 TFTP 请求"), peer).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rrq(fields: &[&str]) -> Vec<u8> {
        let mut v = RRQ.to_be_bytes().to_vec();
        for f in fields {
            v.extend_from_slice(f.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn parses_a_plain_rrq() {
        let req = parse_rrq(&rrq(&["bootx64.efi", "octet"])).unwrap();
        assert_eq!(req.filename, "bootx64.efi");
        assert_eq!(req.blksize, 512);
        assert!(!req.blksize_requested);
        assert!(!req.tsize);
    }

    #[test]
    fn parses_option_negotiation_and_clamps_blksize() {
        let req = parse_rrq(&rrq(&[
            "ipxe.efi", "octet", "blksize", "1468", "tsize", "0",
        ]))
        .unwrap();
        assert_eq!(req.blksize, 1468);
        assert!(req.blksize_requested);
        assert!(req.tsize);

        // 客户端可能要一个会触发 IP 分片的块大小，服务端说了算
        assert_eq!(
            parse_rrq(&rrq(&["x", "octet", "blksize", "65535"]))
                .unwrap()
                .blksize,
            8192
        );
        assert_eq!(
            parse_rrq(&rrq(&["x", "octet", "blksize", "1"]))
                .unwrap()
                .blksize,
            8
        );
    }

    #[test]
    fn oack_only_echoes_requested_options() {
        let mut req = parse_rrq(&rrq(&["x", "octet", "blksize", "1468"])).unwrap();
        assert!(req.needs_oack());
        let body = String::from_utf8_lossy(&oack_pkt(&req, 1234)[2..]).into_owned();
        assert!(body.contains("blksize"));
        assert!(!body.contains("tsize"));

        req.tsize = true;
        let body = String::from_utf8_lossy(&oack_pkt(&req, 1234)[2..]).into_owned();
        assert!(body.contains("tsize"));
        assert!(body.contains("1234"));
    }

    #[test]
    fn a_requested_default_blksize_still_needs_an_oack() {
        let req = parse_rrq(&rrq(&["x", "octet", "blksize", "512"])).unwrap();
        assert_eq!(req.blksize, 512);
        assert!(req.needs_oack());
    }

    #[test]
    fn rejects_bad_rrq() {
        assert!(parse_rrq(&[]).is_err());
        assert!(parse_rrq(&rrq(&["x", "netascii"])).is_err());
        assert!(parse_rrq(&[0, 3, 1, 2, 3, 4]).is_err()); // DATA，不是 RRQ
        assert!(parse_rrq(&[0, 1, b'x', 0, b'o', b'c']).is_err()); // 没以 NUL 结尾
        assert!(parse_rrq(&rrq(&["x", "octet", "blksize", "abc"])).is_err());
    }

    #[test]
    fn filenames_cannot_escape_the_root() {
        assert_eq!(
            safe_name("efi/boot/bootx64.efi").unwrap(),
            Path::new("efi/boot/bootx64.efi")
        );
        for bad in [
            "",
            "../secret",
            "a/../../secret",
            "/etc/passwd",
            r"..\secret",
            "C:\\secret",
        ] {
            assert!(safe_name(bad).is_none(), "{bad} 不该被放行");
        }
    }

    #[test]
    fn http_paths_cannot_escape_the_root() {
        assert_eq!(safe_relative("/boot.ipxe").unwrap(), Path::new("boot.ipxe"));
        assert_eq!(safe_relative("/efi/./x").unwrap(), Path::new("efi/x"));
        for bad in ["/", "", "/../secret", "/a/../secret", r"/..\secret"] {
            assert!(safe_relative(bad).is_none(), "{bad} 不该被放行");
        }
    }

    #[test]
    fn block_numbers_wrap_at_16_bits() {
        let pkt = data_pkt(0, b"x");
        assert_eq!(&pkt[..4], &[0, 3, 0, 0]);
        assert_eq!(ack_block(&ack_pkt(0)), Some(0));
        assert_eq!(ack_block(&ack_pkt(65535)), Some(u16::MAX));
        assert_eq!(ack_block(&[0, 3, 0, 0]), None);
        assert_eq!(((65536_usize + 1) % 65536) as u16, 1);
    }

    #[test]
    fn plan_only_advertises_files_that_exist() {
        let dir = std::env::temp_dir().join(format!("lessor-boot-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let empty = plan(
            &dir,
            "192.168.88.1:8081".parse().unwrap(),
            "192.168.88.1".parse().unwrap(),
        );
        assert!(empty.found.is_empty());
        assert!(empty.boot.filename.is_none());
        assert!(empty.boot.ipxe_url.is_none());

        std::fs::write(dir.join(IPXE_EFI), b"efi").unwrap();
        std::fs::write(dir.join(IPXE_SCRIPT), b"#!ipxe").unwrap();
        let p = plan(
            &dir,
            "192.168.88.1:8081".parse().unwrap(),
            "192.168.88.1".parse().unwrap(),
        );
        assert_eq!(p.boot.filename.as_deref(), Some(IPXE_EFI));
        assert_eq!(
            p.boot.ipxe_url.as_deref(),
            Some("http://192.168.88.1:8081/boot.ipxe")
        );
        assert_eq!(p.boot.next_server.unwrap().to_string(), "192.168.88.1");
        assert!(p.boot.http_url.is_none()); // bootx64.efi 不在，不指向不存在的文件

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fill_defaults_does_not_overwrite_an_explicit_boot_config() {
        let mut scopes = vec![
            lessor_core::Scope::new(1, "a", "10.0.0.0".parse().unwrap(), 24),
            lessor_core::Scope::new(2, "b", "10.0.1.0".parse().unwrap(), 24),
        ];
        scopes[1].boot = Some(lessor_core::BootConfig {
            filename: Some("custom.efi".into()),
            ..Default::default()
        });
        let default = lessor_core::BootConfig {
            filename: Some("ipxe.efi".into()),
            ..Default::default()
        };
        assert_eq!(fill_defaults(&mut scopes, &default), 1);
        assert_eq!(
            scopes[0].boot.as_ref().unwrap().filename.as_deref(),
            Some("ipxe.efi")
        );
        assert_eq!(
            scopes[1].boot.as_ref().unwrap().filename.as_deref(),
            Some("custom.efi")
        );
    }

    #[test]
    fn unspecified_bind_is_advertised_as_the_dhcp_address() {
        let a = advertised_addr(
            "0.0.0.0:8081".parse().unwrap(),
            "192.168.88.1".parse().unwrap(),
        );
        assert_eq!(a.to_string(), "192.168.88.1:8081");
        let b = advertised_addr(
            "192.168.88.1:8081".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        );
        assert_eq!(b.to_string(), "192.168.88.1:8081");
    }

    /// 真 UDP 走一遍：选项协商 → 分块 → ACK → 重组，字节完全一致。
    /// 不测的话，"块号回绕 / OACK 必须先 ACK(0)" 这类错误只有拿真固件才看得见。
    #[tokio::test]
    async fn tftp_transfer_roundtrip_over_udp() {
        let dir = std::env::temp_dir().join(format!("lessor-tftp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let payload: Vec<u8> = (0..37u8).collect();
        std::fs::write(dir.join("boot.ipxe"), &payload).unwrap();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = client.local_addr().unwrap();
        let req = ReadRequest {
            filename: "boot.ipxe".into(),
            mode: "octet".into(),
            blksize: 8,
            blksize_requested: true,
            tsize: true,
        };
        let root = Arc::new(dir.clone());
        let handle = tokio::spawn(async move {
            transfer(root, Ipv4Addr::LOCALHOST, req, peer).await;
        });

        let mut buf = [0u8; 512];
        // 收一个包，返回 (长度, 对端 TID)。写成函数而不是闭包 ——
        // 闭包会长期借用 buf，后面解析包内容就拿不到了。
        async fn recv(client: &UdpSocket, buf: &mut [u8]) -> (usize, SocketAddr) {
            tokio::time::timeout(Duration::from_secs(3), client.recv_from(buf))
                .await
                .expect("等 TFTP 包超时")
                .expect("收包失败")
        }

        // OACK 必须先回 ACK(0)
        let (n, tid) = recv(&client, &mut buf).await;
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), OACK);
        assert!(String::from_utf8_lossy(&buf[2..n]).contains("blksize"));
        client.send_to(&ack_pkt(0), tid).await.unwrap();

        let mut got = Vec::new();
        loop {
            let (n, from) = recv(&client, &mut buf).await;
            assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), DATA);
            let block = u16::from_be_bytes([buf[2], buf[3]]);
            let chunk = buf[4..n].to_vec();
            got.extend_from_slice(&chunk);
            client.send_to(&ack_pkt(block), from).await.unwrap();
            if chunk.len() < 8 {
                break;
            }
        }
        assert_eq!(got, payload);
        handle.await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
