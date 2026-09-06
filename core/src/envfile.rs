//! 把 DeepSeek API Key 写入 $DSH_HOME/.env（权限 600）。
//! DSH 的 .env 后备层会读取 DEEPSEEK_API_KEY。

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// 写 .env。key 为空时清空文件内容。返回 None 表示成功，Some(msg) 表示失败原因。
pub fn write_env(home_dir: &Path, api_key: &str) -> Option<String> {
    let _ = std::fs::create_dir_all(home_dir);
    let env_file = home_dir.join(".env");
    let content = if api_key.is_empty() {
        String::new()
    } else {
        format!("# written by DSH Mobile settings\nDEEPSEEK_API_KEY={api_key}\n")
    };
    match OpenOptions::new().create(true).write(true).truncate(true).open(&env_file) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(content.as_bytes()) {
                return Some(format!("write .env: {e}"));
            }
            if let Ok(meta) = f.metadata() {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&env_file, perms);
            }
            None
        }
        Err(e) => Some(format!("open .env: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_written_with_600() {
        let dir = std::env::temp_dir().join(format!("dshm-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(write_env(&dir, "sk-test").is_none());
        let content = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert!(content.contains("DEEPSEEK_API_KEY=sk-test"));
        let perms = std::fs::metadata(dir.join(".env")).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
