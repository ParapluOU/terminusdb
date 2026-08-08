:- module(query_response,[
              run_context_ast_jsonld_response/5,
              run_context_ast_jsonld_response/6,
              run_context_ast_jsonld_response_with_proof/9,
              run_context_ast_jsonld_streaming_response/4,
              pretty_print_query_response/3
          ]).

:- use_module(woql_compile).
:- use_module(ask).
:- use_module(jsonld).

:- use_module(core(api)).
:- use_module(core(util)).
:- use_module(core(transaction)).
:- use_module(core(triple/literals)).

:- use_module(library(apply)).
:- use_module(library(lists)).
:- use_module(library(pairs)).
:- use_module(library(yall)).

:- use_module(library(json)).
:- use_module(library(terminus_store), [object_id/3,
                                       query_proof_run_envelope/6]).

% Load document/json module to register json:json_write_hook/4 for rational precision
% This must be loaded AFTER library(json) to ensure the multifile hook is registered
:- use_module(core(document/json)).

run_context_ast_jsonld_response(Context, AST, Requested_Data_Version, Transaction_Meta_Data, Binding_JSON) :-
    Options = _{ optimize: true, all_witnesses: false, streaming: false},
    run_context_ast_jsonld_response(Context, AST, Requested_Data_Version, Transaction_Meta_Data, Binding_JSON, Options).

/** <module> Query Response
 *
 * Code to generate bindings for query response in JSON-LD
 */
run_context_ast_jsonld_response(Context, AST, Requested_Data_Version, Transaction_Meta_Data, Binding_JSON, Options) :-
    run_context_ast_jsonld_response_(Context, AST, Requested_Data_Version,
                                     Transaction_Meta_Data, Binding_JSON, Options,
                                     none).

run_context_ast_jsonld_response_with_proof(Context, AST, Query_JSON, Expected_Root,
                                           Requested_Data_Version, Transaction_Meta_Data,
                                           Binding_JSON, Envelope, Options) :-
    run_context_ast_jsonld_response_(Context, AST, Requested_Data_Version,
                                     Transaction_Meta_Data, Binding_JSON, Options,
                                     some(Query_JSON, Expected_Root, Envelope)).

run_context_ast_jsonld_response_(Context, AST, Requested_Data_Version, Transaction_Meta_Data,
                                 Binding_JSON, Options, Proof_Request) :-
    compile_query(AST,Prog,Context,Output_Context,Options),
    do_or_die(
        query_default_collection(Output_Context, Transaction),
        error(query_default_collection_failed_unexpectedly(Output_Context), _)),
    transaction_data_version(Transaction, Actual_Data_Version),
    compare_data_versions(Requested_Data_Version, Actual_Data_Version),
    with_transaction(
        Output_Context,
        query_response:(
            % Prog occurs only here: proof mode captures IDs alongside the ordinary
            % JSON binding during one enumeration per transaction attempt, never by
            % replaying the query for proof generation. The existing transaction retry
            % mechanism may, as for the non-proof path, replay an entire failed attempt.
            findall(JSON_Binding-Proof_Row,
                    (   woql_compile:Prog,
                        get_dict(bindings, Output_Context, Bindings),
                        * json_log_info_formatted('~N[Bindings] ~q~n', [Bindings]),
                        json_transform_binding_set(Output_Context, Bindings, JSON_Binding),
                        proof_binding_row(Proof_Request, Output_Context, Transaction, Proof_Row)),
                    Binding_Rows),
            pairs_keys_values(Binding_Rows, Binding_Set, Proof_Rows),
            % Keep proof generation inside the transaction body. object_id/3 above and
            % the native proof boundary below therefore use the exact same immutable
            % Transaction.instance_objects[0].read layer that Prog queried.
            maybe_create_query_proof(Proof_Request, Output_Context, Transaction, Proof_Rows),
            * json_log_info_formatted('~N[Binding Rows] ~q~n', [Binding_Rows])
        ),
        Meta_Data
    ),
    Transaction_Meta_Data = Meta_Data,
    context_variable_names(Output_Context, Names),
    Binding_JSON = json{'@type' : 'api:WoqlResponse',
                        'api:status' : 'api:success',
                        'api:variable_names' : Names,
                        'bindings' : Binding_Set,
                        % These are whitelisted metadata fields for serialization.
                        % Other fields (e.g. data_versions) cannot be serialized.
                        inserts : Meta_Data.inserts,
                        deletes : Meta_Data.deletes,
                        transaction_retry_count : Meta_Data.transaction_retry_count }.

proof_binding_row(none, _Context, _Transaction, []).
proof_binding_row(some(Query_JSON, _, _), Context, _Transaction, [Count]) :-
    query_proof_count_variable(Query_JSON, Count_Name),
    !,
    context_variable_names(Context, [Count_Name]),
    member(Record, Context.bindings),
    Record.var_name = Count_Name,
    query_proof_count_value(Record.woql_var, Count).
