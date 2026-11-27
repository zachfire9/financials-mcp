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

use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use crate::error::Error;
use crate::projector::Projector;
use crate::server::FinancialsServer;

/// Where the MCP endpoint lives: `http://<address>/mcp`.
pub const MCP_PATH: &str = "/mcp";

/// The default listen address: loopback only, so nothing outside this
/// machine can connect.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// The transport settings: stateless, JSON responses.
///
/// `StreamableHttpServerConfig` is `#[non_exhaustive]`, so it can't be built
/// with `StreamableHttpServerConfig { ... }` outside rmcp: a new field in a
/// future version would otherwise break this code. Instead, start from
/// `Default` and change settings with its builder methods.
///
/// The default `Host` check stays on: only `localhost`, `127.0.0.1` and `::1`
/// are accepted, which blocks DNS-rebinding attacks (a web page tricking a
/// browser into calling a server on your machine). Step 20 makes the list
/// configurable for a hosted server.
pub fn config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        // No sessions: every request stands alone.
        .with_legacy_session_mode(false)
        // A tool call returns `application/json`, not an event stream.
        .with_json_response(true)
        // Keep-alive pings are for long-lived event streams; there are none.
        .with_sse_keep_alive(None)
}

/// An axum router serving `server` at [`MCP_PATH`].
///
/// `move || Ok(server.clone())` is the *service factory*: rmcp calls it for
/// each request to get a server to handle it. The closure owns one
/// `FinancialsServer` (moved in) and hands out clones, which are cheap: they
/// share the projector through an `Arc`.
pub fn router<P: Projector + 'static>(
    server: FinancialsServer<P>,
    config: StreamableHttpServerConfig,
) -> axum::Router {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    axum::Router::new().nest_service(MCP_PATH, service)
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
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), Error> {
    let config = config();
    // rmcp's own token for stopping the transport. It's cancelled once axum
    // has finished the requests in progress.
    let stop_transport = config.cancellation_token.clone();
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
    let result = axum::serve(listener, router(server, config))
        .with_graceful_shutdown(shutdown)
        .await;
    stop_transport.cancel();
    tracing::info!("HTTP server stopped");

    result.map_err(|err| Error::Server {
        problem: format!("the HTTP server stopped unexpectedly: {}", err.kind()),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::StreamableHttpClientTransport;
    use tokio::sync::{Notify, oneshot};

    use super::*;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    const PLAN: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn projection() -> Projection {
        serde_json::from_str(PROJECTION).unwrap()
    }

    /// A server on a random local port, stopped when the returned sender is
    /// dropped or sent to. Returns its `/mcp` URL.
    async fn start<P: Projector + 'static>(
        projector: P,
    ) -> (
        String,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), Error>>,
    ) {
        // Port 0 asks the operating system for any free port.
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let url = format!("http://{}{MCP_PATH}", listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(FinancialsServer::new(projector), listener, async {
            // `let _ =`: a dropped sender also means "stop".
            let _ = stopped.await;
        }));
        (url, stop, task)
    }

    fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    #[tokio::test]
    async fn an_mcp_client_lists_and_calls_tools_over_http() {
        let (url, _stop, _task) = start(FakeProjector::from_fn(|_| Ok(projection()))).await;

        // The same rmcp client as the stdio tests, with an HTTP transport.
        let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await.unwrap();
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

    /// Posts one raw JSON-RPC message, the way any HTTP client would.
    async fn post(url: &str, body: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"0"}}}"#;
    const LIST_TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

    #[tokio::test]
    async fn requests_are_stateless() {
        let (url, _stop, _task) = start(FakeProjector::scripted(Vec::new())).await;

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

    #[tokio::test]
    async fn a_host_header_that_isnt_allowed_is_rejected() {
        let (url, _stop, _task) = start(FakeProjector::scripted(Vec::new())).await;

        // A DNS-rebinding attack arrives at the local port with the
        // attacker's hostname in `Host`.
        let response = reqwest::Client::new()
            .post(&url)
            .header("Host", "attacker.example")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(INITIALIZE)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
    }

    #[tokio::test]
    async fn other_paths_are_not_found() {
        let (url, _stop, _task) = start(FakeProjector::scripted(Vec::new())).await;
        let response = post(&url.replace(MCP_PATH, "/other"), LIST_TOOLS).await;
        assert_eq!(response.status(), 404);
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
        let (url, stop, task) = start(Paused {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        })
        .await;

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
