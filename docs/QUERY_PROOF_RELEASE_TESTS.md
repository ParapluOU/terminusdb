# Query-proof release integration test

The canonical cross-repository accelerator status, benchmark commands, macOS
limitations, and continuation checklist live in the sibling `terminusdb-rs`
checkout at `docs/WOQL_QUERY_PROOF_GPU_HANDOFF.md`. This document remains the
source of truth for the TerminusDB running-node release test.

The full running-node proof test is deliberately serial and opt-in. It builds a
multi-layer archive database, executes `woql_query_json_with_proof/11`, verifies
the envelope before and after reopening the archive, and runs the negative
root/query/envelope/multiplicity checks against the native verifier.

From the TerminusDB checkout, the Linux x86-64 CI command using the sibling
`terminusdb-rs` vendored dependencies is:

```sh
DEPS=../terminusdb-rs/crates/bin/.deps
TARGET=x86_64-unknown-linux-gnu
export RUSTUP_TOOLCHAIN=nightly-2025-09-19
export SWIPL="$DEPS/swipl-env/$TARGET/bin/swipl"
export SWI_HOME_DIR="$DEPS/swipl-env/$TARGET/lib/swipl"
export PROTOC="$DEPS/protoc/bin/protoc"
export LIBCLANG_PATH="$DEPS/libclang-env/$TARGET/lib"
export BINDGEN_EXTRA_CLANG_ARGS="-I$LIBCLANG_PATH/clang/22/include"
export PKG_CONFIG_PATH="$DEPS/swipl-env/$TARGET/share/pkgconfig"
export LIBRARY_PATH="$DEPS/gmp/$TARGET/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$LIBCLANG_PATH:$SWI_HOME_DIR/lib/$TARGET${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export CFLAGS=-std=gnu17

(cd src/rust && cargo build -p terminusdb-dylib --release --offline)
cp src/rust/target/release/libterminusdb_dylib.so src/rust/librust.so
TERMINUSDB_QUERY_PROOF_RELEASE_TESTS=true \
  /usr/bin/time -p make test SUITE=woql_query_proof_release \
  SWIPL_DIR="$DEPS/swipl-env/$TARGET/bin/"
```

The suite is not marked concurrent and generates exactly sixteen proofs: one
projected BGP, one live `Count` over the same relation, and present/absent
standalone ground triples (including an object absent from the dictionary), plus
a present/absent all-ground conjunction, a nested
`Using`/`From(instance)`/`Pin`/`Immediately` query, and a full-schema `Distinct`
triple query, present/absent ground gates over the projected BGP, and a
disconnected component Cartesian-composed at the multi-layer head, and an exact
bag `Or` whose second branch adds a true authenticated ground gate. All
verification, reopen and tamper cases reuse those envelopes; the mixed-gate
cases also reject swapping the true and false result branches. The Cartesian
case rejects reordered components, reordered result metadata, and a duplicated
product row.
The `Or` case returns every overlapping row twice and rejects branch reordering,
a dropped row, and envelope corruption after archive reopen.
The fourteenth proof is a correlated `Optional` with duplicate matches and one
unmatched left row. Its ordinary and proof-mode JSON both expose the unbound value
as `null`; the native row boundary carries the atom `null`, reopens and verifies the
envelope from an archive, and rejects omission, duplication, a forged nonzero value
in the nullable column, and `null` in the shared key.
The fifteenth is a global `Optional` whose authenticated right relation is empty;
it null-extends every left row across predicate and node/object namespaces, survives
archive reopen, and rejects missing/extra rows, predicate-namespace substitution,
and `null` in a nonnullable left column.
The sixteenth is an exact, already-bound subject-node `Equals` filter. It matches the
ordinary executor before and after archive reopen and rejects a dropped selected row.
The same fixture confirms that Prolog accepts cross-datatype numeric equality for both
variable/constant and variable/variable operands while proof compilation rejects those
coercive forms before proof generation.
On 2026-08-09 the clean
release build took 215.47 seconds and the complete suite took 14.19 seconds
(`user 13.79`, `sys 0.43`) on the development runner. CI should retain a
5-minute suite timeout and record `/usr/bin/time` output for regressions. A
debug-library run took approximately 3 minutes 30 seconds.

After adding live `Count` coverage, the incremental optimized suite took 20.42
seconds (`user 20.08`, `sys 0.37`) on the same runner.

