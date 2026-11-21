:- module(api_info, [info/3]).

:- use_module(core(util)).
:- use_module(core(triple)).
:- use_module(config(terminus_config), [terminusdb_version/1,
                                        is_memory_mode/0,
                                        is_enterprise/0,
                                        indexer_backend/1]).

:- use_module(library(terminus_store), [terminus_store_version/1]).

% indexer_available/0 is a semidet predicate exported by plugin_api.
% It succeeds when the Rust indexer FFI predicates are registered.
% indexer_endpoint_available/0 and indexer_tokenizer_available/1 delegate
% to the vectorlink plugin (probe-cached at most once a second) so info
% never hammers the endpoint. We use autoload instead of explicit
% import to avoid load-order issues.
:- autoload(core(plugin_api), [indexer_available/0,
                               indexer_endpoint_available/0,
                               indexer_tokenizer_available/1]).

info(_System_DB, Auth, Info) :-

    % Community serves the full info payload to any caller, anonymous
    % included; enterprise redacts for anonymous instead.

    terminusdb_version(TerminusDB_Version),
    terminus_store_version(TerminusDB_Store_Version),
    current_prolog_flag(terminusdb_git_hash, Git_Hash),
    % In memory mode, there's no storage version file - use current database_version
    (   is_memory_mode
    ->  database_version(Storage_Version)
    ;   get_db_version(Storage_Version)
    ),
    number_string(Storage_Version, Storage_Version_String),
    server_edition(Edition),
    http_engine(Http_Engine),
    distribution(Distribution),
    server_extensions(Extensions),

    indexer_info(Indexer_Info),
    (   is_anonymous_authority(Auth)
    ->  Info = _{
               authority: Auth,
               edition: Edition,
               http_engine: Http_Engine,
               distribution: Distribution,
               extensions: Extensions,
               indexer: Indexer_Info,
               storage:
               _{
                   version: Storage_Version_String
               }
           }
    ;   Info = _{
               authority: Auth,
               edition: Edition,
               http_engine: Http_Engine,
               distribution: Distribution,
               extensions: Extensions,
               indexer: Indexer_Info,
               terminusdb :
               _{
                   version : TerminusDB_Version,
                   git_hash : Git_Hash
               },
               terminusdb_store :
               _{
                   version : TerminusDB_Store_Version
               },
               storage :
               _{
                   version: Storage_Version_String
               }
           }
    ).

%% distribution(-Distribution) is det.
%
%  Which build of TerminusDB this is. `paraplu` for this fork, which carries
%  surfaces upstream does not: see server_extensions/1 for the list.
%
%  A client should branch on the EXTENSIONS rather than on this. The
%  distribution says who built the server, not what it can do, and an older
%  paraplu build answers `paraplu` while lacking the newest surfaces — so
%  mapping a distribution name to a feature set means maintaining a version
%  table that silently goes stale. This is here for diagnostics and for support
%  questions ("what are you running?"), not as a capability check.
distribution(paraplu).

%% server_extensions(-Extensions) is det.
%
%  The non-upstream surfaces this build actually carries, as a list of atoms.
%
%  Derived from what is registered rather than declared from a literal: each
%  extension is detected through a predicate its own loader installs, so the
%  answer cannot drift from reality the way a hand-maintained list would. A
%  dylib that failed to register, or a build with a module compiled out,
%  reports the truth instead of a promise.
%
%    difflog       - the pollable per-commit diff log at
%                    /api/graphql/_difflog/<org>/<db>
%    commit_hooks  - after-commit fan-out for derived structures
server_extensions(Extensions) :-
    findall(Extension, server_extension(Extension), Extensions).

server_extension(difflog) :-
    current_predicate('$difflog':difflog_attach_store/1).
server_extension(commit_hooks) :-
    current_predicate('$appserver':appserver_publish_commit/7).

%% server_edition(-Edition) is det.
%
%  Reports which TerminusDB edition is running. The value is the atom
%  `community` for the open source build and `enterprise` when the
%  enterprise extension is loaded (terminusdb_enterprise flag set).
server_edition(enterprise) :-
    is_enterprise,
    !.
