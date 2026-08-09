//! Explicit foreign predicates binding normal Prolog WOQL rows to proof envelopes.

use std::convert::TryInto;
use std::io;
use std::io::Write;
use std::sync::Arc;

use swipl::prelude::*;
use terminusdb_query_proof::{
    decode_verify_executed_inputs_compact_envelope, decode_verify_executed_inputs_envelope,
    encode_verifier_commitment, migrate_legacy_chain, prove_executed_inputs_and_encode,
    ExecutedResultInput, ProofHash, QueryBlitzarConfig, QueryProver, QueryProverTier,
};

use crate::layer::WrappedLayer;
use crate::store::WrappedStore;
use terminus_store::storage::string_to_name;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn expected_root(text: &str) -> io::Result<ProofHash> {
    let bytes = hex::decode(text).map_err(|_| invalid("expected root is not hexadecimal"))?;
    bytes
        .try_into()
        .map_err(|_| invalid("expected root must contain exactly 32 bytes"))
}

/// Foreign-wire value for an ordinary WOQL result cell. Positive integers are
/// Store dictionary IDs, `generated_decimal(String)` carries aggregate output
/// lexically, and the atom `null` is the sole nullable representation. The
/// independently compiled Rust result descriptor decides which kind and, for a
/// generated decimal, which signed domain is legal in each column.
struct ProofResultValue(ExecutedResultInput);

term_getable! {
    (ProofResultValue, "positive Store ID, generated_decimal(String), or null", term) => {
        if let Ok(id) = term.get::<u64>() {
            return Some(ProofResultValue(ExecutedResultInput::Id(id)));
        }
        if let Ok(functor) = term.get::<Functor>() {
            if functor.arity() == 1 && functor.name_string() == "generated_decimal" {
                let lexical: PrologText = attempt_opt(term.get_arg(1)).unwrap_or(None)?;
                return Some(ProofResultValue(ExecutedResultInput::GeneratedDecimal(
                    lexical.into_inner(),
                )));
            }
        }
        let is_null = term
            .get_atom_name(|name| name == Some("null"))
            .ok()?;
        is_null.then_some(ProofResultValue(ExecutedResultInput::Null))
    }
}

fn result_rows(rows: Vec<Vec<ProofResultValue>>) -> Vec<Vec<ExecutedResultInput>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(|value| value.0).collect())
        .collect()
}

fn prover_tier(text: &str) -> io::Result<QueryProverTier> {
    match text {
        "log20" => Ok(QueryProverTier::Log20),
        "log22" => Ok(QueryProverTier::Log22),
        "log24" => Ok(QueryProverTier::Log24),
        _ => Err(invalid(
            "prover tier must be exactly log20, log22, or log24",
        )),
    }
}

wrapped_arc_blob!("query_prover", pub WrappedQueryProver, QueryProver);

impl WrappedArcBlobImpl for WrappedQueryProver {
    fn write(_this: &QueryProver, stream: &mut PrologStream) -> io::Result<()> {
        write!(stream, "<query_prover>")
    }
}

