//! Explicit Proof-of-WOQL integration boundary for TerminusDB.
//!
//! This module deliberately contains no scheduler or background-job policy. Callers may
//! compile in advance, run the ordinary query through Prolog, and decide themselves if
//! and when to invoke [`prove`]. Verification always requires a caller-selected trusted
//! root.

use std::io;

use terminus_store::layer::Layer;
#[cfg(any(
    all(feature = "blitzar-cpu", not(feature = "blitzar-gpu")),
    all(feature = "blitzar-gpu", not(feature = "blitzar-cpu"))
))]
use terminus_store::proof::argument::BlitzarRuntimeConfig;
use terminus_store::proof::argument::{ProverSetupTier, StoreProverContext};
use terminus_store::proof::commitment::VerifierCommitment;
use terminus_store::store::sync::{SyncStore, SyncStoreLayer};
use terminusdb_schema::FromTDBInstance;
use terminusdb_woql2::proof::{
    decode_and_verify_envelope as decode_woql_envelope,
    decode_verifier_commitment_and_verify_envelope as decode_compact_woql_envelope, plan_bgp,
    BgpPlanError, CompiledBgp, ProvedBgp, QueryProofError, VerifiedBgpEnvelope,
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

/// Exact Store setup tier selected by an application-owned accelerated prover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryProverTier {
    Log20,
    Log22,
    Log24,
}

impl From<QueryProverTier> for ProverSetupTier {
    fn from(value: QueryProverTier) -> Self {
        match value {
            QueryProverTier::Log20 => Self::Log20,
            QueryProverTier::Log22 => Self::Log22,
            QueryProverTier::Log24 => Self::Log24,
        }
    }
}

/// Explicit native preprocessing configuration. No default is chosen at proof time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryBlitzarConfig {
    pub num_precomputed_generators: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryProverConfigError {
    #[error("the requested `{0}` proving backend is not enabled in this build")]
    FeatureUnavailable(&'static str),
    #[error("Blitzar CPU and GPU features are mutually exclusive for QueryProver")]
    ConflictingBackends,
    #[error(transparent)]
    Store(#[from] io::Error),
}

/// Application-domain owner of all proving arithmetic state.
///
/// Construction is the only place native initialization may occur. Methods perform
/// proofs synchronously when called and contain no scheduler, cache, or lifecycle policy.
pub struct QueryProver {
    context: StoreProverContext<'static>,
}

impl QueryProver {
    pub fn cpu() -> Self {
        Self {
            context: StoreProverContext::cpu(),
        }
    }

    pub fn blitzar_cpu(
        tier: QueryProverTier,
        config: QueryBlitzarConfig,
    ) -> Result<Self, QueryProverConfigError> {
        #[cfg(all(feature = "blitzar-cpu", not(feature = "blitzar-gpu")))]
        {
            let runtime = StoreProverContext::initialize_blitzar(BlitzarRuntimeConfig {
                num_precomputed_generators: config.num_precomputed_generators,
            });
            return Ok(Self {
                context: StoreProverContext::blitzar(&runtime, tier.into())?,
            });
        }
        #[cfg(all(feature = "blitzar-cpu", feature = "blitzar-gpu"))]
        {
            let _ = (tier, config);
            Err(QueryProverConfigError::ConflictingBackends)
        }
        #[cfg(not(feature = "blitzar-cpu"))]
        {
            let _ = (tier, config);
            Err(QueryProverConfigError::FeatureUnavailable("blitzar-cpu"))
        }
    }

    pub fn blitzar_gpu(
        tier: QueryProverTier,
        config: QueryBlitzarConfig,
    ) -> Result<Self, QueryProverConfigError> {
        #[cfg(all(feature = "blitzar-gpu", not(feature = "blitzar-cpu")))]
        {
            let runtime = StoreProverContext::initialize_blitzar(BlitzarRuntimeConfig {
                num_precomputed_generators: config.num_precomputed_generators,
            });
            return Ok(Self {
                context: StoreProverContext::blitzar(&runtime, tier.into())?,
            });
        }
        #[cfg(all(feature = "blitzar-cpu", feature = "blitzar-gpu"))]
        {
            let _ = (tier, config);
            Err(QueryProverConfigError::ConflictingBackends)
        }
        #[cfg(not(feature = "blitzar-gpu"))]
        {
            let _ = (tier, config);
            Err(QueryProverConfigError::FeatureUnavailable("blitzar-gpu"))
        }
    }

    pub fn prove(
        &self,
        layer: &SyncStoreLayer,
        compiled: &CompiledBgp,
    ) -> Result<ProvedBgp, QueryProofError> {
        compiled.prove_on_layer_with_context(&self.context, layer)
    }

    pub fn prove_executed_and_encode(
        &self,
        layer: &SyncStoreLayer,
        query_json: &str,
        expected_root: ProofHash,
        variables: &[String],
        rows: &[Vec<u64>],
    ) -> Result<Vec<u8>, ExecutionProofError> {
        let compiled = compile_json(query_json)?;
        let proved = self.prove(layer, &compiled)?;
        Ok(encode_executed_envelope(
            layer,
            &compiled,
            &proved,
            expected_root,
            variables,
            rows,
        )?)
    }

    pub fn prove_executed_values_and_encode(
        &self,
        layer: &SyncStoreLayer,
        query_json: &str,
        expected_root: ProofHash,
        variables: &[String],
        rows: &[Vec<ExecutedResultValue>],
    ) -> Result<Vec<u8>, ExecutionProofError> {
        let compiled = compile_json(query_json)?;
        let proved = self.prove(layer, &compiled)?;
        Ok(encode_executed_values_envelope(
            layer,
            &compiled,
            &proved,
            expected_root,
            variables,
            rows,
        )?)
    }
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
    // Use woql2/schema's canonical JSON-LD decoder. `serde_json::from_str<Query>`
    // happens to delegate through generated serde glue today, but it is not the
    // domain decoding API and obscures typed-value errors behind serde context.
    let json: serde_json::Value = serde_json::from_str(query_json)?;
    let supplied = json.clone();
    let query = Query::from_json(json).map_err(|error| {
        serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })?;
    // The generated domain decoder intentionally builds model instances and may
    // otherwise ignore an unknown property. This trust boundary accepts only the
    // canonical normalized WOQL representation: round-tripping must reproduce the
    // exact JSON value, which rejects unknown/misspelled fields without a second
    // hand-written query decoder.
    if query.to_woql_json() != supplied {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "WOQL JSON is not the canonical normalized query representation",
        ))
        .into());
    }
    Ok(compile(&query).map_err(QueryProofError::from)?)
}

