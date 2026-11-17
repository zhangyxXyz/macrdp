//! Windows→Mac file clipboard preparation and NSPasteboard publication.
//! Supported flat file copies use system File Provider dataless URLs; actual
//! reads fetch content on demand. Unsupported batches/provider failures use
//! complete downloads in a private temp directory before publishing URLs.
//! Finder paste consumes file URLs, not the drag-only NSFilePromiseReceiver.
//! File items carry concrete URL strings, with read-back verification before
//! success is reported. Never substitute a plain-text pathname for a file.
//!
//! Work runs on the captured tokio runtime. Every pasteboard access is serialized
//! with clipboard::pasteboard_guard; copy tickets protect newer local/remote
//! contents from late completions. Complete fallback files are cleaned on the
//! next remote copy/disconnect, with dead-process staging reaped at startup.

#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_cliprdr::pdu::{FileContentsFlags, FileContentsRequest, FileContentsResponse};
use ironrdp_server::ServerEvent;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSString, NSURL};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

/// Per-RANGE request size for the eager path. 1 MiB matches what mstsc
/// itself asks for when fetching files from us in the Mac→Windows
/// direction. Larger chunks (we tried 4 MiB) appeared to flood the SVC
/// channel and starve the display/audio path — the connection felt
/// unresponsive while a big download ran.
pub const EAGER_CHUNK_SIZE: u32 = 1024 * 1024;

/// Per-RANGE request size for the lazy path. 128 KiB combined with
/// `LAZY_PARALLEL_CHUNKS = 1` (serial fetch) keeps peak inbound
/// in-flight at 128 KiB. We tried bumping back to 256 KiB / 2 (peak
/// 512 KiB) after the `dispatch_audio` carve-out — paste was faster
/// (~73 s vs ~89 s for 500 MB) but TCP send-buffer pressure at that
/// in-flight level introduced a ~10 s audio/video stall near the end
/// of large pastes. The carve-out fixed the per-connection `Mutex<Self>`
/// contention, but at higher inbound bandwidth the bottleneck moved
/// down to the shared `SharedWriter` / kernel TCP send buffer itself.
/// Throughput tradeoff (~16 s slower for a 500 MB paste) is small
/// compared to the audio quality gain. If you want to revisit, see
/// option D in the audio-sync notes — splitting display writes from
/// audio at the SharedWriter level, or kernel send-buffer tuning.
pub const LAZY_CHUNK_SIZE: u32 = 128 * 1024;

/// Number of in-flight `FileContentsRequest` PDUs for the eager flow.
/// 8 × `EAGER_CHUNK_SIZE` of pending response data is enough to keep the
/// cliprdr round-trip pipelined on a LAN without head-of-line-blocking
/// other static virtual channels.
pub const EAGER_PARALLEL_CHUNKS: usize = 8;

/// Number of in-flight `FileContentsRequest` PDUs for the lazy flow.
/// Serial (1) — the lazy path runs *while the user is interacting*
/// (paste-time). Combined with `LAZY_CHUNK_SIZE = 128 KiB`, peak
/// inbound in-flight is 128 KiB. The `dispatch_audio` carve-out in
/// vendor/ironrdp-server fixed the per-connection `Mutex<Self>`
/// contention; at 2-in-flight 256 KiB we found TCP send-buffer
/// pressure introduces audio/video stalls. Serial fetch keeps inbound
/// pressure low enough that the shared writer / kernel buffer isn't
/// the bottleneck.
pub const LAZY_PARALLEL_CHUNKS: usize = 1;

/// Shared event sender used by both the cliprdr backend (for ack PDUs) and
/// any eager-download task.
pub type EventSender = Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>;

/// Tracks the NSPasteboard `changeCount` value we ourselves set after
/// publishing remote files. The change-count poller in `clipboard.rs`
/// reads this and skips if the current changeCount matches — otherwise
/// the poller would see "new files on Mac pasteboard!" and round-trip
/// them back to Windows as a fresh Mac→Windows copy.
pub type SelfChangeCount = Arc<AtomicI64>;

/// Routes `FileContentsResponse` PDUs back to whichever download task is
/// awaiting them, keyed by `stream_id`. One instance lives on
/// `MacCliprdr` (the factory) and is cloned into the backend (which calls
/// `deliver` on each response) and into each spawned download task
/// (which calls `register` to allocate stream-ids).
#[derive(Clone, Debug, Default)]
pub struct DownloadRouter {
    pending: Arc<Mutex<HashMap<u32, oneshot::Sender<FileContentsResponse<'static>>>>>,
    next_stream_id: Arc<AtomicU32>,
    lease: Option<Arc<DownloadLease>>,
}
#[derive(Debug)]
struct DownloadLease {
    lock: u32,
    alive: Arc<AtomicBool>,
    sender: EventSender,
}
impl Drop for DownloadLease {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Acquire) {
            if let Some(sender) = self.sender.lock().unwrap().as_ref() {
                let _ = sender.send(ServerEvent::ClipboardLease {
                    active: self.alive.clone(),
                    id: self.lock,
                    retain: false,
                });
            }
        }
    }
}

