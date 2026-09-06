package com.rustdsh.mobile;

/**
 * Rust 核心库（libdsh_mobile.so）的 JNI 桥。
 * 方法签名必须与 app/src/lib.rs 的导出符号一一对应：
 *   Java_com_rustdsh_mobile_NativeBridge_<method>
 */
public final class NativeBridge {

    static {
        System.loadLibrary("dsh_mobile");
    }

    private NativeBridge() {
    }

    /** 解压 payload.zip 到 destRoot，进度写 stateFile。返回 1 成功 / 0 失败。 */
    public static native int dshExtractPayload(String tmpZip, String destRoot, String stateFile);

    /** 启动内置 node 服务（幂等，立即返回；服务在 Rust 后台线程运行）。返回 1。 */
    public static native int dshStartServer(String nodeBin, String appDir, String filesDir,
                                            String payloadDir, String homeDir, String stateFile);

    /** 停止内置 node 服务。 */
    public static native void dshStopServer();

    /** 轮询等待端口可用（毫秒），返回 1/0。 */
    public static native int dshWaitPort(int port, int timeoutMs);

    /** 单次端口连通性探测，返回 1/0。 */
    public static native int dshIsPortOpen(int port, int timeoutMs);

    /** 写 API Key 到 $DSH_HOME/.env。成功返回 null，失败返回错误信息。 */
    public static native String dshWriteEnv(String homeDir, String apiKey);

    /** 列出收纳箱文件，返回 JSON：{"files":[{"rel","size","mtime"}]}。 */
    public static native String dshListArtifacts(String filesDir);

    /** 递归删除文件/目录，返回 1/0。 */
    public static native int dshDeletePath(String path);

    /** 返回版本号字符串。 */
    public static native String dshVersion();
}
