//! DSH Link 配对协议（Windows 守护进程 ↔ 安卓客户端）。
//!
//! 参考 [Paseo](https://github.com/getpaseo/paseo) 的「扫码配对 → 多端访问本地
//! daemon」模型，但协议自研、更精简、局域网优先：
//!
//! 1. Windows 守护进程启动时生成一个一次性 `PairingKey`，并把 `Link`
//!    编码成一个文本链接（同时作为二维码负载）；
//! 2. 安卓客户端扫描二维码 / 手动填入该链接，解析出 `host:port` 与密钥；
//! 3. 双方经 WebSocket/HTTP 通道使用 `Message` 报文（纵深由 daemon 层实现）。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 协议标识与版本。
pub const PROTOCOL_VERSION: u8 = 1;
/// 默认配对/控制服务端口（DSH web 默认 3080，与之分离）。
pub const DEFAULT_LINK_PORT: u16 = 5780;
/// 默认链接 scheme。
pub const SCHEME: &str = "dsh-link";
/// WSS（TLS 加密）链接 scheme，用于跨网络经 Cloudflare 隧道配对。
pub const SCHEME_WSS: &str = "dsh-link-wss";
/// 手动输入/浏览器打开的 http 前缀（兼容），同一链接也会被编码成该形态便于分享。
pub const SCHEME_HTTP: &str = "http";

/// 配对密钥：20 字节随机数，hex 40 位。作为预共享密钥参与通道认证。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingKey(pub [u8; 20]);

impl PairingKey {
    pub fn random() -> Self {
        use rand::RngCore;
        let mut b = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut b);
        PairingKey(b)
    }

    /// hex 编码（40 位小写）。
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// 从 hex 解析。
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() != 40 {
            return None;
        }
        let mut b = [0u8; 20];
        for (i, ch) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_val(ch[0])?;
            let lo = hex_val(ch[1])?;
            b[i] = (hi << 4) | lo;
        }
        Some(PairingKey(b))
    }
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Debug for PairingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 调试输出也打码，避免密钥误泄漏日志
        write!(f, "PairingKey({}…)", &self.to_hex()[..6])
    }
}

/// 一个可分享的配对链接；也等价于二维码负载。
///
/// `dsh-link://example.com:5780/#key=<hex>`（主形态，二维码，局域网）
/// `dsh-link-wss://example.com/#key=<hex>&web=<webhost>`（WSS/TLS 形态，跨网络经 Cloudflare 隧道；
///   `web` 是第二个隧道（DSH Web UI）的地址，可缺省）
/// `https://example.com/pair?key=<hex>`（浏览器/手动输入兼容形态）
#[derive(Clone, PartialEq, Eq)]
pub struct Link {
    pub host: String,
    pub port: u16,
    pub key: PairingKey,
    /// 是否附带端口（默认 false，使用 DEFAULT_LINK_PORT）。
    pub http_mode: bool,
    /// 是否走 WSS/TLS（true 时二维码用 dsh-link-wss://，安卓用 SSL Socket 连接）。
    pub tls: bool,
    /// 可选的 DSH Web UI 对外地址（第二个 Cloudflare 隧道），形如 `xxx.trycloudflare.com`。
    /// 缺省时手机端回落用 `host` 访问 Web UI。
    pub web_host: Option<String>,
}

impl Link {
    pub fn new(host: impl Into<String>, port: u16, key: PairingKey) -> Self {
        Link {
            host: host.into(),
            port,
            key,
            http_mode: false,
            tls: false,
            web_host: None,
        }
    }

    /// 便捷构造：WSS 链接（跨网络隧道）。
    pub fn secure(host: impl Into<String>, port: u16, key: PairingKey) -> Self {
        let mut l = Link::new(host, port, key);
        l.tls = true;
        l
    }

