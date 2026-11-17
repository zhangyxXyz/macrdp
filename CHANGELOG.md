# Changelog / 更新记录

Only implemented behavior and completed checks are recorded here. Client-side manual checks are identified separately.
这里只记录已实现的行为和已完成的检查；客户端实测另行注明。

## v0.10.8

- End the current RDP session when ScreenCaptureKit loses its capture source on a locked, sleeping display. The normal disconnect path restores physical displays and releases the virtual display. Other explicit capture stops have up to three seconds to resume with a usable frame. Ordinary locking, an idle picture and a minimized client leave a healthy connection open.
- 锁屏熄屏且 ScreenCaptureKit 确认丢失采集源时，主动结束当前 RDP 会话，沿用断线流程恢复实体屏并释放虚拟屏。其他明确的采集中断最多尝试恢复三秒，以收到可用画面为准。普通锁屏、画面静止和客户端最小化时，正常连接继续保留。

- Adopt `libz-sys 1.1.33` for RUSTSEC-2026-0544 and official stable `rsa 0.10.0` for RUSTSEC-2023-0071. Remove the RSA audit exception and scan the independent server, acceptor, RDPEUDP and IFD dependency trees alongside the main application.
- Backport Picky's EC key coordinate fix [#557](https://github.com/Devolutions/picky-rs/pull/557) and JWE decryption error handling [#563](https://github.com/Devolutions/picky-rs/pull/563). Create private clipboard staging directories atomically to prevent concurrent filename collisions.
- Test the authentication adapters in CI and verify correct and incorrect passwords separately with FreeRDP. The application and release tag stay at `0.10.8` / `v0.10.8` for subsequent rebuilds. Use the packaged `SOURCE_COMMIT` to identify a particular build.
- 升级到 `libz-sys 1.1.33` 和官方稳定 `rsa 0.10.0`，修复对应安全公告并移除 RSA 审计例外。审计覆盖主程序及独立 server、acceptor、RDPEUDP、IFD。
- 回移 Picky 的 EC 坐标修复 #557 和 JWE 解密错误处理 #563。剪贴板私有暂存目录改为原子创建，避免并发撞名。
- CI 增加认证依赖回归，FreeRDP 分别核对正确密码通过和错误密码拒绝。后续重构建沿用应用版本 `0.10.8` 和标签 `v0.10.8`，以安装包的 `SOURCE_COMMIT` 区分。

## v0.10.6-dev

### 2026-11-18 dependency audit / 依赖安全检查

