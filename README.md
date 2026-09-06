# DSH Mobile (Rust)

在 Android 手机上**独立运行完整 DeepSeek Harness（DSH）**的移动端，使用 **Rust 完全重写**其中核心服务端逻辑与构建管线的开源实现：一个原生 WebView 壳 + 内嵌 aarch64 Node.js 运行时 + 裁剪后的 DSH 依赖树。手机自己启动 DSH 服务（`127.0.0.1:3080`），不依赖电脑，可离线于 PC 使用。

仓库同时包含一个 **Windows 桌面守护进程（`desktop/`，`dsh-desktop`）**，让 DSH 也能直接在 Windows 上运行，并自研了 **DSH Link 配对协议**：电脑端启动后生成配对链接/二维码，手机扫码或填链接即可配对，通过 WebSocket 通道与电脑端双向同步（查看会话、向电脑端下发指令），参考了 [Paseo](https://github.com/getpaseo/paseo) 的「扫码配对 → 多端访问本地 daemon」模型。

> 本仓库是原项目 [aojiepp/dsh-mobile](https://github.com/aojiepp/dsh-mobile)（Java + Python 构建）的 **Rust 重写版**：将原先由 Java/Python 承担的「Node 进程守护、payload 解包、端到端 URL 解析、崩溃重启、APK 构建」等逻辑全部迁移到 Rust，Java 仅保留一层极薄的 WebView 壳。

## 为什么是 Rust
- 核心逻辑（进程管理、状态机、payload 打包与解包、端口/URL 解析、JNI 桥）由 Rust 实现，交叉编译为 `libdsh_mobile.so`，经 JNI 暴露给 Java。
- 构建管线（payload 依赖裁剪、aapt2/d8/zipalign/apksigner 编排、签名）也由 Rust 工具 `dsh-tool` 完成，零 Gradle。
- 单二进制、无运行时依赖、内存安全，便于审计与分发。

## 架构
```
┌────────────────────────── DSH Mobile (Rust) APK ──────────────────────────┐
│  MainActivity (WebView 全屏)       DshServerService (前台服务)              │
│       │ 127.0.0.1:3080                    │ spawn (Rust dsh-core)          │
│       ▼                                   ▼                                │
│  ┌─────────┐  首次运行解压   ┌───────────────────────────────────────────┐ │
│  │ WebView │◄──────────────│  payload/termux/usr/bin/node --expose-     │ │
│  └─────────┘               │    internals lib/bin.js web --port 3080    │ │
│                            │    (DSH_HOME = files/dsh-home)             │ │
│                            └───────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────┘
```
- **Rust 核心（`core/`）**：`dsh-core` crate —— Node 内置服务启动/停止、stdout 解析（捕获带 launch token 的入口 URL）、进程崩溃自动重启、状态落盘、端口就绪检测。纯 Rust，无 Android 依赖，可在宿主上单测。
- **JNI（`app/`）**：`dsh-app` crate —— JNI 绑定，将 `dsh-core` 能力暴露给 Java。
- **Java 壳（`android/`）**：仅 `MainActivity` / `DshServerService` 等少量文件，负责 WebView 渲染与前台服务承载。
- **构建工具（`tool/`）**：`dsh-tool` —— 全 Rust 构建链：Termux `.deb` 解包 → ELF 动态节分析 → RUNPATH 改写 → DSH 依赖树裁剪与原生模块 stub → `payload.zip` 打包 → APK 资源编译 / dex / 签名 / 对齐。

## 目录结构
```
dsh-mobile-rust/
├── Cargo.toml                    # workspace：core / app / tool / link / desktop
├── core/                         # 纯 Rust 核心逻辑（服务管理、状态、端口）
├── app/                          # Android JNI 接口与实现
├── link/                         # DSH Link 配对协议 crate（密钥/链接/报文编解码，两端共用）
├── desktop/                      # Windows 桌面守护进程 dsh-desktop（配对服务 + DSH 拉起）
├── tool/                         # Rust 构建工具（payload 打包、APK 构建）
├── android/                      # Java 壳、资源、Manifest
│   ├── java/com/rustdsh/mobile/  # MainActivity / DshServerService / SettingsActivity …
│   ├── assets/payload.zip        # 构建产物（不入库，由工具链生成）
│   └── res/
├── fetch-debs.sh                 # 下载 Termux 运行时 .deb
└── debs/ payload/ target/ dist/  # 归档与构建产物（已 gitignore）
```

## Windows 桌面端 + DSH Link 多端配对

### 多端架构
```
┌──────────────────────────── 手机（Android DshCommunity） ────────────────────────────┐
│ MainActivity(WebView)  DshServerService(本机3080)   SettingsActivity ┌ 配对/通道 UI │
│                                                  DshChannel ──────────┐            │
└───────────────────────────────────────────────────────────────────┬───┴────────────┘
                                                                    │ 配对链接/二维码
                                                                    │ (dsh-link://) + WebSocket /ws?key=…
   Windows 电脑 · dsh-desktop ─────────────────────────────────────┴───────────┐
 │  node 子进程 → DSH Web (0.0.0.0:3080)    配对/控制服务 (0.0.0.0:5780)          │
 │  （崩溃自动重启）                        · /pair 配对页                       │
 └──────────────────────────────────────────· /ws    通道（hello/session_snap）──┘
```

### DSH Link 配对协议（`link/` crate，自研，两端共用）
- 每次启动 `dsh-desktop` 生成一次性 `PairingKey`（20 字节 hex 40 位），作为通道的预共享密钥。
- 链接编码为二维码负载 / 可复制文本：`dsh-link://<host>[:port]/#key=<hex>`（另有 `http://…/pair?key=` 浏览器兼容形态）。
- 通道：客户端以 `GET /ws?key=<hex>` 发起 **WebSocket** 升级（RFC 6455，客户端掩码帧），随后按 `Message` 报文信封收发：
  - 客户端 `hello` → 服务端 `hello_ack` + `session_snap`（电脑端会话摘要，自动分发到手机）；
  - 客户端 `send_msg` → 服务端落盘 `$DSH_HOME/dsh-link.received.jsonl` 并回 `ack`，供上层接管执行。
- Rust 与 Android 端（`DshChannel.java`，纯 Java 手写 WS，零三方依赖）解析逻辑逐字段一致，单测与端到端验证通过。

### Windows：免安装包（开箱即用，推荐）
无需单独安装 Node.js 或其他运行时。从 GitHub Releases 下载 **`DSH-Desktop-win-x64.zip`**，解压到任意目录（保持整个文件夹完整），**双击 `start.bat`** 即可：

```text
DSH-Desktop/
├── start.bat              # 局域网模式：双击启动（自动打开浏览器配对页）
├── start_cloudflare.bat   # 异网络模式（推荐，免服务器）：自动开 Cloudflare 隧道
├── cloudflared.exe        # Cloudflare 隧道客户端（已内置）
├── dsh-desktop.exe        # Windows 守护进程（仅依赖系统自带 DLL）
├── node.exe               # 内置 Node 运行时（已随包附带，无需安装）
└── payload/dsh-app/       # DSH Web 前端/服务源码（已内置）
```

启动后会自动拉起内置 DSH Web 服务，并弹出浏览器打开 `http://127.0.0.1:5780/pair`。手机打开本 App「设置 → 连接电脑」，**扫码**配对页二维码，或**粘贴**终端窗口里打印的 `dsh-link://…` 链接，即可完成配对同步（详见下方「手机端配对」）。

### Windows：从源码构建（开发者）
```bash
# Rust 交叉编译到 Windows（在 Windows 本机或 CI 中）
cargo build --release -p dsh-desktop --target x86_64-pc-windows-msvc
```
发布目录内需要三样东西，组成一个免安装运行包：
1. `dsh-desktop.exe`（本 daemon）
2. 一个 `node.exe`（DSH 的运行依赖；Windows x64 官方安装包里的 node 即可）
3. `payload/dsh-app/`（DSH 前端/服务 JS 源码树，即本仓库 `payload/dsh-app`）

```bash
# Windows 命令提示符：启动桌面端（自动拉起 DSH Web，并开启配对服务）
dsh-desktop.exe --app-dir payload\dsh-app --node node --home dsh-home
```
启动后终端会打印**配对链接**并把 `http://<电脑IP>:5780/pair` 配对页输出到浏览器（含**二维码**）。

### 手机端配对（两种方式任选）
1. **扫码**：电脑端配对页显示二维码，用手机任意带扫码能力的 App 扫一下，即可唤起本 App 并自动进入「连接电脑」配对。
2. **填链接**：在 App「设置 → 连接电脑」里粘贴 `dsh-link://…` 链接 →「使用配对链接连接」。

> 说明：手机与电脑需在同一局域网（电脑的 5780/3080 端口防火墙放行）。配对成功后在设置页可见「已配对：dsh-desktop@主机名」，并列出**电脑端 DSH 会话数量**；点「向电脑端发送测试指令」可验证双向通道（电脑端会落盘 `dsh-link.received.jsonl` 并应答）。

### 异网络配对（跨网络）

默认 `dsh-link://…` 指的是电脑的局域网地址，手机必须与电脑同网。要实现 **手机与电脑在不同网络也能配对**，准备了两种方案，**推荐方案一（免服务器、免域名、免 token，开箱即用）**：

#### 方案一：Cloudflare 快速隧道（推荐，免服务器）
免安装包里已内置 `cloudflared.exe`，**双击 `start_cloudflare.bat`** 即可自动完成异网络穿透，无需任何账号、token 或域名：

```text
DSH-Desktop/
├── start_cloudflare.bat   # 异网络模式（推荐）：一键开 Cloudflare 快速隧道
└── cloudflared.exe        # Cloudflare 隧道客户端（已内置）
```

原理：`start_cloudflare.bat` 会用 `cloudflared` 创建**两个临时公网隧道**（`*.trycloudflare.com`）：

```
 隧道 A -> 本机 5780（配对/WSS 通道）      隧道 B -> 本机 3080（DSH Web UI）
```

启动后自动把两个公网地址交给 `dsh-desktop`，配对链接/二维码自动变为 WSS 加密形态：

```
dsh-link-wss://xxxx.trycloudflare.com/#key=<hex>&web=yyyy.trycloudflare.com
```

手机（**任意网络**）扫码或粘贴该链接后，经 **TLS 加密通道**与电脑端配对、双向同步，体验与局域网完全一致，且电脑无需开放任何端口、无需公网 IP。

#### 方案二：自建中继 `dsh-relay`（需一台公网服务器）
两端**都主动出站连到中继**，中继按配对密钥撮合两端后做帧级双向透明转发（业务 hello/session_snap/send_msg 由两端自完成，中继不解析内容）。

```
 手机（任意网络）                中继 dsh-relay（公网可达）              电脑 dsh-desktop
        │  ──出站 WebSocket──►  ws://relay:5781/ws?key=…  ◄──出站 WebSocket──  │
        └──────────────► 中继按 key 撮合两端，双向转发帧 ◄──────────────┘
```

配置步骤：
1. 在**公网可达**的服务器上部署 `dsh-relay`（Release 附带 `dsh-relay.exe` / `dsh-relay-linux-x64`），放行 TCP `5781` 端口并运行。Windows 服务器直接双击 `start_relay.bat`。
2. 在电脑端免安装包目录新建 `relay-server.txt`，填入一行中继地址，例如 `my-server.com:5781`。
3. 双击 `start.bat`。`dsh-desktop` 额外以中继地址生成配对链接/二维码，并在后台出站连中继。
4. 手机扫码/填 `dsh-link://my-server.com:5781/#key=…` 即完成**异网络**配对。

> 安全边界：中继/隧道为**透明+WSS 传输加密**（在公共链路上保密；明文帧由配对 key 约束两端身份）。如需更强的端到端加密（Curve25519 ECDH + XSalsa20-Poly1305，参考 Paseo）可作后续增强。

## 构建（全 Rust，零 Gradle）
需要：Rust 稳定版、`aarch64-linux-android` 交叉目标 + NDK 链接器、Android SDK（build-tools + platform android-34）、JDK 17。
```bash
# 1. 下载并解包 Termux 运行时（node + bash + ripgrep + 依赖 .so）
./fetch-debs.sh

# 2. 用 dsh-tool 构建 payload.zip 并放入 android/assets，再构建 APK
cargo build --release -p dsh-tool
target/release/dsh-tool build-apk <android-dir> <out.apk> \
    --version-code 31 --version-name 3.1 \
    --native-so target/aarch64-linux-android/release/libdsh_mobile.so \
    --sdk <ANDROID_SDK> --java-home <JDK17> --keystore debug.keystore --ks-pass ... --alias ...
```
产物：`dist/dsh-mobile-release.apk`。

## 安装与使用
- 首次启动解压 payload 约 1–3 分钟，之后秒开。
- **API Key**：App 设置页填写（写入 `$DSH_HOME/.env`），或启动后在 DSH 界面「设置 → 模型」里存储。
- **双模式**：设置页可切换「本机内置服务」/「连接电脑上的 DSH」。前者支持 USB + adb reverse 隧道与断连自动重连；后者支持 `dsh-link://` 扫码/填链接配对（见上文「DSH Link 配对协议」），配对后经 WebSocket 通道与电脑端双向同步。
- 通知权限用于前台服务保活；进程随应用关闭而退出。

## 已知限制与设计取舍
| 项 | 说明 |
|---|---|
| targetSdk 28 | Android 10+ 对 targetSdk≥29 的应用禁止执行私有目录二进制（SELinux exec）；28 是 Termux 同款解法 |
| 硬链接 `link()` 不可用 | Android SELinux 禁止应用硬链接；DSH 会话/附件落盘与 write 工具的新建文件路径均改写为 `rename()` |
| 交互式终端 / 图像附件 / 沙箱隔离 | 无非对应原生原语（node-pty/sharp/landlock 无 arm64 产物），已打 stub，调用时干净报错而非崩溃 |
| 体积 | APK 约 64MB，解压后占用约 300MB |
| 服务生命周期 | Node 进程随 App 进程存活；被系统杀掉后重新打开 App 会自动拉起 |

## 许可
本仓库采用 [MIT License](LICENSE)。第三方组件按各自许可分发：Termux nodejs 运行时来自 [Termux 官方仓库](https://packages.termux.dev/)；DeepSeek Harness 依赖树来自 npm 安装的 checkout，仅用于运行时，版权归其各自作者。