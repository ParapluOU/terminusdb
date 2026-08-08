//! Explicit Proof-of-WOQL integration boundary for TerminusDB.
//!
//! This module deliberately contains no scheduler or background-job policy. Callers may
//! compile in advance, run the ordinary query through Prolog, and decide themselves if
//! and when to invoke [`prove`]. Verification always requires a caller-selected trusted
//! root.

use terminus_store::store::sync::SyncStoreLayer;
use terminusdb_woql2::proof::{
    plan_bgp, BgpPlanError, CompiledBgp, ProvedBgp, QueryProofError,
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

pub use terminus_store::proof::ProofHash;
pub use terminusdb_woql2::proof::QueryProofError as Error;
