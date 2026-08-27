// 桌面外壳。
//
// 有意做成**先连、连不上才自带拉起**（attach-first）：
//
// 1. 启动先探测 lessord。已经在跑（比如装成了系统服务、或跑在机房另一台
//    机器上）→ 只当客户端，什么都不拉起 —— 标准服务器场景不受影响。
// 2. 没在跑 → 回退页上给一个"启动本机实例"的表单，选网卡后把随包自带的
//    lessord 作为子进程拉起，窗口关闭时带走。
//
// 所以"自包含"不是架构：架构上服务始终独立，桌面端只是多了一个便利的
// 启动器。拉起的实例明确是临时的现场实例，不冒充系统服务 ——
// 要常驻就单独跑 lessord，桌面端会自动变回纯客户端。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};

/// 默认连本机的 lessord。可以用 `LESSOR_URL` 指向别处 ——
/// 比如机房里另一台机器上的服务。
const DEFAULT_URL: &str = "http://127.0.0.1:8080";

/// 由本外壳拉起的那个 lessord。窗口关闭时必须带走它 ——
/// 留下一个没人管的 DHCP 服务器比没有服务更糟。
struct LocalServer(Mutex<Option<Child>>);

fn target_url() -> String {
    std::env::var("LESSOR_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned())
}

/// 探一下服务在不在。
///
/// 不用 HTTP 请求是为了不引入 HTTP 客户端依赖 —— 这里只需要知道
/// "端口有没有人听"，TCP 连一下就够了。
fn is_up(url: &str) -> bool {
    let Some(hostport) = url.split("://").nth(1) else {
        return false;
    };
    let hostport = hostport.split('/').next().unwrap_or(hostport);
    let addrs: Vec<SocketAddr> = match std::net::ToSocketAddrs::to_socket_addrs(&hostport) {
        Ok(it) => it.collect(),
        Err(_) => return false,
    };
    addrs
        .iter()
        .any(|a| TcpStream::connect_timeout(a, Duration::from_millis(400)).is_ok())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IfaceChoice {
    name: String,
    addr: String,
    prefix: u8,
}

/// 给"启动本机实例"的表单列可用网卡。复用 lessor-net，和 lessord 同一份逻辑。
#[tauri::command]
fn list_interfaces() -> Result<Vec<IfaceChoice>, String> {
    let ifs = lessor_net::interfaces().map_err(|e| e.to_string())?;
    Ok(ifs
        .into_iter()
        .filter(lessor_net::Interface::is_servable)
        .filter_map(|i| {
            let cidr = i.primary_ipv4()?;
            Some(IfaceChoice {
                name: i.name,
                addr: cidr.addr.to_string(),
                prefix: cidr.prefix,
            })
        })
        .collect())
}

/// 随包自带的 lessord 在哪。
///
/// 安装后 Tauri 把 sidecar 放在主程序旁边；开发时退回工作区的 target 目录。
fn sidecar_path() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "lessord.exe"
    } else {
        "lessord"
    };
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    let mut candidates = vec![exe_dir.join(name)];
    // 开发时：ui/src-tauri/target/{debug,release}/ → 仓库根的 target/release
    for up in ["../../../../target/release", "../../../../target/debug"] {
        candidates.push(exe_dir.join(up).join(name));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Windows 上的"父死子亡"保险。
///
/// 正常关窗时 `RunEvent` 里会显式带走子进程，但**强杀外壳**（任务管理器
/// 结束进程、崩溃）走不到任何回调。那种情况下留下的是一个没人管、还在
/// 发地址的 DHCP 服务器 —— 在机房里这比服务没起来更麻烦。
///
/// Job Object 把清理交给内核：作业句柄随进程消亡而关闭，
/// `KILL_ON_JOB_CLOSE` 保证里面的进程一起走。绕不过去。
#[cfg(windows)]
mod jobkill {
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    struct Job(HANDLE);
    // 句柄只在创建时写入，之后仅作为参数传给 Win32 调用
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    static JOB: OnceLock<Option<Job>> = OnceLock::new();

    fn job() -> Option<HANDLE> {
        JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                None
            } else {
                Some(Job(h))
            }
        })
        .as_ref()
        .map(|j| j.0)
    }

    /// 把子进程并入作业。失败不致命 —— 正常关窗那条路径仍然会清理。
    pub fn adopt(child: &std::process::Child) {
        use std::os::windows::io::AsRawHandle;
        if let Some(j) = job() {
            unsafe {
                AssignProcessToJobObject(j, child.as_raw_handle() as HANDLE);
            }
        }
    }
}

