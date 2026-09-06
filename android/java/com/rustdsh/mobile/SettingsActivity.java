package com.rustdsh.mobile;

import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.RadioButton;
import android.widget.RadioGroup;
import android.widget.TextView;
import android.widget.Toast;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.net.ConnectException;
import java.net.HttpURLConnection;
import java.net.SocketTimeoutException;
import java.net.URL;
import java.net.UnknownHostException;
import java.nio.charset.StandardCharsets;
import java.util.List;

import javax.net.ssl.SSLException;

/** 设置页：运行模式（本机内置服务 / PC 服务器）、连接检测、API Key、服务状态与重启 */
public class SettingsActivity extends Activity {

    private RadioGroup modeGroup;
    private RadioButton radioLocal;
    private RadioButton radioPc;
    private EditText urlInput;
    private EditText apiKeyInput;
    private CheckBox keepOn;
    private TextView statusText;
    private TextView apiKeyHint;
    private TextView pcStatus;
    private TextView hintConn;
    private TextView pairScanTip;
    private TextView pairLabel;
    private EditText pairInput;
    private Button btnPair;
    private Button btnRestart;
    private Button btnTest;
    private TextView channelStatus;
    private TextView sessionStatus;
    private Button btnSend;
    private SharedPreferences prefs;

    /** 与电脑端 DSH 的配对通道（WebSocket）。 */
    private DshChannel channel;
    /** 最近一次成功解析的配对链接（含 host/linkPort/key）。 */
    private DshLink paired;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_settings);
        prefs = getSharedPreferences(MainActivity.PREFS, MODE_PRIVATE);

        radioLocal = findViewById(R.id.radio_local);
        radioPc = findViewById(R.id.radio_pc);
        modeGroup = findViewById(R.id.mode_group);
        urlInput = findViewById(R.id.url_input);
        apiKeyInput = findViewById(R.id.api_key_input);
        apiKeyHint = findViewById(R.id.api_key_hint);
        keepOn = findViewById(R.id.keep_on);
        statusText = findViewById(R.id.server_status);
        pcStatus = findViewById(R.id.pc_status);
        hintConn = findViewById(R.id.hint_conn);
        pairScanTip = findViewById(R.id.pair_scan_tip);
        pairLabel = findViewById(R.id.pair_label);
        pairInput = findViewById(R.id.pair_input);
        btnPair = findViewById(R.id.btn_pair);
        btnRestart = findViewById(R.id.btn_restart);
        btnTest = findViewById(R.id.btn_test);
        channelStatus = findViewById(R.id.channel_status);
        sessionStatus = findViewById(R.id.session_status);
        btnSend = findViewById(R.id.btn_send);

        boolean local = MainActivity.MODE_LOCAL.equals(
                prefs.getString(MainActivity.KEY_MODE, MainActivity.MODE_LOCAL));
        radioLocal.setChecked(local);
        radioPc.setChecked(!local);

        urlInput.setText(prefs.getString(MainActivity.KEY_URL, MainActivity.DEFAULT_URL));
        pairInput.setText(prefs.getString(MainActivity.KEY_PAIR_LINK, ""));
        apiKeyInput.setText(prefs.getString(MainActivity.KEY_API_KEY, ""));
        keepOn.setChecked(prefs.getBoolean(MainActivity.KEY_KEEP_ON, true));

        modeGroup.setOnCheckedChangeListener(new RadioGroup.OnCheckedChangeListener() {
            @Override public void onCheckedChanged(RadioGroup group, int checkedId) {
                toggleModeUi();
            }
        });

        findViewById(R.id.btn_save).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { save(); }
        });

        btnRestart.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { restartServer(); }
        });

        btnTest.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { testConnection(); }
        });

        btnPair.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { pairConnect(); }
        });

        btnSend.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { sendToPc(); }
        });

        findViewById(R.id.btn_artifacts).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) {
                startActivity(new Intent(SettingsActivity.this, ArtifactsActivity.class));
            }
        });

        toggleModeUi();

        TextView version = findViewById(R.id.version);
        version.setText(getString(R.string.app_name) + " v" + MainActivity.VERSION);
    }

    @Override
    protected void onResume() {
        super.onResume();
        refreshStatus();
        // 「连接电脑」模式回到本页时，若已有配对链接则自动恢复通道
        maybeReopenChannel();
    }

    @Override
    protected void onDestroy() {
        closeChannel();
        super.onDestroy();
    }

    private boolean localChecked() { return radioLocal.isChecked(); }

    /** 按模式切换界面：本机模式显示重启/API Key，连接电脑模式显示检测按钮与指引 */
    private void toggleModeUi() {
        boolean local = localChecked();
        btnRestart.setVisibility(local ? View.VISIBLE : View.GONE);
        apiKeyInput.setEnabled(local);
        apiKeyHint.setEnabled(local);
        btnTest.setVisibility(local ? View.GONE : View.VISIBLE);
        pcStatus.setVisibility(local ? View.GONE : View.VISIBLE);
        hintConn.setVisibility(local ? View.GONE : View.VISIBLE);
        pairScanTip.setVisibility(local ? View.GONE : View.VISIBLE);
        pairLabel.setVisibility(local ? View.GONE : View.VISIBLE);
        pairInput.setVisibility(local ? View.GONE : View.VISIBLE);
        btnPair.setVisibility(local ? View.GONE : View.VISIBLE);
        int chanVis = local ? View.GONE : View.VISIBLE;
        channelStatus.setVisibility(chanVis);
        sessionStatus.setVisibility(chanVis);
        btnSend.setVisibility(chanVis);
        if (local) {
            closeChannel();
        } else if (channel == null) {
            // 未建立通道时展示配对引导
            String stored = prefs.getString(MainActivity.KEY_PAIR_LINK, "");
            if (stored != null && !stored.isEmpty()) {
                channelStatus.setText(R.string.channel_connecting);
            } else {
                channelStatus.setText(R.string.channel_need_pair);
            }
        }
        refreshStatus();
    }

    private void refreshStatus() {
        if (!localChecked()) {
            statusText.setText(R.string.status_pc_note);
            return;
        }
        btnRestart.setEnabled(true);
        String st = DshServerService.serverState(this);
        String pt = DshServerService.payloadState(this);
        if (pt.startsWith(DshServerService.STATE_EXTRACTING)) {
            String pct = pt.length() > pt.indexOf(':') + 1 ? pt.substring(pt.indexOf(':') + 1) : "";
            statusText.setText(getString(R.string.status_extracting) + " " + pct + "%");
        } else if (pt.startsWith(DshServerService.STATE_FAILED)) {
            statusText.setText(getString(R.string.status_failed));
        } else if (st == null || st.isEmpty()) {
            statusText.setText(DshServerService.isPayloadReady(this)
                    ? R.string.status_ready : R.string.status_extracting);
        } else if (st.equals(DshServerService.STATE_RUNNING)) {
            statusText.setText(R.string.status_running);
        } else if (st.startsWith(DshServerService.STATE_EXITED)) {
            statusText.setText(getString(R.string.status_exited) + " (" + st + ")");
        } else if (st.equals("stopped")) {
            statusText.setText(R.string.status_stopped);
        } else if (st.startsWith(DshServerService.STATE_FAILED)) {
            statusText.setText(getString(R.string.status_failed) + "\n" + st);
        } else {
            statusText.setText(getString(R.string.status_starting));
        }
    }

    private void save() {
        boolean local = localChecked();
        String url = normalizeUrl(urlInput.getText().toString().trim());
        String apiKey = apiKeyInput.getText().toString().trim();

        prefs.edit()
                .putString(MainActivity.KEY_MODE, local ? MainActivity.MODE_LOCAL : MainActivity.MODE_PC)
                .putString(MainActivity.KEY_URL, url)
                .putString(MainActivity.KEY_API_KEY, apiKey)
                .putBoolean(MainActivity.KEY_KEEP_ON, keepOn.isChecked())
                .apply();

        if (local) {
            writeApiKeyEnv(apiKey);
            restartServer();
            Toast.makeText(this, R.string.saved_local, Toast.LENGTH_SHORT).show();
            finish();
        } else {
            // 切到「连接电脑」：停掉本机服务，释放 127.0.0.1:3080 给 adb reverse 隧道
            Intent stop = new Intent(this, DshServerService.class);
            stop.setAction(DshServerService.ACTION_STOP);
            startService(stop);
            // 连接电脑模式：留在设置页，让用户直接看到检测结果
            Toast.makeText(this, R.string.saved_testing, Toast.LENGTH_SHORT).show();
            testConnection();
        }
    }

    private static String normalizeUrl(String url) {
        if (url == null) url = "";
        url = url.trim();
        if (url.isEmpty()) url = MainActivity.DEFAULT_URL;
        if (!url.startsWith("http://") && !url.startsWith("https://")) url = "http://" + url;
        return url;
    }

    /** 连接探测结果：类别用于决定是否重试（切换模式的瞬间连接会被重置/拒绝） */
    private static class ProbeResult {
        final String kind;   // OK / OK_UNKNOWN / HTTP / REFUSED / TIMEOUT / HOST / SSL / ERR
        final String text;
        ProbeResult(String kind, String text) { this.kind = kind; this.text = text; }
    }

    /** 解析并保存一条 DSH Link 配对链接，切到「连接电脑」后检测连接。 */
    private void pairConnect() {
        String text = pairInput.getText().toString().trim();
        DshLink dl = DshLink.parse(text);
        if (dl == null) {
            Toast.makeText(this, R.string.pair_invalid, Toast.LENGTH_LONG).show();
            return;
        }
        prefs.edit()
                .putString(MainActivity.KEY_MODE, MainActivity.MODE_PC)
                .putString(MainActivity.KEY_URL, dl.webUrl())
                .putString(MainActivity.KEY_PAIR_LINK, dl.toString())
                .apply();
        // 停掉本机服务，释放 127.0.0.1:3080
        Intent stop = new Intent(this, DshServerService.class);
        stop.setAction(DshServerService.ACTION_STOP);
        startService(stop);

        radioPc.setChecked(true);
        urlInput.setText(dl.webUrl());
        pcStatus.setText(getString(R.string.pair_ok, dl.host));
        openChannelFor(dl); // 建立 DSH Link 双向通道
        testConnection();
    }

    /** 用配对链接建立/重建与电脑端的 DSH Link 通道。 */
    private void openChannelFor(final DshLink dl) {
        if (dl == null) return;
        if (channel != null &&
                dl.host.equals(paired != null ? paired.host : null)
                && dl.linkPort == (paired != null ? paired.linkPort : -1)
                && dl.key.equals(paired != null ? paired.key : null)) {
            return; // 相同配对目标，复用当前通道
        }
        closeChannel();
        paired = dl;
        channelStatus.setText(R.string.channel_connecting);
        sessionStatus.setText(R.string.session_idle);
        DshChannel c = new DshChannel(dl.host, dl.linkPort, dl.key, dl.tls, new DshChannel.Listener() {
            @Override public void onOpen(String serverId, String serverName) {
                runOnUiThread(new Runnable() {
                    @Override public void run() {
                        channelStatus.setText(getString(R.string.channel_ok, serverName));
                    }
                });
            }
            @Override public void onSessionSnap(List<DshChannel.SessionInfo> sessions) {
                runOnUiThread(new Runnable() {
                    @Override public void run() {
                        if (sessions == null || sessions.isEmpty()) {
                            sessionStatus.setText(R.string.session_zero);
                        } else {
                            sessionStatus.setText(getString(R.string.session_count, sessions.size()));
                        }
                    }
                });
            }
            @Override public void onAck(boolean ok, String reason) {
                runOnUiThread(new Runnable() {
                    @Override public void run() {
                        if (ok) {
                            sessionStatus.setText(R.string.send_ack_ok);
                        } else {
                            sessionStatus.setText(getString(R.string.send_ack_fail,
                                    reason == null ? "" : reason));
                        }
                    }
                });
            }
            @Override public void onClose(String reason) {
                runOnUiThread(new Runnable() {
                    @Override public void run() {
                        channelStatus.setText(reason == null ? "" : reason);
                    }
                });
            }
        });
        channel = c;
        c.start();
    }

    /** 回到本页且处于「连接电脑」模式时，用已保存配对链接自动恢复通道。 */
    private void maybeReopenChannel() {
        if (localChecked()) return;
        if (channel != null) return; // 已连接
        String stored = prefs.getString(MainActivity.KEY_PAIR_LINK, "");
        if (stored == null || stored.isEmpty()) return;
        DshLink dl = DshLink.parse(stored);
        if (dl != null) openChannelFor(dl);
    }

    /** 关闭当前通道。 */
    private void closeChannel() {
        if (channel != null) {
            channel.stop();
            channel = null;
        }
        paired = null;
    }

    /** 向电脑端发一条测试指令，验证双向通道。 */
    private void sendToPc() {
        if (channel == null) {
            Toast.makeText(this, R.string.send_not_open, Toast.LENGTH_LONG).show();
            return;
        }
        sessionStatus.setText(R.string.send_sent);
        channel.sendMessage(null, "dsh-link ping from android @" + System.currentTimeMillis());
    }

    /** 「连接电脑」模式的连接检测：后台 HTTP 探测，把失败原因翻译成人话 */
    private void testConnection() {
        final String url = normalizeUrl(urlInput.getText().toString());
        pcStatus.setText(R.string.pc_status_testing);
        new Thread(new Runnable() {
            @Override public void run() {
                ProbeResult r = probeUrl(url);
                // 保存后隧道（adb reverse / 本机服务让位）可能在数秒内才就绪，
                // 对瞬时性失败（拒绝/连接重置）稍候重试两次，避免误报
                int tries = 0;
                while ((r.kind.equals("REFUSED") || r.kind.equals("ERR")) && tries < 2) {
                    try { Thread.sleep(2000); } catch (InterruptedException e) { break; }
                    r = probeUrl(url);
                    tries++;
                }
                final String text = r.text;
                runOnUiThread(new Runnable() {
                    @Override public void run() {
                        pcStatus.setText(text);
                    }
                });
            }
        }).start();
    }

    private ProbeResult probeUrl(String url) {
        HttpURLConnection conn = null;
        try {
            URL u = new URL(url);
            conn = (HttpURLConnection) u.openConnection();
            conn.setConnectTimeout(3000);
            conn.setReadTimeout(5000);
            conn.setInstanceFollowRedirects(true);
            conn.setRequestMethod("GET");
            conn.setRequestProperty("User-Agent", "DSH-Mobile-ConnectionTest/2.7");
            int code = conn.getResponseCode();
            if (code == 200) {
                InputStream in = conn.getInputStream();
                StringBuilder sb = new StringBuilder();
                byte[] buf = new byte[16384];
                int n;
                while ((n = in.read(buf)) > 0 && sb.length() < 300000) {
                    sb.append(new String(buf, 0, n, StandardCharsets.UTF_8));
                }
                in.close();
                String body = sb.toString();
                if (body.contains("__DSH_BOOT__") || body.contains("DeepSeek Harness")) {
                    return new ProbeResult("OK", getString(R.string.pc_status_ok));
                }
                return new ProbeResult("OK_UNKNOWN", getString(R.string.pc_status_ok_unknown));
            }
            return new ProbeResult("HTTP", getString(R.string.pc_status_http, code));
        } catch (UnknownHostException e) {
            return new ProbeResult("HOST", getString(R.string.pc_status_host));
        } catch (SocketTimeoutException e) {
            return new ProbeResult("TIMEOUT", getString(R.string.pc_status_timeout));
        } catch (ConnectException e) {
            return new ProbeResult("REFUSED", getString(R.string.pc_status_refused));
        } catch (SSLException e) {
            return new ProbeResult("SSL", getString(R.string.pc_status_ssl));
        } catch (IOException e) {
            return new ProbeResult("ERR", getString(R.string.pc_status_err, String.valueOf(e.getMessage())));
        } finally {
            if (conn != null) conn.disconnect();
        }
    }

    /** 把 API Key 交给 Rust 核心写入 $DSH_HOME/.env（权限 600；null 表示成功） */
    private void writeApiKeyEnv(String apiKey) {
        File home = DshServerService.dshHomeDir(this);
        if (!home.exists()) home.mkdirs();
        String err = NativeBridge.dshWriteEnv(home.getAbsolutePath(), apiKey);
        if (err != null) {
            Toast.makeText(this, "写入 .env 失败: " + err, Toast.LENGTH_LONG).show();
        }
    }

    private void restartServer() {
        if (!localChecked()) return;
        // 单条 RESTART 命令原子重启（服务内部先停 Rust 服务再拉起），
        // 避免 STOP 后立刻 startForegroundService 的竞态导致系统杀进程
        Intent restart = new Intent(this, DshServerService.class);
        restart.setAction(DshServerService.ACTION_RESTART);
        if (Build.VERSION.SDK_INT >= 26) {
            startForegroundService(restart);
        } else {
            startService(restart);
        }
        statusText.setText(R.string.status_starting);
    }
}