server_edition(community).

%% http_engine(-Engine) is det.
%
%  Reports which HTTP engine is serving requests. The value is the atom
%  `metal` when the Rust webserver is active (the default) and `swipl`
%  when the traditional SWI-Prolog HTTP server is selected via
%  TERMINUSDB_SERVER_BACKEND=swipl.
http_engine(Engine) :-
    (   getenv('TERMINUSDB_SERVER_BACKEND', BackendEnv)
    ->  atom_string(BackendEnv, BackendAtom),
        downcase_atom(BackendAtom, Backend),
        (   Backend == swipl
        ->  Engine = swipl
        ;   Engine = metal
        )
    ;   Engine = metal
    ).

%% indexer_info(-Info) is det.
%
%  Reports the indexer backend configuration and availability.
%  - backend: the configured backend atom (none, http_vectorlink, http_legacy_vectorlink)
%  - configured: true when backend is not 'none'
%  - available: true when the Rust indexer FFI predicates are registered
%    (i.e. the indexer runtime is loaded and operational)
%  - endpoint_available: an OpenAI-compatible endpoint answered the last
%    probe (cached, at most one probe per second) — what boot detection
%    keyed on. Boot-only for now; re-probing later is a naming-compatible
%    change.
%  - tokenizer_available: a tokenizer file the embedded engine can load
%    is present (env-pinned or shipped asset)
indexer_info(Info) :-
    (   catch(indexer_backend(Backend), _, Backend = none)
    ->  true
    ;   Backend = none
    ),
    (   Backend \= none
    ->  Configured = true
    ;   Configured = false
    ),
    (   Configured = true,
        catch(indexer_available, _, fail)
    ->  Available = true
    ;   Available = false
    ),
    (   catch(indexer_endpoint_available, _, fail)
    ->  Endpoint_Available = true
    ;   Endpoint_Available = false
    ),
    catch(indexer_tokenizer_available(Tokenizer_Available), _, false),
    Info = _{
        backend: Backend,
        configured: Configured,
        available: Available,
        endpoint_available: Endpoint_Available,
        tokenizer_available: Tokenizer_Available
    }.

%% is_anonymous_authority(+Auth) is semidet.
%
%  True when Auth is the anonymous user authority, i.e. the request was
%  made without any authentication credentials. The anonymous authority
%  URI is the same one asserted by authenticate/3 in routes.pl when no
%  authentication information is submitted.
is_anonymous_authority('terminusdb://system/data/User/anonymous') :- !.
is_anonymous_authority(doc:anonymous) :- !.

:- begin_tests(api_info, []).

test(distribution_names_this_fork, []) :-
    distribution(paraplu).

test(extensions_are_detected_not_declared, []) :-
    % The list comes from registered predicates, so in a build where the dylib
    % loaded both surfaces are present; in one where it did not, neither is.
    % Either way the answer matches reality rather than a literal someone
    % forgot to update.
    server_extensions(Extensions),
    is_list(Extensions),
    forall(member(Extension, Extensions),
           memberchk(Extension, [difflog, commit_hooks])),
    (   current_predicate('$difflog':difflog_attach_store/1)
    ->  memberchk(difflog, Extensions)
    ;   \+ memberchk(difflog, Extensions)
    ),
    (   current_predicate('$appserver':appserver_publish_commit/7)
    ->  memberchk(commit_hooks, Extensions)
    ;   \+ memberchk(commit_hooks, Extensions)
    ).
:- use_module(core(util/test_utils)).
:- use_module(core(triple/constants), [super_user_authority/1]).
:- use_module(config(terminus_config), [set_memory_mode/0,
                                        is_memory_mode/0,
                                        is_enterprise/0]).

% server_edition/1 is pure logic over the terminusdb_enterprise flag.
% It reports the atom `community` for the open source build and
% `enterprise` when the enterprise extension is loaded.
test(server_edition_is_atom) :-
    server_edition(Edition),
    atom(Edition).

test(server_edition_matches_enterprise_flag) :-
    server_edition(Edition),
    (   is_enterprise
    ->  Edition == enterprise
    ;   Edition == community
    ).

