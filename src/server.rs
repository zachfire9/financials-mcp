//! The MCP server: tools an AI agent can call.
//!
//! MCP (Model Context Protocol) is JSON-RPC between a client (Claude Desktop,
//! Claude Code, later agent-harness) and a server (this program). Over
//! **stdio**, the client starts the server as a child process and they
//! exchange one JSON message per line on its stdin and stdout. That's why
//! nothing else may ever be printed to stdout while serving.
//!
//! A session goes: `initialize` (the server reports its name, capabilities and
//! instructions) → `tools/list` (names, descriptions, JSON Schemas for the
//! arguments) → any number of `tools/call`.
//!
//! The official Rust SDK, `rmcp`, handles the protocol. This module supplies
//! the tools, using its attribute macros: `#[tool_router]` on an `impl` block
//! collects every `#[tool]` method into a router, and `#[tool_handler]`
//! connects that router to the server's request handling.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::plan::Plan;
use crate::projector::{Projector, project_and_summarize};
use crate::summary::Summary;

/// What the model reads when it connects. Kept short; Step 15 refines it.
pub const INSTRUCTIONS: &str = "\
Retirement projection tools backed by the financials-api engine. \
Use these tools for every number: never estimate or calculate a projection yourself. \
A plan is the financials-api projection request: money in integer cents, rates in basis points (700 = 7%). \
Results are in whole dollars. Projections use fixed annual returns and no taxes, \
so mention those limitations and the assumptions returned with each result.";

/// Arguments for `run_projection`.
///
/// `plan` is advertised to clients with the full `Plan` schema
/// (`#[schemars(with = "Plan")]`), but received as raw JSON
/// (`serde_json::Value`) and parsed by [`Plan::from_json`]. That keeps the
/// error messages that never quote plan values, and turns a bad plan into a
/// tool error the model can read and fix. If rmcp parsed `Plan` directly, a
/// wrong type would become a protocol error carrying serde's raw message.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunProjectionArgs {
    /// The plan to project.
    #[schemars(with = "Plan")]
    pub plan: serde_json::Value,
}

/// The successful result of `run_projection`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionResult {
    pub summary: Summary,
    pub assumptions: Assumptions,
}

