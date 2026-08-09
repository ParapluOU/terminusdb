//! Explicit Proof-of-WOQL integration boundary for TerminusDB.
//!
//! This module deliberately contains no scheduler or background-job policy. Callers may
//! compile in advance, run the ordinary query through Prolog, and decide themselves if
//! and when to invoke [`prove`]. Verification always requires a caller-selected trusted
//! root.

use std::io;

use terminus_store::layer::Layer;
use terminus_store::store::sync::{SyncStore, SyncStoreLayer};
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

/// Outcome of an explicitly requested migration of one immutable layer head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStatus {
    /// The exact selected head already had a valid current PF4 commitment chain.
    AlreadyCurrent,
    /// Store migrated at least one missing/legacy sidecar in the selected chain.
    Migrated,
}

impl MigrationStatus {
    pub fn as_atom(self) -> &'static str {
        match self {
            Self::AlreadyCurrent => "already_current",
            Self::Migrated => "migrated",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub layer_id: [u32; 5],
    pub trusted_root: ProofHash,
    pub status: MigrationStatus,
}

/// Explicitly migrate the proof chain ending at exactly `layer_id`.
///
/// This is a low-level privileged primitive: routing and authorization remain with
/// the caller that owns `store`. It never resolves a mutable named-graph head and
/// is never invoked by open/query/startup paths. Store owns all v3 validation and
/// compare-and-swap replacement logic.
pub fn migrate_legacy_chain(store: &SyncStore, layer_id: [u32; 5]) -> io::Result<MigrationOutcome> {
    // Store is the sole legacy admission point and derives status inside the
    // migration/CAS operation, without a racy before/after observation.
    let migration = store.migrate_proof_chain_with_status(layer_id)?;

    // Report success only after reopening and validating the exact immutable head.
    // A failed/interrupted chain or concurrent CAS race returns an error before this.
    let persisted = store.get_layer_from_id(layer_id)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "requested migration layer disappeared after migration",
        )
    })?;
    if persisted.name() != layer_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "post-migration store resolved a different layer than requested",
        ));
    }
    let persisted_state = persisted.proof_commitment()?.state.clone();
    if persisted_state != migration.state {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "migration did not persist the complete reported v4 head state",
        ));
    }

    Ok(MigrationOutcome {
        layer_id,
        trusted_root: persisted_state.commitment_root,
        status: if migration.migrated {
            MigrationStatus::Migrated
        } else {
            MigrationStatus::AlreadyCurrent
        },
    })
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
    let commitment = layer.proof_commitment()?;
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
    let commitment = layer.proof_commitment()?;
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
    let commitment = layer.proof_commitment()?;
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
    let commitment = layer.proof_commitment()?;
    verified
        .proved
        .verify_executed_rows(&compiled, &commitment, expected_root, variables, rows)?;
    Ok(verified)
}

fn scalar_rows(rows: &[Vec<ExecutedResultValue>]) -> io::Result<Vec<Vec<u64>>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|value| match value {
                    ExecutedResultValue::Id(value) => Ok(*value),
                    ExecutedResultValue::Null => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Count results cannot contain null",
                    )),
                })
                .collect()
        })
        .collect()
}

/// Explicitly prove and encode after binding typed Store-ID/null rows returned by
/// the ordinary executor. Rust's compiled result descriptors remain authoritative:
/// `Null` is accepted only in plan-declared nullable columns. Count uses its existing
/// scalar boundary because zero is a valid count rather than a Store dictionary ID.
pub fn prove_executed_values_and_encode(
    layer: &SyncStoreLayer,
    query_json: &str,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<ExecutedResultValue>],
) -> Result<Vec<u8>, ExecutionProofError> {
    let compiled = compile_json(query_json)?;
    let proved = prove(layer, &compiled)?;
    Ok(encode_executed_values_envelope(
        layer,
        &compiled,
        &proved,
        expected_root,
        variables,
        rows,
    )?)
}

/// Bind an already generated proof to typed executor values and encode it.
pub fn encode_executed_values_envelope(
    layer: &SyncStoreLayer,
    compiled: &CompiledBgp,
    proved: &ProvedBgp,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<ExecutedResultValue>],
) -> Result<Vec<u8>, QueryProofError> {
    let commitment = layer.proof_commitment()?;
    if compiled.count_variable.is_some() {
        let rows = scalar_rows(rows)?;
        proved.verify_executed_rows(compiled, &commitment, expected_root, variables, &rows)?;
    } else {
        proved.verify_executed_values(compiled, &commitment, expected_root, variables, rows)?;
    }
    Ok(proved.encode_envelope(compiled, &commitment, expected_root)?)
}

/// Decode and verify an envelope, then bind it independently to typed executor values.
pub fn decode_verify_executed_values_envelope(
    bytes: &[u8],
    layer: &SyncStoreLayer,
    query_json: &str,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<ExecutedResultValue>],
) -> Result<VerifiedBgpEnvelope, ExecutionProofError> {
    let compiled = compile_json(query_json)?;
    let verified = decode_and_verify_envelope(bytes, layer, &compiled, expected_root)?;
    let commitment = layer.proof_commitment()?;
    if compiled.count_variable.is_some() {
        let rows = scalar_rows(rows)?;
        verified.proved.verify_executed_rows(
            &compiled,
            &commitment,
            expected_root,
            variables,
            &rows,
        )?;
    } else {
        verified.proved.verify_executed_values(
            &compiled,
            &commitment,
            expected_root,
            variables,
            rows,
        )?;
    }
    Ok(verified)
}

pub use terminus_store::proof::ProofHash;
pub use terminusdb_woql2::proof::{ExecutedResultValue, QueryProofError as Error};
