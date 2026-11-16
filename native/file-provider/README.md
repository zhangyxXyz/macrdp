# macrdp File Provider

English | 简体中文见下文

`src/file_provider.rs` exposes only the remote file contents already authorized by
an RDP clipboard copy. A random per-process token authenticates a loopback-only
HTTP broker. The bundled `macrdpFiles.app` host supplies the transient endpoint
through macOS 15+ `NSFileProviderDomain.userInfo`; it never stores a Mac/RDP login
password. The separately sandboxed `.appex` requests immutable session/copy/lock
ranges. Neither bundle requests App Group or device/screen privileges.

`Core.swift` validates names, sizes and reply identity, streams serial 128 KiB
reads, and atomically persists an additive metadata journal with monotonic
anchors. A copy directory prevents collisions between old/new same-name files.
Only complete private files are returned through the manager's same-volume
staging directory. OS File Provider owns completed-content caching and read
coordination. Cancellation removes partial files. The journal is bounded to
4,096 items / 4 MiB; source files are bounded to 128 GiB, with no full-file RAM
buffer. OS/container metadata may remain after removing a domain.

The provider broker retains at most 64 copies / 4,096 files. Non-current idle
copies expire after ten minutes; all source locks expire after two hours.
Pinning uses reference-counted protocol leases established before callbacks can
enqueue work, and never authorizes an old session to read current-session data.
Normal shutdown removes its own UUID-scoped domain. Stale-domain reaping checks
schema/owner PID and removes only a definitely dead owner; `EPERM` is treated as
alive. Other running instances/user providers are not removed.

`--no-lazy-paste`, older macOS, folders, unavailable/disabled providers and rejected
provider metadata use complete-file fallback. All file downloads require clipboard
locking. No fallback publishes pre-sized holes. The existing bounded image fallback
also supplies native PNG data. A general flat-file provider copy supplies file
URLs; bitmap-only screenshots continue through the PNG/DIB clipboard codecs.

Build/sign without registration:

```sh
python3 packaging/make-file-provider.py --destination /absolute/separate/build
python3 scripts/check-file-provider.py --build-root /absolute/separate/checks
```

Real OS checks explicitly register/remove a domain and need the file provider
enabled in System Settings. This command uses synthetic data, not mstsc:

```sh
python3 scripts/check-file-provider-system.py \
  --host-app /absolute/separate/build/macrdpFiles.app \
  --build-root /absolute/separate/system-checks --allow-system-registration
```

Measured: nine adapter checks, including >16 MiB streaming, short reads, cat/mmap,
invalid credentials, journal restoration, duplicate names, reply tampering,
disconnect and cancellation. Six real OS checks cover zero-range dataless metadata,
direct POSIX hydration, offline cached reads, failed disconnected reads,
same-domain/same-name reconnect and stale URL rejection. These do not prove an
actual mstsc transfer; Windows-side screenshot and reconnect validation is pending.
Ad-hoc loading on this Mac was tested, not notarized distribution.

## 简体中文

这是当前应用的文件传输后端，不再是独立测试端点。macOS 15+ 的 host 和扩展打包在
`macrdp.app/Contents/Helpers`，扩展启用后提供真正的 dataless 文件；没有单独的 RDP
测试端口。Windows 原有截图 PNG／DIB 编解码继续保留。

本地 broker 仅监听 loopback，每个进程生成随机令牌，通过域配置交给沙箱扩展，不保存
Mac／RDP 登录密码，不依赖 App Group、录屏或设备控制 entitlement。所有分块携带原会话、
副本和锁；协议层在回调前建立引用计数租约，防止本机复制或其他读取者释放正在使用的锁。

扩展串行流式写入，整文件完成后才交给系统，不将全文件放入 RAM。增量元数据锚点持久化，
同名的新副本位于不同目录。系统负责已完成内容的缓存和读取协调；取消会清理部分文件。
元数据最多 4,096 项／4 MiB，单文件 128 GiB。broker 最多 64 个副本／4,096 个文件，
非当前副本闲置十分钟回收，源锁最长两小时。已下载内容离线可读，旧未下载源安全报错。

正常退出只移除本进程 UUID 域；启动仅回收 schema 符合且确认所属进程已死亡的旧域，
权限不足视为进程仍活着。系统可能保留 app／容器元数据。文件夹、旧 macOS、扩展未启用、
不支持的批次和 `--no-lazy-paste` 回退为完整下载后发布，不再发布稀疏占位文件。文件传输
要求协商剪贴板锁。纯图片回退仍在原安全边界内提供 PNG，普通 provider 文件提供文件 URL。

构建／检查命令见上文。普通检查不注册；带 `--allow-system-registration` 的系统检查会
注册并清理自己的测试域。已测 9 项适配层及 6 项真实 OS 行为（合成源），包括同域同名重连。
不能等同 Windows mstsc 端到端已经通过；截图复制与真实重连仍待用户验证。临时签名本机
加载已验证，Developer ID／公证分发未验证。

Thanks to [macrdp](https://github.com/clintcan/macrdp) and
[IronRDP](https://github.com/Devolutions/IronRDP) contributors.

## Outgoing reads and paste safety / 回传与粘贴安全

Mac→Windows reads run outside the RDP loop, so reading an unmaterialized provider
file can request its original Windows bytes without a network-loop deadlock.
Four workers, 32 operations and a 30s queue/read timeout bound resource use.
Timeout does not cancel an OS read; its worker stays occupied until return.
Late responses cannot cross connections. Incoming file preparation clears stale
text; eager readiness only plays a sound, never sends Cmd+V.

Mac 回传 Windows 的读取放到 RDP 循环之外，未物化文件可以请求原 Windows 数据，
避免同一网络循环互等。限制 4 线程／32 操作／含排队 30 秒；系统读取不能强制取消，
超时后名额仍保留到读取返回。迟到回复不跨会话。文件准备期间清除旧文本，
完整下载就绪只播放提示音，不再自动发送 Cmd+V。此次修改仍待真实 mstsc 回归。
