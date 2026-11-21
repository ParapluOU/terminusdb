//! Tests against a synthetic store built the way TerminusDB builds one: a
//! repository graph naming a commit graph, a commit graph holding a branch and
//! a chain of commits, and instance layers carrying the triples. No server and
//! no Prolog are involved, which is the point — the feed reads layers.

use terminusdb_store_prolog::terminus_store::storage::name_to_string;
use terminusdb_store_prolog::terminus_store::store::sync::{
    open_sync_memory_store, SyncStore, SyncStoreLayer,
};
use tdb_succinct::TdbDataType;
use terminusdb_store_prolog::terminus_store::{Layer, ValueTriple};

use super::feed::{BranchKey, Feed};
use super::record;
use super::resolve;

const REF_HEAD: &str = "http://terminusdb.com/schema/ref#head";
const REF_PARENT: &str = "http://terminusdb.com/schema/ref#parent";
const REF_INSTANCE: &str = "http://terminusdb.com/schema/ref#instance";
const REF_IDENTIFIER: &str = "http://terminusdb.com/schema/ref#identifier";
const REF_MESSAGE: &str = "http://terminusdb.com/schema/ref#message";
const REPO_HEAD: &str = "http://terminusdb.com/schema/repository#head";
const LAYER_IDENTIFIER: &str = "http://terminusdb.com/schema/layer#identifier";
/// `admin|test`, as `www_form_encode/2` writes it.
const ENCODED_LABEL: &str = "admin%7ctest";

fn layer_iri(id: &str) -> String {
    format!("terminusdb://layer/data/Layer_{id}")
}

fn commit_iri(identifier: &str) -> String {
    format!("terminusdb://ref/data/ValidCommit/{identifier}")
}

/// One instance-graph commit: a child layer that adds and removes triples.
fn instance_layer(
    store: &SyncStore,
    parent: Option<&SyncStoreLayer>,
    adds: &[(&str, &str, &str)],
    removes: &[(&str, &str, &str)],
) -> SyncStoreLayer {
    let builder = match parent {
        Some(parent) => parent.open_write().unwrap(),
        None => store.create_base_layer().unwrap(),
    };
    for (s, p, o) in adds {
        builder
            .add_value_triple(ValueTriple::new_node(s, p, o))
            .unwrap();
    }
    for (s, p, o) in removes {
        builder
            .remove_value_triple(ValueTriple::new_node(s, p, o))
            .unwrap();
    }
    builder.commit().unwrap()
}

