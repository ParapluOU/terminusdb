//! Getting from a database name to its commit chain, in Rust, without Prolog.
//!
//! TerminusDB's own metadata is triples in ordinary layers, so the whole path
//! is readable with the `Layer` trait:
//!
//! ```text
//! label "<org>|<db>"  ──head──▶  repository graph
//!   Local/local --repository#head--▶ Layer --layer#identifier--▶ commit-graph layer id
//!                                       │
//!                                       ▼
//!                                 commit graph
//!   Branch/<branch> --ref#head--▶ Commit --ref#parent--▶ Commit --▶ ... (the log, newest first)
//!     each Commit --ref#instance--▶ Layer --layer#identifier--▶ instance layer id
//! ```
//!
//! Every lookup below is a `single_triple_sp` on a document IRI this module
//! derives from the schema's own key rules, never a scan: `Branch` is
//! `Lexical` on `name`, `Local` is `Lexical` on `name`, and `Commit`
//! subclasses are `Lexical` on `identifier`. So resolving a branch head is a
//! handful of dictionary lookups whatever the size of the database.

use std::io;

use terminusdb_store_prolog::terminus_store::storage::string_to_name;
use terminusdb_store_prolog::terminus_store::store::sync::{SyncStore, SyncStoreLayer};
use terminusdb_store_prolog::terminus_store::{Layer, ObjectType};

use crate::value::value_to_string;

const REF_HEAD: &str = "http://terminusdb.com/schema/ref#head";
const REF_PARENT: &str = "http://terminusdb.com/schema/ref#parent";
const REF_INSTANCE: &str = "http://terminusdb.com/schema/ref#instance";
const REF_SCHEMA: &str = "http://terminusdb.com/schema/ref#schema";
const REF_IDENTIFIER: &str = "http://terminusdb.com/schema/ref#identifier";
const REF_TIMESTAMP: &str = "http://terminusdb.com/schema/ref#timestamp";
const REF_AUTHOR: &str = "http://terminusdb.com/schema/ref#author";
const REF_MESSAGE: &str = "http://terminusdb.com/schema/ref#message";
const REF_BRANCH_BASE: &str = "terminusdb://ref/data/Branch/";

const REPO_HEAD: &str = "http://terminusdb.com/schema/repository#head";
const REPO_LOCAL: &str = "terminusdb://repository/data/Local/local";

const LAYER_IDENTIFIER: &str = "http://terminusdb.com/schema/layer#identifier";

/// One commit as the feed sees it: an ordering key, an identity, and the layer
/// its diff lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitEntry {
    /// The commit's own identifier (not its IRI).
    pub commit: String,
    /// The instance layer this commit produced, if it produced one. A
    /// schema-only commit has none, and is still an entry in the log — a
    /// consumer that skipped it would mis-order the ones around it.
    pub instance_layer: Option<String>,
    pub schema_layer: Option<String>,
    pub timestamp: Option<String>,
    pub author: Option<String>,
    pub message: Option<String>,
}

fn not_found(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, what.into())
}

/// The logical name of a database's repository graph, as
/// `organization_database_name/3` builds it: `<organization>|<database>`.
///
/// This is NOT the label it is stored under. Prolog reaches the store through
/// `safe_open_named_graph/3`, which www-form-encodes the name first, so
/// `admin|people` lives under `admin%7cpeople`.
pub fn logical_name(org: &str, db: &str) -> String {
    format!("{org}|{db}")
}

/// Recover a logical database name from a stored label.
///
/// We decode what is on disk rather than re-encode what we are looking for,
/// deliberately: reproducing SWI-Prolog's `www_form_encode/2` character for
/// character is a liability — differ on one escaped character and that
/// database becomes invisible to the feed with no error anywhere — whereas
/// decoding is forgiving of which characters the encoder chose to escape.
pub fn decode_label(label: &str) -> Option<String> {
    // Form encoding writes a space as `+`; a literal plus arrives as `%2b`,
    // so this substitution cannot corrupt one.
    let spaced = label.replace('+', " ");
    urlencoding::decode(&spaced).ok().map(|name| name.into_owned())
}

/// The label a given logical graph name is stored under, found by decoding
/// the store's labels rather than guessing at an encoding.
///
/// Used for graphs that are not databases — the system graph is
/// `terminusdb:///system/data`, which encodes to
/// `terminusdb%3a%2f%2f%2fsystem%2fdata`.
pub fn find_label_exact(store: &SyncStore, logical: &str) -> io::Result<Option<String>> {
    Ok(store
        .labels()?
        .into_iter()
        .find(|label| decode_label(label).as_deref() == Some(logical)))
}

/// The label a database's repository graph is actually stored under, found by
/// decoding the store's labels rather than guessing at an encoding.
pub fn find_label(store: &SyncStore, org: &str, db: &str) -> io::Result<Option<String>> {
    find_label_exact(store, &logical_name(org, db))
}

/// Follow a single-valued object property from `subject`, returning the object
/// IRI.
fn object_iri(layer: &SyncStoreLayer, subject: &str, predicate: &str) -> Option<String> {
    let s = layer.subject_id(subject)?;
    let p = layer.predicate_id(predicate)?;
    let triple = layer.single_triple_sp(s, p)?;
    layer.id_object_node(triple.object)
}