proof_binding_row(some(_, _, _), Context, Transaction, Row) :-
    context_variable_names(Context, Names),
    [Instance_Object] = Transaction.instance_objects,
    Layer = Instance_Object.read,
    maplist({Context,Layer}/[Name,Id]>>proof_binding_id(Context, Layer, Name, Id), Names, Row).

proof_binding_id(Context, Layer, Name, Id) :-
    member(Record, Context.bindings),
    Record.var_name = Name,
    Value = Record.woql_var,
    (   var(Value)
    ->  throw(error(query_proof_unbound_result(Name), _))
    ;   Value = Lexical^^Datatype
    ->  object_id(Layer, value(Lexical, Datatype), Id)
    ;   Value = Lexical@Language
    ->  object_id(Layer, lang(Lexical, Language), Id)
    ;   object_id(Layer, node(Value), Id)
    ).

query_proof_json_type(JSON, Type) :-
    get_dict('@type', JSON, Raw_Type),
    ( atom(Raw_Type)
    -> Type = Raw_Type
    ;  atom_string(Type, Raw_Type)
    ).

% Mirror only the Count-bearing wrapper chain accepted by woql2::plan_bgp.
% Rust remains authoritative and rejects any semantic mismatch; this traversal
% solely chooses the wire representation for the ordinary Count result.
query_proof_count_variable(Query, Name) :-
    query_proof_json_type(Query, Count),
    Count == 'Count',
    Count_Value = Query.count,
    get_dict(variable, Count_Value, Raw_Name),
    ( atom(Raw_Name)
    -> Name = Raw_Name
    ;  atom_string(Name, Raw_Name)
    ).
query_proof_count_variable(Query, Name) :-
    query_proof_json_type(Query, Wrapper),
    memberchk(Wrapper, ['Select','Using','From','Pin','Immediately']),
    query_proof_count_variable(Query.query, Name).

query_proof_count_value(Value, Count) :-
    ( integer(Value)
    -> Count = Value
    ;  Value = Lexical^^_Datatype,
       integer(Lexical),
       Count = Lexical
    ),
    Count >= 0,
    Count =< 18446744073709551615.

maybe_create_query_proof(none, _Context, _Transaction, _Rows).
maybe_create_query_proof(some(Query_JSON, Expected_Root, Envelope), Context, Transaction, Rows) :-
    context_variable_names(Context, Names),
    maplist(atom_string, Names, Variable_Strings),
    [Instance_Object] = Transaction.instance_objects,
    Layer = Instance_Object.read,
    atom_json_dict(Query_Atom, Query_JSON, []),
    query_proof_run_envelope(Layer, Query_Atom, Expected_Root,
                             Variable_Strings, Rows, Envelope).


run_context_ast_jsonld_streaming_response(Context, AST, Requested_Data_Version, Options) :-
    compile_query(AST,Prog,Context,Output_Context,Options),
    do_or_die(
        query_default_collection(Output_Context, Transaction),
        error(query_default_collection_failed_unexpectedly(Output_Context), _)),
    transaction_data_version(Transaction, Actual_Data_Version),
    compare_data_versions(Requested_Data_Version, Actual_Data_Version),
    write_preface_record(Output_Context),
    with_transaction(
        Output_Context,
        forall(woql_compile:Prog,
               query_response:(
                   get_dict(bindings, Output_Context, Bindings),
                   json_transform_binding_set(Output_Context, Bindings, JSON_Binding),
                   write_woql_json_binding(JSON_Binding)
               )
              ),
        Meta_Data
    ),
    meta_data_version(Transaction, Meta_Data, New_Data_Version),
    Postscript_Record = _{'@type' : 'PostscriptRecord',
                          status : success,
                          inserts : (Meta_Data.inserts),
                          deletes : (Meta_Data.deletes),
                          transaction_retry_count : (Meta_Data.transaction_retry_count) },
    (   serialize_data_version(New_Data_Version, Version_String)
    ->  put_dict(_{ version: Version_String },
                 Postscript_Record,
                 Postscript_Record_Final)
    ;   Postscript_Record = Postscript_Record_Final),
    json_write_dict(current_output,
                    Postscript_Record_Final,
                    [width(0)]),
    nl.

write_woql_json_binding(JSON_Binding) :-
    /* We may need to add some identifier here */
    put_dict(_{'@type' : 'Binding' }, JSON_Binding, Output_Binding),
    json_write_dict(current_output,
                    Output_Binding, [width(0)]),
    nl.

