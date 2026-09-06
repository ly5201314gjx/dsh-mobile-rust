//! 收纳箱：列出智能体产物（工作台产物 + 会话记录），供设置页展示。

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

/// 列出文件（仅文件，按修改时间倒序）。
fn list_files_sorted(dir: &Path) -> Vec<std::fs::DirEntry> {
    let mut out: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.flatten().filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false)).collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by_key(|a| {
        std::cmp::Reverse(
            a.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(UNIX_EPOCH),
        )
    });
    out
}

fn entry_json(rel: &str, e: &std::fs::DirEntry) -> Value {
    let meta = e.metadata().ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    json!({ "rel": rel, "size": size, "mtime": mtime })
}

/// 列出收纳箱全部文件，返回 JSON：
/// `{"files":[{"rel":"workspace/xxx","size":..,"mtime":..}, ...]}`
pub fn list_artifacts(files_dir: &Path) -> String {
    let mut files: Vec<Value> = Vec::new();

    // 分类 1：工作台产物
    let workspace = files_dir.join("workspace");
    for e in list_files_sorted(&workspace) {
        let rel = format!("workspace/{}", e.file_name().to_string_lossy());
        files.push(entry_json(&rel, &e));
    }

    // 分类 2：会话记录 dsh-home/sessions/<ws>/<session>/<file>
    let sessions = files_dir.join("dsh-home").join("sessions");
    if let Ok(ws_dirs) = std::fs::read_dir(&sessions) {
        for ws in ws_dirs.flatten() {
            if !ws.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let ws_name = ws.file_name().to_string_lossy().to_string();
            if let Ok(sess_dirs) = std::fs::read_dir(ws.path()) {
                for sd in sess_dirs.flatten() {
                    if !sd.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let sess_name = sd.file_name().to_string_lossy().to_string();
                    for e in list_files_sorted(&sd.path()) {
                        let rel = format!("sessions/{ws_name}/{sess_name}/{}", e.file_name().to_string_lossy());
                        files.push(entry_json(&rel, &e));
                    }
                }
            }
        }
    }

    json!({ "files": files }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_both_categories() {
        let dir = std::env::temp_dir().join(format!("dshm-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspace")).unwrap();
        std::fs::create_dir_all(dir.join("dsh-home/sessions/ws1/sess-abc123")).unwrap();
        std::fs::write(dir.join("workspace/a.txt"), b"a").unwrap();
        std::fs::write(dir.join("dsh-home/sessions/ws1/sess-abc123/session.jsonl.zstd"), b"b").unwrap();
        let s = list_artifacts(&dir);
        assert!(s.contains("workspace/a.txt"));
        assert!(s.contains("sessions/ws1/sess-abc123/session.jsonl.zstd"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