- Follow up with official stable `rsa 0.10.0` and remove the RUSTSEC-2023-0071 ignore. Vendor narrow picky/sspi/winscard compatibility adapters because even their current published versions pin the prerelease; keep IronRDP unchanged. Use OS randomness to blind raw PIV private-key operations and retain stable RSA's 1024-bit public-key minimum. Expand independent dependency audits and distinguish successful/failed FreeRDP authentication per connection.
- 后续采用官方稳定 `rsa 0.10.0`，移除 RUSTSEC-2023-0071 忽略项。上层已发布版本仍精确绑定候选版，因此增加 picky／sspi／winscard 的小范围本地兼容适配，IronRDP 不变。PIV 私钥运算使用系统随机源盲化，保留稳定 RSA 的 1024 位公钥下限；补齐独立依赖审计，FreeRDP 冒烟按连接分别验证正确／错误密码。
- Backport Picky upstream [#557](https://github.com/Devolutions/picky-rs/pull/557) for leading-zero EC key coordinates, caught by the integrated dependency tests, and [#563](https://github.com/Devolutions/picky-rs/pull/563) for uniform RSA JWE decryption failure handling. These library fixes do not establish that a JWE attack was reachable through macrdp.
- 回移 Picky 上游 [#557](https://github.com/Devolutions/picky-rs/pull/557) 的 EC 密钥前导零修复（集成依赖测试发现），以及 [#563](https://github.com/Devolutions/picky-rs/pull/563) 的 RSA JWE 解密失败统一处理。这些依赖库修复不等于已证明 macrdp 存在可达的 JWE 攻击路径。
- Fix a staging-directory collision found by the full macOS suite: replace PID-plus-time naming with atomic private `mkdtemp` creation. Keep the existing owner-PID cleanup policy and add concurrent creation, permission and read/write coverage.
- macOS 全量回归发现文件暂存目录撞名；改用原子私有 `mkdtemp` 创建，保留按所属 PID 清理的规则，并增加并发、权限和读写回归。

- The first fix (`fcaeb63`) updated only `libz-sys 1.1.31 → 1.1.33` for RUSTSEC-2026-0544, which failed the scheduled `security` workflow. Its local root/standalone audits and subsequent security workflow passed under the then-existing policy; native Rust tests: 528 passed, 8 ignored. Application settings and the installed release remain unchanged by these source updates.
- 第一阶段 `fcaeb63` 仅将 `libz-sys 1.1.31 → 1.1.33`，修正定时 `security` 发现的 RUSTSEC-2026-0544；当时策略下的本地根依赖树／独立库审计及后续云端 security 通过，原生 Rust 测试 528 通过、8 跳过。这些源码更新未更改应用设置或已安装版本。

### 2026-11-18 installed-build feedback / 安装版实测反馈

- The user confirmed Directory Opus file copying and normal screen locking/unlocking on the installed build from `3bea2b7`. This supersedes the earlier Opus failure report for `31ee2fb`; it does not establish every file-transfer case, monitor-free/hot-plug behavior or rapid reconnect/resize correctness. Relocking after abandoned pre-capture preparation or capture failure remains unguaranteed.
- 用户确认安装来源为 `3bea2b7` 的版本中，Directory Opus 文件复制及常规锁屏／解锁正常；这更新了此前 `31ee2fb` 的 Opus 失败记录，不外推到全部文件传输、无屏／热插拔或快速重连改分辨率场景。捕获前准备取消或解锁后捕获失败时，仍不能保证重新锁定。

### 2026-11-17 upstream sync and review / 上游同步与复核

- Validation: 528 native release-mode Rust tests passed (8 intentionally ignored), strict all-target Clippy, 55 configuration CLI checks, 13 controller checks, full GUI build, 48 release-script tests, workflow lint, 10 File Provider adapter checks and sanitized native audio-buffer tests passed. FreeRDP loopback CredSSP/audit smoke passed with disposable credentials. Windows paste/playback and hardware disconnect cycles remain separate checks. See the [review record](docs/release-review-2026-11-17.md).
- 验证：528 项原生发布模式 Rust 测试通过（8 项按设计跳过）、全部目标严格 Clippy、55 项配置 CLI、13 项控制器检查、完整 GUI 编译、48 项发布脚本测试、workflow 检查、10 项 File Provider 适配及原生音频缓冲检查通过；临时凭证 FreeRDP 回环认证／审计冒烟通过。Windows 粘贴／播放及硬件断开周期另行验证，详见[审查记录](docs/release-review-2026-11-17.md)。

- Announce local file descriptors and their immutable source paths in the same protocol dispatch. Reject an invalid descriptor list as a whole instead of filtering and shifting file indices; scope queued file requests and lease releases to their original session. This addresses wrong-generation file data and disconnect races; incoming file copies still require a valid standard file stream. Later Opus client verification is recorded above.
- 本地文件描述符与不可变源路径在同一次协议分发中更新；整轮拒绝非法文件列表，避免过滤后索引错位。已排队文件请求与租约释放绑定原会话，防止断线后串入新连接。这修复数据代次和断线竞态，传入文件仍需有效的标准文件流；后续 Opus 客户端实测见上文。
- Always request immediate disconnect locking and confirm both the screen-lock flag and the elapsed password-requirement deadline. An active screen saver alone cannot confirm an authentication-required lock; missing or malformed private state remains unknown.
- 断开锁屏始终发起立即锁定，同时确认锁屏标志与已到期的密码要求时间；屏保活动本身不能证明必须认证，私有状态缺失或格式异常仍视为未知。
- Share the remote-only audio recovery budget across open and read failures. Successful opens alone cannot reset it; valid stereo samples, including silence, can. Stop after five retries and report recovery/error state.
- 仅远端音频的打开／读取失败共用五次恢复预算；仅打开成功不重置，收到有效双声道样本（含静音）后才重置，耗尽后停止并报告错误。
- Give new LaunchAgents a 60-second shutdown allowance and settings-triggered restart commands a separate 75-second limit, followed by 12 seconds of startup verification. A local launchd probe confirmed that kickstart waits for SIGTERM cleanup; the old 8-second settings limit was shorter than the observed 10.5-second monitor restoration.
- 新建 LaunchAgent 允许 60 秒退出清理，设置重启命令单独限时 75 秒，随后再检查新进程启动 12 秒。本机 launchd 探针确认 kickstart 等待 SIGTERM 清理；原设置页 8 秒限时短于已观察到的 10.5 秒显示恢复。

- Merge upstream through `fb1facf` (#262): opt-in TCP H.264 ack-lag pacing, default off, with TCP trailing-flush exemption. Preserve the fork's default-off QoE blank recovery and display/lock/clipboard behavior. Advertise and validate `H264_MAX_UNACKED_FRAMES`, reject duplicate escape-hatch flags, and report requested/effective values. Upstream client results are not a local 4K/200% verification.
- 同步上游至 `fb1facf`（#262）：可选 TCP H.264 帧确认积压控制，默认关闭，TCP 末尾补帧豁免。保留本 fork 默认关闭的 QoE 黑屏恢复及显示／锁屏／剪贴板行为；补齐新配置键的能力声明、校验、重复参数拒绝及请求／有效值报告。上游客户端结果不等同本机 4K／200% 验证。
- Include #268's regression for lossy-audio Soft-Sync after mstsc's late S_OK, and consolidate #230's already-ported startup-frame hold into upstream ancestry.
- 纳入 #268 的 mstsc 迟到 S_OK 后有损音频 Soft-Sync 回归测试；统一已移植 #230 启动帧等待修复的上游合并历史。

### 2026-11-17 rebuild / 同版本重构建

- After `f2d30dc` restored both monitors in live testing, the user observed an unlocked desktop before the delayed lock. Logs measured ~2.5s grace + ~10.5s native restoration + ~0.15s lock. Detach now confirms the lock before restoring physical displays, holds the auto-unlock gate through restoration and avoids a second delayed lock; capture/shield ordering is unchanged. An isolated, authorized live probe locked with only the virtual display active in 102ms. Full reordered-session verification remains separate.
- `f2d30dc` 实测恢复了两台显示器，但用户看到桌面先亮起、随后才锁屏；日志为约 2.5 秒宽限 + 10.5 秒原生恢复 + 0.15 秒锁屏。Detach 改为先确认锁屏再恢复实体屏，恢复期间保持与自动解锁互斥，完成后不再补延迟锁屏；Capture／Shield 顺序不变。获授权的独立实测在仅虚拟屏活动时 102 毫秒内锁定，完整新会话顺序另行验证。

- Follow-up to manual feedback on `31ee2fb`: audio and Windows desktop file copies work, while Directory Opus copies and two-monitor restoration still have failures. Keep saved detach targets even when disabled displays disappear from the online list; attempt every enable and verify each display is active. Recovery restart must preserve disconnect locking and newer-session protection. Actual hardware verification of the correction is pending.
- 根据 `31ee2fb` 的用户实测继续修正：音频和 Windows 桌面文件复制正常，Directory Opus 复制及双屏恢复仍有失败。保留从在线列表消失的已禁用屏幕，逐个尝试启用并确认活动状态；恢复重启须保留断开锁屏及新会话保护。修正版的真实硬件验证待完成。
- Retry a failed remote file descriptor once, like other clipboard formats, instead of dropping it as an unexpected format. A second failure stops with a clear diagnostic; malformed successful responses are not decoded as text. This does not claim to fix a client's persistent inability to supply file data.
- 远端文件描述符失败时与其他格式一样最多重试一次，不再当作意外格式直接丢弃；第二次失败明确记录并停止，损坏的成功回复也不解码成文本。不宣称能修复客户端持续无法提供文件数据的情况。
- Follow-up validation: 506 Rust tests passed (8 ignored), strict Clippy and formatting checks. Tests cover lost dual-display targets, partial restoration, lock-before-restart ordering, retention of the sole display after lock failure, reconnects during lock confirmation and stale clipboard replies. The installed Windows client/hardware checks are separate.
- 后续修正验证：506 项 Rust 测试通过（8 项忽略），严格 Clippy 及格式检查通过。覆盖双屏目标消失、部分恢复、锁屏先于恢复重启、锁屏失败保留唯一显示面、锁屏确认期间重连及过期剪贴板回复；安装后的 Windows 客户端及硬件实测另行确认。

- Add an optional Intel macOS runner fallback when hosted ARM64 capacity is unavailable. Keep native host regression gates and separately check ARM64; explicitly cross-build all app components, verify every Mach-O contains ARM64 and four bundle source markers match, and record the build host in `BUILD_INFO.json`. This does not claim ARM64 execution on Intel. Fix the Linux gate for a macOS-only clipboard test helper.
- 托管 ARM64 容量不足时可选 Intel macOS runner：保留主机原生回归，另查 ARM64，所有应用组件显式交叉编译，逐个核对 Mach-O 的 ARM64 架构及四份 bundle 源提交，并在 `BUILD_INFO.json` 记录构建主机，不把 Intel 检查说成 ARM64 运行验证。补齐剪贴板原生测试的 Linux 平台条件。

- Consolidate regression checks into reusable CI. Release independently packages, stages/downloads/verifies, publishes and prunes serially without waiting for CI or using Actions artifacts; pre-tag review/regressions remain the maintainer workflow. Use one macOS runner for native checks, default to ARM64 `macos-26`, and unify lowercase workflow names. The previous cancelled jobs failed to acquire GitHub hosted runners before executing steps.
- 回归集中在可复用 CI；发布独立串行打包、草稿上传／下载校验、发布和清理，不等待 CI、不使用 Actions 归档；维护者在打 tag 前完成必要审查和回归。原生检查共用一个 macOS runner，默认 ARM64 `macos-26`，统一小写工作流名称。此前被取消的任务是在执行步骤前无法取得 GitHub 托管 runner。
- Same-tag replacement keeps the old owned Release available until the separate staging draft verifies. Validate remote tag/source identity and reject stale runs or concurrent edits; keep a verified draft retryable if final publication fails. Version remains `0.10.6`.
- 同标签覆盖先保留旧 Release，独立暂存草稿验证完成后再替换；核对远端 tag 与来源提交，拒绝过期运行或并发修改。最终发布失败时保留可重试的已验证草稿。应用版本仍为 `0.10.6`。
- Pre-push local validation: 491 Rust tests passed (8 ignored), strict Clippy, 33 configuration CLI checks, 10 controller configuration tests, 10 File Provider adapter checks and sanitized native audio-buffer checks. Actual Windows playback/paste, disconnect lock and monitor-free reconnect remain manual checks.
- 推送前本地验证：491 项 Rust 测试通过（8 项忽略）、严格 Clippy、33 项配置 CLI、10 项控制器配置、10 项 File Provider 适配检查及带内存／未定义行为检查的原生音频缓冲测试。真实 Windows 音频、粘贴、断开锁屏及无屏重连仍需手动验证。

- Handle logged-in sessions without monitors: distinguish no online display from failed restore, use no-op physical guards, filter unplugged restoration targets, keep the virtual surface through disconnect locking, and bootstrap it before headless auto-unlock with cancellation-safe ownership. Shield requires a helper ACK even with zero targets. Real unplug/replug/locked reconnect checks remain pending.
- 覆盖已登录但未插显示器：区分无屏与恢复失败，实体屏空操作、过滤已拔目标、持有虚拟屏至断开锁屏结束，无屏解锁前准备显示面并保护取消时的所有权。Shield 零目标也要求辅助进程确认；真实拔插及无屏锁定重连仍待验证。

- Add an opt-in private Core Audio tap for remote-only playback, capture state/error reporting, actual-rate resampling, and cancellable audio waits. No hardware mute/volume/default-device writes. A local isolated probe captured a nonzero 48 kHz test tone and cleaned up successfully; rebuilt-server/mstsc verification is pending.
- 新增可选的 Core Audio 私有 Tap，仅远端播放、报告捕获状态与错误、按实际采样率转换，并在无回调时仍响应取消。不修改硬件静音、音量或默认设备。独立本机诊断已捕获 48 kHz 非零提示音并成功清理；新版 mstsc 端到端验证待进行。
- Cancel obsolete disconnect locks across reconnect epochs, retain the final disconnect when notifications merge, and serialize final lock requests with auto-unlock. Physical restoration and bounded handshake protection remain; the default extra delay is zero.
- 跨重连代次取消过期锁屏任务，通知合并时仍处理最后断开，最终锁屏与自动解锁串行执行；保留实体屏恢复和有上限的握手保护，默认额外等待为零。

- File Provider publishes writable local clipboard snapshots, persists local edits with version checks and atomic metadata, and never uploads to the Windows source. Ten adapter and eight synthetic OS checks include unlocked/editable exported copies and offline rehydration of local edits. Remove the menu-bar status tooltip that can linger across display changes.
- File Provider 提供可写本地剪贴板副本，按版本及原子元数据保存本地修改，不回写 Windows 源文件。10 项适配层与 8 项合成源系统检查包含导出文件无锁可编辑、本地修改离线再物化；取消可能在显示切换后滞留的菜单栏状态浮条。

- Invalidate stale remote clipboard representations for unsupported formats, including the observed Directory Opus private-format announcement. Publish concrete file-URL items and verify read-back; keep on-demand File Provider hydration. Real Finder paste with the revised binary is pending.
- Add an optional Full Disk Access settings entry for protected Mac source files, explain logical points versus 4K output, remove the extra default 22.5-second disconnect-lock delay, and account for reconnect attempts during native display restoration. Restoration/grace/reconnect holds still apply.
- 新的远端格式声明使旧内容失效，覆盖实测 Directory Opus 私有格式；文件 URL 显式写入并读回核对，保留系统按需物化。修正版的 Finder 实际粘贴仍待验证。
- 增加可选的完全磁盘访问入口，说明逻辑工作区与 4K 输出差异；移除默认额外 22.5 秒断开锁屏等待，计入实体屏恢复期间发生的重连。仍保留恢复、宽限及重连保护。

### Earlier changes / 既有变更

### English

- Bound event batches to 32, preserve async clipboard priority and retain completion permits until dispatch ends. Add regressions for reply priority, budget lifetime, bounded draining and timed-out OS worker retention. This does not eliminate TCP backpressure; real mstsc verification remains pending.
- Include upstream #230 (7a2c5fe): bounded initial legacy-frame hold with static-screen/NV12 handling and legacy fallback. Keep existing blank-recovery and display policies.
- Merge upstream v0.10.6 / 0255a8b (#267): park headless startup on missing/wrong credentials, retry only changed passwords, and surface password correction in the controller. Preserve side-effect-free fork configuration inspection and pre-capture unlock.

- Keep Windows file-copy intent through preparation: clear stale text, prioritize Explorer file streams over previews, refuse filename-as-text fallback and stale replies, and remove automatic Cmd+V after downloads. Preserve screenshot PNG/DIB and system dataless paths.
- Move Mac→Windows clipboard I/O off the RDP loop with bounded workers/queues and a 30s deadline. Serialize format replies (including inline file/drop-effect replies), snapshot original locked paths, and discard retired-session completions. OS reads may outlive timeout but cannot create unbounded workers.
- Create virtual displays only for authenticated capture; release after a 2.5s reactivation grace and physical restoration. Protect reconnects by generation and refresh input/audio/HUD IDs. Keep the only active display if physical restoration fails; fixed 4K/200% and keyboard configuration remain unchanged.
- Repair draft uploads through stable Release/asset IDs, verify exact ownership/source, and restore the real tag on publication. Make build-only packaging manual to avoid duplicate release packaging; no Actions artifacts. This same-version rebuild is published for later manual testing, without installing locally.

- Merge upstream cef6073 (#204), retaining opt-in NV12 capture, bitmap fallback, immutable capture-buffer retention and the fork lock/clipboard/display policies. Add strict CAPTURE_NV12 validation and requested/effective inspection; keep the feature off in existing configs. Upstream performance reports are not local results; merged Windows rendering is not yet manually tested.
- Run opt-in auto-unlock after credential/ARC validation, before display synchronization and capture; wait at most 8s for the requested SCK display after confirmed unlock. Preserve skip-auth gating, serialized real submissions and the shared two-submission budget; report unsafe/budget failures explicitly.
- Request a true immediate lock via runtime-resolved login.framework and independently confirm the lock flag within 2s. No screen-saver fallback or password-delay policy changes. Preserve display restoration, disconnect grace, safety delay and bounded reconnect hold.
- Add regression coverage and a manual build-only Actions workflow that verifies packaged apps without publishing releases or uploading artifacts. Live Windows lock/reconnect validation of this rebuild is pending.
- Rebuild this development release at the user's request without changing version `0.10.6`. Include upstream [#228](https://github.com/clintcan/macrdp/pull/228): Windows 10 mstsc black-screen compatibility for RemoteFX surface commands and H.264 SPS bitstream restrictions, its vendored encoder tests and CI step. Preserve the fork's clipboard maintenance/leases and use the local CLIPRDR patch in standalone server checks. Win10 client rendering still requires a post-update manual check; upstream's separate frozen-start fix #230 is now included (7a2c5fe).
- Add a bundled sandboxed File Provider host/extension for macOS 15+, using system dataless file URLs for flat file copies of any type. Preserve screenshot PNG/DIB handling. Unavailable providers, folders and eager mode download complete files before publication; remove the sparse NSFilePresenter path.
- Stream file contents to private staging, persist immutable metadata and incremental enumeration anchors, pin every SIZE/RANGE to its original clipboard lock, retain concurrent readers across local copies, and isolate cleanup by connection. Reject unknown locks instead of falling back to current clipboard data; drive timeout maintenance in the serving connection.
- Use per-process authenticated loopback IPC and documented domain configuration without App Group entitlements. Local ad-hoc host/extension loading and real dataless reads were verified; Developer ID/notarization remains unverified. Report file clipboard startup readiness, request counters and last helper error through existing stats.
- Bound transfers to 4,096 items/catalog, 128 GiB per file, 64 provider copies, 10-minute non-current idle retention and two-hour maximum source locks. OS-downloaded files work offline; stale unmaterialized sources fail safely.
- Verify the Rust regression suite, nine production adapter checks, and six real system checks with synthetic sources, including same-domain/same-name reconnect. The user reported successful testing of the previously installed `92f4c8c` build before this Win10 merge; this is not a manual check of the rebuilt binary. Release verification requires both embedded File Provider executables. Notes use a short bilingual template linking to this changelog; old owned releases and their tags are pruned only after retained-release verification.

### 简体中文

- 每批事件限制为 32 项，恢复异步剪贴板回复的优先级，直到分发结束才释放并发名额；补齐优先级、预算生命周期、批次上限和超时后读取线程保留测试。TCP 背压仍可能延迟传输，mstsc 实测待完成。
- 纳入上游 #230（7a2c5fe）：首次协商期间有限暂存传统首帧，覆盖静止桌面、NV12 和位图回退；保留已有黑屏恢复与显示策略。
- 合入上游 v0.10.6／0255a8b（#267）：无头启动凭证缺失或错误时等待，仅验证变化后的密码，控制器提供修正入口；保留本 fork 无副作用配置检查及捕获前解锁。

- 保留 Windows 文件复制意图：准备期间清除旧文本，Explorer 文件流优先于预览，拒绝文件名降级成文本及过期回复，取消下载后的自动 Cmd+V；保留截图 PNG／DIB 和系统 dataless 路径。
- Mac→Windows 剪贴板读取移出 RDP 循环，限制后台线程／队列，30 秒请求超时；格式回复（含内联文件列表／drop effect）保持顺序，绑定原锁路径，丢弃旧会话迟到完成。系统阻塞读取可能晚于超时结束，但不会无限增加线程。
- 虚拟屏仅在认证后捕获时创建，2.5 秒重激活宽限及实体屏恢复后释放；按代次保护重连，刷新输入／音频／HUD 的屏幕 ID。实体屏恢复失败时保留唯一活动屏幕；固定 4K／200% 和键盘配置含义不变。
- 修复草稿上传：使用稳定 Release／asset ID，严格核对来源和提交，发布时恢复真实 tag。仅手动运行独立打包验证，避免重复发布构建，不使用 Actions 归档。本次同版本发布供后续手动回归，不在本机安装。

- 合入上游 cef6073（#204），保留主动开启的 NV12 捕获、位图回退和捕获缓冲区保留，同时保留本 fork 锁屏／剪贴板／显示策略。补齐 CAPTURE_NV12 严格校验和请求／有效状态检查；现有配置保持默认关闭。上游性能数据不是本机实测，合并后 Windows 显示待手动验证。
- 主动开启的自动解锁改在凭证／ARC 校验后、显示调整与捕获前执行；确认解锁后最多等待 8 秒 SCK 显示器就绪。保留 skip-auth 限制、串行真实提交和共享两次密码预算，明确报告不安全／预算耗尽错误。
- 运行时解析 login.framework 请求真正立即锁定，并在 2 秒内独立确认锁定标志；不回退屏保、不修改密码延迟策略。保留实体屏恢复、断线宽限、安全等待和有上限的重连保护。
- 补充回归及只验证打包的 Actions，不发布 Release、不上传归档。此次重构建的 Windows 锁屏重连实测待验证。
- 按用户要求保留 `0.10.6` 版本号，重新构建本开发版。包含上游 [#228](https://github.com/clintcan/macrdp/pull/228)：修复 Win10 mstsc 在 RemoteFX 指令及 H.264 参数上的黑屏兼容问题，并纳入独立编码器测试和 CI 检查；保留本 fork 的剪贴板维护和租约，独立 server 检查使用本地 CLIPRDR。更新后的 Win10 实际显示仍需客户端验证；另已纳入上游启动画面冻结修复 #230（7a2c5fe）。
- 打包 macOS 15+ 沙箱 File Provider host／扩展，普通顶层文件不分类型均可使用系统 dataless URL；保留截图 PNG／DIB 路径。扩展不可用、文件夹和主动关闭按需时先完整下载再发布，移除稀疏 NSFilePresenter 占位路径。
- 流式写入私有临时文件，持久化不可变元数据与增量枚举锚点；每个 SIZE／RANGE 携带原始锁，保留本机复制前已有的远端读取者，按连接隔离清理。未知锁不再回退到当前剪贴板，服务循环驱动超时维护。
- 使用每进程鉴权的 loopback IPC 和文档化域配置，不申请 App Group entitlement。已验证本机临时签名 host／扩展加载和真实 dataless 读取；Developer ID／公证分发仍未验证。现有 stats 新增文件剪贴板启动就绪状态、请求计数和最近 helper 错误。
- 限制单批／元数据目录 4,096 项、单文件 128 GiB、provider 64 副本、非当前副本闲置 10 分钟、源锁最长两小时；已下载系统文件可离线读取，旧未物化源安全报错。
- 完成 Rust 回归、9 项应用侧适配层检查及 6 项合成源真实系统检查，覆盖同域同名重连。用户已反馈此前安装的 `92f4c8c` 构建测试正常；这不代表已手动验证此次合入 Win10 修复后的新二进制。发布校验要求两个内嵌 File Provider 可执行文件存在。说明使用简短双语模板链接到本更新记录；验证保留版本后才清理旧自有 Release 及其 tag。

## v0.10.5-dev

### English

- Fix Windows screenshot clipboard selection: try registered PNG, DIBV5, then DIB before the virtual-file representation. Successful images skip the redundant file request.
- For top-level image-only virtual-file copies, download and decode the complete batch before publishing real file URLs and PNG data. Do not expose sparse placeholders to apps that read files directly. Reject corrupt/incomplete images and stale copies; complete short range responses instead of leaving zero-filled tails.
- Bound this image-file path to 16 files, 50 MiB total source and normalized data, 16 megapixels per image, 8192 pixels per side, and a 120-second batch timeout. Other files and mixed/folder copies retain their existing transfer path. Animated formats provide a decoded still image for image paste; the downloaded file retains its original bytes.
- Include earlier fork work: validated/atomic configuration, display policy and measured state, reconnect lifecycle handling, and independent Win/Alt position swapping and Ctrl editing compatibility.
- Package only on `v*` tag pushes. Upload directly to a draft Release, download and verify checksum/source/bundle contents, then publish. Retain the newest verified pipeline-owned Release and preserve historical Git tags. Ordinary dev/PR checks remain.
- Automated regression tests cover image corruption, format fallback, complete file publication, short reads, stale/local clipboard changes and release verification/cleanup. Real Windows screenshot-copy validation of this version is pending user testing. Existing local native 4K/200% display results are documented in the README.

### 简体中文

- 修复 Windows 截图剪贴板格式选择：依次尝试注册的 PNG、DIBV5、DIB，再回退到虚拟文件；图片成功后跳过多余的文件请求。
- 顶层纯图片文件批次先完整下载、解码校验，再发布真实文件 URL 和 PNG 数据，避免直接读文件的应用拿到稀疏占位内容。损坏、不完整或已过期的复制不发布；文件分块短读继续补齐，避免留下全零尾部。
- 该图片文件路径限制为最多 16 个文件、源数据和标准化数据各合计 50 MiB、单图 1600 万级像素（16×1024×1024）、单边 8192 像素、整批 120 秒超时。普通文件、混合批次和文件夹保持现有传输路径。动图的图片粘贴提供解码后的静态画面，下载文件保留原始字节。
- 包含此前 fork 开发：配置校验与原子保存、显示策略与真实状态、重连生命周期处理，以及独立的 Win／Alt 位置交换和 Ctrl 编辑兼容设置。
- 仅推送 `v*` tag 时打包；直接上传草稿 Release，回读核对校验和、源提交和包内容后发布。默认仅保留最新已验证、由本流水线创建的 Release，历史 Git tags 保留；dev／PR 的日常检查继续执行。
- 自动回归覆盖图片损坏、格式回退、完整文件发布、短读、过期／本机剪贴板变化，以及发布校验和清理。本版 Windows 截图复制仍待用户实测；此前本机原生 4K／200% 的验证记录见 README。

Thanks / 致谢：感谢 [clintcan/macrdp](https://github.com/clintcan/macrdp) 与 IronRDP 的贡献者；发布清理编排参考 [CLIProxyAPI-Plugins](https://github.com/zhangyxXyz/CLIProxyAPI-Plugins)。
