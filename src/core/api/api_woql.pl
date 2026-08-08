:- module(api_woql, [woql_query_json/9,
                     woql_query_json_with_proof/11,
                     woql_query_streaming_json/5]).
:- use_module(core(util)).
:- use_module(core(query)).
:- use_module(core(transaction)).
:- use_module(core(account)).
:- use_module(core(api/api_error)).

:- use_module(library(option)).
:- use_module(library(json)).

prepare_woql_query(System_DB, Auth, Path_Option, Query, AST, Context, Requested_Data_Version, Options) :-
    option(commit_info(Commit_Info0), Options),
    maybe_inject_auth_user(Auth, Commit_Info0, Commit_Info),
    option(all_witnesses(All_Witnesses), Options),
    option(files(Files), Options),
    option(data_version(Requested_Data_Version), Options),
    option(library(Maybe_Library), Options),

    % No descriptor to work with until the query sets one up
    (   some(Path) = Path_Option
    ->  do_or_die(resolve_absolute_string_descriptor(Path, Descriptor),
                  error(invalid_absolute_path(Path),_)),
        do_or_die(askable_context(Descriptor, System_DB, Auth, Commit_Info, Context0),
                  error(unresolvable_collection(Descriptor),_)),
        Context1 = (Context0.put(_{files:Files,
                                  library: Library,
                                  all_witnesses : All_Witnesses
                                 }))
    ;   none = Path_Option
    ->  empty_context(Empty),
        Context1 = (Empty.put(_{system: System_DB,
                               files: Files,
                               library: Library,
                               authorization : Auth,
                               all_witnesses : All_Witnesses,
                               commit_info : Commit_Info}))
    ;   throw(error(unexpected_path_option(Path_Option), _))
    ),

    (   Maybe_Library = some(Library)
    ->  do_or_die(
            resolve_absolute_string_descriptor(Library, Library_Descriptor),
            error(invalid_absolute_path(Library),_)
        ),
        check_descriptor_auth(System_DB, Library_Descriptor, '@schema':'Action/instance_read_access', Auth),
        open_descriptor(Library_Descriptor, Library_Transaction),
        Context = Context1.put(_{ library: Library_Transaction })
    ;   Context = Context1
    ),

    (   Query = json_query(JSON_Query)
    ->  json_woql(JSON_Query, AST)
    ;   Query = atom_query(Atom_Query)
    ->  atom_woql(Atom_Query, AST)
    ;   throw(error(unexpected_query_type(Query), _))
    ).

write_streaming_error_response(Error) :-
    api_error_jsonld(woql, Error, Error_Record),
    json_write_dict(current_output,
                    Error_Record,
                    [width(0)]),
    nl.

woql_query_streaming_json(System_DB, Auth, Path_Option, Query, Options) :-
    catch(
        (   prepare_woql_query(System_DB, Auth, Path_Option, Query, AST, Context, Requested_Data_Version, Options),
            run_context_ast_jsonld_streaming_response(Context, AST, Requested_Data_Version, Options)
        ),
        Exception,
        write_streaming_error_response(Exception)
    ).

woql_query_json(System_DB, Auth, Path_Option, Query, Context, New_Data_Version, Transaction_Meta_Data, JSON, Options) :-
    prepare_woql_query(System_DB, Auth, Path_Option, Query, AST, Context, Requested_Data_Version, Options),
    run_context_ast_jsonld_response(Context, AST, Requested_Data_Version, Transaction_Meta_Data, JSON, Options),
    query_default_collection(Context, Transaction),
    meta_data_version(Transaction, Transaction_Meta_Data, New_Data_Version).

% Explicit application/domain entry point. It enumerates the ordinary WOQL program once
% per transaction attempt and asks Rust to prove the captured Store-ID row multiset,
% without a second proof-only execution. Normal transaction retry semantics are
% unchanged. No route calls this predicate automatically and no envelope is persisted.
woql_query_json_with_proof(System_DB, Auth, Path_Option, Query, Expected_Root,
                           Context, New_Data_Version, Transaction_Meta_Data,
                           JSON, Envelope, Options) :-
    prepare_woql_query(System_DB, Auth, Path_Option, Query, AST, Context,
                       Requested_Data_Version, Options),
    (   Query = json_query(Query_JSON)
    ->  true
    ;   throw(error(query_proof_requires_json_query, _))
    ),
    run_context_ast_jsonld_response_with_proof(
        Context, AST, Query_JSON, Expected_Root, Requested_Data_Version,
        Transaction_Meta_Data, JSON, Envelope, Options),
    query_default_collection(Context, Transaction),
    meta_data_version(Transaction, Transaction_Meta_Data, New_Data_Version).

bind_vars([],_).
bind_vars([Name=Var|Tail],AST) :-
    Var = v(Name),
    bind_vars(Tail,AST).

atom_woql(Query, AST) :-
    read_term_from_atom(Query, AST, [variable_names(Names)]),
    bind_vars(Names,AST).
