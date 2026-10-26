//! A DDG-style bang search engine in one binary.
//!
//! `GET /?q=v<bang> <terms>` looks the bang up in `user.json` and redirects to
//! it; anything else goes to Google. `/` is the only route — this is an API
//! server, not a web app, with no static content. `GET` searches, `HEAD`
//! answers 200 as an existence check, and every other method answers 404. See
//! `README.md` for the behaviour notes, the configuration, and the logging
//! format.
//!
//! The bare path works because there is nothing else to serve: no homepage,
//! stylesheet or repository file that would need a distinct route.

mod core;
mod url;

use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::any,
};
use serde::Deserialize;
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 3000;

/// Query parameters of `/`.
///
/// `Option`, not `String`: a bare `q: String` makes the `Query` extractor
/// answer 400 for a missing key, when what the client deserves is a 404 saying
/// the query is missing. `search` drops an empty `q` the same way as an absent
/// one, so `?q=` is a 404 too, not a redirect to Google with nothing after the
/// `q=`.
#[derive(Debug, Deserialize)]
struct ApiQuery {
    q: Option<String>,
}

/// `GET` searches, `HEAD` is an existence check, everything else is a 404.
///
/// The route is registered with `any` so axum does not turn the other methods
/// into a 405 carrying an `Allow` header — there is nothing to enumerate,
/// because only GET does anything. `Query` still runs first, so a query axum
/// cannot parse is its 400 on any method.
async fn api(
    method: Method,
    State(bangs): State<Arc<[core::Bang]>>,
    Query(query): Query<ApiQuery>,
) -> Response {
    match method {
        Method::HEAD => StatusCode::OK.into_response(),
        Method::GET => search(bangs, query.q),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Resolve `q` and redirect. Plain `fn`, so the method dispatch above stays a
/// match with no awaits.
fn search(bangs: Arc<[core::Bang]>, q: Option<String>) -> Response {
    let Some(q) = q.filter(|q| !q.is_empty()) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "No query parameter provided" })),
        )
            .into_response();
    };

    // The one log line: what was searched for. In `search` rather than at the
    // route entry, so `HEAD` liveness probes and the methods that answer 404
    // stay silent — those can be frequent and carry nothing of interest.
    // Nothing else is written, and in particular not the destination URL,
    // which a bang may point at a paywalled or confidential place.
    eprintln!("query: {q}");

    match core::resolve(&q, &bangs) {
        core::Decision::Bang { url, status } | core::Decision::Search { url, status } => {
            // The status travels with the `Decision`, so the 302/303 split is
            // decided next to the logic and not here.
            (status, Redirect::to(&url)).into_response()
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let loaded = match core::load_bangs(Path::new("user.json")) {
        Ok(bangs) => bangs,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };
    let bangs: Arc<[core::Bang]> = Arc::from(loaded);
    let bang_count = bangs.len();

    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let router = Router::new().route("/", any(api)).with_state(bangs);

    eprintln!("listening on {addr}, {bang_count} bangs");
    axum::serve(TcpListener::bind(addr).await?, router).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::header;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app(bangs: Vec<core::Bang>) -> Router {
        Router::new()
            .route("/", any(api))
            .with_state(Arc::from(bangs))
    }

    async fn call(method: axum::http::Method, uri: &str) -> axum::http::Response<Body> {
        let request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        app(vec![core::Bang {
            t: "b".into(),
            u: "https://example.org/?q={{{s}}}".into(),
        }])
        .oneshot(request)
        .await
        .unwrap()
    }

    async fn location(uri: &str) -> (StatusCode, Option<String>) {
        let response = call(axum::http::Method::GET, uri).await;
        (
            response.status(),
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok().map(String::from)),
        )
    }

    #[tokio::test]
    async fn bang_hit_redirects_302() {
        let (status, location) = location("/?q=vb+hello+world").await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            location.as_deref(),
            Some("https://example.org/?q=hello%20world")
        );
    }

    #[tokio::test]
    async fn an_unknown_query_parameter_is_ignored() {
        // Only a duplicate key is rejected. An unknown parameter is ignored, so
        // this answers exactly like `bang_hit_redirects_302` without the `foo`.
        let (status, location) = location("/?q=vb+hello+world&foo=bar").await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            location.as_deref(),
            Some("https://example.org/?q=hello%20world")
        );
    }

    #[tokio::test]
    async fn no_bang_redirects_303() {
        let (status, location) = location("/?q=hello+world").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(
            location
                .unwrap()
                .starts_with("https://www.google.com/search?newwindow=1&q=")
        );
    }

    /// A missing `q` and an empty one are the same error: `search` filters
    /// empty strings, so `?q=` reaches the same 404 JSON as `/`.
    async fn missing_q(uri: &str) {
        let response = call(axum::http::Method::GET, uri).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok().map(str::to_lowercase)),
            Some("application/json".to_string()),
            "{uri}",
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "No query parameter provided", "{uri}");
    }

    #[tokio::test]
    async fn a_missing_q_is_a_404_json() {
        missing_q("/").await;
    }

    #[tokio::test]
    async fn an_empty_q_is_a_404_json() {
        missing_q("/?q=").await;
    }

    #[tokio::test]
    async fn head_is_an_existence_check() {
        // HEAD answers 200 whatever the query says. It is a liveness probe, not
        // a search, so a `q` that would 404 under GET is irrelevant here.
        let response = call(axum::http::Method::HEAD, "/").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::LOCATION).is_none());
        assert!(response.headers().get(header::CONTENT_TYPE).is_none());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.is_empty());
    }

    /// `any` is what keeps these at 404: registered with `get`, axum would turn
    /// them into a 405 carrying an `Allow: GET, HEAD` header.
    #[tokio::test]
    async fn every_other_method_is_a_404() {
        let calls = [
            (axum::http::Method::POST, "/"),
            (axum::http::Method::PUT, "/"),
            (axum::http::Method::DELETE, "/"),
            (axum::http::Method::PATCH, "/"),
            (axum::http::Method::OPTIONS, "/"),
            (axum::http::Method::POST, "/?q=vb+hello"),
        ];
        for (method, uri) in calls {
            let response = call(method.clone(), uri).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
            assert!(
                response.headers().get(header::ALLOW).is_none(),
                "{method} {uri} must not advertise allowed methods"
            );
        }
    }

    #[tokio::test]
    async fn a_duplicate_key_is_a_400() {
        // `Query` runs before the handler body, so this is axum's own 400 and
        // the same on any method: a duplicate key is a malformed request, not a
        // server fault.
        let response = call(axum::http::Method::GET, "/?q=a&q=b").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn nothing_besides_the_search_route_is_served() {
        // No static directory: nothing but the search route is served, and `/`
        // is the search route itself, so it is not here either. Asserting the
        // status only — each miss is a bare 404 with no body.
        for path in [
            "/index.html",
            "/style.css",
            "/user.json",
            "/robots.txt",
            "/nope",
        ] {
            let response = call(axum::http::Method::GET, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }
}
