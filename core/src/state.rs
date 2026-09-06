//! 状态文件读写（payload 解压进度 / 服务状态）。
//! 用文件而非 SharedPreferences，保证 Java 与 Rust 都能读写同一份状态。

use std::path::Path;

/// 读取状态文件内容，文件不存在时返回空字符串。
pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 原子写状态文件（先写临时文件再 rename，避免读到半截内容）。
pub fn write(path: &Path, value: &str) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, value).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// 服务状态：运行中 / 已退出 / 失败 / 停止。
pub const STATE_RUNNING: &str = "running";
pub const STATE_STARTING: &str = "starting";
pub const STATE_STOPPED: &str = "stopped";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("dshm-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("state.txt");
        write(&f, "running");
        assert_eq!(read(&f), "running");
        assert_eq!(read(&dir.join("missing.txt")), "");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