    /// 二维码负载 / 分享文本。
    pub fn to_qr(&self) -> String {
        let scheme = if self.http_mode {
            if self.tls {
                "https"
            } else {
                SCHEME_HTTP
            }
        } else if self.tls {
            SCHEME_WSS
        } else {
            SCHEME
        };
        // WSS 形态的 host 跟随 tls：默认 443，无需显式端口（CLOUDFLARE 80/443）
        let authority = if self.tls {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        // tls 形态把可选 web 隧道地址作为 `&web=` 参数携带
        let web_param = if self.tls {
            match &self.web_host {
                Some(w) => format!("&web={w}"),
                None => String::new(),
            }
        } else {
            String::new()
        };
        if self.http_mode {
            let web = match &self.web_host {
                Some(w) => format!("&web={w}"),
                None => String::new(),
            };
            format!(
                "{}://{}/pair?key={}{}",
                scheme,
                authority,
                self.key.to_hex(),
                web
            )
        } else {
            format!(
                "{}://{}/#key={}{}",
                scheme,
                authority,
                self.key.to_hex(),
                web_param
            )
        }
    }

    /// 从用户填入/扫描的文本解析链接。支持 `dsh-link://`、`dsh-link-wss://` 与 `http(s)://` 形态。
    pub fn parse(s: &str) -> Option<Link> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // 统一剥掉 scheme，抽出 authority 与 key；同时判定是否 TLS
        let lower = s.to_ascii_lowercase();
        let (rest, http_mode, tls): (&str, bool, bool) =
            if let Some(r) = lower.strip_prefix("dsh-link-wss://") {
                (&s["dsh-link-wss://".len()..], false, true)
            } else if let Some(_) = lower.strip_prefix("dsh-link://") {
                (&s["dsh-link://".len()..], false, false)
            } else if let Some(_) = lower.strip_prefix("https://") {
                (&s["https://".len()..], true, true)
            } else if let Some(_) = lower.strip_prefix("http://") {
                (&s["http://".len()..], true, false)
            } else {
                // 也允许直接剥掉 scheme 后就是 `host:port/...`
                (&s[..], false, false)
            };

        let rest = rest.split('/').next()?; // 去掉路径
        if rest.is_empty() {
            return None;
        }
        // key 可能在 fragment `#key=`（默认）、query `?key=`（http 形态）；
        // `tls=1` 查询参数可显式开启 TLS
        let key = extract_key(s)?;
        let tls = tls || s.contains("tls=1");

        // host[:port]，IPv6 用 [..]
        let (host, port) = split_host_port(rest)?;
        // WSS 默认端口 443（无显式端口时）
        let port = match port {
            Some(p) => p,
            None => {
                if tls {
                    443
                } else {
                    DEFAULT_LINK_PORT
                }
            }
        };
        // 可选 `web=` 参数（DSH Web UI 的第二个隧道地址）
        let web_host = extract_web(s);
        Some(Link {
            host,
            port,
            key,
            http_mode,
            tls,
            web_host,
        })
    }

    /// 供手机端连接的显式 host:port。
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

fn extract_key(s: &str) -> Option<PairingKey> {
    for marker in ["#key=", "?key=", "&key="] {
        if let Some(idx) = s.find(marker) {
            let tail = &s[idx + marker.len()..];
            // key 到下一个分隔符为止（# / ? / & / 空白）
            let end = tail
                .find(|c: char| c == '#' || c == '?' || c == '&' || c.is_whitespace())
                .unwrap_or(tail.len());
            if let Some(k) = PairingKey::from_hex(&tail[..end]) {
                return Some(k);
            }
        }
    }
    None
}

/// 提取可选的 `web=` DSH Web UI 隧道地址；不存在返回 None。
fn extract_web(s: &str) -> Option<String> {
    for marker in ["&web=", "?web=", "#web="] {
        if let Some(idx) = s.find(marker) {
            let tail = &s[idx + marker.len()..];
            let end = tail
                .find(|c: char| c == '#' || c == '?' || c == '&' || c.is_whitespace())
                .unwrap_or(tail.len());
            let w = tail[..end].trim();
            if !w.is_empty() {
                return Some(w.to_string());
            }
        }
    }
    None
}

/// `host[:port]` 拆分；支持 `[::1]` 方括号 IPv6。
fn split_host_port(s: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = s.strip_prefix('[') {
        // IPv6: [addr]:port
        let close = rest.find(']')?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = if let Some(p) = after.strip_prefix(':') {
            Some(p.parse::<u16>().ok()?)
        } else {
            None
        };
        return Some((host.to_string(), port));
    }
    if let Some(idx) = s.rfind(':') {
        // 普通 host:port（rfind 避免与 IPv6 内冒号混淆，但走上面分支）
        let host = &s[..idx];
        if host.is_empty() {
            return None;
        }
        let port = s[idx + 1..].parse::<u16>().ok()?;
        return Some((host.to_string(), Some(port)));
    }
    Some((s.to_string(), None))
}

/// 通道报文信封：`{ "ver": 1, "type": "...", ...fields }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    /// 客户端上线握手（连接建立后先发）。
    #[serde(rename = "hello")]
    Hello {
        client_id: String,
        name: String,
        role: Role,
    },
    /// 服务端回应握手。
    #[serde(rename = "hello_ack")]
    HelloAck { server_id: String, name: String },
    /// 远程发指令（抽象，具体承载由 DSH 会话层决定）。
    #[serde(rename = "send_msg")]
    SendMsg { session_id: Option<String>, text: String },
    /// 服务端把 DSH 会话列表推给客户端。
    #[serde(rename = "session_snap")]
    SessionSnap { sessions: Vec<Session> },
    /// 会话事件推送（新增/更新/结束）。
    #[serde(rename = "event")]
    Event { kind: String, session_id: Option<String> },
    /// 错误 / ACK 通用回复。
    #[serde(rename = "ack")]
    Ack { ok: bool, reason: Option<String> },
}

