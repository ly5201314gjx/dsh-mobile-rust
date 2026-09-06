//! DSH Mobile Android 原生库：把 `dsh-core` 的逻辑以 JNI 方式暴露给 Java。
//!
//! 约定：所有 JNI 函数都带 `Java_com_rustdsh_mobile_NativeBridge_` 前缀，
//! 与 `glue/.../NativeBridge.java` 的 native 声明一一对应。

use std::path::PathBuf;

use dsh_core::artifacts;
use dsh_core::envfile;
use dsh_core::payload;
use dsh_core::port;
use dsh_core::server::ServerCfg;
use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use jni::JNIEnv;

fn str_of(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|v| v.into()).unwrap_or_default()
}

fn path_of(env: &mut JNIEnv, s: &JString) -> PathBuf {
    PathBuf::from(str_of(env, s))
}

/// 解压 payload.zip（assets 拷出的临时文件）到 filesDir/payload。
/// 返回 1 成功，0 失败。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshExtractPayload(
    mut env: JNIEnv,
    _class: JClass,
    tmp_zip: JString,
    dest_root: JString,
    state_file: JString,
) -> jint {
    match payload::extract_payload(
        &path_of(&mut env, &tmp_zip),
        &path_of(&mut env, &dest_root),
        &path_of(&mut env, &state_file),
    ) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// 启动内置 node 服务（幂等，立即返回；服务在后台线程运行）。
/// 若 payload 未就绪，会先解压（tmp_zip 必须已由 Java 拷好）。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshStartServer(
    mut env: JNIEnv,
    _class: JClass,
    node_bin: JString,
    app_dir: JString,
    files_dir: JString,
    payload_dir: JString,
    home_dir: JString,
    state_file: JString,
) -> jint {
    let cfg = ServerCfg {
        node_bin: path_of(&mut env, &node_bin),
        app_dir: path_of(&mut env, &app_dir),
        files_dir: path_of(&mut env, &files_dir),
        payload_dir: path_of(&mut env, &payload_dir),
        home_dir: path_of(&mut env, &home_dir),
        state_file: path_of(&mut env, &state_file),
    };
    dsh_core::server::start_server(cfg);
    1
}

/// 停止内置 node 服务。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshStopServer(
    _env: JNIEnv,
    _class: JClass,
) {
    dsh_core::server::stop_server();
}

/// 轮询等待端口可用（毫秒），返回 1/0。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshWaitPort(
    _env: JNIEnv,
    _class: JClass,
    port: jint,
    timeout_ms: jint,
) -> jint {
    if port::wait_for_port(port as u16, timeout_ms as u64) {
        1
    } else {
        0
    }
}

/// 单次端口连通性探测。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshIsPortOpen(
    _env: JNIEnv,
    _class: JClass,
    port: jint,
    timeout_ms: jint,
) -> jint {
    if port::is_port_open(port as u16, timeout_ms as u64) {
        1
    } else {
        0
    }
}

/// 写 API Key 到 $DSH_HOME/.env。成功返回 null，失败返回错误信息。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshWriteEnv<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass,
    home_dir: JString,
    api_key: JString,
) -> jstring {
    let err = envfile::write_env(&path_of(&mut env, &home_dir), &str_of(&mut env, &api_key));
    match err {
        None => std::ptr::null_mut(),
        Some(msg) => env
            .new_string(msg)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
    }
}

/// 列出收纳箱文件，返回 JSON 字符串。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshListArtifacts<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass,
    files_dir: JString,
) -> jstring {
    let json = artifacts::list_artifacts(&path_of(&mut env, &files_dir));
    env.new_string(json)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// 递归删除文件/目录，返回 1/0。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshDeletePath(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) -> jint {
    if dsh_core::delete_path(&path_of(&mut env, &path)) {
        1
    } else {
        0
    }
}

/// 返回版本号字符串。
#[no_mangle]
pub extern "system" fn Java_com_rustdsh_mobile_NativeBridge_dshVersion<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass,
) -> jstring {
    env.new_string(dsh_core::VERSION)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
