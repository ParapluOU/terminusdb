//! The feed's GraphQL surface.
//!
//! Deliberately small, and deliberately shaped like a log consumer rather than
//! a document query: you ask for a range of offsets and you are told where the
//! log now ends, so one field gives you both the data and your lag.
//!
//! ```graphql
//! {
//!   feed(database: "admin/people", from: 0, limit: 10, triples: true) {
//!     epoch head lag nextOffset
//!     records { offset commit addedCount added { subject predicate object node } }
//!   }
//! }
//! ```
//!
//! `triples: false` (the default) is the cheap mode: offsets, commit metadata
//! and counts, with no dictionary resolution at all. A consumer follows the
//! log in that mode and asks for payloads only where it cares.

use std::sync::Arc;

use juniper::{graphql_object, EmptyMutation, EmptySubscription, FieldResult, RootNode};

use super::auth::{Authorizer, Denied, Identity};
use super::feed::{BranchKey, Feed, Page, Record};
use super::record::{Delta, FeedTriple};

pub struct FeedContext {
    pub feed: Arc<Feed>,
    pub authorizer: Arc<Authorizer>,
    pub identity: Identity,
}
impl juniper::Context for FeedContext {}

impl FeedContext {
    /// Every field that names a database goes through here first. Keeping the
    /// check at the field rather than at the route is what lets `databases`
    /// filter instead of refuse, and makes it impossible to add a field that
    /// reads a database without authorizing it.
    fn authorize(&self, org: &str, db: &str) -> FieldResult<()> {
        match self.authorizer.authorize(&self.identity, org, db) {
            Ok(()) => Ok(()),
            Err(Denied::Forbidden) => Err(format!(
                "{} may not read {org}/{db} through the feed: it needs \
                 instance_read_access and commit_read_access",
                self.identity.name()
            )
            .into()),
            Err(Denied::Unauthenticated) => Err("not authenticated".into()),
            Err(Denied::Unavailable(reason)) => Err(reason.into()),
        }
    }
}

pub type FeedSchema =
    RootNode<'static, FeedRoot, EmptyMutation<FeedContext>, EmptySubscription<FeedContext>>;

pub fn schema() -> FeedSchema {
    RootNode::new(
        FeedRoot,
        EmptyMutation::<FeedContext>::new(),
        EmptySubscription::<FeedContext>::new(),
    )
}

/// Split `"org/db"`, the form the feed names databases by.
fn split_database(database: &str) -> FieldResult<(String, String)> {
    match database.split_once('/') {
        Some((org, db)) if !org.is_empty() && !db.is_empty() => {
            Ok((org.to_string(), db.to_string()))
        }
        _ => Err(format!(
            "database must be \"<organization>/<database>\", got {database:?}"
        )
        .into()),
    }
}

/// Offsets and counts cross the wire as GraphQL `Int`, which is 32-bit
/// signed. Saturating is right for a count that cannot be represented: the
/// alternative is failing a whole page over a display value.
fn as_int(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn as_usize(value: i32) -> usize {
    usize::try_from(value).unwrap_or(0)
}

pub struct FeedRoot;

#[graphql_object(context = FeedContext)]
#[no_async]
impl FeedRoot {
    /// Databases in this store that you may read, as
    /// `organization/database`. Read from the store's labels, so it needs no
    /// transaction.
    ///
    /// Filtered rather than refused: a user sees what their capabilities
    /// cover, so this stays useful for discovery without becoming a way to
    /// enumerate databases you have no access to.
    fn databases(#[graphql(context)] context: &FeedContext) -> FieldResult<Vec<String>> {
        Ok(context
            .feed
            .databases()?
            .into_iter()
            .filter(|(org, db)| {
                context
                    .authorizer
                    .authorize(&context.identity, org, db)
                    .is_ok()
            })
            .map(|(org, db)| format!("{org}/{db}"))
            .collect())
    }

    /// Branches of a database, each of which is its own log.
    fn branches(
        #[graphql(context)] context: &FeedContext,
        database: String,
    ) -> FieldResult<Vec<String>> {
        let (org, db) = split_database(&database)?;
        context.authorize(&org, &db)?;
        Ok(context.feed.branches(&org, &db)?)
    }

    /// Read a page of a branch's diff log.
    ///
    /// Defaults live in the `arguments(...)` list rather than on each
    /// parameter: this juniper version parses them only there, and a default
    /// it does not see silently becomes a required non-null argument.
    #[graphql(arguments(
        database(description = "`organization/database`."),
        branch(
            default = "main".to_string(),
            description = "Which branch's log. Each branch is a separate sequence."
        ),
        from(
            default = 0,
            description = "First offset to read. An offset is the commit's depth from the root of \
                           its chain, so it is stable: it does not depend on when anything was \
                           cached."
        ),
        limit(
            default = 100,
            description = "How many records to return. This is the only thing that bounds the work \
                           one request does, which is what keeps a poll's latency predictable when \
                           a commit changed a great many triples."
        ),
        after(
            description = "The commit identifier you last saw at offset `from - 1`. Supply it and \
                           a rewritten history is reported through `rewoundTo` instead of being \
                           served as though the offsets still meant the same thing."
        ),
        triples(
            default = false,
            description = "Resolve each record's changed triples. Off by default: following the \
                           log and reading payloads are separate costs."
        ),
    ))]
    fn feed(
        #[graphql(context)] context: &FeedContext,
        database: String,
        branch: String,
        from: i32,
        limit: i32,
        after: Option<String>,
        triples: bool,
    ) -> FieldResult<FeedPage> {
        let (org, db) = split_database(&database)?;
        context.authorize(&org, &db)?;
        let key = BranchKey { org, db, branch };
        let limit = as_usize(limit).min(10_000);
        let page = context
            .feed
            .poll(&key, as_usize(from), limit, after.as_deref(), triples)?;
        Ok(FeedPage {
            from: as_usize(from),
            page,
        })
    }
}

