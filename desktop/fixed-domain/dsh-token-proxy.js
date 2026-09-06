// DSH Web reverse proxy: token injection only.
// DSH natively trusts dsh.lg0304.xyz (set via profiles/web/cordis.patch.yml -> trustedHosts),
// so we STOP rewriting Host/Origin. The real domain flows through: both the /api browser-trust
// fence and the signed browser-session cookie anchor to dsh.lg0304.xyz consistently.
// Only job left: on FIRST root HTML navigation (no ?token= and no auth cookie yet) inject the
// launch token so DSH mints the signed session cookie (303 -> /, no redirect loop).
const net = require('net');
const { appendFileSync } = require('fs');
const LOG = process.env.PROXY_LOG || "C:\\Users\\Administrator\\DSH-Desktop\\proxy-debug.log";
const log = (s) => { try { appendFileSync(LOG, new Date().toISOString() + ' ' + s + '\n'); } catch {} };
const TOKEN = process.env.PROXY_TOKEN || '';
const UP_HOST = process.env.UP_HOST || '127.0.0.1';
const UP_PORT = parseInt(process.env.UP_PORT || '3080', 10);
const PORT = parseInt(process.env.PORT || '3090', 10);
if (!TOKEN) { console.error('PROXY_TOKEN is not set'); process.exit(1); }

function injectToken(headText) {
  const i = headText.indexOf('\r\n');
  const requestLine = headText.slice(0, i);
  const rest = headText.slice(i);
  const m = requestLine.match(/^(GET\s+\/\S*)\s+(HTTP\/[^\s]+)$/i);
  if (!m) return headText;
  // preserve non-token query params, drop any stale token, always put the fresh token
  const req = m[1];
  const qi = req.indexOf('?');
  const pOnly = qi >= 0 ? req.slice(0, qi) : req;
  const qs = qi >= 0 && qi + 1 < req.length ? req.slice(qi + 1) : '';
  const bits = [];
  if (qs) for (const kv of qs.split('&')) if (kv && kv.split('=')[0] !== 'token') bits.push(kv);
  bits.push('token=' + encodeURIComponent(TOKEN));
  return pOnly + '?' + bits.join('&') + ' ' + m[2] + rest;
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
    try { log('REQ ' + method + ' ' + path + ' html=' + isHtmlNav + ' token=' + hasToken + ' cookie=' + hasAuthCookie + ' accept-html=' + (/\btext\/html\b/i.test(headText))); } catch {}
    // Force a FRESH token on every root HTML navigation (ignore stale cookie / old-token state),
    // so expired sessions and PWA windows pinned to an old ?token= always get renewed.
    const isHtmlNav = method === 'GET' && /\btext\/html\b/i.test(headText) && !/^\/api\//i.test(path) && (path === '/' || path.startsWith('/?'));
    const outHead = isHtmlNav ? injectToken(headText) : headText;
    const u = net.connect(UP_PORT, UP_HOST, () => { u.write(outHead); if (rest.length) u.write(rest); });
    u.on('error', () => c.destroy());
    u.once('data', (chunk) => { const rl = chunk.toString('latin1').split('\r\n')[0] || ''; try { log((isHtmlNav ? 'INJ ' : 'RAW ') + method + ' ' + path + ' : Resp ' + rl); } catch {} });
    c.on('error', () => u.destroy());
    u.pipe(c);
    c.pipe(u);
  };
  c.on('data', onData);
  c.on('error', () => {});
});
srv.listen(PORT, '127.0.0.1', () => console.log('dsh-token-proxy(token-only) on 127.0.0.1:' + PORT + ' -> ' + UP_HOST + ':' + UP_PORT));