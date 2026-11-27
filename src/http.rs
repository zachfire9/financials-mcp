//! **MCP over Streamable HTTP**: the same server, a second transport.
//!
//! Over stdio, a client starts this program and talks to it through stdin
//! and stdout. Over HTTP, the client sends each JSON-RPC message as a
//! `POST /mcp` request and gets the response back in the HTTP response. That
//! lets clients that can't start a local process (Claude on the web or a
//! phone) reach the tools, once the server is hosted (Steps 20–22).
//!
//! The tools don't change at all: [`router`] wraps the same
//! [`FinancialsServer`] that `serve` uses over stdio.
//!
//! # Locked by default
//!
//! Every request must carry `Authorization: Bearer <MCP_BEARER_TOKEN>`
//! (checked in constant time, before the body is read), the `Host` header
//! must be on the allowed list, and bodies are capped at
//! [`MAX_REQUEST_BYTES`]. Each tool call is also limited to
//! [`TOOL_TIME_LIMIT`](crate::server::TOOL_TIME_LIMIT).
//!
//! # Stateless
//!
//! The server runs in rmcp's **stateless** mode: no `Mcp-Session-Id`, no
//! session kept between requests, and plain JSON responses. Each request is
//! handled on its own, by a fresh clone of the server (sharing one projector
//! and its connection pool). That suits Lambda, where any request may land on
//! a different instance, and it means there's nothing per client to store.
//!
//! # Tower services
//!
//! rmcp's [`StreamableHttpService`] implements `tower::Service`: "given an
//! HTTP request, eventually produce an HTTP response". axum routes requests
//! to services like it, and in Step 21 the Lambda adapter will call the same
//! service. It's Go's `http.Handler` idea, with async and typed errors.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use crate::config::{BEARER_TOKEN_VAR, BearerToken, Config};
use crate::error::Error;
use crate::projector::Projector;
use crate::server::FinancialsServer;

/// Where the MCP endpoint lives: `http://<address>/mcp`.
pub const MCP_PATH: &str = "/mcp";

/// The default listen address: loopback only, so nothing outside this
/// machine can connect.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// The largest request body accepted: 1 MiB. Plans are a few kilobytes.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// What `serve --http` needs beyond the financials-api settings: the token
/// clients must send, and the `Host` values to accept.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub bearer_token: BearerToken,
    /// `None` keeps rmcp's default: loopback only.
    pub allowed_hosts: Option<Vec<String>>,
}

impl HttpOptions {
    /// Takes the HTTP settings from `config`, refusing to start without a
    /// usable token: an HTTP server with no lock is never an option.
    pub fn from_config(config: &Config) -> Result<HttpOptions, Error> {
        // `let … else`: take the token out of the `Option`, or stop here.
        let Some(bearer_token) = config.bearer_token.clone() else {
            return Err(Error::Config {
                problem: format!(
                    "{BEARER_TOKEN_VAR} is required for serve --http: set it to a random secret of \
                     at least {} characters (see the README)",
                    BearerToken::MIN_LEN
                ),
            });
        };
        bearer_token.check()?;
        Ok(HttpOptions {
            bearer_token,
            allowed_hosts: config.allowed_hosts.clone(),
        })
    }
}

/// The transport settings: stateless, JSON responses, the allowed hosts and
/// the body limit.
///
/// `StreamableHttpServerConfig` is `#[non_exhaustive]`, so it can't be built
/// with `StreamableHttpServerConfig { ... }` outside rmcp: a new field in a
/// future version would otherwise break this code. Instead, start from
/// `Default` and change settings with its builder methods.
///
/// rmcp checks the `Host` header against the allowed list, which blocks
/// DNS-rebinding attacks (a web page tricking a browser into calling a
/// server on your machine). Its default list is loopback only; a hosted
/// server lists its own domain in `MCP_ALLOWED_HOSTS`.
pub fn config(options: &HttpOptions) -> StreamableHttpServerConfig {
    let config = StreamableHttpServerConfig::default()
        // No sessions: every request stands alone.
        .with_legacy_session_mode(false)
        // A tool call returns `application/json`, not an event stream.
        .with_json_response(true)
        // Keep-alive pings are for long-lived event streams; there are none.
        .with_sse_keep_alive(None)
        // Bodies over the limit get `413 Payload Too Large`, counted while
        // reading, whatever `Content-Length` claims.
        .with_max_request_body_bytes(MAX_REQUEST_BYTES);
    match &options.allowed_hosts {
        Some(hosts) => config.with_allowed_hosts(hosts.clone()),
        None => config,
    }
}

