package com.rustdsh.mobile;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;

/**
 * DSH 内置服务（v3.0，Rust 核心版）：
 * 首次运行把 assets/payload.zip（Termux Node 运行时 + 裁剪后的 DSH 应用树）
 * 解压到私有目录（NativeBridge.dshExtractPayload），然后以前台服务身份调用
 * NativeBridge.dshStartServer 在 Rust 后台线程 spawn node 启动 127.0.0.1:3080
 * 的 DSH Web 服务，实现完全脱离 PC 的独立运行。
 *
 * 服务状态由 Rust 写入 filesDir/dsh-server.state（running/starting/exited:N/
 * failed:msg/stopped），Java 侧读取该文件（并镜像到 SharedPreferences）供 UI 判断。
 */
public class DshServerService extends Service {

    public static final String ACTION_START = "com.rustdsh.mobile.action.START_SERVER";
    public static final String ACTION_STOP = "com.rustdsh.mobile.action.STOP_SERVER";
    /** 同一实例内原子重启：先停 Rust 服务再重新拉起，避免 STOP+START 两条命令竞态
     *  （竞态会让系统误判前台服务 5 秒未 startForeground，直接杀进程） */
    public static final String ACTION_RESTART = "com.rustdsh.mobile.action.RESTART_SERVER";

    /** SharedPreferences 中的状态值 */
    public static final String STATE_EXTRACTING = "extracting";
    public static final String STATE_READY = "ready";
    public static final String STATE_STARTING = "starting";
    public static final String STATE_RUNNING = "running";
    public static final String STATE_EXITED = "exited";
    public static final String STATE_FAILED = "failed";
    public static final String KEY_PAYLOAD_STATE = "payload_state";
    public static final String KEY_SERVER_STATE = "server_state";

    private static final String TAG = "DshServer";
    private static final int NOTIF_ID = 1;
    private static final String CHANNEL_ID = "dsh_server";
    private static final String PAYLOAD_ZIP = "payload.zip";
    private static final String PAYLOAD_MARKER = "payload.v1.ok";
    /** Rust 侧状态文件（filesDir 下） */
    private static final String STATE_FILE = "dsh-server.state";
    private static final String PAYLOAD_STATE_FILE = "payload.state";

    private static final Object EXTRACT_LOCK = new Object();

    // ---------- 路径工具 ----------

    public static File payloadDir(Context ctx) { return new File(ctx.getFilesDir(), "payload"); }
    public static File dshAppDir(Context ctx) { return new File(payloadDir(ctx), "dsh-app"); }
    public static File nodeBin(Context ctx) {
        return new File(payloadDir(ctx), "termux" + File.separator + "usr" + File.separator + "bin" + File.separator + "node");
    }
    public static File dshHomeDir(Context ctx) { return new File(ctx.getFilesDir(), "dsh-home"); }

    /** Rust 解析 node stdout 写出的带 launch token 的入口 URL 文件（filesDir/web-url.txt）。 */
    public static File webUrlFile(Context ctx) { return new File(ctx.getFilesDir(), "web-url.txt"); }

    /** 读取入口 URL；文件缺失/未就绪返回 null。 */
    public static String readWebUrl(Context ctx) {
        String s = readFile(webUrlFile(ctx));
        return s.startsWith("http://") ? s : null;
    }

    /** 轮询等待 Rust 写出带 token 的入口 URL（最多 timeoutMs）。 */
    public static boolean waitForWebUrl(Context ctx, long timeoutMs) {
        long deadline = System.currentTimeMillis() + timeoutMs;
        while (System.currentTimeMillis() < deadline) {
            if (readWebUrl(ctx) != null) return true;
            try { Thread.sleep(300); } catch (InterruptedException e) { return false; }
        }
        return false;
    }

    public static boolean isPayloadReady(Context ctx) {
        return new File(payloadDir(ctx), PAYLOAD_MARKER).exists();
    }

