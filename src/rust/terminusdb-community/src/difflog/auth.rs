//! Who is asking, and may they read it — answered against TerminusDB's own
//! system graph, with TerminusDB's own credentials.
//!
//! There is no separate secret for the feed. Users, their password hashes and
//! their capabilities are documents in the system graph, which is a layer like
//! any other, so the feed checks the credential the rest of the API checks:
//!
//! ```text
//! User --name-->      "admin"
//!      --key_hash-->  "$pbkdf2-sha512$t=1024$<salt>$<hash>"
//!      --capability-> Capability --role--> Role --action--> Action
//!                                --scope-> Resource  ..(child|database)*..> Database
//! ```
//!
//! Two things make this cheap enough to do per request. The stored hash names
//! its own parameters and TerminusDB's cost is 1024 iterations, so a
//! verification is about a millisecond; and successes are memoized briefly, so
//! a polling consumer pays it once per interval rather than once per poll.
//!
//! Only successes are cached, and only for seconds: a capability just granted
//! must not be denied for the length of a TTL, while one just revoked costs at
//! most that long.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hmac::Hmac;
use sha2::Sha512;
use terminusdb_store_prolog::terminus_store::store::sync::{SyncStore, SyncStoreLayer};
use terminusdb_store_prolog::terminus_store::{Layer, ObjectType};

use crate::value::value_to_string;

/// The system graph's logical name, as `system_instance_name/1` gives it.
const SYSTEM_GRAPH: &str = "terminusdb:///system/data";
const SYSTEM_BASE: &str = "terminusdb://system/data/";
const SYS: &str = "http://terminusdb.com/schema/system#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The one user TerminusDB treats as unconditionally authorized.
const SUPER_USER: &str = "terminusdb://system/data/User/admin";

/// Actions the feed needs: it serves instance triples and commit metadata, so
/// it asks for exactly the two read actions that covers, and nothing else.
const REQUIRED_ACTIONS: [&str; 2] = ["instance_read_access", "commit_read_access"];

/// How long a successful authentication or authorization is trusted. Short on
/// purpose: it exists to keep a polling consumer from re-deriving a KDF every
/// few hundred milliseconds, not to be a session.
const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// `User/admin`, which TerminusDB authorizes for everything.
    SuperUser,
    User { iri: String, name: String },
}

impl Identity {
    pub fn name(&self) -> &str {
        match self {
            Identity::SuperUser => "admin",
            Identity::User { name, .. } => name,
        }
    }
}

#[derive(Debug)]
pub enum Denied {
    /// No credential, or one that does not verify.
    Unauthenticated,
    /// A real user without the actions the feed needs on that database.
    Forbidden,
    /// The system graph could not be read at all, which is not the caller's
    /// fault and must not read as "wrong password".
    Unavailable(String),
}

pub struct Authorizer {
    store: SyncStore,
    /// (username, password) -> identity. Keyed on the password itself so a
    /// changed password stops working within a TTL rather than at a restart.
    authenticated: Mutex<HashMap<(String, String), (Instant, Identity)>>,
    /// (user iri, org, db) -> allowed. Successes only.
    authorized: Mutex<HashMap<(String, String, String), Instant>>,
}

impl Authorizer {
    pub fn new(store: SyncStore) -> Self {
        Self {
            store,
            authenticated: Mutex::new(HashMap::new()),
            authorized: Mutex::new(HashMap::new()),
        }
    }

    fn system(&self) -> Result<SyncStoreLayer, Denied> {
        let label = super::resolve::find_label_exact(&self.store, SYSTEM_GRAPH)
            .map_err(|error| Denied::Unavailable(error.to_string()))?
            .ok_or_else(|| Denied::Unavailable("no system graph in this store".to_string()))?;
        self.store
            .open(&label)
            .and_then(|graph| match graph {
                Some(graph) => graph.head(),
                None => Ok(None),
            })
            .map_err(|error| Denied::Unavailable(error.to_string()))?
            .ok_or_else(|| Denied::Unavailable("system graph is empty".to_string()))
    }

    /// Verify a username and password against the system graph.
    pub fn authenticate(&self, user: &str, password: &str) -> Result<Identity, Denied> {
        let key = (user.to_string(), password.to_string());
        if let Some((seen, identity)) = self
            .authenticated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            if seen.elapsed() < CACHE_TTL {
                return Ok(identity.clone());
            }
        }

