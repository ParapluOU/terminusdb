/** <module> Release integration tests for explicit WOQL query proofs.
 *
 * The suite is serial and opt-in because one proof exercises the full Dory
 * prover. Set TERMINUSDB_QUERY_PROOF_RELEASE_TESTS=true to enable it.
 */

:- use_module(core(util/test_utils)).
:- use_module(core(triple), [retract_local_triple_store/1,
                             set_local_triple_store/1,
                             super_user_authority/1]).
:- use_module(library(filesex), [delete_directory_and_contents/1]).
:- use_module(library(yall)).
:- use_module(library(terminus_store), [open_archive_store/3,
                                       query_proof_layer_root/2,
                                       query_proof_verify_envelope/6]).

query_proof_release_tests_enabled :-
    getenv('TERMINUSDB_QUERY_PROOF_RELEASE_TESTS', Value),
    memberchk(Value, ['true', '1', 'yes']).

query_proof_test_stage(Stage) :-
    format(user_error, '% query-proof release stage: ~w~n', [Stage]).

query_proof_test_require(Goal, Stage) :-
    ( call(Goal)
    -> true
    ;  throw(error(query_proof_release_stage_failed(Stage), _))
    ).

query_proof_test_options(
    _{ files: [],
       all_witnesses: false,
       data_version: no_data_version,
       commit_info: commit_info{author:"query proof release test",
                                message:"query proof release test"},
       optimize: false,
       streaming: false,
       library: none
     }).

query_proof_test_system_auth(System_DB, Auth) :-
    open_descriptor(system_descriptor{}, System_DB),
    super_user_authority(Auth).

query_proof_test_run(Path, Query, Context, JSON) :-
    query_proof_test_system_auth(System_DB, Auth),
    query_proof_test_options(Options),
    woql_query_json(System_DB, Auth, some(Path), json_query(Query),
                    Context, _Version, _Meta, JSON, Options).

query_proof_test_run_with_proof(Path, Query, Root, Context, JSON, Envelope) :-
    query_proof_test_system_auth(System_DB, Auth),
    query_proof_test_options(Options),
    woql_query_json_with_proof(System_DB, Auth, some(Path), json_query(Query), Root,
                               Context, _Version, Meta, JSON, Envelope, Options),
    0 = Meta.transaction_retry_count.

query_proof_test_node(Node, _{'@type':'NodeValue', node:Node}).
query_proof_test_variable_node(Name, _{'@type':'NodeValue', variable:Name}).
query_proof_test_variable_value(Name, _{'@type':'Value', variable:Name}).
query_proof_test_node_value(Node, _{'@type':'Value', node:Node}).
query_proof_test_typed_value(Type, Lexical,
                             _{'@type':'Value',
                               data:_{'@type':Type, '@value':Lexical}}).
query_proof_test_lang_value(Language, Lexical,
                            _{'@type':'Value',
                              data:_{'@language':Language, '@value':Lexical}}).

query_proof_test_update(Type, Subject, Predicate, Object,
                        _{'@type':Type,
                          subject:Subject,
                          predicate:Predicate,
                          object:Object}).

query_proof_test_fixture_queries(Base, Child, Read) :-
    P = "http://example.com/proof/p",
    Tag = "http://example.com/proof/tag",
    query_proof_test_node(P, P_Node),
    query_proof_test_node(Tag, Tag_Node),
    query_proof_test_node("http://example.com/proof/alice", Alice),
    query_proof_test_node("http://example.com/proof/typed", Typed),
    query_proof_test_node("http://example.com/proof/lang", Lang),
    query_proof_test_node("http://example.com/proof/removed", Removed),
    query_proof_test_node("http://example.com/proof/child", Child_Subject),
    query_proof_test_node_value("http://example.com/proof/bob", Bob),
    query_proof_test_node_value("http://example.com/proof/gone", Gone),
    query_proof_test_node_value("http://example.com/proof/erin", Erin),
    query_proof_test_node_value("http://example.com/proof/t1", T1),
    query_proof_test_node_value("http://example.com/proof/t2", T2),
    query_proof_test_node_value("http://example.com/proof/t3", T3),
    query_proof_test_node_value("http://example.com/proof/t4", T4),
    query_proof_test_node_value("http://example.com/proof/t5", T5),
    query_proof_test_node_value("http://example.com/proof/t6", T6),
    query_proof_test_typed_value('xsd:integer', 42, Forty_Two),
    query_proof_test_lang_value("en", "hello", Hello),
    maplist([S,Pred,O,Q]>>query_proof_test_update('AddTriple', S, Pred, O, Q),
            [Alice,Alice,Typed,Lang,Removed,Alice,Alice,Typed,Lang,Removed],
            [P_Node,Tag_Node,P_Node,P_Node,P_Node,Tag_Node,Tag_Node,Tag_Node,Tag_Node,Tag_Node],
            [Bob,T1,Forty_Two,Hello,Gone,T2,T1,T3,T4,T5],
            Base_Queries),
    Base = _{'@type':'And', and:Base_Queries},
    query_proof_test_update('DeleteTriple', Removed, P_Node, Gone, Delete_Removed),
    query_proof_test_update('AddTriple', Child_Subject, P_Node, Erin, Add_Child),
    query_proof_test_update('AddTriple', Child_Subject, Tag_Node, T6, Add_Child_Tag),
    Child = _{'@type':'And', and:[Delete_Removed,Add_Child,Add_Child_Tag]},
    query_proof_test_variable_node("S", S_Var),
    query_proof_test_variable_value("O", O_Var),
    query_proof_test_variable_value("T", T_Var),
    P_Triple = _{'@type':'Triple', subject:S_Var, predicate:P_Node, object:O_Var},
    Tag_Triple = _{'@type':'Triple', subject:S_Var, predicate:Tag_Node, object:T_Var},
    Read = _{'@type':'Select', variables:["O","S"],
             query:_{'@type':'And', and:[P_Triple,Tag_Triple]}}.