    /** 读取 Rust 写入的服务状态文件（filesDir/dsh-server.state）。 */
    public static String serverState(Context ctx) {
        String s = readFile(new File(ctx.getFilesDir(), STATE_FILE));
        if (!s.isEmpty()) return s;
        return ctx.getSharedPreferences(MainActivity.PREFS, Context.MODE_PRIVATE)
                .getString(KEY_SERVER_STATE, "");
    }

    /** 读取 Rust 写入的解压进度状态文件（filesDir/payload.state）。 */
    public static String payloadState(Context ctx) {
        String s = readFile(new File(ctx.getFilesDir(), PAYLOAD_STATE_FILE));
        if (!s.isEmpty()) return s;
        return ctx.getSharedPreferences(MainActivity.PREFS, Context.MODE_PRIVATE)
                .getString(KEY_PAYLOAD_STATE, "");
    }

    private static void putState(Context ctx, String key, String value) {
        ctx.getSharedPreferences(MainActivity.PREFS, Context.MODE_PRIVATE)
                .edit().putString(key, value).apply();
    }

    // ---------- Service 生命周期 ----------

    @Override
    public void onCreate() {
        super.onCreate();
        createChannel();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent != null ? intent.getAction() : ACTION_START;
        if (ACTION_STOP.equals(action)) {
            stopServer();
            stopSelf();
            return START_NOT_STICKY;
        }
        startForeground(NOTIF_ID, buildNotification());
        if (ACTION_RESTART.equals(action)) {
            // 原子重启：先停 Rust 服务再重新拉起，规避 STOP+START 竞态
            stopServer();
            startServer();
            return START_STICKY;
        }
        // 幂等：已在启动中/运行中就不要再拉起（Rust 侧本身也是幂等的）
        String st = readFile(new File(getFilesDir(), STATE_FILE));
        if (STATE_RUNNING.equals(st) || STATE_STARTING.equals(st)) {
            return START_STICKY;
        }
        startServer();
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        stopServer();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) { return null; }

    // ---------- 启动 / 停止 ----------

    /** 确保 payload 就绪后交给 Rust 启动内置服务（立即返回，服务在 Rust 后台线程运行）。 */
    private void startServer() {
        if (!isPayloadReady(this)) {
            if (!extractPayload()) {
                writeServerState(STATE_FAILED + ":payload-extract");
                return;
            }
        }
        writeServerState(STATE_STARTING);
        NativeBridge.dshStartServer(
                nodeBin(this).getAbsolutePath(),
                dshAppDir(this).getAbsolutePath(),
                getFilesDir().getAbsolutePath(),
                payloadDir(this).getAbsolutePath(),
                dshHomeDir(this).getAbsolutePath(),
                new File(getFilesDir(), STATE_FILE).getAbsolutePath());
        // Rust 内部在后台线程 spawn node，这里轮询端口并把状态刷成 running
        final Thread t = new Thread(new Runnable() {
            @Override public void run() {
                if (NativeBridge.dshWaitPort(3080, 120000) == 1) {
                    writeServerState(STATE_RUNNING);
                }
            }
        }, "dsh-port-wait");
        t.start();
    }

    /** 停止内置服务（Rust 负责杀 node 并回收后台线程）。 */
    private void stopServer() {
        try {
            NativeBridge.dshStopServer();
        } catch (Throwable t) {
            Log.w(TAG, "stop server failed", t);
        }
        writeServerState("stopped");
    }

    // ---------- payload 解压 ----------

