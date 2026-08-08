use terminus_store::store::sync::open_sync_memory_store;
use terminus_store::ValueTriple;
use terminusdb_query_proof::{compile, prove};
use terminusdb_woql2::query::{And, Query};
use terminusdb_woql2::triple::Triple;
use terminusdb_woql2::value::{NodeValue, Value};

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

    let query = Query::And(And {
        and: vec![triple("x", "knows", "y"), triple("y", "knows", "z")],
    });

    // These are deliberately separate calls: merely compiling or executing WOQL does
    // not start proving. The app/domain layer decides when `prove` is invoked.
    let compiled = compile(&query).unwrap();
    let proved = prove(&layer, &compiled).unwrap();
    assert_eq!(compiled.schema, vec!["x", "y", "z"]);
    assert_eq!(proved.result_len, 2);

    let commitment = layer.proof_commitment().unwrap().unwrap();
    proved
        .verify(&compiled, &commitment, commitment.state.commitment_root)
        .unwrap();

    let mut wrong_root = commitment.state.commitment_root;
    wrong_root[0] ^= 0xff;
    assert!(proved.verify(&compiled, &commitment, wrong_root).is_err());
}
