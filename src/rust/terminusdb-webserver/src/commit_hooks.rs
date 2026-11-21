//! After-commit derived-structure work, published once and fanned out.
//!
//! TerminusDB keeps growing structures that are *derived* from a commit and
//! only ever need to be eventually consistent with it: the per-layer proof
//! sidecar, the change window's per-commit IRI sets, the diff log's offset
//! index. Today each one is wired in separately and two of the three are
//! computed synchronously — the sidecar inside `Store::commit`, the change
//! window inside `validate.pl`, which also computes its changed-IRI sets on
//! the commit path. That is the same job three times, paid for at the worst
//! possible moment.
//!
//! This is the one place a commit is announced, and the one place consumers
//! attach.
//!
//! # The property that makes it safe
//!
//! **A hook may never be a correctness dependency.** Every consumer must be
//! able to derive its structure from the store on demand, because this channel
//! makes no delivery promise: the queue is bounded, a worker can die, a
//! process can restart mid-commit, and an event published before a consumer
//! registered is simply never seen. Each of those costs latency and nothing
//! else — the proof sidecar can be rebuilt with `migrate_proof_chain`, and the
//! diff log materialises lazily on read whether or not it was warmed.
//!
//! So this is a *warm-up*, not a pipeline. That is also what lets the queue be
//! bounded without being an arbitrary retention cutoff: overflow degrades to
//! "computed on next read", which is exactly the unwarmed behaviour.
//!
//! # Why the work is not done where the commit happens
//!
//! Prolog publishes and returns. It passes only what it already has in hand —
//! descriptor, branch, commit id, parent, the two layer ids — and computes
//! nothing, so a slow or failing consumer cannot lengthen a transaction or
//! fail it. Everything else happens on this module's worker threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;

/// What happened, in the terms the commit path already knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitEvent {
    pub organization: String,
    pub database: String,
    pub branch: String,
    pub commit_id: String,
    /// Absent for the first commit on a branch.
    pub parent_commit_id: Option<String>,
    /// Layer ids as hex strings, absent when the commit did not write that
    /// graph. A schema-only commit has no instance layer and vice versa.
    pub instance_layer: Option<String>,
    pub schema_layer: Option<String>,
}

impl CommitEvent {
    /// The key a watermark is kept per: one log per branch.
    pub fn branch_key(&self) -> String {
        format!("{}/{}/{}", self.organization, self.database, self.branch)
    }
}

/// A consumer of commits.
///
/// `on_commit` runs on a worker thread, off the commit path, and may block —
/// which is the point. It must be idempotent: the same event can arrive twice
/// (a retry, two publishers) and events can be missed entirely, so a consumer
/// that cannot recompute from the store does not belong here.
pub trait CommitHook: Send + Sync {
    /// Name, used in logs and in the stats endpoint.
    fn name(&self) -> &'static str;

    /// Bring this consumer's derived structure up to date with `event`.
    ///
    /// An error is logged and counted, never retried: the next commit on that
    /// branch will carry the consumer forward anyway, and a reader that
    /// arrives first derives what it needs itself.
    fn on_commit(&self, event: &CommitEvent) -> Result<(), String>;
}

/// How many events may be waiting before the oldest are dropped.
///
/// Dropping is safe (see the module docs) and is the right failure: a burst of
/// writes should not grow memory without bound, and a consumer that has fallen
/// this far behind recovers faster from the store than by replaying a queue.
const QUEUE_CAPACITY: usize = 4096;

struct Registry {
    hooks: Vec<Arc<dyn CommitHook>>,
    sender: Option<mpsc::Sender<CommitEvent>>,
}

struct Counters {
    published: AtomicU64,
    dropped: AtomicU64,
    handled: AtomicU64,
    failed: AtomicU64,
}

static COUNTERS: Counters = Counters {
    published: AtomicU64::new(0),
    dropped: AtomicU64::new(0),
    handled: AtomicU64::new(0),
    failed: AtomicU64::new(0),
};

