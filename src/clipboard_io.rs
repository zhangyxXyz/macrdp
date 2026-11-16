//! Bounded blocking clipboard reads. File Provider hydration must be able to
//! re-enter the RDP channel while an outgoing file read waits for its bytes.
use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_server::ServerEvent;
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};
pub type Sender = Arc<Mutex<Option<mpsc::UnboundedSender<ServerEvent>>>>;

struct Budget {
    queued: Arc<Semaphore>,
    workers: Arc<Semaphore>,
}
fn send(
    sender: &Sender,
    active: Arc<AtomicBool>,
    message: ClipboardMessage,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) {
    if let Some(tx) = sender.lock().unwrap().as_ref() {
        // Check again at dispatch, not only here: a worker can finish after
        // the old connection's global event queue has already been drained.
        let _ = tx.send(ServerEvent::SessionClipboard {
            active,
            message,
            permit,
        });
    }
}
pub fn schedule(
    sender: Sender,
    active: Arc<AtomicBool>,
    fallback: ClipboardMessage,
    operation: impl FnOnce() -> ClipboardMessage + Send + 'static,
) {
    #[cfg(all(target_os = "macos", test))]
    let operation = crate::clipboard::inherit_test_pasteboard(operation);
    static BUDGET: OnceLock<Arc<Budget>> = OnceLock::new();
    let budget = BUDGET
        .get_or_init(|| {
            Arc::new(Budget {
                queued: Arc::new(Semaphore::new(32)),
                workers: Arc::new(Semaphore::new(4)),
            })
        })
        .clone();
    schedule_with(
        budget,
        sender,
        active,
        fallback,
        Duration::from_secs(30),
        operation,
    );
}
fn schedule_with(
    budget: Arc<Budget>,
    sender: Sender,
    active: Arc<AtomicBool>,
    fallback: ClipboardMessage,
    timeout: Duration,
    operation: impl FnOnce() -> ClipboardMessage + Send + 'static,
) {
    let Ok(queue) = budget.queued.clone().try_acquire_owned() else {
        tracing::warn!("outgoing clipboard I/O queue limit reached");
        send(&sender, active, fallback, None);
        return;
    };
    tokio::spawn(async move {
        let result = tokio::time::timeout(timeout, async {
            let worker = budget.workers.clone().acquire_owned().await.ok()?;
            if !active.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            tokio::task::spawn_blocking(move || {
                // Retain the process-wide worker permit even after timeout or
                // disconnect: an OS hydration/read syscall cannot be cancelled.
                let _worker = worker;
                operation()
            })
            .await
            .ok()
        })
        .await;
        if result.is_err() {
            tracing::warn!(
                timeout_ms = timeout.as_millis() as u64,
                "outgoing clipboard I/O exceeded its budget"
            );
        }
        let result = result.ok().flatten();
        send(&sender, active, result.unwrap_or(fallback), Some(queue));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn error() -> ClipboardMessage {
        ClipboardMessage::SendFileContentsResponse(
            ironrdp_cliprdr::pdu::FileContentsResponse::new_error(42),
        )
    }
    #[tokio::test]
    async fn stalled_read_does_not_block_runtime_and_times_out_once() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let done = Arc::new(AtomicBool::new(false));
        let release = done.clone();
        let budget = Arc::new(Budget {
            queued: Arc::new(Semaphore::new(2)),
            workers: Arc::new(Semaphore::new(1)),
        });
        schedule_with(
            budget.clone(),
            Arc::new(Mutex::new(Some(tx))),
            Arc::new(AtomicBool::new(true)),
            error(),
            Duration::from_millis(20),
            move || {
                while !release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                error()
            },
        );
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        done.store(true, Ordering::Release);
        assert!(matches!(event, ServerEvent::SessionClipboard { .. }));
        assert!(
            rx.recv().await.is_none(),
            "only one completion, then sender closes"
        );
    }
    #[tokio::test]
    async fn disconnected_queued_request_never_runs_io() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let budget = Arc::new(Budget {
            queued: Arc::new(Semaphore::new(2)),
            workers: Arc::new(Semaphore::new(1)),
        });
        schedule_with(
            budget,
            Arc::new(Mutex::new(Some(tx))),
            Arc::new(AtomicBool::new(false)),
            error(),
            Duration::from_secs(1),
            move || {
                called.fetch_add(1, Ordering::SeqCst);
                error()
            },
        );
        let event = rx.recv().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let ServerEvent::SessionClipboard { active, .. } = event else {
            panic!("missing connection scope")
        };
        assert!(!active.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn timed_out_os_read_keeps_worker_slot_and_cannot_reply_twice() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = Arc::new(Mutex::new(Some(tx)));
        let active = Arc::new(AtomicBool::new(true));
        let budget = Arc::new(Budget {
            queued: Arc::new(Semaphore::new(2)),
            workers: Arc::new(Semaphore::new(1)),
        });
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        schedule_with(
            budget.clone(),
            sender.clone(),
            active.clone(),
            error(),
            Duration::from_millis(100),
            move || {
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
                error()
            },
        );
        entered_rx.await.unwrap();
        let timed_out = rx.recv().await.unwrap();
        assert_eq!(budget.workers.available_permits(), 0);
        assert_eq!(budget.queued.available_permits(), 1);
        drop(timed_out);
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        schedule_with(
            budget.clone(),
            sender.clone(),
            active,
            error(),
            Duration::from_millis(20),
            move || {
                called.fetch_add(1, Ordering::SeqCst);
                error()
            },
        );
        drop(rx.recv().await.unwrap());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "timeout must not free a still-blocked OS worker"
        );
        release_tx.send(()).unwrap();
        let worker = tokio::time::timeout(Duration::from_secs(2), budget.workers.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(worker);
        drop(sender);
        assert!(
            rx.recv().await.is_none(),
            "the late OS result must not send a second reply"
        );
        assert_eq!(budget.queued.available_permits(), 2);
    }
}