write_preface_record(Context) :-
    context_variable_names(Context, Names),
    Preface_Record = _{ '@type' : 'PrefaceRecord',
                       names : Names },
    json_write_dict(current_output,
                    Preface_Record,
                    [width(0)]),
    nl.

context_variable_names(Context, Header) :-
    get_dict(bindings, Context, Bindings),
    maplist([Record, Name]>>get_dict(var_name, Record, Name),
            Bindings, Rev),
    reverse(Rev,Header).

json_transform_binding_set(Context, Binding, JSON) :-
    % TODO: We probably want to "compress" the URIs using the context
    Prefixes = (Context.prefixes),
    maplist({Prefixes}/[Record,Var_Name=Term]>>(
                get_dict(var_name, Record, Var_Name),
                get_dict(woql_var, Record, Prolog_Var),
                (   var(Prolog_Var)
                ->  Term = null
                ;   term_jsonld(Prolog_Var, Prefixes, Term))),
            Binding,
            Data),
    dict_create(JSON, json, Data).

field_separator(true,"",_Size).
field_separator(false,Sep,Size) :-
    atomic_list_concat(["~` t~",Size,"| | "],Sep).

cumulative_([],_,[]).
cumulative_([X|Res],Acc,[Sum|Remainder]) :-
    Sum is X + Acc,
    cumulative_(Res,Sum,Remainder).

cumulative(List,Cum) :-
    cumulative_(List,0,Cum).

column_size(Name,Bindings,Size) :-
    atom_length(Name,Name_Length),
    foldl({Name}/[Binding,R,Max]>>(
              get_dict(Name,Binding,Res),
              format(atom(Val),"~w", Res),
              atom_length(Val,Length),
              Max is max(R,Length)
          ),
          Bindings,
          Name_Length,
          Size).

pretty_print_value(Value,Prefixes,Compressed) :-
    once(   atom(Value)
        ;   string(Value)),
    !,
    uri_to_prefixed(Value,Prefixes,Term),
    format(atom(Compressed), "~w", [Term]).
pretty_print_value(Literal,Prefixes,Compressed) :-
    is_dict(Literal),
    !,
    get_dict('@value',Literal,Value),
    (   get_dict('@type', Literal, Type)
    ->  uri_to_prefixed(Type,Prefixes,Type_Prefixed),
        format(atom(Compressed),'~q^^~q',[Value,Type_Prefixed])
    ;   get_dict('@language', Literal, Lang),
        format(atom(Compressed),'~q@~q',[Value,Lang])
    ).
pretty_print_value(List,Prefixes,Atom) :-
    is_list(List),
    !,
    maplist({Prefixes}/[Literal,Printed]>>pretty_print_value(Literal,Prefixes,Printed),
            List, Atoms),
    format(atom(Atom), '~w', [Atoms]).
pretty_print_value(Value,_Prefixes,_Compressed) :-
    throw(error(no_case_coverage_for_pretty_printer(Value))).

pretty_print_query_response(Response, Prefixes, String) :-

    Names = (Response.'api:variable_names'),
    Bindings = (Response.bindings),
    maplist({Prefixes,Names}/[Binding,New_Binding]>>(
                maplist({Prefixes,Names,Binding}/[Name,Name-New_Value]>>(
                            get_dict(Name,Binding,Value),
                            pretty_print_value(Value,Prefixes,New_Value)
                        ),
                        Names,
                        Dict_Values),
                dict_create(New_Binding,_,Dict_Values)
            ),Bindings,Format_Bindings),
    maplist({Format_Bindings}/[Name,Size]>>column_size(Name,Format_Bindings,Size),Names,Sizes),
    maplist([Size,New_Size]>>(New_Size is Size + 1),Sizes,New_Sizes),

    (   append(Offset_Sizes,[_],New_Sizes)

    ->  maplist([Size,Format]>>atomic_list_concat(['~w~` t~',Size,'+'],Format),Offset_Sizes,Offset_Formats),

        append(Offset_Formats,['~w'],Formats),
        atomic_list_concat(Formats,Format_String)
    ;   Format_String = 'No results'),

    with_output_to(
        string(String),
        (   format(current_output,Format_String, Names),
            format(current_output,"~n",[]),
            forall(
                member(Binding, Format_Bindings),
                (
                    maplist({Binding}/[Name,Value]>>
                            get_dict(Name,Binding,Value), Names, Values),
                    (   Values \= []
                    ->  format(current_output,Format_String, Values),
                        format(current_output,"~n",[])
                    ;   true)
                )
            ),

            get_dict(inserts, Response, Inserts),
            (   Inserts > 0
            ->  format(current_output, "Inserts: ~d~n", [Inserts])
            ;   true),

            get_dict(deletes, Response, Deletes),
            (   Deletes > 0
            ->  format(current_output, "Deletes: ~d~n", [Deletes])
            ;   true)
        )
    ).