query_proof_test_current_layer(Context, Layer) :-
    query_default_collection(Context, Transaction),
    [Instance_Object] = Transaction.instance_objects,
    Layer = Instance_Object.read.

% Independently run the normal compiler/executor to reconstruct the Store-ID
% rows consumed by the native verifier. This is verification, not proof generation.
query_proof_test_execution_rows(Path, Query, Layer, Variables, Rows) :-
    query_proof_test_system_auth(System_DB, Auth),
    query_proof_test_options(Options),
    prepare_woql_query(System_DB, Auth, some(Path), json_query(Query), AST,
                       Context, Requested_Data_Version, Options),
    compile_query(AST, Prog, Context, Output_Context, Options),
    query_default_collection(Output_Context, Transaction),
    transaction_data_version(Transaction, Actual_Data_Version),
    compare_data_versions(Requested_Data_Version, Actual_Data_Version),
    with_transaction(
        Output_Context,
        findall(Row,
                ( woql_compile:Prog,
                  query_response:proof_binding_row(
                      some(Query, ignored, ignored), Output_Context, Transaction, Row)
                ),
                Rows),
        _Meta),
    query_response:context_variable_names(Output_Context, Variable_Atoms),
    maplist(atom_string, Variable_Atoms, Variables),
    query_proof_test_current_layer(Output_Context, Layer).

query_proof_test_rejected(Goal) :-
    catch((call(Goal), Result = accepted), _, Result = rejected),
    Result == rejected.

query_proof_test_wrong_root(Root, Wrong) :-
    string_codes(Root, [First|Rest]),
    ( First =:= 48 -> Replacement = 49 ; Replacement = 48 ),
    string_codes(Wrong, [Replacement|Rest]).

query_proof_test_corrupt_envelope(Envelope, Corrupt) :-
    ( string(Envelope)
    -> string_codes(Envelope, [First|Tail]),
       Corrupt_First is First xor 1,
       string_codes(Corrupt, [Corrupt_First|Tail])
    ;  Envelope = [First|Tail],
       Corrupt_First is First xor 1,
       Corrupt = [Corrupt_First|Tail]
    ).

query_proof_test_assert_json_semantics(JSON) :-
    ['O','S'] = JSON.'api:variable_names',
    Bindings = JSON.bindings,
    length(Bindings, 5),
    msort(Bindings, Sorted),
    append(_, [Duplicate,Duplicate|_], Sorted),
    member(Typed, Bindings),
    get_dict('O', Typed, Typed_Object),
    is_dict(Typed_Object),
    get_dict('@type', Typed_Object, 'xsd:integer'),
    get_dict('@value', Typed_Object, Typed_Value),
    Typed_Value =:= 42,
    member(Language, Bindings),
    get_dict('O', Language, Language_Object),
    is_dict(Language_Object),
    get_dict('@language', Language_Object, en),
    get_dict('@value', Language_Object, "hello"),
    \+ ( member(Binding, Bindings),
         get_dict('S', Binding, "http://example.com/proof/removed")
       ).

query_proof_test_count_query(Query,
                             _{'@type':'Count',
                               query:Query,
                               count:_{'@type':'DataValue', variable:"Count"}}).

query_proof_test_assert_count_json(JSON) :-
    ['Count'] = JSON.'api:variable_names',
    [Binding] = JSON.bindings,
    get_dict('Count', Binding, Count_Object),
    is_dict(Count_Object),
    get_dict('@type', Count_Object, 'xsd:decimal'),
    get_dict('@value', Count_Object, Count),
    Count =:= 5.

query_proof_test_ground_queries(Present, Absent, Unknown) :-
    query_proof_test_node("http://example.com/proof/alice", Alice),
    query_proof_test_node("http://example.com/proof/p", Predicate),
    query_proof_test_node_value("http://example.com/proof/bob", Bob),
    query_proof_test_node_value("http://example.com/proof/erin", Erin),
    query_proof_test_node_value("http://example.com/proof/unknown", Unknown_Object),
    Present = _{'@type':'Triple', subject:Alice, predicate:Predicate, object:Bob},
    Absent = _{'@type':'Triple', subject:Alice, predicate:Predicate, object:Erin},
    Unknown = _{'@type':'Triple', subject:Alice, predicate:Predicate,
                object:Unknown_Object}.