/// The inputs a summary depends on, echoed back so the model can state them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assumptions {
    /// Years of saving (the plan-wide value; see `people` for per-person).
    pub saving_years: i32,
    pub drawdown_years: i32,
    /// Per-person saving years, when the plan has people.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<PersonTimeline>,
    pub withdrawal_inflation_rate_basis_points: i32,
    pub returns: Vec<ItemReturn>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonTimeline {
    pub name: String,
    pub saving_years: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReturn {
    pub item: String,
    pub annual_return_rate_basis_points: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drawdown_annual_return_rate_basis_points: Option<i32>,
}

impl Assumptions {
    /// Collects the assumptions from a plan.
    fn from_plan(plan: &Plan) -> Assumptions {
        Assumptions {
            saving_years: plan.saving_years,
            drawdown_years: plan.drawdown_years,
            people: plan
                .people
                .iter()
                .map(|person| PersonTimeline {
                    name: person.name.clone(),
                    saving_years: person.saving_years,
                })
                .collect(),
            withdrawal_inflation_rate_basis_points: plan
                .annual_withdrawal_inflation_rate_basis_points,
            returns: plan
                .items
                .iter()
                .map(|item| ItemReturn {
                    item: item.name.clone(),
                    annual_return_rate_basis_points: item.annual_return_rate_basis_points,
                    drawdown_annual_return_rate_basis_points: item
                        .drawdown_annual_return_rate_basis_points,
                })
                .collect(),
        }
    }
}

/// The MCP server, generic over the projector so tests can use a fake.
///
/// The projector sits behind an `Arc` because rmcp clones the server for
/// each connection and request: an `Arc` clone is just a pointer copy, and
/// every clone shares the same projector (and its HTTP connection pool).
pub struct FinancialsServer<P: Projector> {
    projector: Arc<P>,
    // Built by the `#[tool_router]` macro below: the list of tools and the
    // code that routes each `tools/call` to the right method.
    tool_router: ToolRouter<Self>,
}

// `#[derive(Clone)]` would require `P: Clone`, because derives add a bound
// for every type parameter. Only the `Arc` needs cloning, so `Clone` is
// written by hand without that bound.
impl<P: Projector> Clone for FinancialsServer<P> {
    fn clone(&self) -> Self {
        FinancialsServer {
            projector: Arc::clone(&self.projector),
            tool_router: self.tool_router.clone(),
        }
    }
}

// `impl<P: Projector + 'static> FinancialsServer<P>` reads "for every type
// `P` that is a `Projector` (and owns its data), these methods exist".
#[tool_router]
impl<P: Projector + 'static> FinancialsServer<P> {
    pub fn new(projector: P) -> Self {
        FinancialsServer {
            projector: Arc::new(projector),
            // `Self::tool_router()` is generated by `#[tool_router]`.
            tool_router: Self::tool_router(),
        }
    }

    /// Run one retirement projection for a plan and return a compact summary
    /// (does the money last, when does it run short, lowest balance, totals)
    /// plus the assumptions it used. Amounts are whole dollars.
    ///
    /// A `#[tool]` method's doc comment becomes the tool's description; its
    /// `Parameters<T>` argument becomes the input schema (from `T`'s
    /// `JsonSchema`). `annotations(...)` are hints clients can show or use:
    /// this tool only reads and computes, so it's read-only, safe to retry,
    /// and touches nothing outside the projection engine.
    #[tool(annotations(
        title = "Run a retirement projection",
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    pub async fn run_projection(
        &self,
        // A pattern in the parameter list: `Parameters(args)` unpacks the
        // wrapper so `args` is the `RunProjectionArgs` inside.
        Parameters(args): Parameters<RunProjectionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // The outer `Result` is for protocol failures (rmcp's `ErrorData`).
        // Problems with the plan or the API are not protocol failures: they're
        // returned as a normal result marked `isError`, so the model reads the
        // message and can fix the plan or explain the problem.
        Ok(match self.project(args.plan).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&result).expect("a result always serializes"),
            )]),
            Err(err) => CallToolResult::error(vec![ContentBlock::text(err.to_string())]),
        })
    }

    /// Parses, checks, projects and summarizes, using the crate's own errors.
    async fn project(&self, plan: serde_json::Value) -> Result<ProjectionResult, Error> {
        let plan = Plan::from_json(&plan.to_string())?;
        // `&*self.projector` turns `&Arc<P>` into `&P`: `*` reaches through
        // the `Arc` to the projector, `&` borrows it.
        let summary = project_and_summarize(&*self.projector, &plan).await?;
        Ok(ProjectionResult {
            summary,
            assumptions: Assumptions::from_plan(&plan),
        })
    }
}

