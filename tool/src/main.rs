//! dsh-tool：DSH Mobile 构建工具（Rust 重写原 Python 构建链）。
//!
//! 子命令：
//!   fetch-debs        <debs_dir>                   下载 Termux .deb
//!   extract-termux    <debs_dir> <prefix>          解包 termux 前缀（bin/lib/etc）
//!   patch-runpath     <prefix>                     RUNPATH -> $ORIGIN
//!   verify-needed     <prefix>                     校验 DT_NEEDED 完整性
//!   prepare-dsh       <checkout> <prefix> <dst>    裁剪 DSH 树（去原生包/注入 stub/rg 平台包/fs 补丁）
//!   zip-payload       <prefix> <dsh_app> <out.zip> 打包 payload.zip
//!   build-apk         (见 build_apk_help)

mod apk;
mod deb;
mod payload;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let r = match cmd {
        "fetch-debs" => {
            let debs = PathBuf::from(arg(&args, 2, "debs_dir"));
            payload::fetch_debs(&debs).map(|_| debs)
        }
        "extract-termux" => {
            let debs = PathBuf::from(arg(&args, 2, "debs_dir"));
            let prefix = PathBuf::from(arg(&args, 3, "prefix"));
            payload::extract_termux(&debs, &prefix).map(|_| prefix)
        }
        "patch-runpath" => {
            let prefix = PathBuf::from(arg(&args, 2, "prefix"));
            payload::patch_runpaths(&prefix).map(|_| prefix)
        }
        "verify-needed" => {
            let prefix = PathBuf::from(arg(&args, 2, "prefix"));
            payload::verify_needed(&prefix).map(|_| prefix)
        }
        "prepare-dsh" => {
            let checkout = PathBuf::from(arg(&args, 2, "checkout"));
            let prefix = PathBuf::from(arg(&args, 3, "prefix"));
            let dst = PathBuf::from(arg(&args, 4, "dst"));
            payload::prepare_dsh_tree(&checkout, &prefix, &dst).map(|_| dst)
        }
        "zip-payload" => {
            let prefix = PathBuf::from(arg(&args, 2, "prefix"));
            let dsh_app = PathBuf::from(arg(&args, 3, "dsh_app"));
            let out = PathBuf::from(arg(&args, 4, "out.zip"));
            payload::zip_payload(&prefix, &dsh_app, &out)
        }
        "build-apk" => {
            let opt = apk::ApkOptions {
                sdk: PathBuf::from(flag(&args, "--sdk", "/opt/android-sdk")),
                java_home: PathBuf::from(flag(&args, "--java-home", "/usr/lib/jvm/default")),
                src: PathBuf::from(arg(&args, 2, "src")),
                out: PathBuf::from(arg(&args, 3, "out.apk")),
                version_code: flag(&args, "--version-code", "30").parse().unwrap_or(30),
                version_name: flag(&args, "--version-name", "3.0"),
                keystore: PathBuf::from(flag(&args, "--keystore", "debug.keystore")),
                keystore_pass: flag(&args, "--ks-pass", "rustdsh"),
                alias: flag(&args, "--alias", "rustdsh"),
                native_so: flag_opt(&args, "--native-so").map(PathBuf::from),
                tmp: PathBuf::from(flag(&args, "--tmp", "/tmp/dshm-build")),
            };
            apk::build(&opt)
        }
        "help" | "-h" | "--help" => {
            print_help();
            std::process::exit(0);
        }
        _ => {
            print_help();
            std::process::exit(2);
        }
    };
    match r {
        Ok(v) => {
            println!("OK: {}", v.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            ExitCode::FAILURE
        }
    }
}

fn arg(args: &[String], i: usize, name: &str) -> String {
    args.get(i).cloned().unwrap_or_else(|| {
        eprintln!("missing argument: {name}");
        std::process::exit(2);
    })
}

fn flag(args: &[String], name: &str, def: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| def.to_string())
}

fn flag_opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_help() {
    println!(
        "dsh-tool — DSH Mobile 构建工具\n\
         \n\
         fetch-debs        <debs_dir>\n\
         extract-termux    <debs_dir> <prefix>\n\
         patch-runpath     <prefix>\n\
         verify-needed     <prefix>\n\
         prepare-dsh       <checkout> <prefix> <dst>\n\
         zip-payload       <prefix> <dsh_app> <out.zip>\n\
         build-apk         <src> <out.apk> [--sdk ..] [--java-home ..] [--version-code N]\n\
                           [--version-name X] [--keystore ks] [--ks-pass p] [--alias a]\n\
                           [--native-so libdsh_mobile.so] [--tmp dir]"
    );
}