/// Build a whole database: instance layers, a commit graph over them, and a
/// repository graph naming it. Returns the store and the instance layer ids in
/// commit order.
fn database(commits: usize) -> (SyncStore, Vec<String>) {
    let store = open_sync_memory_store();

    // Instance layers, oldest first. Each commit adds two triples about its
    // own subject and removes one of its predecessor's.
    let mut instance_ids = Vec::new();
    let mut parent: Option<SyncStoreLayer> = None;
    for index in 0..commits {
        let subject = format!("terminusdb:///data/Thing/{index}");
        let previous = format!("terminusdb:///data/Thing/{}", index.saturating_sub(1));
        let adds = [
            (
                subject.as_str(),
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "terminusdb:///schema#Thing",
            ),
            (
                subject.as_str(),
                "terminusdb:///schema#next",
                previous.as_str(),
            ),
        ];
        let removes: Vec<(&str, &str, &str)> = if index == 0 {
            Vec::new()
        } else {
            vec![(
                previous.as_str(),
                "terminusdb:///schema#next",
                format!("terminusdb:///data/Thing/{}", index.saturating_sub(2)).leak(),
            )]
        };
        let layer = instance_layer(&store, parent.as_ref(), &adds, &removes);
        instance_ids.push(name_to_string(layer.name()));
        parent = Some(layer);
    }

    // The commit graph: a chain of ValidCommits, each naming its instance
    // layer, with Branch/main pointing at the newest.
    let commit_builder = store.create_base_layer().unwrap();
    for (index, instance_id) in instance_ids.iter().enumerate() {
        let identifier = format!("c{index}");
        let iri = commit_iri(&identifier);
        let layer_object = layer_iri(instance_id);
        commit_builder
            .add_value_triple(ValueTriple::new_string_value(
                &iri,
                REF_IDENTIFIER,
                &identifier,
            ))
            .unwrap();
        commit_builder
            .add_value_triple(ValueTriple::new_string_value(
                &iri,
                REF_MESSAGE,
                &format!("commit {index}"),
            ))
            .unwrap();
        commit_builder
            .add_value_triple(ValueTriple::new_node(&iri, REF_INSTANCE, &layer_object))
            .unwrap();
        commit_builder
            .add_value_triple(ValueTriple::new_string_value(
                &layer_object,
                LAYER_IDENTIFIER,
                instance_id,
            ))
            .unwrap();
        if index > 0 {
            commit_builder
                .add_value_triple(ValueTriple::new_node(
                    &iri,
                    REF_PARENT,
                    &commit_iri(&format!("c{}", index - 1)),
                ))
                .unwrap();
        }
    }
    if let Some(last) = instance_ids.len().checked_sub(1) {
        commit_builder
            .add_value_triple(ValueTriple::new_node(
                "terminusdb://ref/data/Branch/main",
                REF_HEAD,
                &commit_iri(&format!("c{last}")),
            ))
            .unwrap();
    }
    let commit_layer = commit_builder.commit().unwrap();
    let commit_layer_id = name_to_string(commit_layer.name());

    // The repository graph, under the label the database is known by.
    let repo_builder = store.create_base_layer().unwrap();
    let commit_layer_object = layer_iri(&commit_layer_id);
    repo_builder
        .add_value_triple(ValueTriple::new_node(
            "terminusdb://repository/data/Local/local",
            REPO_HEAD,
            &commit_layer_object,
        ))
        .unwrap();
    repo_builder
        .add_value_triple(ValueTriple::new_string_value(
            &commit_layer_object,
            LAYER_IDENTIFIER,
            &commit_layer_id,
        ))
        .unwrap();
    let repo_layer = repo_builder.commit().unwrap();
    // Prolog creates this label through `safe_create_named_graph/3`, which
    // www-form-encodes the name, so the test store must look the same or it
    // would not exercise the resolution the server actually needs.
    let graph = store.create(ENCODED_LABEL).unwrap();
    graph.set_head(&repo_layer).unwrap();

    (store, instance_ids)
}

fn key() -> BranchKey {
    BranchKey {
        org: "admin".to_string(),
        db: "test".to_string(),
        branch: "main".to_string(),
    }
}

#[test]
fn a_database_is_found_through_its_encoded_label_and_nothing_else_is() {
    let (store, _) = database(1);
    assert_eq!(
        vec![("admin".to_string(), "test".to_string())],
        resolve::databases(&store).unwrap()
    );
    assert_eq!(
        Some(ENCODED_LABEL.to_string()),
        resolve::find_label(&store, "admin", "test").unwrap()
    );
    assert_eq!(None, resolve::find_label(&store, "admin", "other").unwrap());
}

#[test]
fn a_label_is_matched_by_decoding_it_not_by_re_encoding_the_name() {
    // The pipe is what `www_form_encode/2` escapes here, but the feed must not
    // depend on WHICH characters it escapes: an encoder that also escaped the
    // dash, or used uppercase hex, has to keep working.
    assert_eq!(Some("admin|test".to_string()), resolve::decode_label("admin%7ctest"));
    assert_eq!(Some("admin|test".to_string()), resolve::decode_label("admin%7Ctest"));
    assert_eq!(
        Some("my org|my-db".to_string()),
        resolve::decode_label("my+org%7cmy%2ddb")
    );
    // A literal plus survives, because an encoder writes it as %2b.
    assert_eq!(Some("a+b|c".to_string()), resolve::decode_label("a%2bb%7cc"));
}