query_proof_test_ground_case(Path, Query, Root, Expected_Bindings,
                             Envelope, Variables, Rows) :-
    query_proof_test_run(Path, Query, _Ordinary_Context, Ordinary_JSON),
    query_proof_test_run_with_proof(Path, Query, Root, _Proof_Context,
                                    Proof_JSON, Envelope),
    query_proof_test_require(Ordinary_JSON = Proof_JSON, ground_json_changed),
    query_proof_test_require(Proof_JSON.'api:variable_names' = [],
                             ground_variables_changed),
    query_proof_test_require(Proof_JSON.bindings = Expected_Bindings,
                             ground_bindings_changed),
    query_proof_test_execution_rows(Path, Query, Layer, Variables, Rows),
    atom_json_dict(Query_Atom, Query, []),
    query_proof_verify_envelope(Layer, Query_Atom, Root, Variables, Rows, Envelope).

query_proof_test_ground_conjunction_queries(Present, Absent,
                                            Present_Conjunction, Absent_Conjunction) :-
    Present_Conjunction = _{'@type':'And', and:[Present,Present]},
    Absent_Conjunction = _{'@type':'And', and:[Present,Absent]}.

query_proof_test_ground_gated_query(Base_Query, Ground, Query) :-
    Base_And = Base_Query.query,
    append(Base_And.and, [Ground], Mixed_Atoms),
    Query = Base_Query.put(query, Base_And.put(and, Mixed_Atoms)).

query_proof_test_disconnected_query(Base_Query, Query) :-
    query_proof_test_node("http://example.com/proof/child", Child),
    query_proof_test_node("http://example.com/proof/p", Predicate),
    query_proof_test_variable_value("D", D),
    Component = _{'@type':'Triple', subject:Child,
                  predicate:Predicate, object:D},
    Base_And = Base_Query.query,
    append(Base_And.and, [Component], Product_Atoms),
    Query = Base_Query.put(query, Base_And.put(and, Product_Atoms)).

query_proof_test_or_query(Base_Query, Present_Ground, Query) :-
    [P_Triple|_] = Base_Query.query.and,
    Gated_Branch = _{'@type':'And', and:[P_Triple,Present_Ground]},
    Query = Base_Query.put(query,
        _{'@type':'Or', or:[P_Triple,Gated_Branch]}).

query_proof_test_transparent_wrapper_query(Path, Base_Query, Query) :-
    Query = _{'@type':'Using', collection:Path,
              query:_{'@type':'From', graph:"instance",
                      query:_{'@type':'Pin',
                              query:_{'@type':'Immediately', query:Base_Query}}}}.

query_proof_test_distinct_triple_query(
    _{'@type':'Distinct', variables:["S","T"],
      query:_{'@type':'Triple', subject:S, predicate:Tag, object:T}}) :-
    query_proof_test_variable_node("S", S),
    query_proof_test_variable_value("T", T),
    query_proof_test_node("http://example.com/proof/tag", Tag).

% A correlated Optional with both matched and unmatched left rows. The deleted
% `removed/p/gone` triple leaves `removed/tag/t5` as the one null-extended row.
query_proof_test_optional_query(
    _{'@type':'Select', variables:["O","S","T"],
      query:_{'@type':'And', and:[Left,_{'@type':'Optional', query:Right}]}}) :-
    query_proof_test_variable_node("S", S),
    query_proof_test_variable_value("O", O),
    query_proof_test_variable_value("T", T),
    query_proof_test_node("http://example.com/proof/p", P),
    query_proof_test_node("http://example.com/proof/tag", Tag),
    Left = _{'@type':'Triple', subject:S, predicate:Tag, object:T},
    Right = _{'@type':'Triple', subject:S, predicate:P, object:O}.

query_proof_test_verified_case(Path, Query, Root, Envelope, Variables, Rows) :-
    query_proof_test_run(Path, Query, _Ordinary_Context, Ordinary_JSON),
    query_proof_test_run_with_proof(Path, Query, Root, _Proof_Context,
                                    Proof_JSON, Envelope),
    query_proof_test_require(Ordinary_JSON = Proof_JSON, wrapper_json_changed),
    query_proof_test_execution_rows(Path, Query, Layer, Variables, Rows),
    atom_json_dict(Query_Atom, Query, []),
    query_proof_verify_envelope(Layer, Query_Atom, Root, Variables, Rows, Envelope).

