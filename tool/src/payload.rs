//! Termux 运行时前缀构建（Rust 重写原 extract_termux.py + patch_runpath.py）：
//! 下载 .deb → 解包到 termux/usr（bin/lib/etc）→ 按 SONAME 去重 .so →
//! 把 RUNPATH 改写为 $ORIGIN 相对路径 → 校验 DT_NEEDED 完整。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::deb;

pub const PACKAGES: &[&str] = &[
    "nodejs", "bash", "ripgrep",
    "libandroid-support", "libiconv", "readline", "ncurses", "pcre2", "libc++",
    "openssl", "c-ares", "libicu", "libsqlite", "zlib", "libffi", "ca-certificates",
];

const DROP_LIBS: &[&str] = &[
    "libpcre2-16.so", "libpcre2-32.so", "libpcre2-posix.so", "libsqlite3.53.4.so",
];

const KEEP_PRIORITY: &[&str] = &[
    "libncursesw.so.6", "libreadline.so.8", "libhistory.so.8", "libsqlite3.so",
    "libz.so.1", "libcrypto.so.3", "libssl.so.3", "libicudata.so.78", "libicui18n.so.78",
    "libicuuc.so.78", "libc++.so.1", "libc++abi.so.1", "libgcc_s.so.1", "libandroid-support.so",
    "libiconv.so.2", "libcharset.so.1", "libpcre2-8.so.0", "libcares.so.2", "libffi.so.8",
];

/// Android 系统自带、不需要打进 payload 的库。
const SYSTEM_LIBS: &[&str] = &[
    "libc.so", "libdl.so", "libm.so", "liblog.so", "libandroid.so", "libEGL.so",
    "libGLESv2.so", "libOpenSLES.so", "libjnigraphics.so", "libmediandk.so",
    "libandroid_runtime.so", "libutils.so", "libcutils.so", "libbinder.so",
];

/// 下载全部 debs 到 debs_dir（已存在且非空则跳过）。
pub fn fetch_debs(debs_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(debs_dir).map_err(|e| format!("mkdir {}: {e}", debs_dir.display()))?;
    let index = deb::fetch_index()?;
    let pkgs = deb::parse_index(&index);
    for name in PACKAGES {
        let info = pkgs
            .get(*name)
            .ok_or_else(|| format!("package not in index: {name}"))?;
        let dest = debs_dir.join(format!("{name}.deb"));
        if dest.exists() && std::fs::metadata(&dest).map(|m| m.len() > 0).unwrap_or(false) {
            println!("skip (exists): {name} -> {}", info.filename);
            continue;
        }
        let url = format!("{}/{}", deb::TERMUX_REPO, info.filename);
        println!("download {name} {} -> {}", info.filename, dest.display());
        deb::download(&url, &dest)?;
    }
    Ok(())
}