#[cfg(not(windows))]
mod jobkill {
    pub fn adopt(_child: &std::process::Child) {}
}

/// 挑一个 HTTP 端口。尽量用默认的 8080 —— 下次纯客户端模式还能直接连上；
/// 被占了就随机。
fn pick_http_port() -> u16 {
    for port in [8080u16, 0] {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            if let Ok(a) = l.local_addr() {
                return a.port();
            }
        }
    }
    8080
}

/// 拉起本机实例，返回它的界面地址。
///
/// 参数在这里逐个校验成类型化的值再拼命令行 —— 表单来的东西不能直接透传。
#[tauri::command]
fn start_local(
    state: State<'_, LocalServer>,
    listen: String,
    prefix: u8,
    pool: String,
) -> Result<String, String> {
    let listen: Ipv4Addr = listen.trim().parse().map_err(|_| "监听地址不合法")?;
    if !(1..=32).contains(&prefix) {
        return Err("前缀长度要在 1-32 之间".into());
    }
    let (a, b) = pool
        .split_once('-')
        .ok_or("地址池要写成 起始-结束 的形式")?;
    let a: Ipv4Addr = a.trim().parse().map_err(|_| "地址池起始地址不合法")?;
    let b: Ipv4Addr = b.trim().parse().map_err(|_| "地址池结束地址不合法")?;

    let bin = sidecar_path().ok_or("没找到随包自带的 lessord，可能安装不完整")?;
    let port = pick_http_port();
    let http = format!("127.0.0.1:{port}");

    let mut cmd = Command::new(&bin);
    cmd.arg("--listen")
        .arg(listen.to_string())
        .arg("--prefix")
        .arg(prefix.to_string())
        .arg("--pool")
        .arg(format!("{a}-{b}"))
        .arg("--http")
        .arg(&http)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // 别为子进程弹一个黑色控制台窗口出来
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    // 重试场景：上一个还活着就先带走，别攒一堆孤儿进程
    let mut guard = state.0.lock().map_err(|_| "内部状态错误")?;
    if let Some(old) = guard.as_mut() {
        let _ = old.kill();
        let _ = old.wait();
    }

    let mut child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    // 强杀外壳时的兜底，正常退出路径见 main() 末尾
    jobkill::adopt(&child);

    // 等它把 HTTP 端口听起来。起不来的话把退出码带给用户，而不是干等超时。
    let url = format!("http://{http}");
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if is_up(&url) {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "lessord 启动后立即退出（{status}）。常见原因：端口 67 被占用，\
                 或监听地址不是本机地址"
            ));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("等待服务就绪超时".into());
        }
        std::thread::sleep(Duration::from_millis(120));
    }

    *guard = Some(child);
    Ok(url)
}

fn main() {
    tauri::Builder::default()
        .manage(LocalServer(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![list_interfaces, start_local])
        .setup(|app| {
            let url = target_url();

            // 服务在就直接进界面；不在就先给回退页 ——
            // 那上面既能等外部服务，也能一键拉起本机实例。
            let start = if is_up(&url) {
                WebviewUrl::External(url.parse()?)
            } else {
                WebviewUrl::App("index.html".into())
            };

            let win = WebviewWindowBuilder::new(app, "main", start)
                .title("lessor")
                .inner_size(1180.0, 780.0)
                .min_inner_size(760.0, 520.0)
                .build()?;

            // 把目标地址传给回退页，它据此轮询并在服务起来后跳过去
            let js = format!(
                "window.__LESSOR_URL__ = {};",
                serde_json::to_string(&url).unwrap_or_else(|_| "\"\"".into())
            );
            let _ = win.eval(&js);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("桌面外壳启动失败")
        .run(|app, event| {
            // 把自己拉起的 lessord 带走。attach 上的外部服务不归这里管 ——
            // 那是别人的进程。
            //
            // 必须同时管 ExitRequested 和 Exit：关掉最后一个窗口时先来的是
            // ExitRequested，只监听 Exit 的话，进程可能在清理执行前就没了，
            // 留下一个没人管的 DHCP 服务器 —— 比没有服务更糟。
            if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
                if let Some(state) = app.try_state::<LocalServer>() {
                    if let Ok(mut guard) = state.0.lock() {
                        if let Some(child) = guard.as_mut() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        *guard = None;
                    }
                }
            }
        });
}
