//! A pollable diff log: TerminusDB's commits as an append-only feed.
//!
//! A reactive cache wants to follow a database the way a consumer follows a
//! Kafka topic — ask "anything after offset N?" as often as it likes, cheaply,
//! with a latency that does not grow with history. `/api/log` cannot do that:
//! it is unbounded by default and costs `O(depth)` (467 ms at depth 261,
//! growing), and it carries no diffs. The commit-graph GraphQL endpoint is
//! fast but likewise has no diffs, because the per-commit document sets
//! `ref.json` declares are never populated.
//!
//! The data was already there. Every commit is an immutable delta layer whose
//! additions and removals *are* the diff, and the commit chain is already an
//! append-only sequence. This module serves it:
//!
//! - entirely in Rust, in the webserver already running inside the process;
//! - entirely off the Prolog path, read and write both — the only Prolog
//!   involvement is one handshake at startup that hands over the store handle;
//! - at `O(triples the commit changed)` per record, and `O(1)` for a poll with
//!   nothing to report.
//!
//! See [`feed`] for why the offsets are stable and what the bounds are (caches,
//! not retention), [`resolve`] for the pure-Rust path from a database name to
//! its commit chain, [`record`] for the extraction and its measured cost, and
//! [`route`] for the HTTP surface and its deliberately coarse authorization.

use std::sync::{Arc, RwLock};

use lazy_static::lazy_static;
use swipl::prelude::*;
use terminusdb_store_prolog::store::WrappedStore;
use terminusdb_store_prolog::terminus_store::store::sync::SyncStore;

pub mod auth;
pub mod feed;
pub mod graphql;
pub mod record;
pub mod resolve;
pub mod route;

#[cfg(test)]
mod tests;

use feed::Feed;

/// Default number of branches indexed at once. Each index is a `Vec` of
/// commit metadata, so a branch costs roughly its own history in a couple of
/// hundred bytes per commit; this bounds how many of those exist, not how far
/// back any of them goes.
const DEFAULT_MAX_LOGS: usize = 1024;
/// Default number of rendered deltas retained. A delta is the expensive thing
/// to produce and the cheap thing to re-produce, which is what a cache is for.
const DEFAULT_MAX_DELTAS: usize = 256;

/// Everything the routes need once a store is attached: the log index and the
/// authorizer, which read the same store.
pub struct Attached {
    pub feed: Arc<Feed>,
    pub authorizer: Arc<auth::Authorizer>,
}

lazy_static! {
    static ref FEED: RwLock<Option<Attached>> = RwLock::new(None);
}

pub enum FeedState {
    Attached(Arc<Feed>, Arc<auth::Authorizer>),
    Detached,
}

/// The feed, if a store has been handed over.
pub fn feed_handle() -> FeedState {
    match FEED
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        Some(attached) => {
            FeedState::Attached(attached.feed.clone(), attached.authorizer.clone())
        }
        None => FeedState::Detached,
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// Attach a store to the feed. Idempotent in effect: the last store wins,
/// which is what a test that opens a fresh store in the same process needs.
pub fn attach(store: SyncStore) {
    let feed = Feed::new(
        store.clone(),
        env_usize("TERMINUSDB_FEED_MAX_LOGS", DEFAULT_MAX_LOGS),
        env_usize("TERMINUSDB_FEED_MAX_DELTAS", DEFAULT_MAX_DELTAS),
    );
    let attached = Attached {
        feed: Arc::new(feed),
        authorizer: Arc::new(auth::Authorizer::new(store)),
    };
    *FEED
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(attached);
}

predicates! {
    #[module("$difflog")]
    /// Hand the process's store to the diff-log feed.
    ///
    /// Signature: `difflog_attach_store(+Store)`.
    ///
    /// This is the feed's ONLY contact with Prolog, and it happens once, at
    /// startup, before the listener is bound. Everything after it — resolving
    /// a database, walking a chain, reading a delta, answering a query — is
    /// Rust reading immutable layers, so no amount of polling touches the
    /// Prolog solver or the write path.
    pub semidet fn difflog_attach_store(_context, store_term) {
        let store: WrappedStore = store_term.get_ex()?;
        attach((*store).clone());
        Ok(())
    }
}

/// Register the feed: its Prolog handshake predicate, and its routes with the
/// webserver.
pub fn register() {
    register_difflog_attach_store();
    terminusdb_webserver::server::register_router(route::router());
}