After adding ground existence/nonmembership coverage, the incremental optimized
suite took 22.46 seconds (`user 22.07`, `sys 0.42`). The absent case uses terms
that are all authenticated dictionary members and proves the triple row itself
is absent; dictionary-term nonmembership remains a fail-closed planner boundary.

After adding transparent-wrapper and full-schema `Distinct` coverage, the
incremental optimized suite took 28.10 seconds (`user 27.71`, `sys 0.41`).

With root-authenticated dictionary absence enabled, the seven-proof suite took
28.44 seconds (`user 28.06`, `sys 0.40`).

The `Using` integration reuses the wrapper proof count and additionally rejects
the envelope under a changed collection identity. That suite took 28.71 seconds
(`user 28.35`, `sys 0.39`).

With all-ground conjunctions enabled, the nine-proof suite took 32.35 seconds
(`user 31.97`, `sys 0.41`). The present case intentionally repeats the same
ground atom, covering conjunction idempotence as well as the zero-arity AND.

With mixed ground/relational gating enabled, the eleven-proof suite took 47.06
seconds (`user 46.65`, `sys 0.43`). The false gate authenticates an empty result
at the relational output arity, and the reopen checks reject substituting either
branch's rows for the other.

With disconnected BGP composition enabled, the twelve-proof suite took 63.05
seconds (`user 62.65`, `sys 0.42`). Its Cartesian component exists only in the
child layer, covering the current multi-layer head as well as component/schema
reordering and duplicate-row rejection after archive reopen.

With exact `Or` bag union enabled, the thirteen-proof suite took 68.43 seconds
(`user 68.04`, `sys 0.42`). The two branches deliberately overlap completely,
so all four source rows occur twice; reopen checks reject branch reordering, a
dropped duplicate, and envelope corruption.

With correlated `Optional` enabled, the fourteen-proof optimized suite took 88.15
seconds (`user 87.65`, `sys 0.53`). The incremental release dylib rebuild after the
nullable/global Optional work took 100.53 seconds.

The focused global-Optional live/archive case took 33.49 seconds (`user 33.09`,
`sys 0.40`) using the same optimized dylib. The final complete fifteen-proof,
three-test suite took 117.63 seconds (`user 117.10`, `sys 0.58`).

The ordinary PlUnit run leaves this suite blocked and performs no query-proof
work. The suite additionally runs a normal `woql_query_json/9` `Limit` query;
`Limit` is intentionally unsupported by the proof planner, so its success
guards against accidental proof compilation on the ordinary path.

The bounded scalar `GroupBy` followed immediately by `RangeMin`/`RangeMax` is
covered end-to-end in `terminusdb-rs` (recursive child, outer `Select`, count,
portable envelope, and compact verifier artifact). The running-node bridge uses
woql2/schema's canonical `Query::from_json` decoder after parsing JSON, rather
than serde-derived `Query` decoding. Its focused regression round-trips both
normalized extrema shapes, rejects unknown tags and missing/unknown fields, and
runs `RangeMin` through the executed-row envelope and compact verifier artifact.

The integration fixture does not manufacture a legacy PF3/v3 sidecar. There is
deliberately no production v3 writer, and duplicating the private legacy codec
here would weaken the migration boundary. Store's focused migration tests cover
v3-to-v4, corrupt v3, interrupted/CAS retry, idempotence, and archive reopen;
this suite covers the explicit running-node API on the resulting intrinsic PF4
v4 layer contract.

Generated integral aggregate results cross the Prolog foreign boundary as
`generated_decimal(CanonicalString)`, never as an ambiguous integer Store ID.
Rust recompiles the same query and uses its result descriptor to choose the
unsigned `TripleCount`/unsigned `GroupBy`-`Sum` domain or signed `GroupBy`-`Sum`
domain. Noncanonical lexical integers, overflow, and value-kind/domain mismatch
fail closed. The live Count path in the first release fixture exercises this
wire value through proof generation, ordinary-layer verification, archive
reopen, and wrong-count rejection.

For a development run that does not overwrite the checked-in shared library,
build `terminusdb-dylib` and put a temporary `librust.so` symlink first on the
SWI foreign path. On the 2026-08-09 Linux runner the successful debug artifact
was `src/rust/target/debug/libterminusdb_dylib.so`; linking required the SWI
root `lib` directory and the system GCC runtime in `LIBRARY_PATH`. The checked-in
`src/rust/librust.so` was deliberately left untouched.
