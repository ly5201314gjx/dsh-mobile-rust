//! DSH Desktop 守护进程
//!
//! 用法：
//! ```text
//! dsh-desktop [--app-dir <dsh-app目录>] [--node <node/node.exe>]
//!             [--home <DSH_HOME>] [--link-port <配对端口=5780>]
//!             [--web-port <DSH web端口=3080>] [--host <对外IP>]
//!             [--no-web] [--no-home]
//! ```
//!
//! 启动后：
//!  - 后台拉起 DSH Web 服务（node lib/bin.js web）；
//!  - 在 `link-port` 上开放配对服务：`/pair` 配对页（含二维码）、`/` 链接信息、`/qr.svg`、
//!    `/ws` WebSocket 远程通道；
//!  - 手机 App「连接电脑」扫码/填链接即可配对同步。

mod node;
mod pair;
mod relay_client;
mod util;
mod ws;

use std::net::UdpSocket;
use std::path::PathBuf;

use dsh_link::PairingKey;

const DEFAULT_LINK_PORT: u16 = 5780;
const DEFAULT_WEB_PORT: u16 = 3080;

/// 在 UNIX 上捕获 SIGINT/SIGTERM，优雅回收 node 子进程后退出。
#[cfg(unix)]
fn install_signal_handlers() {
    use std::sync::atomic::{AtomicI32, Ordering};
    static SET: AtomicI32 = AtomicI32::new(0);
    if SET.swap(1, Ordering::SeqCst) == 1 {
        return;
    }
    unsafe extern "C" fn on_signal(_sig: libc::c_int) {
        node::shutdown_node();
        std::process::exit(0);
    }
    unsafe {
        libc::signal(libc::SIGINT, on_signal as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "dsh-desktop".into())
}

/// 探测本机主网卡 IPv4（同一局域网手机可访问的地址）。
/// 用 UDP connect 到假目标取本地源地址，不产生真实流量，Windows/Linux 均可。
fn primary_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

fn flag(args: &[String], name: &str, default: &str) -> String {
    for i in 0..args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned().unwrap_or_else(|| default.into());
        }
    }
    default.into()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let link_port: u16 = flag(&args, "--link-port", &DEFAULT_LINK_PORT.to_string())
        .parse()
        .unwrap_or(DEFAULT_LINK_PORT);
    let web_port: u16 = flag(&args, "--web-port", &DEFAULT_WEB_PORT.to_string())
        .parse()
        .unwrap_or(DEFAULT_WEB_PORT);
    let host = if has(&args, "--host") {
        flag(&args, "--host", "127.0.0.1")
    } else {
        primary_ip().unwrap_or_else(|| "127.0.0.1".into())
    };
    let app_dir = PathBuf::from(flag(&args, "--app-dir", "payload/dsh-app"));
    let home_dir = PathBuf::from(flag(&args, "--home", "dsh-home"));
    let node = PathBuf::from(flag(&args, "--node", "node"));

    // 中继模式：`--relay <host:port>`。若指定，二维码/链接指向中继，且本端出站连中继承担 server 角色。
    let relay = if has(&args, "--relay") {
        let s = flag(&args, "--relay", "");
        crate::relay_client::parse_endpoint(&s)
    } else {
        None
    };

    // 隧道模式：`--advertise <host|host:port|wss://host|https://host[:port]>`。
    // 只改二维码/链接指向的对外地址（如 Cloudflare /wss 隧道），本端继续监听 local WS 服务，不出站连中继。
    // WSS 就用 TLS 链接，安卓以 SSL Socket 连接。
    let advertise = if has(&args, "--advertise") {
        parse_advertise(&flag(&args, "--advertise", ""))
    } else {
        None
    };
    // 隧道模式：`--advertise-web <host[:port]>` 指定 DSH Web UI 的第二个隧道地址
    //（形如 xxx.trycloudflare.com），会随配对链接以 `&web=` 参数带给手机端。
    let advertise_web = if has(&args, "--advertise-web") {
        let v = flag(&args, "--advertise-web", "").trim().to_string();
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    } else {
        None
    };

    let key = PairingKey::random();
    install_signal_handlers();
    println!(
        "[DSH Desktop] host={} link_port={} web_port={} hostname={}",
        host,
        link_port,
        web_port,
        hostname()
    );

    if !has(&args, "--no-home") {
        if has(&args, "--no-web") {
            println!("[dsh-desktop] --no-web: 只启动配对服务，不拉起 DSH Web");
        } else {
            node::start(node::DesktopCfg {
                node,
                app_dir: app_dir.clone(),
                home_dir: home_dir.clone(),
                log_file: home_dir.join("dsh-desktop-node.log"),
                web_port,
                host: "0.0.0.0".into(),
            });
        }
    }

    let home_opt = if has(&args, "--no-home") {
        None
    } else {
        Some(home_dir)
    };

    // 广告地址：优先 --advertise（隧道），其次 --relay（中继），否则 LAN 本机地址
    let (advertise_host, advertise_port, advertise_tls) = if let Some((h, p, t)) = advertise {
        (Some(h), Some(p), t)
    } else if let Some((h, p)) = relay.as_ref() {
        (Some(h.clone()), Some(*p), false)
    } else {
        (None, None, false)
    };

    let server = pair::PairServer {
        bind: "0.0.0.0".into(),
        port: link_port,
        public_ip: host.clone(),
        key: key.clone(),
        home_dir: home_opt.clone(),
        advertise_host,
        advertise_port,
        advertise_tls,
        advertise_web_host: advertise_web.clone(),
    };

    // 中继模式：后台线程持续出站连中继，承担 server 角色（hello/session_snap/send_msg）
    if let Some((rhost, rport)) = relay {
        let k = key.clone();
        let h = home_opt.clone();
        println!(
            "[DSH Desktop] 中继模式已开启: 二维码/链接指向 {}:{} , 手机可在异网络配对",
            rhost, rport
        );
        std::thread::spawn(move || relay_client::run_forever(rhost, rport, k, h));
    }

    println!("[DSH Desktop] 配对链接: {}", server.link().to_qr());
    if let Some(w) = &advertise_web {
        println!("[DSH Desktop] Web UI 隧道地址: https://{w}");
    }
    println!(
        "[DSH Desktop] 浏览器打开配对页（含二维码）: {}",
        server.page_url()
    );

    if let Err(e) = server.run() {
        eprintln!("[dsh-desktop] pair server error: {e}");
        std::process::exit(1);
    }
}

