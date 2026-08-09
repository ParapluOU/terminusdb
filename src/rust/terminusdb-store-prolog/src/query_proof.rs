//! Explicit foreign predicates binding normal Prolog WOQL rows to proof envelopes.

use std::convert::TryInto;
use std::io;

use swipl::prelude::*;
use terminusdb_query_proof::{
    decode_verify_executed_values_envelope, migrate_legacy_chain, prove_executed_values_and_encode,
    ExecutedResultValue, ProofHash,
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
/// Store dictionary IDs and the atom `null` is the sole nullable representation.
/// Whether that atom is legal in a particular column is decided later by the
/// independently compiled Rust result descriptor.
struct ProofResultValue(ExecutedResultValue);

term_getable! {
    (ProofResultValue, "positive Store ID or null", term) => {
        if let Ok(id) = term.get::<u64>() {
            return Some(ProofResultValue(ExecutedResultValue::Id(id)));
        }
        let is_null = term
            .get_atom_name(|name| name == Some("null"))
            .ok()?;
        is_null.then_some(ProofResultValue(ExecutedResultValue::Null))
    }
}

fn result_rows(rows: Vec<Vec<ProofResultValue>>) -> Vec<Vec<ExecutedResultValue>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(|value| value.0).collect())
        .collect()
}

predicates! {
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

    /// Explicitly compile/prove/encode after the ordinary Prolog executor has returned
    /// `rows` as Store IDs in `variables` order.
    pub semidet fn query_proof_run_envelope(context, layer_term, query_json_term, expected_root_term, variables_term, rows_term, envelope_term) {
        let layer: WrappedLayer = layer_term.get_ex()?;
        let query_json: PrologText = query_json_term.get_ex()?;
        let root_text: PrologText = expected_root_term.get_ex()?;
        let variables: Vec<String> = variables_term.get_ex()?;
        let rows = result_rows(rows_term.get_ex::<Vec<Vec<ProofResultValue>>>()?);
        let root = context.try_or_die(expected_root(&root_text))?;
        let envelope = context.try_or_die(
            prove_executed_values_and_encode(&layer, &query_json, root, &variables, &rows)
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
            decode_verify_executed_values_envelope(
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
}
