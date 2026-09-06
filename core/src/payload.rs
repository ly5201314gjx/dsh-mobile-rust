//! payload.zip 解压：把 assets 里的 payload.zip 解到 filesDir/payload，
//! 设置可执行位、写完成标记，并把进度写入状态文件。
//! 幂等：解压完成后写入 payload.v1.ok 标记，已存在则直接成功。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use zip::ZipArchive;

use crate::state;

/// 解压完成的标记文件名（位于 dest_root 下）。
pub const MARKER: &str = "payload.v1.ok";

/// 解压结果。
pub struct ExtractOutcome {
    /// 解压出的文件数。
    pub files: usize,
    /// 解压出的字节数。
    pub bytes: u64,
}

fn chmod_x(p: &Path) {
    #[cfg(unix)]
    {
        if let Ok(meta) = std::fs::metadata(p) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(p, perms);
        }
    }
}

/// 解压 payload.zip。`tmp_zip` 是 assets 拷贝出来的 zip 文件路径（assets 流
/// 不支持随机访问，Android 侧先拷出再解）。
pub fn extract_payload(
    tmp_zip: &Path,
    dest_root: &Path,
    state_file: &Path,
) -> Result<ExtractOutcome, String> {
    if dest_root.join(MARKER).exists() {
        state::write(state_file, state::STATE_STOPPED); // 已就绪，静默返回
        return Ok(ExtractOutcome { files: 0, bytes: 0 });
    }

    let file = File::open(tmp_zip).map_err(|e| format!("open {}: {e}", tmp_zip.display()))?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("read {}: {e}", tmp_zip.display()))?;
    let total = zip.len().max(1);

    std::fs::create_dir_all(dest_root).map_err(|e| format!("mkdir {}: {e}", dest_root.display()))?;

    let mut files = 0usize;
    let mut bytes = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if name.contains("..") {
            continue; // 防 zip-slip
        }
        let out: PathBuf = dest_root.join(&name);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let mut f = File::create(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
        std::io::copy(&mut entry, &mut f)
            .map_err(|e| format!("write {}: {e}", out.display()))?;
        bytes += entry.size();
        files += 1;
        if files % 500 == 0 || files == total {
            let pct = files * 100 / total;
            state::write(state_file, &format!("extracting:{pct}"));
        }
    }

    // zip 不保留权限：恢复可执行位
    for bin in ["node", "bash", "rg"] {
        let p = dest_root.join("termux").join("usr").join("bin").join(bin);
        if p.exists() {
            chmod_x(&p);
        }
    }

    // 完成标记
    std::fs::write(dest_root.join(MARKER), b"ok").map_err(|e| format!("marker: {e}"))?;
    state::write(state_file, "ready");
    Ok(ExtractOutcome { files, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_zip_with_marker_and_chmod() {
        let dir = std::env::temp_dir().join(format!("dshm-payload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("zip/termux/usr/bin")).unwrap();
        std::fs::create_dir_all(dir.join("zip/dsh-app")).unwrap();
        std::fs::write(dir.join("zip/termux/usr/bin/node"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(dir.join("zip/dsh-app/package.json"), b"{}").unwrap();
        let zip_path = dir.join("in.zip");
        let f = File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zw.start_file("termux/usr/bin/node", opts).unwrap();
        zw.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        zw.start_file("dsh-app/package.json", opts).unwrap();
        zw.write_all(b"{}").unwrap();
        zw.finish().unwrap();

        let dest = dir.join("out");
        let state_f = dir.join("state.txt");
        let r = extract_payload(&zip_path, &dest, &state_f).unwrap();
        assert_eq!(r.files, 2);
        assert!(dest.join(MARKER).exists());
        assert_eq!(state::read(&state_f), "ready");
        #[cfg(unix)]
        {
            let meta = std::fs::metadata(dest.join("termux/usr/bin/node")).unwrap();
            assert_eq!(meta.permissions().mode() & 0o111, 0o111);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
