//! APK 构建链（Rust 重写原 tools/build_manual.py）：
//! aapt2 compile/link -> javac -> d8 -> 合并 dex + .so -> zipalign -> apksigner。
//! 外部依赖：Android SDK build-tools 34 + android-34 platform + JDK。

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ApkOptions {
    pub sdk: PathBuf,
    pub java_home: PathBuf,
    pub src: PathBuf,        // app/src/main（manifest/res/java/assets）
    pub out: PathBuf,        // 最终 apk 路径
    pub version_code: u32,
    pub version_name: String,
    pub keystore: PathBuf,
    pub keystore_pass: String,
    pub alias: String,
    pub native_so: Option<PathBuf>, // libdsh_mobile.so -> lib/arm64-v8a/
    pub tmp: PathBuf,
}

fn run(args: &[&str], log: &Path) -> Result<(), String> {
    println!(">> {}", args.join(" "));
    let out = Command::new(args[0])
        .args(&args[1..])
        .output()
        .map_err(|e| format!("spawn {}: {e}", args[0]))?;
    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("!!! FAILED rc={:?}", out.status.code());
        eprintln!("--- stdout ---\n{}", tail(&stdout, 4000));
        eprintln!("--- stderr ---\n{}", tail(&stderr, 4000));
        let _ = std::fs::write(log.with_extension("out"), &out.stdout);
        let _ = std::fs::write(log.with_extension("err"), &out.stderr);
        return Err(format!("command failed: {}", args.join(" ")));
    }
    Ok(())
}

fn tail(s: &str, n: usize) -> String {
    let b = s.as_bytes();
    if b.len() <= n {
        s.to_string()
    } else {
        String::from_utf8_lossy(&b[b.len() - n..]).to_string()
    }
}

/// 把目录下的所有文件（相对路径）打包进 zip。
pub fn zip_tree(zip_path: &Path, roots: &[(&Path, &str)]) -> Result<(), String> {
    let f = std::fs::File::create(zip_path).map_err(|e| format!("create {}: {e}", zip_path.display()))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (root, prefix) in roots {
        let mut files = Vec::new();
        collect_files(root, "", &mut files);
        for (rel, path) in files {
            let arc = if prefix.is_empty() { rel.clone() } else { format!("{prefix}/{rel}") };
            zw.start_file(&arc, opts).map_err(|e| format!("zip add {arc}: {e}"))?;
            let mut f = std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
            std::io::copy(&mut f, &mut zw).map_err(|e| format!("zip write {arc}: {e}"))?;
        }
    }
    zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(())
}

fn collect_files(root: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let dir = if prefix.is_empty() { root.to_path_buf() } else { root.join(prefix) };
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_files(root, &rel, out);
        } else {
            out.push((rel, p));
        }
    }
}

/// 查找目录下所有 .java 文件。
fn find_java(root: &Path, out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            find_java(&p, out);
        } else if p.extension().map(|x| x == "java").unwrap_or(false) {
            out.push(p);
        }
    }
}