pub struct FeedPage {
    from: usize,
    page: Page,
}

#[graphql_object(context = FeedContext)]
#[no_async]
impl FeedPage {
    /// Identifier of the chain's root commit: the identity of this log's
    /// history. A database recreated under the same name has a different one,
    /// which is how a consumer tells "longer" from "not the same log".
    fn epoch(&self) -> Option<&str> {
        self.page.epoch.as_deref()
    }

    /// Number of records in the log, which is the offset the next commit will
    /// take.
    fn head(&self) -> i32 {
        as_int(self.page.head)
    }

    /// How far behind the request was: `head - from`, floored at zero.
    fn lag(&self) -> i32 {
        as_int(self.page.head.saturating_sub(self.from))
    }

    /// Where to continue. Equal to `from` when there was nothing new, so a
    /// consumer can hand it straight back on the next poll.
    fn next_offset(&self) -> i32 {
        as_int(self.page.next_offset)
    }

    /// Set when `after` did not match, meaning the history was rewritten:
    /// resume from here. Exact when the commit you named is still in the log
    /// (the offset just after it); 0 when it is not, because one commit id is
    /// not enough to tell how much of your history survived.
    fn rewound_to(&self) -> Option<i32> {
        self.page.rewound_to.map(as_int)
    }

    fn records(&self) -> &[Record] {
        &self.page.records
    }
}

#[graphql_object(context = FeedContext, name = "Record")]
#[no_async]
impl Record {
    /// This commit's position in the log.
    fn offset(&self) -> i32 {
        as_int(self.offset)
    }

    /// The commit identifier, as `/api/log` reports it.
    fn commit(&self) -> &str {
        &self.commit
    }

    /// The instance layer this commit produced. Absent on a commit that
    /// changed only the schema — which still occupies an offset, because
    /// skipping it would misalign every offset after it.
    fn instance_layer(&self) -> Option<&str> {
        self.instance_layer.as_deref()
    }

    fn schema_layer(&self) -> Option<&str> {
        self.schema_layer.as_deref()
    }

    fn timestamp(&self) -> Option<&str> {
        self.timestamp.as_deref()
    }

    fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Triples this commit added. Read off the layer's header, so it is
    /// available in the cheap mode too.
    fn added_count(&self) -> i32 {
        as_int(self.added_count)
    }

    fn removed_count(&self) -> i32 {
        as_int(self.removed_count)
    }

    /// Null unless the query asked for `triples: true`.
    fn added(&self) -> Option<&[FeedTriple]> {
        self.delta.as_ref().map(|delta: &Delta| &delta.added[..])
    }

    fn removed(&self) -> Option<&[FeedTriple]> {
        self.delta.as_ref().map(|delta: &Delta| &delta.removed[..])
    }
}

#[graphql_object(context = FeedContext, name = "Triple")]
#[no_async]
impl FeedTriple {
    fn subject(&self) -> &str {
        &self.subject
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    /// The object's IRI when `node` is true, its lexical form when false.
    fn object(&self) -> &str {
        &self.object
    }

    /// True for a graph node (an IRI), false for a literal. Without it a
    /// consumer cannot tell the string "3" from the node <3>.
    fn node(&self) -> bool {
        self.node
    }

    /// The literal's datatype, null for a node. This is the store's own
    /// datatype name (`Integer`, `String`, `DateTime`), not an xsd IRI —
    /// enough to tell 30 from "30" when rebuilding a document.
    fn datatype(&self) -> Option<&str> {
        self.datatype.as_deref()
    }
}
