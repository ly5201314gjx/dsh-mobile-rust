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

    let advertise_host = relay.as_ref().map(|(h, _)| h.clone());
    let advertise_port = relay.as_ref().map(|(_, p)| *p);

    let server = pair::PairServer {
        bind: "0.0.0.0".into(),
        port: link_port,
        public_ip: host.clone(),
        key: key.clone(),
        home_dir: home_opt.clone(),
        advertise_host,
        advertise_port,
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
    println!(
        "[DSH Desktop] 浏览器打开配对页（含二维码）: {}",
        server.page_url()
    );

    if let Err(e) = server.run() {
        eprintln!("[dsh-desktop] pair server error: {e}");
        std::process::exit(1);
    }
}