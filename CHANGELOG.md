# Changelog

## v4.0 (2026-09-06)

### 新增
- **自有域名固定穿透（方案三）**：新增 `desktop/fixed-domain/`，支持把配对/Web 入口固定到
  你自己的 Cloudflare 子域名（`config.example.yml` + `SETUP_OWN_DOMAIN.md`），
  提供一键启动 `start_fixed_domain.bat`、打开 Web 控制台 `open-web-console.bat` 与
  token 注入代理 `dsh-token-proxy.js`。域名永不失效，仅每次启动的 key/token 变化。
- 一键拉起整套固定域名栈：命名隧道 + dsh-desktop + token 代理，自动打印配对链接。

### 修复
- **DSH Web 无法启动（Windows 桌面端）**：`dsh-desktop` 将 web 绑定由 `0.0.0.0` 改为
  `127.0.0.1`。DSH web 正式拒绝非回环绑定（`0.0.0.0` 会直接退出），此前会导致 `dsh web exited`
  无限重启；现在 web 正常监听回环，对外访问一律经配对端口 5780 / 隧道 / token 代理转发，
  不再暴露 3080。

### 打包
- `DSH-Desktop-win-x64.zip` 更新：内置上述固定域名脚本与修好的 `dsh-desktop.exe`。

## v3.3 (2026-08-31)
- 异网络配对：Cloudflare 快速隧道（WSS 加密）+ 免服务器开箱即用；dsh-core 平台门控兼容
  Windows 编译。