query_proof_test_prepare_optional_archive(Dir,
                                          payload(Path,Query,Root,Envelope,
                                                  Variables,Rows)) :-
    setup_unattached_store(Store-Dir),
    setup_call_cleanup(
        set_local_triple_store(Store),
        ( create_db_without_schema("admin", "proof_optional"),
          Path = "admin/proof_optional",
          query_proof_test_fixture_queries(Base, Child, _Read),
          query_proof_test_run(Path, Base, _Base_Context, _Base_JSON),
          query_proof_test_run(Path, Child, _Child_Context, _Child_JSON),
          query_proof_test_optional_query(Query),
          query_proof_test_run(Path, Query, _Ordinary_Context, Ordinary_JSON),
          query_proof_test_execution_rows(Path, Query, Layer, Variables, Rows),
          query_proof_layer_root(Layer, Root),
          query_proof_test_run_with_proof(Path, Query, Root, _Proof_Context,
                                          Proof_JSON, Envelope),
          query_proof_test_require(Ordinary_JSON = Proof_JSON,
                                   optional_ordinary_json_changed),
          query_proof_test_require(Variables = ["O","S","T"],
                                   optional_variable_order_changed),
          query_proof_test_require(
              ( member([null,_,_], Rows),
                member([Matched_O,_,_], Rows), integer(Matched_O) ),
              optional_typed_rows_changed),
          query_proof_test_require(
              ( member(Unbound_Binding, Proof_JSON.bindings),
                \+ get_dict('O', Unbound_Binding, _) ),
              optional_json_unbound_semantics_changed)
        ),
        retract_local_triple_store(Store)),
    garbage_collect.