/// An axum router serving `server` at [`MCP_PATH`], behind the token check.
///
/// `move || Ok(server.clone())` is the *service factory*: rmcp calls it for
/// each request to get a server to handle it. The closure owns one
/// `FinancialsServer` (moved in) and hands out clones, which are cheap: they
/// share the projector through an `Arc`.
///
/// `.layer(...)` wraps every route in **middleware**: a function that sees
/// each request first and decides whether to pass it on. It's tower's idea
/// of layers, a service wrapping a service, like Go's
/// `func(http.Handler) http.Handler` or a Python WSGI middleware. Because the
/// layer is outside the router, even unknown paths need the token.
pub fn router<P: Projector + 'static>(
    server: FinancialsServer<P>,
    options: &HttpOptions,
) -> axum::Router {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config(options),
    );
    axum::Router::new()
        .nest_service(MCP_PATH, service)
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(options.bearer_token.clone()),
            require_bearer_token,
        ))
}

/// The middleware: let a request through only with the right token.
///
/// axum calls it with the shared state (the expected token, behind an `Arc`
/// so every request shares one copy), the request, and `next`: the rest of
/// the stack. Calling `next.run(request)` passes the request on; returning a
/// response without calling it stops the request here, before rmcp reads
/// its body.
async fn require_bearer_token(
    State(expected): State<Arc<BearerToken>>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(reason) = check_bearer(request.headers().get(header::AUTHORIZATION), &expected) {
        // Why it failed, as a fixed word. Never the header's contents.
        tracing::warn!(reason, "request rejected: auth_failed");
        return unauthorized();
    }
    let response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        tracing::warn!("request rejected: too_large");
    }
    response
}

/// Checks an `Authorization` header against the expected token, returning a
/// fixed reason on failure: `missing`, `malformed` or `wrong`.
fn check_bearer(header: Option<&HeaderValue>, expected: &BearerToken) -> Result<(), &'static str> {
    let value = header.ok_or("missing")?;
    // A header value is bytes; `to_str` fails if they aren't visible ASCII.
    let value = value.to_str().map_err(|_| "malformed")?;
    // `Bearer <token>`. The scheme name is case-insensitive (RFC 9110).
    let (scheme, token) = value.split_once(' ').ok_or("malformed")?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err("malformed");
    }
    if expected.matches(token.trim()) {
        Ok(())
    } else {
        Err("wrong")
    }
}

/// `401 Unauthorized`, saying only that a bearer token is required.
///
/// `(status, headers, body)` tuples implement axum's `IntoResponse`, so
/// `.into_response()` builds the full response from them.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "a bearer token is required",
    )
        .into_response()
}

/// Binds `address`, or explains why it couldn't.
///
/// The error says what kind of failure it was (address in use, permission
/// denied) without the operating system's full message.
pub async fn bind(address: SocketAddr) -> Result<TcpListener, Error> {
    TcpListener::bind(address)
        .await
        .map_err(|err| Error::Server {
            problem: format!("couldn't listen on {address}: {}", err.kind()),
        })
}

/// Serves MCP on `listener` until `shutdown` completes, then lets requests in
/// progress finish before returning.
///
/// `shutdown` is any future: Ctrl+C in `main.rs`, a test's signal in tests.
/// `impl Future<Output = ()> + Send + 'static` in argument position means
/// "some future that finishes with nothing, can move between threads, and
/// owns its data", without naming its type.
pub async fn serve<P: Projector + 'static>(
    server: FinancialsServer<P>,
    options: HttpOptions,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), Error> {
    let app = router(server, &options);
    let address = listener.local_addr().ok();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        // `%` logs a value with its `Display`: `127.0.0.1:8787`. This is the
        // local listen address, never the financials-api URL.
        address = %address.map(|a| a.to_string()).unwrap_or_default(),
        "serving MCP over HTTP at {MCP_PATH}"
    );

    // `with_graceful_shutdown` stops accepting connections when `shutdown`
    // completes, then waits for open requests to finish.
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;
    tracing::info!("HTTP server stopped");

    result.map_err(|err| Error::Server {
        problem: format!("the HTTP server stopped unexpectedly: {}", err.kind()),
    })
}

/// A token and options for tests, shared with the server tests.
#[cfg(test)]
pub(crate) const TEST_TOKEN: &str = "test-token-0123456789abcdefghijklmnopqrstuv";

#[cfg(test)]
pub(crate) fn test_options() -> HttpOptions {
    HttpOptions {
        bearer_token: BearerToken::for_tests(TEST_TOKEN),
        allowed_hosts: None,
    }
}

