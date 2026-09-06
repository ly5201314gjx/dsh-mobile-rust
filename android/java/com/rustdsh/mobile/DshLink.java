package com.rustdsh.mobile;

/**
 * DSH Link 配对链接解析器（Java 镜像 Rust 端 dsh-link，保持两端一致）。
 *
 * 链接形态与 Rust 端 `Link::to_qr` / `Link::parse` 对应：
 *   dsh-link://<host>[:port]/#key=<40位hex>     ← 二维码 / 复制的主形态
 *   http(s)://<host>[:port]/pair?key=<hex>      ← 浏览器 / 手动输入兼容形态
 */
public final class DshLink {

    /** 默认配对端口（与 Rust 端 dsh-link::DEFAULT_LINK_PORT 一致）。 */
    public static final int DEFAULT_LINK_PORT = 5780;
    /** 桌面端 DSH Web 服务端口（dsh-desktop --web-port 默认值）。 */
    public static final int DEFAULT_WEB_PORT = 3080;

    public final String host;
    public final int linkPort;
    public final String key;
    public final boolean httpMode;
    /** 是否走 WSS/TLS（dsh-link-wss:// 或 https://）→ 连接用 SSL Socket。 */
    public final boolean tls;
    /** 可选的 DSH Web UI 对外地址（第二个 CF 隧道），缺省为 null。 */
    public final String webHost;

    private DshLink(String host, int linkPort, String key, boolean httpMode, boolean tls, String webHost) {
        this.host = host;
        this.linkPort = linkPort;
        this.key = key;
        this.httpMode = httpMode;
        this.tls = tls;
        this.webHost = webHost;
    }

    /** 解析一条配对链接；无法识别时返回 null。 */
    public static DshLink parse(String text) {
        if (text == null) return null;
        String s = text.trim();
        if (s.isEmpty()) return null;

        boolean httpMode;
        boolean tls;
        String rest;
        String lower = s.toLowerCase();
        if (lower.startsWith("dsh-link-wss://")) {
            httpMode = false;
            tls = true;
            rest = s.substring("dsh-link-wss://".length());
        } else if (lower.startsWith("dsh-link://")) {
            httpMode = false;
            tls = false;
            rest = s.substring("dsh-link://".length());
        } else if (lower.startsWith("https://")) {
            httpMode = true;
            tls = true;
            rest = s.substring("https://".length());
        } else if (lower.startsWith("http://")) {
            httpMode = true;
            tls = false;
            rest = s.substring("http://".length());
        } else {
            httpMode = false;
            // 兼容 `tls=1` 查询参数显式开启
            tls = s.contains("tls=1");
            rest = s;
        }

        // 去掉路径段
        int slash = rest.indexOf('/');
        String authority = slash >= 0 ? rest.substring(0, slash) : rest;
        if (authority.isEmpty()) return null;

        String key = extractKey(s);
        if (key == null) return null;

        String host = authority;
        int port = tls ? 443 : DEFAULT_LINK_PORT;
        // 支持 [ipv6]:port
        if (authority.startsWith("[")) {
            int close = authority.indexOf(']');
            if (close < 0) return null;
            host = authority.substring(1, close);
            String after = authority.substring(close + 1);
            if (after.startsWith(":")) {
                port = parsePort(after.substring(1));
            }
        } else {
            int idx = authority.lastIndexOf(':');
            if (idx > 0) {
                Integer p = parsePort(authority.substring(idx + 1));
                if (p != null) {
                    host = authority.substring(0, idx);
                    port = p;
                }
            }
        }
        if (host.isEmpty()) return null;
        return new DshLink(host, port, key, httpMode, tls, extractWeb(s));
    }

    private static Integer parsePort(String p) {
        try {
            int v = Integer.parseInt(p);
            return (v > 0 && v <= 65535) ? v : null;
        } catch (NumberFormatException e) {
            return null;
        }
    }

    private static String extractKey(String s) {
        int sub = s.indexOf("key=");
        if (sub < 0) return null;
        int start = sub + 4;
        int end = start;
        int n = s.length();
        while (end < n) {
            char c = s.charAt(end);
            if (c == '#' || c == '?' || c == '&' || Character.isWhitespace(c)) break;
            end++;
        }
        if (end - start != 40) return null;
        return s.substring(start, end);
    }

    /** 提取可选的 `web=`（DSH Web UI 隧道地址）；不存在返回 null。 */
    private static String extractWeb(String s) {
        int idx = s.indexOf("web=");
        if (idx < 0) return null;
        int start = idx + 4;
        int end = start;
        int n = s.length();
        while (end < n) {
            char c = s.charAt(end);
            if (c == '#' || c == '?' || c == '&' || Character.isWhitespace(c)) break;
            end++;
        }
        String w = s.substring(start, end).trim();
        return w.isEmpty() ? null : w;
    }

    /** 电脑端可远程操控的 DSH Web 地址。
     *  WSS/隧道形态：优先用 `webHost`（第二个 CF 隧道，https）；缺省回落 host https。 */
    public String webUrl() {
        if (tls) {
            String w = (webHost != null && !webHost.isEmpty()) ? webHost : host;
            return "https://" + w + "/";
        }
        return "http://" + host + ":" + DEFAULT_WEB_PORT + "/";
    }

    /** 配对服务（二维码/链接信息/通道）地址。 */
    public String pairBase() {
        if (tls) {
            // 隧道默认 443，无显式端口
            return "https://" + host;
        }
        return "http://" + host + ":" + linkPort;
    }

    @Override
    public String toString() {
        String scheme = tls ? "dsh-link-wss" : "dsh-link";
        // tls 形态默认 443：省略端口，与 to_qr 一致；web 隧道地址随 &web= 携带
        String base;
        if (tls) {
            String hostPart = (linkPort == 443) ? host : host + ":" + linkPort;
            base = scheme + "://" + hostPart + "/#key=" + key;
            if (webHost != null && !webHost.isEmpty()) base += "&web=" + webHost;
            return base;
        }
        return scheme + "://" + host + ":" + linkPort + "/#key=" + key;
    }
}