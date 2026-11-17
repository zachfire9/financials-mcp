//! The real [`Projector`]: calls financials-api's `POST /projections`.
//!
//! Every way the call can go wrong becomes a specific [`Error`]:
//!
//! | What happened                         | Error                     | What to do about it        |
//! |---------------------------------------|---------------------------|----------------------------|
//! | `400`: the API rejected the plan       | `InvalidPlan { problems }`| fix the plan               |
//! | `401`: missing or wrong access token   | `Auth`                    | fix `FINANCIALS_ACCESS_TOKEN` |
//! | can't connect, timeout, `5xx`, other   | `Api { problem }`         | check the API / its URL     |
//! | `200` but not a projection             | `UnexpectedResponse`      | likely a version mismatch   |
//!
//! Two security rules: **redirects are refused** (the token travels in a
//! custom header, which HTTP clients don't strip when following a redirect to
//! another host), and **values are removed from API error messages** before
//! they're passed on, just like the pre-checks.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderValue, USER_AGENT};

use crate::config::Config;
use crate::error::Error;
use crate::plan::Plan;
use crate::projection::Projection;
use crate::projector::Projector;

/// Header financials-api reads the access token from.
pub const ACCESS_TOKEN_HEADER: &str = "X-Financials-Access-Token";

/// How long one projection call may take, connecting included.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response accepted. Real projections are tens of kilobytes;
/// this guards against a misconfigured URL streaming something huge.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Prefix financials-api puts on validation errors.
const VALIDATION_PREFIX: &str = "invalid projection request: ";

/// A [`Projector`] backed by financials-api over HTTP.
///
/// `Clone` is cheap: a `reqwest::Client` is a handle to a shared connection
/// pool (internally an `Arc`), so clones reuse the same connections.
#[derive(Clone)]
pub struct ApiProjector {
    client: reqwest::Client,
    projections_url: String,
    access_token: Option<HeaderValue>,
    timeout: Duration,
    max_response_bytes: usize,
}

impl ApiProjector {
    /// Builds a client for the configured API with the default limits.
    pub fn new(config: &Config) -> Result<ApiProjector, Error> {
        ApiProjector::with_limits(config, DEFAULT_TIMEOUT, DEFAULT_MAX_RESPONSE_BYTES)
    }