        let system = self.system()?;
        // `User` is Lexical on `name`, so the IRI is derivable and no scan is
        // needed to find the user.
        let iri = format!("{SYSTEM_BASE}User/{user}");
        let Some(hash) = object_value(&system, &iri, &format!("{SYS}key_hash")) else {
            // No such user, or a user with no password (`User/anonymous`).
            // Both are "that credential does not authenticate", and neither
            // says which, so an attacker learns nothing about who exists.
            return Err(Denied::Unauthenticated);
        };
        if !verify_password(password, &hash) {
            return Err(Denied::Unauthenticated);
        }

        let identity = if iri == SUPER_USER {
            Identity::SuperUser
        } else {
            Identity::User {
                iri,
                name: user.to_string(),
            }
        };
        let mut cache = self
            .authenticated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune(&mut cache, |(seen, _)| seen.elapsed() < CACHE_TTL);
        cache.insert(key, (Instant::now(), identity.clone()));
        Ok(identity)
    }

    /// May this identity read `org/db` through the feed?
    pub fn authorize(&self, identity: &Identity, org: &str, db: &str) -> Result<(), Denied> {
        let Identity::User { iri, .. } = identity else {
            // The super user is authorized for everything, as everywhere else
            // in TerminusDB.
            return Ok(());
        };
        let key = (iri.clone(), org.to_string(), db.to_string());
        if let Some(seen) = self
            .authorized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            if seen.elapsed() < CACHE_TTL {
                return Ok(());
            }
        }

        let system = self.system()?;
        let Some(database) = database_iri(&system, org, db) else {
            // The database is not in the system graph, so no capability can
            // name it. Reported as forbidden rather than not-found: whether a
            // database exists is itself something to be authorized for.
            return Err(Denied::Forbidden);
        };
        if !has_actions(&system, iri, &database, &REQUIRED_ACTIONS) {
            return Err(Denied::Forbidden);
        }

        let mut cache = self
            .authorized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune(&mut cache, |seen| seen.elapsed() < CACHE_TTL);
        cache.insert(key, Instant::now());
        Ok(())
    }
}

/// Drop expired entries, and keep the map from growing without bound if every
/// entry is live (an attacker trying many passwords would otherwise be able to
/// grow it; only successes are inserted, so this is belt and braces).
fn prune<K: Clone + std::hash::Hash + Eq, V>(
    map: &mut HashMap<K, V>,
    live: impl Fn(&V) -> bool,
) {
    const MAX_ENTRIES: usize = 4096;
    map.retain(|_, value| live(value));
    if map.len() > MAX_ENTRIES {
        map.clear();
    }
}

fn object_value(layer: &SyncStoreLayer, subject: &str, predicate: &str) -> Option<String> {
    let s = layer.subject_id(subject)?;
    let p = layer.predicate_id(predicate)?;
    let triple = layer.single_triple_sp(s, p)?;
    match layer.id_object(triple.object)? {
        ObjectType::Value(value) => Some(value_to_string(&value).into_owned()),
        ObjectType::Node(node) => Some(node),
    }
}

/// Verify a password against SWI-Prolog's `crypto_password_hash/2` output.
///
/// The format is `$pbkdf2-sha512$t=<iterations>$<salt>$<hash>`, base64 without
/// padding, and it names its own parameters — so this reads them rather than
/// assuming TerminusDB's current cost, and a future change to that cost keeps
/// working. Unknown algorithms fail closed.
fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    // A leading `$` means the first field is empty.
    if parts.next() != Some("") {
        return false;
    }
    if parts.next() != Some("pbkdf2-sha512") {
        return false;
    }
    let Some(iterations) = parts
        .next()
        .and_then(|field| field.strip_prefix("t="))
        .and_then(|digits| digits.parse::<u32>().ok())
        .filter(|iterations| *iterations > 0)
    else {
        return false;
    };
    let (Some(salt), Some(expected)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let (Some(salt), Some(expected)) = (decode_b64(salt), decode_b64(expected)) else {
        return false;
    };
    if expected.is_empty() {
        return false;
    }
    let mut derived = vec![0u8; expected.len()];
    if pbkdf2::pbkdf2::<Hmac<Sha512>>(password.as_bytes(), &salt, iterations, &mut derived).is_err()
    {
        return false;
    }
    constant_time_eq(&derived, &expected)
}

/// Decode base64 without being fussy about alphabet or padding: SWI's exact
/// choice is not something to depend on, and both alphabets decode the same
/// bytes for the characters they share.
fn decode_b64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let engines: [&dyn Fn(&str) -> Option<Vec<u8>>; 2] = [
        &|text| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(text)
                .ok()
        },
        &|text| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(text)
                .ok()
        },
    ];
    engines.iter().find_map(|decode| decode(text))
}