/// 解包 debs 到 termux 前缀。
pub fn extract_termux(debs_dir: &Path, prefix: &Path) -> Result<(), String> {
    let bin = prefix.join("bin");
    let lib = prefix.join("lib");
    let certs = prefix.join("etc/ssl/certs");
    for d in [&bin, &lib, &certs] {
        std::fs::create_dir_all(d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
    }
    // 清空 bin/lib
    for sub in ["lib", "bin"] {
        let d = prefix.join(sub);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // 具体文件映射（None = 只抽 .so）
    let file_map: HashMap<&str, Option<&str>> = [
        ("nodejs", Some("bin/node")),
        ("bash", Some("bin/bash")),
        ("ripgrep", Some("bin/rg")),
        ("ca-certificates", Some("etc/ssl/certs/ca-certificates.crt")),
        ("libandroid-support", None),
        ("libiconv", None),
        ("readline", None),
        ("ncurses", None),
        ("pcre2", None),
        ("libc++", None),
        ("openssl", None),
        ("c-ares", None),
        ("libicu", None),
        ("libsqlite", None),
        ("zlib", None),
        ("libffi", None),
    ]
    .into_iter()
    .collect();

    // 收集到的库文件：basename -> bytes
    let mut libs: Vec<(String, Vec<u8>)> = Vec::new();

    for (pkg, mapping) in &file_map {
        let deb_path = debs_dir.join(format!("{pkg}.deb"));
        if !deb_path.exists() || std::fs::metadata(&deb_path).map(|m| m.len() == 0).unwrap_or(true) {
            println!("SKIP (missing/empty): {pkg}");
            continue;
        }
        let data = std::fs::read(&deb_path).map_err(|e| format!("read {}: {e}", deb_path.display()))?;
        let tar = deb::deb_data_tar_bytes(&data)?;
        let files = deb::tar_files(&tar)?;
        for (inner, bytes) in files {
            // 每个包里只要 .so 都抽（nodejs 包里也有 libnode.so 等）
            let base = inner.rsplit('/').next().unwrap_or("");
            if base.starts_with("lib") && base.contains(".so") {
                libs.push((base.to_string(), bytes));
                continue;
            }
            if let Some(outname) = mapping {
                let want = format!("./data/data/com.termux/files/usr/{outname}");
                if inner == want {
                    let out = prefix.join(outname);
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
                    }
                    std::fs::write(&out, &bytes).map_err(|e| format!("write {}: {e}", out.display()))?;
                    println!("  + {outname} {}", bytes.len());
                }
                continue;
            }
        }
    }

    // ca-certificates 有时用 tls/ 布局，两种路径都兜底抽取
    let cert_deb = debs_dir.join("ca-certificates.deb");
    if cert_deb.exists() {
        let data = std::fs::read(&cert_deb).map_err(|e| format!("read cert deb: {e}"))?;
        if let Ok(tar) = deb::deb_data_tar_bytes(&data) {
            if let Ok(files) = deb::tar_files(&tar) {
                for (inner, bytes) in files {
                    for (from, to) in [
                        (
                            "./data/data/com.termux/files/usr/etc/tls/cert.pem",
                            "etc/tls/cert.pem",
                        ),
                        (
                            "./data/data/com.termux/files/usr/etc/ssl/certs/ca-certificates.crt",
                            "etc/ssl/certs/ca-certificates.crt",
                        ),
                    ] {
                        if inner == from {
                            let out = prefix.join(to);
                            if let Some(parent) = out.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
                            }
                            std::fs::write(&out, &bytes).map_err(|e| format!("write {}: {e}", out.display()))?;
                            println!("  + {to} {}", bytes.len());
                        }
                    }
                }
            }
        }
    }

    // 按内容去重，每组优先保留 SONAME / 优先级名单里的名字
    dedupe_libs(&mut libs, &lib)?;

    // 补别名符号链接：Termux 包安装脚本会生成这些链接，deb 数据里没有。
    // node 的 DT_NEEDED 是 libsqlite3.so，而 libsqlite deb 只带 libsqlite3.so.3.53.4。
    for (target, alias) in ALIAS_LINKS {
        let t = lib.join(target);
        let a = lib.join(alias);
        if t.is_file() && !a.exists() {
            std::os::unix::fs::symlink(target, &a)
                .map_err(|e| format!("symlink {alias}: {e}"))?;
            println!("  ~ lib/{alias} -> {target}");
        }
    }
    println!("extract done");
    Ok(())
}

/// SONAME -> 需要补的别名链接（Termux postinst 生成）。
const ALIAS_LINKS: &[(&str, &str)] = &[
    ("libsqlite3.so.3.53.4", "libsqlite3.so"),
];

