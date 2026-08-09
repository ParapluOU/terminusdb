use tdb_succinct::TdbDataType;
use terminus_store::proof::query::CanonicalResultValue;
use terminus_store::proof::CanonicalObject;
use terminus_store::store::sync::{open_sync_archive_store, open_sync_memory_store};
use terminus_store::{Layer, ValueTriple};
use terminusdb_query_proof::{
    compile, compile_json, decode_and_verify_compact_envelope, decode_and_verify_envelope,
    decode_verify_executed_envelope, encode_envelope, encode_executed_envelope,
    encode_verifier_commitment, prove, QueryBlitzarConfig, QueryProver, QueryProverConfigError,
    QueryProverTier,
};
use terminusdb_schema::{GraphType, XSDAnySimpleType};
use terminusdb_woql2::compare::Gte;
use terminusdb_woql2::control::Select;
use terminusdb_woql2::control::WoqlOptional;
use terminusdb_woql2::misc::{Count, RangeMax, RangeMin};
use terminusdb_woql2::order::GroupBy;
use terminusdb_woql2::query::{And, Not, Or, Query};
use terminusdb_woql2::triple::{Data, Triple};
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

fn predicate_triple(subject: &str, predicate: &str, object: &str) -> Query {
    Query::Triple(Triple {
        subject: NodeValue::Variable(subject.into()),
        predicate: NodeValue::Variable(predicate.into()),
        object: Value::Variable(object.into()),
        graph: None,
    })
}