/// 协议版本放最外层的携带字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub ver: u8,
    #[serde(flatten)]
    pub msg: Message,
}

impl Envelope {
    pub fn wrap(msg: Message) -> Self {
        Envelope {
            ver: PROTOCOL_VERSION,
            msg,
        }
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
    pub fn from_json(s: &str) -> Option<Envelope> {
        let e: Envelope = serde_json::from_str(s).ok()?;
        if e.ver == PROTOCOL_VERSION {
            Some(e)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Server,
    Client,
}

/// 一个 DSH 会话的摘要（供手机远看进度）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub updated_unix: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hex_roundtrip() {
        let k = PairingKey::random();
        let h = k.to_hex();
        assert_eq!(h.len(), 40);
        assert_eq!(PairingKey::from_hex(&h), Some(k.clone()));
        assert_eq!(PairingKey::from_hex(&h.to_uppercase()), Some(k));
        assert_eq!(PairingKey::from_hex("abcd"), None);
        assert_eq!(PairingKey::from_hex(&"z".repeat(40)), None);
    }

    #[test]
    fn link_roundtrip() {
        let k = PairingKey::random();
        let l = Link::new("192.168.1.5", 5780, k.clone());
        let qr = l.to_qr();
        assert!(qr.starts_with("dsh-link://192.168.1.5:5780/#key="));
        let parsed = Link::parse(&qr).unwrap();
        assert_eq!(parsed.host, "192.168.1.5");
        assert_eq!(parsed.port, 5780);
        assert_eq!(parsed.key, k.clone());
        assert!(!parsed.tls);
        assert_eq!(parsed.web_host, None);
    }

    #[test]
    fn link_wss_roundtrip() {
        let k = PairingKey::random();
        let mut l = Link::secure("abc.trycloudflare.com", 443, k.clone());
        l.web_host = Some("web.trycloudflare.com".to_string());
        let qr = l.to_qr();
        assert!(qr.starts_with("dsh-link-wss://abc.trycloudflare.com/#key="), "qr={qr}");
        assert!(qr.contains("&web=web.trycloudflare.com"), "qr={qr}");
        // 注意 to_qr 的 `&web=` 实际在 #key=… 之后
        let parsed = Link::parse(&qr).unwrap();
        assert_eq!(parsed.host, "abc.trycloudflare.com");
        assert_eq!(parsed.port, 443);
        assert_eq!(parsed.key, k.clone());
        assert!(parsed.tls);
        assert_eq!(parsed.web_host.as_deref(), Some("web.trycloudflare.com"));
    }

    #[test]
    fn link_wss_parse_no_web() {
        let k = PairingKey::random();
        let qr = format!("dsh-link-wss://abc.trycloudflare.com/#key={}", k.to_hex());
        let l = Link::parse(&qr).unwrap();
        assert!(l.tls);
        assert_eq!(l.host, "abc.trycloudflare.com");
        assert_eq!(l.port, 443);
        assert_eq!(l.web_host, None);
    }

    #[test]
    fn link_http_parse() {
        let k = PairingKey::random();
        let url = format!("http://example.com:9000/pair?key={}", k.to_hex());
        let l = Link::parse(&url).unwrap();
        assert_eq!(l.host, "example.com");
        assert_eq!(l.port, 9000);
        assert_eq!(l.key, k);
        assert!(l.http_mode);
    }

    #[test]
    fn link_default_port() {
        let k = PairingKey::random();
        // 无端口的 http 形态 → 默认端口
        let l = Link::parse(&format!("dsh-link://host/#key={}", k.to_hex())).unwrap();
        assert_eq!(l.port, DEFAULT_LINK_PORT);
    }

    #[test]
    fn message_roundtrip() {
        let e = Envelope::wrap(Message::Hello {
            client_id: "c1".into(),
            name: "Pixel".into(),
            role: Role::Client,
        });
        let json = e.to_json();
        assert!(json.contains("\"type\":\"hello\""));
        let back = Envelope::from_json(&json).unwrap();
        match back.msg {
            Message::Hello { name, .. } => assert_eq!(name, "Pixel"),
            _ => panic!("wrong variant"),
        }
        assert_eq!(Envelope::from_json("{\"ver\":9,\"type\":\"hello\"}"), None);
    }
}