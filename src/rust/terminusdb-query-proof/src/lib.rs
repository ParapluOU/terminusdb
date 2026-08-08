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

/// Fail-closed errors at the JSON/executed-result node integration boundary.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionProofError {
    #[error("invalid WOQL JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Proof(#[from] QueryProofError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Compile and shape-check a WOQL query without generating a proof.
pub fn compile(query: &Query) -> Result<CompiledBgp, BgpPlanError> {
    plan_bgp(query)
}

/// Parse and compile the normalized WOQL JSON used by the running node.
pub fn compile_json(query_json: &str) -> Result<CompiledBgp, ExecutionProofError> {
    let query: Query = serde_json::from_str(query_json)?;
    Ok(compile(&query).map_err(QueryProofError::from)?)
}

/// Generate a proof only when explicitly requested by the caller.
pub fn prove(layer: &SyncStoreLayer, compiled: &CompiledBgp) -> Result<ProvedBgp, QueryProofError> {
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

/// Explicitly prove and encode only after the ordinary executor's Store-ID rows match
/// the proven unordered result exactly. No call is made unless the application invokes
/// this function after normal query execution.
pub fn prove_executed_and_encode(
    layer: &SyncStoreLayer,
    query_json: &str,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<u64>],
) -> Result<Vec<u8>, ExecutionProofError> {
    let compiled = compile_json(query_json)?;
    let proved = prove(layer, &compiled)?;
    Ok(encode_executed_envelope(
        layer,
        &compiled,
        &proved,
        expected_root,
        variables,
        rows,
    )?)
}

/// Bind an already explicitly generated proof to ordinary executor rows and encode it.
pub fn encode_executed_envelope(
    layer: &SyncStoreLayer,
    compiled: &CompiledBgp,
    proved: &ProvedBgp,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<u64>],
) -> Result<Vec<u8>, QueryProofError> {
    let commitment = layer
        .proof_commitment()?
        .ok_or(QueryProofError::LayerNotProofEnabled)?;
    proved.verify_executed_rows(compiled, &commitment, expected_root, variables, rows)?;
    Ok(proved.encode_envelope(compiled, &commitment, expected_root)?)
}

/// Decode and verify an envelope, then independently bind it to the ordinary executor's
/// Store-ID row multiset.
pub fn decode_verify_executed_envelope(
    bytes: &[u8],
    layer: &SyncStoreLayer,
    query_json: &str,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<u64>],
) -> Result<VerifiedBgpEnvelope, ExecutionProofError> {
    let compiled = compile_json(query_json)?;
    let verified = decode_and_verify_envelope(bytes, layer, &compiled, expected_root)?;
    let commitment = layer
        .proof_commitment()?
        .ok_or(QueryProofError::LayerNotProofEnabled)?;
    verified
        .proved
        .verify_executed_rows(&compiled, &commitment, expected_root, variables, rows)?;
    Ok(verified)
}

pub use terminus_store::proof::ProofHash;
pub use terminusdb_woql2::proof::QueryProofError as Error;
