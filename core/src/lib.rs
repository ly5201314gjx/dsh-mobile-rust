//! DSH Mobile 核心逻辑库。
//!
//! 纯 Rust 实现，不依赖 Android API，可在桌面/Linux 上直接单元测试。
//! Android 侧通过 `app` crate（cdylib）以 JNI 形式复用本库。

pub mod artifacts;
pub mod envfile;
pub mod payload;
pub mod port;
pub mod server;
pub mod state;

/// 应用版本号（与 Android versionName 保持一致）。
pub const VERSION: &str = "3.0";

/// 内置服务监听地址。
pub const SERVER_HOST: &str = "127.0.0.1";
/// 内置服务监听端口。
pub const SERVER_PORT: u16 = 3080;

/// 递归删除文件/目录，返回是否成功。
pub fn delete_path(path: &std::path::Path) -> bool {
    remove_recursive(path)
}

fn remove_recursive(p: &std::path::Path) -> bool {
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                remove_recursive(&e.path());
            }
        }
    }
    match std::fs::remove_file(p) {
        Ok(_) => true,
        Err(_) => {
            // 目录需要 remove_dir
            std::fs::remove_dir_all(p).is_ok() || !p.exists()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
        assert!(SERVER_PORT > 0);
        assert_eq!(delete_path(std::path::Path::new("/nonexistent/xyz")), true);
    }
}