impl DownloadRouter {
    pub fn for_copy(&self, lock: u32, alive: Arc<AtomicBool>, sender: EventSender) -> Self {
        Self {
            pending: self.pending.clone(),
            next_stream_id: self.next_stream_id.clone(),
            lease: Some(Arc::new(DownloadLease {
                lock,
                alive,
                sender,
            })),
        }
    }
    pub fn is_connected(&self) -> bool {
        self.lease
            .as_ref()
            .is_none_or(|lease| lease.alive.load(Ordering::Acquire))
    }
    pub fn event_sender(&self) -> Option<EventSender> {
        self.lease.as_ref().map(|lease| lease.sender.clone())
    }
    pub fn lock_id(&self) -> Option<u32> {
        self.lease.as_ref().map(|lease| lease.lock)
    }
    pub fn cancel_all(&self) {
        self.pending.lock().unwrap().clear();
    }

    /// Allocate a fresh stream-id and register the matching oneshot
    /// receiver. The id starts at 1 (mstsc treats 0 as "no stream") and
    /// skips zero on wrap-around.
    pub fn register(&self) -> (u32, oneshot::Receiver<FileContentsResponse<'static>>) {
        let mut sid = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        if sid == 0 {
            sid = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(sid, tx);
        (sid, rx)
    }

    /// Backend's `on_file_contents_response` calls this; routes the
    /// response to whichever task is awaiting this `stream_id`. Dropped
    /// silently if no one's waiting (e.g. paste was cancelled).
    pub fn deliver(&self, response: FileContentsResponse<'static>) {
        let stream_id = response.stream_id();
        if let Some(tx) = self.pending.lock().unwrap().remove(&stream_id) {
            let _ = tx.send(response);
        } else {
            debug!(stream_id, "no awaiter for FileContentsResponse; dropping");
        }
    }
}

/// One entry in a remote-copy batch — file OR directory. Directories
/// don't have an `index` worth fetching but DO have a path we need to
/// `mkdir -p` before any descendant file lands.
#[derive(Clone, Debug)]
pub struct RemoteEntry {
    pub index: i32,
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    /// MS-RDPECLIP relative directory path (using `\` as the separator),
    /// e.g. `MyFolder\sub`. `None` for top-level entries.
    pub relative_path: Option<String>,
}

/// Image-only virtual-file copies are fully downloaded and decoded before URLs
/// become visible. Apps that bypass NSFileCoordinator must never read holes.
pub fn spawn_remote_images(
    entries: Vec<RemoteEntry>,
    router: DownloadRouter,
    sender: EventSender,
    current_temp_dir: Arc<Mutex<Option<PathBuf>>>,
    self_change_count: SelfChangeCount,
    ticket: crate::clipboard_image::CopyTicket,
) {
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let dir = {
        let _guard = crate::clipboard::pasteboard_guard();
        if !ticket.is_current() || !router.is_connected() {
            return;
        }
        crate::file_promise_lazy::cleanup_previous(&current_temp_dir);
        let dir = match make_temp_dir() {
            Ok(dir) => dir,
            Err(error) => {
                warn!(%error, "image clipboard staging failed");
                return;
            }
        };
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        *current_temp_dir.lock().unwrap() = Some(dir.clone());
        dir
    };
    rt.spawn(async move {
        let work = async {
            let mut paths = Vec::new();
            for entry in &entries {
                let path = resolve_dest(&dir, entry)
                    .ok_or_else(|| anyhow::anyhow!("unsafe image path"))?;
                anyhow::ensure!(!paths.contains(&path), "duplicate image path");
                paths.push(path);
            }
            let specs = entries
                .iter()
                .map(|e| crate::clipboard_image::ImageSpec {
                    index: e.index,
                    size: e.size,
                })
                .collect();
            let images = crate::clipboard_image::prepare_images(specs, &ticket, |spec, budget| {
                let router = router.clone();
                let sender = sender.clone();
                let path =
                    paths[entries.iter().position(|e| e.index == spec.index).unwrap()].clone();
                async move {
                    let total = match spec.size {
                        Some(size) => size,
                        None => fetch_size(spec.index, &router, &sender)
                            .await
                            .map_err(anyhow::Error::msg)?,
                    };
                    anyhow::ensure!(
                        total > 0 && total <= budget,
                        "image download byte limit exceeded"
                    );
                    fetch_one_file(
                        spec.index,
                        Some(total),
                        &path,
                        &router,
                        &sender,
                        LAZY_PARALLEL_CHUNKS,
                        LAZY_CHUNK_SIZE,
                    )
                    .await
                    .map_err(anyhow::Error::msg)?;
                    Ok(tokio::task::spawn_blocking(move || std::fs::read(path)).await??)
                }
            })
            .await?;
            anyhow::ensure!(
                publish_ready_images(
                    &paths,
                    &images,
                    &ticket,
                    &current_temp_dir,
                    &dir,
                    &self_change_count
                ),
                "image copy superseded before publication"
            );
            info!(
                count = images.len(),
                "validated image clipboard published (complete files and PNG data)"
            );
            Ok(())
        };
        let result: anyhow::Result<()> = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(120), work) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("image download timed out"))),
            _ = ticket.superseded() => Err(anyhow::anyhow!("clipboard copy superseded")),
        };
        if let Err(error) = result {
            warn!(%error, "image clipboard not published");
            let mut current = current_temp_dir.lock().unwrap();
            if current.as_ref() == Some(&dir) {
                current.take();
            }
            drop(current);
            let _ = std::fs::remove_dir_all(&dir);
        }
    });
}