/// Starts a server for tests on a random local port and returns its `/mcp`
/// URL. It runs until the test's runtime shuts down.
#[cfg(test)]
pub(crate) async fn start_for_tests<P: Projector + 'static>(
    projector: P,
    options: HttpOptions,
) -> String {
    // Port 0 asks the operating system for any free port.
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let url = format!("http://{}{MCP_PATH}", listener.local_addr().unwrap());
    tokio::spawn(serve(
        FinancialsServer::new(projector),
        options,
        listener,
        std::future::pending(),
    ));
    url
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::sync::{Notify, oneshot};

    use super::*;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    const PLAN: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn projection() -> Projection {
        serde_json::from_str(PROJECTION).unwrap()
    }

    async fn start<P: Projector + 'static>(projector: P) -> String {
        start_for_tests(projector, test_options()).await
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"0"}}}"#;
    const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

    /// A raw JSON-RPC POST, the way any HTTP client would send it, with an
    /// optional `Authorization` header value.
    async fn post_with(url: &str, authorization: Option<&str>, body: String) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body);
        if let Some(value) = authorization {
            request = request.header("Authorization", value);
        }
        request.send().await.unwrap()
    }

    /// A raw POST with the right token.
    async fn post(url: &str, body: &str) -> reqwest::Response {
        post_with(url, Some(&format!("Bearer {TEST_TOKEN}")), body.to_string()).await
    }

    fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    #[tokio::test]
    async fn an_mcp_client_with_the_token_lists_and_calls_tools() {
        let url = start(FakeProjector::from_fn(|_| Ok(projection()))).await;

        // The same rmcp client as the stdio tests, with an HTTP transport that
        // sends `Authorization: Bearer <token>` on every request.
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url).auth_header(TEST_TOKEN),
        );
        let client = ().serve(transport).await.unwrap();
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 5);

        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        let result = client
            .call_tool(
                CallToolRequestParams::new("run_projection")
                    .with_arguments(arguments(serde_json::json!({ "plan": plan }))),
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn requests_without_the_right_token_get_401() {
        let url = start(FakeProjector::scripted(Vec::new())).await;
        let wrong_same_length = format!("Bearer {}", "x".repeat(TEST_TOKEN.len()));

        for authorization in [
            None,                                   // missing
            Some("Basic dXNlcjpwYXNz"),             // another scheme
            Some("Bearer"),                         // no token
            Some(wrong_same_length.as_str()),       // wrong, same length
            Some("Bearer test-token"),              // a prefix of the token
            Some(&format!("Bearer {TEST_TOKEN}x")), // the token plus one
        ] {
            let response = post_with(&url, authorization, INITIALIZE.to_string()).await;
            assert_eq!(response.status(), 401, "{authorization:?}");
            assert_eq!(response.headers()["www-authenticate"], "Bearer");
        }

        // The scheme name is case-insensitive.
        let lowercase = format!("bearer {TEST_TOKEN}");
        let response = post_with(&url, Some(&lowercase), INITIALIZE.to_string()).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn unknown_paths_need_the_token_too() {
        let url = start(FakeProjector::scripted(Vec::new())).await;
        let other = url.replace(MCP_PATH, "/other");
        assert_eq!(
            post_with(&other, None, LIST_TOOLS.to_string())
                .await
                .status(),
            401
        );
        assert_eq!(post(&other, LIST_TOOLS).await.status(), 404);
    }

    #[tokio::test]
    async fn an_oversized_body_gets_413_before_reaching_a_tool() {
        let fake = std::sync::Arc::new(FakeProjector::scripted(Vec::new()));
        // A projector that records whether it was ever called.
        struct Shared(std::sync::Arc<FakeProjector>);
        impl Projector for Shared {
            async fn project(&self, plan: &crate::plan::Plan) -> Result<Projection, Error> {
                self.0.project(plan).await
            }
        }
        let url = start(Shared(std::sync::Arc::clone(&fake))).await;

        // A tools/call whose arguments pad it past the 1 MiB limit.
        let padding = "x".repeat(MAX_REQUEST_BYTES);
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "run_projection", "arguments": {"plan": {"padding": padding}}}
        })
        .to_string();
        let response = post_with(&url, Some(&format!("Bearer {TEST_TOKEN}")), body).await;

        assert_eq!(response.status(), 413);
        assert!(fake.received().is_empty());
    }

    #[tokio::test]
    async fn requests_are_stateless() {
        let url = start(FakeProjector::scripted(Vec::new())).await;

        let response = post(&url, INITIALIZE).await;
        assert_eq!(response.status(), 200);
        // No session is created, so there's no session id to send back.
        assert!(response.headers().get("mcp-session-id").is_none());
        assert_eq!(
            response.headers()["content-type"].to_str().unwrap(),
            "application/json"
        );

        // A separate request, with no session id, still works.
        let response = post(&url, LIST_TOOLS).await;
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 5);
    }

    /// Posts with the right token and a chosen `Host` header.
    async fn post_as_host(url: &str, host: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(url)
            .header("Host", host)
            .header("Authorization", format!("Bearer {TEST_TOKEN}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(INITIALIZE)
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn only_allowed_host_headers_are_accepted() {
        // Default: loopback only. A DNS-rebinding attack arrives at the local
        // port with the attacker's hostname in `Host`.
        let url = start(FakeProjector::scripted(Vec::new())).await;
        assert_eq!(post_as_host(&url, "attacker.example").await.status(), 403);

        // A configured list replaces the default.
        let options = HttpOptions {
            allowed_hosts: Some(vec!["mcp.example.com".to_string()]),
            ..test_options()
        };
        let url = start_for_tests(FakeProjector::scripted(Vec::new()), options).await;
        assert_eq!(post_as_host(&url, "mcp.example.com").await.status(), 200);
        assert_eq!(post_as_host(&url, "attacker.example").await.status(), 403);
        assert_eq!(post_as_host(&url, "localhost").await.status(), 403);
    }

    #[test]
    fn serve_http_refuses_to_start_without_a_usable_token() {
        let config = |env: &[(&str, &str)]| {
            let env: Vec<(String, String)> = env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            Config::from_sources(
                move |key| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()),
                None,
            )
            .unwrap()
        };

        let missing = HttpOptions::from_config(&config(&[]))
            .unwrap_err()
            .to_string();
        assert!(
            missing.contains("MCP_BEARER_TOKEN is required for serve --http"),
            "{missing}"
        );

        let short = HttpOptions::from_config(&config(&[("MCP_BEARER_TOKEN", "too-short")]))
            .unwrap_err()
            .to_string();
        assert!(short.contains("at least 32 characters"), "{short}");
        assert!(!short.contains("too-short"), "{short}");

        let options =
            HttpOptions::from_config(&config(&[("MCP_BEARER_TOKEN", TEST_TOKEN)])).unwrap();
        assert!(options.bearer_token.matches(TEST_TOKEN));
    }

    #[test]
    fn check_bearer_reasons() {
        let expected = BearerToken::for_tests(TEST_TOKEN);
        let header = |value: &str| HeaderValue::from_str(value).unwrap();
        assert_eq!(check_bearer(None, &expected), Err("missing"));
        assert_eq!(
            check_bearer(Some(&header("Bearer")), &expected),
            Err("malformed")
        );
        assert_eq!(
            check_bearer(Some(&header("Token abc")), &expected),
            Err("malformed")
        );
        assert_eq!(
            check_bearer(Some(&header("Bearer abc")), &expected),
            Err("wrong")
        );
        let right = format!("Bearer {TEST_TOKEN}");
        assert_eq!(check_bearer(Some(&header(&right)), &expected), Ok(()));
    }

    #[tokio::test]
    async fn rejected_requests_are_logged_without_the_token_or_header() {
        let (captured, _guard) = crate::logging::capture::start("financials_mcp=debug");
        let url = start(FakeProjector::scripted(Vec::new())).await;
        let wrong = "Bearer wrong-token-that-is-long-enough-to-matter-xyz";

        post_with(&url, Some(wrong), INITIALIZE.to_string()).await;
        let padding = "x".repeat(MAX_REQUEST_BYTES + 1);
        post_with(&url, Some(&format!("Bearer {TEST_TOKEN}")), padding).await;

        let logs = captured.text();
        assert!(
            logs.contains("request rejected: auth_failed reason=\"wrong\""),
            "{logs}"
        );
        assert!(logs.contains("request rejected: too_large"), "{logs}");
        for secret in ["wrong-token", TEST_TOKEN, "Authorization", "Bearer"] {
            assert!(!logs.contains(secret), "logs contain {secret:?}:\n{logs}");
        }
    }

    /// A projector that waits until told to finish, so a test can stop the
    /// server while a tool call is in progress.
    struct Paused {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl Projector for Paused {
        async fn project(&self, _plan: &crate::plan::Plan) -> Result<Projection, Error> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(projection())
        }
    }

    #[tokio::test]
    async fn shutting_down_lets_a_call_in_progress_finish() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let url = format!("http://{}{MCP_PATH}", listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel::<()>();
        let projector = Paused {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        };
        let task = tokio::spawn(serve(
            FinancialsServer::new(projector),
            test_options(),
            listener,
            async {
                // `let _ =`: a dropped sender also means "stop".
                let _ = stopped.await;
            },
        ));

        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        let call = serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "run_projection", "arguments": {"plan": plan}}
        })
        .to_string();
        let request = tokio::spawn(async move { post(&url, &call).await });

        // Stop the server while the projection is waiting.
        started.notified().await;
        stop.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished(), "the server waits for the open request");

        release.notify_one();
        let response = request.await.unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_ne!(body["result"]["isError"], true, "{body}");
        // Then the server finishes cleanly.
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_busy_port_is_a_clear_error() {
        let taken = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let address = taken.local_addr().unwrap();
        let err = bind(address).await.unwrap_err().to_string();
        assert!(
            err.contains(&format!("couldn't listen on {address}")),
            "{err}"
        );
    }
}