predicates! {
    /// Construct an explicit Arkworks CPU prover handle. The Arc-backed SWI blob owns
    /// the prover and releases it safely when the final foreign reference is collected.
    pub semidet fn query_proof_open_cpu_prover(_context, prover_term) {
        prover_term.unify(&WrappedQueryProver(Arc::new(QueryProver::cpu())))
    }

    /// Construct an explicitly selected accelerated prover. Backend and tier atoms are
    /// exact; unavailable compile-time features fail closed through a Prolog exception.
    pub semidet fn query_proof_open_blitzar_prover(context, backend_term, tier_term, precomputed_term, prover_term) {
        let backend: PrologText = backend_term.get_ex()?;
        let tier_text: PrologText = tier_term.get_ex()?;
        let precomputed: u64 = precomputed_term.get_ex()?;
        let tier = context.try_or_die(prover_tier(&tier_text))?;
        let config = QueryBlitzarConfig { num_precomputed_generators: precomputed };
        let prover = match backend.as_ref() {
            "cpu" => context.try_or_die(QueryProver::blitzar_cpu(tier, config).map_err(|error| invalid(error.to_string())))?,
            "gpu" => context.try_or_die(QueryProver::blitzar_gpu(tier, config).map_err(|error| invalid(error.to_string())))?,
            _ => return context.try_or_die(Err(invalid("Blitzar backend must be exactly cpu or gpu"))),
        };
        prover_term.unify(&WrappedQueryProver(Arc::new(prover)))
    }

    /// Explicitly migrate the intrinsic proof chain ending at exactly `layer_id`
    /// in the caller-supplied Store. This does not resolve or follow a mutable graph
    /// head and is never called by open/query/startup paths.
    pub semidet fn query_proof_migrate_layer(context, store_term, layer_id_term, root_term, status_term) {
        let store: WrappedStore = store_term.get_ex()?;
        let layer_id_text: PrologText = layer_id_term.get_ex()?;
        let layer_id = context.try_or_die(string_to_name(&layer_id_text))?;
        let outcome = context.try_or_die(migrate_legacy_chain(&store, layer_id))?;
        root_term.unify(hex::encode(outcome.trusted_root))?;
        status_term.unify(Atom::new(outcome.status.as_atom()))
    }

    /// Return the proof root for an explicitly selected committed layer.
    pub semidet fn query_proof_layer_root(context, layer_term, root_term) {
        let layer: WrappedLayer = layer_term.get_ex()?;
        let commitment = context.try_or_die(layer.proof_commitment())?;
        root_term.unify(hex::encode(commitment.state.commitment_root))
    }

    /// Derive a portable verifier-only artifact for an explicitly selected layer.
    pub semidet fn query_proof_verifier_commitment(context, layer_term, artifact_term) {
        let layer: WrappedLayer = layer_term.get_ex()?;
        let artifact = context.try_or_die(
            encode_verifier_commitment(&layer).map_err(|error| invalid(error.to_string()))
        )?;
        artifact_term.unify(artifact.as_slice())
    }

    /// Explicitly compile/prove/encode after the ordinary Prolog executor has returned
    /// foreign-wire `rows` in `variables` order.
    pub semidet fn query_proof_run_envelope(context, layer_term, query_json_term, expected_root_term, variables_term, rows_term, envelope_term) {
        let layer: WrappedLayer = layer_term.get_ex()?;
        let query_json: PrologText = query_json_term.get_ex()?;
        let root_text: PrologText = expected_root_term.get_ex()?;
        let variables: Vec<String> = variables_term.get_ex()?;
        let rows = result_rows(rows_term.get_ex::<Vec<Vec<ProofResultValue>>>()?);
        let root = context.try_or_die(expected_root(&root_text))?;
        let envelope = context.try_or_die(
            prove_executed_inputs_and_encode(&layer, &query_json, root, &variables, &rows)
                .map_err(|error| invalid(error.to_string()))
        )?;
        envelope_term.unify(envelope.as_slice())
    }

    /// Prove with an explicitly caller-created foreign prover handle. This predicate
    /// owns no global runtime and adds no scheduling or proof lifecycle policy.
    pub semidet fn query_proof_run_envelope_with_prover(context, prover_term, layer_term, query_json_term, expected_root_term, variables_term, rows_term, envelope_term) {
        let prover: WrappedQueryProver = prover_term.get_ex()?;
        let layer: WrappedLayer = layer_term.get_ex()?;
        let query_json: PrologText = query_json_term.get_ex()?;
        let root_text: PrologText = expected_root_term.get_ex()?;
        let variables: Vec<String> = variables_term.get_ex()?;
        let rows = result_rows(rows_term.get_ex::<Vec<Vec<ProofResultValue>>>()?);
        let root = context.try_or_die(expected_root(&root_text))?;
        let envelope = context.try_or_die(
            prover
                .prove_executed_inputs_and_encode(&layer, &query_json, root, &variables, &rows)
                .map_err(|error| invalid(error.to_string()))
        )?;
        envelope_term.unify(envelope.as_slice())
    }

    /// Explicitly decode/verify an envelope and bind it independently to rows returned
    /// by the ordinary Prolog executor.
    pub semidet fn query_proof_verify_envelope(context, layer_term, query_json_term, expected_root_term, variables_term, rows_term, envelope_term) {
        let layer: WrappedLayer = layer_term.get_ex()?;
        let query_json: PrologText = query_json_term.get_ex()?;
        let root_text: PrologText = expected_root_term.get_ex()?;
        let variables: Vec<String> = variables_term.get_ex()?;
        let rows = result_rows(rows_term.get_ex::<Vec<Vec<ProofResultValue>>>()?);
        let envelope: Vec<u8> = envelope_term.get_ex()?;
        let root = context.try_or_die(expected_root(&root_text))?;
        context.try_or_die(
            decode_verify_executed_inputs_envelope(
                &envelope,
                &layer,
                &query_json,
                root,
                &variables,
                &rows,
            )
            .map(|_| ())
            .map_err(|error| invalid(error.to_string()))
        )
    }


    /// Decode/verify and bind executor rows using only a compact verifier artifact.
    pub semidet fn query_proof_verify_compact_envelope(context, artifact_term, query_json_term, expected_root_term, variables_term, rows_term, envelope_term) {
        let artifact: Vec<u8> = artifact_term.get_ex()?;
        let query_json: PrologText = query_json_term.get_ex()?;
        let root_text: PrologText = expected_root_term.get_ex()?;
        let variables: Vec<String> = variables_term.get_ex()?;
        let rows = result_rows(rows_term.get_ex::<Vec<Vec<ProofResultValue>>>()?);
        let envelope: Vec<u8> = envelope_term.get_ex()?;
        let root = context.try_or_die(expected_root(&root_text))?;
        context.try_or_die(
            decode_verify_executed_inputs_compact_envelope(
                &envelope,
                &artifact,
                &query_json,
                root,
                &variables,
                &rows,
            )
            .map(|_| ())
            .map_err(|error| invalid(error.to_string()))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prover_tiers_are_exact() {
        assert_eq!(prover_tier("log20").unwrap(), QueryProverTier::Log20);
        assert_eq!(prover_tier("log22").unwrap(), QueryProverTier::Log22);
        assert_eq!(prover_tier("log24").unwrap(), QueryProverTier::Log24);

        for invalid in ["20", "Log20", "log21", "log24 ", ""] {
            assert!(
                prover_tier(invalid).is_err(),
                "accepted invalid tier {:?}",
                invalid
            );
        }
    }
}
