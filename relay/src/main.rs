//! DSH Link 中继（relay）：让 Windows 守护进程与安卓客户端在**不同网络**也能配对。
//!
//! 模型参考 [Paseo](https://github.com/getpaseo/paseo) 的 relay：两端**都主动出站连到中继**，
//! 中继按配对密钥（`GET /ws?key=<hex>`）撮合两端，之后在中继两端之间做**帧级双向透明转发**
//! （只透传 opcode 与 payload，不解业务）。业务握手/报文（hello、session_snap、send_msg…）
//! 完全由两端自己完成，中继看不到也不关心内容。
//!
//! 安全边界：本阶段中转**透明转发**（relay 可读内容），端到端加密（Curve25519 ECDH +
//! XSalsa20-Poly1305，参考 paseo）为后续增强；届时两端握手后加密载荷再经此转发。
//!
//! 用法：`dsh-relay --port 5781`
//! 手机/电脑端配对链接形如 `dsh-link://<relayHost>:<relayPort>/#key=<hex>`。

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const DEFAULT_PORT: u16 = 5781;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port: u16 = flag(&args, "--port", &DEFAULT_PORT.to_string())
        .parse()
        .unwrap_or(DEFAULT_PORT);

    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[dsh-relay] bind 0.0.0.0:{port} failed: {e}");
            std::process::exit(1);
        }
    };
    println!("[dsh-relay] listening on 0.0.0.0:{port}");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let r = rooms.clone();
                std::thread::spawn(move || handle_conn(s, r));
            }
            Err(_) => continue,
        }
    }
}

fn flag(args: &[String], name: &str, default: &str) -> String {
    for i in 0..args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned().unwrap_or_else(|| default.into());
        }
    }
    default.into()
}

/// 一个待撮合的连接（同 room 的第一个来者）。
struct Waiting {
    stream: TcpStream,
    pending: Vec<u8>,
}

/// key(hex) → 等待配对的连接。至多存 1 个；第二个进来即撮合并发起双向转发。
type Rooms = Arc<Mutex<HashMap<String, Waiting>>>;

fn handle_conn(stream: TcpStream, rooms: Rooms) {
    let mut stream = stream;
    // 读 HTTP 头（GET /ws?key=…），返回 (头, 同包剩余字节 pending)
    let (head, pending) = match read_http_head(&mut stream) {
        Ok(v) => v,
        Err(_) => return,
    };
    // 从请求行提取 key
    let key = match request_line(&head)
        .and_then(ws_key_from_query)
    {
        Some(k) if !k.is_empty() => k,
        _ => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    };

    // WebSocket 握手（中继扮演 server）
    let client_ws_key = match header_val(&head, "Sec-WebSocket-Key") {
        Some(k) if !k.is_empty() => k.to_owned(),
        _ => {
            let _ = stream.write_all(b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    };
    let accept = ws_accept(&client_ws_key);
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\
         \r\n"
    );
    if stream.write_all(resp.as_bytes()).is_err() {
        return;
    }
    if stream.flush().is_err() {
        return;
    }

    // 撮合
    let mut locked = match rooms.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(waiting) = locked.remove(&key) {
        // 有两个连接 -> 互相转发
        drop(locked);
        relay_pair(waiting, stream, pending);
    } else {
        locked.insert(key, Waiting { stream, pending });
    }
}

fn relay_pair(first: Waiting, second: TcpStream, second_pending: Vec<u8>) {
    // 两端分别 try_clone 出读写句柄
    if let (Ok(a_w), Ok(a_r)) = (first.stream.try_clone(), first.stream.try_clone()) {
        if let (Ok(b_w), Ok(b_r)) = (second.try_clone(), second.try_clone()) {
            // A -> B
            std::thread::spawn(move || pump(first.pending, a_r, b_w));
            // B -> A
            std::thread::spawn(move || pump(second_pending, b_r, a_w));
        }
    }
}

/// 从 `pending`（握手同包剩余字节）往下持续读 A 的帧，原样（不掩码）转发给 B。
fn pump(pending: Vec<u8>, read_side: TcpStream, mut write_side: TcpStream) {
    let mut reader = BufReader::new(SeqReader {
        pending: std::collections::VecDeque::from(pending),
        inner: read_side,
    });
    loop {
        let frame = match read_frame(&mut reader) {
            Some(f) => f,
            None => return, // 对端关闭/出错
        };
        match frame.opcode {
            0x9 => {
                // ping → 中继代为应答 pong，不透传（两端都把自己当中继的 client）
                let _ = send_frame(&mut write_side, 0xA, &frame.payload);
            }
            0x8 => {
                // close → 透传给对端并终止本方向
                let _ = send_frame(&mut write_side, 0x8, &frame.payload);
                return;
            }
            _ => {
                if send_frame(&mut write_side, frame.opcode, &frame.payload).is_err() {
                    return;
                }
            }
        }
    }
}

struct Frame {
    opcode: u8,
    payload: Vec<u8>,
}

/// 读一个 WebSocket 帧（同时兼容掩码/未掩码），掩码帧先解开。
fn read_frame<R: Read>(reader: &mut BufReader<R>) -> Option<Frame> {
    let mut hdr = [0u8; 2];
    if reader.read_exact(&mut hdr).is_err() {
        return None;
    }
    let opcode = (hdr[0] & 0x0f) | (hdr[0] & 0x80); // 保留 FIN，便于透传
    let masked = hdr[1] & 0x80 != 0;
    let mut len = (hdr[1] & 0x7f) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        if reader.read_exact(&mut b).is_err() {
            return None;
        }
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        if reader.read_exact(&mut b).is_err() {
            return None;
        }
        len = u64::from_be_bytes(b);
    }
    let mut mask = [0u8; 4];
    if masked && reader.read_exact(&mut mask).is_err() {
        return None;
    }
    let mut payload = vec![0u8; len.min(1 << 22) as usize];
    if reader.read_exact(&mut payload).is_err() {
        return None;
    }
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
    }
    Some(Frame { opcode, payload })
}

