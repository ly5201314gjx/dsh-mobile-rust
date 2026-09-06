// dsh-token-proxy —— 固定域名 Web 控制台的 token 注入反向代理
// 背景：DSH Web 需要入口 token，无 token 直接访问会 401。手机端配对链接只带
// web= 域名、不带 token；此代理把对根路径 "/" 的 GET 请求 302 重定向到
// "/?token=<TOKEN>"，让手机直接打开 https://<web域>/ 即可自动鉴权，其余请求
// （静态资源 / WebSocket / SSE）全部透明转发到 DSH Web (UP_HOST:UP_PORT)。
// 用法：PROXY_TOKEN=<token> node dsh-token-proxy.js
//   env: PROXY_TOKEN 必填；UP_HOST 默认 127.0.0.1；UP_PORT 默认 3080；PORT 默认 3090
const net = require('net');
const TOKEN = process.env.PROXY_TOKEN || '';
const UP_HOST = process.env.UP_HOST || '127.0.0.1';
const UP_PORT = parseInt(process.env.UP_PORT || '3080', 10);
const PORT = parseInt(process.env.PORT || '3090', 10);
if (!TOKEN) { console.error('PROXY_TOKEN is not set'); process.exit(1); }
const srv = net.createServer((c) => {
  let buf = Buffer.alloc(0);
  let done = false;
  const onData = (d) => {
    if (done) return;
    buf = Buffer.concat([buf, d]);
    const i = buf.indexOf('\r\n\r\n');                 // 请求头结束
    if (i < 0) { if (buf.length > 65536) c.destroy(); return; }
    done = true;
    c.removeListener('data', onData);
    const head = buf.slice(0, i + 4);
    const rest = buf.slice(i + 4);
    const line = head.toString('latin1').split('\r\n')[0] || '';
    const parts = line.split(' ');
    const method = parts[0] || 'GET';
    const path = parts[1] || '/';
    if (method === 'GET' && path === '/' && !/token=/.test(path)) {
      c.write('HTTP/1.1 302 Found\r\nLocation: /?token=' + TOKEN + '\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
      c.end();
      return;
    }
    const u = net.connect(UP_PORT, UP_HOST, () => { u.write(head); if (rest.length) u.write(rest); });
    u.on('error', () => c.destroy());
    c.on('error', () => u.destroy());
    u.pipe(c);
    c.pipe(u);
  };
  c.on('data', onData);
  c.on('error', () => {});
});
srv.listen(PORT, '127.0.0.1', () =>
  console.log('dsh-token-proxy on 127.0.0.1:' + PORT + ' -> ' + UP_HOST + ':' + UP_PORT));