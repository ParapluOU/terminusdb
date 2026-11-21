//! The log, and what it costs to follow it.
//!
//! A branch's commit chain already *is* an append-only log: commits are
//! immutable, each names its parent, and layers are never deleted. So an
//! offset here is not a number this module invents and has to defend — it is
//! the commit's depth from the root of its chain, which is intrinsic. Drop
//! every cache in this file and rebuild from the store and a consumer's
//! offsets still mean what they meant.
//!
//! That is what makes the polling cost right. Following the log is:
//!
//! - **nothing new**: one label-version read. No layer is opened, no triple is
//!   touched. This is the case a Kafka-style consumer is in almost always, and
//!   it is why it can poll as often as it likes.
//! - **new commits**: a walk of exactly the new ones.
//! - **first sight of a branch**: one walk of its chain, once.
//!
//! and never depends on the size of the database.
//!
//! ### The two bounds here are caches, not cutoffs
//!
//! `max_logs` bounds how many branches are indexed at once and `max_deltas`
//! how many rendered deltas are retained. Both are LRU, and a miss costs a
//! re-read of data that has not moved — never a gap in what can be served. No
//! request is refused and no record becomes unreachable because something was
//! evicted, which is the difference between a cache and a retention policy.

use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lru::LruCache;
use terminusdb_store_prolog::terminus_store::store::sync::SyncStore;

use super::record::{self, Delta};
use super::resolve::{self, CommitEntry};

/// How deep a single chain walk may go before we call it a cycle. This is not
/// a retention bound: it is a guard against a malformed `ref#parent` loop,
/// which would otherwise walk forever.
const MAX_CHAIN: usize = 100_000_000;

/// Take a lock, ignoring poisoning.
///
/// A panic while one of these is held must not take the feed down for the rest
/// of the process, which is what poisoning would do: one bad layer would make
/// a branch permanently unreadable. It is sound here because nothing behind
/// these locks is left half-written — `Log` is replaced field-group by
/// field-group only after every fallible step has succeeded (see `refresh`),
/// and the caches hold independent entries — so the worst a recovered lock can
/// expose is a log that is one poll out of date.
fn ignore_poison<'a, T>(
    result: Result<MutexGuard<'a, T>, PoisonError<MutexGuard<'a, T>>>,
) -> MutexGuard<'a, T> {
    result.unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BranchKey {
    pub org: String,
    pub db: String,
    pub branch: String,
}

/// One branch's index: its commits in order, plus what we last saw of the
/// label so a poll can decide in one read whether anything happened.
#[derive(Default)]
struct Log {
    entries: Vec<CommitEntry>,
    head_commit: Option<String>,
    repo_version: Option<u64>,
    /// The label this database is stored under, which is the www-form-encoded
    /// form of its name. Cached so the fast path opens it directly instead of
    /// scanning the label set on every poll.
    label: Option<String>,
    /// Identifier of the chain's root commit. Stable for the life of the
    /// branch's history, so a consumer can tell "the same log, longer" from
    /// "a different database wearing the same name".
    epoch: Option<String>,
}

/// What a poll found.
#[derive(Debug)]
pub struct Page {
    /// Root commit identifier of the chain these offsets belong to.
    pub epoch: Option<String>,
    /// The number of records in the log: the offset the next commit will take.
    pub head: usize,
    pub records: Vec<Record>,
    pub next_offset: usize,
    /// Set when the caller passed `after` and the log disagrees: the history
    /// was rewritten under them, so offsets above this one have changed
    /// meaning and must be re-read.
    ///
    /// It is a SAFE offset to resume from, not necessarily the furthest one.
    /// If the commit the caller named is still somewhere in the log, this is
    /// the offset just after it — exact. If it is not, the log cannot tell how
    /// much of the caller's history survived (it sent one commit id, not all
    /// of them), so this is 0 and the caller re-reads. Over-rewinding costs a
    /// re-read; under-rewinding would hand out records the caller believes it
    /// has already seen.
    pub rewound_to: Option<usize>,
}

