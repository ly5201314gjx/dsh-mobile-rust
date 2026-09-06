//! 桌面端启动内置 DSH Web 服务（node `lib/bin.js web`）。
//! 与 `dsh-core::server` 的目标一致（守护 node 进程、崩溃重启），但：
//!  - node 来自系统/随包（非 termux 前缀），无需 LD_LIBRARY_PATH；
//!  - 默认绑定 `127.0.0.1`（回环）：DSH web 拒绝非回环地址，对外访问一律经配对端口 5780 / 隧道 / token 代理转发。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

/// 桌面服务配置。
pub struct DesktopCfg {
    /// node 可执行文件（Windows 为 node.exe）。
    pub node: PathBuf,
    /// dsh-app 目录（含 lib/bin.js）。
    pub app_dir: PathBuf,
    /// DSH_HOME（会话/收纳箱等）。
    pub home_dir: PathBuf,
    /// 日志文件。
    pub log_file: PathBuf,
    /// DSH web 端口。
    pub web_port: u16,
    /// 绑定地址（默认 127.0.0.1 回环；DSH web 拒绝非回环）。
    pub host: String,
    /// 对外 Web UI 隧道域名（--advertise-web），注入 --trusted-host。
    pub advertise_web_host: Option<String>,
}

static RUNNING: AtomicI32 = AtomicI32::new(0);
static GEN: AtomicI32 = AtomicI32::new(0);
/// 当前 node 子进程 pid（供 Ctrl+C 等信号直接回收）。
static CHILD_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
const MAX_RESTARTS: i32 = 5;
const RESTART_DELAY_MS: u64 = 2000;
const POLL_MS: u64 = 300;

/// 后台启动 node（幂等，立即返回）。
pub fn start(cfg: DesktopCfg) {
    if RUNNING.swap(1, Ordering::SeqCst) == 1 {
        return;
    }
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    thread::Builder::new()
        .name("dsh-desktop-node".into())
        .spawn(move || boot_loop(cfg, gen))
        .ok();
}

/// 优雅关闭：置停止标志并直接结束当前 node 子进程（在信号处理里用，避免孤儿进程）。
#[cfg(unix)]
pub fn shutdown_node() {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    stop();
}

/// 请求停止（代际 +1，boot 线程轮询到后自行结束进程）。
pub fn stop() {
    GEN.fetch_add(1, Ordering::SeqCst);
    RUNNING.store(0, Ordering::SeqCst);
}

fn boot_loop(cfg: DesktopCfg, gen: i32) {
    let _ = std::fs::create_dir_all(&cfg.home_dir);
    let mut restarts = 0;
    while RUNNING.load(Ordering::SeqCst) == 1 && gen == GEN.load(Ordering::SeqCst) {
        let mut child = match spawn_node(&cfg) {
            Ok(c) => {
                CHILD_PID.store(c.id() as i32, Ordering::SeqCst);
                c
            }
            Err(e) => {
                eprintln!("[dsh-desktop] spawn node failed: {e}");
                restarts += 1;
                if restarts >= MAX_RESTARTS {
                    RUNNING.store(0, Ordering::SeqCst);
                    return;
                }
                thread::sleep(Duration::from_millis(RESTART_DELAY_MS));
                continue;
            }
        };
        let code = loop {
            if RUNNING.load(Ordering::SeqCst) != 1 || gen != GEN.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            match child.try_wait() {
                Ok(Some(st)) => break st.code(),
                Ok(None) => thread::sleep(Duration::from_millis(POLL_MS)),
                Err(_) => break None,
            }
        };
        if gen != GEN.load(Ordering::SeqCst) || RUNNING.load(Ordering::SeqCst) != 1 {
            return;
        }
        restarts += 1;
        eprintln!(
            "[dsh-desktop] dsh web exited ({:?}); restarts={}",
            code, restarts
        );
        if restarts >= MAX_RESTARTS {
            RUNNING.store(0, Ordering::SeqCst);
            return;
        }
        thread::sleep(Duration::from_millis(RESTART_DELAY_MS));
    }
}

fn spawn_node(cfg: &DesktopCfg) -> Result<Child, String> {
    let log = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
        .map_err(|e| format!("open log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("clone log: {e}"))?;

    let mut cmd_args: Vec<String> = vec![
        "--expose-internals".into(),
        "lib/bin.js".into(),
        "web".into(),
        "--host".into(),
        cfg.host.clone(),
        "--port".into(),
        cfg.web_port.to_string(),
    ];
    if let Some(w) = &cfg.advertise_web_host {
        cmd_args.push("--trusted-host".into());
        cmd_args.push(w.clone());
    }
    let child = Command::new(&cfg.node)
        .args(cmd_args)
        .current_dir(&cfg.app_dir)
        // 继承宿主 PATH 等环境，仅注入 DSH_HOME
        .env("DSH_HOME", &cfg.home_dir)
        .env("NODE_NO_WARNINGS", "1")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|e| format!("spawn node: {e}"))?;
    Ok(child)
}