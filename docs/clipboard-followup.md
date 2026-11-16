# Clipboard follow-up / 剪贴板后续调查

Reviewed 2026-11-17. This documents source-level findings, not a claim that the
user's Windows freeze has been reproduced or resolved on a real client.

## Confirmed paths and changes

- Earlier outgoing callbacks performed file metadata/read and pasteboard/image
  conversion synchronously inside CLIPRDR dispatch. They now use four blocking
  workers, a 32-operation budget and a 30-second queue/read deadline. A timed-out
  syscall retains its worker slot until it returns; its late result is discarded.
- File enumeration was reachable from format-list callbacks; those callbacks now
  request a later advertise. Directory traversal no longer holds the pasteboard
  mutex. Enumeration itself still uses synchronous filesystem calls on the poller;
  this is not a guarantee about unresponsive network mounts or AppKit IPC.
- Async completions introduced a new `SessionClipboard` event. It must have the
  same priority as `Clipboard`, otherwise a file reply can wait behind video.
  Stable sorting now recognizes both. Their relative order remains unchanged.
- Completion normalization previously released its queue permit before writing.
  Dispatch now retains it until that event finishes, including a blocked write.
- The event consumer previously drained `try_recv` without a limit after receiving
  its initial batch. Continuous producers could grow one lock-holding batch.
  Each acquisition now dispatches at most 32 events, leaving the rest queued.
- Original lock snapshots and stream IDs remain essential. File replies have IDs;
  format replies do not, so asynchronous format requests must stay serialized.
  See Microsoft's [request handling](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeclip/d19de33e-4027-43de-b150-668ee21bced8)
  and [file response fields](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeclip/df87c178-ab02-471a-acde-bb921aa1af85).

The existing connection integration test drives real IronRDP negotiation and
checks input while a socket write is blocked. New unit tests cover batch bounds,
priority/order, completion budget lifetime, and worker retention after timeout.
Neither is an end-to-end Explorer copy test. Historical writer-stall log entries
prove that backpressure has occurred, but do not identify clipboard as its cause.
A File Provider hydration loop is an additional risk, not an established cause
of the user's reported freeze.

Remaining limits: one large TCP write can still occupy the writer; bounded event
count is not a time budget. Inbound X224 work has a bounded queue and shares server
state, so sufficiently sustained backpressure can still delay input. The change
does not split or truncate wire messages, drop file bytes, or force a reconnect.
Copying again while a previous file list is being advertised and unresponsive
filesystem/pasteboard providers also need real-client coverage.

## Upstream review

`main` remains exactly upstream `0255a8b` (v0.10.6). No main commit is missing
from dev at this review. Open PRs are assessed separately from merged upstream:

| PR | Decision / 决定 |
| --- | --- |
| [#230](https://github.com/clintcan/macrdp/pull/230), `7a2c5fe` | Included with original authorship. Initial EGFX/legacy race fix; bounded 3-second fallback, static-screen and NV12 handling. Upstream reports Win10 repeated-connect verification; local Windows verification pending. 合入首帧冻结修复，保留作者信息，实机待测。 |
| [#231](https://github.com/clintcan/macrdp/pull/231) | Defer: conflicts and HiDPI recreation concerns; overlaps the fork's session display lifetime. 暂缓虚拟屏重建改动，避免破坏 4K/200%。 |
| [#232](https://github.com/clintcan/macrdp/pull/232) | Defer: depends on display lifecycle work and needs wake/allow-sleep validation. 暂缓唤醒行为改动。 |
| [#262](https://github.com/clintcan/macrdp/pull/262) | Optional TCP video pacing; not a demonstrated clipboard fix. Do not enable as a workaround. 可选视频节流，不作为剪贴板修复。 |
| [#235](https://github.com/clintcan/macrdp/pull/235) | Conflicting bandwidth/quality negotiation; requires rebase and separate validation. 暂缓带宽与质量协商改动。 |

## Manual verification after the user requests installation

Keep the current 4K/200% and keyboard configuration. Test small/large local files,
folders, text and screenshots in both directions. During a large transfer, move
windows and type; check both transferred file hashes and interactive progress.
Repeat with a new copy during a transfer, cancel, disconnect and reconnect. Check
that an old completion cannot publish into the new session. For virtual displays,
check that physical displays restore before the last remote display retires.
No installation, restart, TCC change or system File Provider registration is part
of these automatic checks.

中文结论：已确认并修复同步 I/O、异步回复优先级、并发名额提前释放与无界批次等代码问题。
尚未在用户 mstsc 上复现并定位唯一根因，不能把“自动测试通过”写成“实机卡死彻底修复”。
发布后等待用户要求再安装，随后按以上步骤验收；现有 app 和配置保持不变。
