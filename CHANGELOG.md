# Changelog / 更新记录

The package’s `SOURCE_COMMIT` identifies the source of each same-version rebuild.
同版本重构建可通过安装包内的 `SOURCE_COMMIT` 区分。[完整变更明细 / Detailed rebuild history](https://github.com/zhangyxXyz/macrdp/blob/e30641fea377b7599922d65d39df781c844dfa6b/CHANGELOG.md)

## v0.10.8

- Keep the host awake while allowing displays to sleep according to macOS settings. The default `caffeinate` process no longer blocks display sleep or declares user activity; `--allow-sleep` still permits system sleep.
- Follow a re-registered virtual display and retry sleep-related display setup. Add opt-in RDP connection quality reporting under **Connection**. [#231](https://github.com/clintcan/macrdp/pull/231) · [#232](https://github.com/clintcan/macrdp/pull/232) · [#235](https://github.com/clintcan/macrdp/pull/235)
- Configure the display wake timeout under **Display → Lock while away** (1–15 seconds, default 3). Monitor-free reconnects use the virtual display’s wake state to decide when auto-unlock can start.
- When a locked display sleeps and ScreenCaptureKit loses its source, macrdp disconnects and cleans up the virtual display. Other explicit capture stops get up to three seconds to recover. Empty capture samples can no longer evict the first usable frame.
- Update to stable `rsa 0.10.0` and `libz-sys 1.1.33`, remove the RSA audit exception, and backport Picky’s EC and JWE fixes. Clipboard transfers use atomically created private staging directories. [Dependency details](vendor/AUTH-SECURITY.md)

- 主机保持唤醒，显示器按 macOS 设置自动熄屏。默认的 `caffeinate` 不再阻止显示器休眠或主动声明用户活动；`--allow-sleep` 仍可允许主机休眠。
- 虚拟屏 ID 变化后跟随新 ID；显示器休眠导致配置失败时唤醒并重试。**Connection** 新增可选的 RDP 连接质量上报，默认关闭。
- 可在 **Display → Lock while away** 设置显示器唤醒等待上限（1～15 秒，默认 3 秒）。无屏重连通过认证后，服务会等虚拟屏就绪再自动解锁。
- 锁屏熄屏且 ScreenCaptureKit 丢失采集源时，服务断开会话并清理虚拟屏。其他明确的采集中断最多尝试恢复三秒。空采集样本会跳过，保留首个有效画面。
- 升级到稳定版 `rsa 0.10.0` 和 `libz-sys 1.1.33`，移除 RSA 审计例外，回移 Picky 的 EC 与 JWE 修复。剪贴板传输改为原子创建私有暂存目录。[依赖说明](vendor/AUTH-SECURITY.md)

## v0.10.6-dev

- Added on-demand Windows file copies through macOS File Provider, writable local copies, and bounded background reads for Mac-to-Windows transfers. Fixed stale clipboard data on file paste.
- Added remote-only audio playback. Virtual displays follow the authenticated capture session. In Detach mode, disconnect locking confirms the lock before restoring physical monitors.
- Integrated upstream Windows 10 display fixes, optional NV12 capture and TCP H.264 pacing. Same-tag releases verify a draft package before replacing the previous download. [Review and test record](docs/release-review-2026-11-17.md)

- Windows 文件可通过 macOS 文件提供程序按需下载，粘贴生成可编辑的本地副本。Mac 向 Windows 传文件改用有超时的后台读取，并修复文件粘贴时误用旧剪贴板内容的问题。
- 新增仅在远程客户端播放音频的选项。虚拟屏随认证后的采集会话创建和释放。在 Detach 模式下，断开锁屏先确认 Mac 已锁定，再恢复实体显示器。
- 合入上游 Windows 10 显示修复、可选 NV12 捕获和 TCP H.264 发送节奏控制。同标签发布先验证草稿安装包，再替换原下载。[审查与测试记录](docs/release-review-2026-11-17.md)

## v0.10.5-dev

- Windows screenshots prefer PNG, DIBV5 and DIB in that order. Image files are downloaded and validated before publication to the Mac clipboard.
- Added validated atomic configuration saves, fixed or client-driven display resolution, and independent controls for swapping left Windows/Alt and mapping Ctrl editing shortcuts. Tag builds publish verified packages directly to GitHub Releases.

- Windows 截图依次选用 PNG、DIBV5、DIB；图片文件下载并通过校验后才写入 Mac 剪贴板。
- 配置保存前校验并原子写入，显示分辨率可固定或跟随客户端。左 Windows／Alt 交换与 Ctrl 编辑快捷键可独立设置。标签构建验证安装包后直接发布到 GitHub Releases。

Thanks / 致谢 · [clintcan/macrdp](https://github.com/clintcan/macrdp) · [IronRDP](https://github.com/Devolutions/IronRDP) · [CLIProxyAPI-Plugins](https://github.com/zhangyxXyz/CLIProxyAPI-Plugins)