    /// Like [`ApiProjector::new`], with explicit limits (tests use short ones).
    pub fn with_limits(
        config: &Config,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<ApiProjector, Error> {
        // The builder pattern: each method configures one thing and returns
        // the builder, so calls chain; `.build()` produces the client.
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Api {
                problem: "could not create the HTTP client".to_string(),
            })?;

        // `Option::map` + `transpose`: turn `Option<Result<T, E>>` into
        // `Result<Option<T>, E>`, so `?` can handle the error case while `None`
        // (no token configured) passes through untouched.
        let access_token = config
            .access_token
            .as_ref()
            .map(|token| {
                let mut value =
                    HeaderValue::from_str(token.expose()).map_err(|_| Error::Config {
                        problem:
                            "FINANCIALS_ACCESS_TOKEN contains characters not allowed in a header"
                                .to_string(),
                    })?;
                // Marks the header as sensitive so HTTP-level debug output
                // shows it as `Sensitive` instead of its value.
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;

        Ok(ApiProjector {
            client,
            projections_url: format!("{}/projections", config.api_url),
            access_token,
            timeout,
            max_response_bytes,
        })
    }

    /// Reads the response body, refusing to buffer more than the limit.
    ///
    /// `while let Some(chunk) = ...` keeps looping as long as the pattern
    /// matches: each `.chunk().await` yields `Some(bytes)` until the body is
    /// finished, then `None`, which ends the loop.
    async fn read_limited(&self, mut response: reqwest::Response) -> Result<Vec<u8>, Error> {
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| self.transport_error(&err))?
        {
            if body.len() + chunk.len() > self.max_response_bytes {
                return Err(Error::UnexpectedResponse {
                    problem: format!("response is larger than {} bytes", self.max_response_bytes),
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Describes a failure to talk to the API at all.
    fn transport_error(&self, err: &reqwest::Error) -> Error {
        let problem = if err.is_timeout() {
            format!(
                "financials-api did not respond within {} seconds",
                self.timeout.as_secs_f32()
            )
        } else if err.is_connect() {
            format!(
                "could not connect to financials-api at {}; is it running? (check FINANCIALS_API_URL)",
                self.projections_url.trim_end_matches("/projections")
            )
        } else {
            "the request to financials-api failed".to_string()
        };
        Error::Api { problem }
    }
}

impl Projector for ApiProjector {
    async fn project(&self, plan: &Plan) -> Result<Projection, Error> {
        let mut request = self
            .client
            .post(&self.projections_url)
            .header(CONTENT_TYPE, "application/json")
            .header(
                USER_AGENT,
                concat!("financials-mcp/", env!("CARGO_PKG_VERSION")),
            )
            .body(plan.to_json());
        // `if let Some(token) = &self.access_token` borrows the token only when
        // one is configured; otherwise no header is sent at all.
        if let Some(token) = &self.access_token {
            request = request.header(ACCESS_TOKEN_HEADER, token.clone());
        }

        // The first `.await`: the task pauses here until the response headers
        // arrive (or the timeout fires), and tokio runs other work meanwhile.
        let response = request
            .send()
            .await
            .map_err(|err| self.transport_error(&err))?;
        let status = response.status();
        let body = self.read_limited(response).await?;

        // `match` on the status code. `StatusCode::OK` etc. are constants, and
        // the `status if status.is_server_error()` arm is a guard.
        match status {
            StatusCode::OK => serde_json::from_slice(&body).map_err(|_| Error::UnexpectedResponse {
                problem: "financials-api returned something that isn't a projection".to_string(),
            }),
            StatusCode::BAD_REQUEST => Err(invalid_plan_from(&api_message(&body))),
            StatusCode::UNAUTHORIZED => Err(Error::Auth),
            StatusCode::PAYLOAD_TOO_LARGE => Err(Error::InvalidPlan {
                problems: vec!["plan is too large for financials-api".to_string()],
            }),
            StatusCode::NOT_FOUND => Err(Error::Api {
                problem: "financials-api has no /projections endpoint at this URL (check FINANCIALS_API_URL)"
                    .to_string(),
            }),
            status if status.is_redirection() => Err(Error::Api {
                problem: format!(
                    "financials-api answered with a redirect ({}); redirects aren't followed, so the access token stays put. Set FINANCIALS_API_URL to the final URL",
                    status.as_u16()
                ),
            }),
            status if status.is_server_error() => Err(Error::Api {
                problem: format!("financials-api had an internal error (status {})", status.as_u16()),
            }),
            status => Err(Error::Api {
                problem: format!("financials-api returned unexpected status {}", status.as_u16()),
            }),
        }
    }
}

/// The `error` message from a financials-api JSON error body, if there is one.
fn api_message(body: &[u8]) -> String {
    // A struct defined inside a function is only visible there: handy for a
    // one-off shape like `{"error": "..."}`.
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        error: String,
    }
    serde_json::from_slice::<ErrorBody>(body)
        .map(|parsed| parsed.error)
        .unwrap_or_else(|_| "financials-api rejected the plan".to_string())
}

/// Turns an API `400` message into `InvalidPlan`, one problem per entry.
///
/// Validation errors look like `invalid projection request: a; b`, the same
/// problems `Plan::check` reports, so they become the same error.
fn invalid_plan_from(message: &str) -> Error {
    let problems = match message.strip_prefix(VALIDATION_PREFIX) {
        Some(list) => list.split("; ").map(strip_values).collect(),
        None => vec![strip_values(message)],
    };
    Error::InvalidPlan { problems }
}

/// Removes numbers in parentheses, such as the cents amount in
/// `...more than the first month's interest (112500)`.
///
/// financials-api's messages otherwise only contain field paths, indexes
/// (`items[2]`) and fixed limits, which aren't private. A plan-derived amount
/// only ever appears in parentheses, so that's what's removed.
fn strip_values(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    // Walk through the text one `(` at a time.
    while let Some(open) = rest.find(" (") {
        let after = &rest[open + 2..];
        match after.find(')') {
            // `&after[..close]` is a *slice*: a borrowed view of part of the
            // string, no copying. Slicing uses byte positions, which is safe
            // here because `find` returns positions on character boundaries.
            Some(close) if is_number(&after[..close]) => {
                out.push_str(&rest[..open]);
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str(&rest[..open + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::summarize;

    use wiremock::matchers::{body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PLAN: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn plan() -> Plan {
        Plan::from_json(PLAN).unwrap()
    }

    /// A config pointing at the mock server, with or without a token.
    fn config(url: &str, token: Option<&str>) -> Config {
        let token = token.map(str::to_string);
        Config::from_sources(
            |key| match key {
                "FINANCIALS_API_URL" => Some(url.to_string()),
                "FINANCIALS_ACCESS_TOKEN" => token.clone(),
                _ => None,
            },
            None,
        )
        .unwrap()
    }

    /// Starts a mock financials-api that answers `POST /projections` once.
    async fn api_answering(response: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/projections"))
            .respond_with(response)
            .mount(&server)
            .await;
        server
    }

    fn error_body(message: &str) -> ResponseTemplate {
        ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": message }))
    }

    async fn project_against(
        server: &MockServer,
        token: Option<&str>,
    ) -> Result<Projection, Error> {
        let api = ApiProjector::new(&config(&server.uri(), token)).unwrap();
        api.project(&plan()).await
    }

    #[tokio::test]
    async fn a_successful_projection_is_parsed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/projections"))
            .and(header("content-type", "application/json"))
            // The request body must be exactly the plan's JSON.
            .and(body_string(plan().to_json()))
            .respond_with(ResponseTemplate::new(200).set_body_string(PROJECTION))
            .expect(1)
            .mount(&server)
            .await;

        let projection = project_against(&server, None).await.unwrap();

        assert_eq!(summarize(&projection).unwrap().depletion_year, Some(36));
    }

    #[tokio::test]
    async fn the_access_token_is_sent_only_when_configured() {
        let server = api_answering(ResponseTemplate::new(200).set_body_string(PROJECTION)).await;

        project_against(&server, Some("token-123")).await.unwrap();
        project_against(&server, None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[0].headers.get(ACCESS_TOKEN_HEADER).unwrap(),
            "token-123"
        );
        assert!(requests[1].headers.get(ACCESS_TOKEN_HEADER).is_none());
    }

    #[tokio::test]
    async fn api_validation_errors_become_invalid_plan_problems() {
        let server = api_answering(error_body(
            "invalid projection request: items[0].currency must be a 3-letter uppercase code such as USD; mortgage.monthlyPaymentCents must be more than the first month's interest (112500)",
        ))
        .await;

        let err = project_against(&server, None).await.unwrap_err();

        match err {
            Error::InvalidPlan { problems } => assert_eq!(
                problems,
                [
                    "items[0].currency must be a 3-letter uppercase code such as USD",
                    // The cents amount is removed; the field and rule stay.
                    "mortgage.monthlyPaymentCents must be more than the first month's interest",
                ]
            ),
            other => panic!("expected InvalidPlan, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn other_bad_requests_are_reported_as_one_problem() {
        let server =
            api_answering(error_body("request body must contain a single JSON object")).await;
        let err = project_against(&server, None).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid plan: request body must contain a single JSON object"
        );
    }

    #[tokio::test]
    async fn unauthorized_is_an_auth_error_without_the_token() {
        let server = api_answering(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({ "error": "missing or invalid access token" })),
        )
        .await;

        let err = project_against(&server, Some("secret-token-xyz"))
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Auth), "{err:?}");
        assert!(!err.to_string().contains("secret-token-xyz"));
        assert!(!format!("{err:?}").contains("secret-token-xyz"));
    }

    #[tokio::test]
    async fn server_errors_and_wrong_urls_are_api_errors() {
        let cases = [
            (500, "internal error (status 500)"),
            (503, "internal error (status 503)"),
            (404, "no /projections endpoint"),
            (418, "unexpected status 418"),
        ];
        for (status, expected) in cases {
            let server = api_answering(ResponseTemplate::new(status)).await;
            let err = project_against(&server, None).await.unwrap_err();
            assert!(matches!(err, Error::Api { .. }), "{status}: {err:?}");
            assert!(err.to_string().contains(expected), "{status}: {err}");
        }
    }

    #[tokio::test]
    async fn redirects_are_not_followed_so_the_token_stays_put() {
        let elsewhere = MockServer::start().await;
        let server = api_answering(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/projections", elsewhere.uri())),
        )
        .await;

        let err = project_against(&server, Some("token-123"))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("redirect (307)"), "{err}");
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "the redirect target must never see the request or the token"
        );
    }

    #[tokio::test]
    async fn a_slow_api_times_out() {
        let server = api_answering(
            ResponseTemplate::new(200)
                .set_body_string(PROJECTION)
                .set_delay(Duration::from_secs(5)),
        )
        .await;
        let api = ApiProjector::with_limits(
            &config(&server.uri(), None),
            Duration::from_millis(200),
            DEFAULT_MAX_RESPONSE_BYTES,
        )
        .unwrap();

        let err = api.project(&plan()).await.unwrap_err();

        assert!(
            err.to_string()
                .contains("did not respond within 0.2 seconds"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_api_is_reported_clearly() {
        // Port 9 ("discard") on localhost is closed on CI runners and dev
        // machines, so the connection is refused straight away.
        let api = ApiProjector::new(&config("http://127.0.0.1:9", None)).unwrap();

        let err = api.project(&plan()).await.unwrap_err();

        assert!(matches!(err, Error::Api { .. }), "{err:?}");
        assert!(err.to_string().contains("could not connect"), "{err}");
    }

    #[tokio::test]
    async fn a_200_that_isnt_a_projection_is_an_unexpected_response() {
        let server =
            api_answering(ResponseTemplate::new(200).set_body_string("<html>hello</html>")).await;
        let err = project_against(&server, None).await.unwrap_err();
        assert!(matches!(err, Error::UnexpectedResponse { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn oversized_responses_are_refused() {
        let server = api_answering(ResponseTemplate::new(200).set_body_string(PROJECTION)).await;
        let api =
            ApiProjector::with_limits(&config(&server.uri(), None), DEFAULT_TIMEOUT, 1024).unwrap();

        let err = api.project(&plan()).await.unwrap_err();

        assert!(err.to_string().contains("larger than 1024 bytes"), "{err}");
    }

    #[test]
    fn values_in_parentheses_are_stripped_but_indexes_and_limits_stay() {
        assert_eq!(
            strip_values(
                "mortgage.monthlyPaymentCents must be more than the first month's interest (112500)"
            ),
            "mortgage.monthlyPaymentCents must be more than the first month's interest"
        );
        assert_eq!(
            strip_values("items[2].withdrawalPriority must be 1 (first), 2 (normal) or 3 (last)"),
            "items[2].withdrawalPriority must be 1 (first), 2 (normal) or 3 (last)"
        );
        assert_eq!(strip_values("a (-5) b (x) c (7)"), "a b (x) c");
    }
}