query_proof_test_prepare_closed_archive(Dir, Payload) :-
    setup_unattached_store(Store-Dir),
    setup_call_cleanup(
        set_local_triple_store(Store),
        ( create_db_without_schema("admin", "proof"),
          Path = "admin/proof",
          query_proof_test_fixture_queries(Base, Child, Query),
          query_proof_test_run(Path, Base, _Base_Context, _Base_JSON),
          query_proof_test_stage(base_committed),
          query_proof_test_run(Path, Child, _Child_Context, _Child_JSON),
          query_proof_test_stage(child_committed),
          % Limit is a valid ordinary WOQL wrapper and intentionally outside the
          % authenticated BGP planner. Its success demonstrates that /9 does not
          % accidentally compile or generate a proof.
          Ordinary_Only_Query = _{'@type':'Limit', limit:1, query:Query},
          query_proof_test_run(Path, Ordinary_Only_Query,
                               _Ordinary_Only_Context, Ordinary_Only_JSON),
          [_] = Ordinary_Only_JSON.bindings,
          query_proof_test_stage(ordinary_unsupported_proof_shape_complete),
          query_proof_test_run(Path, Query, Ordinary_Context, Ordinary_JSON),
          query_proof_test_stage(ordinary_query_complete),
          query_proof_test_current_layer(Ordinary_Context, Layer),
          query_proof_layer_root(Layer, Root),
          query_proof_test_run_with_proof(Path, Query, Root, Proof_Context,
                                          Proof_JSON, Envelope),
          query_proof_test_stage(proof_query_complete),
          query_proof_test_require(Ordinary_JSON = Proof_JSON, ordinary_json_changed),
          query_proof_test_current_layer(Proof_Context, _Proof_Layer),
          query_proof_test_require(query_proof_test_assert_json_semantics(Proof_JSON),
                                   json_semantics),
          query_proof_test_stage(json_semantics_checked),
          query_proof_test_execution_rows(Path, Query, Verify_Layer, Variables, Rows),
          atom_json_dict(Query_Atom, Query, []),
          query_proof_verify_envelope(Verify_Layer, Query_Atom, Root,
                                      Variables, Rows, Envelope),
          query_proof_test_stage(native_verify_complete),
          query_proof_test_count_query(Query, Count_Query),
          query_proof_test_run(Path, Count_Query,
                               _Ordinary_Count_Context, Ordinary_Count_JSON),
          query_proof_test_run_with_proof(Path, Count_Query, Root,
                                          _Proof_Count_Context, Proof_Count_JSON,
                                          Count_Envelope),
          query_proof_test_require(Ordinary_Count_JSON = Proof_Count_JSON,
                                   ordinary_count_json_changed),
          query_proof_test_require(query_proof_test_assert_count_json(Proof_Count_JSON),
                                   count_json_semantics),
          query_proof_test_execution_rows(Path, Count_Query, Count_Layer,
                                          Count_Variables, Count_Rows),
          atom_json_dict(Count_Query_Atom, Count_Query, []),
          query_proof_verify_envelope(Count_Layer, Count_Query_Atom, Root,
                                      Count_Variables, Count_Rows, Count_Envelope),
          query_proof_test_stage(count_native_verify_complete),
          query_proof_test_ground_queries(Present_Ground, Absent_Ground, Unknown_Ground),
          query_proof_test_ground_case(Path, Present_Ground, Root, [_{}],
                                       Present_Envelope, Present_Variables, Present_Rows),
          query_proof_test_require(Present_Rows = [[]], present_ground_rows_changed),
          query_proof_test_stage(present_ground_native_verify_complete),
          query_proof_test_ground_case(Path, Absent_Ground, Root, [],
                                       Absent_Envelope, Absent_Variables, Absent_Rows),
          query_proof_test_require(Absent_Rows = [], absent_ground_rows_changed),
          query_proof_test_stage(absent_ground_native_verify_complete),
          query_proof_test_ground_case(Path, Unknown_Ground, Root, [],
                                       Unknown_Envelope, Unknown_Variables, Unknown_Rows),
          query_proof_test_require(Unknown_Rows = [], unknown_ground_rows_changed),
          query_proof_test_stage(unknown_ground_native_verify_complete),
          query_proof_test_ground_conjunction_queries(
              Present_Ground, Absent_Ground,
              Present_Conjunction, Absent_Conjunction),
          query_proof_test_ground_case(Path, Present_Conjunction, Root, [_{}],
                                       Present_Conjunction_Envelope,
                                       Present_Conjunction_Variables,
                                       Present_Conjunction_Rows),
          query_proof_test_stage(present_ground_conjunction_native_verify_complete),
          query_proof_test_ground_case(Path, Absent_Conjunction, Root, [],
                                       Absent_Conjunction_Envelope,
                                       Absent_Conjunction_Variables,
                                       Absent_Conjunction_Rows),
          query_proof_test_stage(absent_ground_conjunction_native_verify_complete),
          query_proof_test_ground_gated_query(Query, Present_Ground,
                                              Present_Gated_Query),
          query_proof_test_verified_case(Path, Present_Gated_Query, Root,
                                         Present_Gated_Envelope,
                                         Present_Gated_Variables,
                                         Present_Gated_Rows),
          query_proof_test_require(Present_Gated_Rows = Rows,
                                   present_ground_gate_rows_changed),
          query_proof_test_stage(present_ground_gate_native_verify_complete),
          query_proof_test_ground_gated_query(Query, Absent_Ground,
                                              Absent_Gated_Query),
          query_proof_test_verified_case(Path, Absent_Gated_Query, Root,
                                         Absent_Gated_Envelope,
                                         Absent_Gated_Variables,
                                         Absent_Gated_Rows),
          query_proof_test_require(Absent_Gated_Rows = [],
                                   absent_ground_gate_rows_changed),
          query_proof_test_stage(absent_ground_gate_native_verify_complete),
          query_proof_test_disconnected_query(Query, Disconnected_Query),
          query_proof_test_verified_case(Path, Disconnected_Query, Root,
                                         Disconnected_Envelope,
                                         Disconnected_Variables,
                                         Disconnected_Rows),
          query_proof_test_require(Disconnected_Rows = Rows,
                                   disconnected_product_rows_changed),
          query_proof_test_require(Disconnected_Variables = Variables,
                                   disconnected_product_variables_changed),
          query_proof_test_stage(disconnected_product_native_verify_complete),
          query_proof_test_or_query(Query, Present_Ground, Or_Query),
          query_proof_test_verified_case(Path, Or_Query, Root,
                                         Or_Envelope, Or_Variables, Or_Rows),
          query_proof_test_require(length(Or_Rows, 8), or_bag_length_changed),
          query_proof_test_stage(or_bag_union_native_verify_complete),
          query_proof_test_transparent_wrapper_query(Path, Query, Wrapper_Query),
          query_proof_test_verified_case(Path, Wrapper_Query, Root,
                                         Wrapper_Envelope, Wrapper_Variables, Wrapper_Rows),
          query_proof_test_stage(transparent_wrappers_native_verify_complete),
          query_proof_test_distinct_triple_query(Distinct_Query),
          query_proof_test_verified_case(Path, Distinct_Query, Root,
                                         Distinct_Envelope, Distinct_Variables, Distinct_Rows),
          query_proof_test_stage(distinct_triple_native_verify_complete),
          Payload = payload(Path,Query,Root,Envelope,Variables,Rows,
                            Count_Query,Count_Envelope,Count_Variables,Count_Rows,
                            Present_Ground,Present_Envelope,Present_Variables,Present_Rows,
                            Absent_Ground,Absent_Envelope,Absent_Variables,Absent_Rows,
                            Unknown_Ground,Unknown_Envelope,Unknown_Variables,Unknown_Rows,
                            Present_Conjunction,Present_Conjunction_Envelope,
                            Present_Conjunction_Variables,Present_Conjunction_Rows,
                            Absent_Conjunction,Absent_Conjunction_Envelope,
                            Absent_Conjunction_Variables,Absent_Conjunction_Rows,
                            Present_Gated_Query,Present_Gated_Envelope,
                            Present_Gated_Variables,Present_Gated_Rows,
                            Absent_Gated_Query,Absent_Gated_Envelope,
                            Absent_Gated_Variables,Absent_Gated_Rows,
                            Disconnected_Query,Disconnected_Envelope,
                            Disconnected_Variables,Disconnected_Rows,
                            Or_Query,Or_Envelope,Or_Variables,Or_Rows,
                            Wrapper_Query,Wrapper_Envelope,Wrapper_Variables,Wrapper_Rows,
                            Distinct_Query,Distinct_Envelope,Distinct_Variables,Distinct_Rows)
        ),
        retract_local_triple_store(Store)),
    garbage_collect.