#[derive(Debug)]
pub struct Record {
    pub offset: usize,
    pub commit: String,
    pub instance_layer: Option<String>,
    pub schema_layer: Option<String>,
    pub timestamp: Option<String>,
    pub author: Option<String>,
    pub message: Option<String>,
    pub added_count: usize,
    pub removed_count: usize,
    /// Present only when the caller asked for triples. Following the log and
    /// reading payloads are separate costs, and a consumer that only needs to
    /// know *that* something changed should not pay the second one.
    pub delta: Option<Delta>,
}

pub struct Feed {
    store: SyncStore,
    logs: Mutex<LruCache<BranchKey, Arc<Mutex<Log>>>>,
    deltas: Mutex<LruCache<String, Arc<Delta>>>,
}

impl Feed {
    pub fn new(store: SyncStore, max_logs: usize, max_deltas: usize) -> Self {
        let cap = |n: usize| NonZeroUsize::new(n.max(1)).expect("max(1) is non-zero");
        Self {
            store,
            logs: Mutex::new(LruCache::new(cap(max_logs))),
            deltas: Mutex::new(LruCache::new(cap(max_deltas))),
        }
    }

    /// Databases this store can serve a feed for.
    pub fn databases(&self) -> io::Result<Vec<(String, String)>> {
        resolve::databases(&self.store)
    }

    fn log_for(&self, key: &BranchKey) -> Arc<Mutex<Log>> {
        let mut logs = ignore_poison(self.logs.lock());
        if let Some(log) = logs.get(key) {
            return log.clone();
        }
        let log = Arc::new(Mutex::new(Log::default()));
        logs.put(key.clone(), log.clone());
        log
    }

    /// Bring a branch's index up to date with the store, doing as little as the
    /// store's own bookkeeping allows.
    fn refresh(&self, key: &BranchKey, log: &mut Log) -> io::Result<()> {
        let missing = || {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no database {}/{} in this store", key.org, key.db),
            )
        };
        // The label is found by decoding the store's label set, so do it once
        // and keep it: every later poll opens it by name.
        if log.label.is_none() {
            log.label = resolve::find_label(&self.store, &key.org, &key.db)?;
        }
        let label = log.label.clone().ok_or_else(missing)?;
        let Some((repo, version)) = resolve::repo_graph_at(&self.store, &label)? else {
            // The label went away under us (a deleted database). Forget it so
            // a database recreated under the same name is found again.
            log.label = None;
            return Err(missing());
        };
        // The label version moves on every head change, so an unchanged
        // version means nothing can have been appended. This is the branch a
        // polling consumer takes almost every time.
        if log.repo_version == Some(version) && log.epoch.is_some() {
            return Ok(());
        }

        let commits = resolve::commit_graph(&self.store, &repo)?;
        let head_iri = resolve::branch_head(&commits, &key.branch)?;
        let Some(head_iri) = head_iri else {
            // A branch with no commits is a valid, empty log.
            log.repo_version = Some(version);
            return Ok(());
        };

        let (new_entries, met_known) =
            resolve::walk_back(&commits, &head_iri, log.head_commit.as_deref(), MAX_CHAIN)?;

