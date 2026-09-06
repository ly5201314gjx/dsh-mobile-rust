//! 内置 node 服务管理：解压 payload（如需要）→ spawn node → 崩溃自动重启。
//! 线程模型与 Java 版 bootLoop 一致，状态写入 state 文件供 UI 读取。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

use crate::payload::{self, MARKER};
use crate::state;

/// 服务配置（路径全部由 Android 侧传入）。
#[derive(Debug, Clone)]
pub struct ServerCfg {
    /// termux/usr/bin/node
    pub node_bin: PathBuf,
    /// dsh-app 目录（cwd）
    pub app_dir: PathBuf,
    /// getFilesDir()
    pub files_dir: PathBuf,
    /// filesDir/payload
    pub payload_dir: PathBuf,
    /// filesDir/dsh-home
    pub home_dir: PathBuf,
    /// 状态文件路径（filesDir/dsh-server.state）
    pub state_file: PathBuf,
}

impl ServerCfg {
    /// 临时目录（node 的 TMPDIR/TMP）
    pub fn tmp_dir(&self) -> PathBuf {
        self.files_dir.join("tmp")
    }
    /// 日志文件
    pub fn log_file(&self) -> PathBuf {
        self.files_dir.join("dsh-server.log")
    }
    /// 带 launch token 的 Web 入口 URL（从 node stdout 解析出，供 WebView 加载）
    pub fn web_url_file(&self) -> PathBuf {
        self.files_dir.join("web-url.txt")
    }
    /// 解压进度状态文件
    pub fn payload_state_file(&self) -> PathBuf {
        self.files_dir.join("payload.state")
    }
}

/// 是否已解压完成。
pub fn payload_ready(cfg: &ServerCfg) -> bool {
    cfg.payload_dir.join(MARKER).exists()
}

static GEN: AtomicI32 = AtomicI32::new(0);
static RUNNING: AtomicI32 = AtomicI32::new(0);

const MAX_RESTARTS: i32 = 5;
const RESTART_DELAY_MS: u64 = 3000;
const POLL_INTERVAL_MS: u64 = 200;

/// 启动内置服务（幂等）。立即返回，服务在后台线程运行。
pub fn start_server(cfg: ServerCfg) {
    if RUNNING.swap(1, Ordering::SeqCst) == 1 {
        return;
    }
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    thread::Builder::new()
        .name("dsh-server".into())
        .spawn(move || boot_loop(cfg, gen))
        .ok();
}

/// 停止内置服务（RUNNING=0 + 代际 +1，bootLoop 轮询到后自行杀进程退出）。
pub fn stop_server() {
    GEN.fetch_add(1, Ordering::SeqCst);
    RUNNING.store(0, Ordering::SeqCst);
}

fn boot_loop(cfg: ServerCfg, gen: i32) {
    if !payload_ready(&cfg) {
        let tmp_zip = cfg.files_dir.join("payload.zip.tmp");
        match payload::extract_payload(&tmp_zip, &cfg.payload_dir, &cfg.payload_state_file()) {
            Ok(_) => {}
            Err(e) => {
                state::write(&cfg.state_file, &format!("failed:{e}"));
                RUNNING.store(0, Ordering::SeqCst);
                return;
            }
        }
    }

    let _ = std::fs::create_dir_all(cfg.tmp_dir());
    let _ = std::fs::create_dir_all(&cfg.home_dir);

    state::write(&cfg.state_file, state::STATE_STARTING);
    let mut restarts = 0;
    while RUNNING.load(Ordering::SeqCst) == 1 && gen == GEN.load(Ordering::SeqCst) {
        let mut child = match spawn_node(&cfg) {
            Ok(c) => c,
            Err(e) => {
                state::write(&cfg.state_file, &format!("failed:{e}"));
                restarts += 1;
                if restarts >= MAX_RESTARTS {
                    RUNNING.store(0, Ordering::SeqCst);
                    return;
                }
                thread::sleep(Duration::from_millis(RESTART_DELAY_MS));
                continue;
            }
        };

        state::write(&cfg.state_file, state::STATE_STARTING);
        // 轮询等待退出；期间若收到停止信号（RUNNING=0 / 代际变化），
        // 自己负责杀进程并退出，无需跨线程共享 Child 句柄。
        let code = loop {
            if RUNNING.load(Ordering::SeqCst) != 1 || gen != GEN.load(Ordering::SeqCst) {
                stop_child(&mut child);
                return;
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => thread::sleep(Duration::from_millis(POLL_INTERVAL_MS)),
                Err(_) => break None,
            }
        };

        if gen != GEN.load(Ordering::SeqCst) || RUNNING.load(Ordering::SeqCst) != 1 {
            return;
        }
        restarts += 1;
        match code {
            Some(c) => state::write(&cfg.state_file, &format!("exited:{c}")),
            None => state::write(&cfg.state_file, "exited:signal"),
        }
        if restarts >= MAX_RESTARTS {
            state::write(&cfg.state_file, "failed:too-many-restarts");
            RUNNING.store(0, Ordering::SeqCst);
            return;
        }
        thread::sleep(Duration::from_millis(RESTART_DELAY_MS));
    }
}

