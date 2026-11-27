# Architecture

How financials-mcp is put together: the pieces, how a tool call flows through them, the decisions behind the design, how an agent uses the tools, and how it's tested.

## The big picture

```text
You ──► Claude (Desktop or Code) ──MCP over stdio──► financials-mcp ──HTTPS──► financials-api
         picks scenarios,                             applies overrides,        POST /projections
         explains results                             runs batches and          (ephemeral mode: a pure
                                                      searches, summarizes      calculator, stores nothing)
```

- **Claude** decides *what* to ask: which scenarios to try, what to compare, when to search. It never computes a projection itself.
- **financials-mcp** turns one tool call into the right set of projections, runs them, and boils each one down to a compact summary.
- **financials-api** does all the math. It's the same engine the financials-ui planner uses, so answers match what you'd see there.

There are two transports, carrying the same JSON-RPC messages to the same server:

- **stdio:** the client starts `financials-mcp serve` as a child process and exchanges messages over its stdin and stdout, one per line. That's why nothing else may ever be printed to stdout while serving, and why logs go to stderr.
- **Streamable HTTP** (`serve --http`, in [`http.rs`](../src/http.rs)): each message is a `POST /mcp`, answered with JSON. It's stateless: no sessions, and each request is handled by a fresh clone of the server (sharing one projector). rmcp provides the transport as a `tower::Service`, and axum routes requests to it. It's locked: every request needs `Authorization: Bearer <MCP_BEARER_TOKEN>` (checked in constant time by middleware, before the body is read), the `Host` header must be on the allowed list, and bodies are capped at 1 MiB. Steps 21–22 host it on AWS Lambda.

## Modules

| Module | Role |
|---|---|
| [`lib.rs`](../src/lib.rs) | The library crate root: declares the modules (everything testable lives below it) |
| [`main.rs`](../src/main.rs) | The executable: parses the command line, installs logging for `serve`, runs a command, turns errors into exit codes |
| [`server.rs`](../src/server.rs) | The MCP server: the five tools, their arguments and results, and turning errors into tool results |
| [`guidance.rs`](../src/guidance.rs) | The instructions sent to the model at `initialize` |
| [`http.rs`](../src/http.rs) | `serve --http`: rmcp's Streamable HTTP service in stateless mode, routed by axum, behind bearer-token middleware, with the allowed hosts, a body limit and graceful shutdown (behind the `http` feature) |
| [`logging.rs`](../src/logging.rs) | stderr logging, the level filter, and `Counted`, a projector wrapper that counts and logs projections |
| [`plan.rs`](../src/plan.rs) | The plan types (the API request), strict parsing, and pre-checks in the API's wording |
| [`overrides.rs`](../src/overrides.rs) | The ten "what if" operations, applied in order to a copy of a plan |
| [`compare.rs`](../src/compare.rs) | `compare_scenarios`: baseline plus named scenarios, and the `Change` between summaries |
| [`sweep.rs`](../src/sweep.rs) | `sweep`: one parameter across many values, and breakpoint detection |
| [`max_spending.rs`](../src/max_spending.rs) | `find_max_spending`: double, then binary search, within a projection budget |
| [`withdrawal_order.rs`](../src/withdrawal_order.rs) | `withdrawal_order_options`: canonical orders, candidates, ranking |
| [`batch.rs`](../src/batch.rs) | `project_all`: runs many plans concurrently, at most 4 at once, results in input order |
| [`projector.rs`](../src/projector.rs) | The `Projector` trait, `project_and_summarize`, and `FakeProjector` for tests |
| [`api.rs`](../src/api.rs) | `ApiProjector`: the financials-api client, with timeouts, size caps and status mapping |
| [`config.rs`](../src/config.rs) | Settings from the environment and `.env`; the access and bearer tokens are redacted in `Debug`, and the bearer token is compared in constant time |
| [`projection.rs`](../src/projection.rs) | The API response types, parsed leniently |
| [`summary.rs`](../src/summary.rs) | `summarize`: projection to compact summary; `Headline` for batch results |
| [`error.rs`](../src/error.rs) | The `Error` enum, value-free messages, and `kind()` for logs |
| [`cli.rs`](../src/cli.rs) | The `project` debug command |

## A tool call, step by step

Here's what happens when the model calls `compare_scenarios` with a plan and two scenarios.

