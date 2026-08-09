# Query-proof release integration test

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

The suite is not marked concurrent and generates exactly seven proofs: one
projected BGP, one live `Count` over the same relation, and present/absent
standalone ground triples (including an object absent from the dictionary), plus
a nested `From(instance)`/`Pin`/`Immediately` query and a full-schema `Distinct`
triple query. All verification, reopen and tamper cases reuse those envelopes.
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

The ordinary PlUnit run leaves this suite blocked and performs no query-proof
work. The suite additionally runs a normal `woql_query_json/9` `Limit` query;
`Limit` is intentionally unsupported by the proof planner, so its success
guards against accidental proof compilation on the ordinary path.

The integration fixture does not manufacture a legacy PF3/v3 sidecar. There is
deliberately no production v3 writer, and duplicating the private legacy codec
here would weaken the migration boundary. Store's focused migration tests cover
v3-to-v4, corrupt v3, interrupted/CAS retry, idempotence, and archive reopen;
this suite covers the explicit running-node API on the resulting intrinsic PF4
v4 layer contract.