% http_engine/1 reports the HTTP engine serving requests. The default is
% `metal` (the Rust webserver); `swipl` is selected only when
% TERMINUSDB_SERVER_BACKEND=swipl is set.
test(http_engine_default_is_metal,
     [setup(unsetenv('TERMINUSDB_SERVER_BACKEND')),
      cleanup(unsetenv('TERMINUSDB_SERVER_BACKEND'))]) :-
    http_engine(Engine),
    Engine == metal.

test(http_engine_swipl_when_env_set,
     [setup(setenv('TERMINUSDB_SERVER_BACKEND', 'swipl')),
      cleanup(unsetenv('TERMINUSDB_SERVER_BACKEND'))]) :-
    http_engine(Engine),
    Engine == swipl.

test(http_engine_metal_when_env_set,
     [setup(setenv('TERMINUSDB_SERVER_BACKEND', 'rust')),
      cleanup(unsetenv('TERMINUSDB_SERVER_BACKEND'))]) :-
    http_engine(Engine),
    Engine == metal.

test(http_engine_case_insensitive,
     [setup(setenv('TERMINUSDB_SERVER_BACKEND', 'SWIPL')),
      cleanup(unsetenv('TERMINUSDB_SERVER_BACKEND'))]) :-
    http_engine(Engine),
    Engine == swipl.

% is_anonymous_authority/1 recognises both the anonymous URI used by the
% route handler and the doc:anonymous term.
test(is_anonymous_authority_uri) :-
    is_anonymous_authority('terminusdb://system/data/User/anonymous').
test(is_not_anonymous_authority_for_admin) :-
    \+ is_anonymous_authority('terminusdb://system/data/User/admin').

% indexer_info/1 always produces a dict with backend, configured, and available.
test(indexer_info_produces_dict) :-
    indexer_info(Info),
    get_dict(backend, Info, _),
    get_dict(configured, Info, _),
    get_dict(available, Info, _).

% Detection feedback: the info block names what the boot probe saw, so
% an agent can tell an unreachable endpoint from a missing tokenizer.
test(indexer_info_reports_endpoint_and_tokenizer) :-
    indexer_info(Info),
    get_dict(endpoint_available, Info, _),
    get_dict(tokenizer_available, Info, _).

% indexer_info/1 reports configured=false when backend is none.
test(indexer_info_none_backend_not_configured,
     [setup(terminus_config:clear_indexer_backend_config),
      cleanup(terminus_config:clear_indexer_backend_config)]) :-
    indexer_info(Info),
    get_dict(backend, Info, Backend),
    Backend == none,
    get_dict(configured, Info, false).

% info/3 for a logged-in user includes all sections.
test(info_logged_in_includes_all_sections,
     [setup((setup_temp_store(State),
             set_memory_mode)),
      cleanup((terminus_config:retractall(memory_mode_enabled),
               teardown_temp_store(State)))
     ]) :-
    super_user_authority(Auth),
    info(_System_DB, Auth, Info),
    get_dict(authority, Info, Auth),
    get_dict(edition, Info, _),
    get_dict(http_engine, Info, _),
    get_dict(indexer, Info, _),
    get_dict(terminusdb, Info, _),
    get_dict(terminusdb_store, Info, _),
    get_dict(storage, Info, _).

% info/3 for an anonymous user omits terminusdb and terminusdb_store
% but still includes the indexer section.
test(info_anonymous_omits_version_sections,
     [condition(\+ is_enterprise),
      setup((setup_temp_store(State),
             set_memory_mode)),
      cleanup((terminus_config:retractall(memory_mode_enabled),
               teardown_temp_store(State)))
     ]) :-
    info(_System_DB, 'terminusdb://system/data/User/anonymous', Info),
    get_dict(authority, Info, 'terminusdb://system/data/User/anonymous'),
    get_dict(edition, Info, _),
    get_dict(http_engine, Info, _),
    get_dict(indexer, Info, _),
    get_dict(storage, Info, _),
    \+ get_dict(terminusdb, Info, _),
    \+ get_dict(terminusdb_store, Info, _).

:- end_tests(api_info).
