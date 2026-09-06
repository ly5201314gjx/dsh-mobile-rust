# DSH Mobile (Rust)

在 Android 手机上**独立运行完整 DeepSeek Harness（DSH）**的移动端，使用 **Rust 完全重写**其中核心服务端逻辑与构建管线的开源实现：一个原生 WebView 壳 + 内嵌 aarch64 Node.js 运行时 + 裁剪后的 DSH 依赖树。手机自己启动 DSH 服务（`127.0.0.1:3080`），不依赖电脑，可离线于 PC 使用。

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
├── Cargo.toml                    # workspace：core / app / tool
├── core/                         # 纯 Rust 核心逻辑（服务管理、状态、端口）
├── app/                          # Android JNI 接口与实现
├── tool/                         # Rust 构建工具（payload 打包、APK 构建）
├── android/                      # Java 壳、资源、Manifest
│   ├── java/com/rustdsh/mobile/  # MainActivity / DshServerService / SettingsActivity …
│   ├── assets/payload.zip        # 构建产物（不入库，由工具链生成）
│   └── res/
├── fetch-debs.sh                 # 下载 Termux 运行时 .deb
└── debs/ payload/ target/ dist/  # 归档与构建产物（已 gitignore）
```

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
- **双模式**：设置页可切换「本机内置服务」/「连接电脑上的 DSH」，后者支持 USB + adb reverse 隧道与断连自动重连。
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