fn dedupe_libs(libs: &mut Vec<(String, Vec<u8>)>, lib_dir: &Path) -> Result<(), String> {
    // 分组：内容 sha1 -> 组内 (名字, 字节)
    let mut groups: HashMap<String, Vec<(String, Vec<u8>)>> = HashMap::new();
    for (name, bytes) in libs.drain(..) {
        let h = Sha1::digest(&bytes);
        groups.entry(hex(&h)).or_default().push((name, bytes));
    }
    let mut chosen: Vec<(String, Vec<u8>)> = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a, b| a.0.cmp(&b.0));
        // 尝试找 SONAME
        let mut best: Option<String> = None;
        for (name, bytes) in &group {
            if DROP_LIBS.contains(&name.as_str()) {
                continue;
            }
            let soname = deb::elf_soname(bytes).filter(|s| !s.is_empty());
            if let Some(s) = soname {
                best = Some(s);
                break;
            }
        }
        if best.is_none() {
            // 优先级名单
            for (name, _) in &group {
                if KEEP_PRIORITY.contains(&name.as_str()) {
                    best = Some(name.clone());
                    break;
                }
            }
        }
        if best.is_none() {
            // 最短名（SONAME 通常最短）
            best = group.iter().map(|(n, _)| n.clone()).min_by_key(|n| n.len());
        }
        if let Some(name) = best {
            if DROP_LIBS.contains(&name.as_str()) {
                continue;
            }
            let bytes = group.remove(0).1;
            chosen.push((name, bytes));
        }
    }
    chosen.sort();
    for (name, bytes) in chosen {
        let out = lib_dir.join(&name);
        std::fs::write(&out, &bytes).map_err(|e| format!("write {}: {e}", out.display()))?;
        println!("  + lib/{name} {}", bytes.len());
    }
    Ok(())
}

