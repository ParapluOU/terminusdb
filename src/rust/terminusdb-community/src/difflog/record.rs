//! A layer's delta, as feed records.
//!
//! This is the whole of the per-commit work, and it is `O(triples the commit
//! changed)`: `triple_additions`/`triple_removals` walk the layer's own
//! adjacency lists (nothing of its parents), and resolution turns each id into
//! its logical term through the layer's dictionaries.
//!
//! Measured in `terminus-store`'s `delta_feed_throughput` example: the id walk
//! is ~0.02 us per triple and resolution ~1.05 us, falling to ~0.40 us with
//! the per-commit memo this module keeps — and none of those figures move with
//! the size of the database or the depth of its history.

use std::collections::HashMap;
use std::io;

use terminusdb_store_prolog::terminus_store::storage::string_to_name;
use terminusdb_store_prolog::terminus_store::store::sync::{SyncStore, SyncStoreLayer};
use terminusdb_store_prolog::terminus_store::{Layer, ObjectType};

use crate::value::value_to_json;

/// One changed triple in logical terms. Store ids are deliberately not
/// exposed: they mean nothing to a consumer and nothing across databases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedTriple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// True when the object is a graph node (an IRI), false when it is a
    /// literal. Without this a consumer cannot tell `"3"` from `<3>`.
    pub node: bool,
    /// The literal's datatype, absent for a node. This is the store's own
    /// datatype name (`Integer`, `String`, `DateTime`), not an xsd IRI: a
    /// consumer rebuilding a document needs to tell 30 from "30", and the
    /// lexical form alone does not.
    pub datatype: Option<String>,
}

/// Render a triple's object.
///
/// Literals go through `value_to_json`, which covers every `Datatype` the
/// store can hold, and a JSON string is unwrapped to its text so a string
/// literal is not delivered with quotes around it. The earlier version of
/// this used `value_to_string`, which panics on anything that is not
/// "stringy" — an `xsd:integer` field was enough to take down the request.
fn render_object(object: ObjectType) -> (String, bool, Option<String>) {
    match object {
        ObjectType::Node(node) => (node, true, None),
        ObjectType::Value(value) => {
            let datatype = format!("{:?}", value.datatype());
            let rendered = match value_to_json(&value) {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            };
            (rendered, false, Some(datatype))
        }
    }
}

/// What one commit did to the instance graph.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    pub added: Vec<FeedTriple>,
    pub removed: Vec<FeedTriple>,
}

/// Addition and removal counts for a layer, without resolving anything.
///
/// This is what lets a consumer follow the log cheaply and fetch payloads
/// only where it cares: the counts come off the layer's own headers.
pub fn counts(store: &SyncStore, layer_id: &str) -> io::Result<(usize, usize)> {
    let Some(layer) = store.get_layer_from_id(string_to_name(layer_id)?)? else {
        return Ok((0, 0));
    };
    Ok((
        layer.triple_layer_addition_count()?,
        layer.triple_layer_removal_count()?,
    ))
}

/// Resolve a layer's own additions and removals into logical terms.
pub fn delta(store: &SyncStore, layer_id: &str) -> io::Result<Delta> {
    let Some(layer) = store.get_layer_from_id(string_to_name(layer_id)?)? else {
        return Ok(Delta::default());
    };
    let mut memo = Memo::default();
    let added = resolve(&layer, layer.triple_additions()?, &mut memo);
    let removed = resolve(&layer, layer.triple_removals()?, &mut memo);
    Ok(Delta { added, removed })
}

/// Subjects and predicates repeat hard inside one commit — a document write
/// touches one subject across a dozen fields, and a class's predicates recur
/// across every document of it — so resolving each id once is worth a map.
/// Objects are not memoized: they are mostly distinct values, and a map would
/// cost more than it saved.
#[derive(Default)]
struct Memo {
    subjects: HashMap<u64, String>,
    predicates: HashMap<u64, String>,
}

fn resolve(
    layer: &SyncStoreLayer,
    triples: impl Iterator<Item = terminusdb_store_prolog::terminus_store::IdTriple>,
    memo: &mut Memo,
) -> Vec<FeedTriple> {
    let mut out = Vec::new();
    for triple in triples {
        let subject = match memo.subjects.get(&triple.subject) {
            Some(subject) => subject.clone(),
            None => {
                // A dangling id means the layer disagrees with its own
                // dictionary; skip the triple rather than invent a term for
                // it, and let the counts show the discrepancy.
                let Some(subject) = layer.id_subject(triple.subject) else {
                    continue;
                };
                memo.subjects.insert(triple.subject, subject.clone());
                subject
            }
        };
        let predicate = match memo.predicates.get(&triple.predicate) {
            Some(predicate) => predicate.clone(),
            None => {
                let Some(predicate) = layer.id_predicate(triple.predicate) else {
                    continue;
                };
                memo.predicates.insert(triple.predicate, predicate.clone());
                predicate
            }
        };
        let Some(object) = layer.id_object(triple.object) else {
            continue;
        };
        let (object, node, datatype) = render_object(object);
        out.push(FeedTriple {
            subject,
            predicate,
            object,
            node,
            datatype,
        });
    }
    out
}