/// `ServerHandler` is rmcp's trait for answering requests. Every method has a
/// default; `#[tool_handler]` fills in the tool ones (`tools/list`,
/// `tools/call`) from `self.tool_router`, and `get_info` answers
/// `initialize`.
#[tool_handler]
impl<P: Projector + 'static> ServerHandler for FinancialsServer<P> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            // `env!` here expands in *this* crate, so it reports
            // financials-mcp's name and version. (rmcp's
            // `Implementation::from_build_env()` would report "rmcp": its
            // `env!` calls expand inside rmcp.)
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;
    use rmcp::service::{RoleClient, RunningService};

    const PLAN: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn projection() -> Projection {
        serde_json::from_str(PROJECTION).unwrap()
    }

    /// Starts the server with `projector` and connects an in-process client.
    ///
    /// `tokio::io::duplex` makes a pair of connected in-memory pipes: what one
    /// end writes, the other reads, just like the stdin/stdout pair a real
    /// client uses, without a child process. `()` is rmcp's do-nothing client
    /// handler.
    async fn connect(projector: FakeProjector) -> RunningService<RoleClient, ()> {
        let (server_end, client_end) = tokio::io::duplex(64 * 1024);
        let server = FinancialsServer::new(projector);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_end).await {
                let _ = running.waiting().await;
            }
        });
        ().serve(client_end).await.expect("client should connect")
    }

    /// Calls `run_projection` with the given plan JSON text.
    async fn call(client: &RunningService<RoleClient, ()>, plan: &str) -> CallToolResult {
        let plan: serde_json::Value = serde_json::from_str(plan).unwrap();
        let arguments = serde_json::json!({ "plan": plan });
        client
            .call_tool(
                CallToolRequestParams::new("run_projection")
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
            .expect("the call itself should succeed")
    }

    fn text_of(result: &CallToolResult) -> &str {
        result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.as_str())
            .expect("text content")
    }

    #[tokio::test]
    async fn initialize_reports_name_version_and_instructions() {
        let client = connect(FakeProjector::scripted(Vec::new())).await;
        let info = client.peer_info().expect("server info after initialize");

        // `server_info` is optional in the protocol, so it's an `Option`;
        // `.as_ref()` borrows what's inside instead of moving it out.
        let server = info.server_info.as_ref().expect("server info");
        assert_eq!(server.name, "financials-mcp");
        assert_eq!(server.version, env!("CARGO_PKG_VERSION"));
        assert!(
            info.instructions
                .as_deref()
                .unwrap()
                .contains("never estimate")
        );
    }

    #[tokio::test]
    async fn lists_exactly_one_read_only_tool_with_the_plan_schema() {
        let client = connect(FakeProjector::scripted(Vec::new())).await;

        let tools = client.list_all_tools().await.unwrap();

        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name, "run_projection");
        assert!(
            tool.description
                .as_deref()
                .unwrap()
                .contains("retirement projection")
        );
        let annotations = tool.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));

        // The input schema describes the plan in full, from `Plan`'s types and
        // doc comments, with JSON (camelCase) field names.
        let schema = serde_json::to_string(&tool.input_schema).unwrap();
        for expected in [
            "plan",
            "items",
            "amountCents",
            "annualReturnRateBasisPoints",
            "drawdownYears",
        ] {
            assert!(schema.contains(expected), "schema is missing {expected}");
        }
        assert!(
            !schema.contains("Rust"),
            "Rust notes leaked into the schema"
        );
    }

    #[tokio::test]
    async fn run_projection_returns_the_summary_and_assumptions() {
        let fake = FakeProjector::scripted(vec![Ok(projection())]);
        let client = connect(fake).await;

        let result = call(&client, PLAN).await;

        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(body["summary"]["depletionYear"], 36);
        assert_eq!(body["assumptions"]["drawdownYears"], 30);
        assert_eq!(
            body["assumptions"]["withdrawalInflationRateBasisPoints"],
            250
        );
        assert_eq!(body["assumptions"]["returns"][0]["item"], "Example 401(k)");
        assert_eq!(
            body["assumptions"]["returns"][0]["annualReturnRateBasisPoints"],
            700
        );
    }

    #[tokio::test]
    async fn an_invalid_plan_is_a_tool_error_the_model_can_read() {
        let client = connect(FakeProjector::scripted(Vec::new())).await;

        let result = call(&client, r#"{"years": 99, "items": []}"#).await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "invalid plan: items are required; years must be between 1 and 75"
        );
    }

    #[tokio::test]
    async fn a_wrongly_typed_plan_is_a_tool_error_without_the_value() {
        let client = connect(FakeProjector::scripted(Vec::new())).await;
        let wrong = PLAN.replacen("25000000", "250000.75", 1);

        let result = call(&client, &wrong).await;

        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).contains("`items[0].amountCents`"),
            "{}",
            text_of(&result)
        );
        assert!(!text_of(&result).contains("250000"), "{}", text_of(&result));
    }

    #[tokio::test]
    async fn api_failures_are_tool_errors_too() {
        let client = connect(FakeProjector::scripted(vec![Err(Error::Auth)])).await;

        let result = call(&client, PLAN).await;

        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).contains("FINANCIALS_ACCESS_TOKEN"),
            "{}",
            text_of(&result)
        );
    }
}
