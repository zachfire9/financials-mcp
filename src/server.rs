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
use std::time::Instant;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::Instrument;

use crate::compare::{Scenario, ScenarioRow, compare_scenarios};
use crate::error::Error;
use crate::guidance::INSTRUCTIONS;
use crate::logging::{self, Counted};
use crate::max_spending::{Constraints, MaxSpending, find_max_spending};
use crate::overrides::{Override, apply_overrides};
use crate::plan::Plan;
use crate::projector::{Projector, project_and_summarize};
use crate::summary::Summary;
use crate::sweep::{Parameter, Sweep, SweepResult, run_sweep};
use crate::withdrawal_order::{WithdrawalOrderOptions, withdrawal_order_options};

/// The most one tool call may take, from start to result. It stays under
/// API Gateway's 30-second limit (Steps 21–22), so a slow call becomes a
/// readable tool error rather than a gateway timeout.
pub const TOOL_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(25);

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
    /// Optional changes applied in order to a copy of the plan before
    /// projecting, such as `{"op": "shift_retirement", "years": -2}`.
    // Raw JSON for the same reason as `plan`. `Option` plus `default` makes
    // the field optional: leaving it out gives `None`.
    #[serde(default)]
    #[schemars(with = "Option<Vec<Override>>")]
    pub overrides: Option<serde_json::Value>,
}

/// Arguments for `compare_scenarios`. Raw JSON for the same reasons as
/// [`RunProjectionArgs`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompareScenariosArgs {
    /// The baseline plan every scenario starts from.
    #[schemars(with = "Plan")]
    pub plan: serde_json::Value,
    /// Up to 12 named scenarios, each a list of overrides to the baseline.
    #[schemars(with = "Vec<Scenario>")]
    pub scenarios: serde_json::Value,
}

/// The successful result of `compare_scenarios`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonResult {
    /// The baseline's summary and assumptions.
    pub baseline: ProjectionResult,
    pub scenarios: Vec<ScenarioRow>,
}

/// Arguments for `sweep`. Raw JSON for the same reasons as
/// [`RunProjectionArgs`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SweepArgs {
    /// The plan to vary.
    #[schemars(with = "Plan")]
    pub plan: serde_json::Value,
    /// Which input to vary.
    #[schemars(with = "Parameter")]
    pub parameter: serde_json::Value,
    /// 1 to 25 values in strictly ascending order, in the parameter's units:
    /// cents for `annual_withdrawal`, years for `retirement_shift`, basis
    /// points for `return_adjustment`, percent for `expense_scale`.
    #[schemars(with = "Vec<i64>")]
    pub values: serde_json::Value,
}

/// The successful result of `sweep`: the series and breakpoints, plus the
/// base plan's assumptions.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepToolResult {
    // `flatten` puts the sweep's own fields (`parameter`, `points`,
    // `breakpoints`) at the top level, next to `assumptions`.
    #[serde(flatten)]
    pub sweep: SweepResult,
    pub assumptions: Assumptions,
}

/// Arguments for `find_max_spending`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindMaxSpendingArgs {
    /// The plan whose spending to maximize.
    #[schemars(with = "Plan")]
    pub plan: serde_json::Value,
    /// Optional: a minimum balance to keep and the last year it applies to.
    #[serde(default)]
    #[schemars(with = "Option<Constraints>")]
    pub constraints: Option<serde_json::Value>,
}

/// The successful result of `find_max_spending`, plus the base plan's
/// assumptions.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaxSpendingResult {
    #[serde(flatten)]
    pub search: MaxSpending,
    pub assumptions: Assumptions,
}

/// Arguments for `withdrawal_order_options`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WithdrawalOrderArgs {
    /// The plan whose withdrawal order to compare.
    #[schemars(with = "Plan")]
    pub plan: serde_json::Value,
}

