:- module(srv, [
              start_server/1
          ]).

:- use_module(core(plugins)).
:- use_module(core(appserver_hooks)).
:- use_module(core(triple/triplestore), [storage/1]).
:- use_module(core(util/json_log), [json_log_error_formatted/2]).
:- use_module(config(terminus_config), [root_redirect_target/1]).

%% start_server(+Port) is det.
%
%  Start the Rust webserver on Port. Static paths are normalized first, then
%  appserver_start is called with the configured root redirect target.
%  This is used by the main server when TERMINUSDB_SERVER_BACKEND=rust.
start_server(Port) :-
    appserver_hooks:normalize_static_paths,
    root_redirect_target(RedirectTarget),
    attach_difflog_feed,
    '$appserver':appserver_start(Port, RedirectTarget),
    % The hook worker drains derived-structure work published by commits. It
    % starts after the listener so a consumer registered during dylib load is
    % already in the registry; nothing is lost if it never starts, since every
    % consumer can derive its structure on demand.
    catch('$appserver':appserver_start_commit_hooks, Hook_Error,
          json_log_error_formatted("commit hooks not started: ~w", [Hook_Error])).

%% attach_difflog_feed is det.
%
%  Hand the process's store to the Rust diff-log feed, once, before the
%  listener binds. This is the feed's ONLY contact with Prolog: everything it
%  does afterwards (resolving a database, walking a commit chain, reading a
%  layer's delta, answering a GraphQL query) is Rust reading immutable layers,
%  so no amount of polling reaches the solver or the write path.
%
%  Failure is not fatal: the feed then reports itself unavailable and the rest
%  of the server starts as usual.
attach_difflog_feed :-
    (   current_predicate('$difflog':difflog_attach_store/1)
    ->  catch(
            (   storage(Store),
                '$difflog':difflog_attach_store(Store)
            ),
            Error,
            json_log_error_formatted("diff-log feed not attached: ~w", [Error])
        )
    ;   true
    ),
    !.
attach_difflog_feed.

% Destroy the request-dispatcher Prolog engine (tdb_http) at halt so it
% does not linger into SWI-Prolog's thread shutdown sweep. Registered
% unconditionally: appserver_shutdown succeeds when the dispatcher was
% never started.
:- at_halt(catch('$appserver':appserver_shutdown, _, true)).