fn publish_ready_images(
    paths: &[PathBuf],
    images: &[Vec<u8>],
    ticket: &crate::clipboard_image::CopyTicket,
    current_temp_dir: &Arc<Mutex<Option<PathBuf>>>,
    dir: &Path,
    mark: &SelfChangeCount,
) -> bool {
    use objc2_app_kit::{NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypePNG};
    use objc2_foundation::NSData;
    let _guard = crate::clipboard::pasteboard_guard();
    let current = current_temp_dir.lock().unwrap();
    unsafe {
        let pb = crate::clipboard::pasteboard();
        if !ticket.may_publish(pb.changeCount() as i64) || current.as_deref() != Some(dir) {
            return false;
        }
        let mut writers: Vec<Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>>> =
            Vec::new();
        for (path, png) in paths.iter().zip(images) {
            let Some(path) = path.to_str() else {
                return false;
            };
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            let item = NSPasteboardItem::new();
            let Some(url_string) = url.absoluteString() else {
                return false;
            };
            if !item.setString_forType(&url_string, NSPasteboardTypeFileURL)
                || !item.setData_forType(&NSData::with_bytes(png), NSPasteboardTypePNG)
            {
                return false;
            }
            writers.push(ProtocolObject::from_retained(item));
        }
        crate::clipboard::begin_local_write(&pb);
        let ok = pb.writeObjects(&NSArray::from_vec(writers));
        mark.store(pb.changeCount() as i64, Ordering::Relaxed);
        ok
    }
}

/// Spawned by `on_remote_file_list`: downloads every entry in `entries`
/// into a fresh temp directory (recreating the directory structure from
/// the `relative_path` fields), then publishes the **top-level** paths
/// (entries where `relative_path` is `None`) to NSPasteboard so Finder
/// pastes the folder tree rather than 1000 leaf URLs.
pub struct FilePasteOptions {
    pub ticket: crate::clipboard_image::CopyTicket,
    pub session: String,
    pub prefer_provider: bool,
}

pub fn spawn_remote_paste(
    mut entries: Vec<RemoteEntry>,
    router: DownloadRouter,
    sender: EventSender,
    current_temp_dir: Arc<Mutex<Option<PathBuf>>>,
    self_change_count: SelfChangeCount,
    rt_handle: tokio::runtime::Handle,
    options: FilePasteOptions,
) {
    let FilePasteOptions {
        ticket,
        session,
        prefer_provider,
    } = options;
    if entries.is_empty() {
        return;
    }
    rt_handle.spawn(async move {
        if !ticket.is_current() || !router.is_connected() { return; }
        if entries.len() > 4096 { warn!("remote file count exceeds transfer limit"); return; }
        for entry in &mut entries {
            if !ticket.is_current() || !router.is_connected() { return; }
                if !entry.is_dir && entry.size.is_none() {
                    match fetch_size(entry.index, &router, &sender).await {
                        Ok(size) => entry.size = Some(size),
                        Err(error) => { warn!(%error, "remote file size request failed"); return; }
                    }
                }
            }
        if prefer_provider && crate::file_provider::available() && entries.iter().all(|e| !e.is_dir && e.relative_path.as_ref().is_none_or(|p| p.is_empty())) {
            match crate::file_provider::offer(&session, &entries, router.clone()).await {
                Ok(paths) => {
                    if publish_file_urls(&paths, &self_change_count, &ticket) { info!(files=paths.len(), "published OS dataless clipboard file URLs"); }
                    return;
                }
                Err(error) => warn!(%error, "File Provider unavailable for copy; downloading complete files before publication"),
            }
        }
        if !ticket.is_current() || !router.is_connected() { return; }
        // Preserve native PNG pasteboard representations on the complete-image fallback.
        if entries.len() <= 16 && entries.iter().map(|e| e.size.unwrap_or(u64::MAX)).try_fold(0u64,|sum,size|sum.checked_add(size)).is_some_and(|total|total<=crate::clipboard_image::MAX_IMAGE_BYTES as u64)
            && entries.iter().all(|e| !e.is_dir && e.relative_path.as_ref().is_none_or(|p|p.is_empty()) && crate::clipboard_image::is_image_name(&e.name)) {
            spawn_remote_images(entries, router, sender, current_temp_dir, self_change_count, ticket);
            return;
        }
        let dir = {
            let _guard = crate::clipboard::pasteboard_guard();
            if !ticket.is_current() || !router.is_connected() { return; }
            crate::file_promise_lazy::cleanup_previous(&current_temp_dir);
            let dir = match make_temp_dir() {
                Ok(dir) => dir,
                Err(error) => { warn!(%error, "file clipboard staging failed"); return; }
            };
            *current_temp_dir.lock().unwrap() = Some(dir.clone());
            dir
        };

        // Compute every entry's full destination path up front. Path-
        // safety check rejects anything that escapes the temp root via
        // `..` or absolute components — a malicious remote could otherwise
        // craft a relative_path that writes outside the temp dir.
        let mut top_level: Vec<PathBuf> = Vec::new();
        let mut planned: Vec<(RemoteEntry, PathBuf)> = Vec::with_capacity(entries.len());
        for e in entries {
            let dst = match resolve_dest(&dir, &e) {
                Some(p) => p,
                None => {
                    warn!(name = %e.name, "rejecting entry with unsafe relative_path");
                    return;
                }
            };
            if e.relative_path.as_ref().is_none_or(|p| p.is_empty()) {
                top_level.push(dst.clone());
            }
            if planned.iter().any(|(_, path): &(RemoteEntry, PathBuf)| path == &dst) {
                warn!("duplicate remote file destination"); return;
            }
            planned.push((e, dst));
        }

        // Create directories first so file fetches don't race the
        // mkdir. Sort directories shortest-path-first so parent dirs
        // exist before children.
        let mut dirs: Vec<&PathBuf> = planned
            .iter()
            .filter(|(e, _)| e.is_dir)
            .map(|(_, p)| p)
            .collect();
        dirs.sort_by_key(|p| p.components().count());
        for d in dirs {
            if let Err(err) = std::fs::create_dir_all(d) {
                warn!(?d, "create_dir_all failed: {err}");
                return;
            }
        }

        // Now fetch files. Sequential per-file (each file uses parallel
        // chunks internally), one failure aborts.
        for (e, dst) in &planned {
            if !ticket.is_current() || !router.is_connected() { return; }
            if e.size.is_some_and(|size| size > 128 * 1024 * 1024 * 1024) { warn!("remote file exceeds 128 GiB transfer limit"); return; }
            if e.is_dir {
                continue;
            }
            // Some clients only put descendant files in the descriptor
            // list, not their parent directories. Ensure the dst's
            // parent exists in that case.
            if let Some(parent) = dst.parent() {
                if let Err(err) = std::fs::create_dir_all(parent) {
                    warn!(?parent, "create_dir_all for file parent failed: {err}");
                    return;
                }
            }
            let download = fetch_one_file(e.index, e.size, dst, &router, &sender, EAGER_PARALLEL_CHUNKS, EAGER_CHUNK_SIZE);
            let result = tokio::select! { result = download => result, _ = ticket.superseded() => return };
            match result {
                Ok(()) => {
                    debug!(name = %e.name, dest = ?dst, "downloaded remote file");
                }
                Err(err) => {
                    warn!(name = %e.name, "download failed: {err}");
                    return;
                }
            }
        }

        if !publish_file_urls(&top_level, &self_change_count, &ticket) { return; }
        info!(
            files = planned.iter().filter(|(e, _)| !e.is_dir).count(),
            roots = top_level.len(),
            "published remote paste to NSPasteboard"
        );
        ready_signal();
    });
}

