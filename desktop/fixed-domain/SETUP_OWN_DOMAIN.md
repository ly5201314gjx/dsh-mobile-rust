# 自有域名固定穿透（方案三：命名隧道 + 固定子域名）

> 如果你有自己的域名（例如 `lg0304.xyz`，托管在 Cloudflare），可以把配对/Web 入口
> **固定到自己的子域名**（如 `pair.lg0304.xyz`、`dsh.lg0304.xyz`），域名**永不失效**，
> 只有每次启动的配对 key / 入口 token 会变。相比临时 `*.trycloudflare.com`，更适合长期使用。

## 原理
用 Cloudflare **命名隧道** + 一条 `config.yml` 的 ingress 规则，把两个子域名分别路由：
```
pair 子域名 -> 本机 5780（配对 / WSS 通道）
web  子域名 -> 本机 3090（token 注入代理）-> 127.0.0.1:3080（DSH Web）
```
手机任意网络扫码 / 填 `dsh-link-wss://pair.你的域/#key=…&web=web.你的域` 即可配对，
电脑 Web 控制台固定为 `https://web.你的域/`（token 由代理自动注入）。

## 完整步骤（一次性）

### 0. 准备
- 域名已托管在 Cloudflare（DNS 由 CF 解析）。
- 免安装包内自带 `cloudflared.exe`、`node.exe`、`dsh-desktop.exe`、`dsh-token-proxy.js`。

### 1. 授权 cloudflared 登录你的 Cloudflare 账号
```bash
cloudflared login
```
浏览器打开打印的链接，登录并点「授权」。成功后在 `C:\Users\<你>\.cloudflared\cert.pem` 生成证书。

### 2. 创建命名隧道
```bash
cloudflared tunnel create dsh-lg
# 输出 Tunnel ID，例如 abdfd753-d244-43aa-a5d1-89955cd03ada，并生成凭据文件
# C:\Users\<你>\.cloudflared\abdfd753-….json
```

### 3. 把子域名 CNAME 绑到该隧道
```bash
cloudflared tunnel route dns dsh-lg pair.lg0304.xyz   # 配对
cloudflared tunnel route dns dsh-lg web.lg0304.xyz    # Web 控制台
```

### 4. 写 config.yml（把示例复制并按上面信息填）
- 把本目录 `config.example.yml` 复制为 `config.yml`；
- 替换两个 `hostname:` 为你的子域名；
- 替换 `credentials-file:` 为其真实绝对路径。

### 5. 一键启动
双击 **`start_fixed_domain.bat`**（保持窗口开着）。会：
- 自动拉起命名隧道；
- 以固定子域名启动 `dsh-desktop`（广播 `wss://pair.你的域` + `web.你的域`）；
- 启动 token 注入代理；打印配对链接。

## 手机配对（每次重启后 key 会变）
扫码或粘贴窗口打印的：
```
dsh-link-wss://pair.lg0304.xyz/#key=<hex>&web=web.lg0304.xyz
```
或手机浏览器打开手机配对页 `https://pair.lg0304.xyz/pair`。

## 电脑端 Web 控制台
网页控制台固定：`https://web.lg0304.xyz/`（双击 **`open-web-console.bat`** 直接打开）。
若程序未在运行，该脚本会先自动拉起整套再打开。

## 验证是否打通
脚本窗口保持打开时，在手机/浏览器测试：
- `https://pair.你的域/pair` -> 200（配对页）
- `https://web.你的域/`   -> 200 且标题为 DeepSeek Harness