/// 解析 `--advertise` 对外地址，返回 (host, port, tls)。
/// 支持：`host`、`host:port`、`wss://host[:port]`、`https://host[:port]`、
/// `ws://host[:port]`、`http://host[:port]`（后两者默认 5780）。TLS 默认端口 443。
fn parse_advertise(s: &str) -> Option<(String, u16, bool)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // 剥 scheme 并判定 tls
    let (rest, tls) = {
        let lower = s.to_ascii_lowercase();
        if let Some(r) = lower.strip_prefix("wss://") {
            (&s["wss://".len()..], true)
        } else if let Some(r) = lower.strip_prefix("https://") {
            (&s["https://".len()..], true)
        } else if let Some(r) = lower.strip_prefix("ws://") {
            (&s["ws://".len()..], false)
        } else if let Some(r) = lower.strip_prefix("http://") {
            (&s["http://".len()..], false)
        } else {
            (s, false)
        }
    };
    let rest = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match crate::relay_client::parse_endpoint(rest) {
        Some((h, p)) => (h, p),
        // 无端口的裸 host（如 wss://xxx.trycloudflare.com）→ 默认端口
        None => (rest.to_string(), 0),
    };
    let port = if port != 0 { port } else if tls { 443 } else { DEFAULT_LINK_PORT };
    Some((host, port, tls))
}