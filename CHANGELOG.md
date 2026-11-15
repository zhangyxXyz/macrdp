# Changelog / 更新记录

Only implemented behavior and completed checks are recorded here. Client-side manual checks are identified separately.
这里只记录已实现的行为和已完成的检查；客户端实测另行注明。

## Unreleased — locked-session lifecycle / 锁屏会话生命周期

### English

- Run opt-in auto-unlock after credential/ARC validation, before display synchronization and capture; wait at most 8s for the requested SCK display after confirmed unlock. Preserve skip-auth gating, serialized real submissions and the shared two-submission budget; report unsafe/budget failures explicitly.
- Request a true immediate lock via runtime-resolved login.framework and independently confirm the lock flag within 2s. No screen-saver fallback or password-delay policy changes. Preserve display restoration, disconnect grace, safety delay and bounded reconnect hold.
- Add regression coverage and a build-only Actions workflow that verifies packaged apps without publishing releases or uploading artifacts. Local installation and live Windows lock/reconnect validation are deferred.

### 简体中文

- 主动开启的自动解锁改在凭证／ARC 校验后、显示调整与捕获前执行；确认解锁后最多等待 8 秒 SCK 显示器就绪。保留 skip-auth 限制、串行真实提交和共享两次密码预算，明确报告不安全／预算耗尽错误。
- 运行时解析 login.framework 请求真正立即锁定，并在 2 秒内独立确认锁定标志；不回退屏保、不修改密码延迟策略。保留实体屏恢复、断线宽限、安全等待和有上限的重连保护。
- 补充回归及只验证打包的 Actions，不发布 Release、不上传归档。本机安装与 Windows 锁屏重连实测留待后续。

## v0.10.6-dev

### English

- Rebuild this development release at the user's request without changing version `0.10.6`. Include upstream [#228](https://github.com/clintcan/macrdp/pull/228): Windows 10 mstsc black-screen compatibility for RemoteFX surface commands and H.264 SPS bitstream restrictions, its vendored encoder tests and CI step. Preserve the fork's clipboard maintenance/leases and use the local CLIPRDR patch in standalone server checks. Win10 client rendering still requires a post-update manual check; upstream's separate frozen-start issue #230 is not included.
- Add a bundled sandboxed File Provider host/extension for macOS 15+, using system dataless file URLs for flat file copies of any type. Preserve screenshot PNG/DIB handling. Unavailable providers, folders and eager mode download complete files before publication; remove the sparse NSFilePresenter path.
- Stream file contents to private staging, persist immutable metadata and incremental enumeration anchors, pin every SIZE/RANGE to its original clipboard lock, retain concurrent readers across local copies, and isolate cleanup by connection. Reject unknown locks instead of falling back to current clipboard data; drive timeout maintenance in the serving connection.
- Use per-process authenticated loopback IPC and documented domain configuration without App Group entitlements. Local ad-hoc host/extension loading and real dataless reads were verified; Developer ID/notarization remains unverified. Report file clipboard startup readiness, request counters and last helper error through existing stats.
- Bound transfers to 4,096 items/catalog, 128 GiB per file, 64 provider copies, 10-minute non-current idle retention and two-hour maximum source locks. OS-downloaded files work offline; stale unmaterialized sources fail safely.
- Verify the Rust regression suite, nine production adapter checks, and six real system checks with synthetic sources, including same-domain/same-name reconnect. The user reported successful testing of the previously installed `92f4c8c` build before this Win10 merge; this is not a manual check of the rebuilt binary. Release verification requires both embedded File Provider executables. Notes use a short bilingual template linking to this changelog; old owned releases and their tags are pruned only after retained-release verification.

### 简体中文

- 按用户要求保留 `0.10.6` 版本号，重新构建本开发版。包含上游 [#228](https://github.com/clintcan/macrdp/pull/228)：修复 Win10 mstsc 在 RemoteFX 指令及 H.264 参数上的黑屏兼容问题，并纳入独立编码器测试和 CI 检查；保留本 fork 的剪贴板维护和租约，独立 server 检查使用本地 CLIPRDR。更新后的 Win10 实际显示仍需客户端验证；上游独立的启动画面冻结问题 #230 尚未包含。
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
