//! OS dataless file URLs for flat remote file copies. The broker is loopback-only,
//! per-process authenticated, and every RANGE retains its original session/lock.
#![cfg(target_os = "macos")]
use crate::file_promise::{DownloadRouter, RemoteEntry};
use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
use tracing::{info, warn};

const MAX_FILES: usize = 4096;
const MAX_SIZE: u64 = 128 * 1024 * 1024 * 1024;
static BRIDGE: OnceLock<Arc<FilesBridge>> = OnceLock::new();
struct Copy {
    session: String,
    id: String,
    entries: Vec<RemoteEntry>,
    router: DownloadRouter,
    born: Instant,
    touched: Instant,
    current: bool,
}
#[derive(Default)]
struct State {
    active: Option<String>,
    copies: HashMap<String, Copy>,
}
pub struct FilesBridge {
    state: Mutex<State>,
    helper: PathBuf,
    instance: String,
    ready: AtomicBool,
    pub ranges: AtomicU64,
    metadata: AtomicU64,
    downloads: Semaphore,
    last_error: Mutex<Option<String>>,
}
pub fn random_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        return String::new();
    }
    let text = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &text[..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..]
    )
}
fn helper_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .parent()?
        .join("Helpers/macrdpFiles.app/Contents/MacOS/macrdpFiles");
    path.is_file().then_some(path)
}
pub fn start_if_bundled() {
    let Some(helper) = helper_path() else {
        return;
    };
    let instance = random_id();
    if instance.is_empty() {
        warn!("File Provider: random identity unavailable");
        return;
    }
    let bridge = Arc::new(FilesBridge {
        state: Mutex::new(State::default()),
        helper,
        instance,
        ready: AtomicBool::new(false),
        ranges: AtomicU64::new(0),
        metadata: AtomicU64::new(0),
        downloads: Semaphore::new(1),
        last_error: Mutex::new(None),
    });
    if BRIDGE.set(bridge.clone()).is_err() {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = bridge.clone().start().await {
            *bridge.last_error.lock().unwrap() = Some(error.to_string());
            warn!(%error,"File Provider unavailable; complete-file download fallback remains active");
        }
    });
}
pub fn activate(session: &str) {
    if let Some(bridge) = BRIDGE.get() {
        bridge.state.lock().unwrap().active = Some(session.into());
    }
}
pub fn disconnect(session: &str) {
    if let Some(bridge) = BRIDGE.get() {
        let mut state = bridge.state.lock().unwrap();
        if state.active.as_deref() == Some(session) {
            state.active = None;
        }
        state.copies.retain(|_, copy| copy.session != session);
    }
}
pub fn transition(session: &str) {
    if let Some(bridge) = BRIDGE.get() {
        for copy in bridge.state.lock().unwrap().copies.values_mut() {
            if copy.session == session {
                copy.current = false;
            }
        }
    }
}
pub fn status() -> Value {
    BRIDGE.get().map_or_else(|| json!({"helper_bundled":false,"provider_ready_at_startup":false}), |bridge| json!({
        "helper_bundled":true,"provider_ready_at_startup":bridge.ready.load(Ordering::Acquire),
        "content_range_requests":bridge.ranges.load(Ordering::Relaxed),"metadata_requests":bridge.metadata.load(Ordering::Relaxed),
        "retained_copies":bridge.state.lock().unwrap().copies.len(),"last_error":*bridge.last_error.lock().unwrap()
    }))
}
pub fn available() -> bool {
    BRIDGE
        .get()
        .is_some_and(|bridge| bridge.ready.load(Ordering::Acquire))
}
pub async fn shutdown() {
    if let Some(bridge) = BRIDGE.get() {
        bridge.ready.store(false, Ordering::Release);
        let cleanup = bridge.command("stop", json!({"instance":bridge.instance}));
        if tokio::time::timeout(Duration::from_secs(3), cleanup)
            .await
            .is_err()
        {
            warn!("File Provider shutdown cleanup deferred to the next startup");
        }
    }
}
/// Returns paths only after the OS has registered genuine provider items. No
/// content fetch is performed by the host process that resolves these URLs.
pub async fn offer(
    session: &str,
    entries: &[RemoteEntry],
    router: DownloadRouter,
) -> Result<Vec<PathBuf>> {
    let bridge = BRIDGE.get().context("File Provider helper unavailable")?;
    ensure!(
        bridge.ready.load(Ordering::Acquire),
        "File Provider not ready"
    );
    validate(entries)?;
    ensure!(
        router.lock_id().is_some(),
        "remote clipboard locking required"
    );
    let id = random_id();
    ensure!(!id.is_empty(), "random copy identity unavailable");
    {
        let mut state = bridge.state.lock().unwrap();
        ensure!(
            state.active.as_deref() == Some(session),
            "clipboard session changed"
        );
        ensure!(
            state.copies.len() < 64
                && state
                    .copies
                    .values()
                    .map(|c| c.entries.len())
                    .sum::<usize>()
                    + entries.len()
                    <= MAX_FILES,
            "provider catalog capacity reached"
        );
        state.copies.insert(
            id.clone(),
            Copy {
                session: session.into(),
                id: id.clone(),
                entries: entries.to_vec(),
                router,
                born: Instant::now(),
                touched: Instant::now(),
                current: true,
            },
        );
    }
    let items: Vec<_> = entries
        .iter()
        .map(|entry| format!("{session}/{id}/{}", entry.index))
        .collect();
    let result = bridge
        .command("resolve", json!({"instance":bridge.instance,"items":items}))
        .await;
    match result {
        Ok(value) => {
            *bridge.last_error.lock().unwrap() = None;
            let paths = value["paths"]
                .as_array()
                .context("missing provider paths")?;
            ensure!(paths.len() == entries.len(), "incomplete provider URLs");
            paths
                .iter()
                .map(|path| {
                    path.as_str()
                        .filter(|p| Path::new(p).is_absolute())
                        .map(PathBuf::from)
                        .context("invalid provider URL")
                })
                .collect()
        }
        Err(error) => {
            *bridge.last_error.lock().unwrap() = Some(error.to_string());
            bridge.state.lock().unwrap().copies.remove(&id);
            Err(error)
        }
    }
}
fn validate(entries: &[RemoteEntry]) -> Result<()> {
    ensure!(
        !entries.is_empty() && entries.len() <= MAX_FILES,
        "file count limit"
    );
    let mut names = HashSet::new();
    let mut indexes = HashSet::new();
    for entry in entries {
        ensure!(
            !entry.is_dir && entry.relative_path.as_ref().is_none_or(|p| p.is_empty()),
            "folders use complete-file fallback"
        );
        ensure!(
            entry.index >= 0
                && indexes.insert(entry.index)
                && !entry.name.is_empty()
                && entry.name != "."
                && entry.name != ".."
                && entry.name.len() <= 255
                && !entry.name.contains(['/', '\\', '\0'])
                && names.insert(entry.name.to_lowercase()),
            "invalid or duplicate file name/index"
        );
        ensure!(
            entry.size.is_some_and(|n| n <= MAX_SIZE),
            "file size unavailable or exceeds 128 GiB"
        );
    }
    Ok(())
}
impl FilesBridge {
    async fn command(&self, command: &str, input: Value) -> Result<Value> {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new(&self.helper)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().context("missing helper stdin")?;
        stdin.write_all(&serde_json::to_vec(&input)?).await?;
        drop(stdin);
        let result = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
            .await
            .context("File Provider helper timeout")??;
        ensure!(
            result.stdout.len() <= 2 * 1024 * 1024,
            "provider response limit"
        );
        let value: Value =
            serde_json::from_slice(&result.stdout).context("invalid helper response")?;
        if !result.status.success() {
            bail!(
                "File Provider {}: {} ({})",
                command,
                value["error"],
                value["code"]
            );
        }
        Ok(value)
    }
    async fn start(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| anyhow!("random token unavailable"))?;
        let token = Arc::new(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let bridge = self.clone();
        let auth = token.clone();
        tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(8));
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    continue;
                };
                let b = bridge.clone();
                let token = auth.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = b.serve(socket, &token).await;
                });
            }
        });
        let app = self
            .helper
            .parent()
            .and_then(Path::parent)
            .context("helper bundle path")?;
        let bundle = app.parent().context("helper bundle root")?;
        let registrar="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
        let result = tokio::process::Command::new(registrar)
            .arg("-f")
            .arg(bundle)
            .output()
            .await?;
        ensure!(
            result.status.success(),
            "could not register File Provider host"
        );
        let result = tokio::process::Command::new("/usr/bin/pluginkit")
            .arg("-a")
            .arg(bundle.join("Contents/PlugIns/macrdpFiles.appex"))
            .output()
            .await?;
        ensure!(
            result.status.success(),
            "could not register File Provider extension"
        );

        // Listener is already serving before adding the domain: macOS may
        // immediately ask the extension for metadata from inside addDomain.
        self.command(
            "start",
            json!({"instance":self.instance,"port":port,"token":*token,"pid":std::process::id()}),
        )
        .await?;
        self.ready.store(true, Ordering::Release);
        info!("File Provider registered; dataless clipboard files available after extension is enabled");
        Ok(())
    }
    fn snapshots(&self) -> Value {
        self.metadata.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        state.copies.retain(|_, copy| {
            copy.router.is_connected()
                && copy.born.elapsed() < Duration::from_secs(7200)
                && (copy.current || copy.touched.elapsed() < Duration::from_secs(600))
        });
        let snapshots:Vec<Value>=state.copies.values().flat_map(|copy|copy.entries.iter().map(move|entry|json!({"session":copy.session,"copy":copy.id,"lock":copy.router.lock_id(),"index":entry.index,"name":entry.name,"size":entry.size}))).collect();
        json!({"snapshots":snapshots})
    }
    async fn range(&self, request: &Value) -> Result<Value> {
        let _permit =
            tokio::time::timeout(Duration::from_secs(25), self.downloads.acquire()).await??;
        let session = request["session"].as_str().context("stale")?;
        let id = request["copy"].as_str().context("stale")?;
        let index = request["index"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .context("stale")?;
        let (router, size) = {
            let mut state = self.state.lock().unwrap();
            let copy = state.copies.get_mut(id).context("stale")?;
            ensure!(
                copy.session == session
                    && copy.router.is_connected()
                    && request["lock"].as_u64() == copy.router.lock_id().map(u64::from),
                "stale"
            );
            ensure!(copy.born.elapsed() < Duration::from_secs(7200), "stale");
            let entry = copy
                .entries
                .iter()
                .find(|entry| entry.index == index)
                .context("stale")?;
            let size = entry.size.context("stale")?;
            copy.touched = Instant::now();
            (copy.router.clone(), size)
        };
        let offset = request["offset"].as_u64().context("invalid range")?;
        let length = request["length"].as_u64().context("invalid range")?;
        ensure!(
            length > 0
                && length <= 128 * 1024
                && offset.checked_add(length).is_some_and(|end| end <= size),
            "invalid range"
        );
        self.ranges.fetch_add(1, Ordering::Relaxed);
        let sender = router.event_sender().context("stale")?;
        let bytes =
            crate::file_promise::fetch_range(index, offset, length as u32, &router, &sender)
                .await
                .map_err(anyhow::Error::msg)?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= length as usize && router.is_connected(),
            "invalid or disconnected range response"
        );
        Ok(
            json!({"session":session,"copy":id,"lock":router.lock_id(),"index":index,"offset":offset,"bytes":STANDARD.encode(bytes)}),
        )
    }
    async fn serve(&self, mut socket: TcpStream, token: &str) -> Result<()> {
        let (auth, payload) =
            tokio::time::timeout(Duration::from_secs(5), read_request(&mut socket)).await??;
        let expected = format!("Bearer {token}");
        let authorized = auth.len() == expected.len()
            && auth
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |a, (b, c)| a | (b ^ c))
                == 0;
        let (status, body) = if !authorized {
            (401, json!({}))
        } else {
            match payload["op"].as_str() {
                Some("metadata") => (200, self.snapshots()),
                Some("stats") => (
                    200,
                    json!({"ranges":self.ranges.load(Ordering::Relaxed),"metadata":self.metadata.load(Ordering::Relaxed),"ready":self.ready.load(Ordering::Relaxed)}),
                ),
                Some("range") => match self.range(&payload).await {
                    Ok(value) => (200, value),
                    Err(error) => (
                        if error.to_string() == "stale" {
                            409
                        } else {
                            503
                        },
                        json!({}),
                    ),
                },
                _ => (400, json!({})),
            }
        };
        let bytes = serde_json::to_vec(&body)?;
        let header=format!("HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len());
        tokio::time::timeout(Duration::from_secs(5), async {
            socket.write_all(header.as_bytes()).await?;
            socket.write_all(&bytes).await
        })
        .await??;
        Ok(())
    }
}
async fn read_request(stream: &mut TcpStream) -> Result<(String, Value)> {
    let mut input = Vec::new();
    let mut chunk = [0u8; 512];
    let end = loop {
        if let Some(index) = input.windows(4).position(|w| w == b"\r\n\r\n") {
            break index + 4;
        }
        let n = stream.read(&mut chunk).await?;
        ensure!(n > 0 && input.len() + n <= 8192, "header limit");
        input.extend_from_slice(&chunk[..n]);
    };
    let header = std::str::from_utf8(&input[..end])?;
    ensure!(header.starts_with("POST /v1 HTTP/1."), "invalid request");
    let mut length = None;
    let mut auth = String::new();
    for line in header.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                ensure!(length.is_none(), "duplicate length");
                length = Some(value.trim().parse::<usize>()?);
            }
            if name.eq_ignore_ascii_case("authorization") {
                ensure!(auth.is_empty(), "duplicate authorization");
                auth = value.trim().into();
            }
        }
    }
    let length = length.context("missing length")?;
    ensure!(length > 0 && length <= 4096, "body limit");
    while input.len() < end + length {
        let count = stream
            .read(&mut chunk[..(end + length - input.len()).min(512)])
            .await?;
        ensure!(count > 0, "short body");
        input.extend_from_slice(&chunk[..count]);
    }
    Ok((auth, serde_json::from_slice(&input[end..end + length])?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::FileContentsResponse};
    use ironrdp_server::ServerEvent;
    fn entry(name: &str) -> RemoteEntry {
        RemoteEntry {
            index: 0,
            name: name.into(),
            size: Some(32),
            is_dir: false,
            relative_path: None,
        }
    }
    fn bridge() -> FilesBridge {
        FilesBridge {
            state: Mutex::new(State::default()),
            helper: PathBuf::new(),
            instance: random_id(),
            ready: AtomicBool::new(false),
            ranges: AtomicU64::new(0),
            metadata: AtomicU64::new(0),
            downloads: Semaphore::new(1),
            last_error: Mutex::new(None),
        }
    }
    #[test]
    fn flat_copy_validation_rejects_unsupported_or_ambiguous_items() {
        assert!(validate(&[entry("file.bin")]).is_ok());
        for name in ["", ".", "..", "../file", "a\\b", "a/b", "a\0b"] {
            assert!(validate(&[entry(name)]).is_err());
        }
        let mut other = entry("FILE.bin");
        other.index = 1;
        assert!(validate(&[entry("file.bin"), other]).is_err());
        let mut folder = entry("folder");
        folder.is_dir = true;
        assert!(validate(&[folder]).is_err());
        let mut unknown = entry("unknown");
        unknown.size = None;
        assert!(validate(&[unknown]).is_err());
        let mut big = entry("big");
        big.size = Some(MAX_SIZE + 1);
        assert!(validate(&[big]).is_err());
    }
    #[tokio::test]
    async fn range_uses_original_copy_lock_without_changing_current_clipboard() {
        let bridge = Arc::new(bridge());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let alive = Arc::new(AtomicBool::new(true));
        let base = DownloadRouter::default();
        let router = base.for_copy(7, alive, Arc::new(Mutex::new(Some(tx))));
        bridge.state.lock().unwrap().copies.insert(
            "old".into(),
            Copy {
                session: "A".into(),
                id: "old".into(),
                entries: vec![entry("file.bin")],
                router,
                born: Instant::now(),
                touched: Instant::now(),
                current: false,
            },
        );
        let request = json!({"session":"A","copy":"old","lock":7,"index":0,"offset":0,"length":16});
        let task = {
            let bridge = bridge.clone();
            tokio::spawn(async move { bridge.range(&request).await })
        };
        let ServerEvent::Clipboard(ClipboardMessage::SendFileContentsRequest(request)) =
            rx.recv().await.unwrap()
        else {
            panic!("unexpected event")
        };
        assert_eq!(request.data_id, Some(7));
        assert_eq!(request.position, 0);
        assert_eq!(request.requested_size, 16);
        base.deliver(FileContentsResponse::new_data_response(
            request.stream_id,
            vec![3; 8],
        ));
        let reply = task.await.unwrap().unwrap();
        assert_eq!(reply["copy"], "old");
        assert_eq!(reply["lock"], 7);
        assert_eq!(
            STANDARD.decode(reply["bytes"].as_str().unwrap()).unwrap(),
            vec![3; 8]
        );
    }
    #[tokio::test]
    async fn old_session_and_wrong_lock_fail_before_network_io() {
        let bridge = bridge();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let alive = Arc::new(AtomicBool::new(true));
        let router =
            DownloadRouter::default().for_copy(7, alive.clone(), Arc::new(Mutex::new(Some(tx))));
        bridge.state.lock().unwrap().copies.insert(
            "old".into(),
            Copy {
                session: "A".into(),
                id: "old".into(),
                entries: vec![entry("file.bin")],
                router,
                born: Instant::now(),
                touched: Instant::now(),
                current: false,
            },
        );
        let wrong = json!({"session":"A","copy":"old","lock":8,"index":0,"offset":0,"length":16});
        assert!(bridge.range(&wrong).await.is_err());
        alive.store(false, Ordering::Release);
        let correct = json!({"session":"A","copy":"old","lock":7,"index":0,"offset":0,"length":16});
        assert!(bridge.range(&correct).await.is_err());
        assert_eq!(bridge.ranges.load(Ordering::Relaxed), 0);
        assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    #[ignore = "registers a real OS domain; requires MACRDP_FILE_PROVIDER_SYSTEM_TEST_HOST"]
    async fn real_os_broker_to_extension_to_original_lock() {
        let helper = PathBuf::from(
            std::env::var_os("MACRDP_FILE_PROVIDER_SYSTEM_TEST_HOST")
                .expect("explicit staged helper path required"),
        );
        assert!(helper.is_file());
        let mut state = bridge();
        state.helper = helper;
        let bridge = Arc::new(state);
        assert!(BRIDGE.set(bridge.clone()).is_ok());
        let bytes: Vec<u8> = (0..196631).map(|n| (n % 251) as u8).collect();
        let base = DownloadRouter::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let alive = Arc::new(AtomicBool::new(true));
        let router = base.for_copy(17, alive, Arc::new(Mutex::new(Some(tx))));
        let payload = bytes.clone();
        let remote = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let ServerEvent::Clipboard(ClipboardMessage::SendFileContentsRequest(request)) =
                    event
                {
                    assert_eq!(request.data_id, Some(17));
                    let start = request.position as usize;
                    let end = (start + request.requested_size as usize).min(payload.len());
                    base.deliver(FileContentsResponse::new_data_response(
                        request.stream_id,
                        payload[start..end].to_vec(),
                    ));
                }
            }
        });
        let outcome: Result<()> = async {
            bridge.clone().start().await?;
            activate("session-A");
            let paths = offer(
                "session-A",
                &[RemoteEntry {
                    size: Some(bytes.len() as u64),
                    ..entry("broker.bin")
                }],
                router,
            )
            .await?;
            ensure!(
                bridge.ranges.load(Ordering::Relaxed) == 0,
                "metadata unexpectedly read contents"
            );
            use std::os::macos::fs::MetadataExt;
            ensure!(
                std::fs::metadata(&paths[0])?.st_flags() & 0x40000000 != 0,
                "expected a system dataless file"
            );
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                tokio::process::Command::new("/bin/cat")
                    .arg(&paths[0])
                    .output(),
            )
            .await??;
            ensure!(
                result.status.success() && result.stdout == bytes,
                "direct OS read did not match remote bytes"
            );
            ensure!(
                bridge.ranges.load(Ordering::Relaxed) > 0,
                "missing real broker RANGE"
            );
            Ok(())
        }
        .await;
        shutdown().await;
        remote.abort();
        let _ = remote.await;
        outcome.unwrap();
    }
}