1. **rmcp receives the request.** The `#[tool_router]` macro on `FinancialsServer` generated the routing, so `tools/call` with `name: "compare_scenarios"` reaches the `compare_scenarios` method, with the arguments as `CompareScenariosArgs`.
2. **The logging wrapper starts.** Every tool goes through `FinancialsServer::call`. It wraps the shared projector in a fresh `Counted` for this call, starts a `tool_call{tool="compare_scenarios"}` span, and runs the work under a 25-second limit (`tokio::time::timeout`), so a call that runs too long becomes a tool error.
3. **The arguments are parsed.** `plan` and `scenarios` arrive as raw JSON (`serde_json::Value`) and are parsed by `Plan::from_json` and `Scenario::list_from_value`. That way a wrong field becomes a readable tool error that names the location but never the value.
4. **The plans are prepared.** `compare::compare_scenarios` checks the base plan (`Plan::check`), then applies each scenario's overrides to its own copy (`apply_overrides`). A scenario whose overrides don't apply keeps its error and is never projected.
5. **The batch runs.** `batch::project_all` spawns one task per plan into a `JoinSet`. Each task waits for one of 4 `Semaphore` permits, then calls `project_and_summarize`:
   - `Plan::check` again (cheap, and it keeps the function safe on its own)
   - `Counted::project`, which counts, then calls `ApiProjector::project`, which sends `POST /projections` with a 10-second timeout and an 8 MiB response cap
   - `summarize`, which turns the yearly totals into a `Summary`
6. **The results are put back in order.** Tasks finish in any order. Each returns its index, so results land in their own slots and the rows come out in request order.
7. **The comparison is built.** Each successful scenario gets `Change::between(baseline, scenario)`; a failed one becomes an `{name, error}` row. If the baseline itself failed, the whole call fails.
8. **The result is sent.** `tool_result` turns `Ok` into pretty JSON text, or `Err` into the error message marked `isError: true`. The wrapper logs one line (`tool call succeeded duration_ms=… projections=3`), and rmcp sends the response.

The other batch tools follow the same path with different preparation: `sweep` builds one override per value, `withdrawal_order_options` builds one plan per candidate order, and `find_max_spending` calls the projector one projection at a time, because each step of its search depends on the last.

## Design decisions

- **Stateless and read-only.** The plan travels with every call, and nothing is stored, cached or written. That matches financials-api's `ephemeral` mode and the planner UI's browser-owned data, and it means there's nothing to secure at rest.
- **One source of truth for the math.** Every number comes from financials-api. The server edits inputs (overrides), runs projections and summarizes outputs, but never projects anything itself.
- **The plan is the API request, field for field.** There's no translation layer to drift out of sync. Plans are parsed strictly (unknown fields are errors, as in the API), while responses are parsed leniently (unknown fields and phases are tolerated), so a newer financials-api doesn't break the server.
- **Money is integer cents, rates are basis points.** There are no floats anywhere. Scaling uses `i128` and explicit rounding.
- **Batch work on the server.** A comparison, sweep or search is one tool call that runs many projections. That's faster, keeps tool outputs compact, and stops the model from guessing.
- **Errors are values the model can act on.** Bad input and API failures are successful tool calls with `isError: true` and a message that names the field or override and suggests a fix. Only a broken protocol is a protocol error.
- **No plan contents anywhere but the conversation.** Error messages name locations and rules, never values. Logs record the tool, duration, projection count and error *kind*. The log level can't be raised for dependencies, because the MCP library logs full requests at `debug`.
- **Generic over the projector.** Every tool works with any `Projector`: `ApiProjector` in production, `FakeProjector` in tests, and `Counted<…>` wrapping either for logging. Generics are resolved at compile time, so this costs nothing at runtime.
- **Concurrency without threads.** The server runs on tokio's single-threaded runtime, over HTTP too. Projections spend nearly all their time waiting on the network, so many requests in flight on one thread is enough.
- **Locked before it can be hosted.** `serve --http` refuses to start without a bearer token of at least 32 characters. Middleware rejects any request without it (`401`) before rmcp reads the body. The token is compared in constant time, and rejections log a fixed reason, never the header.
- **Optional transports are Cargo features.** The HTTP transport (and axum) is the default `http` feature. `cargo build --no-default-features` leaves it out entirely, and CI tests both builds.

## How an agent uses the tools

The server sends the model [instructions](../src/guidance.rs) when it connects:
- take every number from a tool
- ask what the user is optimizing for
- present trade-offs rather than one "best" plan
- prefer batch tools
- state the assumptions and limitations every time

Each tool's description says when to use it instead of another.

### An example conversation

This uses the made-up [basic fixture plan](../tests/fixtures/plan-basic.json): 10 more years of saving, then 30 years of drawdown at $60,000 a year growing 2.5% a year, with three accounts returning 6–7%. **The numbers are real financials-api results for that plan, from end-to-end runs while building Steps 10–13. Claude's wording is illustrative.**

> **You:** Here's our plan. Does the money last?

Claude calls `run_projection` with the plan. The summary says `lastsThroughFinalYear: false`, `depletionYear: 36` and `yearsShort: 5`.

