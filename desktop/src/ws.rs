//! 远程通道：按 DSH Link 报文协议（`dsh-link::Message`）运行的 WebSocket 服务端。
//! WebSocket 握手与帧编解码用极简自研实现（`util::sha1/base64`），不依赖任何第三方 socket crate。

use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::UNIX_EPOCH;

use dsh_link::{Envelope, Message, PairingKey, Role, Session};

use crate::util;

const WEBSOCKET_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const SERVER_ID: &str = "dsh-desktop";

/// 通道上下文：配对密钥 + 可选的 DSH home（用于推送真实会话摘要）。
#[derive(Clone)]
pub struct ChannelCtx {
    pub key: PairingKey,
    pub home_dir: Option<std::path::PathBuf>,
}

/// 读取请求头直到 `\r\n\r\n`，返回 (头部文本, 未消费的剩余字节)。一次只读一个 TCP 块。
/// 关键：不把 WebSocket 帧字节吞掉。
pub fn read_http_head(stream: &mut TcpStream) -> io::Result<(String, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut scratch = [0u8; 4096];
    loop {
        let n = stream.read(&mut scratch)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof before head"));
        }
        buf.extend_from_slice(&scratch[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            let end = pos + 4;
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let rest = buf[end..].to_vec();
            return Ok((head, rest));
        }
        if buf.len() > 1 << 20 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    buf.windows(4).position(|w| w == b"\r\n\r\n")
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

/// 处理一次 WebSocket 连接。`ws_key` 为查询串中的配对密钥（需与服务端密钥一致）。
/// `pending` 是读取 HTTP 头时同包吞入的剩余字节（首个 WebSocket 帧），需喂给帧读取器，
/// 否则会丢帧（切不可写回 socket）。
pub fn serve(
    mut stream: TcpStream,
    head: &str,
    ws_key: Option<&str>,
    ctx: &ChannelCtx,
    pending: Vec<u8>,
) -> io::Result<()> {
    // 认证：查询串密钥必须匹配服务端配对密钥
    match ws_key {
        Some(k) if k.eq_ignore_ascii_case(&ctx.key.to_hex()) => {}
        _ => {
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Ok(());
        }
    }

    let client_key = match header_val(head, "Sec-WebSocket-Key") {
        Some(k) if !k.is_empty() => k.to_owned(),
        _ => {
            let _ = stream.write_all(
                b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Ok(());
        }
    };

    // 计算 Sec-WebSocket-Accept
    let mut hashed = client_key.into_bytes();
    hashed.extend_from_slice(WEBSOCKET_GUID);
    let accept = util::base64(&util::sha1(&hashed));

    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\
         \r\n"
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()?;

    // 帧读写：将 HTTP 头阶段同包吞入的剩余字节作为首段 pending，避免丢帧
    let read_handle = stream.try_clone()?;
    let mut reader = BufReader::new(SeqReader {
        pending: VecDeque::from(pending),
        inner: read_handle,
    });
    frame_loop(&mut reader, &mut stream, ctx)
}

/// 先消费连接建立时未读的字节，再读 stream。
struct SeqReader {
    pending: VecDeque<u8>,
    inner: TcpStream,
}
impl Read for SeqReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
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

fn frame_loop<R: Read>(reader: &mut BufReader<R>, out: &mut TcpStream, ctx: &ChannelCtx) -> io::Result<()> {
    loop {
        let mut hdr = [0u8; 2];
        match reader.read_exact(&mut hdr) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
        let opcode = hdr[0] & 0x0f;
        let fin = hdr[0] & 0x80 != 0;
        let masked = hdr[1] & 0x80 != 0;
        let mut len = (hdr[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            reader.read_exact(&mut b)?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            reader.read_exact(&mut b)?;
            len = u64::from_be_bytes(b);
        }
        let mut mask = [0u8; 4];
        if masked {
            reader.read_exact(&mut mask)?;
        }
        if len > 1 << 22 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut payload = vec![0u8; len as usize];
        reader.read_exact(&mut payload)?;
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }

        match opcode {
            0x8 => {
                // close → 回执并断开
                let _ = send_close(out, 1000, "bye");
                return Ok(());
            }
            0x9 => {
                send_frame(out, 0x8A, &payload)?; // pong
            }
            0x1 | 0x2 => {
                let _ = dispatch_text(&payload, out, ctx);
            }
            _ if fin => {} // 忽略其它控制帧
            _ => {}
        }
    }
}

/// 处理一条 DSH Link 业务报文（server 角色）：对 hello 回 ack + 会话摘要，对 send_msg 落盘 + ack。
/// `out` 用于写回客户端帧。供局域网 ws 服务与中继 relay 客户端共用。
pub fn dispatch_text<W: Write>(payload: &[u8], out: &mut W, ctx: &ChannelCtx) -> io::Result<()> {
    let text = String::from_utf8_lossy(payload).to_string();
    let msg = match Envelope::from_json(&text) {
        Some(env) => env.msg,
        None => return Ok(()),
    };
    match msg {
        Message::Hello { name, .. } => {
            // 回握 + 推送当前会话摘要
            send_text(
                out,
                &Envelope::wrap(Message::HelloAck {
                    server_id: SERVER_ID.into(),
                    name: format!("{}@{}", SERVER_ID, crate::hostname()),
                })
                .to_json(),
            )?;
            let sessions = ctx.home_dir.as_deref().map(collect_sessions).unwrap_or_default();
            send_text(out, &Envelope::wrap(Message::SessionSnap { sessions }).to_json())?;
            let _ = name; // 记名
            Ok(())
        }
        Message::SendMsg { session_id, text } => {
            // 远程指令（当前版本：落盘为 received.jsonl，供上层接管；回复 ACK）
            append_received(ctx, &session_id, &text);
            send_text(
                out,
                &Envelope::wrap(Message::Ack { ok: true, reason: None }).to_json(),
            )
        }
        _ => Ok(()), // 其它（SessionSnap/Event/Ack）服务端侧忽略
    }
}

/// 已配对客户端发来的指令落盘，便于后续接入真实 DSH 会话执行。
fn append_received(ctx: &ChannelCtx, session_id: &Option<String>, text: &str) {
    if let Some(home) = &ctx.home_dir {
        let _ = std::fs::create_dir_all(home);
        let path = home.join("dsh-link.received.jsonl");
        let line = serde_json::json!({
            "t": now_unix(),
            "session_id": session_id,
            "text": text,
            "role": Role::Client,
        })
        .to_string();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 从 `home/sessions/<ws>/<sid>` 收集会话摘要。
fn collect_sessions(home: &Path) -> Vec<Session> {
    let mut v = Vec::new();
    let root = home.join("sessions");
    if let Ok(ws_dirs) = std::fs::read_dir(&root) {
        for ws in ws_dirs
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        {
            let ws_name = ws.file_name().to_string_lossy().to_string();
            if let Ok(sds) = std::fs::read_dir(ws.path()) {
                for sd in sds
                    .flatten()
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                {
                    let sid = sd.file_name().to_string_lossy().to_string();
                    let mtime = sd
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    v.push(Session {
                        id: sid.clone(),
                        title: format!("{ws_name}/{sid}"),
                        updated_unix: mtime,
                    });
                }
            }
        }
    }
    v
}

/// 发送文本帧（服务端→客户端，不掩码）。
fn send_text<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    send_frame(out, 0x1, text.as_bytes())
}

fn send_close<W: Write>(out: &mut W, code: u16, reason: &str) -> io::Result<()> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    send_frame(out, 0x8, &payload)
}

fn send_frame<W: Write>(out: &mut W, opcode: u8, payload: &[u8]) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_accept() {
        // RFC 6455 示例向量
        let key = b"dGhlIHNhbXBsZSBub25jZQ==";
        let mut input = key.to_vec();
        input.extend_from_slice(WEBSOCKET_GUID);
        assert_eq!(util::base64(&util::sha1(&input)), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn head_split() {
        let s = b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\nEXTRA";
        let pos = find_double_crlf(s).unwrap();
        assert_eq!(&s[..pos + 4], b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n");
        assert_eq!(&s[pos + 4..], b"EXTRA");
    }
}