fn spawn_node(cfg: &ServerCfg) -> Result<Child, String> {
    let termux = cfg.payload_dir.join("termux").join("usr");
    let lib_dir = termux.join("lib");
    let bin_dir = termux.join("bin");

    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("LD_LIBRARY_PATH".into(), lib_dir.display().to_string());
    env.insert("HOME".into(), cfg.files_dir.display().to_string());
    env.insert("DSH_HOME".into(), cfg.home_dir.display().to_string());
    env.insert("TMPDIR".into(), cfg.tmp_dir().display().to_string());
    env.insert("TMP".into(), cfg.tmp_dir().display().to_string());
    env.insert(
        "SSL_CERT_FILE".into(),
        termux.join("etc/ssl/certs/ca-certificates.crt").display().to_string(),
    );
    env.insert("SSL_CERT_DIR".into(), termux.join("etc/ssl/certs").display().to_string());
    env.insert(
        "PATH".into(),
        format!("{}:/system/bin:/system/xbin", bin_dir.display()),
    );
    env.insert("LANG".into(), "C.UTF-8".into());
    env.insert("LC_ALL".into(), "C.UTF-8".into());
    env.insert("NODE_NO_WARNINGS".into(), "1".into());

    let log = std::fs::File::options()
        .create(true)
        .append(true)
        .open(cfg.log_file())
        .map_err(|e| format!("open log: {e}"))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("clone log: {e}"))?;

    let mut child = Command::new(&cfg.node_bin)
        .args([
            "--expose-internals",
            "lib/bin.js",
            "web",
            "--host",
            "127.0.0.1",
            "--port",
            "3080",
        ])
        .current_dir(&cfg.app_dir)
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|e| format!("spawn node: {e}"))?;

    // stdout 交给后台线程 tee 到日志，并解析 `dsh web: <url>` 拿到带 token 的入口 URL
    if let Some(out) = child.stdout.take() {
        let log_path = cfg.log_file();
        let url_path = cfg.web_url_file();
        thread::Builder::new()
            .name("dsh-stdout".into())
            .spawn(move || tee_and_capture(out, &log_path, &url_path))
            .ok();
    }
    Ok(child)
}

/// 读取 node 的 stdout 行：每行追加到日志，并捕获首个 `dsh web: <http...>` URL 原子写入 web-url.txt。
fn tee_and_capture<R: std::io::Read>(mut out: R, log_path: &Path, url_path: &Path) {
    use std::io::{BufRead, BufReader};
    let mut reader = BufReader::new(out);
    let mut line = String::new();
    let mut captured = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                // tee 到日志
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                    let _ = std::io::Write::write_all(&mut f, line.as_bytes());
                }
                // 解析 `dsh web: http://host:port/?token=...`
                if !captured {
                    if let Some(rest) = line.strip_prefix("dsh web: ") {
                        let rest = rest.trim();
                        // 取到行尾或第一个空白前的完整 URL（LAN 行会有 " (LAN: ..." 后缀）
                        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                        let url = &rest[..end];
                        if url.starts_with("http://") {
                            atomic_write_text(url_path, url);
                            captured = true;
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }
}

/// 原子写入文本（先写临时文件再 rename）。
fn atomic_write_text(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        let _ = std::io::Write::write_all(&mut f, text.as_bytes());
        let _ = std::fs::rename(&tmp, path);
    }
}

/// 先 SIGTERM，等 1 秒，再 SIGKILL。
fn stop_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        for _ in 0..10 {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_paths() {
        let cfg = ServerCfg {
            node_bin: "/x/node".into(),
            app_dir: "/x/app".into(),
            files_dir: "/x/files".into(),
            payload_dir: "/x/files/payload".into(),
            home_dir: "/x/files/dsh-home".into(),
            state_file: "/x/files/dsh-server.state".into(),
        };
        assert_eq!(cfg.tmp_dir(), Path::new("/x/files/tmp"));
        assert!(!payload_ready(&cfg));
    }
}
