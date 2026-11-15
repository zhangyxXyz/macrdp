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

## 当前开发版

上游 Windows 10 mstsc 黑屏修复 [#228](https://github.com/clintcan/macrdp/pull/228)
包含在同版本重构建的 `v0.10.6-dev` 中，版本号保持 `0.10.6`；可通过下载包中的
`SOURCE_COMMIT` 区分构建。更新后的 Win10 实际显示仍需客户端验证，上游独立的启动画面
冻结修复 #230 尚未包含。详见[更新记录](CHANGELOG.md#v0106-dev)。

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

## Windows 剪贴板图片与文件

PNG／DIB 截图数据继续优先于虚拟文件描述符，保留完整图片校验和 PNG／DIB 回退路径。

打包应用在 macOS 15+ 新增沙箱 **macrdp Files** helper 和 File Provider 扩展。
在**系统设置 → 通用 → 登录项与扩展 → 文件提供程序**中启用它，再重启 macrdp。
普通顶层文件不分图片或其他类型，可以发布真正的系统 dataless URL；应用直接读取、复制
时由系统触发下载。每次复制有独立的会话／副本／原始锁及目录，同名重连不会偷换内容。
已验证的本机路径不依赖 App Group 或 Developer ID 证书；临时签名仍不等于已验证公证分发。

文件源仅监听 loopback，使用每进程随机令牌，经域配置传给扩展；按原锁串行请求
128 KiB 分块。扩展流式写入私有文件，完成后才交给系统。增量元数据锚点会持久保存。
新复制会阻止旧任务覆盖剪贴板，但保留已有读取者的读取权。扩展不可用、不支持的批次、
旧版 macOS、`--no-lazy-paste` 或文件夹均使用**完整下载后再发布**的回退路径；不再发布
稀疏 presenter 占位文件。纯图片回退在原有图片安全限制内额外提供解码后的 PNG 数据。

边界：单批／元数据目录最多 4,096 项，单文件最多 128 GiB；broker 最多保留 64 个副本，
非当前副本闲置 10 分钟后回收，源锁最长两小时，过期需重新复制。文件传输要求客户端协商
剪贴板锁。过大或名称有歧义的复制可能被拒绝；失败不会发布全零替代文件。系统已经下载的
文件断线后仍可读取，未下载文件会报错，不会读取新会话的字节。退出服务时移除域，下一次
启动会回收确认属于已死亡进程的旧域。

自动检查包含 Rust 全量回归、9 项适配层／IPC 检查（含 >16 MiB 流式文件），以及 6 项使用
合成内容的真实 File Provider 检查：仅元数据时零 RANGE、直接物化、离线读取、断线读取、
同域同名重连和旧 URL 拒绝。用户已反馈此前安装的 `92f4c8c` 构建测试正常；
此次合入 Win10 修复后的同版本重构建需另行客户端实测。
详见[实现与检查](native/file-provider/README.md)和[更新记录](CHANGELOG.md#v0106-dev)。

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

`v0.10.6-dev` 首次 `92f4c8c` 构建的本地检查：430 项 Rust 测试通过、8 项默认忽略；显式运行的真实 OS broker 测试另行通过。
10 项 Foundation 配置、25 项 CLI 进程、13 项发布脚本、9 项适配层、6 项真实 File Provider 检查通过。
硬件行为、权限和客户端实际显示仍需手动验证。跟随客户端与动态调整依赖客户端发送对应请求，
固定 4K 的 mstsc 测试不能代表所有客户端和缩放策略。本 fork 不额外宣称长期压力测试或生产成熟度。

## Actions 与维护

- `ci.yml`：`dev` 变更和 PR 执行 Linux/macOS 回归、配置检查与审计集成检查，也支持手动运行。
- `release.yml`：只有推送 `v*` tag 才打包 Apple Silicon 应用，先通过 Rust／Swift／Python 检查，且 tag 提交必须属于 `dev`。两个 app、源提交信息与校验和直接上传草稿 Release，回读验证后发布，不使用中间 Actions Artifacts。包含 `-` 的 tag 为预发布（例如 `v0.10.6-dev`）。失败时重跑原 tag 工作流续传自有草稿；已发布版本拒绝覆盖。
- Release notes 使用 [简短双语模板](packaging/RELEASE_NOTES.md)，链接到 [CHANGELOG.md](CHANGELOG.md) 对应版本，不复制整段更新记录；打 tag 前必须先补齐该节。
- `cleanup-releases.yml`：手动默认只预览。发布成功后也会自动清理：先验证最新自有 Release，保留它，删除更旧的流水线自有 Release，同时删除这些旧自有 Release 对应的 Git tag。未知来源的 Release 和草稿不动；校验失败或发现并发编辑即中止。编排参考 CLIProxyAPI-Plugins，原先手动 `dev-build.yml` 归档构建已移除。
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
