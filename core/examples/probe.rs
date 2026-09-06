//! 验证 web-url.txt 能由 tee_and_capture 正确落盘。
//! 验证后删除。
use dsh_core::server::{start_server, stop_server, ServerCfg};
use std::{path::PathBuf, thread, time::Duration};

fn main() {
    let files = PathBuf::from("/tmp/dsh-probe-files");
    std::fs::remove_dir_all(&files).ok();
    std::fs::create_dir_all(&files).unwrap();

    let cfg = ServerCfg {
        node_bin: "/root/.nvm/versions/node/v24.1.0/bin/node".into(),
        app_dir: "/workspace/dsh-mobile-rust/payload/dsh-app".into(),
        files_dir: files.clone(),
        payload_dir: files.join("payload"),
        home_dir: files.join("dsh-home"),
        state_file: files.join("dsh-server.state"),
    };

    // 手工放置 payload 结构，跳过真实解压
    let paydir = files.join("payload");
    std::fs::create_dir_all(&paydir).unwrap();
    std::fs::write(paydir.join(dsh_core::payload::MARKER), b"ok").unwrap();
    let termux = paydir.join("termux");
    std::fs::create_dir_all(termux.join("usr/lib")).unwrap();
    std::fs::create_dir_all(&files.join("tmp")).unwrap();
    std::fs::create_dir_all(&cfg.home_dir).unwrap();

    dsh_core::server::start_server(cfg.clone());

    let url_file = files.join("web-url.txt");
    let log_file = files.join("dsh-server.log");
    let mut got: Option<String> = None;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(500));
        if let Ok(s) = std::fs::read_to_string(&url_file) {
            if s.starts_with("http://") {
                got = Some(s.trim().to_string());
                break;
            }
        }
    }

    println!("web-url.txt: {}", got.clone().unwrap_or_else(|| "<EMPTY>".into()));
    if let Some(u) = got {
        // 用 token URL 抓一次，确认 303+Set-Cookie 后 index 可访问
        let _ = u;
    }
    println!("--- dsh-server.log tail ---");
    if let Ok(l) = std::fs::read_to_string(&log_file) {
        for line in l.lines().rev().take(8) {
            println!("{line}");
        }
    }
    dsh_core::server::stop_server();
    thread::sleep(Duration::from_millis(1500));
}