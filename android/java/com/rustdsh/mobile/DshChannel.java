package com.rustdsh.mobile;

import android.util.Base64;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.List;

/**
 * DSH Link 通道客户端：纯 Java 实现的 WebSocket 客户端 + DSH Link 报文协议，
 * 与电脑端守护进程 desktop/src/ws.rs（dsh-desktop 配对服务 /ws 路由）互通。
 *
 * 职责：
 *  1. WebSocket 握手（RFC 6455，客户端侧掩码帧）；
 *  2. 用配对密钥（hex）带进查询串做通道认证；
 *  3. 发 hello 上线 → 收 hello_ack（配对确认）→ 收 session_snap（电脑端会话摘要）；
 *  4. 可发 send_msg 向电脑端投递远程指令。
 *
 * 监听对象回调在通道线程触发，调用方需自行切换到 UI 线程。
 */
public final class DshChannel {

    private static final String WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

    /** 通道事件回调。 */
    public interface Listener {
        /** 握手成功且收到 hello_ack。 */
        void onOpen(String serverId, String serverName);
        /** 收到电脑端会话摘要。 */
        void onSessionSnap(List<SessionInfo> sessions);
        /** 收到 ack 应答。 */
        void onAck(boolean ok, String reason);
        /** 连接关闭 / 出错（reason 为用户可读描述）。 */
        void onClose(String reason);
    }

    /** 电脑端一个 DSH 会话的摘要（与 link/src/lib.rs 的 Session 对应）。 */
    public static final class SessionInfo {
        public final String id;
        public final String title;
        public final long updatedUnix;
        SessionInfo(String id, String title, long updatedUnix) {
            this.id = id;
            this.title = title;
            this.updatedUnix = updatedUnix;
        }
        @Override public String toString() { return title; }
    }

    private final String host;
    private final int port;
    private final String hexKey;
    private final Listener listener;

    private volatile boolean running = false;
    private Socket socket;
    private OutputStream out;
    private String clientId;

    public DshChannel(String host, int port, String hexKey, Listener listener) {
        this.host = host;
        this.port = port;
        this.hexKey = hexKey;
        this.listener = listener;
    }

    /** 建立连接并运行（非阻塞，内部线程工作）。 */
    public synchronized void start() {
        if (running) return;
        running = true;
        Thread t = new Thread(new Runnable() {
            @Override public void run() { runLoop(); }
        }, "dsh-channel");
        t.setDaemon(true);
        t.start();
    }

    /** 断开连接。 */
    public synchronized void stop() {
        running = false;
        if (socket != null) {
            try { socket.close(); } catch (IOException ignored) { }
        }
    }

    /** 向电脑端投递一条远程指令到指定会话（sessionId 可为 null）。 */
    public void sendMessage(String sessionId, String text) {
        sendText("{" +
            "\"ver\":1,\"type\":\"send_msg\"," +
            "\"session_id\":" + jsonNullable(sessionId) + "," +
            "\"text\":" + jsonString(text) + "}");
    }

    // ------------------------------------------------------------------ 握手

    private void runLoop() {
        Socket s = null;
        try {
            s = new Socket();
            s.connect(new InetSocketAddress(host, port), 8000);
            s.setSoTimeout(20000);
            socket = s;
            out = s.getOutputStream();

            String clientKey = randomKey();
            StringBuilder req = new StringBuilder();
            req.append("GET /ws?key=").append(hexKey).append(" HTTP/1.1\r\n");
            req.append("Host: ").append(host).append(":").append(port).append("\r\n");
            req.append("Upgrade: websocket\r\n");
            req.append("Connection: Upgrade\r\n");
            req.append("Sec-WebSocket-Key: ").append(clientKey).append("\r\n");
            req.append("Sec-WebSocket-Version: 13\r\n");
            req.append("\r\n");
            out.write(req.toString().getBytes(StandardCharsets.UTF_8));
            out.flush();

            String head = new String(readLinear(s.getInputStream()), StandardCharsets.ISO_8859_1);
            if (!head.startsWith("HTTP/1.1 101")) {
                fail("配对通道握手失败：" + (head.isEmpty() ? "空响应" : firstLine(head)));
                return;
            }
            String accept = header(head, "sec-websocket-accept");
            if (accept == null || !accept.equalsIgnoreCase(expectedAccept(clientKey))) {
                fail("配对通道校验失败（Sec-WebSocket-Accept 不匹配）");
                return;
            }

            // 上线握手
            clientId = "android-" + Long.toHexString(System.currentTimeMillis());
            sendText("{" +
                "\"ver\":1,\"type\":\"hello\"," +
                "\"client_id\":" + jsonString(clientId) + "," +
                "\"name\":" + jsonString(androidDeviceName()) + "," +
                "\"role\":\"client\"}");

            // 帧读循环
            frameLoop(s.getInputStream());
        } catch (IOException e) {
            if (running) fail("与电脑端连接断开：" + e.getMessage());
        } catch (Exception e) {
            if (running) fail("通道异常：" + e.getMessage());
        } finally {
            running = false;
            if (s != null) {
                try { s.close(); } catch (IOException ignored) { }
            }
        }
    }

