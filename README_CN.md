# macrdp — 开发分支

[English](README.md) | **简体中文**

基于 Rust 和 IronRDP 的原生 macOS RDP 服务端。本仓库 fork 自
[clintcan/macrdp](https://github.com/clintcan/macrdp)，主要开发可靠的显示配置与连接生命周期处理。
远程连接使用当前已登录的 Mac 桌面，不会创建独立的 Windows 式登录会话。

## 分支约定

- `main`：与上游 `main` 完全一致的同步基线，不加入本仓库的修改。
- `dev`：本仓库的功能开发、文档、CI 与构建产物。先同步 `main`，再将上游更新合入 `dev`。

首次开发基线为上游提交
[`29d01ed`](https://github.com/clintcan/macrdp/commit/29d01ed72946ebde25ca08c924429b5fc37de7c5)。
[README_UPSTREAM.md](README_UPSTREAM.md) 保留上游原文，其中的发布、验证和支持声明属于上游，不能视为本 fork 的验证结果。

## dev 分支的差异

| 范围 | 已实现行为 |
| --- | --- |
| 显示策略 | 明确区分跟随客户端与固定分辨率，动态调整独立配置；自动界面缩放依据客户端实际发送的信息。 |
| 配置可靠性 | 共用配置解析，提供无运行副作用的 `--capabilities` / `--check-config`；控制器先校验候选文件，再原子保存、检查激活结果并提供恢复。 |
| 真实状态 | 展示 PID、配置版本、启动权限、请求/协商像素、实际 backing 像素和逻辑尺寸、原生捕获及回退信息；未知值保持未知。 |
| 会话生命周期 | 使用最新布局队列、会话代次和过期任务检查，输入映射依据实际尺寸。 |
| 重连稳定性 | QoE 黑屏恢复默认关闭，0 毫秒耗时不能证明黑屏；实体屏安装与恢复在网络异步工作线程之外串行执行。 |

Display 设置页提供这些策略，并区分请求配置与实测状态。详见
[显示配置](docs/display-settings.md)、[控制器说明](gui/README.md)和 [CLI 文档](docs/cli.md)。

## 修饰键位置与 Ctrl 快捷键兼容

两项设置独立存在于 **Input → Modifier key positions（修饰键位置）** 与
**Windows editing compatibility（Windows 编辑兼容）**。
未配置 `MODIFIER_LAYOUT` 时使用 `standard`，原有 `MAP_CTRL_TO_CMD` 的含义保持不变。

| Windows 实体按键 | `standard` 标准 | `swap-left-win-alt` 仅交换左侧 | `swap-win-alt` 交换左右两侧 |
| --- | --- | --- | --- |
| 左 Ctrl | Control | Control | Control |
| 左 Windows | Command | Option | Option |
| 左 Alt | Option | Command | Command |
| 右 Windows | Command | Command | Option |
| 右 Alt | Option | Option | Command |

推荐的 Mac 按键位置组合，让靠近空格的左 Alt 承担 Command，并保留 Ctrl：

```dotenv
MODIFIER_LAYOUT=swap-left-win-alt
MAP_CTRL_TO_CMD=0
```

此时左 Alt+C/V 对应 Command+C/V，左 Alt+Tab 对应 Command+Tab；Ctrl+C 仍是 Control+C，
终端中也一样。若还需要 Windows 编辑习惯，可以独立开启 `MAP_CTRL_TO_CMD=1`，
使选定的 Ctrl 编辑快捷键和 Ctrl+点击转成 Command 等效操作，继续保留终端和应用排除规则。
该开关不会将所有 Ctrl 组合转换为 Command。

`swap-win-alt` 会额外交换右侧按键，可能影响 AltGr 输入；仅交换左侧的模式不映射右 Alt。
位置映射作用于实际收到的 RDP 扫描码，覆盖对应的按下、松开以及键盘/鼠标修饰键状态，
不会更改 Mac 本机键盘。客户端可能提前拦截某些组合，服务端无法恢复没有收到的事件。
可选的 Option 快捷键按映射后的 Option 识别（交换后为 Windows 键），界面会显示对应的物理按键提示。
维护快捷键始终保持物理 **Ctrl+Alt+G** 与 **Ctrl+Alt+Shift+R**。

CLI 参数为 `--modifier-layout standard|swap-left-win-alt|swap-win-alt`，
`--map-ctrl-to-cmd` 独立控制编辑兼容。应用配置会重启服务；未知布局以及专用字段与 Extra flags 的重复映射会被校验拒绝。
自动检查覆盖映射和状态变化，但新增功能尚未用真实 Windows 快捷键和 AltGr 输入完成验证。

## 构建与运行

当前构建产物面向 **Apple Silicon Mac**。CI 使用 Rust 1.95 和兼容 Swift 5.9+ 的 macOS SDK；
本机开发也使用了 Rust 1.100。构建前安装 Xcode 命令行工具、Rust 和 CMake。

```sh
git clone --branch dev https://github.com/zhangyxXyz/macrdp.git
cd macrdp
cargo build --release --locked
# 将两个 app 组装到暂存目录，可替换为其他可写的绝对路径。
APP_DIR="$PWD/target/apps" packaging/make-app.sh
APP_DIR="$PWD/target/apps" gui/make-tray-app.sh
```

配套安装两个 app，并按[打包说明](packaging/README.md)配置 LaunchAgent 和认证。
在系统设置中为安装后的 macrdp 开启“录屏”和“辅助功能/设备控制和数据访问”，然后重启服务。
默认 bundle ID 与上游控制器和 LaunchAgent 保持兼容，因此会替换已有上游安装，而非并行安装。
替换前备份 app 与配置。

本 fork 的产物使用**临时签名（ad-hoc）**，没有 Developer ID 公证。
重新编译会改变签名哈希，可能需要在隐私设置中移除并重新添加 app。
上游 Homebrew 安装命令安装的是上游发布版。本 fork 的构建工作流不包含摄像头系统扩展。

## 原生 4K、200% 界面

将以下键合入 `~/Library/Application Support/macrdp/config.env`，保留自己的监听地址、认证、TLS 和设备设置。

```dotenv
VIRTUAL_DISPLAY=1
PRIMARY_MODE=detach
VD_WIDTH=3840
VD_HEIGHT=2160
VD_HIDPI=1
RESOLUTION_POLICY=fixed
LIVE_RESIZE=0
ENABLE_H264=1
STATS_ENDPOINT=1
BLANK_RECOVERY=0
```

应用前先校验：

```sh
/Applications/macrdp.app/Contents/MacOS/macrdp --check-config "$HOME/Library/Application Support/macrdp/config.env"
```

原生 4K/200% 指协商和 backing 均为 3840×2160 像素，逻辑尺寸为 1920×1080。
客户端的位图拉伸是另一项设置。应用服务端配置会重启服务并断开当前连接。

## 已验证事实与边界

2026-11-15，在一台本地 Apple Silicon Mac 与 Windows 自带 mstsc build 26100 上验证了
3840×2160 原生捕获、200% 界面与 60Hz 虚拟显示器。固定 4K 会话持续 6 分 12 秒，
随后两次重连没有触发自动 QoE 恢复断线。重连从接受连接到服务端 H.264 就绪约 0.73～0.75 秒，
这不代表客户端实际首帧显示时间。实体屏切换本身仍约需 10.5 秒。
60Hz 模式与 60fps 目标也不等于持续帧率保证。

本地检查：399 项 Rust 测试通过、7 项忽略；10 项 Foundation 配置检查与 25 项 CLI 进程检查通过。
硬件行为、权限和客户端实际显示仍需手动验证。跟随客户端与动态调整依赖客户端发送对应请求，
固定 4K 的 mstsc 测试不能代表所有客户端和缩放策略。本 fork 不额外宣称长期压力测试或生产成熟度。

## Actions 与维护

- `ci.yml`：`dev` 变更和 PR 执行 Linux/macOS 回归、配置检查与审计集成检查，也支持手动运行。
- `dev-build.yml`：手动构建 Apple Silicon 版本，检查 Rust/Swift，输出两个 app、校验和与源提交信息；产物保留 14 天，不发布 Release。
- `release.yml`：保留上游基于标签的发布流程，补充控制器产物；本 fork 的开发构建使用上一项工作流。
- `security.yml`：依赖检查，包含 `dev` 上的依赖变更。
- `delete-old-workflow-runs.yml`：沿用 [CLIProxyAPI-Plugins](https://github.com/zhangyxXyz/CLIProxyAPI-Plugins) 的策略。每个 workflow 保留最新 6 次记录和最近 30 天的全部记录，只清理更旧且已结束的记录。push 只预览，手动默认预览，每周定时清理。

定时工作流从仓库默认分支 `dev` 运行。同步上游时，先将 `main` 快进到 `upstream/main`，
再把 `main` 合入 `dev`；冲突和本仓库差异都在 `dev` 处理，README 和工作流修改不进入 `main`。

## 许可证与致谢

本 fork 的修改沿用上游 **MIT OR Apache-2.0** 许可，详见
[LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)。

感谢 **[clintcan](https://github.com/clintcan)** 及
[macrdp 贡献者](https://github.com/clintcan/macrdp/graphs/contributors)提供原始服务端、协议集成、打包工具和文档。
同时感谢 [IronRDP](https://github.com/Devolutions/IronRDP) 及其贡献者。
