//! 中继（relay）客户端：让电脑端与安卓端在**不同网络**也能配对（参考 Paseo 的 relay 模型）。
//!
//! 电脑端主动**出站连到中继**（`dsh-relay`），作为「server 角色」承载 DSH Link 业务握手：
//! 手机扫到的链接指向中继 `dsh-link://<relayHost>:<relayPort>/#key=<hex>`；手机也出站连同一个
//! 中继。中继按 key 撮合两端后做帧级透明转发。本模块 = WebSocket 客户端握手 + 帧读写，
//! 业务报文处理复用 `ws::dispatch_text`。

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use dsh_link::PairingKey;

use crate::util;
use crate::ws::{self, ChannelCtx};

/// 客户端写帧用的包装：让 `dispatch_text` 的泛型 `W: Write` 能写回原来的 stream。
struct MaskedWriter<'a>(&'a mut TcpStream);

impl<'a> Write for MaskedWriter<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// 连一次中继并处理业务，直至断开/出错返回。供外层循环重连。
pub fn run_once(relay_host: &str, relay_port: u16, key: &PairingKey, home_dir: Option<PathBuf>) -> std::io::Result<()> {
    let mut stream = TcpStream::connect((relay_host, relay_port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(20)))?;

    // ---- WebSocket 客户端握手 ----
    let hex = key.to_hex();
    let sec_key = util::base64(&PairingKey::random().0);
    let req = format!(
        "GET /ws?key={hex} HTTP/1.1\r\n\
         Host: {relay_host}:{relay_port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {sec_key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let (resp_line, _rest) = read_resp_line(&mut stream)?;
    if !resp_line.contains("101") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("relay handshake failed: {resp_line}"),
        ));
    }
    // 2xx 之外剩下的响应头：读到 \r\n\r\n 为止（保持格式简单）
    drain_headers(&mut stream)?;

    println!(
        "[dsh-desktop] relay 已连接 {}:{} (key={}…)，等待手机配对…",
        relay_host,
        relay_port,
        &key.to_hex()[..6]
    );

    // ---- 业务帧循环：读未掩码帧 → dispatch；写掩码帧回中继 ----
    let read_handle = stream.try_clone()?;
    let mut reader = BufReader::new(read_handle);
    let ctx = ChannelCtx {
        key: key.clone(),
        home_dir,
    };
    loop {
        let mut hdr = [0u8; 2];
        if reader.read_exact(&mut hdr).is_err() {
            break;
        }
        let opcode = hdr[0] & 0x0f;
        let masked = hdr[1] & 0x80 != 0;
        let mut len = (hdr[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            if reader.read_exact(&mut b).is_err() {
                break;
            }
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            if reader.read_exact(&mut b).is_err() {
                break;
            }
            len = u64::from_be_bytes(b);
        }
        let mut mask = [0u8; 4];
        if masked && reader.read_exact(&mut mask).is_err() {
            break;
        }
        let mut payload = vec![0u8; len.min(1 << 22) as usize];
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i & 3];
            }
        }

        match opcode {
            0x8 => break,
            0x9 => {
                // ping → pong（本端要掩码）
                let _ = send_masked(&mut stream, 0xA, &payload);
            }
            0x1 | 0x2 => {
                if ws::dispatch_text(&payload, &mut MaskedWriter(&mut stream), &ctx).is_err() {
                    break;
                }
            }
            _ => {}
        }
    }
    let _ = send_masked(&mut stream, 0x8, &[0x03, 0xe8, b'o', b'k']);
    Ok(())
}

/// 循环连中继；断线 3 秒重试。用于守护线程主循环。
pub fn run_forever(relay_host: String, relay_port: u16, key: PairingKey, home_dir: Option<PathBuf>) {
    loop {
        if let Err(e) = run_once(&relay_host, relay_port, &key, home_dir.clone()) {
            eprintln!("[dsh-desktop] relay 连接失败/断开: {e}，3 秒后重连…");
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

/// 解析 `host:port`。IPv6 支持 `[::1]:port`。缺端口时报 None（由调用方告警）。
pub fn parse_endpoint(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port = after.strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host.to_string(), port));
    }
    let idx = s.rfind(':')?;
    let host = &s[..idx];
    if host.is_empty() {
        return None;
    }
    let port = s[idx + 1..].parse::<u16>().ok()?;
    Some((host.to_string(), port))
}

/// 客户端写帧：**掩码**。掩码帧由中继去掩码后重封给对端。
fn send_masked(out: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len();
    let mut hdr = Vec::with_capacity(10);
    hdr.push(0x80 | opcode);
    if len < 126 {
        hdr.push(0x80 | len as u8);
    } else if len < 65536 {
        hdr.push(0x80 | 126);
        hdr.extend((len as u16).to_be_bytes());
    } else {
        hdr.push(0x80 | 127);
        hdr.extend((len as u64).to_be_bytes());
    }
    // 4 字节随机掩码
    let mask = PairingKey::random().0;
    hdr.extend_from_slice(&mask[..4]);
    out.write_all(&hdr)?;
    let enc: Vec<u8> = payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i & 3])
        .collect();
    out.write_all(&enc)?;
    out.flush()
}

// ------------------------------------------------------------------ 握手响应解析（极简）

/// 只读一行（到 \r\n）返回。用于检查响应状态行是否 101。
fn read_resp_line(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut line = Vec::new();
    let mut buf = [0u8; 1];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
            Ok(_) => {
                line.push(buf[0]);
                if line.ends_with(b"\r\n") {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
    }
    let line = String::from_utf8_lossy(&line[..line.len().saturating_sub(2)]).to_string();
    Ok((line, Vec::new()))
}

/// 吞掉剩余响应头直到 `\r\n\r\n`。
fn drain_headers(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut scratch = [0u8; 1];
    loop {
        match stream.read(&mut scratch) {
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
            Ok(_) => {
                buf.push(scratch[0]);
                if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
                    return Ok(());
                }
                if buf.len() > 1 << 16 {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "head too large"));
                }
            }
            Err(e) => return Err(e),
        }
    }
}