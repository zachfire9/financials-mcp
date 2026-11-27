//! **Running on AWS Lambda.**
//!
//! Lambda runs a function per request, with no long-lived server listening on
//! a port. Lambda's runtime API hands the program one event at a time
//! (here, an API Gateway HTTP API request), and the program answers each one.
//! [`lambda_http`] turns those events into ordinary `http::Request`s and the
//! answers back into API Gateway responses, so the **same** axum router as
//! `serve --http` handles them: the same tools, the same bearer token, the
//! same allowed hosts and limits. Nothing about the tools knows it's on
//! Lambda.
//!
//! # Cold starts
//!
//! The first request to a new Lambda instance (a *cold start*) runs
//! [`run`] from the top: read the settings, check the token, build the HTTP
//! client and the router. Later requests to the same instance reuse all of
//! that; each one only clones the router (cheap: it's reference-counted) and
//! handles the request. So anything expensive belongs *before*
//! `lambda_http::run`, never inside the per-request path.

use crate::api::ApiProjector;
use crate::config::Config;
use crate::error::Error;
use crate::http::{HttpOptions, router};
use crate::server::FinancialsServer;

/// Loads the settings from the environment, builds the server once, and
/// serves Lambda events until the runtime stops the instance.
///
/// Settings come from the function's environment variables, set by the
/// deployment (Step 22): `FINANCIALS_API_URL`, `FINANCIALS_ACCESS_TOKEN`,
/// `MCP_BEARER_TOKEN`, `MCP_ALLOWED_HOSTS` and `FINANCIALS_MCP_LOG`.
pub async fn run() -> Result<(), Error> {
    // In Lambda, stdout and stderr both go to CloudWatch Logs. The same
    // rules apply: one line per tool call, never plan contents.
    crate::logging::init();

    // Everything from here to `lambda_http::run` happens once per cold start.
    let app = build()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "serving MCP on AWS Lambda"
    );

    // `lambda_http::run` polls Lambda's runtime API for the next event,
    // converts it to a request, calls the router, and posts the response
    // back, forever. It only returns if the runtime API fails.
    lambda_http::run(app).await.map_err(|_| Error::Server {
        problem: "the Lambda runtime stopped unexpectedly".to_string(),
    })
}

/// The router Lambda serves, built from the environment: the same one as
/// `serve --http`, behind the same token check. Fails, before any request is
/// served, without a usable `MCP_BEARER_TOKEN`.
pub fn build() -> Result<axum::Router, Error> {
    build_from(&Config::from_env()?)
}

/// [`build`] from explicit settings, so tests don't touch the environment.
pub fn build_from(config: &Config) -> Result<axum::Router, Error> {
    let options = HttpOptions::from_config(config)?;
    let server = FinancialsServer::new(ApiProjector::new(config)?);
    Ok(router(server, &options))
}

#[cfg(test)]
mod tests {
    use lambda_http::tower::ServiceExt;

    use super::*;
    use crate::http::TEST_TOKEN;