    private void frameLoop(InputStream in) throws IOException {
        while (running) {
            int b0 = in.read();
            if (b0 < 0) break; // EOF
            int b1 = in.read();
            if (b1 < 0) break;
            int opcode = b0 & 0x0f;
            long len = b1 & 0x7fL;
            if (len == 126) {
                byte[] e = readFully(in, 2);
                len = ((e[0] & 0xff) << 8) | (e[1] & 0xff);
            } else if (len == 127) {
                byte[] e = readFully(in, 8);
                len = 0;
                for (byte x : e) len = (len << 8) | (x & 0xff);
            }
            byte[] mask = null;
            if ((b1 & 0x80) != 0) mask = readFully(in, 4); // 服务端一般不掩码，兜底仍处理
            if (len < 0 || len > (1 << 22)) { fail("通道帧过大，已断开"); return; }
            byte[] payload = len == 0 ? new byte[0] : readFully(in, (int) len);
            if (mask != null) {
                for (int i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
            }

            switch (opcode) {
                case 0x8: // close
                    fail("通道已关闭");
                    return;
                case 0x9: // ping → pong
                    sendRaw(0xA, payload);
                    break;
                case 0x1: // text
                    dispatch(new String(payload, StandardCharsets.UTF_8));
                    break;
                default: // 其它数据帧 / 控制帧忽略
                    break;
            }
        }
        if (running) fail("通道已关闭");
    }

    private void dispatch(String json) {
        if (json == null) return;
        String type = field(json, "type");
        if (type == null) return;
        if ("hello_ack".equals(type)) {
            String sid = field(json, "server_id");
            String name = field(json, "name");
            if (listener != null) listener.onOpen(sid == null ? "" : sid, name == null ? "" : name);
        } else if ("session_snap".equals(type)) {
            listenerOnSessions(json);
        } else if ("ack".equals(type)) {
            boolean ok = "true".equals(field(json, "ok"));
            String reason = jsonNullableField(json, "reason");
            if (listener != null) listener.onAck(ok, reason);
        }
        // 其它（event）忽略
    }

    private void listenerOnSessions(String json) {
        List<SessionInfo> list = new ArrayList<SessionInfo>();
        int idx = json.indexOf("sessions");
        int from = json.indexOf('[', idx >= 0 ? idx : 0);
        if (from >= 0) {
            int depth = 0;
            int start = -1;
            for (int i = from; i < json.length(); i++) {
                char c = json.charAt(i);
                if (c == '{') { if (depth == 0) start = i; depth++; }
                else if (c == '}') { depth--; if (depth == 0 && start >= 0) addSession(list, json.substring(start, i + 1)); }
            }
        }
        if (listener != null) listener.onSessionSnap(list);
    }

    private void addSession(List<SessionInfo> list, String seg) {
        String id = field(seg, "id");
        String title = field(seg, "title");
        long updated = 0;
        String u = field(seg, "updated_unix");
        if (u != null) { try { updated = Long.parseLong(u); } catch (NumberFormatException ignored) { } }
        if (id == null && title == null) return;
        list.add(new SessionInfo(id == null ? "" : id, title == null ? "" : title, updated));
    }

    // ------------------------------------------------------------------ 写帧

    private void sendText(String text) {
        sendRaw(0x1, text.getBytes(StandardCharsets.UTF_8));
    }

    private void sendRaw(int opcode, byte[] payload) {
        OutputStream o = out;
        if (o == null) return;
        try {
            byte[] mask = new byte[4];
            new SecureRandom().nextBytes(mask);
            ByteArrayOutputStream buf = new ByteArrayOutputStream();
            int b0 = 0x80 | opcode; // FIN
            buf.write(b0);
            int len = payload.length;
            if (len < 126) {
                buf.write(0x80 | len);
            } else if (len < 65536) {
                buf.write(0x80 | 126);
                buf.write((len >> 8) & 0xff);
                buf.write(len & 0xff);
            } else {
                buf.write(0x80 | 127);
                for (int i = 7; i >= 0; i--) buf.write((int) ((len >> (8 * i)) & 0xff));
            }
            buf.write(mask);
            for (int i = 0; i < payload.length; i++) buf.write(payload[i] ^ mask[i & 3]);
            o.write(buf.toByteArray());
            o.flush();
        } catch (IOException ignored) {
            // 写失败由读循环兜底断开
        }
    }

    // ------------------------------------------------------------------ 工具

    private void fail(final String reason) {
        running = false;
        if (listener != null) listener.onClose(reason);
    }

    private static byte[] readLinear(InputStream in) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        int prev = 0;
        int b;
        while ((b = in.read()) >= 0) {
            out.write(b);
            if (prev == '\r' && b == '\n' && (out.size() >= 4)) {
                // 已读到 \r\n\r\n
                byte[] all = out.toByteArray();
                if (all.length >= 4 && all[all.length - 1] == '\n'
                        && all[all.length - 2] == '\r'
                        && all[all.length - 3] == '\n'
                        && all[all.length - 4] == '\r') {
                    return all;
                }
            }
            prev = b;
        }
        throw new IOException("读取响应头提前结束");
    }

