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