/// Generate a proof only when explicitly requested by the caller.
pub fn prove(layer: &SyncStoreLayer, compiled: &CompiledBgp) -> Result<ProvedBgp, QueryProofError> {
    QueryProver::cpu().prove(layer, compiled)
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

/// Derive the portable verifier-only projection of an explicitly selected layer.
/// This chooses no persistence or transport policy.
pub fn encode_verifier_commitment(layer: &SyncStoreLayer) -> Result<Vec<u8>, QueryProofError> {
    let commitment = layer.proof_commitment()?;
    Ok(commitment.verifier_commitment()?.encode()?)
}

/// Decode and verify using only a compact verifier artifact. The trusted root remains
/// an independent required input and is never adopted from the decoded artifact.
pub fn decode_and_verify_compact_envelope(
    bytes: &[u8],
    verifier_commitment_bytes: &[u8],
    compiled: &CompiledBgp,
    expected_root: ProofHash,
) -> Result<VerifiedBgpEnvelope, QueryProofError> {
    Ok(decode_compact_woql_envelope(
        bytes,
        compiled,
        verifier_commitment_bytes,
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
    QueryProver::cpu().prove_executed_and_encode(layer, query_json, expected_root, variables, rows)
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
    QueryProver::cpu().prove_executed_values_and_encode(
        layer,
        query_json,
        expected_root,
        variables,
        rows,
    )
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

/// Compact-artifact variant of [`decode_verify_executed_values_envelope`]. It binds
/// ordinary executor values without loading a native Store layer or witness tables.
pub fn decode_verify_executed_values_compact_envelope(
    bytes: &[u8],
    verifier_commitment_bytes: &[u8],
    query_json: &str,
    expected_root: ProofHash,
    variables: &[String],
    rows: &[Vec<ExecutedResultValue>],
) -> Result<VerifiedBgpEnvelope, ExecutionProofError> {
    let compiled = compile_json(query_json)?;
    let verified = decode_and_verify_compact_envelope(
        bytes,
        verifier_commitment_bytes,
        &compiled,
        expected_root,
    )?;
    let commitment = VerifierCommitment::decode(verifier_commitment_bytes)?;
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