:- begin_tests(woql_query_proof_release,
               [condition(api_woql:query_proof_release_tests_enabled)]).

test(running_node_multilayer_archive_envelope,
     [ setup(api_woql:query_proof_test_prepare_closed_archive(Dir, Payload)),
       cleanup(delete_directory_and_contents(Dir))
     ]) :-
    Payload = payload(Path,Query,Root,Envelope,Variables,Rows,
                      Count_Query,Count_Envelope,Count_Variables,Count_Rows,
                      Present_Ground,Present_Envelope,Present_Variables,Present_Rows,
                      Absent_Ground,Absent_Envelope,Absent_Variables,Absent_Rows,
                      Unknown_Ground,Unknown_Envelope,Unknown_Variables,Unknown_Rows,
                      Present_Conjunction,Present_Conjunction_Envelope,
                      Present_Conjunction_Variables,Present_Conjunction_Rows,
                      Absent_Conjunction,Absent_Conjunction_Envelope,
                      Absent_Conjunction_Variables,Absent_Conjunction_Rows,
                      Present_Gated_Query,Present_Gated_Envelope,
                      Present_Gated_Variables,Present_Gated_Rows,
                      Absent_Gated_Query,Absent_Gated_Envelope,
                      Absent_Gated_Variables,Absent_Gated_Rows,
                      Disconnected_Query,Disconnected_Envelope,
                      Disconnected_Variables,Disconnected_Rows,
                      Or_Query,Or_Envelope,Or_Variables,Or_Rows,
                      Wrapper_Query,Wrapper_Envelope,Wrapper_Variables,Wrapper_Rows,
                      Distinct_Query,Distinct_Envelope,Distinct_Variables,Distinct_Rows),
    open_archive_store(Dir, 8, Reopened),
    setup_call_cleanup(
        set_local_triple_store(Reopened),
        ( query_proof_test_execution_rows(Path, Query, Layer, Variables, Rows),
          atom_json_dict(Query_Atom, Query, []),
          query_proof_verify_envelope(Layer, Query_Atom, Root,
                                      Variables, Rows, Envelope),
          query_proof_test_stage(reopen_verify_complete),
          query_proof_test_corrupt_envelope(Envelope, Corrupt),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Root,
                                              Variables, Rows, Corrupt)),
              corrupt_envelope_accepted),
          query_proof_test_stage(corrupt_envelope_rejected),
          query_proof_test_wrong_root(Root, Wrong_Root),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Wrong_Root,
                                              Variables, Rows, Envelope)),
              wrong_root_accepted),
          query_proof_test_stage(wrong_root_rejected),
          query_proof_test_fixture_queries(_Base, _Child, Wrong_Query0),
          [First_Atom,Second_Atom] = Wrong_Query0.query.and,
          Wrong_Query = Wrong_Query0.put(query,
              Wrong_Query0.query.put(and,
                  [Second_Atom,First_Atom])),
          atom_json_dict(Wrong_Query_Atom, Wrong_Query, []),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Wrong_Query_Atom, Root,
                                              Variables, Rows, Envelope)),
              wrong_query_accepted),
          query_proof_test_stage(wrong_query_rejected),
          Rows = [_Dropped|Missing_Row_Multiset],
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Root,
                                              Variables, Missing_Row_Multiset, Envelope)),
              wrong_multiplicity_accepted),
          query_proof_test_stage(wrong_multiplicity_rejected),
          query_proof_test_execution_rows(Path, Count_Query, Count_Layer,
                                          Count_Variables, Count_Rows),
          atom_json_dict(Count_Query_Atom, Count_Query, []),
          query_proof_verify_envelope(Count_Layer, Count_Query_Atom, Root,
                                      Count_Variables, Count_Rows, Count_Envelope),
          query_proof_test_stage(count_reopen_verify_complete),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Count_Layer, Count_Query_Atom, Root,
                                              Count_Variables, [[4]], Count_Envelope)),
              wrong_count_accepted),
          query_proof_test_stage(wrong_count_rejected),
          query_proof_test_execution_rows(Path, Present_Ground, Present_Layer,
                                          Present_Variables, Present_Rows),
          atom_json_dict(Present_Atom, Present_Ground, []),
          query_proof_verify_envelope(Present_Layer, Present_Atom, Root,
                                      Present_Variables, Present_Rows, Present_Envelope),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Present_Layer, Present_Atom, Root,
                                              Present_Variables, [], Present_Envelope)),
              present_ground_omission_accepted),
          query_proof_test_stage(present_ground_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Absent_Ground, Absent_Layer,
                                          Absent_Variables, Absent_Rows),
          atom_json_dict(Absent_Atom, Absent_Ground, []),
          query_proof_verify_envelope(Absent_Layer, Absent_Atom, Root,
                                      Absent_Variables, Absent_Rows, Absent_Envelope),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Absent_Layer, Absent_Atom, Root,
                                              Absent_Variables, [[]], Absent_Envelope)),
              absent_ground_insertion_accepted),
          query_proof_test_stage(absent_ground_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Unknown_Ground, Unknown_Layer,
                                          Unknown_Variables, Unknown_Rows),
          atom_json_dict(Unknown_Atom, Unknown_Ground, []),
          query_proof_verify_envelope(Unknown_Layer, Unknown_Atom, Root,
                                      Unknown_Variables, Unknown_Rows, Unknown_Envelope),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Unknown_Layer, Unknown_Atom, Root,
                                              Unknown_Variables, [[]], Unknown_Envelope)),
              unknown_ground_insertion_accepted),
          query_proof_test_stage(unknown_ground_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Present_Conjunction,
                                          Present_Conjunction_Layer,
                                          Present_Conjunction_Variables,
                                          Present_Conjunction_Rows),
          atom_json_dict(Present_Conjunction_Atom, Present_Conjunction, []),
          query_proof_verify_envelope(Present_Conjunction_Layer,
                                      Present_Conjunction_Atom, Root,
                                      Present_Conjunction_Variables,
                                      Present_Conjunction_Rows,
                                      Present_Conjunction_Envelope),
          query_proof_test_stage(present_ground_conjunction_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Absent_Conjunction,
                                          Absent_Conjunction_Layer,
                                          Absent_Conjunction_Variables,
                                          Absent_Conjunction_Rows),
          atom_json_dict(Absent_Conjunction_Atom, Absent_Conjunction, []),
          query_proof_verify_envelope(Absent_Conjunction_Layer,
                                      Absent_Conjunction_Atom, Root,
                                      Absent_Conjunction_Variables,
                                      Absent_Conjunction_Rows,
                                      Absent_Conjunction_Envelope),
          query_proof_test_stage(absent_ground_conjunction_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Present_Gated_Query,
                                          Present_Gated_Layer,
                                          Present_Gated_Variables,
                                          Present_Gated_Rows),
          atom_json_dict(Present_Gated_Atom, Present_Gated_Query, []),
          query_proof_verify_envelope(Present_Gated_Layer,
                                      Present_Gated_Atom, Root,
                                      Present_Gated_Variables,
                                      Present_Gated_Rows,
                                      Present_Gated_Envelope),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Present_Gated_Layer,
                                              Present_Gated_Atom, Root,
                                              Present_Gated_Variables, [],
                                              Present_Gated_Envelope)),
              present_ground_gate_omission_accepted),
          query_proof_test_stage(present_ground_gate_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Absent_Gated_Query,
                                          Absent_Gated_Layer,
                                          Absent_Gated_Variables,
                                          Absent_Gated_Rows),
          atom_json_dict(Absent_Gated_Atom, Absent_Gated_Query, []),
          query_proof_verify_envelope(Absent_Gated_Layer,
                                      Absent_Gated_Atom, Root,
                                      Absent_Gated_Variables,
                                      Absent_Gated_Rows,
                                      Absent_Gated_Envelope),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Absent_Gated_Layer,
                                              Absent_Gated_Atom, Root,
                                              Absent_Gated_Variables,
                                              Present_Gated_Rows,
                                              Absent_Gated_Envelope)),
              absent_ground_gate_insertion_accepted),
          query_proof_test_stage(absent_ground_gate_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Disconnected_Query,
                                          Disconnected_Layer,
                                          Disconnected_Variables,
                                          Disconnected_Rows),
          atom_json_dict(Disconnected_Atom, Disconnected_Query, []),
          query_proof_verify_envelope(Disconnected_Layer,
                                      Disconnected_Atom, Root,
                                      Disconnected_Variables,
                                      Disconnected_Rows,
                                      Disconnected_Envelope),
          [First_Component,Second_Component,Third_Component] =
              Disconnected_Query.query.and,
          Reordered_Disconnected_Query = Disconnected_Query.put(query,
              Disconnected_Query.query.put(and,
                  [Third_Component,First_Component,Second_Component])),
          atom_json_dict(Reordered_Disconnected_Atom,
                         Reordered_Disconnected_Query, []),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Disconnected_Layer,
                                              Reordered_Disconnected_Atom, Root,
                                              Disconnected_Variables,
                                              Disconnected_Rows,
                                              Disconnected_Envelope)),
              reordered_disconnected_components_accepted),
          [First_Variable,Second_Variable] = Disconnected_Variables,
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Disconnected_Layer,
                                              Disconnected_Atom, Root,
                                              [Second_Variable,First_Variable],
                                              Disconnected_Rows,
                                              Disconnected_Envelope)),
              reordered_disconnected_schema_accepted),
          [Duplicate_Row|_] = Disconnected_Rows,
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Disconnected_Layer,
                                              Disconnected_Atom, Root,
                                              Disconnected_Variables,
                                              [Duplicate_Row|Disconnected_Rows],
                                              Disconnected_Envelope)),
              duplicated_disconnected_product_row_accepted),
          query_proof_test_stage(disconnected_product_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Or_Query, Or_Layer,
                                          Or_Variables, Or_Rows),
          atom_json_dict(Or_Atom, Or_Query, []),
          query_proof_verify_envelope(Or_Layer, Or_Atom, Root,
                                      Or_Variables, Or_Rows, Or_Envelope),
          [First_Or_Branch,Second_Or_Branch] = Or_Query.query.or,
          Reordered_Or_Query = Or_Query.put(query,
              Or_Query.query.put(or,
                  [Second_Or_Branch,First_Or_Branch])),
          atom_json_dict(Reordered_Or_Atom, Reordered_Or_Query, []),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Or_Layer, Reordered_Or_Atom, Root,
                                              Or_Variables, Or_Rows,
                                              Or_Envelope)),
              reordered_or_branches_accepted),
          [_Dropped_Or_Row|Missing_Or_Row_Multiset] = Or_Rows,
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Or_Layer, Or_Atom, Root,
                                              Or_Variables,
                                              Missing_Or_Row_Multiset,
                                              Or_Envelope)),
              dropped_or_row_accepted),
          query_proof_test_corrupt_envelope(Or_Envelope, Corrupt_Or_Envelope),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Or_Layer, Or_Atom, Root,
                                              Or_Variables, Or_Rows,
                                              Corrupt_Or_Envelope)),
              corrupt_or_envelope_accepted),
          query_proof_test_stage(or_bag_union_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Wrapper_Query, Wrapper_Layer,
                                          Wrapper_Variables, Wrapper_Rows),
          atom_json_dict(Wrapper_Atom, Wrapper_Query, []),
          query_proof_verify_envelope(Wrapper_Layer, Wrapper_Atom, Root,
                                      Wrapper_Variables, Wrapper_Rows, Wrapper_Envelope),
          Wrong_Routing_Query = Wrapper_Query.put(collection, "admin/not-proof"),
          atom_json_dict(Wrong_Routing_Atom, Wrong_Routing_Query, []),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Wrapper_Layer, Wrong_Routing_Atom, Root,
                                              Wrapper_Variables, Wrapper_Rows,
                                              Wrapper_Envelope)),
              wrong_using_collection_accepted),
          query_proof_test_stage(transparent_wrappers_reopen_verify_complete),
          query_proof_test_execution_rows(Path, Distinct_Query, Distinct_Layer,
                                          Distinct_Variables, Distinct_Rows),
          atom_json_dict(Distinct_Atom, Distinct_Query, []),
          query_proof_verify_envelope(Distinct_Layer, Distinct_Atom, Root,
                                      Distinct_Variables, Distinct_Rows, Distinct_Envelope),
          query_proof_test_stage(distinct_triple_reopen_verify_complete)
        ),
        retract_local_triple_store(Reopened)).