/// The successful result of `withdrawal_order_options`, plus the plan's
/// assumptions.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawalOrderResult {
    #[serde(flatten)]
    pub options: WithdrawalOrderOptions,
    pub assumptions: Assumptions,
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
    /// The overrides applied, echoed back so the scenario can be described.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<Override>,
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
    /// Collects the assumptions from a plan (after any overrides) and the
    /// overrides that produced it.
    fn from_plan(plan: &Plan, overrides: Vec<Override>) -> Assumptions {
        Assumptions {
            overrides,
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

    /// Project one retirement plan and summarize it: does the money last, the
    /// year it first runs short, years short, lowest and ending balances, and
    /// totals withdrawn and unfunded, plus the assumptions used. Use for a single
    /// plan, or one what-if via `overrides` (retire earlier, spend less, lower
    /// returns, add an expense) without rewriting the plan. For several options,
    /// use compare_scenarios instead. Plan money is in cents; results are whole
    /// dollars.
    // A `#[tool]` method's doc comment (above) becomes the tool's description,
    // so it's written for the model; Rust notes go in `//` comments like this.
    // Its `Parameters<T>` argument becomes the input schema (from `T`'s
    // `JsonSchema`). `annotations(...)` are hints clients can show or use: the
    // tool only reads and computes, so it's read-only, safe to retry, and
    // touches nothing outside the projection engine.
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
        Ok(self
            .call("run_projection", |projector| Self::project(projector, args))
            .await)
    }

    /// Compare a baseline plan with up to 12 named what-if scenarios in one
    /// call. Each scenario is a name and a list of overrides (the same ones
    /// run_projection accepts). Returns the baseline's summary and assumptions,
    /// then one row per scenario in request order: its summary and
    /// `changeFromBaseline` (scenario minus baseline; positive = later, more or
    /// bigger), or an error for that scenario alone. Use this instead of several
    /// run_projection calls. Results are whole dollars.
    #[tool(annotations(
        title = "Compare retirement scenarios",
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    pub async fn compare_scenarios(
        &self,
        Parameters(args): Parameters<CompareScenariosArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .call("compare_scenarios", |projector| {
                Self::compare(projector, args)
            })
            .await)
    }

    /// Vary one input across up to 25 ascending values to find where the plan
    /// stops (or starts) lasting through its final year. `parameter` is
    /// `annual_withdrawal` (cents), `retirement_shift` (years, + = later),
    /// `return_adjustment` (basis points added to every return) or
    /// `expense_scale` (percent of current spending). Returns one compact point
    /// per value and the `breakpoints`: neighboring values where the outcome
    /// changes (`lastsAt` / `shortAt`); sweep again with finer values between
    /// them to narrow it down. Results are whole dollars.
    #[tool(annotations(
        title = "Sweep one input across a range",
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    pub async fn sweep(
        &self,
        Parameters(args): Parameters<SweepArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .call("sweep", |projector| Self::run_sweep(projector, args))
            .await)
    }

    /// Find the most a plan can spend each year by searching with projections
    /// (at most 25, to within $100); use this instead of guessing amounts.
    /// Spending is `annualWithdrawalCents`, or for itemized plans every expense
    /// scaled together. Optional `constraints`: a balance of at least
    /// `minBalanceCents` (default 0) and no unfunded years, in every year from
    /// the first retirement through `throughYear` (default the final year);
    /// years after `throughYear` aren't checked. Returns `outcome` (`found`,
    /// `fails_at_zero` or `no_upper_limit`), the amount in cents and dollars,
    /// the summary at that amount, and the projections used.
    #[tool(annotations(
        title = "Find the maximum annual spending",
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    pub async fn find_max_spending(
        &self,
        Parameters(args): Parameters<FindMaxSpendingArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .call("find_max_spending", |projector| {
                Self::max_spending(projector, args)
            })
            .await)
    }

    /// Compare the orders the plan's accounts could be drawn down in: the
    /// current order, all accounts together (in proportion to balances), each
    /// account first, each account last and, for up to 3 accounts, every strict
    /// order (at most 13). Ranked best first by how long the money lasts, then
    /// ending balance. Each option has a description, headline numbers, its
    /// change from the current order, and each account's `withdrawalPriority`
    /// (usable with the `set_item` override). Rankings reflect returns and
    /// access dates only: projections include no taxes, so tax-driven orders
    /// (such as Roth last) aren't captured; say so when presenting them.
    /// Results are whole dollars.
    #[tool(annotations(
        title = "Compare withdrawal orders",
        read_only_hint = true,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    pub async fn withdrawal_order_options(
        &self,
        Parameters(args): Parameters<WithdrawalOrderArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .call("withdrawal_order_options", |projector| {
                Self::withdrawal_orders(projector, args)
            })
            .await)
    }

    async fn withdrawal_orders(
        projector: Arc<Counted<P>>,
        args: WithdrawalOrderArgs,
    ) -> Result<WithdrawalOrderResult, Error> {
        let plan = Plan::from_json(&args.plan.to_string())?;
        let options = withdrawal_order_options(projector, &plan).await?;
        Ok(WithdrawalOrderResult {
            options,
            assumptions: Assumptions::from_plan(&plan, Vec::new()),
        })
    }

    async fn max_spending(
        projector: Arc<Counted<P>>,
        args: FindMaxSpendingArgs,
    ) -> Result<MaxSpendingResult, Error> {
        let plan = Plan::from_json(&args.plan.to_string())?;
        let constraints = Constraints::from_value(args.constraints.unwrap_or_default())?;
        let search = find_max_spending(&*projector, &plan, constraints).await?;
        Ok(MaxSpendingResult {
            search,
            assumptions: Assumptions::from_plan(&plan, Vec::new()),
        })
    }

    async fn run_sweep(
        projector: Arc<Counted<P>>,
        args: SweepArgs,
    ) -> Result<SweepToolResult, Error> {
        let plan = Plan::from_json(&args.plan.to_string())?;
        // `json!` builds a JSON value with Rust expressions inside, so the
        // two fields are parsed together as one `Sweep`.
        let sweep = Sweep::from_value(serde_json::json!({
            "parameter": args.parameter,
            "values": args.values,
        }))?;
        let result = run_sweep(projector, &plan, &sweep).await?;
        Ok(SweepToolResult {
            sweep: result,
            assumptions: Assumptions::from_plan(&plan, Vec::new()),
        })
    }

    async fn compare(
        projector: Arc<Counted<P>>,
        args: CompareScenariosArgs,
    ) -> Result<ComparisonResult, Error> {
        let plan = Plan::from_json(&args.plan.to_string())?;
        let scenarios = Scenario::list_from_value(args.scenarios)?;
        let comparison = compare_scenarios(projector, &plan, &scenarios).await?;
        Ok(ComparisonResult {
            baseline: ProjectionResult {
                summary: comparison.baseline,
                assumptions: Assumptions::from_plan(&plan, Vec::new()),
            },
            scenarios: comparison.scenarios,
        })
    }

    /// Runs one tool call with logging: a `tool_call` span tagged with the
    /// tool name, a fresh [`Counted`] projector, and one `info` line at the
    /// end with the duration, the projection count and the error kind.
    ///
    /// `run` is a closure that receives the counted projector and returns a
    /// future. The `where` clause spells out what each type must be:
    /// - `F: FnOnce(...) -> Fut`: called once with the projector;
    /// - `Fut: Future<Output = Result<T, Error>>`: what it returns, awaited;
    /// - `+ Send`: rmcp may run tool calls on any thread, so everything held
    ///   across an `.await` must be sendable between threads.
    async fn call<T, F, Fut>(&self, tool: &'static str, run: F) -> CallToolResult
    where
        T: Serialize + Send,
        F: FnOnce(Arc<Counted<P>>) -> Fut + Send,
        Fut: Future<Output = Result<T, Error>> + Send,
    {
        let projector = Arc::new(Counted::new(Arc::clone(&self.projector)));
        let started = Instant::now();
        // `.instrument(span)` attaches the span to the future: every event
        // logged while it runs, including each projection's `debug` line and
        // the final `info` line, is tagged with `tool=...`, across `.await`
        // points. (Tasks spawned inside don't inherit it automatically;
        // `batch.rs` passes it on explicitly.)
        async move {
            // `tokio::time::timeout` races the work against a timer. If the
            // timer wins, the work's future is *dropped*, which cancels it:
            // every `.await` inside it simply never resumes, and anything it
            // owned (HTTP requests in flight, semaphore permits) is cleaned
            // up by `Drop`. Go would need a `context.Context` passed all the
            // way down for the same effect.
            let outcome =
                match tokio::time::timeout(TOOL_TIME_LIMIT, run(Arc::clone(&projector))).await {
                    Ok(outcome) => outcome,
                    Err(_elapsed) => Err(Error::TimedOut {
                        seconds: TOOL_TIME_LIMIT.as_secs(),
                    }),
                };
            logging::tool_call_finished(started, projector.count(), outcome.as_ref().err());
            tool_result(outcome)
        }
        .instrument(tracing::info_span!("tool_call", tool))
        .await
    }

    /// Parses, applies overrides, projects and summarizes, using the crate's
    /// own errors.
    async fn project(
        projector: Arc<Counted<P>>,
        args: RunProjectionArgs,
    ) -> Result<ProjectionResult, Error> {
        let plan = Plan::from_json(&args.plan.to_string())?;
        // `Option::map` parses the overrides only if they were sent;
        // `transpose` flips `Option<Result<..>>` into `Result<Option<..>>`
        // so `?` can return the error; `unwrap_or_default` gives an empty
        // list for `None`.
        let overrides = args
            .overrides
            .map(Override::list_from_value)
            .transpose()?
            .unwrap_or_default();
        let plan = apply_overrides(&plan, &overrides)?;
        // `&*projector` turns `&Arc<Counted<P>>` into `&Counted<P>`: `*`
        // reaches through the `Arc` to the projector, `&` borrows it.
        let summary = project_and_summarize(&*projector, &plan).await?;
        Ok(ProjectionResult {
            summary,
            assumptions: Assumptions::from_plan(&plan, overrides),
        })
    }
}

