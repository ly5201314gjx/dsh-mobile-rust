//! 配对/控制 HTTP 服务：监听局域网，提供配对页面、二维码、链接信息与 WebSocket 通道。
//! 纯 std TCP 实现的小型 HTTP 路由，无第三方 web 框架。

use std::io::Write;
use std::net::{TcpListener, TcpStream};

use dsh_link::{Link, PairingKey};

use crate::ws;

/// 配对服务配置。
pub struct PairServer {
    pub bind: String,
    pub port: u16,
    /// 公之于众的 LAN IP（供手机访问；识别失败时回退绑定地址）。
    pub public_ip: String,
    pub key: PairingKey,
    pub home_dir: Option<std::path::PathBuf>,
    /// 中继模式：覆盖链接/二维码里的 host（否则回落 public_ip）。
    pub advertise_host: Option<String>,
    /// 中继模式：覆盖链接/二维码里的端口（否则用本机 port）。
    pub advertise_port: Option<u16>,
    /// 中继/隧道模式：链接是否走 WSS/TLS（true 则二维码用 dsh-link-wss://）。
    pub advertise_tls: bool,
    /// 隧道模式：DSH Web UI 的对外地址（第二个 Cloudflare 隧道），形如 `xxx.trycloudflare.com`。
    pub advertise_web_host: Option<String>,
}

impl PairServer {
    /// 供手机/浏览器使用的 dsh-link 链接（中继/隧道模式下指向中继/隧道地址）。
    pub fn link(&self) -> Link {
        let host = self
            .advertise_host
            .clone()
            .unwrap_or_else(|| self.public_ip.clone());
        let port = self.advertise_port.unwrap_or(self.port);
        let mut l = Link::new(host, port, self.key.clone());
        l.tls = self.advertise_tls;
        l.web_host = self.advertise_web_host.clone();
        l
    }

    /// 配对页地址（浏览器打开）。
    pub fn page_url(&self) -> String {
        format!("http://{}:{}/pair", self.public_ip, self.port)
    }