    /// Settings as the deployment will set them, with the API at
    /// `api_url` and the API Gateway domain as the allowed host.
    fn config(api_url: &str) -> Config {
        let values = [
            ("FINANCIALS_API_URL", api_url.to_string()),
            ("MCP_BEARER_TOKEN", TEST_TOKEN.to_string()),
            (
                "MCP_ALLOWED_HOSTS",
                "abc123.execute-api.us-east-1.amazonaws.com".to_string(),
            ),
        ];
        Config::from_sources(
            move |key| {
                values
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.clone())
            },
            None,
        )
        .unwrap()
    }

    /// An API Gateway HTTP API (payload v2) event, the JSON Lambda receives,
    /// with the given `authorization` header (or none) and JSON-RPC `body`.
    ///
    /// The fields are the real event's, trimmed to what matters here; the
    /// domain and ids are made up.
    fn event(authorization: Option<&str>, body: &str) -> String {
        let mut headers = serde_json::json!({
            "host": "abc123.execute-api.us-east-1.amazonaws.com",
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
        });
        if let Some(value) = authorization {
            headers["authorization"] = value.into();
        }
        serde_json::json!({
            "version": "2.0",
            "routeKey": "POST /mcp",
            "rawPath": "/mcp",
            "rawQueryString": "",
            "headers": headers,
            "requestContext": {
                "accountId": "000000000000",
                "apiId": "abc123",
                "domainName": "abc123.execute-api.us-east-1.amazonaws.com",
                "domainPrefix": "abc123",
                "http": {
                    "method": "POST",
                    "path": "/mcp",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "192.0.2.1",
                    "userAgent": "test"
                },
                "requestId": "test-request",
                "routeKey": "POST /mcp",
                "stage": "$default",
                "time": "27/Nov/2026:15:00:00 +0000",
                "timeEpoch": 1795791600000_i64
            },
            "body": body,
            "isBase64Encoded": false
        })
        .to_string()
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"lambda-test","version":"0"}}}"#;

    /// Sends one event through the same path Lambda does: parse the event
    /// JSON with `lambda_http`, then call the router as a tower service.
    /// Returns the status and the body as JSON (or `Null` if it isn't JSON).
    async fn invoke(app: axum::Router, event: &str) -> (u16, serde_json::Value) {
        let request = lambda_http::request::from_str(event).expect("a valid API Gateway event");
        // `oneshot` (from tower's `ServiceExt`) waits until the service is
        // ready, then calls it once with this request.
        let response = app.oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn an_api_gateway_event_with_the_token_is_answered() {
        let app = build_from(&config("http://127.0.0.1:9")).unwrap();
        let bearer = format!("Bearer {TEST_TOKEN}");

        let (status, body) = invoke(app.clone(), &event(Some(&bearer), INITIALIZE)).await;
        assert_eq!(status, 200);
        assert_eq!(body["result"]["serverInfo"]["name"], "financials-mcp");

        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let (status, body) = invoke(app, &event(Some(&bearer), list)).await;
        assert_eq!(status, 200);
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn a_tool_call_reaches_financials_api() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/projections"))
            .and(header("content-type", "application/json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../tests/fixtures/projection-basic.json")),
            )
            .expect(1)
            .mount(&api)
            .await;
        let app = build_from(&config(&api.uri())).unwrap();
        let plan: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/plan-basic.json")).unwrap();
        let call = serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "run_projection", "arguments": {"plan": plan}}
        })
        .to_string();

        let bearer = format!("Bearer {TEST_TOKEN}");
        let (status, body) = invoke(app, &event(Some(&bearer), &call)).await;

        assert_eq!(status, 200);
        assert_ne!(body["result"]["isError"], true, "{body}");
        let summary: serde_json::Value =
            serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(summary["summary"]["depletionYear"], 36);
    }

    #[tokio::test]
    async fn an_event_without_the_token_gets_401() {
        let app = build_from(&config("http://127.0.0.1:9")).unwrap();
        for authorization in [None, Some("Bearer wrong")] {
            let (status, _) = invoke(app.clone(), &event(authorization, INITIALIZE)).await;
            assert_eq!(status, 401, "{authorization:?}");
        }
    }

    #[tokio::test]
    async fn an_oversized_event_body_is_rejected() {
        let app = build_from(&config("http://127.0.0.1:9")).unwrap();
        let bearer = format!("Bearer {TEST_TOKEN}");
        let padding = "x".repeat(crate::http::MAX_REQUEST_BYTES + 1);
        let (status, _) = invoke(app, &event(Some(&bearer), &padding)).await;
        assert_eq!(status, 413);
    }

    #[tokio::test]
    async fn an_event_for_another_host_is_rejected() {
        let app = build_from(&config("http://127.0.0.1:9")).unwrap();
        let bearer = format!("Bearer {TEST_TOKEN}");
        let wrong_host = event(Some(&bearer), INITIALIZE).replace(
            r#""host":"abc123.execute-api.us-east-1.amazonaws.com""#,
            r#""host":"attacker.example""#,
        );
        assert!(wrong_host.contains("attacker.example"));
        let (status, _) = invoke(app, &wrong_host).await;
        assert_eq!(status, 403);
    }

    #[test]
    fn startup_fails_without_a_token() {
        let without_token = Config::from_sources(|_| None, None).unwrap();
        let err = build_from(&without_token).unwrap_err().to_string();
        assert!(err.contains("MCP_BEARER_TOKEN is required"), "{err}");
    }
}