/// Compute the on-disk destination for a remote entry, joining its
/// `relative_path` components under `root` while rejecting any
/// path-traversal attempts. Returns `None` on unsafe input so the caller
/// can abort the whole paste.
///
/// Shared with `file_promise_lazy` so both paths apply the same
/// `..`/forward-slash rejection rules — a malicious remote could
/// otherwise craft a `relative_path` that writes outside the temp dir.
pub fn resolve_dest(root: &Path, entry: &RemoteEntry) -> Option<PathBuf> {
    let mut p = root.to_path_buf();
    if let Some(rp) = &entry.relative_path {
        for component in rp.split('\\') {
            if component.is_empty() {
                continue;
            }
            if component == "." || component == ".." || component.contains(['/', '\0']) {
                return None;
            }
            p.push(component);
        }
    }
    if entry.name.is_empty()
        || entry.name == "."
        || entry.name == ".."
        || entry.name.contains(['/', '\\', '\0'])
    {
        return None;
    }
    p.push(&entry.name);
    Some(p)
}

/// Audible completion cue only. Never synthesize a later Cmd-V: another
/// copy or focus change could turn it into pasting unrelated text/files.
fn ready_signal() {
    play_glass();
}

fn play_glass() {
    if let Err(e) = std::process::Command::new("afplay")
        .arg("/System/Library/Sounds/Glass.aiff")
        .spawn()
    {
        debug!("afplay spawn failed: {e}");
    }
}

fn make_temp_dir() -> std::io::Result<PathBuf> {
    use std::ffi::{CString, OsString};
    use std::os::unix::ffi::OsStringExt;

    // Atomically choose and create a private directory. Wall-clock timestamps
    // can repeat across concurrent copies even within the same process.
    let template = std::env::temp_dir().join(format!("macrdp-paste-{}-XXXXXX", std::process::id()));
    let mut template = CString::new(template.into_os_string().into_vec())
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?
        .into_bytes_with_nul();
    // SAFETY: mkdtemp receives an owned, writable, NUL-terminated buffer ending
    // in six Xs. It changes only that suffix and creates the directory as 0700.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let _ = template.pop(); // Remove the unchanged trailing NUL, preserving path bytes.
    Ok(PathBuf::from(OsString::from_vec(template)))
}

/// Reap eager-paste temp dirs (`$TMPDIR/macrdp-paste-<pid>-<suffix>`) left by a
/// PRIOR macrdp process that died uncleanly (SIGKILL/panic skip the backend's
/// Drop + the signal handler's cleanup). Dead-pid-gated, never our own, so it's
/// safe with another instance live. Best-effort; called once at startup.
pub fn reap_stale() {
    crate::reaper::for_each_stale(
        &std::env::temp_dir(),
        "macrdp-paste-",
        false,
        std::process::id() as i32,
        &crate::reaper::process_is_alive,
        &mut |dir| {
            let _ = std::fs::remove_dir_all(dir);
            debug!(stale_dir = ?dir, "eager paste: reaped temp dir from a dead prior macrdp process");
        },
    );
}