    private static byte[] readFully(InputStream in, int n) throws IOException {
        byte[] buf = new byte[n];
        int off = 0;
        while (off < n) {
            int r = in.read(buf, off, n - off);
            if (r < 0) throw new IOException("连接被关闭");
            off += r;
        }
        return buf;
    }

    private static String randomKey() {
        byte[] b = new byte[16];
        new SecureRandom().nextBytes(b);
        return Base64.encodeToString(b, Base64.NO_WRAP);
    }

    private static String expectedAccept(String clientKey) throws Exception {
        MessageDigest md = MessageDigest.getInstance("SHA-1");
        byte[] digest = md.digest((clientKey + WEBSOCKET_GUID).getBytes(StandardCharsets.ISO_8859_1));
        return Base64.encodeToString(digest, Base64.NO_WRAP);
    }

    private static String firstLine(String head) {
        int e = head.indexOf('\n');
        return e >= 0 ? head.substring(0, e).trim() : head.trim();
    }

    private static String header(String head, String name) {
        for (String line : head.split("\r\n")) {
            int c = line.indexOf(':');
            if (c > 0 && line.substring(0, c).trim().equalsIgnoreCase(name)) {
                return line.substring(c + 1).trim();
            }
        }
        return null;
    }

    /** 提取 JSON 里某字段的字符串值；不存在返回 null。 */
    private static String field(String json, String name) {
        String prefix = "\"" + name + "\":";
        int i = json.indexOf(prefix);
        if (i < 0) return null;
        i += prefix.length();
        while (i < json.length() && Character.isWhitespace(json.charAt(i))) i++;
        if (i < json.length() && json.charAt(i) == '"') {
            int j = i + 1;
            StringBuilder sb = new StringBuilder();
            while (j < json.length()) {
                char c = json.charAt(j);
                if (c == '\\') { sb.append(json.charAt(j + 1)); j += 2; continue; }
                if (c == '"') break;
                sb.append(c);
                j++;
            }
            return sb.toString();
        }
        // 非字符串（数字/布尔/null）
        int j = i;
        while (j < json.length()) {
            char c = json.charAt(j);
            if (c == ',' || c == '}' || Character.isWhitespace(c)) break;
            j++;
        }
        return json.substring(i, j);
    }

    /** 取可空字段：null → null，否则取字符串值。 */
    private static String jsonNullableField(String json, String name) {
        String v = field(json, name);
        return "null".equals(v) ? null : v;
    }

    private static String jsonString(String v) {
        if (v == null) return "null";
        StringBuilder sb = new StringBuilder("\"");
        for (int i = 0; i < v.length(); i++) {
            char c = v.charAt(i);
            if (c == '"' || c == '\\') { sb.append('\\'); sb.append(c); }
            else if (c == '\n') sb.append("\\n");
            else if (c == '\r') sb.append("\\r");
            else if (c == '\t') sb.append("\\t");
            else if (c < 0x20) sb.append(String.format("\\u%04x", (int) c));
            else sb.append(c);
        }
        return sb.append('"').toString();
    }

    private static String jsonNullable(String v) {
        return v == null ? "null" : jsonString(v);
    }

    /** 安卓设备名（简化：Build.MODEL 前缀的脱敏别名），避免把完整机型发到远端。 */
    private static String androidDeviceName() {
        try {
            String name = android.os.Build.MODEL;
            if (name == null || name.isEmpty()) name = "Android";
            String clean = name.replaceAll("[^\\p{Alnum} _-]", "").trim();
            return clean.isEmpty() ? "Android" : (clean.length() > 24 ? clean.substring(0, 24) : clean);
        } catch (Throwable t) {
            return "Android";
        }
    }
}