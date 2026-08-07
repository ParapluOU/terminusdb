//! End-to-end Proof-of-WOQL through terminusdb's store stack.
//!
//! Builds a base layer of triples through the SAME `terminus_store` store API that
//! terminusdb-store-prolog's foreign predicates wrap (`create_base_layer` ->
//! `add_value_triple` -> `commit_with_proof`), materializes its authenticated
//! `LayerCommitment` (`proof_commitment`), and then drives the fork's Proof-of-WOQL
//! query API (`prove/verify_layer_graph_pattern2`, `prove/verify_layer_filter`) to
//! produce AND verify a proof against the layer's committed root. This demonstrates
//! the whole pipeline is real through terminusdb's dependency on the audited store
//! fork. (Mapping a full WOQL query string to a proof is a later phase.)

use std::sync::Arc;

use terminus_store::layer::ValueTriple;
use terminus_store::proof::commitment::LayerCommitment;
use terminus_store::proof::join::{compute_self_join, self_join_commitments};
use terminus_store::proof::query::{
    prove_layer_filter, prove_layer_graph_pattern2, verify_layer_filter,
    verify_layer_graph_pattern2,
};
use terminus_store::store::sync::open_sync_memory_store;

use dory_pcs::backends::arkworks::ArkFr;
use dory_pcs::primitives::arithmetic::Field;

const KNOWS: &[u8] = b"http://ex/knows";

fn iri(suffix: &str) -> String {
    format!("http://ex/{suffix}")
}

/// Build + commit a base layer of node triples through terminusdb's store stack,
/// returning its authenticated per-layer commitment.
fn build_layer(triples: &[(&str, &str, &str)]) -> Arc<LayerCommitment> {
    let store = open_sync_memory_store();
    let builder = store.create_base_layer().unwrap();
    for (s, p, o) in triples {
        builder
            .add_value_triple(ValueTriple::new_node(&iri(s), &iri(p), &iri(o)))
            .unwrap();
    }
    let (layer, _state) = builder.commit_with_proof().unwrap();
    layer.proof_commitment().unwrap().unwrap()
}

#[test]
fn e2e_two_hop_knows_graph_pattern_through_terminusdb_store() {
    // knows: a->b, b->c, c->d, a->e (e a sink); one unrelated "likes" edge. The 2-hop
    // pattern t(?x,knows,?y),t(?y,knows,?z) has exactly the paths (a,b,c) and (b,c,d).
    let lc = build_layer(&[
        ("a", "knows", "b"),
        ("b", "knows", "c"),
        ("c", "knows", "d"),
        ("a", "knows", "e"),
        ("a", "likes", "zz"),
    ]);

    // Produce the proof through the fork's query API.
    let (proof, com_x, com_y, com_z, m_j, id) = prove_layer_graph_pattern2(&lc, KNOWS).unwrap();

    // Verify, anchored to the layer's committed root as the externally-trusted root.
    // The verifier sees only the IRI + revealed id + result commitments + the root.
    verify_layer_graph_pattern2(
        &lc,
        lc.state.commitment_root,
        KNOWS,
        id,
        &com_x,
        &com_y,
        &com_z,
        m_j,
        &proof,
    )
    .unwrap();

    // Exactly two 2-hop paths.
    assert_eq!(m_j, 2);

    // Independently reconstruct the complete self-join over F = the predicate-"knows"
    // (subject, object) pairs (in the layer's stored order) and assert the returned
    // result commitments are EXACTLY that hand-computed set (count + contents).
    let mut sf = Vec::new();
    let mut of = Vec::new();
    for record in &lc.additions {
        if record.predicate_id == id {
            sf.push(ArkFr::from_u64(record.subject_id));
            of.push(ArkFr::from_u64(record.object_id));
        }
    }
    let (x, y, z, rho_l, rho_r, u, w_l, w_r, expected_m) = compute_self_join(&sf, &of);
    assert_eq!(expected_m, m_j);
    let expected = self_join_commitments(
        &sf, &of, &x, &y, &z, &rho_l, &rho_r, &u, &w_l, &w_r,
    )
    .unwrap();
    assert_eq!(expected.x, com_x);
    assert_eq!(expected.y, com_y);
    assert_eq!(expected.z, com_z);

    // A wrong externally-trusted root must reject (external-root anchoring).
    let mut wrong_root = lc.state.commitment_root;
    wrong_root[0] ^= 0xff;
    assert!(verify_layer_graph_pattern2(
        &lc, wrong_root, KNOWS, id, &com_x, &com_y, &com_z, m_j, &proof
    )
    .is_err());
}

#[test]
fn e2e_single_pattern_filter_through_terminusdb_store() {
    // The single-atom path t(?s, knows, ?o): two "knows" triples, one unrelated edge.
    let lc = build_layer(&[
        ("a", "knows", "b"),
        ("b", "knows", "c"),
        ("x", "likes", "y"),
    ]);
    let (proof, com_sout, com_oout, m, id) = prove_layer_filter(&lc, KNOWS).unwrap();
    verify_layer_filter(
        &lc,
        lc.state.commitment_root,
        KNOWS,
        id,
        &com_sout,
        &com_oout,
        m,
        &proof,
    )
    .unwrap();
    assert_eq!(m, 2);
}