fn hex(d: &[u8]) -> String {
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 把 bin/ 与 lib/ 下所有 ELF 的 RUNPATH 改为 $ORIGIN 相对路径。
pub fn patch_runpaths(prefix: &Path) -> Result<(), String> {
    let bin = prefix.join("bin");
    let lib = prefix.join("lib");
    for p in &[bin, lib] {
        let rd = std::fs::read_dir(p).map_err(|e| format!("read_dir {}: {e}", p.display()))?;
        for e in rd.flatten() {
            let path = e.path();
            let new_rpath = if e.file_name().to_string_lossy().starts_with("lib") {
                "$ORIGIN"
            } else {
                "$ORIGIN/../lib"
            };
            match deb::patch_runpath_file(&path, new_rpath) {
                Ok(n) => {
                    if n > 0 {
                        println!("patched {} -> {new_rpath}", path.display());
                    }
                }
                Err(_) => {} // 非 ELF 或没有 strtab，跳过
            }
        }
    }
    println!("all patched");
    Ok(())
}

// ---------- DSH 应用树裁剪（原 prepare_dsh_tree.py） ----------

/// 需要整体移除的桌面专用/原生预编译包。
const REMOVE_PACKAGES: &[&str] = &[
    "koffi",
    "sharp",
    "node-pty",
    "@koromix",
    "@img",
    "@deepseek-ai/dsh-sandbox-windows-acl",
];

/// 注入的等价 stub：包名 -> (文件名, 内容)。
const STUBS: &[(&str, &[(&str, &str)])] = &[
    (
        "sharp",
        &[
            (
                "package.json",
                r#"{"name":"sharp","version":"0.35.3","type":"module","main":"index.js","exports":{".":{"import":"./index.js","default":"./index.js"}}}"#,
            ),
            (
                "index.js",
                "const unavailable = (what) => { throw new Error('sharp: ' + what + ' is unavailable on this platform'); };\n\
                 function sharp() {\n\
                   return {\n\
                     metadata: async () => { throw new Error('sharp: metadata unavailable'); },\n\
                     raw: () => ({ toBuffer: async () => { throw new Error('sharp: decode unavailable'); } }),\n\
                     toBuffer: async () => { throw new Error('sharp: encode unavailable'); },\n\
                     resize: () => sharp(),\n\
                     rotate: () => sharp(),\n\
                   };\n\
                 }\n\
                 export default sharp;\n\
                 export { sharp };\n",
            ),
        ],
    ),
    (
        "node-pty",
        &[
            (
                "package.json",
                r#"{"name":"node-pty","version":"1.1.0","type":"module","main":"index.js","exports":{".":{"import":"./index.js","default":"./index.js"}}}"#,
            ),
            (
                "index.js",
                "export function spawn() { throw new Error('node-pty: terminal sessions are unavailable on this platform'); }\n\
                 const nodePty = { spawn };\n\
                 export default nodePty;\n",
            ),
        ],
    ),
    (
        "koffi",
        &[
            (
                "package.json",
                r#"{"name":"koffi","version":"2.9.2-stub","type":"module","main":"index.js","exports":{".":"./index.js","./package.json":"./package.json"}}"#,
            ),
            (
                // koffi 是仅供 Windows FFI 的预编译 addon：dsh-subprocess-local /
                // dsh-win32-process 在模块顶层 `import koffi` 且顶层就执行
                // `koffi.pointer("void")`，所以 stub 的 pointer/struct/array 必须
                // 能返回空白对象（仅在 Windows 路径下才会真正调用到这些绑定）。
                "index.js",
                "const dummy = (name) => ({ name: String(name) });\n\
                 function unavailable(what) { throw new Error('koffi: ' + what + ' is unavailable on this platform'); }\n\
                 const koffi = {\n\
                   pointer: dummy,\n\
                   struct: (name, def) => ({ name: String(name), def }),\n\
                   array: dummy,\n\
                   register: unavailable,\n\
                   decode: unavailable,\n\
                   encode: unavailable,\n\
                   cdecl: unavailable,\n\
                 };\n\
                 export default koffi;\n",
            ),
        ],
    ),
    (
        "@deepseek-ai/dsh-sandbox-windows-acl",
        &[
            (
                "package.json",
                r#"{"name":"@deepseek-ai/dsh-sandbox-windows-acl","version":"1.0.0-stub","type":"module","main":"index.js","exports":{".":"./index.js","./runner":"./index.js","./package.json":"./package.json"}}"#,
            ),
            (
                "index.js",
                "export class AclWriteGrant {\n\
                   static create() { throw new Error('windows-acl sandbox is unavailable on this platform'); }\n\
                 }\n\
                 export function assertTempRootOutsideWorkspace() {}\n\
                 export function tempWriteSid() { return ''; }\n\
                 export function workspaceWriteSid() { return ''; }\n",
            ),
        ],
    ),
];

/// 递归删除目录/文件（尽力而为）。
pub fn remove_all(p: &Path) {
    let _ = std::fs::remove_dir_all(p);
    let _ = std::fs::remove_file(p);
}

/// 递归复制，保留符号链接。
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("meta {}: {e}", src.display()))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src).map_err(|e| format!("readlink {}: {e}", src.display()))?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::os::unix::fs::symlink(target, dst).map_err(|e| format!("symlink {}: {e}", dst.display()))?;
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
        for e in std::fs::read_dir(src).map_err(|e| format!("read_dir {}: {e}", src.display()))? {
            let e = e.map_err(|e| format!("entry: {e}"))?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::copy(src, dst).map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))?;
    }
    Ok(())
}

/// 复制时跳过名为 skip_name 的子目录（checkout 的 node_modules）。
fn copy_tree_skip(src: &Path, dst: &Path, skip_name: &str) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("meta {}: {e}", src.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return copy_tree(src, dst);
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    for e in std::fs::read_dir(src).map_err(|e| format!("read_dir {}: {e}", src.display()))? {
        let e = e.map_err(|e| format!("entry: {e}"))?;
        if e.file_name() == skip_name {
            continue;
        }
        copy_tree(&e.path(), &dst.join(e.file_name()))?;
    }
    Ok(())
}

/// 收集目录下所有文件（相对 root 的路径, 绝对路径）。
fn collect_all(root: &Path, out: &mut Vec<(String, PathBuf)>) {
    for p in walk_files(root) {
        let rel = p
            .strip_prefix(root)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push((rel, p));
    }
}