test(correlated_optional_nullable_rows_survive_archive_reopen,
     [ setup(api_woql:query_proof_test_prepare_optional_archive(Dir, Payload)),
       cleanup(delete_directory_and_contents(Dir))
     ]) :-
    Payload = payload(Path,Query,Root,Envelope,Variables,Rows),
    open_archive_store(Dir, 8, Reopened),
    setup_call_cleanup(
        set_local_triple_store(Reopened),
        ( query_proof_test_execution_rows(Path, Query, Layer,
                                          Reopened_Variables, Reopened_Rows),
          query_proof_test_require(Reopened_Variables = Variables,
                                   optional_reopen_variables_changed),
          query_proof_test_require(Reopened_Rows = Rows,
                                   optional_reopen_rows_changed),
          atom_json_dict(Query_Atom, Query, []),
          query_proof_verify_envelope(Layer, Query_Atom, Root,
                                      Variables, Rows, Envelope),
          Unmatched = [null,Shared,T],
          member(Unmatched, Rows),
          member([Matched_O,_,_], Rows),
          integer(Matched_O),
          select(Unmatched, Rows, Missing_Unmatched),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Root,
                                              Variables, Missing_Unmatched,
                                              Envelope)),
              optional_unmatched_omission_accepted),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Root,
                                              Variables, [Unmatched|Rows],
                                              Envelope)),
              optional_unmatched_duplication_accepted),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Root,
                                              Variables,
                                              [[Matched_O,Shared,T]|Missing_Unmatched],
                                              Envelope)),
              optional_forged_nonzero_nullable_id_accepted),
          query_proof_test_require(
              query_proof_test_rejected(
                  query_proof_verify_envelope(Layer, Query_Atom, Root,
                                              Variables,
                                              [[null,null,T]|Missing_Unmatched],
                                              Envelope)),
              optional_null_shared_key_accepted),
          query_proof_test_stage(optional_archive_reopen_verify_complete)
        ),
        retract_local_triple_store(Reopened)).

:- end_tests(woql_query_proof_release).
