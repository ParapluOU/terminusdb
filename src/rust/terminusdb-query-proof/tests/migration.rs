use terminus_store::layer::{Layer, ValueTriple};
use terminus_store::store::sync::{
    open_sync_archive_store, open_sync_memory_store, SyncStoreLayer,
};
use terminusdb_query_proof::{migrate_legacy_chain, MigrationStatus};

fn base(store: &terminus_store::store::sync::SyncStore, subject: &str) -> SyncStoreLayer {
    let builder = store.create_base_layer().unwrap();
    builder
        .add_value_triple(ValueTriple::new_node(subject, "http://ex/p", "http://ex/o"))
        .unwrap();
    builder.commit().unwrap()
}

#[test]
fn exact_layer_migration_is_idempotent_and_never_substitutes_another_layer() {
    let store = open_sync_memory_store();
    let first = base(&store, "http://ex/first");
    let second = base(&store, "http://ex/second");
    let first_root = first.proof_commitment().unwrap().state.commitment_root;
    let second_root = second.proof_commitment().unwrap().state.commitment_root;
    assert_ne!(first_root, second_root);

    let outcome = migrate_legacy_chain(&store, first.name()).unwrap();
    assert_eq!(outcome.layer_id, first.name());
    assert_eq!(outcome.trusted_root, first_root);
    assert_eq!(outcome.status, MigrationStatus::AlreadyCurrent);

    let repeated = migrate_legacy_chain(&store, first.name()).unwrap();
    assert_eq!(repeated, outcome);
    assert_ne!(repeated.trusted_root, second_root);
}

#[test]
fn missing_exact_layer_is_an_explicit_error() {
    let store = open_sync_memory_store();
    let error = migrate_legacy_chain(&store, [u32::MAX; 5]).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn archive_result_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_sync_archive_store(directory.path(), 8);
    let layer = base(&store, "http://ex/archive");
    let id = layer.name();
    let first = migrate_legacy_chain(&store, id).unwrap();
    drop(layer);
    drop(store);

    let reopened = open_sync_archive_store(directory.path(), 8);
    let second = migrate_legacy_chain(&reopened, id).unwrap();
    assert_eq!(second.layer_id, id);
    assert_eq!(second.trusted_root, first.trusted_root);
    assert_eq!(second.status, MigrationStatus::AlreadyCurrent);
}
