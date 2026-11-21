//! The feed's HTTP surface, served by the Rust webserver.
//!
//! Three routes, none of which goes through Prolog:
//!
//! - `POST /api/v1/feed/graphql` — a GraphQL request, the normal way in.
//! - `GET  /api/v1/feed/graphql?query=…` — the same, for a consumer (or a
//!   `curl`) that would rather not POST.
//! - `GET  /api/v1/feed/status` — whether the feed is attached, and what its
//!   indexes currently hold.
//!
//! ## Authorization
//!
//! The same credential as the rest of the API: HTTP `Basic` with a TerminusDB
//! username and password, verified against the system graph, and then the same
//! capability check — `instance_read_access` and `commit_read_access` over the
//! database being read. There is no feed-specific secret, because a second
//! credential to provision and rotate is a worse interface than the one every
//! client already has, and a shared token cannot express "this consumer may
//! follow this database and no other". See [`super::auth`].
//!
//! `databases` is filtered rather than refused: a user sees the databases they
//! may read, which is what makes discovery usable for a non-admin.
//!
//! ## One rule for every handler here
//!
//! **No `SyncStore` call may happen in an async context.** `task_sync` drives
//! its own global runtime with `block_on`, and tokio panics if that is
//! attempted from a thread already driving a runtime — so reading a label,
//! opening a layer or verifying a password from a handler body takes the
//! connection down rather than returning an error. Everything that touches the
//! store, authentication included, runs inside the one `spawn_blocking` below.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::Query as AxumQuery;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use juniper::http::GraphQLRequest;
use serde::Deserialize;
use serde_json::json;

use super::auth::{self, Authorizer, Denied};
use super::graphql::{schema, FeedContext};
use super::{feed_handle, FeedState};

/// Routes to merge into the webserver's router.
pub fn router() -> Router {
    Router::new()
        .route("/api/v1/feed/graphql", post(post_graphql).get(get_graphql))
        .route("/api/v1/feed/status", get(status))
}

/// Turn a refusal into a response. `WWW-Authenticate` is set on an
/// authentication failure so an ordinary HTTP client knows to offer
/// credentials, exactly as the rest of the API does.
fn deny(denied: Denied) -> Response {
    match denied {
        Denied::Unauthenticated => (
            StatusCode::UNAUTHORIZED,
            [(
                axum::http::header::WWW_AUTHENTICATE,
                "Basic realm=\"terminusdb\"",
            )],
            Json(json!({"status": "unauthorized"})),
        )
            .into_response(),
        Denied::Forbidden => (
            StatusCode::FORBIDDEN,
            Json(json!({
                "status": "forbidden",
                "reason": "the feed needs instance_read_access and commit_read_access on this database"
            })),
        )
            .into_response(),
        Denied::Unavailable(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "unavailable", "reason": reason})),
        )
            .into_response(),
    }
}

/// A request with a credential attached but not yet checked. Extracting this
/// touches no store, so it is safe in an async handler; verifying it is not.
struct Pending {
    feed: Arc<super::feed::Feed>,
    authorizer: Arc<Authorizer>,
    user: String,
    password: String,
}

/// Parse the request's credential and pick up the attached state. Does not
/// verify anything, and deliberately reads nothing from the store.
fn pending(headers: &HeaderMap) -> Result<Pending, Response> {
    let FeedState::Attached(feed, authorizer) = feed_handle() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "unavailable",
                "reason": "no store is attached to the diff-log feed yet"
            })),
        )
            .into_response());
    };
    let credential = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(auth::parse_basic);
    let Some((user, password)) = credential else {
        return Err(deny(Denied::Unauthenticated));
    };
    Ok(Pending {
        feed,
        authorizer,
        user,
        password,
    })
}

/// Run a GraphQL request. The store reads inside are blocking (mmap, and
/// potentially a first-time chain walk), so they go to the blocking pool
/// rather than stalling a runtime worker that other connections need.
async fn execute(request: GraphQLRequest, pending: Pending) -> Response {
    // Authentication AND execution happen here, on a blocking-pool thread:
    // both read the store, and neither may run on a runtime worker.
    let result = tokio::task::spawn_blocking(move || {
        let identity = pending
            .authorizer
            .authenticate(&pending.user, &pending.password)
            .map_err(Outcome::Denied)?;
        let context = FeedContext {
            feed: pending.feed,
            authorizer: pending.authorizer,
            identity,
        };
        let schema = schema();
        let response = request.execute_sync(&schema, &context);
        let ok = response.is_ok();
        let body = serde_json::to_string(&response)
            .map_err(|error| Outcome::Failed(error.to_string()))?;
        Ok((ok, body))
    })
    .await;

    match result {
        Ok(Ok((ok, body))) => (
            if ok {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            },
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Ok(Err(Outcome::Denied(denied))) => deny(denied),
        Ok(Err(Outcome::Failed(reason))) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"errors": [{"message": reason}]})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"errors": [{"message": format!("feed task failed: {error}")}]})),
        )
            .into_response(),
    }
}

/// Why a blocking task did not produce a response body.
enum Outcome {
    Denied(Denied),
    Failed(String),
}

async fn post_graphql(headers: HeaderMap, body: Bytes) -> Response {
    let pending = match pending(&headers) {
        Ok(pending) => pending,
        Err(response) => return response,
    };
    let request: GraphQLRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"errors": [{"message": format!("malformed GraphQL request: {error}")}]})),
            )
                .into_response()
        }
    };
    execute(request, pending).await
}

#[derive(Deserialize)]
struct GetParams {
    query: String,
    #[serde(default)]
    variables: Option<String>,
    #[serde(default, rename = "operationName")]
    operation_name: Option<String>,
}

async fn get_graphql(headers: HeaderMap, AxumQuery(params): AxumQuery<GetParams>) -> Response {
    let pending = match pending(&headers) {
        Ok(pending) => pending,
        Err(response) => return response,
    };
    let variables = match params.variables.as_deref() {
        Some(raw) => match serde_json::from_str(raw) {
            Ok(variables) => Some(variables),
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"errors": [{"message": format!("malformed variables: {error}")}]})),
                )
                    .into_response()
            }
        },
        None => None,
    };
    let request = GraphQLRequest::new(params.query, params.operation_name, variables);
    execute(request, pending).await
}

async fn status(headers: HeaderMap) -> Response {
    let pending = match pending(&headers) {
        Ok(pending) => pending,
        Err(response) => return response,
    };
    // Authentication reads the system graph, so it goes to the blocking pool
    // like every other store access here.
    let result = tokio::task::spawn_blocking(move || {
        let identity = pending
            .authorizer
            .authenticate(&pending.user, &pending.password)?;
        Ok::<_, Denied>((identity.name().to_string(), pending.feed.stats()))
    })
    .await;
    match result {
        Ok(Ok((user, indexed))) => (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "user": user,
                // What this process is holding: cache occupancy, not anything
                // about a particular database's contents.
                "indexed": indexed,
            })),
        )
            .into_response(),
        Ok(Err(denied)) => deny(denied),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "error", "reason": format!("feed task failed: {error}")})),
        )
            .into_response(),
    }
}