    /**
     * 把 assets/payload.zip 拷到 filesDir/payload.zip.tmp，再交给 Rust 解压
     * 到 filesDir/payload（NativeBridge 内部幂等：payload.v1.ok 标记存在则直接成功）。
     * 多线程/多进程竞争由 EXTRACT_LOCK + 完成标记保护。返回是否成功。
     */
    private boolean extractPayload() {
        synchronized (EXTRACT_LOCK) {
            if (isPayloadReady(this)) return true;
            Log.i(TAG, "extract: begin");
            File root = payloadDir(this);
            if (!root.exists() && !root.mkdirs()) {
                putState(this, KEY_PAYLOAD_STATE, STATE_FAILED);
                return false;
            }
            putState(this, KEY_PAYLOAD_STATE, STATE_EXTRACTING + ":0");
            File tmpZip = new File(getFilesDir(), "payload.zip.tmp");
            try {
                // 1. 先把 zip 从 assets 拷出（assets 流不支持随机访问）
                long t0 = System.currentTimeMillis();
                try (InputStream in = getAssets().open(PAYLOAD_ZIP);
                     FileOutputStream out = new FileOutputStream(tmpZip)) {
                    byte[] buf = new byte[256 * 1024];
                    int n;
                    long copied = 0;
                    while ((n = in.read(buf)) > 0) {
                        out.write(buf, 0, n);
                        copied += n;
                        if (copied % (20L * 1024 * 1024) < 256 * 1024) {
                            Log.i(TAG, "extract: asset copy " + (copied / 1048576) + "MB");
                        }
                    }
                    out.flush();
                    Log.i(TAG, "extract: asset copy done " + (copied / 1048576)
                            + "MB in " + (System.currentTimeMillis() - t0) + "ms");
                }
                // 2. 交给 Rust 解压（进度写入 filesDir/payload.state）
                int rc = NativeBridge.dshExtractPayload(
                        tmpZip.getAbsolutePath(),
                        root.getAbsolutePath(),
                        new File(getFilesDir(), PAYLOAD_STATE_FILE).getAbsolutePath());
                if (rc != 1) {
                    Log.e(TAG, "extract: native failed");
                    putState(this, KEY_PAYLOAD_STATE, STATE_FAILED);
                    return false;
                }
                Log.i(TAG, "extract: complete");
                return true;
            } catch (Exception e) {
                Log.e(TAG, "extract failed", e);
                putState(this, KEY_PAYLOAD_STATE, STATE_FAILED + ":" + e.getMessage());
                return false;
            } finally {
                tmpZip.delete();
            }
        }
    }

    // ---------- 工具 ----------

    /** 轮询等待端口可用（转 Rust 实现）。 */
    public static boolean waitForPort(int port, long timeoutMs) {
        return NativeBridge.dshWaitPort(port, (int) timeoutMs) == 1;
    }

    public static boolean isPortOpen(int port, int timeoutMs) {
        return NativeBridge.dshIsPortOpen(port, timeoutMs) == 1;
    }

    private void writeServerState(String value) {
        writeFile(new File(getFilesDir(), STATE_FILE), value);
        putState(this, KEY_SERVER_STATE, value);
    }

    private static void writeFile(File f, String value) {
        try (FileOutputStream fos = new FileOutputStream(f)) {
            fos.write(value.getBytes("UTF-8"));
            fos.flush();
        } catch (Exception e) {
            Log.w(TAG, "write state failed", e);
        }
    }

    private static String readFile(File f) {
        if (f == null || !f.exists()) return "";
        try (FileInputStream in = new FileInputStream(f)) {
            byte[] buf = new byte[(int) Math.min(f.length(), 65536)];
            int n = in.read(buf);
            return new String(buf, 0, Math.max(n, 0), "UTF-8").trim();
        } catch (Exception e) {
            return "";
        }
    }

    private void createChannel() {
        if (Build.VERSION.SDK_INT >= 26) {
            NotificationManager nm = getSystemService(NotificationManager.class);
            if (nm != null) {
                NotificationChannel ch = new NotificationChannel(
                        CHANNEL_ID, getString(R.string.notif_channel),
                        NotificationManager.IMPORTANCE_LOW);
                ch.setDescription(getString(R.string.notif_channel_desc));
                nm.createNotificationChannel(ch);
            }
        }
    }

    private Notification buildNotification() {
        Notification.Builder b = Build.VERSION.SDK_INT >= 26
                ? new Notification.Builder(this, CHANNEL_ID)
                : new Notification.Builder(this);
        return b.setSmallIcon(R.drawable.ic_stat_dsh)
                .setContentTitle(getString(R.string.notif_title))
                .setContentText(getString(R.string.notif_text))
                .setOngoing(true)
                .build();
    }
}