/// Compare without an early exit, so a wrong password does not reveal how much
/// of the derived key matched.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut differs = left.len() ^ right.len();
    for index in 0..left.len() {
        differs |= usize::from(left[index] ^ right.get(index).copied().unwrap_or(0));
    }
    differs == 0
}

/// The system graph's IRI for `org/db`: `Organization --database--> Database`,
/// matched by name on both ends.
fn database_iri(system: &SyncStoreLayer, org: &str, db: &str) -> Option<String> {
    let name = system.predicate_id(&format!("{SYS}name"))?;
    let database = system.predicate_id(&format!("{SYS}database"))?;
    let organization = subject_with_name(system, name, org, &format!("{SYS}Organization"))?;
    let organization_id = system.subject_id(&organization)?;
    for triple in system.triples_sp(organization_id, database) {
        let Some(candidate) = system.id_object_node(triple.object) else {
            continue;
        };
        let Some(candidate_id) = system.subject_id(&candidate) else {
            continue;
        };
        let matches = system
            .single_triple_sp(candidate_id, name)
            .and_then(|triple| system.id_object(triple.object))
            .map(|object| match object {
                ObjectType::Value(value) => value_to_string(&value).into_owned() == db,
                ObjectType::Node(node) => node == db,
            })
            .unwrap_or(false);
        if matches {
            return Some(candidate);
        }
    }
    None
}

/// Find a subject of the given type whose `name` is `wanted`.
fn subject_with_name(
    system: &SyncStoreLayer,
    name: u64,
    wanted: &str,
    class: &str,
) -> Option<String> {
    let type_ = system.predicate_id(RDF_TYPE)?;
    let class_id = system.object_node_id(class)?;
    for triple in system.triples_p(type_) {
        if triple.object != class_id {
            continue;
        }
        let matches = system
            .single_triple_sp(triple.subject, name)
            .and_then(|triple| system.id_object(triple.object))
            .map(|object| match object {
                ObjectType::Value(value) => value_to_string(&value).into_owned() == wanted,
                ObjectType::Node(node) => node == wanted,
            })
            .unwrap_or(false);
        if matches {
            return system.id_subject(triple.subject);
        }
    }
    None
}

/// Does `user` hold every one of `actions` over `database`?
///
/// This is `auth_action_scope/4`'s user clause: a capability whose role grants
/// the action, and whose scope reaches the database through the reflexive
/// transitive closure of `child` and `database` — so a capability over an
/// organization covers the databases under it.
fn has_actions(
    system: &SyncStoreLayer,
    user: &str,
    database: &str,
    actions: &[&str],
) -> bool {
    let Some(user_id) = system.subject_id(user) else {
        return false;
    };
    let (Some(capability), Some(role), Some(action), Some(scope)) = (
        system.predicate_id(&format!("{SYS}capability")),
        system.predicate_id(&format!("{SYS}role")),
        system.predicate_id(&format!("{SYS}action")),
        system.predicate_id(&format!("{SYS}scope")),
    ) else {
        return false;
    };

    let mut granted: HashSet<String> = HashSet::new();
    for capability_triple in system.triples_sp(user_id, capability) {
        let Some(capability_iri) = system.id_object_node(capability_triple.object) else {
            continue;
        };
        let Some(capability_id) = system.subject_id(&capability_iri) else {
            continue;
        };
        // Does this capability's scope reach the database?
        let reaches = system
            .triples_sp(capability_id, scope)
            .filter_map(|triple| system.id_object_node(triple.object))
            .any(|from| scope_reaches(system, &from, database));
        if !reaches {
            continue;
        }
        for role_triple in system.triples_sp(capability_id, role) {
            let Some(role_iri) = system.id_object_node(role_triple.object) else {
                continue;
            };
            let Some(role_id) = system.subject_id(&role_iri) else {
                continue;
            };
            for action_triple in system.triples_sp(role_id, action) {
                if let Some(action_iri) = system.id_object_node(action_triple.object) {
                    granted.insert(action_iri);
                }
            }
        }
    }

    actions.iter().all(|wanted| {
        let iri = format!("{SYS}Action/{wanted}");
        granted.contains(&iri)
    })
}