/// Follow a single-valued datatype property from `subject`, returning its
/// lexical form.
fn object_value(layer: &SyncStoreLayer, subject: &str, predicate: &str) -> Option<String> {
    let s = layer.subject_id(subject)?;
    let p = layer.predicate_id(predicate)?;
    let triple = layer.single_triple_sp(s, p)?;
    match layer.id_object(triple.object)? {
        ObjectType::Value(value) => Some(value_to_string(&value).into_owned()),
        // A property declared as a literal that holds a node is a schema
        // violation upstream of us; report the node rather than drop the fact.
        ObjectType::Node(node) => Some(node),
    }
}

/// `<something> --predicate--> layer:Layer --layer:identifier--> "<hex>"`.
fn layer_identifier(layer: &SyncStoreLayer, subject: &str, predicate: &str) -> Option<String> {
    let layer_object = object_iri(layer, subject, predicate)?;
    object_value(layer, &layer_object, LAYER_IDENTIFIER)
}

/// The repository graph behind a stored label, or `None` if the label is gone.
///
/// The label version is returned with it: it changes on every head move, which
/// is what makes a poll that has nothing to report cost one label read instead
/// of a graph walk.
pub fn repo_graph_at(
    store: &SyncStore,
    label: &str,
) -> io::Result<Option<(SyncStoreLayer, u64)>> {
    let Some(graph) = store.open(label)? else {
        return Ok(None);
    };
    let (head, version) = graph.head_version()?;
    Ok(head.map(|layer| (layer, version)))
}

/// The repository graph of a database by name, resolving its label first.
pub fn repo_graph(
    store: &SyncStore,
    org: &str,
    db: &str,
) -> io::Result<Option<(SyncStoreLayer, u64)>> {
    let Some(label) = find_label(store, org, db)? else {
        return Ok(None);
    };
    repo_graph_at(store, &label)
}

/// The commit graph of the `local` repository, read out of the repository graph.
pub fn commit_graph(store: &SyncStore, repo: &SyncStoreLayer) -> io::Result<SyncStoreLayer> {
    let id = layer_identifier(repo, REPO_LOCAL, REPO_HEAD)
        .ok_or_else(|| not_found("repository graph names no local commit graph"))?;
    store
        .get_layer_from_id(string_to_name(&id)?)?
        .ok_or_else(|| not_found(format!("commit graph layer {id} is not in this store")))
}

/// The IRI of a branch's head commit, or `None` for a branch with no commits.
pub fn branch_head(commits: &SyncStoreLayer, branch: &str) -> io::Result<Option<String>> {
    let branch_iri = format!("{REF_BRANCH_BASE}{branch}");
    if commits.subject_id(&branch_iri).is_none() {
        return Err(not_found(format!("no branch {branch} in this database")));
    }
    Ok(object_iri(commits, &branch_iri, REF_HEAD))
}

/// Read one commit document.
fn read_commit(commits: &SyncStoreLayer, iri: &str) -> CommitEntry {
    CommitEntry {
        commit: object_value(commits, iri, REF_IDENTIFIER)
            // Fall back to the IRI's own lexical tail, which for every Commit
            // subclass IS the identifier.
            .unwrap_or_else(|| iri.rsplit('/').next().unwrap_or(iri).to_string()),
        instance_layer: layer_identifier(commits, iri, REF_INSTANCE),
        schema_layer: layer_identifier(commits, iri, REF_SCHEMA),
        timestamp: object_value(commits, iri, REF_TIMESTAMP),
        author: object_value(commits, iri, REF_AUTHOR),
        message: object_value(commits, iri, REF_MESSAGE),
    }
}

/// Walk a branch's commit chain from `head` towards the root, stopping as soon
/// as a commit identifier is one the caller already knows.
///
/// Returned **oldest first**, so appending it to an existing log keeps offsets
/// stable. `reached_known` says whether the walk met `known` or ran off the
/// root: a walk that ran off the root when the caller expected to meet
/// something is how a rewritten history (reset, rebase, force-push) announces
/// itself, and the caller turns that into a new epoch rather than silently
/// renumbering offsets under a consumer.
pub fn walk_back(
    commits: &SyncStoreLayer,
    head_iri: &str,
    known: Option<&str>,
    max_steps: usize,
) -> io::Result<(Vec<CommitEntry>, bool)> {
    let mut out = Vec::new();
    let mut cursor = Some(head_iri.to_string());
    let mut steps = 0usize;
    while let Some(iri) = cursor {
        let entry = read_commit(commits, &iri);
        if Some(entry.commit.as_str()) == known {
            out.reverse();
            return Ok((out, true));
        }
        steps += 1;
        if steps > max_steps {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("commit chain exceeded {max_steps} steps while reading the log"),
            ));
        }
        cursor = object_iri(commits, &iri, REF_PARENT);
        out.push(entry);
    }
    out.reverse();
    Ok((out, known.is_none()))
}

/// Every database in this store, as `(organization, database)`.
///
/// Read from the label set, so it needs no system-graph query and no
/// transaction. Labels that do not decode to `<org>|<db>` — the system graph,
/// the built-in schema graphs, a bare `label_descriptor` store used by tests —
/// are skipped.
pub fn databases(store: &SyncStore) -> io::Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = store
        .labels()?
        .into_iter()
        .filter_map(|label| {
            let name = decode_label(&label)?;
            let (org, db) = name.split_once('|')?;
            if org.is_empty() || db.is_empty() {
                return None;
            }
            Some((org.to_string(), db.to_string()))
        })
        .collect();
    out.sort();
    Ok(out)
}
