//! Explicit Proof-of-WOQL integration boundary for TerminusDB.
//!
//! This module deliberately contains no scheduler or background-job policy. Callers may
//! compile in advance, run the ordinary query through Prolog, and decide themselves if
//! and when to invoke [`prove`]. Verification always requires a caller-selected trusted
//! root.

use terminus_store::store::sync::SyncStoreLayer;
use terminusdb_woql2::proof::{
    decode_and_verify_envelope as decode_woql_envelope, plan_bgp, BgpPlanError, CompiledBgp,
    ProvedBgp, QueryProofError, VerifiedBgpEnvelope,
};
use terminusdb_woql2::query::Query;

/// Compile and shape-check a WOQL query without generating a proof.
pub fn compile(query: &Query) -> Result<CompiledBgp, BgpPlanError> {
    plan_bgp(query)
}

/// Generate a proof only when explicitly requested by the caller.
pub fn prove(
    layer: &SyncStoreLayer,
    compiled: &CompiledBgp,
) -> Result<ProvedBgp, QueryProofError> {
    compiled.prove_on_layer(layer)
}

/// Serialize an explicitly generated proof/result after verifying it against the
/// caller-selected root. This chooses no storage, transport, or generation policy.
pub fn encode_envelope(
    layer: &SyncStoreLayer,
    compiled: &CompiledBgp,
    proved: &ProvedBgp,
    expected_root: ProofHash,
) -> Result<Vec<u8>, QueryProofError> {
    let commitment = layer
        .proof_commitment()?
        .ok_or(QueryProofError::LayerNotProofEnabled)?;
    Ok(proved.encode_envelope(compiled, &commitment, expected_root)?)
}

/// Decode and verify a portable proof/result against an independently compiled query,
/// caller-selected layer, and trusted root.
pub fn decode_and_verify_envelope(
    bytes: &[u8],
    layer: &SyncStoreLayer,
    compiled: &CompiledBgp,
    expected_root: ProofHash,
) -> Result<VerifiedBgpEnvelope, QueryProofError> {
    let commitment = layer
        .proof_commitment()?
        .ok_or(QueryProofError::LayerNotProofEnabled)?;
    Ok(decode_woql_envelope(
        bytes,
        compiled,
        &commitment,
        expected_root,
    )?)
}

pub use terminus_store::proof::ProofHash;
pub use terminusdb_woql2::proof::QueryProofError as Error;