/// 深度优先遍历，返回所有普通文件路径（不含目录）。
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            // 用 symlink_metadata + is_dir 判断真实目录（不跟随符号链接，避免环）
            let is_dir = p
                .symlink_metadata()
                .map(|m| m.file_type().is_dir())
                .unwrap_or(false);
            if is_dir {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files
}

/// 剥离 node_modules 里的测试/文档/类型声明/sourcemap。
fn strip_tree(root: &Path) -> Result<usize, String> {
    let strip_dirs: &[&str] = &[
        "test", "tests", "__tests__", "spec", "bench", "benchmark", "docs", "examples",
        "fixtures", "coverage",
    ];
    let mut removed = 0usize;
    // 深度优先，删除目录后不再进入
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut subdirs = Vec::new();
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = p.is_dir();
            if is_dir {
                if strip_dirs.contains(&name.as_str()) {
                    remove_all(&p);
                    removed += 1;
                    continue;
                }
                subdirs.push(p);
                continue;
            }
            let low = name.to_lowercase();
            let drop = name.ends_with(".map")
                || name.ends_with(".d.ts")
                || low.ends_with(".md")
                || low.ends_with(".markdown")
                || name.starts_with("README")
                || name.starts_with("CHANGELOG")
                || name.starts_with("CHANGES")
                || name.starts_with("HISTORY")
                || name.starts_with("LICENSE")
                || name.starts_with("NOTICE")
                || name.starts_with("AUTHORS")
                || name.starts_with("CONTRIBUTING")
                || name.starts_with("SECURITY");
            if drop {
                let _ = std::fs::remove_file(&p);
                removed += 1;
            }
        }
        stack.extend(subdirs);
    }
    println!("stripped {removed} files");
    Ok(removed)
}

/// 把文本按行写入文件。
fn write_text(p: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(p, text).map_err(|e| format!("write {}: {e}", p.display()))
}

/// 文本替换（出现 1 次），返回是否命中。
fn patch_once(s: &mut String, old: &str, new: &str) -> bool {
    if let Some(pos) = s.find(old) {
        s.replace_range(pos..pos + old.len(), new);
        true
    } else {
        false
    }
}

/// 对 DSH 树应用 Android 文件系统补丁（link() -> rename()）。
/// 针对当前 DSH 0.1.2-rc.1 的实际源码模式。
fn patch_android_fs(root: &Path) -> Result<usize, String> {
    let nm = root.join("node_modules/@deepseek-ai");
    let targets = [
        (
            nm.join("dsh-session-persistence-jsonl/lib/index.js"),
            vec![
                (
                    "import { link, mkdir, mkdtemp, open, readFile, readdir, realpath, rm, stat, truncate } from \"node:fs/promises\";",
                    "import { mkdir, mkdtemp, open, readFile, readdir, realpath, rename, rm, stat, truncate } from \"node:fs/promises\";",
                ),
                (
                    "await link(tmp, finalPath);",
                    "/* Android: apps cannot hardlink (SELinux EACCES); rename is atomic on the same fs */\n\
                     \t\t\t\t\t\tawait rename(tmp, finalPath);",
                ),
            ],
        ),
        (
            nm.join("dsh-attachment-local/lib/index.js"),
            vec![
                (
                    "import { chmod, link, mkdir, open, readFile, rename, rm, unlink, writeFile } from \"node:fs/promises\";",
                    "import { chmod, mkdir, open, readFile, rename, rm, unlink, writeFile } from \"node:fs/promises\";",
                ),
                (
                    "await link(temporary, target);",
                    "/* Android: apps cannot hardlink (SELinux EACCES); rename is atomic on the same fs */\n\
                     \t\t\t\t\t\tawait rename(temporary, target);",
                ),
                (
                    "await unlink(temporary);",
                    "/* renamed above; the temp path no longer exists */\n\
                     \t\t\t\t\t\tawait unlink(temporary).catch(() => {});",
                ),
            ],
        ),
        (
            nm.join("dsh-fs-local/lib/index.js"),
            vec![(
                "const linkFile = internals.linkFile ?? link;",
                "const linkFile = internals.linkFile ?? (async (from, to) => {\n\
                 \t\t/* Android: apps cannot hardlink (SELinux EACCES); rename is atomic on\n\
                 \t\t   the same fs. The no-replace guarantee is relaxed (single-user app). */\n\
                 \t\ttry {\n\
                 \t\t\treturn await link(from, to);\n\
                 \t\t} catch (error) {\n\
                 \t\t\tif (error.code === \"EACCES\" || error.code === \"EPERM\" || error.code === \"EOPNOTSUPP\")\n\
                 \t\t\t\treturn rename(from, to);\n\
                 \t\t\tthrow error;\n\
                 \t\t}\n\
                 \t});",
            )],
        ),
    ];
    let mut n = 0usize;
    for (path, pairs) in targets {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                println!("  SKIP (absent): {} ({e})", path.display());
                continue;
            }
        };
        let mut s = text.clone();
        let mut applied = 0;
        for (old, new) in &pairs {
            if patch_once(&mut s, old, new) {
                applied += 1;
            } else {
                println!("  WARN: pattern not found in {}: {:?}", path.display(), &old[..old.len().min(60)]);
            }
        }
        if applied > 0 {
            write_text(&path, &s)?;
            n += applied;
        }
    }
    println!("android fs patch done, {n} replacements");
    Ok(n)
}