pub fn build(opt: &ApkOptions) -> Result<PathBuf, String> {
    let bt = opt.sdk.join("build-tools").join("34.0.0");
    let platform_jar = opt.sdk.join("platforms").join("android-34").join("android.jar");
    let aapt2 = bt.join("aapt2");
    let zipalign = bt.join("zipalign");
    let d8_jar = bt.join("lib").join("d8.jar");
    let apksigner_jar = bt.join("lib").join("apksigner.jar");
    let javac = opt.java_home.join("bin").join("javac");
    let keytool = opt.java_home.join("bin").join("keytool");
    let java = opt.java_home.join("bin").join("java");

    for p in [&aapt2, &zipalign, &d8_jar, &apksigner_jar, &platform_jar, &javac, &keytool] {
        if !p.exists() {
            return Err(format!("missing tool: {}", p.display()));
        }
    }

    let tmp = &opt.tmp;
    if tmp.exists() {
        std::fs::remove_dir_all(tmp).map_err(|e| format!("clean tmp: {e}"))?;
    }
    std::fs::create_dir_all(tmp).map_err(|e| format!("mkdir tmp: {e}"))?;
    let log = tmp.join("last_fail");

    let res = opt.src.join("res");
    let compiled = tmp.join("compiled.zip");
    run(&[aapt2.to_str().unwrap(), "compile", "--dir", res.to_str().unwrap(), "-o", compiled.to_str().unwrap()], &log)?;

    let gen = tmp.join("gen");
    std::fs::create_dir_all(&gen).map_err(|e| format!("mkdir gen: {e}"))?;
    let base_apk = tmp.join("base.apk");
    let mut link_args = vec![
        aapt2.to_str().unwrap().to_string(),
        "link".into(),
        "-o".into(),
        base_apk.to_str().unwrap().to_string(),
        "-I".into(),
        platform_jar.to_str().unwrap().to_string(),
        "--manifest".into(),
        opt.src.join("AndroidManifest.xml").to_str().unwrap().to_string(),
        "--java".into(),
        gen.to_str().unwrap().to_string(),
        "--min-sdk-version".into(),
        "26".into(),
        "--target-sdk-version".into(),
        "28".into(),
        "--version-code".into(),
        opt.version_code.to_string(),
        "--version-name".into(),
        opt.version_name.clone(),
    ];
    let assets = opt.src.join("assets");
    if assets.is_dir() {
        link_args.push("-A".into());
        link_args.push(assets.to_str().unwrap().to_string());
    }
    link_args.push(compiled.to_str().unwrap().to_string());
    let args: Vec<&str> = link_args.iter().map(|s| s.as_str()).collect();
    run(&args, &log)?;

    // javac
    let classes = tmp.join("classes");
    std::fs::create_dir_all(&classes).map_err(|e| format!("mkdir classes: {e}"))?;
    let mut sources = Vec::new();
    find_java(&gen, &mut sources);
    find_java(&opt.src.join("java"), &mut sources);
    let srclist = tmp.join("sources.txt");
    let mut src_text = String::new();
    for s in &sources {
        src_text.push_str(&s.display().to_string());
        src_text.push('\n');
    }
    std::fs::write(&srclist, &src_text).map_err(|e| format!("write sources.txt: {e}"))?;
    run(&[
        javac.to_str().unwrap(),
        "-encoding", "UTF-8", "-source", "8", "-target", "8", "-nowarn",
        "-cp", platform_jar.to_str().unwrap(),
        "-d", classes.to_str().unwrap(),
        &format!("@{}", srclist.display()),
    ], &log)?;

    // d8：先把 classes 打成 jar
    let classes_jar = tmp.join("classes.jar");
    zip_tree(&classes_jar, &[(&classes, "")])?;
    let dexdir = tmp.join("dex");
    std::fs::create_dir_all(&dexdir).map_err(|e| format!("mkdir dex: {e}"))?;
    run(&[
        java.to_str().unwrap(), "-cp", d8_jar.to_str().unwrap(),
        "com.android.tools.r8.D8",
        "--lib", platform_jar.to_str().unwrap(),
        "--min-api", "26", "--output", dexdir.to_str().unwrap(),
        classes_jar.to_str().unwrap(),
    ], &log)?;

    // 合并 dex + 原生 so 进 base.apk
    let mut extra_files = vec![(dexdir.join("classes.dex"), "classes.dex".to_string())];
    if let Some(so) = &opt.native_so {
        if so.exists() {
            extra_files.push((so.clone(), "lib/arm64-v8a/libdsh_mobile.so".to_string()));
        }
    }
    append_zip_entries(&base_apk, &extra_files)?;

    // zipalign
    let aligned = tmp.join("aligned.apk");
    run(&[
        zipalign.to_str().unwrap(), "-f", "-p", "4",
        base_apk.to_str().unwrap(), aligned.to_str().unwrap(),
    ], &log)?;

    // keystore（首次生成）
    if !opt.keystore.exists() {
        run(&[
            keytool.to_str().unwrap(), "-genkeypair", "-v",
            "-keystore", opt.keystore.to_str().unwrap(),
            "-alias", &opt.alias, "-keyalg", "RSA", "-keysize", "2048",
            "-validity", "10000",
            "-storepass", &opt.keystore_pass, "-keypass", &opt.keystore_pass,
            "-dname", "CN=Rust DSH Mobile, OU=Dev, O=Dev, L=Dev, ST=Dev, C=CN",
        ], &log)?;
    }

    // apksigner
    if let Some(parent) = opt.out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir out: {e}"))?;
    }
    run(&[
        java.to_str().unwrap(), "-jar", apksigner_jar.to_str().unwrap(), "sign",
        "--ks", opt.keystore.to_str().unwrap(),
        "--ks-pass", &format!("pass:{}", opt.keystore_pass),
        "--ks-key-alias", &opt.alias,
        "--key-pass", &format!("pass:{}", opt.keystore_pass),
        "--out", opt.out.to_str().unwrap(),
        aligned.to_str().unwrap(),
    ], &log)?;

    Ok(opt.out.clone())
}

/// 把 (文件, 归档名) 追加进已有 zip。
fn append_zip_entries(zip_path: &Path, entries: &[(PathBuf, String)]) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(zip_path)
        .map_err(|e| format!("open {}: {e}", zip_path.display()))?;
    let mut zw = zip::ZipWriter::new_append(file).map_err(|e| format!("open zip append: {e}"))?;
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (src, arc) in entries {
        zw.start_file(arc, opts).map_err(|e| format!("zip add {arc}: {e}"))?;
        let mut f = std::fs::File::open(src).map_err(|e| format!("open {}: {e}", src.display()))?;
        std::io::copy(&mut f, &mut zw).map_err(|e| format!("zip write {arc}: {e}"))?;
    }
    zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(())
}