> **Claude:** Not quite. On this plan the money runs short in year 36, the 26th year of retirement, and the last 5 years can't be fully covered. That assumes a fixed 7% return on the 401(k) and Roth and 6% on the brokerage account every year, withdrawals growing 2.5% a year, and no taxes. Before I look at options: what matters most to you? Retiring as early as possible, spending as much as you can, or keeping a buffer?

> **You:** We'd like to keep spending about the same. What would it take?

Claude makes one `compare_scenarios` call rather than several `run_projection` calls:

```json
{
  "plan": { "...": "..." },
  "scenarios": [
    { "name": "Retire 2 years later", "overrides": [{ "op": "shift_retirement", "years": 2 }] },
    { "name": "Spend 15% less", "overrides": [{ "op": "scale_expenses", "percent": 85 }] },
    { "name": "Returns 1 point lower", "overrides": [{ "op": "adjust_returns", "basisPoints": -100 }] }
  ]
}
```

It gets back three rows:
- **Retire 2 years later** lasts, ending with $773,914.
- **Spend 15% less** lasts, ending with $675,070.
- **Returns 1 point lower** runs short 6 years sooner, in year 30.

To find how much later is enough, Claude calls `sweep` with `"parameter": "retirement_shift", "values": [-2, 0, 1, 2, 4]`. The breakpoint is `{"lastsAt": 1, "shortAt": 0}`.

> **Claude:** Retiring just **one year later** is enough for the money to last, though only barely: it ends with about $36,000. Two years later leaves about $774,000. Spending 15% less would also work, ending with about $675,000. It's worth knowing how sensitive the plan is: with returns one point lower, it runs short in year 30. All of these assume the same return every year, so a bad stretch early in retirement would make things worse than these numbers show.

> **You:** If we retire on schedule, what's the most we could spend and still have $200k left?

Claude calls `find_max_spending` with `"constraints": {"minBalanceCents": 20000000}`. It runs 12 projections and returns `annualSpending: 54023`.

> **Claude:** About **$54,000 a year** in the first year of retirement, down from $60,000, keeps at least $200,000 every year through the end. Without the $200k buffer, the most is about $55,250.

The model never estimated a number: each one came from a tool. It asked about goals, compared options in batch calls, and stated the assumptions.

## Testing strategy

Every behavior has a deterministic test, and **no test calls the real financials-api or a model**. There are 220 tests (188 without the `http` feature):

| Kind | Where | What they cover |
|---|---|---|
| Unit tests | `#[cfg(test)] mod tests` in each `src/` file | Parsing and checks, each override, summaries, the search, ranking, error messages, log filters |
| Fake projector | `FakeProjector` in [`projector.rs`](../src/projector.rs) | Every tool's logic, with scripted or computed projections, plus counting and concurrency fakes in `batch.rs` |
| Mock HTTP server | [`wiremock`](https://crates.io/crates/wiremock) in [`api.rs`](../src/api.rs) and the logging test | The real HTTP client: status codes, timeouts, redirects, size caps, the token header, and that the token never reaches logs |
| In-process MCP client | `server.rs` tests, over `tokio::io::duplex` **and** over HTTP on a local port | Tool listing, schemas, annotations, results and tool errors, through rmcp exactly as a client sees them. A `macro_rules!` macro runs each test once per transport |
| HTTP transport | `http.rs` tests | The bearer token (missing, malformed, wrong, a prefix, one character too many, a case-insensitive scheme), startup without a token, the `Host` allow list, the 1 MiB limit, stateless requests, graceful shutdown with a call in progress, and logs without the token |
| Paused time | `#[tokio::test(start_paused = true)]` in `server.rs` | The 25-second tool limit, which passes instantly and exactly |
| The real binary | [`tests/cli.rs`](../tests/cli.rs), [`tests/serve.rs`](../tests/serve.rs) | Exit codes and help text, raw MCP over stdio and over HTTP (`--listen 127.0.0.1:0`), nothing on stdout but protocol, logs on stderr |
| Fixtures | [`tests/fixtures/`](../tests/fixtures) | Two made-up plans and the real financials-api responses for them |
| Docs | [`tests/docs.rs`](../tests/docs.rs) | Links and anchors in the docs resolve, and the docs list every tool, subcommand and setting |

Some tests check the tests themselves. A few were confirmed to fail when the code they guard is broken on purpose:
- the concurrency limit (removing it lets 13 run at once)
- span propagation into spawned tasks
- the description and help-text leak checks

The manual checks run against a real local financials-api and a real client: the end-to-end scripts in each PR's verification section, and Claude Desktop or Claude Code.

CI runs on every push and pull request, and weekly:
- `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` on Linux and Windows, with and without default features
- `cargo deny check` for dependency advisories, licenses and sources