#[test]
fn owned_cpu_prover_matches_the_cpu_convenience() {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (subject, object) in [("a", "b"), ("b", "c")] {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(subject),
                &iri("knows"),
                &iri(object),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let compiled = compile(&triple("subject", "knows", "object")).unwrap();
    let root = layer.proof_commitment().unwrap().state.commitment_root;
    let convenience = prove(&layer, &compiled).unwrap();
    let owned = QueryProver::cpu().prove(&layer, &compiled).unwrap();
    assert_eq!(
        encode_envelope(&layer, &compiled, &convenience, root).unwrap(),
        encode_envelope(&layer, &compiled, &owned, root).unwrap(),
    );
}

#[cfg(not(feature = "blitzar-cpu"))]
#[test]
fn disabled_cpu_acceleration_fails_closed() {
    let error = QueryProver::blitzar_cpu(
        QueryProverTier::Log20,
        QueryBlitzarConfig {
            num_precomputed_generators: 0,
        },
    )
    .err()
    .unwrap();
    assert!(matches!(
        error,
        QueryProverConfigError::FeatureUnavailable("blitzar-cpu")
    ));
}

#[cfg(not(feature = "blitzar-gpu"))]
#[test]
fn disabled_gpu_acceleration_fails_closed() {
    let error = QueryProver::blitzar_gpu(
        QueryProverTier::Log20,
        QueryBlitzarConfig {
            num_precomputed_generators: 0,
        },
    )
    .err()
    .unwrap();
    assert!(matches!(
        error,
        QueryProverConfigError::FeatureUnavailable("blitzar-gpu")
    ));
}

#[cfg(all(feature = "blitzar-cpu", feature = "blitzar-gpu"))]
#[test]
fn conflicting_acceleration_features_fail_closed() {
    let config = QueryBlitzarConfig {
        num_precomputed_generators: 0,
    };
    for result in [
        QueryProver::blitzar_cpu(QueryProverTier::Log20, config),
        QueryProver::blitzar_gpu(QueryProverTier::Log20, config),
    ] {
        assert!(matches!(
            result,
            Err(QueryProverConfigError::ConflictingBackends)
        ));
    }
}

#[cfg(all(feature = "blitzar-cpu", not(feature = "blitzar-gpu")))]
#[test]
fn owned_blitzar_cpu_prover_preserves_envelope_bytes() {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (subject, object) in [("a", "b"), ("b", "c")] {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(subject),
                &iri("knows"),
                &iri(object),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let compiled = compile(&triple("subject", "knows", "object")).unwrap();
    let root = layer.proof_commitment().unwrap().state.commitment_root;
    let cpu = QueryProver::cpu().prove(&layer, &compiled).unwrap();
    let accelerated = QueryProver::blitzar_cpu(
        QueryProverTier::Log20,
        QueryBlitzarConfig {
            num_precomputed_generators: 0,
        },
    )
    .unwrap()
    .prove(&layer, &compiled)
    .unwrap();
    assert_eq!(
        encode_envelope(&layer, &compiled, &cpu, root).unwrap(),
        encode_envelope(&layer, &compiled, &accelerated, root).unwrap(),
    );
}

fn live_grouped_extrema(max: bool) -> Query {
    let child = Query::And(And {
        and: vec![
            Query::Data(Data {
                subject: NodeValue::Variable("category".into()),
                predicate: NodeValue::Node(iri("score")),
                object: DataValue::Variable("score".into()),
                graph: GraphType::Instance,
            }),
            Query::Gte(Gte {
                left: DataValue::Variable("score".into()),
                right: DataValue::Data(XSDAnySimpleType::UnsignedInt(0)),
            }),
        ],
    });
    let group = Query::GroupBy(GroupBy {
        template: Value::Variable("score".into()),
        group_by: vec!["category".into()],
        grouped_value: Value::Variable("scores".into()),
        query: Box::new(child),
    });
    let range = if max {
        Query::RangeMax(RangeMax {
            list: DataValue::Variable("scores".into()),
            result: DataValue::Variable("extremum".into()),
        })
    } else {
        Query::RangeMin(RangeMin {
            list: DataValue::Variable("scores".into()),
            result: DataValue::Variable("extremum".into()),
        })
    };
    Query::Select(Select {
        variables: vec!["category".into(), "extremum".into()],
        query: Box::new(Query::And(And {
            and: vec![group, range],
        })),
    })
}

#[test]
fn canonical_json_grouped_extrema_proves_and_crosses_execution_boundary() {
    let existing = triple("s", "p", "o").to_woql_json().to_string();
    assert_eq!(compile_json(&existing).unwrap().relation.schema, ["s", "o"]);
    for max in [false, true] {
        let query = live_grouped_extrema(max);
        let compiled = compile(&query).unwrap();
        assert_eq!(compiled.relation.schema, ["category", "extremum"]);
        let json = query.to_woql_json().to_string();
        assert_eq!(
            compile_json(&json).unwrap().relation.schema,
            compiled.relation.schema
        );

        let mut malformed: serde_json::Value = serde_json::from_str(&json).unwrap();
        malformed["query"]["and"][1]["@type"] = serde_json::json!("RangeMedian");
        assert!(compile_json(&malformed.to_string()).is_err());
        let mut missing: serde_json::Value = serde_json::from_str(&json).unwrap();
        missing["query"]["and"][1]
            .as_object_mut()
            .unwrap()
            .remove("result");
        assert!(compile_json(&missing.to_string()).is_err());
        let mut unknown: serde_json::Value = serde_json::from_str(&json).unwrap();
        unknown["query"]["and"][1]["forged"] = serde_json::json!(true);
        assert!(compile_json(&unknown.to_string()).is_err());
    }

    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (category, score) in [("c1", 30u32), ("c1", 10), ("c2", 5), ("c2", 20)] {
        builder
            .add_value_triple(ValueTriple::new_value(
                &iri(category),
                &iri("score"),
                u32::make_entry(&score),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let commitment = layer.proof_commitment().unwrap();
    let query_json = live_grouped_extrema(false).to_woql_json().to_string();
    let id_node = |name: &str| {
        commitment
            .node_value_catalog
            .iter()
            .find(|r| r.triple.object == CanonicalObject::Node(iri(name).into_bytes()))
            .unwrap()
            .object_id
    };
    let id_u32 = |value: u32| {
        commitment
            .node_value_catalog
            .iter()
            .find(|r| match &r.triple.object {
                CanonicalObject::Value { datatype, lexical } => {
                    *datatype == terminus_store::proof::CanonicalDatatype::UInt32
                        && lexical.as_slice() == value.to_be_bytes()
                }
                _ => false,
            })
            .unwrap()
            .object_id
    };
    let variables = vec!["category".into(), "extremum".into()];
    let rows = vec![
        vec![id_node("c2"), id_u32(5)],
        vec![id_node("c1"), id_u32(10)],
    ];
    let envelope = terminusdb_query_proof::prove_executed_and_encode(
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows,
    )
    .unwrap();
    decode_verify_executed_envelope(
        &envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows,
    )
    .unwrap();
    let compact = encode_verifier_commitment(&layer).unwrap();
    let compiled = compile_json(&query_json).unwrap();
    decode_and_verify_compact_envelope(
        &envelope,
        &compact,
        &compiled,
        commitment.state.commitment_root,
    )
    .unwrap();
}

#[test]
fn compact_verifier_artifact_is_smaller_than_pf4_for_materialized_data() {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for index in 0..64 {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(&format!("s{index}")),
                &iri("p"),
                &iri(&format!("o{index}")),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let full = layer.proof_commitment().unwrap().encode().unwrap();
    let compact = encode_verifier_commitment(&layer).unwrap();
    eprintln!("PF4={} compact={}", full.len(), compact.len());
    assert!(compact.len() < full.len());
}

#[test]
fn native_boundary_accepts_predicate_ids_and_rejects_dropped_rows() {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (subject, predicate, object) in [("a", "knows", "b"), ("c", "likes", "d")] {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(subject),
                &iri(predicate),
                &iri(object),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let commitment = layer.proof_commitment().unwrap();
    let query_json = predicate_triple("s", "p", "o").to_woql_json().to_string();
    let variables = vec!["s".into(), "p".into(), "o".into()];
    let rows: Vec<Vec<u64>> = commitment
        .materialized
        .iter()
        .map(|record| vec![record.subject_id, record.predicate_id, record.object_id])
        .collect();

    let envelope = terminusdb_query_proof::prove_executed_and_encode(
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows,
    )
    .unwrap();
    decode_verify_executed_envelope(
        &envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows,
    )
    .unwrap();

    assert!(decode_verify_executed_envelope(
        &envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows[..1],
    )
    .is_err());
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
    let layer = builder.commit().unwrap();

    let bgp = Query::And(And {
        and: vec![triple("x", "knows", "y"), triple("y", "knows", "z")],
    });
    // Exercise the node's normalized JSON boundary before the expensive proof work so
    // deserialization failures remain cheap and local.
    let query_json = bgp.to_woql_json().to_string();
    let executed_compiled = compile_json(&query_json).unwrap();
    assert_eq!(executed_compiled.relation.schema, vec!["x", "y", "z"]);
    let query = Query::Count(Count {
        query: Box::new(bgp.clone()),
        count: DataValue::Variable("count".into()),
    });

    // These are deliberately separate calls: merely compiling or executing WOQL does
    // not start proving. The app/domain layer decides when `prove` is invoked.
    let compiled = compile(&query).unwrap();
    let proved = prove(&layer, &compiled).unwrap();
    assert_eq!(compiled.relation.schema, vec!["x", "y", "z"]);
    assert_eq!(proved.result_len, 2);
    assert_eq!(proved.result_columns.len(), 3);
    assert!(proved
        .result_columns
        .iter()
        .all(|column| column.len() == proved.result_len));
    assert_eq!(proved.count_binding(&compiled), Some(("count", 2)));
    assert_eq!(proved.projected_commitments(&compiled).count(), 0);
    assert_eq!(proved.projected_columns(&compiled).count(), 0);

    let commitment = layer.proof_commitment().unwrap();
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

    // The running node receives this normalized JSON and executes the same BGP through
    // Prolog. Its raw bindings are resolved to IDs before JSON-LD prefix rendering.
    let id = |name: &str| {
        commitment
            .node_value_catalog
            .iter()
            .find(|record| record.triple.object == CanonicalObject::Node(iri(name).into_bytes()))
            .unwrap()
            .object_id
    };
    let variables = vec!["x".into(), "y".into(), "z".into()];
    // Deliberately reverse the ordinary executor's row order; WOQL result order is not
    // semantic, while duplicate multiplicity remains exact.
    let executed_rows = vec![
        vec![id("b"), id("c"), id("d")],
        vec![id("a"), id("b"), id("c")],
    ];
    let executed_envelope = encode_executed_envelope(
        &layer,
        &executed_compiled,
        &proved,
        commitment.state.commitment_root,
        &variables,
        &executed_rows,
    )
    .unwrap();
    decode_verify_executed_envelope(
        &executed_envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &executed_rows,
    )
    .unwrap();

    let duplicate_rows = vec![executed_rows[0].clone(), executed_rows[0].clone()];
    assert!(decode_verify_executed_envelope(
        &executed_envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &duplicate_rows,
    )
    .is_err());

    let mut forged_rows = executed_rows.clone();
    forged_rows[0][2] = id("z");
    assert!(decode_verify_executed_envelope(
        &executed_envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &forged_rows,
    )
    .is_err());

    // Count is delivered as its ordinary numeric binding, not as a Store dictionary ID.
    // The envelope remains the complete BGP proof and authenticates its result length.
    let count_query_json = query.to_woql_json().to_string();
    let count_variables = vec!["count".into()];
    let count_rows = vec![vec![2]];
    let count_envelope = encode_executed_envelope(
        &layer,
        &compiled,
        &proved,
        commitment.state.commitment_root,
        &count_variables,
        &count_rows,
    )
    .unwrap();
    decode_verify_executed_envelope(
        &count_envelope,
        &layer,
        &count_query_json,
        commitment.state.commitment_root,
        &count_variables,
        &count_rows,
    )
    .unwrap();
    assert!(decode_verify_executed_envelope(
        &count_envelope,
        &layer,
        &count_query_json,
        commitment.state.commitment_root,
        &count_variables,
        &[vec![1]],
    )
    .is_err());

    let mut omitted_row = proved.result_columns.clone();
    omitted_row[0].pop();
    assert!(proved.verify_columns(&omitted_row).is_err());

    let mut wrong_root = commitment.state.commitment_root;
    wrong_root[0] ^= 0xff;
    assert!(proved.verify(&compiled, &commitment, wrong_root).is_err());
}

#[test]
fn native_boundary_round_trips_correlated_not_rows() {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (subject, predicate, object) in [
        ("a", "p", "b"),
        ("c", "p", "d"),
        ("e", "p", "b"),
        ("b", "q", "a"),
        ("b", "q", "e"),
        ("u", "r", "v"),
    ] {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(subject),
                &iri(predicate),
                &iri(object),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let commitment = layer.proof_commitment().unwrap();
    let query = Query::And(And {
        and: vec![
            triple("x", "p", "y"),
            Query::Not(Not {
                // Both outer variables are correlated in reversed right-schema order;
                // the negated child is itself an exact bag union.
                query: Box::new(Query::Or(Or {
                    or: vec![triple("y", "q", "x"), triple("y", "r", "x")],
                })),
            }),
        ],
    });
    let query_json = query.to_woql_json().to_string();
    let compiled = compile_json(&query_json).unwrap();
    assert!(compiled.relation.as_left_anti().is_some());
    let id = |name: &str| {
        commitment
            .node_value_catalog
            .iter()
            .find(|record| record.triple.object == CanonicalObject::Node(iri(name).into_bytes()))
            .unwrap()
            .object_id
    };
    let variables = vec!["x".into(), "y".into()];
    let rows = vec![vec![id("c"), id("d")]];
    let envelope = terminusdb_query_proof::prove_executed_and_encode(
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows,
    )
    .unwrap();
    let verified = decode_verify_executed_envelope(
        &envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &rows,
    )
    .unwrap();
    assert_eq!(verified.proved.result_len, 1);

    let forged = vec![vec![id("a"), id("b")]];
    assert!(decode_verify_executed_envelope(
        &envelope,
        &layer,
        &query_json,
        commitment.state.commitment_root,
        &variables,
        &forged,
    )
    .is_err());

    // With no shared variables, Prolog negation is one authenticated global gate. The
    // nonempty q relation suppresses the complete p relation.
    let global = Query::And(And {
        and: vec![
            triple("x", "p", "y"),
            Query::Not(Not {
                query: Box::new(triple("a", "q", "b")),
            }),
        ],
    });
    let global_json = global.to_woql_json().to_string();
    let global_compiled = compile_json(&global_json).unwrap();
    assert!(global_compiled
        .relation
        .as_left_anti()
        .unwrap()
        .2
        .is_empty());
    let global_envelope = terminusdb_query_proof::prove_executed_and_encode(
        &layer,
        &global_json,
        commitment.state.commitment_root,
        &variables,
        &[],
    )
    .unwrap();
    decode_verify_executed_envelope(
        &global_envelope,
        &layer,
        &global_json,
        commitment.state.commitment_root,
        &variables,
        &[],
    )
    .unwrap();
}

#[test]
fn recursive_optional_then_not_envelope_survives_archive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_sync_archive_store(directory.path(), 8);
    let builder = store.create_base_layer().unwrap();
    for (subject, predicate, object) in [
        ("a", "p", "b"),
        ("c", "p", "d"),
        ("b", "q", "u"),
        ("a", "r", "blocked"),
    ] {
        builder
            .add_value_triple(ValueTriple::new_node(
                &iri(subject),
                &iri(predicate),
                &iri(object),
            ))
            .unwrap();
    }
    let layer = builder.commit().unwrap();
    let layer_id = layer.name();
    let query = Query::And(And {
        and: vec![
            triple("x", "p", "y"),
            Query::WoqlOptional(WoqlOptional {
                query: Box::new(triple("y", "q", "z")),
            }),
            Query::Not(Not {
                query: Box::new(triple("x", "r", "reason")),
            }),
        ],
    });
    let compiled = compile(&query).unwrap();
    assert!(compiled.relation.as_left_anti().is_some());
    let commitment = layer.proof_commitment().unwrap();
    let trusted_root = commitment.state.commitment_root;
    let proved = prove(&layer, &compiled).unwrap();
    assert_eq!(proved.result_len, 1);
    let envelope = encode_envelope(&layer, &compiled, &proved, trusted_root).unwrap();
    let verifier_commitment = encode_verifier_commitment(&layer).unwrap();

    drop(proved);
    drop(commitment);
    drop(layer);
    drop(store);

    let reopened = open_sync_archive_store(directory.path(), 8);
    let reopened_layer = reopened.get_layer_from_id(layer_id).unwrap().unwrap();
    let verified =
        decode_and_verify_envelope(&envelope, &reopened_layer, &compiled, trusted_root).unwrap();
    assert_eq!(verified.proved.result_len, 1);
    assert!(matches!(
        verified.result_values.as_ref().unwrap()[2][0],
        CanonicalResultValue::Null
    ));
    let compact_verified = decode_and_verify_compact_envelope(
        &envelope,
        &verifier_commitment,
        &compiled,
        trusted_root,
    )
    .unwrap();
    assert_eq!(compact_verified.proved.result_len, 1);
    let mut wrong_root = trusted_root;
    wrong_root[0] ^= 1;
    assert!(decode_and_verify_compact_envelope(
        &envelope,
        &verifier_commitment,
        &compiled,
        wrong_root,
    )
    .is_err());
    let mut tampered = verifier_commitment;
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(
        decode_and_verify_compact_envelope(&envelope, &tampered, &compiled, trusted_root).is_err()
    );
}