#[test]
fn offsets_run_from_the_root_of_the_chain_in_commit_order() {
    let (store, instance_ids) = database(4);
    let feed = Feed::new(store, 8, 8);
    let page = feed.poll(&key(), 0, 100, None, false).unwrap();

    assert_eq!(4, page.head);
    assert_eq!(4, page.next_offset);
    assert_eq!(Some("c0"), page.epoch.as_deref());
    assert_eq!(
        vec![0, 1, 2, 3],
        page.records.iter().map(|r| r.offset).collect::<Vec<_>>()
    );
    assert_eq!(
        vec!["c0", "c1", "c2", "c3"],
        page.records
            .iter()
            .map(|r| r.commit.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        instance_ids,
        page.records
            .iter()
            .map(|r| r.instance_layer.clone().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(Some("commit 0"), page.records[0].message.as_deref());
}

#[test]
fn a_page_is_bounded_by_limit_and_resumes_at_next_offset() {
    let (store, _) = database(5);
    let feed = Feed::new(store, 8, 8);

    let first = feed.poll(&key(), 0, 2, None, false).unwrap();
    assert_eq!(vec![0, 1], first.records.iter().map(|r| r.offset).collect::<Vec<_>>());
    assert_eq!(2, first.next_offset);
    assert_eq!(5, first.head);

    let second = feed
        .poll(&key(), first.next_offset, 2, None, false)
        .unwrap();
    assert_eq!(vec![2, 3], second.records.iter().map(|r| r.offset).collect::<Vec<_>>());
    assert_eq!(4, second.next_offset);
}

#[test]
fn polling_at_the_head_returns_nothing_and_reports_no_lag() {
    let (store, _) = database(3);
    let feed = Feed::new(store, 8, 8);
    let page = feed.poll(&key(), 3, 100, None, false).unwrap();
    assert!(page.records.is_empty());
    assert_eq!(3, page.head);
    assert_eq!(3, page.next_offset);
    assert_eq!(None, page.rewound_to);
}

#[test]
fn reading_past_the_head_is_empty_rather_than_an_error() {
    let (store, _) = database(2);
    let feed = Feed::new(store, 8, 8);
    let page = feed.poll(&key(), 99, 100, None, false).unwrap();
    assert!(page.records.is_empty());
    assert_eq!(2, page.head);
    assert_eq!(2, page.next_offset);
}

#[test]
fn counts_come_without_resolving_triples_and_payloads_only_when_asked() {
    let (store, _) = database(3);
    let feed = Feed::new(store, 8, 8);

    let cheap = feed.poll(&key(), 0, 100, None, false).unwrap();
    // Every commit adds two triples; every commit after the first removes one.
    assert_eq!(
        vec![2, 2, 2],
        cheap.records.iter().map(|r| r.added_count).collect::<Vec<_>>()
    );
    assert_eq!(
        vec![0, 1, 1],
        cheap
            .records
            .iter()
            .map(|r| r.removed_count)
            .collect::<Vec<_>>()
    );
    assert!(cheap.records.iter().all(|r| r.delta.is_none()));

    let full = feed.poll(&key(), 0, 100, None, true).unwrap();
    let delta = full.records[1].delta.as_ref().unwrap();
    assert_eq!(2, delta.added.len());
    assert_eq!(1, delta.removed.len());
    assert!(delta
        .added
        .iter()
        .any(|t| t.subject == "terminusdb:///data/Thing/1"
            && t.predicate == "terminusdb:///schema#next"
            && t.object == "terminusdb:///data/Thing/0"
            && t.node
            && t.datatype.is_none()));
}

#[test]
fn a_literal_of_any_datatype_renders_rather_than_panicking() {
    // `value_to_string` panics on anything that is not "stringy", so an
    // xsd:integer field used to take the whole request down. Every literal
    // must come back rendered, with its datatype named so a consumer can tell
    // 30 from "30".
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    builder
        .add_value_triple(ValueTriple::new_string_value(
            "terminusdb:///data/Thing/1",
            "terminusdb:///schema#name",
            "alice",
        ))
        .unwrap();
    builder
        .add_value_triple(ValueTriple::new_value(
            "terminusdb:///data/Thing/1",
            "terminusdb:///schema#age",
            i32::make_entry(&30),
        ))
        .unwrap();
    builder
        .add_value_triple(ValueTriple::new_value(
            "terminusdb:///data/Thing/1",
            "terminusdb:///schema#active",
            bool::make_entry(&true),
        ))
        .unwrap();
    let layer = builder.commit().unwrap();

    let delta = record::delta(&store, &name_to_string(layer.name())).unwrap();
    let by_predicate = |suffix: &str| {
        delta
            .added
            .iter()
            .find(|t| t.predicate.ends_with(suffix))
            .expect("every added triple is rendered")
    };
    let name = by_predicate("name");
    assert_eq!("alice", name.object);
    assert!(!name.node);
    assert_eq!(Some("String"), name.datatype.as_deref());

    let age = by_predicate("age");
    assert_eq!("30", age.object);
    assert_eq!(Some("Int32"), age.datatype.as_deref());

    let active = by_predicate("active");
    assert_eq!("true", active.object);
    assert_eq!(Some("Boolean"), active.datatype.as_deref());
}

#[test]
fn a_poll_that_names_the_wrong_commit_is_told_where_the_logs_diverge() {
    let (store, _) = database(4);
    let feed = Feed::new(store, 8, 8);

    // Honest continuation: `after` is the commit at from - 1.
    let good = feed.poll(&key(), 2, 100, Some("c1"), false).unwrap();
    assert_eq!(None, good.rewound_to);
    assert_eq!(2, good.records.len());

    // A consumer carrying a commit from some other history is rewound to the
    // start rather than served records whose offsets mean something else.
    let bad = feed.poll(&key(), 2, 100, Some("nonsense"), false).unwrap();
    assert_eq!(Some(0), bad.rewound_to);
    assert!(bad.records.is_empty());
    assert_eq!(0, bad.next_offset);

    // One it does know, but at a different offset: rewound to just after it.
    let shifted = feed.poll(&key(), 4, 100, Some("c0"), false).unwrap();
    assert_eq!(Some(1), shifted.rewound_to);
}

#[test]
fn new_commits_extend_the_log_without_moving_existing_offsets() {
    let (store, _) = database(2);
    let feed = Feed::new(store.clone(), 8, 8);

    let before = feed.poll(&key(), 0, 100, None, false).unwrap();
    assert_eq!(2, before.head);

    // Append a third commit the way the server would: a new instance layer,
    // a new ValidCommit over the existing commit graph, branch head moved.
    let repo = resolve::repo_graph(&store, "admin", "test").unwrap().unwrap().0;
    let commits = resolve::commit_graph(&store, &repo).unwrap();
    let head_instance = store
        .get_layer_from_id(
            terminusdb_store_prolog::terminus_store::storage::string_to_name(
                before.records[1].instance_layer.as_deref().unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
    let new_instance = instance_layer(
        &store,
        Some(&head_instance),
        &[(
            "terminusdb:///data/Thing/9",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "terminusdb:///schema#Thing",
        )],
        &[],
    );
    let new_instance_id = name_to_string(new_instance.name());

    let builder = commits.open_write().unwrap();
    let iri = commit_iri("c2");
    let layer_object = layer_iri(&new_instance_id);
    builder
        .add_value_triple(ValueTriple::new_string_value(&iri, REF_IDENTIFIER, "c2"))
        .unwrap();
    builder
        .add_value_triple(ValueTriple::new_node(&iri, REF_INSTANCE, &layer_object))
        .unwrap();
    builder
        .add_value_triple(ValueTriple::new_string_value(
            &layer_object,
            LAYER_IDENTIFIER,
            &new_instance_id,
        ))
        .unwrap();
    builder
        .add_value_triple(ValueTriple::new_node(&iri, REF_PARENT, &commit_iri("c1")))
        .unwrap();
    builder
        .remove_value_triple(ValueTriple::new_node(
            "terminusdb://ref/data/Branch/main",
            REF_HEAD,
            &commit_iri("c1"),
        ))
        .unwrap();
    builder
        .add_value_triple(ValueTriple::new_node(
            "terminusdb://ref/data/Branch/main",
            REF_HEAD,
            &iri,
        ))
        .unwrap();
    let new_commits = builder.commit().unwrap();

    // Move the repository graph's pointer to the new commit graph layer.
    let repo_builder = repo.open_write().unwrap();
    let old_object = layer_iri(&name_to_string(commits.name()));
    let new_object = layer_iri(&name_to_string(new_commits.name()));
    repo_builder
        .remove_value_triple(ValueTriple::new_node(
            "terminusdb://repository/data/Local/local",
            REPO_HEAD,
            &old_object,
        ))
        .unwrap();
    repo_builder
        .add_value_triple(ValueTriple::new_node(
            "terminusdb://repository/data/Local/local",
            REPO_HEAD,
            &new_object,
        ))
        .unwrap();
    repo_builder
        .add_value_triple(ValueTriple::new_string_value(
            &new_object,
            LAYER_IDENTIFIER,
            &name_to_string(new_commits.name()),
        ))
        .unwrap();
    let new_repo = repo_builder.commit().unwrap();
    store
        .open(ENCODED_LABEL)
        .unwrap()
        .unwrap()
        .set_head(&new_repo)
        .unwrap();

    let after = feed.poll(&key(), 0, 100, None, false).unwrap();
    assert_eq!(3, after.head);
    assert_eq!(
        vec!["c0", "c1", "c2"],
        after
            .records
            .iter()
            .map(|r| r.commit.as_str())
            .collect::<Vec<_>>()
    );
    // The epoch did not move, and neither did the offsets already handed out.
    assert_eq!(before.epoch, after.epoch);
    assert_eq!(before.records[0].commit, after.records[0].commit);
    assert_eq!(before.records[1].commit, after.records[1].commit);

    // And an incremental poll sees only the new one.
    let tail = feed.poll(&key(), 2, 100, Some("c1"), false).unwrap();
    assert_eq!(1, tail.records.len());
    assert_eq!("c2", tail.records[0].commit);
    assert_eq!(2, tail.records[0].offset);
}

#[test]
fn an_unknown_database_or_branch_is_an_error_not_an_empty_log() {
    let (store, _) = database(1);
    let feed = Feed::new(store, 8, 8);

    let missing_db = feed
        .poll(
            &BranchKey {
                org: "admin".to_string(),
                db: "nope".to_string(),
                branch: "main".to_string(),
            },
            0,
            10,
            None,
            false,
        )
        .unwrap_err();
    assert_eq!(std::io::ErrorKind::NotFound, missing_db.kind());

    let missing_branch = feed
        .poll(
            &BranchKey {
                org: "admin".to_string(),
                db: "test".to_string(),
                branch: "nope".to_string(),
            },
            0,
            10,
            None,
            false,
        )
        .unwrap_err();
    assert_eq!(std::io::ErrorKind::NotFound, missing_branch.kind());
}

#[test]
fn warming_reports_whether_the_commit_it_was_told_about_is_visible() {
    // A commit hook fires from inside the transaction that produced it, so the
    // branch head can still be invisible. Warming must say so rather than let
    // a watermark advance past a commit the index does not contain.
    let (store, _) = database(3);
    let feed = Feed::new(store, 8, 8);
    assert!(feed.warm_through(&key(), "c2").unwrap());
    assert!(!feed.warm_through(&key(), "c3").unwrap());
    assert_eq!(3, feed.warm(&key()).unwrap());
}

#[test]
fn branches_are_discoverable() {
    let (store, _) = database(1);
    let feed = Feed::new(store, 8, 8);
    assert_eq!(vec!["main".to_string()], feed.branches("admin", "test").unwrap());
}

#[test]
fn counts_of_a_layer_that_is_not_here_are_zero_rather_than_an_error() {
    let (store, _) = database(1);
    // A well-formed but absent layer id: the commit graph can name a layer
    // this store has not fetched, and the log must still be followable.
    let absent = "0".repeat(40);
    assert_eq!((0, 0), record::counts(&store, &absent).unwrap());
    assert!(record::delta(&store, &absent).unwrap().added.is_empty());
}
