// DSH Web reverse proxy: token injection + Host/Origin/Sec-Fetch normalization to loopback.
//   - On FIRST root HTML navigation (no auth cookie yet) it injects the ?token= launch token,
//     letting DSH mint the signed browser-session cookie. After the cookie exists it stops
//     injecting so DSH serves index directly (no redirect loop).
//   - Every request gets Host/Origin/Referer/Sec-Fetch-Site rewritten to the loopback upstream,
//     so the browser-trust fence in dsh-client-connection (Host+Origin must match) passes.
const net = require('net');
const TOKEN = process.env.PROXY_TOKEN || '';
const UP_HOST = process.env.UP_HOST || '127.0.0.1';
const UP_PORT = parseInt(process.env.UP_PORT || '3080', 10);
const PORT = parseInt(process.env.PORT || '3090', 10);
if (!TOKEN) { console.error('PROXY_TOKEN is not set'); process.exit(1); }
const UP_AUTHORITY = UP_HOST + ':' + UP_PORT;
const UP_ORIGIN = 'http://' + UP_AUTHORITY;
function rewriteHeaders(head) {
  return head.split('\r\n').map((line) => {
    if (!line || !line.includes(':')) return line;
    const idx = line.indexOf(':');
    const name = line.slice(0, idx).trim().toLowerCase();
    const val  = line.slice(idx + 1).trim();
    switch (name) {
      case 'host':               return 'Host: ' + UP_AUTHORITY;
      case 'origin':             return 'Origin: ' + UP_ORIGIN;
      case 'referer':            return (/^(?:https?:)?\/\//i.test(val)) ? 'Referer: ' + UP_ORIGIN + '/' : line;
      case 'sec-fetch-site':     return 'Sec-Fetch-Site: same-origin';
      case 'x-forwarded-host':
      case 'x-forwarded-server': return null;
      default:                   return line;
    }
  }).filter((l) => l !== null).join('\r\n');
}
const srv = net.createServer((c) => {
  let buf = Buffer.alloc(0);
  let done = false;
  const onData = (d) => {
    if (done) return;
    buf = Buffer.concat([buf, d]);
    const i = buf.indexOf('\r\n\r\n');
    if (i < 0) { if (buf.length > 65536) c.destroy(); return; }
    done = true;
    c.removeListener('data', onData);
    const head = buf.slice(0, i + 4);
    const rest = buf.slice(i + 4);
    const headText = head.toString('latin1');
    const line0 = headText.split('\r\n')[0] || '';
    const parts = line0.split(' ');
    const method = parts[0] || 'GET';
    const path = parts[1] || '/';
    const hasToken = /[?&]token=\w/i.test(path);
    const hasAuthCookie = /dsh-auth-/i.test(headText);
    const isHtmlNav = method === 'GET' && path === '/' && !hasToken && !hasAuthCookie && /\btext\/html\b/i.test(headText);
    const via = rewriteHeaders(headText);
    let newHead;
    if (isHtmlNav) {
      const reqLine = 'GET /?token=' + TOKEN + ' HTTP/1.1\r\n';
      newHead = Buffer.from(reqLine + via.split('\r\n').slice(1).join('\r\n'), 'latin1');
    } else {
      newHead = Buffer.from(via, 'latin1');
    }
    const u = net.connect(UP_PORT, UP_HOST, () => { u.write(newHead); if (rest.length) u.write(rest); });
    u.on('error', () => c.destroy());
    c.on('error', () => u.destroy());
    u.pipe(c);
    c.pipe(u);
  };
  c.on('data', onData);
  c.on('error', () => {});
});
srv.listen(PORT, '127.0.0.1', () => console.log('dsh-token-proxy on 127.0.0.1:' + PORT + ' -> ' + UP_HOST + ':' + UP_PORT + ' (Host/Origin normalized to ' + UP_AUTHORITY + ')'));