lazy_static::lazy_static! {
    static ref REGISTRY: Mutex<Registry> = Mutex::new(Registry {
        hooks: Vec::new(),
        sender: None,
    });
    /// Per branch, the last commit every hook has finished with. A reader can
    /// compare it against a branch head to see whether derived structures have
    /// caught up — which is what a cache needs before it trusts a watermark.
    static ref APPLIED: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
    /// Depth of the queue, tracked separately because `mpsc` does not expose it.
    static ref QUEUED: AtomicU64 = AtomicU64::new(0);
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Register a consumer. Call during dylib load, before any commit can happen;
/// an event published before a hook registers is not replayed to it.
pub fn register_hook(hook: Arc<dyn CommitHook>) {
    lock(&REGISTRY).hooks.push(hook);
}

/// Start the worker. Idempotent, so a second call is a no-op rather than a
/// second drain of the same queue.
pub fn start() {
    let mut registry = lock(&REGISTRY);
    if registry.sender.is_some() {
        return;
    }
    let (sender, receiver) = mpsc::channel::<CommitEvent>();
    registry.sender = Some(sender);
    drop(registry);

    // A plain OS thread, not a tokio task: hooks are synchronous store work
    // (mmap, dictionary reads) and have no business on an async runtime.
    thread::Builder::new()
        .name("tdb-commit-hooks".to_string())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                QUEUED.fetch_sub(1, Ordering::Relaxed);
                let hooks = lock(&REGISTRY).hooks.clone();
                let mut all_ok = true;
                for hook in hooks {
                    match hook.on_commit(&event) {
                        Ok(()) => {
                            COUNTERS.handled.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(reason) => {
                            all_ok = false;
                            COUNTERS.failed.fetch_add(1, Ordering::Relaxed);
                            crate::log::log_error(format!(
                                "[commit-hook {}] {} on {}: {reason}",
                                hook.name(),
                                event.commit_id,
                                event.branch_key()
                            ));
                        }
                    }
                }
                // The watermark advances only when every hook finished, so a
                // reader that trusts it is never told a structure is ready
                // when one consumer failed to build it.
                if all_ok {
                    lock(&APPLIED).insert(event.branch_key(), event.commit_id.clone());
                }
            }
        })
        .map_err(|error| {
            crate::log::log_error(format!("[commit-hook] worker not started: {error}"));
        })
        .ok();
}

/// Announce a commit. Never blocks, never fails, and never reports back —
/// the caller is a transaction that must not wait on derived work.
pub fn publish(event: CommitEvent) {
    COUNTERS.published.fetch_add(1, Ordering::Relaxed);
    let registry = lock(&REGISTRY);
    let Some(sender) = registry.sender.as_ref() else {
        // Nobody is draining: either the worker never started or the process
        // is shutting down. Consumers derive on demand, so this is a missed
        // warm-up and not a lost write.
        COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if QUEUED.load(Ordering::Relaxed) >= QUEUE_CAPACITY as u64 {
        COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if sender.send(event).is_ok() {
        QUEUED.fetch_add(1, Ordering::Relaxed);
    } else {
        COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// The last commit every hook has finished with, for a branch. `None` means
/// nothing has been applied here since this process started — which says
/// nothing about the branch itself, only about this process's warm-up.
pub fn applied(branch_key: &str) -> Option<String> {
    lock(&APPLIED).get(branch_key).cloned()
}

/// Counters for an operator: how many events were published, dropped before
/// reaching a hook, handled, and failed inside one.
pub fn stats() -> serde_json::Value {
    let hooks: Vec<&'static str> = lock(&REGISTRY).hooks.iter().map(|h| h.name()).collect();
    serde_json::json!({
        "hooks": hooks,
        "queued": QUEUED.load(Ordering::Relaxed),
        "published": COUNTERS.published.load(Ordering::Relaxed),
        "dropped": COUNTERS.dropped.load(Ordering::Relaxed),
        "handled": COUNTERS.handled.load(Ordering::Relaxed),
        "failed": COUNTERS.failed.load(Ordering::Relaxed),
        "applied": lock(&APPLIED).clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn event(commit: &str) -> CommitEvent {
        CommitEvent {
            organization: "admin".to_string(),
            database: "people".to_string(),
            branch: "main".to_string(),
            commit_id: commit.to_string(),
            parent_commit_id: None,
            instance_layer: Some("a".repeat(40)),
            schema_layer: None,
        }
    }

    struct Recorder {
        seen: Mutex<mpsc::Sender<String>>,
        fail: bool,
    }

    impl CommitHook for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn on_commit(&self, event: &CommitEvent) -> Result<(), String> {
            lock(&self.seen).send(event.commit_id.clone()).ok();
            if self.fail {
                Err("as asked".to_string())
            } else {
                Ok(())
            }
        }
    }

    /// One process-wide registry means these cannot be independent tests; this
    /// drives the whole lifecycle in order instead of pretending otherwise.
    #[test]
    fn events_reach_hooks_and_only_a_clean_pass_advances_the_watermark() {
        let (tx, rx) = channel();
        register_hook(Arc::new(Recorder {
            seen: Mutex::new(tx),
            fail: false,
        }));
        start();
        start(); // idempotent

        publish(event("c1"));
        assert_eq!("c1", rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        // The watermark is set by the worker, so give it a moment to land.
        let key = event("c1").branch_key();
        for _ in 0..50 {
            if applied(&key).as_deref() == Some("c1") {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(Some("c1".to_string()), applied(&key));

        // A hook that fails leaves the watermark where it was: a reader must
        // not be told the structure is ready.
        let (tx, rx) = channel();
        register_hook(Arc::new(Recorder {
            seen: Mutex::new(tx),
            fail: true,
        }));
        publish(event("c2"));
        assert_eq!("c2", rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        for _ in 0..25 {
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            Some("c1".to_string()),
            applied(&key),
            "a failed hook must not advance the watermark"
        );

        let stats = stats();
        assert!(stats["published"].as_u64().unwrap() >= 2);
        assert!(stats["failed"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn a_branch_key_names_one_log() {
        assert_eq!("admin/people/main", event("c").branch_key());
    }
}