    pub fn run(&self) -> std::io::Result<()> {
        let listener = TcpListener::bind((self.bind.as_str(), self.port))?;
        println!(
            "[dsh-desktop] pairing service on http://{}:{}  (page: /pair)",
            self.public_ip, self.port
        );
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => {
                    let ctx = self.clone_for_channel();
                    let ip = self.public_ip.clone();
                    let port = self.port;
                    let key = self.key.clone();
                    let page_url = self.page_url();
                    let qr = qr_svg(&self.link().to_qr());
                    std::thread::spawn(move || {
                        let _ = handle(&mut s, &pair_payload(ip, port, key, page_url, qr), &ctx);
                    });
                }
                Err(_) => continue,
            }
        }
        Ok(())
    }

    fn clone_for_channel(&self) -> ChannelNew {
        ChannelNew {
            key: self.key.clone(),
            home_dir: self.home_dir.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ChannelNew {
    key: PairingKey,
    home_dir: Option<std::path::PathBuf>,
}

fn pair_payload(ip: String, port: u16, key: PairingKey, page_url: String, qr: String) -> PairPayload {
    let link = Link::new(ip.clone(), port, key.clone());
    PairPayload {
        link_text: link.to_qr(),
        page_url,
        host: ip,
        port,
        key: key.to_hex(),
        qr,
    }
}

struct PairPayload {
    link_text: String,
    page_url: String,
    host: String,
    port: u16,
    key: String,
    qr: String,
}

fn handle(stream: &mut TcpStream, pp: &PairPayload, ctx: &ChannelNew) -> std::io::Result<()> {
    let (head, also_read) = match ws::read_http_head(stream) {
        Ok(x) => x,
        Err(_) => return Ok(()),
    };
    let first = head.lines().next().unwrap_or("");
    // 提取 method 与 path+query
    let parts: Vec<&str> = first.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let target = parts[1];

    match (method, target.split('?').next().unwrap_or("")) {
        ("GET", "/health") => reply_text(stream, 200, "ok"),
        ("GET", "/") => reply_json(stream, 200, &status_json(pp, &page_payload(pp))),
        ("GET", "/qr.svg") => reply_bytes(stream, 200, "image/svg+xml; charset=utf-8", pp.qr.as_bytes()),
        ("GET", "/pair") => reply_text(stream, 200, &page_html(pp)),
        ("GET", "/ws") => {
            // 把同包吞入的剩余字节喂给 ws 通道的帧读取器（切不可写回 socket，否则会把它发给客户端）
            let query = target.split('?').nth(1).unwrap_or("");
            let key = parse_query(query).and_then(|q| q.get("key").cloned());
            let chctx = ws::ChannelCtx {
                key: ctx.key.clone(),
                home_dir: ctx.home_dir.clone(),
            };
            ws::serve(stream.try_clone()?, &head, key.as_deref(), &chctx, also_read)
        }
        _ => reply_text(stream, 404, "not found"),
    }
}

fn parse_query(q: &str) -> Option<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    Some(map)
}

fn status_json(pp: &PairPayload, _page: &str) -> String {
    serde_json::json!({
        "version": dsh_link::PROTOCOL_VERSION,
        "scheme": "dsh-link",
        "host": pp.host,
        "port": pp.port,
        "link": pp.link_text,
        "page": pp.page_url,
    })
    .to_string()
}

fn reply_json(stream: &mut TcpStream, code: u16, body: &str) -> std::io::Result<()> {
    reply_bytes(stream, code, "application/json; charset=utf-8", body.as_bytes())
}

fn reply_text(stream: &mut TcpStream, code: u16, body: &str) -> std::io::Result<()> {
    reply_bytes(stream, code, "text/plain; charset=utf-8", body.as_bytes())
}

fn reply_bytes(stream: &mut TcpStream, code: u16, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        426 => "Upgrade Required",
        _ => "",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn page_payload(pp: &PairPayload) -> String {
    status_json(pp, "")
}

/// 生成二维码 SVG。失败时返回空串（调用方已把空串塞进页面）。
fn qr_svg(text: &str) -> String {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(code) => code
            .render::<qrcode::render::svg::Color>()
            .quiet_zone(true)
            .build(),
        Err(_) => String::new(),
    }
}

fn page_html(pp: &PairPayload) -> String {
    format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>DSH 配对</title>
<style>
 body{{font-family:system-ui,Segoe UI,Roboto,sans-serif;background:#0f1222;color:#e8eaf6;margin:0;padding:24px;display:flex;justify-content:center}}
 .card{{background:#1a1f3a;border-radius:16px;padding:28px;max-width:420px;width:100%;text-align:center;box-shadow:0 8px 30px rgba(0,0,0,.4)}}
 h1{{font-size:20px;margin:0 0 6px}} h2{{font-size:14px;font-weight:400;color:#a9b0d4;margin:0 0 18px}}
 .qr{{background:#fff;border-radius:10px;padding:12px;margin:0 auto 18px;width:220px;height:220px;display:flex;align-items:center;justify-content:center}}
 .qr svg{{width:100%;height:100%}}
 .linkbox{{background:#0c0f22;border:1px dashed #3a4066;border-radius:8px;padding:10px 12px;margin-bottom:18px;font-size:12px;word-break:break-all;color:#c9d1ff}}
 .btn{{background:#4f6bff;color:#fff;border:0;border-radius:8px;padding:10px 14px;font-size:14px;width:100%;cursor:pointer}}
 .hint{{font-size:12px;color:#8b93c0;margin-top:10px;line-height:1.6}}
 .status{{margin-top:14px;font-size:12px;color:#7ee08b}}
</style></head><body><div class="card">
<h1>DSH 电脑端配对</h1>
<h2>电脑 ({host}:{port}) · 本配对仅对当前会话有效</h2>
<div class="qr">{qr}</div>
<div class="linkbox">{link}</div>
<button class="btn" onclick="navigator.clipboard.writeText('{link}')">复制配对链接</button>
<div class="hint">① 电脑上启动 DSH Desktop 后，手机 App 打开「连接电脑」→ 扫码或填入上面链接。<br>
② 配对成功后，两端保持同步：手机可查看电脑端 DSH 会话并对电脑发指令。</div>
<div class="status" id="status"></div>
<script>
 const host=location.hostname, port=location.port||'80';
 const proto=location.protocol==='https:'?'wss':'ws';
 try {{
   const ws=new WebSocket(`${{proto}}://${{host}}:${{port}}/ws?key={key}`);
   const st=document.getElementById('status');
   ws.onopen=()=>st.textContent='✓ 已连接电脑端通道';
   ws.onmessage=(ev)=>{{{{
     const d=JSON.parse(ev.data);
     if(d.type==='hello_ack') st.textContent=`✓ 已配对 (${{d.name}})`;
     if(d.type==='session_snap') st.textContent=`✓ 已配对，电脑端会话 ${{d.sessions.length}} 个`;
   }}}};
   ws.onerror=()=>st.textContent='连接失败：请确认手机与电脑在同一局域网';
   ws.onclose=()=>st.textContent='通道已关闭';
 }}catch(e){{}}
</script></div></body></html>"#,
        host = pp.host,
        port = pp.port,
        qr = pp.qr,
        link = pp.link_text,
        key = pp.key,
    )
}