/// Reflexive transitive closure over `child` and `database`, which is how a
/// capability scoped at an organization covers the databases beneath it.
fn scope_reaches(system: &SyncStoreLayer, from: &str, target: &str) -> bool {
    // Each edge is looked up on its own. Requiring both to exist was a bug: a
    // default system graph has no `child` triple at all (nothing nests
    // organizations), so demanding both made `predicate_id` fail and reduced
    // the closure to exact match — an organization-scoped capability then
    // covered no database, which is the common shape for a real deployment.
    let edges: Vec<u64> = [format!("{SYS}child"), format!("{SYS}database")]
        .iter()
        .filter_map(|predicate| system.predicate_id(predicate))
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(from.to_string());
    while let Some(current) = queue.pop_front() {
        if current == target {
            return true;
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        let Some(current_id) = system.subject_id(&current) else {
            continue;
        };
        for predicate in edges.iter().copied() {
            for triple in system.triples_sp(current_id, predicate) {
                if let Some(next) = system.id_object_node(triple.object) {
                    if !seen.contains(&next) {
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    false
}

/// Parse an HTTP `Basic` credential.
pub fn parse_basic(header: &str) -> Option<(String, String)> {
    use base64::Engine;
    let encoded = header.strip_prefix("Basic ")?.trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

impl From<Denied> for io::Error {
    fn from(denied: Denied) -> Self {
        match denied {
            Denied::Unauthenticated => {
                io::Error::new(io::ErrorKind::PermissionDenied, "not authenticated")
            }
            Denied::Forbidden => io::Error::new(io::ErrorKind::PermissionDenied, "forbidden"),
            Denied::Unavailable(reason) => io::Error::new(io::ErrorKind::Other, reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real hash of `root` for `User/admin`, taken from a freshly
    /// initialised store. If SWI's format or TerminusDB's cost ever changes,
    /// this is where it shows up.
    const ADMIN_ROOT: &str = "$pbkdf2-sha512$t=1024$50XXvCIx4F5wbXHrYjP8mg$03xrGVPgnpemG463NULbSq1VtSOQ5W2DRSZgLaE6VCpQVtxCHDNP5ZHxYGdtI6L624BTyYNr1Ocj8cGDxvaiLg";

    #[test]
    fn a_real_terminusdb_hash_verifies_and_a_wrong_password_does_not() {
        assert!(verify_password("root", ADMIN_ROOT));
        assert!(!verify_password("roo", ADMIN_ROOT));
        assert!(!verify_password("root ", ADMIN_ROOT));
        assert!(!verify_password("", ADMIN_ROOT));
    }

    #[test]
    fn a_malformed_or_unknown_hash_fails_closed() {
        for stored in [
            "",
            "root",
            "$pbkdf2-sha256$t=1024$c2FsdA$aGFzaA",
            "$pbkdf2-sha512$t=0$c2FsdA$aGFzaA",
            "$pbkdf2-sha512$1024$c2FsdA$aGFzaA",
            "$pbkdf2-sha512$t=1024$c2FsdA",
            "$pbkdf2-sha512$t=1024$c2FsdA$aGFzaA$extra",
            "$pbkdf2-sha512$t=1024$c2FsdA$",
        ] {
            assert!(!verify_password("root", stored), "accepted {stored:?}");
        }
    }

    #[test]
    fn basic_credentials_are_parsed_and_anything_else_refused() {
        assert_eq!(
            Some(("admin".to_string(), "root".to_string())),
            parse_basic("Basic YWRtaW46cm9vdA==")
        );
        // A password containing a colon survives: only the first splits.
        assert_eq!(
            Some(("u".to_string(), "a:b".to_string())),
            parse_basic("Basic dTphOmI=")
        );
        assert_eq!(None, parse_basic("Bearer YWRtaW46cm9vdA=="));
        assert_eq!(None, parse_basic("Basic not-base64!"));
        assert_eq!(None, parse_basic("Basic bm9jb2xvbg=="));
    }

    /// Build a system graph the way a real one is shaped, with a user whose
    /// single capability is scoped as the argument says.
    fn system_store(scope: &str) -> SyncStore {
        use terminusdb_store_prolog::terminus_store::store::sync::open_sync_memory_store;
        use terminusdb_store_prolog::terminus_store::ValueTriple;

        let store = open_sync_memory_store();
        let builder = store.create_base_layer().unwrap();
        let node = |s: &str, p: &str, o: &str| ValueTriple::new_node(s, p, o);
        let text = |s: &str, p: &str, o: &str| ValueTriple::new_string_value(s, p, o);
        let org = format!("{SYSTEM_BASE}Organization/admin");
        let database = format!("{SYSTEM_BASE}UserDatabase/abc123");
        let user = format!("{SYSTEM_BASE}User/bob");
        let capability = format!("{SYSTEM_BASE}Capability/cap1");
        let role = format!("{SYSTEM_BASE}Role/consumer");

        for triple in [
            node(&org, RDF_TYPE, &format!("{SYS}Organization")),
            text(&org, &format!("{SYS}name"), "admin"),
            // NOTE: no `child` triple anywhere, exactly like a default store.
            node(&org, &format!("{SYS}database"), &database),
            node(&database, RDF_TYPE, &format!("{SYS}UserDatabase")),
            text(&database, &format!("{SYS}name"), "people"),
            text(&user, &format!("{SYS}name"), "bob"),
            text(&user, &format!("{SYS}key_hash"), ADMIN_ROOT),
            node(&user, &format!("{SYS}capability"), &capability),
            node(&capability, &format!("{SYS}scope"), scope),
            node(&capability, &format!("{SYS}role"), &role),
            node(&role, &format!("{SYS}action"), &format!("{SYS}Action/instance_read_access")),
            node(&role, &format!("{SYS}action"), &format!("{SYS}Action/commit_read_access")),
        ] {
            builder.add_value_triple(triple).unwrap();
        }
        let layer = builder.commit().unwrap();
        // `safe_create_named_graph/3` www-form-encodes the name.
        let graph = store.create("terminusdb%3a%2f%2f%2fsystem%2fdata").unwrap();
        graph.set_head(&layer).unwrap();
        store
    }

    fn bob() -> Identity {
        Identity::User {
            iri: format!("{SYSTEM_BASE}User/bob"),
            name: "bob".to_string(),
        }
    }

    #[test]
    fn an_organization_scoped_capability_covers_the_databases_under_it() {
        // The shape TerminusDB ships: `Capability/server_access` is scoped at
        // an organization, not a database. Requiring both closure edges to
        // exist used to break this, because a default system graph has no
        // `child` triple and the lookup failed closed to exact match.
        let store = system_store(&format!("{SYSTEM_BASE}Organization/admin"));
        let authorizer = Authorizer::new(store);
        assert!(authorizer.authorize(&bob(), "admin", "people").is_ok());
    }

    #[test]
    fn a_database_scoped_capability_covers_that_database_only() {
        let store = system_store(&format!("{SYSTEM_BASE}UserDatabase/abc123"));
        let authorizer = Authorizer::new(store);
        assert!(authorizer.authorize(&bob(), "admin", "people").is_ok());
        assert!(matches!(
            authorizer.authorize(&bob(), "admin", "absent"),
            Err(Denied::Forbidden)
        ));
    }

    #[test]
    fn a_capability_scoped_somewhere_else_covers_nothing_here() {
        let store = system_store(&format!("{SYSTEM_BASE}Organization/other"));
        let authorizer = Authorizer::new(store);
        assert!(matches!(
            authorizer.authorize(&bob(), "admin", "people"),
            Err(Denied::Forbidden)
        ));
    }

    #[test]
    fn the_super_user_needs_no_capability_at_all() {
        let store = system_store(&format!("{SYSTEM_BASE}Organization/other"));
        let authorizer = Authorizer::new(store);
        assert!(authorizer.authorize(&Identity::SuperUser, "admin", "people").is_ok());
    }

    #[test]
    fn authentication_reads_the_system_graph_and_rejects_a_bad_password() {
        let store = system_store(&format!("{SYSTEM_BASE}Organization/admin"));
        let authorizer = Authorizer::new(store);
        // `ADMIN_ROOT` is the hash of "root"; the fixture gives it to bob.
        assert_eq!(bob(), authorizer.authenticate("bob", "root").unwrap());
        assert!(matches!(
            authorizer.authenticate("bob", "wrong"),
            Err(Denied::Unauthenticated)
        ));
        assert!(matches!(
            authorizer.authenticate("nosuch", "root"),
            Err(Denied::Unauthenticated)
        ));
    }

    #[test]
    fn base64_is_decoded_under_either_alphabet() {
        // `>` encodes as `Pg` in both; the alphabets differ only for 62/63.
        assert_eq!(Some(vec![0xff, 0xff]), decode_b64("//8"));
        assert_eq!(Some(vec![0xff, 0xff]), decode_b64("__8"));
        assert_eq!(None, decode_b64("!!!"));
    }
}
