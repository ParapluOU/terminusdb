# The diff-log feed

A pollable, append-only feed of a branch's commits and their diffs, served
over GraphQL by the Rust webserver.

It exists because a reactive cache wants to follow a database the way a
consumer follows a Kafka topic — ask "anything after offset N?" as often as it
likes — and nothing in TerminusDB could answer that cheaply. `/api/log` is
`O(depth)` (467 ms at depth 261, and growing) and carries no diffs; the
commit-graph GraphQL endpoint is fast but equally diff-less, because the
per-commit document sets `ref.json` declares are never populated.

The data was already there. Every commit is an immutable delta layer whose
additions and removals *are* the diff, and the commit chain is already ordered
and append-only. The feed just serves it.

## Properties

**An offset is intrinsic.** It is the commit's depth from the root of its
chain. It is not a sequence number the server hands out and has to remember:
delete every cache, restart the process, and your offsets still mean what they
meant.

**Polling is `O(1)` when nothing happened.** A branch's head move bumps its
label version, so a poll that has nothing to report is one label read — no
layer opened, no triple touched. New commits cost a walk of exactly the new
ones. First sight of a branch costs one walk of its chain, once.

**A record costs `O(triples the commit changed)`**, independent of database
size and history depth. Measured in terminus-store's
`delta_feed_throughput` example: ~0.02 µs/triple to walk the ids, ~1.05 µs to
resolve them to terms (~0.40 µs memoized), ~0.065 µs to render — 2.1 M changed
triples/s on one core, flat across a 10× bigger database.

**It is off the Prolog path entirely**, read and write. The only Prolog
involvement is one handshake at startup (`difflog_attach_store/1` in
`src/server/srv.pl`) that hands over the store handle. Nothing a consumer does
reaches the solver.

**Nothing is retained or discarded.** The two bounds
(`TERMINUSDB_FEED_MAX_LOGS`, `TERMINUSDB_FEED_MAX_DELTAS`) are LRU caches: a
miss re-reads data that has not moved. No record becomes unreachable and no
request is refused because something was evicted. Layers are never deleted, so
the whole log is always derivable from the store.

## Using it

No configuration, and no feed-specific secret: it takes the same credential as
the rest of the API. Follow a branch — the cheap mode gives offsets, commit
metadata and counts:

```bash
curl -s -u admin:root localhost:6363/api/v1/feed/graphql \
  -H 'Content-Type: application/json' \
  -d '{"query":"{ feed(database:\"admin/people\", from:0, limit:10) {
         epoch head lag nextOffset
         records { offset commit addedCount removedCount message } } }"}'
```

Ask for payloads where you care:

```graphql
{
  feed(database: "admin/people", from: 7, limit: 1, triples: true) {
    records {
      offset commit
      added   { subject predicate object node }
      removed { subject predicate object node }
    }
  }
}
```

Then loop: hand `nextOffset` back as `from`, and pass the last commit you saw
as `after`. That second part is what makes a rewritten history (reset, rebase,
force-push) visible instead of silently renumbering under you — the response
comes back with `rewoundTo` set and no records. Resume from there: it is the
offset just after the commit you named if that commit is still in the log, and
0 if it is not, since one commit id does not say how much of your history
survived.

`{ databases }` and `{ branches(database: "admin/people") }` are there for
discovery; `GET /api/v1/feed/status` reports what is indexed.

Both `POST` and `GET /api/v1/feed/graphql?query=…` work.

## Authorization

HTTP `Basic`, with a TerminusDB username and password, checked against the
system graph the same way `/api/document` checks it:

- the user's `key_hash` is verified against the supplied password
  (`$pbkdf2-sha512$t=<iterations>$<salt>$<hash>`, parameters read from the
  stored hash rather than assumed, so a change to TerminusDB's cost keeps
  working);
- then the capability walk — `User --capability--> Capability --role--> Role
  --action--> Action`, with the capability's `scope` reaching the database
  through the reflexive transitive closure of `child` and `database`, so a
  capability over an organization covers the databases under it;
- the feed requires `instance_read_access` and `commit_read_access`, the two
  read actions that cover what it serves, and nothing more. `User/admin` is
  the super user, as everywhere else.

Successes are memoized for 30 seconds, which is what keeps a polling consumer
from re-deriving a KDF on every poll. Only successes, and only briefly: a
capability just granted must not be denied for a whole TTL, and one just
revoked costs at most that long.

The check is per GraphQL field, not per route, so `databases` *filters* to
what you may read rather than refusing outright — discovery stays useful for a
non-admin, and a new field cannot read a database without authorizing it.

## What is not done

**No JWT yet.** The fork already verifies JWTs in Rust (`jwt.rs`, used for
`jwt_scopes` auth), so a bearer token is a small addition on top of the Basic
path rather than new machinery.

**Nothing is pushed.** A consumer polls, which is the design — `O(1)` polling
is what makes it cheap to poll often. A long-poll or SSE variant would cut the
idle latency to zero and is a small addition on top of this (the label version
is already the signal to wait on).

**Records are rendered on demand, not ahead of time.** A commit that changed a
million triples costs ~1 s of resolution on the first poll that asks for its
payload, bounded per request by `limit`. Pre-rendering in a background task
would move that off the request path; the measurement says it is worth doing
only for deltas far larger than a document write.

**Schema diffs are not resolved.** A record names its `schemaLayer` and a
schema-only commit occupies an offset (skipping it would misalign everything
after), but only instance-graph triples come back in `added`/`removed`.