/// Turns a tool's outcome into an MCP result: pretty JSON on success, or the
/// error text marked `isError` so the model can read it.
///
/// `impl Serialize` in argument position accepts any serializable type: it's
/// shorthand for a generic `<T: Serialize>(outcome: Result<T, Error>)`.
fn tool_result(outcome: Result<impl Serialize, Error>) -> CallToolResult {
    match outcome {
        Ok(result) => CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).expect("a result always serializes"),
        )]),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(err.to_string())]),
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
    async fn connect<T: Projector + 'static>(projector: T) -> RunningService<RoleClient, ()> {
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
        call_with(client, serde_json::json!({ "plan": plan })).await
    }

    /// Calls `run_projection` with the given arguments object.
    async fn call_with(
        client: &RunningService<RoleClient, ()>,
        arguments: serde_json::Value,
    ) -> CallToolResult {
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

    /// How a test reaches the server: in-process over stdio-style pipes, or
    /// over real HTTP on a local port.
    #[derive(Debug, Clone, Copy)]
    enum Transport {
        Stdio,
        #[cfg(feature = "http")]
        Http,
    }

    /// Connects an MCP client to a server using `projector`, over `transport`.
    async fn connect_via<T: Projector + 'static>(
        transport: Transport,
        projector: T,
    ) -> RunningService<RoleClient, ()> {
        match transport {
            Transport::Stdio => connect(projector).await,
            #[cfg(feature = "http")]
            Transport::Http => {
                let url =
                    crate::http::start_for_tests(projector, crate::http::test_options()).await;
                let transport = rmcp::transport::StreamableHttpClientTransport::from_config(
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url)
                        .auth_header(crate::http::TEST_TOKEN),
                );
                ().serve(transport).await.unwrap()
            }
        }
    }

    /// Turns each test into one test per transport, so every tool behaves
    /// the same over stdio and HTTP.
    ///
    /// `macro_rules!` matches the code it's given against a pattern and
    /// writes new code from it, at compile time. Here,
    /// `async fn name(transport) { body }` becomes a module `name` holding
    /// `async fn run(transport: Transport) { body }` plus two `#[tokio::test]`
    /// functions, `stdio` and `http`, that call it. Test names come out as
    /// `server::tests::lists_read_only_tools_with_the_plan_schema::http`.
    /// `$name:ident` captures an identifier, `$body:block` a `{ ... }` block,
    /// and `$( ... )*` repeats for each test.
    macro_rules! on_every_transport {
        ($(async fn $name:ident($transport:ident) $body:block)*) => {
            $(
                mod $name {
                    use super::*;

                    async fn run($transport: Transport) $body

                    #[tokio::test]
                    async fn stdio() {
                        run(Transport::Stdio).await
                    }

                    #[cfg(feature = "http")]
                    #[tokio::test]
                    async fn http() {
                        run(Transport::Http).await
                    }
                }
            )*
        };
    }

    on_every_transport! {
    async fn initialize_reports_name_version_and_instructions(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
        let info = client.peer_info().expect("server info after initialize");

        // `server_info` is optional in the protocol, so it's an `Option`;
        // `.as_ref()` borrows what's inside instead of moving it out.
        let server = info.server_info.as_ref().expect("server info");
        assert_eq!(server.name, "financials-mcp");
        assert_eq!(server.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.instructions.as_deref(), Some(INSTRUCTIONS));
    }

    async fn every_tool_has_a_model_facing_description_and_read_only_hints(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 5);

        for tool in &tools {
            let name = tool.name.as_ref();
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(description.len() > 100, "{name}: description too short");
            assert!(description.contains("dollars"), "{name}: no units");

            // Rust notes belong in `//` comments, never in what the model
            // reads: check descriptions and input schemas.
            let schema = serde_json::to_string(&tool.input_schema).unwrap();
            for leak in ["Rust", "#[", "Parameters<", "serde", "borrow"] {
                assert!(!description.contains(leak), "{name} description: {leak}");
                assert!(!schema.contains(leak), "{name} schema: {leak}");
            }

            let hints = tool.annotations.as_ref().expect("annotations");
            assert!(hints.title.is_some(), "{name}: no title");
            assert_eq!(hints.read_only_hint, Some(true), "{name}");
            assert_eq!(hints.destructive_hint, Some(false), "{name}");
            assert_eq!(hints.idempotent_hint, Some(true), "{name}");
            assert_eq!(hints.open_world_hint, Some(false), "{name}");
        }
    }

    async fn lists_read_only_tools_with_the_plan_schema(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;

        let tools = client.list_all_tools().await.unwrap();

        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "compare_scenarios",
                "find_max_spending",
                "run_projection",
                "sweep",
                "withdrawal_order_options"
            ]
        );
        for tool in &tools {
            let annotations = tool.annotations.as_ref().expect("annotations");
            assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
        }
        let tool = tools
            .iter()
            .find(|tool| tool.name == "run_projection")
            .unwrap();
        assert_eq!(tool.name, "run_projection");
        assert!(
            tool.description
                .as_deref()
                .unwrap()
                .contains("retirement plan")
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
            "overrides",
            "shift_retirement",
            "basisPoints",
        ] {
            assert!(schema.contains(expected), "schema is missing {expected}");
        }
        // Only the plan is required; overrides are optional.
        assert_eq!(tool.input_schema["required"], serde_json::json!(["plan"]));
        assert!(
            !schema.contains("Rust"),
            "Rust notes leaked into the schema"
        );
    }

    async fn run_projection_returns_the_summary_and_assumptions(transport) {
        let fake = FakeProjector::scripted(vec![Ok(projection())]);
        let client = connect_via(transport, fake).await;

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

    async fn an_invalid_plan_is_a_tool_error_the_model_can_read(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;

        let result = call(&client, r#"{"years": 99, "items": []}"#).await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "invalid plan: items are required; years must be between 1 and 75"
        );
    }

    async fn a_wrongly_typed_plan_is_a_tool_error_without_the_value(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
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

    async fn api_failures_are_tool_errors_too(transport) {
        let client = connect_via(transport, FakeProjector::scripted(vec![Err(Error::Auth)])).await;

        let result = call(&client, PLAN).await;

        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).contains("FINANCIALS_ACCESS_TOKEN"),
            "{}",
            text_of(&result)
        );
    }

    async fn overrides_change_the_plan_that_is_projected(transport) {
        // The server takes ownership of the fake, so the test keeps its own
        // handle to a shared list: `Arc` (shared ownership) around a `Mutex`
        // (one writer at a time). The `move` closure owns one clone of the
        // `Arc`; the test keeps the other.
        let received: Arc<std::sync::Mutex<Vec<Plan>>> = Arc::default();
        let recorder = Arc::clone(&received);
        let fake = FakeProjector::from_fn(move |plan| {
            recorder.lock().unwrap().push(plan.clone());
            Ok(projection())
        });
        let client = connect_via(transport, fake).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = call_with(
            &client,
            serde_json::json!({
                "plan": plan,
                "overrides": [
                    {"op": "shift_retirement", "years": -2},
                    {"op": "adjust_returns", "basisPoints": -100}
                ]
            }),
        )
        .await;

        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let sent = received.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].saving_years, 8);
        assert_eq!(sent[0].drawdown_years, 32);
        assert_eq!(sent[0].items[0].annual_return_rate_basis_points, 600);

        // The assumptions describe the changed plan and echo the overrides.
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        let assumptions = &body["assumptions"];
        assert_eq!(assumptions["savingYears"], 8);
        assert_eq!(assumptions["drawdownYears"], 32);
        assert_eq!(
            assumptions["returns"][0]["annualReturnRateBasisPoints"],
            600
        );
        assert_eq!(assumptions["overrides"][0]["op"], "shift_retirement");
        assert_eq!(assumptions["overrides"][1]["basisPoints"], -100);
    }

    async fn without_overrides_none_are_echoed(transport) {
        let client = connect_via(transport, FakeProjector::scripted(vec![Ok(projection())])).await;
        let result = call(&client, PLAN).await;
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        assert!(body["assumptions"].get("overrides").is_none());
    }

    async fn a_bad_override_is_a_tool_error_naming_it(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = call_with(
            &client,
            serde_json::json!({
                "plan": plan,
                "overrides": [{"op": "set_item", "itemId": "item-boat", "annualContributionCents": 1}]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "overrides[0] (set_item): no item with id `item-boat`; use an `id` from the plan's `items` (items without an id can't be changed)"
        );

        let result = call_with(
            &client,
            serde_json::json!({ "plan": plan, "overrides": [{"op": "retire_now"}] }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).starts_with("override problem at `overrides[0].op`"),
            "{}",
            text_of(&result)
        );
    }

    async fn compare_scenarios_returns_the_baseline_and_rows(transport) {
        let fake = FakeProjector::from_fn(|_| Ok(projection()));
        let client = connect_via(transport, fake).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("compare_scenarios").with_arguments(
                    serde_json::json!({
                        "plan": plan,
                        "scenarios": [
                            {"name": "Lower returns", "overrides": [{"op": "adjust_returns", "basisPoints": -100}]},
                            {"name": "Unknown item", "overrides": [{"op": "set_item", "itemId": "nope", "withdrawalPriority": 1}]}
                        ]
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
            .await
            .unwrap();

        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(body["baseline"]["summary"]["depletionYear"], 36);
        assert_eq!(body["baseline"]["assumptions"]["drawdownYears"], 30);
        assert_eq!(body["scenarios"][0]["name"], "Lower returns");
        assert_eq!(body["scenarios"][0]["changeFromBaseline"]["yearsShort"], 0);
        assert!(
            body["scenarios"][1]["error"]
                .as_str()
                .unwrap()
                .contains("no item with id `nope`")
        );
    }

    async fn compare_scenarios_rejects_a_bad_scenario_list(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("compare_scenarios").with_arguments(
                    serde_json::json!({ "plan": plan, "scenarios": [] })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "scenario problem at `scenarios`: give between 1 and 12 scenarios; split a longer list into several calls"
        );
    }

    async fn sweep_returns_points_breakpoints_and_assumptions(transport) {
        // Lasts while the withdrawal is at most $55,000.
        let fake = FakeProjector::from_fn(|plan| {
            let mut result = projection();
            if plan.annual_withdrawal_cents <= 5_500_000 {
                for year in &mut result.totals {
                    year.unfunded_withdrawal_cents = 0;
                }
            }
            Ok(result)
        });
        let client = connect_via(transport, fake).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("sweep").with_arguments(
                    serde_json::json!({
                        "plan": plan,
                        "parameter": "annual_withdrawal",
                        "values": [5000000, 6000000]
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
            .await
            .unwrap();

        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(body["parameter"], "annual_withdrawal");
        assert_eq!(body["points"][0]["lastsThroughFinalYear"], true);
        assert_eq!(body["points"][1]["depletionYear"], 36);
        assert_eq!(
            body["breakpoints"],
            serde_json::json!([{"lastsAt": 5000000, "shortAt": 6000000}])
        );
        assert_eq!(body["assumptions"]["drawdownYears"], 30);
    }

    async fn sweep_errors_are_tool_errors(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("sweep").with_arguments(
                    serde_json::json!({
                        "plan": plan,
                        "parameter": "retirement_shift",
                        "values": [2, 1]
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "sweep problem at `values[1]`: values must be in strictly ascending order; sort them and remove duplicates"
        );
    }

    async fn find_max_spending_returns_the_amount_and_summary(transport) {
        // Every year funded while the withdrawal is at most $55,000.
        let fake = FakeProjector::from_fn(|plan| {
            let mut result = projection();
            if plan.annual_withdrawal_cents <= 5_500_000 {
                for year in &mut result.totals {
                    year.unfunded_withdrawal_cents = 0;
                }
            }
            Ok(result)
        });
        let client = connect_via(transport, fake).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("find_max_spending").with_arguments(
                    serde_json::json!({ "plan": plan, "constraints": {"throughYear": 40} })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();

        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        assert_eq!(body["outcome"], "found");
        let cents = body["annualSpendingCents"].as_i64().unwrap();
        assert!((5_490_000..=5_500_000).contains(&cents), "{cents}");
        assert_eq!(body["summary"]["lastsThroughFinalYear"], true);
        assert_eq!(body["throughYear"], 40);
        assert_eq!(body["assumptions"]["drawdownYears"], 30);
        assert!(body["projectionsUsed"].as_u64().unwrap() <= 25);
    }

    async fn find_max_spending_bad_constraints_are_tool_errors(transport) {
        let client = connect_via(transport, FakeProjector::scripted(Vec::new())).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("find_max_spending").with_arguments(
                    serde_json::json!({ "plan": plan, "constraints": {"throughYear": 99} })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "constraint problem at `constraints.throughYear`: must be between 1 and the plan's final year (40)"
        );
    }

    async fn withdrawal_order_options_ranks_the_orders(transport) {
        let client = connect_via(transport, FakeProjector::from_fn(|_| Ok(projection()))).await;
        let plan: serde_json::Value = serde_json::from_str(PLAN).unwrap();

        let result = client
            .call_tool(
                CallToolRequestParams::new("withdrawal_order_options").with_arguments(
                    serde_json::json!({ "plan": plan })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();

        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let body: serde_json::Value = serde_json::from_str(text_of(&result)).unwrap();
        let options = body["options"].as_array().unwrap();
        assert_eq!(options.len(), 13);
        assert_eq!(body["truncated"], false);
        // Identical outcomes: the stable sort keeps the current order first.
        assert_eq!(options[0]["rank"], 1);
        assert_eq!(options[0]["isCurrent"], true);
        assert_eq!(
            options[0]["description"],
            "Example brokerage first, then Example 401(k), then Example Roth IRA"
        );
        assert_eq!(options[0]["headline"]["depletionYear"], 36);
        assert_eq!(options[0]["priorities"][2]["itemId"], "item-brokerage");
        assert_eq!(body["assumptions"]["drawdownYears"], 30);
    }
    }

    /// Calls any tool with the given arguments object.
    async fn call_tool(
        client: &RunningService<RoleClient, ()>,
        name: &'static str,
        arguments: serde_json::Value,
    ) -> CallToolResult {
        client
            .call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn logs_record_each_tool_call_without_plan_contents() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Everything this crate logs, at every level.
        let (captured, _guard) = crate::logging::capture::start("financials_mcp=trace");

        // The real HTTP client, with a token, against a mock financials-api.
        let api = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/projections"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../tests/fixtures/projection-household.json")),
            )
            .mount(&api)
            .await;
        let config = crate::config::Config::from_sources(
            |key| match key {
                "FINANCIALS_API_URL" => Some(api.uri()),
                "FINANCIALS_ACCESS_TOKEN" => Some("tok-SECRET-4242".to_string()),
                _ => None,
            },
            None,
        )
        .unwrap();
        let client = connect(crate::api::ApiProjector::new(&config).unwrap()).await;

        let plan: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/plan-household.json")).unwrap();
        let shift =
            serde_json::json!([{"op": "shift_retirement", "years": -1, "personId": "person-a"}]);
        call_tool(
            &client,
            "run_projection",
            serde_json::json!({"plan": plan, "overrides": shift}),
        )
        .await;
        call_tool(
            &client,
            "compare_scenarios",
            serde_json::json!({"plan": plan, "scenarios": [
                {"name": "Person A retires earlier", "overrides": shift},
                {"name": "Travel removed", "overrides": [{"op": "remove_expense", "expenseId": "expense-travel"}]}
            ]}),
        )
        .await;
        call_tool(
            &client,
            "sweep",
            serde_json::json!({"plan": plan, "parameter": "expense_scale", "values": [80, 90]}),
        )
        .await;
        call_tool(
            &client,
            "find_max_spending",
            serde_json::json!({"plan": plan, "constraints": {"minBalanceCents": 12345678}}),
        )
        .await;
        call_tool(
            &client,
            "withdrawal_order_options",
            serde_json::json!({"plan": plan}),
        )
        .await;
        let failed = call_tool(
            &client,
            "run_projection",
            serde_json::json!({"plan": plan, "overrides": [{"op": "set_item", "itemId": "item-joint-cash", "withdrawalPriority": 9}]}),
        )
        .await;
        assert_eq!(failed.is_error, Some(true));

        let logs = captured.text();

        // One `info` line per tool call, tagged with the tool's span.
        for (tool, projections) in [
            ("run_projection", 1),
            ("compare_scenarios", 3),
            ("sweep", 2),
            ("find_max_spending", 1),
            ("withdrawal_order_options", 13),
        ] {
            let line = logs
                .lines()
                .find(|line| {
                    line.contains(&format!("tool=\"{tool}\"")) && line.contains("succeeded")
                })
                .unwrap_or_else(|| panic!("no success line for {tool}:\n{logs}"));
            assert!(
                line.contains(&format!("projections={projections}")),
                "{line}"
            );
            assert!(line.contains("duration_ms="), "{line}");
            assert!(line.contains(" INFO "), "{line}");
        }
        assert!(
            logs.contains("tool call failed duration_ms=")
                && logs.contains("error=\"invalid_override\""),
            "{logs}"
        );
        // Each projection logs at debug, inside its tool's span, including
        // the ones run in spawned tasks.
        assert_eq!(logs.matches("projection finished").count(), 20, "{logs}");
        assert!(
            logs.lines()
                .filter(|line| line.contains("projection finished"))
                .all(|line| line.contains("tool_call{tool=")),
            "{logs}"
        );

        // Nothing from the plan, the overrides, the constraints or the config.
        for private in [
            "Person A",
            "Person B",
            "Example",
            "Living costs",
            "Healthcare",
            "Travel",
            "person-a",
            "expense-travel",
            "item-joint-cash",
            "item-a-401k",
            "30000000",
            "300000",
            "5500000",
            "55000",
            "2500000",
            "25000",
            "12345678",
            "123457",
            "tok-SECRET-4242",
            "SECRET",
            "Retires earlier",
            "retires earlier",
        ] {
            assert!(!logs.contains(private), "logs contain {private:?}:\n{logs}");
        }
    }

    #[tokio::test]
    async fn the_default_level_keeps_tool_calls_and_drops_projection_details() {
        let (captured, _guard) = crate::logging::capture::start(crate::logging::DEFAULT_FILTER);
        let client = connect(FakeProjector::from_fn(|_| Ok(projection()))).await;

        call(&client, PLAN).await;

        let logs = captured.text();
        assert!(logs.contains("tool call succeeded"), "{logs}");
        assert!(!logs.contains("projection finished"), "{logs}");
    }

    /// A projector that never answers.
    struct Silent;

    impl Projector for Silent {
        async fn project(&self, _plan: &Plan) -> Result<crate::projection::Projection, Error> {
            std::future::pending().await
        }
    }

    // `start_paused = true` runs the test with tokio's clock paused: when
    // every task is waiting on a timer, time jumps straight to the next one.
    // The 25-second limit passes instantly, and exactly.
    #[tokio::test(start_paused = true)]
    async fn a_tool_call_past_the_time_limit_becomes_a_tool_error() {
        let client = connect(Silent).await;
        let started = tokio::time::Instant::now();

        let result = call(&client, PLAN).await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "the tool call took longer than 25 seconds and was stopped; \
             try fewer scenarios or values, or check that financials-api is responding"
        );
        assert!(started.elapsed() >= TOOL_TIME_LIMIT);
        // The server is still usable afterwards.
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 5);
    }
}