pub fn publish_file_urls(
    paths: &[PathBuf],
    self_change_count: &SelfChangeCount,
    ticket: &crate::clipboard_image::CopyTicket,
) -> bool {
    if paths.is_empty() {
        return false;
    }
    use objc2_app_kit::{NSPasteboardItem, NSPasteboardTypeFileURL};
    // Supply concrete file-URL data, rather than delegating serialization to
    // NSURL's pasteboard writer. A successful writeObjects alone is not proof
    // of usable contents (observed: true + unchanged CC, but zero file items).
    // No plain-text filename fallback: Finder would paste a text clipping.
    let mut writers: Vec<Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>>> =
        Vec::new();
    let mut expected = Vec::new();
    for path in paths {
        let Some(path) = path.to_str() else {
            return false;
        };
        unsafe {
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            let Some(value) = url.absoluteString() else {
                return false;
            };
            let item = NSPasteboardItem::new();
            if !item.setString_forType(&value, NSPasteboardTypeFileURL) {
                return false;
            }
            expected.push(value);
            writers.push(ProtocolObject::from_retained(item));
        }
    }
    let array = NSArray::from_vec(writers);
    // Serialize against the advertise poller + every other pasteboard toucher
    // (NSPasteboard is not thread-safe). The changeCount read stays inside the
    // guard so no other writer can bump it between our write and the capture.
    let _pb_guard = crate::clipboard::pasteboard_guard();
    let new_change_count = unsafe {
        let pb = crate::clipboard::pasteboard();
        if !ticket.may_publish(pb.changeCount() as i64) {
            return false;
        }
        crate::clipboard::begin_local_write(&pb);
        if !pb.writeObjects(&array) {
            return false;
        }
        let cc = pb.changeCount() as i64;
        self_change_count.store(cc, Ordering::Relaxed);
        let valid = pb.pasteboardItems().is_some_and(|items| {
            items.len() == expected.len()
                && expected.iter().enumerate().all(|(index, url)| {
                    items
                        .objectAtIndex(index)
                        .stringForType(NSPasteboardTypeFileURL)
                        .is_some_and(|actual| actual.isEqualToString(url))
                })
        });
        if !valid {
            warn!(
                files = paths.len(),
                change_count = cc,
                "file clipboard publication failed read-back verification"
            );
            return false;
        }
        cc
    };
    // Tell the Mac-side poller "we just wrote these; ignore the next
    // changeCount tick."
    self_change_count.store(new_change_count, Ordering::Relaxed);
    true
}

/// Download a single file as a fan-out of RANGE chunks. Up to
/// `max_parallel_chunks` requests are in flight at once; each completes
/// independently and writes its slice into the pre-allocated destination
/// at the matching offset. Returns `Ok(())` once every chunk has been
/// written.
///
/// Each chunk task opens its own `File` handle and `seek`s to its
/// position before writing. On Unix that's race-free: each open fd has
/// its own offset, and `pwrite`-like behavior via `seek+write` works as
/// long as no two tasks write the same byte range (which by construction
/// they don't — chunks are disjoint by `position`).
/// Download a single remote file's bytes into `dst`, parallelized as a
/// fan-out of RANGE requests. The destination is pre-allocated to the
/// final size so concurrent chunk writers don't race on extending it,
/// and all chunks share a single `Arc<File>` and write via `pwrite`
/// (`FileExt::write_at`) — race-free because the offset is per-call,
/// not per-fd, and it avoids the open+seek+close-per-chunk syscall
/// overhead of the earlier seek+write_all design.
///
/// Used by the eager paste flow (sequential per file, parallel chunks
/// internally) and by `file_promise_lazy::LazyPresenter` (one call per
/// file on the read-coordination block).
pub async fn fetch_one_file(
    index: i32,
    size_hint: Option<u64>,
    dst: &Path,
    router: &DownloadRouter,
    sender: &EventSender,
    max_parallel_chunks: usize,
    chunk_size: u32,
) -> Result<(), String> {
    let total = match size_hint {
        Some(s) => s,
        None => fetch_size(index, router, sender).await?,
    };

    // Open + pre-allocate the file to its final size once, then share
    // the handle across all chunk tasks. `set_len` writes a sparse hole
    // on APFS; the actual blocks materialize as chunks land.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|e| format!("create {dst:?}: {e}"))?;
    if total > 128 * 1024 * 1024 * 1024 {
        return Err("remote file exceeds 128 GiB transfer limit".into());
    }
    file.set_len(total)
        .map_err(|e| format!("set_len {dst:?}: {e}"))?;
    let file = Arc::new(file);

    if total == 0 {
        return Ok(());
    }

    // Build the chunk plan upfront so we know how many tasks to spawn.
    let mut plan: Vec<(u64, u32)> = Vec::new();
    let mut pos = 0u64;
    while pos < total {
        let req = (total - pos).min(u64::from(chunk_size)) as u32;
        plan.push((pos, req));
        pos += u64::from(req);
    }

    let mut set: tokio::task::JoinSet<Result<(), String>> = tokio::task::JoinSet::new();
    let mut next = 0usize;

    // Prime the in-flight window.
    while next < plan.len() && set.len() < max_parallel_chunks {
        let (p, sz) = plan[next];
        set.spawn(chunk_task(
            index,
            p,
            sz,
            router.clone(),
            sender.clone(),
            file.clone(),
        ));
        next += 1;
    }

    // As each chunk completes, refill the window until the plan is
    // exhausted; collect any error and abort the rest.
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                set.abort_all();
                return Err(e);
            }
            Err(join_err) => {
                set.abort_all();
                return Err(format!("chunk task panicked: {join_err}"));
            }
        }
        if next < plan.len() {
            let (p, sz) = plan[next];
            set.spawn(chunk_task(
                index,
                p,
                sz,
                router.clone(),
                sender.clone(),
                file.clone(),
            ));
            next += 1;
        }
    }
    Ok(())
}

