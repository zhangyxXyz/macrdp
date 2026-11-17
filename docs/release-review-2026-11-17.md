# 2026-11-17 release review / 发布复核

Scope: upstream `fb1facf`, the `dev` fork's configuration, display/lock lifecycle,
clipboard/File Provider, input, audio and release packaging. App version stays
`0.10.6`; `SOURCE_COMMIT` identifies this `v0.10.6-dev` rebuild.

范围：上游 `fb1facf`，以及 dev 的配置、显示／锁屏、剪贴板／File Provider、输入、
音频与发布打包。版本保持 `0.10.6`，通过 `SOURCE_COMMIT` 区分同标签构建。

## Findings and changes / 发现与修正

| Area / 范围 | Evidence and resulting behavior / 依据与结果 |
| --- | --- |
| Mac → Windows file identity | The OS poller could install paths B before the protocol dispatched descriptors B, allowing a lock on descriptors A to retain paths B. Descriptors and an immutable path snapshot now move in one dispatch. Repeated live lock IDs retain both original halves; IDs can be reused after unlock. / 原后台轮询可先换 B 路径、后公告 B 描述符，导致 A 锁读 B 文件；现在同次更新，重复有效锁保留原快照，解锁后才可复用。 |
| File indices | Filtering invalid descriptors renumbered the wire list without renumbering paths. Reject the whole invalid list and clear the current file announcement. Count UTF-16 units, including surrogate pairs, against the 260-unit NUL-terminated wire field. / 不再过滤非法项后错位，整轮拒绝；按 UTF-16 单元计算名称上限。 |
| Disconnect isolation | A request/lease could pass an enqueue-time alive check, then be consumed after disconnect. SIZE/RANGE and lease release carry their original backend's session marker and are checked again at dispatch. / 文件请求及租约释放绑定原会话，在实际分发时再次检查。 |
| Clipboard ordering | A queued descriptor request now retains its announcement generation. A stale request receives FIFO failure instead of the next copy's descriptors. Slow directory enumeration rechecks pasteboard change count and remote-owned writes before announcing its result. / 排队描述请求绑定公告代次，过期按原顺序失败；慢目录枚举结束后复检剪贴板变化和远端写入。 |
| Lock confirmation | A screen-saver flag can be true during the no-password grace period. Always request immediate locking, then check the flag and the elapsed authentication deadline. Missing/malformed private values do not confirm a secure lock. Auto-unlock keeps its separate screen-visibility predicate. / 屏保标志不等于已要求密码；立即锁定后同时检查标志及密码要求时间，未知状态不报成功，自动解锁保留独立判定。 |
| Audio recovery | Successful tap opens followed by immediate read errors previously bypassed the retry cap. Open/read errors now share five retries; only valid stereo samples reset the budget. / 打开成功但读取失败原可无限重开，现共用五次预算，收到有效样本才重置。 |
| Settings restart | A disposable launchd probe received SIGTERM and completed cleanup before `kickstart -k` returned (2.129s). The controller's 8s limit was below the already observed 10.5s display restoration. Restart now has 75s, followed by a separate 12s startup check; new agents have `ExitTimeOut=60`. / 临时服务证明 kickstart 等待清理，原 8 秒会误判；新重启限时独立于启动验证，新增退出清理上限。 |

The launchd probe registered only a disposable test job and removed it after the
check. It did not restart the installed server. Existing LaunchAgents acquire the
explicit exit allowance when reinstalled; this deployment also updates that field
while preserving their other values.

launchd 探针仅注册、移除临时任务，没有重启现用服务。旧 LaunchAgent 重新安装后才取得
显式退出上限；本次部署会保留其他字段，仅补此设置。

## Primary references / 一手依据