/// 裁剪 DSH 应用树：checkout -> dst（dsh-app）。
pub fn prepare_dsh_tree(checkout: &Path, termux_prefix: &Path, dst: &Path) -> Result<(), String> {
    if !checkout.join("lib").is_dir() {
        return Err(format!("checkout not found (need lib/): {}", checkout.display()));
    }
    if dst.exists() {
        remove_all(dst);
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;

    println!("copy checkout (no nested node_modules): {}", checkout.display());
    copy_tree_skip(checkout, dst, "node_modules")?;

    println!("copy nested node_modules ...");
    copy_tree(&checkout.join("node_modules"), &dst.join("node_modules"))?;

    let nm = dst.join("node_modules");
    for pkg in REMOVE_PACKAGES {
        let p = nm.join(pkg);
        if p.exists() {
            remove_all(&p);
            println!("removed: {pkg}");
        }
    }

    for (pkg, files) in STUBS {
        let p = nm.join(pkg);
        remove_all(&p);
        for (name, content) in *files {
            write_text(&p.join(name), content)?;
        }
        println!("stubbed: {pkg}");
    }

    // 平台预编译包移除（Android 上无产物，JS 层有回退）
    let scoped = nm.join("@deepseek-ai");
    if let Ok(rd) = std::fs::read_dir(&scoped) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let drop = (name.starts_with("node-addon-landlock-run-")
                && name != "node-addon-landlock-run")
                || (name.starts_with("node-addon-require-builtin-")
                    && name != "node-addon-require-builtin");
            if drop {
                remove_all(&e.path());
                println!("removed platform pkg: {name}");
            }
        }
    }

    // @vscode/ripgrep：Android 上 node 报告 process.platform==="android"，arch==="arm64"，
    // 需要 @vscode/ripgrep-android-arm64（保留 linux-arm64 以防 arch 探测差异）。
    // 用 Termux 前缀里的 aarch64 rg 构造这两个包。
    let rg_bin = termux_prefix.join("bin").join("rg");
    if !rg_bin.exists() {
        return Err(format!("termux rg not found: {}", rg_bin.display()));
    }
    let vscode_dir = nm.join("@vscode");
    let keep: &[&str] = &["ripgrep-android-arm64", "ripgrep-linux-arm64"];
    if let Ok(rd) = std::fs::read_dir(&vscode_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("ripgrep-") && !keep.contains(&name.as_str()) {
                remove_all(&e.path());
                println!("removed foreign rg platform pkg: {name}");
            }
        }
    }
    for pkg in keep {
        let rgdir = vscode_dir.join(pkg);
        remove_all(&rgdir);
        std::fs::create_dir_all(rgdir.join("bin")).map_err(|e| format!("mkdir: {e}"))?;
        std::fs::copy(&rg_bin, rgdir.join("bin").join("rg"))
            .map_err(|e| format!("copy rg: {e}"))?;
        let plat = pkg.strip_prefix("ripgrep-").unwrap_or(pkg);
        write_text(&rgdir.join("package.json"), &format!("{{\"name\":\"@vscode/ripgrep-{plat}\",\"version\":\"15.2.0\"}}\n"))?;
        println!("created rg platform pkg: @vscode/{pkg}");
    }

    strip_tree(&nm)?;
    patch_android_fs(dst)?;

    // 统计 node_modules 大小
    let mut total = 0u64;
    let mut files = 0usize;
    let mut all = Vec::new();
    collect_all(&nm, &mut all);
    for (_, p) in &all {
        if let Ok(m) = std::fs::metadata(p) {
            total += m.len();
            files += 1;
        }
    }
    println!("DONE -> {} ({} files, {:.1} MB node_modules)", dst.display(), files, total as f64 / 1048576.0);
    Ok(())
}