async fn chunk_task(
    index: i32,
    position: u64,
    requested_size: u32,
    router: DownloadRouter,
    sender: EventSender,
    file: Arc<std::fs::File>,
) -> Result<(), String> {
    use std::os::unix::fs::FileExt;
    let mut received = 0u32;
    while received < requested_size {
        let remaining = requested_size - received;
        let bytes = fetch_range(
            index,
            position + u64::from(received),
            remaining,
            &router,
            &sender,
        )
        .await?;
        if bytes.is_empty() || bytes.len() > remaining as usize {
            return Err(format!(
                "invalid range read at {}: expected 1..={remaining}, got {}",
                position + u64::from(received),
                bytes.len()
            ));
        }
        let mut written = 0usize;
        while written < bytes.len() {
            let n = file
                .write_at(
                    &bytes[written..],
                    position + u64::from(received) + written as u64,
                )
                .map_err(|e| format!("write_at: {e}"))?;
            if n == 0 {
                return Err("write_at returned zero".into());
            }
            written += n;
        }
        received += bytes.len() as u32;
    }
    Ok(())
}

pub async fn fetch_size(
    index: i32,
    router: &DownloadRouter,
    sender: &EventSender,
) -> Result<u64, String> {
    if !router.is_connected() {
        return Err("clipboard session disconnected".into());
    }
    let (stream_id, rx) = router.register();
    struct Pending {
        id: u32,
        router: DownloadRouter,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            self.router.pending.lock().unwrap().remove(&self.id);
        }
    }
    let _pending = Pending {
        id: stream_id,
        router: router.clone(),
    };
    push_request(
        sender,
        router,
        FileContentsRequest {
            stream_id,
            index,
            flags: FileContentsFlags::SIZE,
            position: 0,
            requested_size: 8,
            data_id: router.lock_id(),
        },
    )?;
    let resp = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .map_err(|_| "size request timed out".to_string())?
        .map_err(|_| "size: channel closed".to_string())?;
    if resp.is_error() {
        return Err("size: remote returned CB_RESPONSE_FAIL".into());
    }
    resp.data_as_size().map_err(|e| format!("size decode: {e}"))
}

pub async fn fetch_range(
    index: i32,
    position: u64,
    requested_size: u32,
    router: &DownloadRouter,
    sender: &EventSender,
) -> Result<Vec<u8>, String> {
    if !router.is_connected() {
        return Err("clipboard session disconnected".into());
    }
    let (stream_id, rx) = router.register();
    struct Pending {
        id: u32,
        router: DownloadRouter,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            self.router.pending.lock().unwrap().remove(&self.id);
        }
    }
    let _pending = Pending {
        id: stream_id,
        router: router.clone(),
    };
    push_request(
        sender,
        router,
        FileContentsRequest {
            stream_id,
            index,
            flags: FileContentsFlags::RANGE,
            position,
            requested_size,
            data_id: router.lock_id(),
        },
    )?;
    let resp = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .map_err(|_| "range request timed out".to_string())?
        .map_err(|_| "range: channel closed".to_string())?;
    if !router.is_connected() {
        return Err("clipboard session disconnected".into());
    }
    if resp.is_error() {
        return Err(format!(
            "range at {position}: remote returned CB_RESPONSE_FAIL"
        ));
    }
    Ok(resp.data().to_vec())
}