- [MS-RDPECLIP File Contents Request processing](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeclip/d19de33e-4027-43de-b150-668ee21bced8): `clipDataId` selects the locked clipboard data and `lindex` selects an entry in that descriptor list.
- [MS-RDPECLIP Lock processing](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeclip/5bd7dfac-e468-4c01-bc42-e034bb4fa51a) and [File Descriptor wire format](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeclip/a765d784-2b39-4b88-9faa-88f8666f9c35).
- [Codex's macOS lock-state implementation](https://github.com/openai/codex/blob/main/codex-rs/tui/src/session_lock/macos.rs): screen flag plus Unix-second password-requirement deadline. The local OS field types were also checked without reading other session fields.
- [Chromium immediate lock](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/base/mac/login_util.cc) and [Hammerspoon caffeinate](https://github.com/Hammerspoon/hammerspoon/blob/master/extensions/caffeinate/libcaffeinate.m): runtime lookup of login.framework. These private APIs are not an Apple compatibility guarantee.
- [Apple active displays](https://developer.apple.com/documentation/coregraphics/cggetactivedisplaylist(_:_:_:)) and [online displays](https://developer.apple.com/documentation/coregraphics/cggetonlinedisplaylist(_:_:_:)): mirrored/sleeping/disabled states cannot be treated as equivalent to cable removal. Saved detach targets remain authoritative for restoration attempts.
- [Apple tap mute behavior](https://developer.apple.com/documentation/coreaudio/catapmutebehavior) and [tap lifecycle example](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps): capture-scoped local suppression, without rewriting hardware volume or mute.
- [Apple launchd lifecycle](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html), the installed `launchctl`/`launchd.plist` manuals, and the disposable local probe above. No fixed system-default termination grace is assumed.
- Upstream [#262](https://github.com/clintcan/macrdp/pull/262) is optional TCP frame-ack pacing, default off; [#268](https://github.com/clintcan/macrdp/pull/268) is a late-S_OK lossy-audio regression. Upstream client measurements are not fork 4K measurements.

## Completed checks / 已完成检查

- Strict all-target Clippy and formatting passed; 55 process-level configuration checks passed. FreeRDP loopback CredSSP/audit integration passed with disposable credentials, explicitly using the freshly built debug executable rather than a stale release binary. No real account password or GUI session was used. / 严格 Clippy、格式及 55 项配置进程检查通过；FreeRDP 回环认证审计明确使用新编译的 debug 文件，使用临时凭证，不涉及真实账户密码或桌面会话。
- Native macOS release-mode Rust suite: **528 passed, 8 ignored, 0 failed**. Ignored tests need real audio/display/domain state or are benchmarks; the result is not a full Windows-client test. / macOS 发布模式 Rust 全量 **528 通过、8 跳过、0 失败**，跳过的是实机音频／显示／域及性能测试。
- Swift controller configuration: 13 checks, including real subprocess cleanup exceeding 8 seconds and bounded output without a pipe deadlock; full release GUI build passed. / 13 项配置检查及完整 GUI 发布模式编译通过。
- Release maintenance scripts: 48 mocked tests; `actionlint` and shell syntax checks passed. Mocked publication/deletion messages do not represent a live release operation. / 48 项模拟发布脚本检查、workflow 及 shell 语法检查通过。
- Clipboard: 99 targeted checks; file paths/transfers: 44 passed and one real-domain check intentionally ignored; vendored server completion dispatch: 5 checks. / 剪贴板 99 项、文件相关 44 项及服务分发 5 项通过，真实域检查未运行。
- File Provider production adapter: 10 checks with synthetic sources and no system domain registration. / 生产适配层 10 项通过，无系统域注册。
- Native audio ring: AddressSanitizer and UndefinedBehaviorSanitizer checks passed for interleaved/planar buffers, partial reads, empty queues, contention, overflow and invalid layouts; no HAL tap was opened by this check. / 原生缓冲内存与未定义行为检查通过，未打开真实音频 Tap。

## Verification boundaries / 验证边界

- Updated 2026-11-18: the user confirmed Directory Opus file copying and normal screen locking/unlocking on installed source `3bea2b7`. The earlier Opus failure applied to `31ee2fb`; it is no longer the current result for that tested path. This is user-reported client verification, not a complete file-transfer or display test matrix. Missing/failed standard file streams must still fail visibly rather than paste stale data. / 2026-11-18 更新：用户确认安装来源 `3bea2b7` 的 Directory Opus 文件复制、常规锁屏／解锁正常。此前 Opus 失败对应 `31ee2fb`，不再代表此次已测路径的当前结果；这是用户客户端反馈，不是完整文件传输或显示场景矩阵，标准文件流缺失／失败仍须明确报错，不能粘贴旧内容。
- Protocol and File Provider adapter tests do not prove actual mstsc/Finder behavior. The existing system dataless tests used synthetic sources; signing, TCC and current service readiness are checked separately during installation. / 协议及适配测试不等于真实客户端；安装另查签名、权限与运行状态。
- The slow-enumeration guard drops changes observed before enqueue; an OS clipboard change after that check can still race dispatch. Reading NSPasteboard inside the RDP dispatch loop would reintroduce blocking. Closing this remaining ownership window requires a separate publication-ticket design. / 慢枚举复检只覆盖入队前已观察到的变化，之后到分发间仍有所有权竞争窗口；不能在协议循环读系统剪贴板来阻塞主路径。
- Normal locking/unlocking now has the `3bea2b7` user confirmation above. Detailed dual-monitor transition timing, a Mac with all physical displays unplugged, hot-plug, and rapid reconnects with resolution changes still require hardware/client coverage. Creation/release/overlay transitions are serialized; this does not assert that every resize/reanchor transaction shares that gate. / 常规锁屏／解锁已有上述 `3bea2b7` 用户确认；双屏切换的完整时序、无屏、热插拔及快速重连改分辨率仍需实机覆盖，不宣称全部显示事务共用同一互斥。
- If a connection is cancelled during pre-capture auto-unlock, or capture fails after unlocking, relocking is not currently guaranteed: the capture session may never enter the counted lifecycle. A delayed unconditional lock would risk locking a newer session. / 捕获前解锁时取消或解锁后捕获失败，目前不能保证重锁；不能用无代次保护的延时锁补救。
- Audio route switches, reconnects, crashes and long silence require listening tests. The 250ms read-check interval is not a strict timeout on synchronous HAL open/close. / 音频设备切换等仍需听测，异步读取轮询间隔不是 HAL 同步调用的硬超时。
- No FileVault or pre-login remote desktop support is added. / 不新增 FileVault 或首次登录前远程能力。

## 2026-11-18 security workflow follow-up / 安全工作流跟进

[Security run 44130778125](https://github.com/zhangyxXyz/macrdp/actions/runs/44130778125)
failed its dependency audit because the lockfile selected `libz-sys 1.1.31`, which
matches [RUSTSEC-2026-0544](https://rustsec.org/advisories/RUSTSEC-2026-0544.html)
(CVE-2026-85091). The advisory describes a heap overflow in zlib 1.3.1.2–1.3.2
when a stalled non-blocking `gzwrite()` is followed by `gzprintf()`/`gzvprintf()`;
it lists `libz-sys >= 1.1.33` as patched. The lockfile now selects `1.1.33` without
changing other dependency versions or adding an advisory exception. Local
`cargo-deny 0.19.9` checks of the root and standalone RDPEUDP trees passed under
the existing policy; the native Rust suite passed **528 tests, with 8 ignored**.
These checks do not publish a release or replace the installed `3bea2b7` build.

本次 [security 运行 44130778125](https://github.com/zhangyxXyz/macrdp/actions/runs/44130778125)
因锁文件中的 `libz-sys 1.1.31` 命中
[RUSTSEC-2026-0544](https://rustsec.org/advisories/RUSTSEC-2026-0544.html)
（CVE-2026-85091）而未通过依赖审计。公告描述 zlib 1.3.1.2–1.3.2 中，非阻塞
`gzwrite()` 写入停滞后调用 `gzprintf()`／`gzvprintf()` 可导致堆溢出；修复版本为
`libz-sys >= 1.1.33`。锁文件已更新为 `1.1.33`，未变更其他依赖版本，也未新增漏洞
忽略项。本地 `cargo-deny 0.19.9` 在现有策略下对根依赖树及独立 RDPEUDP 库的检查均
通过；原生 Rust 测试 **528 通过、8 跳过**。这些检查不会发布或替换本机 `3bea2b7`。

The installed macOS build was checked separately: it uses system zlib 1.2.12,
and no `gzwrite`/`gzprintf` calls were found in that check. Those observations do
not match this advisory's affected zlib versions and call path; they do not make
the lockfile audit pass or establish that the application or system zlib is free
of other vulnerabilities. Other build targets must not inherit this local conclusion.

本机已安装版本另行核对为使用系统 zlib 1.2.12，检查中未发现 `gzwrite`／`gzprintf`
调用。这两项证据不符合本次公告列出的受影响版本及调用路径，但不会令锁文件审计自动
通过，也不代表应用或系统 zlib 没有其他漏洞；其他构建目标不能直接套用本机结论。

The existing RUSTSEC-2023-0071 exception remains separate: RustSec now lists
stable `rsa >= 0.10.0` as patched, while the current picky/sspi/winscard versions
pin `=0.10.0-rc.18`. The outdated "no fix exists" justification in `deny.toml`
was corrected. Removing this exception needs a coordinated authentication-stack
upgrade and regression checks; a passing policy scan is not a claim of zero
advisories. A diagnostic scan with only this exception removed failed on
RUSTSEC-2023-0071, confirming the retained exception is active in the root tree.
No reachable rsa-crate private-key decryption was identified in the
inspected CredSSP/NTLM server path; other authentication/smart-card paths are not
proven safe by that observation.

现有 RUSTSEC-2023-0071 例外仍单独保留：RustSec 已列出稳定版 `rsa >= 0.10.0`
为修复版本，而当前 picky／sspi／winscard 精确依赖 `=0.10.0-rc.18`。
已修正 `deny.toml` 里“没有修复版”的过期说明；移除此例外需要协调认证依赖升级及回归，
策略检查通过不等于零漏洞公告。仅临时移除此例外的诊断扫描确实因 RUSTSEC-2023-0071
失败，确认根依赖树仍依赖此例外。已检查的 CredSSP／NTLM 服务端路径未发现可达的
rsa crate 私钥解密调用，这不构成其他认证／智能卡路径安全的证明。