/// 打包 payload.zip：termux/ + dsh-app/。
pub fn zip_payload(termux_prefix: &Path, dsh_app: &Path, out: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let f = std::fs::File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));
    let mut count = 0usize;
    let mut total = 0u64;
    // termux 前缀是 termux/usr，zip 里要 termux/usr/... 路径
    let termux_root = termux_prefix
        .parent()
        .ok_or_else(|| "termux prefix has no parent".to_string())?;
    for (root, prefix) in [(termux_root, "termux"), (dsh_app, "dsh-app")] {
        let mut files = Vec::new();
        collect_all(root, &mut files);
        for (rel, path) in files {
            if rel.ends_with(".map") {
                continue;
            }
            let arc = format!("{prefix}/{rel}");
            zw.start_file(&arc, opts).map_err(|e| format!("zip add {arc}: {e}"))?;
            let mut f = std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
            std::io::copy(&mut f, &mut zw).map_err(|e| format!("zip write {arc}: {e}"))?;
            if let Ok(m) = std::fs::metadata(&path) {
                total += m.len();
            }
            count += 1;
        }
    }
    zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!(
        "zip done: {count} files, {:.1} MB raw -> {:.1} MB zip -> {}",
        total as f64 / 1048576.0,
        size as f64 / 1048576.0,
        out.display()
    );
    Ok(out.to_path_buf())
}

/// 校验 bin/ 下每个 ELF 的 DT_NEEDED 都能在 lib/（或系统库）里解析。
pub fn verify_needed(prefix: &Path) -> Result<(), String> {
    let bin = prefix.join("bin");
    let lib = prefix.join("lib");
    let mut have: Vec<String> = Vec::new();
    let rd = std::fs::read_dir(&lib).map_err(|e| format!("read_dir {}: {e}", lib.display()))?;
    for e in rd.flatten() {
        let p = e.path();
        let is_file = e.file_type().map(|t| t.is_file()).unwrap_or(false)
            || p.symlink_metadata().map(|m| m.file_type().is_symlink() && p.is_file()).unwrap_or(false);
        if is_file {
            have.push(e.file_name().to_string_lossy().to_string());
        }
    }
    let rd = std::fs::read_dir(&bin).map_err(|e| format!("read_dir {}: {e}", bin.display()))?;
    for e in rd.flatten() {
        let path = e.path();
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if &data[..4] != b"\x7fELF" {
            continue;
        }
        let needed = deb::elf_needed(&data);
        let name = e.file_name().to_string_lossy().to_string();
        let missing: Vec<&String> = needed
            .iter()
            .filter(|n| !have.contains(n) && !SYSTEM_LIBS.contains(&n.as_str()))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{name} missing DT_NEEDED: {} (have: {})",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                have.join(", ")
            ));
        }
        println!("{name}: needed {} -> all resolved", needed.join(", "));
    }
    println!("verify ok");
    Ok(())
}