fn push_request(
    sender: &EventSender,
    router: &DownloadRouter,
    req: FileContentsRequest,
) -> Result<(), String> {
    let guard = sender.lock().unwrap();
    let s = guard
        .as_ref()
        .ok_or_else(|| "event sender unavailable (server shutting down?)".to_string())?;
    let message = ClipboardMessage::SendFileContentsRequest(req);
    let event = match &router.lease {
        Some(lease) => ServerEvent::SessionClipboard {
            active: lease.alive.clone(),
            message,
            permit: None,
        },
        None => ServerEvent::Clipboard(message),
    };
    s.send(event)
        .map_err(|_| "event channel closed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_temp_dirs_are_unique_private_writable_and_pid_tagged() {
        use std::collections::HashSet;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Barrier;

        struct TestDir(PathBuf);
        impl Drop for TestDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        const WORKERS: usize = 8;
        const PER_WORKER: usize = 32;
        let barrier = Arc::new(Barrier::new(WORKERS));
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..PER_WORKER)
                        .map(|_| {
                            TestDir(make_temp_dir().expect("create private staging directory"))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let dirs: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("directory worker"))
            .collect();
        let unique: HashSet<_> = dirs.iter().map(|dir| &dir.0).collect();
        assert_eq!(unique.len(), WORKERS * PER_WORKER);

        for (index, dir) in dirs.iter().enumerate() {
            let metadata = std::fs::symlink_metadata(&dir.0).unwrap();
            assert!(metadata.is_dir());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            assert_eq!(
                crate::reaper::pid_from_tagged(
                    dir.0.file_name().unwrap().to_str().unwrap(),
                    "macrdp-paste-",
                    false,
                ),
                Some(std::process::id() as i32)
            );
            let probe = dir.0.join("probe");
            std::fs::write(&probe, index.to_be_bytes()).unwrap();
            assert_eq!(std::fs::read(probe).unwrap(), index.to_be_bytes());
        }
    }

    fn entry(name: &str, rp: Option<&str>) -> RemoteEntry {
        RemoteEntry {
            index: 0,
            name: name.to_owned(),
            size: None,
            is_dir: false,
            relative_path: rp.map(|s| s.to_owned()),
        }
    }

    #[test]
    fn resolve_dest_top_level_file() {
        let root = std::path::Path::new("/tmp/root");
        assert_eq!(
            resolve_dest(root, &entry("a.txt", None)).unwrap(),
            std::path::PathBuf::from("/tmp/root/a.txt")
        );
    }

    #[test]
    fn resolve_dest_joins_backslash_relative_path() {
        let root = std::path::Path::new("/tmp/root");
        assert_eq!(
            resolve_dest(root, &entry("foo.txt", Some("MyFolder\\sub"))).unwrap(),
            std::path::PathBuf::from("/tmp/root/MyFolder/sub/foo.txt")
        );
    }

    #[test]
    fn resolve_dest_rejects_dot_dot_in_relative_path() {
        let root = std::path::Path::new("/tmp/root");
        assert!(resolve_dest(root, &entry("p.txt", Some("..\\evil"))).is_none());
    }

    #[test]
    fn resolve_dest_rejects_dot_dot_in_name() {
        let root = std::path::Path::new("/tmp/root");
        assert!(resolve_dest(root, &entry("..", Some("MyFolder"))).is_none());
    }

    #[test]
    fn resolve_dest_rejects_forward_slash_in_component() {
        // A remote whose relative_path contains a literal `/` is trying
        // to smuggle an extra path component past our `\`-split. Reject.
        let root = std::path::Path::new("/tmp/root");
        assert!(resolve_dest(root, &entry("p.txt", Some("MyFolder/sub"))).is_none());
    }

    #[test]
    fn resolve_dest_handles_empty_relative_path() {
        let root = std::path::Path::new("/tmp/root");
        // Some clients send "" for top-level entries instead of None.
        assert_eq!(
            resolve_dest(root, &entry("a.txt", Some(""))).unwrap(),
            std::path::PathBuf::from("/tmp/root/a.txt")
        );
    }
    #[tokio::test]
    async fn short_range_reads_are_completed_without_zero_filled_tails() {
        let dir = make_temp_dir().unwrap();
        let path = dir.join("bytes");
        let router = DownloadRouter::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = Arc::new(Mutex::new(Some(tx)));
        let route = router.clone();
        let remote = tokio::spawn(async move {
            for (position, payload) in [(0, b"abc".as_slice()), (3, b"def".as_slice())] {
                let ServerEvent::Clipboard(ClipboardMessage::SendFileContentsRequest(req)) =
                    rx.recv().await.unwrap()
                else {
                    panic!("unexpected event");
                };
                assert_eq!(req.position, position);
                assert_eq!(req.requested_size, 6 - position as u32);
                route.deliver(FileContentsResponse::new_data_response(
                    req.stream_id,
                    payload.to_vec(),
                ));
            }
        });
        fetch_one_file(0, Some(6), &path, &router, &sender, 1, 6)
            .await
            .unwrap();
        remote.await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn empty_and_oversized_ranges_fail_instead_of_publishing_holes() {
        for payload in [vec![], vec![1; 7]] {
            let dir = make_temp_dir().unwrap();
            let path = dir.join("bytes");
            let router = DownloadRouter::default();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let sender = Arc::new(Mutex::new(Some(tx)));
            let route = router.clone();
            let remote = tokio::spawn(async move {
                let ServerEvent::Clipboard(ClipboardMessage::SendFileContentsRequest(req)) =
                    rx.recv().await.unwrap()
                else {
                    panic!("unexpected event");
                };
                route.deliver(FileContentsResponse::new_data_response(
                    req.stream_id,
                    payload,
                ));
            });
            assert!(fetch_one_file(0, Some(6), &path, &router, &sender, 1, 6)
                .await
                .is_err());
            remote.await.unwrap();
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn file_publication_exposes_urls_without_text_and_rejects_local_copy_races() {
        use objc2_app_kit::{NSPasteboardTypeFileURL, NSPasteboardTypeString};
        let dir = make_temp_dir().unwrap();
        let paths = vec![dir.join("测试 file #1.txt"), dir.join("second.txt")];
        for path in &paths {
            std::fs::write(path, b"fixture").unwrap();
        }
        let mark = Arc::new(AtomicI64::new(-1));
        let cc = {
            let _guard = crate::clipboard::pasteboard_guard();
            unsafe { crate::clipboard::pasteboard().changeCount() as i64 }
        };
        let ticket = crate::clipboard_image::CopyTicket::new(
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
            cc,
        );
        assert!(publish_file_urls(&paths, &mark, &ticket));
        {
            let _guard = crate::clipboard::pasteboard_guard();
            unsafe {
                let pb = crate::clipboard::pasteboard();
                let items = pb.pasteboardItems().expect("published file items");
                assert_eq!(items.len(), paths.len());
                for (index, path) in paths.iter().enumerate() {
                    let item = items.objectAtIndex(index);
                    let value = item
                        .stringForType(NSPasteboardTypeFileURL)
                        .expect("every file must expose a URL");
                    let url = NSURL::URLWithString(&value).unwrap();
                    assert_eq!(url.path().unwrap().to_string(), path.to_str().unwrap());
                    assert!(item.stringForType(NSPasteboardTypeString).is_none());
                }
                assert_eq!(mark.load(Ordering::Relaxed), pb.changeCount() as i64);
                pb.clearContents();
                pb.setString_forType(&NSString::from_str("local copy"), NSPasteboardTypeString);
            }
        }
        assert!(!publish_file_urls(&paths, &mark, &ticket));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ready_image_publication_includes_real_file_and_rejects_local_copy_races() {
        use image::ImageEncoder;
        let dir = make_temp_dir().unwrap();
        let path = dir.join("Screenshot.png");
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[20, 50, 90, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&path, &png).unwrap();
        let slot = Arc::new(Mutex::new(Some(dir.clone())));
        let mark = Arc::new(AtomicI64::new(-1));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let cc = {
            let _g = crate::clipboard::pasteboard_guard();
            unsafe { crate::clipboard::pasteboard().changeCount() as i64 }
        };
        let ticket = crate::clipboard_image::CopyTicket::new(epoch, cc);
        assert!(publish_ready_images(
            std::slice::from_ref(&path),
            &[png.clone()],
            &ticket,
            &slot,
            &dir,
            &mark
        ));
        assert_eq!(std::fs::read(&path).unwrap(), png);
        let _guard = crate::clipboard::pasteboard_guard();
        unsafe {
            let pb = crate::clipboard::pasteboard();
            assert!(pb.dataForType(objc2_app_kit::NSPasteboardTypePNG).is_some());
            assert!(pb
                .stringForType(objc2_app_kit::NSPasteboardTypeFileURL)
                .is_some());
            pb.clearContents();
            pb.setString_forType(
                &NSString::from_str("new local copy"),
                objc2_app_kit::NSPasteboardTypeString,
            );
        }
        drop(_guard);
        assert!(!publish_ready_images(
            &[path],
            &[png],
            &ticket,
            &slot,
            &dir,
            &mark
        ));
        let _guard = crate::clipboard::pasteboard_guard();
        unsafe {
            assert_eq!(
                crate::clipboard::pasteboard()
                    .stringForType(objc2_app_kit::NSPasteboardTypeString)
                    .unwrap()
                    .to_string(),
                "new local copy"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn virtual_screenshot_publishes_nothing_until_every_byte_arrives() {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[7, 18, 29, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let initial = {
            let _guard = crate::clipboard::pasteboard_guard();
            unsafe {
                let pb = crate::clipboard::pasteboard();
                pb.clearContents();
                pb.changeCount() as i64
            }
        };
        let ticket = crate::clipboard_image::CopyTicket::new(
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
            initial,
        );
        let router = DownloadRouter::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mark = Arc::new(AtomicI64::new(-1));
        let slot = Arc::new(Mutex::new(None));
        spawn_remote_images(
            vec![RemoteEntry {
                size: Some(png.len() as u64),
                ..entry("Screenshot.png", None)
            }],
            router.clone(),
            Arc::new(Mutex::new(Some(tx))),
            slot.clone(),
            mark.clone(),
            ticket,
        );
        let ServerEvent::Clipboard(ClipboardMessage::SendFileContentsRequest(req)) =
            rx.recv().await.unwrap()
        else {
            panic!("unexpected event");
        };
        {
            let _guard = crate::clipboard::pasteboard_guard();
            unsafe {
                assert_eq!(crate::clipboard::pasteboard().changeCount() as i64, initial);
            }
        }
        // A short response must cause a new request, without publishing the sparse file.
        router.deliver(FileContentsResponse::new_data_response(
            req.stream_id,
            png[..20].to_vec(),
        ));
        let ServerEvent::Clipboard(ClipboardMessage::SendFileContentsRequest(req)) =
            rx.recv().await.unwrap()
        else {
            panic!("unexpected event");
        };
        assert_eq!(req.position, 20);
        assert_eq!(mark.load(Ordering::Relaxed), -1);
        router.deliver(FileContentsResponse::new_data_response(
            req.stream_id,
            png[20..].to_vec(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while mark.load(Ordering::Relaxed) == -1 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let dir = slot.lock().unwrap().take().unwrap();
        assert_eq!(std::fs::read(dir.join("Screenshot.png")).unwrap(), png);
        {
            let _guard = crate::clipboard::pasteboard_guard();
            unsafe {
                assert!(crate::clipboard::pasteboard()
                    .dataForType(objc2_app_kit::NSPasteboardTypePNG)
                    .is_some());
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn request_cancellation_removes_its_pending_route() {
        let router = DownloadRouter::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = Arc::new(Mutex::new(Some(tx)));
        let task = {
            let router = router.clone();
            tokio::spawn(async move { fetch_range(0, 0, 1, &router, &sender).await })
        };
        rx.recv().await.unwrap();
        assert_eq!(router.pending.lock().unwrap().len(), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(router.pending.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn copy_lease_releases_only_after_last_consumer_and_never_across_sessions() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = Arc::new(Mutex::new(Some(tx)));
        let alive = Arc::new(AtomicBool::new(true));
        let first = DownloadRouter::default().for_copy(7, alive.clone(), sender.clone());
        let second = first.clone();
        drop(first);
        assert!(rx.try_recv().is_err());
        drop(second);
        assert!(matches!(
            rx.try_recv(),
            Ok(ServerEvent::ClipboardLease {
                id: 7,
                retain: false,
                ..
            })
        ));
        let old = DownloadRouter::default().for_copy(8, alive.clone(), sender);
        alive.store(false, Ordering::Release);
        drop(old);
        assert!(rx.try_recv().is_err());
    }
}
