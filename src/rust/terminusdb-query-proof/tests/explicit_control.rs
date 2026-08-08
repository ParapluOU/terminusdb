use terminus_store::proof::CanonicalObject;
use terminus_store::store::sync::open_sync_memory_store;
use terminus_store::ValueTriple;
use terminusdb_query_proof::{compile, decode_and_verify_envelope, encode_envelope, prove};
use terminusdb_woql2::misc::Count;
use terminusdb_woql2::query::{And, Query};
use terminusdb_woql2::triple::Triple;
use terminusdb_woql2::value::{DataValue, NodeValue, Value};

fn iri(value: &str) -> String {
    format!("http://ex/{value}")
}

fn triple(subject: &str, predicate: &str, object: &str) -> Query {
    Query::Triple(Triple {
        subject: NodeValue::Variable(subject.into()),
        predicate: NodeValue::Node(iri(predicate)),
        object: Value::Variable(object.into()),
        graph: None,
    })
}

#[test]
fn caller_explicitly_compiles_proves_and_selects_the_trusted_root() {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (subject, predicate, object) in [
        ("a", "knows", "b"),
        ("b", "knows", "c"),
        ("c", "knows", "d"),
        ("a", "likes", "z"),
    ] {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(subject),
                &iri(predicate),
                &iri(object),
            ))
            .unwrap();
    }
    let (layer, _) = builder.commit_with_proof().unwrap();

    let bgp = Query::And(And {
        and: vec![triple("x", "knows", "y"), triple("y", "knows", "z")],
    });
    let query = Query::Count(Count {
        query: Box::new(bgp),
        count: DataValue::Variable("count".into()),
    });

    // These are deliberately separate calls: merely compiling or executing WOQL does
    // not start proving. The app/domain layer decides when `prove` is invoked.
    let compiled = compile(&query).unwrap();
    let proved = prove(&layer, &compiled).unwrap();
    assert_eq!(compiled.schema, vec!["x", "y", "z"]);
    assert_eq!(proved.result_len, 2);
    assert_eq!(proved.result_columns.len(), 3);
    assert!(proved
        .result_columns
        .iter()
        .all(|column| column.len() == proved.result_len));
    assert_eq!(proved.count_binding(&compiled), Some(("count", 2)));
    assert_eq!(proved.projected_commitments(&compiled).count(), 0);
    assert_eq!(proved.projected_columns(&compiled).count(), 0);

    let commitment = layer.proof_commitment().unwrap().unwrap();
    proved
        .verify(&compiled, &commitment, commitment.state.commitment_root)
        .unwrap();

    let objects = proved
        .verify_and_resolve(&compiled, &commitment, commitment.state.commitment_root)
        .unwrap();
    assert_eq!(objects.len(), 3);
    assert!(objects.iter().flatten().all(|object| matches!(
        object,
        CanonicalObject::Node(iri) if iri.starts_with(b"http://ex/")
    )));

    let mut wrong_object = objects.clone();
    wrong_object[0][0] = CanonicalObject::Node(iri("forged").into_bytes());
    assert!(proved
        .verify_objects(
            &compiled,
            &commitment,
            commitment.state.commitment_root,
            &wrong_object,
        )
        .is_err());

    // Serialization and verification remain explicit caller actions. The boundary does
    // not persist, publish, schedule, or automatically generate this envelope.
    let encoded =
        encode_envelope(&layer, &compiled, &proved, commitment.state.commitment_root).unwrap();
    let decoded = decode_and_verify_envelope(
        &encoded,
        &layer,
        &compiled,
        commitment.state.commitment_root,
    )
    .unwrap();
    assert_eq!(decoded.proved.result_len, proved.result_len);
    assert_eq!(decoded.result_objects, objects);

    let mut corrupt = encoded;
    corrupt[24] ^= 1;
    assert!(decode_and_verify_envelope(
        &corrupt,
        &layer,
        &compiled,
        commitment.state.commitment_root,
    )
    .is_err());

    let mut omitted_row = proved.result_columns.clone();
    omitted_row[0].pop();
    assert!(proved.verify_columns(&omitted_row).is_err());

    let mut wrong_root = commitment.state.commitment_root;
    wrong_root[0] ^= 0xff;
    assert!(proved.verify(&compiled, &commitment, wrong_root).is_err());
}