        // Everything fallible is done. Assemble the new state and install it
        // in one go, so a failure above leaves the log exactly as it was and a
        // panic cannot strand it half-extended.
        let entries = if !met_known && log.head_commit.is_some() {
            // The walk reached the root without passing what we had, so this
            // is a different chain: rebuild rather than splice two histories
            // together and hand a consumer offsets that mean neither.
            new_entries
        } else {
            let mut entries = std::mem::take(&mut log.entries);
            entries.extend(new_entries);
            entries
        };
        log.head_commit = entries.last().map(|entry| entry.commit.clone());
        log.epoch = entries.first().map(|entry| entry.commit.clone());
        log.entries = entries;
        log.repo_version = Some(version);
        Ok(())
    }

    fn delta_for(&self, layer_id: &str) -> io::Result<Arc<Delta>> {
        if let Some(delta) = ignore_poison(self.deltas.lock()).get(layer_id) {
            return Ok(delta.clone());
        }
        let delta = Arc::new(record::delta(&self.store, layer_id)?);
        ignore_poison(self.deltas.lock()).put(layer_id.to_string(), delta.clone());
        Ok(delta)
    }

    /// Read a page of the log.
    ///
    /// `after`, when given, is the commit identifier the caller last saw at
    /// offset `from - 1`. It is how a rewritten history is detected instead of
    /// being served as if nothing happened.
    pub fn poll(
        &self,
        key: &BranchKey,
        from: usize,
        limit: usize,
        after: Option<&str>,
        triples: bool,
    ) -> io::Result<Page> {
        let log = self.log_for(key);
        let mut log = ignore_poison(log.lock());
        self.refresh(key, &mut log)?;

        if let (Some(after), true) = (after, from > 0) {
            let agrees = log
                .entries
                .get(from - 1)
                .map(|entry| entry.commit == after)
                .unwrap_or(false);
            if !agrees {
                let rewound_to = log
                    .entries
                    .iter()
                    .position(|entry| entry.commit == after)
                    .map(|position| position + 1)
                    .unwrap_or(0);
                return Ok(Page {
                    epoch: log.epoch.clone(),
                    head: log.entries.len(),
                    records: Vec::new(),
                    next_offset: rewound_to,
                    rewound_to: Some(rewound_to),
                });
            }
        }

        let head = log.entries.len();
        let start = from.min(head);
        let end = start.saturating_add(limit).min(head);
        // Copy the slice out and drop the lock before touching any layer.
        // Reading a delta is the expensive part of a poll (~1 us per changed
        // triple), and holding a branch's lock across it would serialise every
        // other consumer of that branch behind one page of payloads.
        let wanted: Vec<CommitEntry> = log.entries[start..end].to_vec();
        let epoch = log.epoch.clone();
        drop(log);

        let mut records = Vec::with_capacity(wanted.len());
        for (index, entry) in wanted.into_iter().enumerate() {
            let (added_count, removed_count) = match entry.instance_layer.as_deref() {
                Some(layer) => record::counts(&self.store, layer)?,
                None => (0, 0),
            };
            let delta = match (triples, entry.instance_layer.as_deref()) {
                (true, Some(layer)) => Some((*self.delta_for(layer)?).clone()),
                (true, None) => Some(Delta::default()),
                (false, _) => None,
            };
            records.push(Record {
                offset: start + index,
                commit: entry.commit,
                instance_layer: entry.instance_layer,
                schema_layer: entry.schema_layer,
                timestamp: entry.timestamp,
                author: entry.author,
                message: entry.message,
                added_count,
                removed_count,
                delta,
            });
        }
        Ok(Page {
            epoch,
            head,
            next_offset: end,
            records,
            rewound_to: None,
        })
    }

    /// Branch names of a database, for a consumer discovering what to follow.
    pub fn branches(&self, org: &str, db: &str) -> io::Result<Vec<String>> {
        const REF_BRANCH_BASE: &str = "terminusdb://ref/data/Branch/";
        let Some((repo, _)) = resolve::repo_graph(&self.store, org, db)? else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no database {org}/{db} in this store"),
            ));
        };
        let commits = resolve::commit_graph(&self.store, &repo)?;
        use terminusdb_store_prolog::terminus_store::Layer;
        let mut names: Vec<String> = commits
            .subjects_with_prefix(REF_BRANCH_BASE)
            .map(|(_, iri)| iri[REF_BRANCH_BASE.len()..].to_string())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Per-branch index sizes, for operators asking what the feed is holding.
    pub fn stats(&self) -> HashMap<String, usize> {
        let logs = ignore_poison(self.logs.lock());
        logs.iter()
            .map(|(key, log)| {
                let len = ignore_poison(log.lock()).entries.len();
                (format!("{}/{}/{}", key.org, key.db, key.branch), len)
            })
            .collect()
    }
}
