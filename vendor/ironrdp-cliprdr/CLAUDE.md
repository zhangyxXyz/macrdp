# Local CLIPRDR divergence / 本地剪贴板协议改动

Based on IronRDP commit `e258f6a06b1fecfc3829a56928043bced1c8ba66`.
Standalone manifest pins sibling crates to the same revision. Upstream MIT and
Apache licenses are retained. This fork is patched from both crates.io and the
git workspace to ensure a single shared CLIPRDR type.

- Correlate each pending file-list response with the lock from its original
  request, rather than the mutable current clipboard lock.
- Optional `retain_remote_file_lists` backend hook installs reference-counted
  leases before the callback can race a local file-copy event. Default is false.
- `retain_remote_lock` / `release_remote_lock` preserve explicitly owned remote
  readers across local file copies. Unleased locks retain their release behavior.
- Reject outgoing unknown explicit locks and incoming unknown locks without
  falling back to current clipboard file metadata/data.
- Existing maintenance is driven by the vendored server every five seconds;
  two-hour maximum lock lifetime also applies to retained/current locks.

Root `src/clipboard_protocol_tests.rs` exercises original-lock attribution,
size validation, delayed responses, timeout delivery, unknown locks, local-copy
lease retention, independent consumers, and hard lifetime bounds.

中文：只改动原锁归属、显式租约、未知锁拒绝及超时边界；不自建 RDP 协议解析器。
回归在根项目运行，正常构建不启用 `__test` 可见性接口。升级 IronRDP pin 时需重新核对
这些差异及服务端的 `ClipboardLease` 事件与维护 timer。

- Serialize incoming FormatDataRequest callbacks and inline file/drop-effect
  responses until the asynchronous backend completes. Responses carry no ID;
  queue up to MAX_PENDING_FORMAT_DATA_REQUESTS and reject overflow. RANGE/SIZE
  remain concurrent because those responses carry stream IDs.
  中文：异步读取期间按请求顺序发送无 ID 的格式回复，文件分块仍可并发。

- File advertisements may carry an immutable `ClipboardFileSnapshot`; install it
  on the backend only in the same protocol dispatch that installs its descriptor
  array. Keep the original no-snapshot API. Clear the current snapshot on a
  non-file copy or rejected FormatList; prior clipDataId snapshots remain owned
  by their locks. Invalid descriptors reject the whole copy (empty FormatList),
  preserving lindex alignment. The 259-character wire limit counts UTF-16 units.
  中文：文件描述符与原路径作为同一公告提交，后台枚举不改当前锁快照；非法名称整轮
  拒绝，避免过滤单项后发送其他文件内容。测试覆盖排队新公告期间锁住旧列表与代理对。
- Duplicate live Lock IDs keep their original descriptor/path mapping; only
  Unlock permits reuse. Queued FormatDataRequest items carry the local copy
  generation and return FAIL in FIFO order if it changed, instead of returning
  a newer descriptor array under an older clipboard lock.
  中文：重复锁不覆盖；旧代次排队请求按顺序失败，避免 B 内容配 A 锁。