/// 服务端发送帧：不掩码。
fn send_frame(out: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len();
    let mut hdr = Vec::with_capacity(10);
    hdr.push(0x80 | opcode);
    if len < 126 {
        hdr.push(len as u8);
    } else if len < 65536 {
        hdr.push(126);
        hdr.extend((len as u16).to_be_bytes());
    } else {
        hdr.push(127);
        hdr.extend((len as u64).to_be_bytes());
    }
    out.write_all(&hdr)?;
    out.write_all(payload)?;
    out.flush()?;
    Ok(())
}

// ------------------------------------------------------------------ HTTP / WS 工具

fn read_http_head(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut scratch = [0u8; 4096];
    loop {
        let n = stream.read(&mut scratch)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
        }
        buf.extend_from_slice(&scratch[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let end = pos + 4;
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let rest = buf[end..].to_vec();
            return Ok((head, rest));
        }
        if buf.len() > 1 << 20 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "head too large"));
        }
    }
}

fn request_line(head: &str) -> Option<&str> {
    head.lines().next().map(str::trim)
}

fn header_val<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    for line in head.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(name) {
                return Some(v.trim());
            }
        }
    }
    None
}

/// 从 `GET /ws?key=<hex>` 中提取 key。
fn ws_key_from_query(request_line: &str) -> Option<String> {
    let path = request_line.split_whitespace().nth(1)?;
    let query = path.split('?').nth(1)?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == "key" && !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn ws_accept(client_key: &str) -> String {
    let mut hashed = client_key.as_bytes().to_vec();
    hashed.extend_from_slice(GUID);
    base64(&sha1(&hashed))
}

// ------------------------------------------------------------------ 先入先出 Read：pending 先行

struct SeqReader {
    pending: std::collections::VecDeque<u8>,
    inner: TcpStream,
}
impl Read for SeqReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            for i in 0..n {
                buf[i] = self.pending.pop_front().unwrap();
            }
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

// ------------------------------------------------------------------ SHA-1 / Base64

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k): (u32, u32) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_vector() {
        // RFC 6455 示例
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(ws_accept(key), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn key_from_query() {
        assert_eq!(
            ws_key_from_query("GET /ws?key=abcd1234 HTTP/1.1"),
            Some("abcd1234".to_string())
        );
        assert_eq!(ws_key_from_query("GET /ws HTTP/1.1"), None);
    }
}