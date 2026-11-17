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

## 已纳入的上游更新

同步上游至 [`fb1facf`](https://github.com/clintcan/macrdp/commit/fb1facf2d0666c8779a92b943221a4270432734d)，
纳入 [#262](https://github.com/clintcan/macrdp/pull/262)：可选 TCP H.264 帧确认积压限制
（`H264_MAX_UNACKED_FRAMES`，默认 `0`，关闭）。客户端落后时丢弃部分捕获帧，保留约
10 fps 的低速发送；TCP 末尾补帧不受此限制，避免最后一次画面变化迟迟不显示。
这是发送节奏阈值，并非严格的队列长度上限。上游报告已测试 Windows App 和 mstsc；
本 fork 尚未在自己的 4K／200% 客户端测量开启后的效果。配置检查同时报告请求值与
有效值，未启用 H.264 时不生效。此次还纳入 #268 的迟到 S_OK 有损音频回归测试，
并在合并历史中统一此前已移植的 #230 启动画面修复。保留本 fork 的锁屏、显示和剪贴板策略。

## 当前开发版

上游 Windows 10 mstsc 黑屏修复 [#228](https://github.com/clintcan/macrdp/pull/228)
包含在同版本重构建的 `v0.10.6-dev` 中，版本号保持 `0.10.6`；可通过下载包中的
`SOURCE_COMMIT` 区分构建。更新后的 Win10 实际显示仍需客户端验证，上游独立的启动画面
冻结修复 #230 也已纳入。详见[更新记录](CHANGELOG.md#v0106-dev)。

## 本次同版本重构建

本次复核将文件描述符与源路径绑定为同一次公告快照，非法列表整轮拒绝，避免索引错位；
丢弃旧会话排队的文件请求及租约释放。断开锁屏确认同时检查是否已经要求认证，避免将
屏保宽限期误判为安全锁定。仅远端音频的打开／读取失败共用有限重试预算；设置重启先
等待旧进程原生清理，再检查新进程，新装 LaunchAgent 允许 60 秒退出清理。
依据、已完成检查与手动验证边界见[审查记录](docs/release-review-2026-11-17.md)。

后续依赖安全源码更新采用 `libz-sys 1.1.33` 和官方稳定版 `rsa 0.10.0`，移除旧 RSA
漏洞例外。三个小范围[认证适配包](vendor/AUTH-SECURITY.md)替换上层精确绑定的候选版，
保留当前协议栈；这次源码更新尚未替换本机 `3bea2b7`。稳定 RSA 拒绝小于 1024 位的
公钥，不恢复弱密钥兼容。
依赖回归还发现了上游 EC 坐标补零缺陷，已连同 JWE 错误统一处理修复回移；剪贴板
回退暂存目录改为原子私有创建，避免并发时间戳撞名。

2026-11-18，用户确认安装来源为 `3bea2b7` 的版本中，Directory Opus 文件复制和
常规锁屏／解锁正常。结论限于此次实际测试路径；无屏、快速重连改分辨率，以及捕获前
准备取消后的重新锁定，仍是独立的验证边界。

此前 `31ee2fb` 构建的反馈是远端音频、从 Windows 桌面复制文件正常，Directory Opus
复制失败。后续修正让失败的 `FileGroupDescriptorW` 请求重试一次，再失败则明确记录，
不用文本或旧图片替代。后来的 `3bea2b7` 实测已确认用户的 Opus 复制路径可用；客户端
仍须提供有效的标准文件流。

Detach 恢复保留所有由服务禁用的显示器，即使它们已从 CoreGraphics 在线列表消失。
事务返回成功后还要确认每台屏幕恢复活动状态；失败时执行有界恢复。开启断开锁屏时，
恢复重启必须先确认锁屏，并避免打断新会话。用户已确认 `f2d30dc` 能自动恢复两台屏幕。
日志显示断开宽限约 2.5 秒、实体屏恢复约 10.5 秒、最后锁屏约 0.13～0.15 秒，期间先
露出未锁定桌面。本次将 Detach 改为先锁屏再恢复，并在恢复期间保持与自动解锁
互斥；Capture／Shield 仍先恢复。恢复完成后不再补一次延迟锁屏。仅虚拟屏活动时的
实际锁屏探针已在 102 毫秒内确认锁定；用户于 2026-11-18 确认 `3bea2b7` 常规锁屏／
解锁正常，完整时序测量和边界场景覆盖仍需另行验证。
本次故障中，重启旧服务未能恢复输出，重新插入视频线后两台屏幕才恢复；进程重启是
一种恢复尝试，不能保证已经卡住的视频链路一定恢复。

收到新的远端剪贴板声明时，即使格式不受支持，也会使旧文本／图片失效。
文件 URL 改为显式序列化，读回核对后才报告发布成功；保留 File Provider 按需下载。
Windows 私有格式仍需要客户端提供标准 `FileGroupDescriptorW` 文件流。

权限页新增可选的「完全磁盘访问权限」入口，用于 Mac→Windows 复制受保护位置的文件。
请在系统设置中授权服务端 `macrdp.app`；该权限不能修复 Windows 传入格式。
断开锁屏取消原有默认额外 22.5 秒等待，仍需经过 2.5 秒重激活宽限及有上限的重连保护；
Detach 先锁屏再恢复实体屏，其他模式先恢复。不等于瞬间锁屏，也没有新增本机操作接管检测。
4K／200% 时，macOS 的 1920×1080 逻辑工作区与 3840×2160 RDP 输出是不同指标。

File Provider 文件改为当前用户可写的本地副本，避免把只读锁定标记带到粘贴结果。
本地修改按版本持久化，不回写 Windows 源文件。10 项适配层检查和 8 项合成源系统检查
覆盖按需读取、导出文件可编辑、离线清除缓存后恢复及重连隔离；这些自动检查本身不等同
真实 mstsc 行为，上述 `3bea2b7` 的 Opus 复制结果是独立的客户端实测。
取消菜单栏悬浮状态提示，状态仍可在菜单和 Status 页查看。
请通过 `SOURCE_COMMIT` 区分此次重构建；其余客户端验证见[审查记录](docs/release-review-2026-11-17.md)。

音频增加默认关闭的实验选项 **Audio → Remote-only playback**（`AUDIO_REMOTE_ONLY=1`）。
它使用 macOS 14.2+ Core Audio 私有 Tap，仅在捕获期间抑制本地播放，不修改系统音量、
静音值或默认输出设备；需要系统音频录制授权。初始化失败会报告错误，不自动改用
双端播放路径。已建立的 Tap 对打开／读取失败最多共用 5 次退避重试，只有有效双声道样本重置预算；首次授权失败仍需
先修正权限。最小化时仍遵循现有远端暂停音频策略；断开销毁 Tap，恢复本地播放。
Status 页报告捕获状态和错误，不能把“capturing”当作 Windows 已能听到。
本机独立诊断已捕获 48 kHz 非零提示音并成功销毁 Tap；用户已确认 `31ee2fb` 的 mstsc
音频正常，输出设备切换、断线重连和异常退出仍待实测。此前直接切换扬声器静音的对照测试未能保留远端提示音，
因此不采用这种实现。此前偶发“仅 Mac 出声”在重启后恢复，尚不能确认其独立根因。
断开锁屏还增加捕获代次检查、合并通知处理及与自动解锁互斥，避免旧任务锁住重连会话。

无实体显示器的已登录会话也按同一锁屏策略处理：没有其他在线显示器时，Detach／Blank
不执行实体屏事务，虚拟屏在断开锁屏完成或取消后可释放；“仍在线但未恢复”的显示器
不会当作已拔掉。无屏重连在认证后先创建临时虚拟显示面，再自动解锁；等待者取消时，
显示租约仍保持到正在执行的解锁操作结束。Shield 即使暂时没有目标，也要求辅助进程
确认，以保留会话期间插入显示器的遮挡能力。
这些路径已加入自动回归，真实拔屏、重新插屏、无屏锁定重连仍待新版客户端实测。
范围是已登录用户会话，不包含 FileVault／首次登录前的开机画面。若连接恰在自动解锁
输入过程中被取消，输入可能仍完成；准备过程取消或解锁后捕获失败、尚未进入会话计数时，
目前不能保证重新锁定，不能把它当成“连接取消一定保持锁定”。

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

## 虚拟显示器的生命周期

虚拟显示器改为通过认证、准备捕获时创建，服务启动时不再创建闲置的扩展桌面。
最后一路捕获结束后，先经过 2.5 秒重激活宽限并等待实体屏恢复，再移除虚拟屏；
快速重连会使旧清理任务失效。捕获、输入、音频和切换器 HUD 使用当前显示器 ID。
新建显示器及确认解锁后，最多等 8 秒让目标屏出现在 SCK 中；出现即继续，
这是固定上限，并非每次等待 8 秒。

仍在线的实体屏无法恢复活动状态时保留虚拟屏。确认没有其他在线显示器时，
断开锁屏完成后可以释放虚拟屏；无屏锁定重连会先准备新的显示面，再自动解锁。
固定 4K／200% 等配置保留，供下次连接使用。用户于 2026-11-18 确认 `3bea2b7` 的
常规锁屏／解锁正常；无屏重连、热插拔和快速改分辨率仍需实机覆盖。仅发布不会替换
本机 app 或修改配置。

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

已合入上游 `v0.10.6`（`0255a8b`，#267）：launchd 启动时若存储的密码缺失或错误，
服务等待凭证修正，仅在密码变化后重新验证，避免反复重启造成重复登录失败。
控制器显示等待状态并提供密码修正入口。这与每次连接的自动解锁独立，
配置检查仍不触发真实登录或写入状态文件。

## 上游 NV12 捕获实验

已合入上游 [cef6073](https://github.com/clintcan/macrdp/commit/cef6073ba461c595261a2705a2c84888cd522673)
（[#204](https://github.com/clintcan/macrdp/pull/204)）：`--capture-nv12` /
`CAPTURE_NV12=1` 让 ScreenCaptureKit 输出全范围 BT.709 NV12，直接交给 VideoToolbox，
减少逐帧 CPU 色彩转换和复制。需要 H.264 才生效，**默认关闭**；初始化及不支持 AVC
的客户端仍有 BGRA 位图回退。上游报告的性能提升不能视为本机 4K 实测结果。

合并保留已认证连接的捕获前解锁、固定 4K/200% 策略、键盘映射及 File Provider 剪贴板。
配置检查区分 NV12 请求值和实际启用条件，校验布尔值并拒绝专用字段／Extra flags 重复。
没有为本机开启此选项。此次合并后的真实 Windows／NV12 显示仍待手动验证。

## 锁屏连接与断开锁屏

使用无头显示模式并开启 `AUTO_UNLOCK=1` 时，自动解锁现在在 **RDP 凭证／ARC
校验成功后、ScreenCaptureKit 捕获之前**执行，消除锁屏或锁屏显示器熄灭后
`SCK sees: []` 导致先断开、会话 watcher 永远无法解锁的循环依赖。
确认解锁后，最多等待 8 秒让指定显示器出现在 SCK 中。锁定状态未知、不安全或密码
提交预算耗尽时明确报错；保留每轮锁屏共享的两次提交限制，并串行执行。
自动解锁仍需主动开启，`--skip-auth` 下禁用。

`LOCK_ON_DISCONNECT=1` 改为通过运行时解析的私有 login.framework 请求真正的
立即锁定，并最多检查 2 秒系统锁定标志。不依赖屏保的密码延迟，不修改系统安全设置，
不回退成启动屏保。API 不存在、调用失败或锁定状态未确认都会记录失败。
实体屏恢复、断线宽限、安全等待和有上限的重连保护保持原样，**不是断开瞬间就锁屏**。
自动解锁会影响实体 Mac 桌面；私有 macOS API 存在随系统升级变化的风险。

回归覆盖准备顺序／失败、串行调用、显示器选择、真实状态确认，以及已有重连保护和
密码预算策略。**用户于 2026-11-18 确认安装来源 `3bea2b7` 的常规锁屏／解锁正常。**
这不等于验证了无屏重连，也不保证准备取消或解锁后捕获失败、尚未计入会话时能够重锁。
构建校验不会修改正在运行的 app 或刷新 TCC 授权。

## Windows 剪贴板图片与文件

PNG／DIB 截图优先于其虚拟图片文件表示；Explorer 文件复制即使带缩略图也走文件流。
文件准备期间先用不可粘贴的标记替换旧文本，文件名／预览不会降级为文本片段；
延迟的远端回复不会覆盖后来在 Mac 上复制的内容。完整下载回退只播放就绪提示音，
不再自动发送 Cmd+V；文件就绪后请再次粘贴。

Mac→Windows 的剪贴板读取与图片编码移到有上限的后台阻塞线程，让 RDP 在等待读取时
仍能处理输入和 File Provider 物化请求。上限为 4 个读取线程、32 个排队／待发送操作，
每个请求含排队最多 30 秒；慢读／卡住时返回失败。无法取消的系统读取会继续占用线程名额，
直到真正返回。无 ID 的格式回复保持原顺序，文件请求绑定原锁对应的路径，
断开会话的迟到回复不能进入新连接。
回复直到分发结束才释放并发名额，并保持批次内剪贴板先于视频的优先级；
每批最多 32 个事件，让入站协议处理有机会重新获取会话锁。这不是网络写入超时：
TCP 拥塞仍可能拖慢大回复。自动检查不能证明实机卡死已经消失，新构建仍需 mstsc 复制测试。
详见[剪贴板调查与上游差异审查](docs/clipboard-followup.md)。

已纳入上游 [#230](https://github.com/clintcan/macrdp/pull/230)：首次协商 H.264 时，
暂存传统位图最多 3 秒，EGFX 就绪后提交；静止桌面也能继续显示。EGFX 未启动则回退
传统位图。`MACRDP_EGFX_LEGACY_HOLD_MS=0` 可关闭等待。上游 Win10 实测不代表本 fork
已完成本机验证。

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
同域同名重连和旧 URL 拒绝。用户已反馈此前安装的 `92f4c8c` 构建测试正常；2026-11-18
又确认安装来源 `3bea2b7` 的 Directory Opus 文件复制正常，尚不覆盖所有文件类型、
文件夹规模及传输中断线场景。
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

本次 `v0.10.6-dev` 重构建的本地检查：491 项 Rust 测试通过、8 项默认忽略；严格 Clippy、
10 项 Foundation 配置、33 项 CLI 进程和 10 项 File Provider 适配层检查通过。原生音频缓冲检查
通过 AddressSanitizer／UndefinedBehaviorSanitizer；8 项合成源系统检查不能替代新版 mstsc 实测。
硬件行为、权限和客户端实际显示仍需手动验证。跟随客户端与动态调整依赖客户端发送对应请求，
固定 4K 的 mstsc 测试不能代表所有客户端和缩放策略。本 fork 不额外宣称长期压力测试或生产成熟度。

## Actions 与维护

- `ci.yml`：可复用回归入口，集中 Linux/macOS 测试、严格 Clippy、配置、原生适配、发布脚本和审计集成检查。macOS 检查共用一个 runner，Linux 同时运行；分支变更／PR 和手动运行也使用此入口。
- `release.yml`：仅推送 `v*` tag 触发“编译打包 → 草稿上传／下载校验 → 发布 → 清理”。CI 独立运行，不再阻塞发布；打 tag 前应完成必要审查和回归，tag 提交必须属于 `dev`。两个 app、来源信息和校验和直接进入草稿，不使用 Actions Artifacts。包含 `-` 的 tag 为预发布（例如 `v0.10.6-dev`）。
- Release notes 使用 [简短双语模板](packaging/RELEASE_NOTES.md)，链接到 [CHANGELOG.md](CHANGELOG.md) 对应版本，不复制整段更新记录；打 tag 前必须先补齐该节。
- `build-check.yml`：仅手动执行“检查 → 编译打包验证”，不发布、不上传归档；普通 dev 推送不执行打包任务。
- 同标签重构建使用独立的 `staging-<tag>-<source>` 草稿及稳定 Release／asset ID。新包回读验证并确认归属后才替换旧自有 Release，保留该版本 tag。GitHub 不支持原子交换 Release，最终发布失败时保留已验证草稿供重试；远端 tag／源提交核对会拒绝过期运行。失败后可重跑原工作流续传自有草稿。
- 原生任务默认使用 Apple Silicon `macos-26`；`MACOS_RUNNER` 可切换到其他 macOS runner，`macos-26-intel` 可应对 ARM 池容量不足。Intel 执行主机回归及 ARM64 Clippy／编译检查；所有打包组件明确指定 ARM64，并核对架构、源提交和嵌套签名。Intel 不能执行 ARM64 成品，`BUILD_INFO.json` 会记录主机架构，以及静态能力信息来自同源主机程序；真正 ARM64 运行单独验证。工作流名称统一小写。
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
