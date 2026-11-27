# financials-mcp Learning Implementation Plan

> **Purpose:** Build a small Rust MCP server one reviewable step at a time. It lets an AI agent run many retirement projections through the financials-api engine and explain the trade-offs. Each step should teach one Rust or MCP concept, include deterministic tests, and land through its own pull request after this initial plan commit.

The model chooses which scenarios to try and explains the results; **the financials-api engine does all the math**. Steps 1–18 build the server and run it locally over stdio; Steps 19–22 host it on AWS Lambda over HTTP so Claude can use it from any device. This is also a first Rust codebase, so early steps are deliberately small, and each step names the Rust concepts it introduces.

## Working agreement

- This plan is the source of truth for the staged build.
- After this initial plan commit, every implementation step happens on its own branch.
- Branch naming format: `step-NN-short-description`, for example `step-01-rust-project-foundation`.
- Every step gets a GitHub pull request before it is merged into `main`.
- PR title format: `Step N: Short step description`, for example `Step 1: Rust project foundation`.
- Each PR should explain:
  - what concept the step teaches (Rust and/or MCP)
  - what code changed
  - what tests were added
  - how it was verified
- **Every step teaches Rust along the way.** Zach knows Go and Python but is new to Rust, so each PR (and the summary when the step is handed over) includes a short **Rust notes** section: a few specific places in that step's code where a Rust concept shows up, what it does, and how it compares to Go or Python. Code comments explain a concept where it first appears.
- Zach reviews/approves/merges PRs before the next step is based on `main`.
- After a PR is created, add the PR link to that step in this plan.
- On the step branch, mark that step as completed once the step's implementation and tests are done so it will show as completed when the PR is merged into `main`.
- Updating the plan status/PR links should be part of the relevant step branch/PR unless Zach asks for a separate docs-only update.
- Prefer the simplest Rust that works. When a step uses a more advanced feature (traits, generics, async, lifetimes), the PR should explain why it's needed there.

## Testing standard

Every behavior introduced by a commit gets a deterministic test.

- Unit tests must not call the real financials-api or any LLM.
- HTTP behavior is tested against a mock server (the `wiremock` crate); projection logic is tested with a fake projector behind a trait.
- Fixture plans and responses in `tests/fixtures/` are **made up**. Real financial data is never committed.
- Calls to a real financials-api (local, `ephemeral` mode) and real MCP clients (MCP Inspector, Claude Desktop/Code) are manual smoke tests only.
- Each implementation PR should pass:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The development rhythm should be:

```text
small concept -> failing or updated test -> minimal implementation -> passing tests -> PR -> review/merge
```

## Proposed project shape

This section started as the intended layout. What was actually built (Steps 1–18) is listed in the README's [Project layout](../../README.md#project-layout) and described module by module in [docs/architecture.md](../architecture.md#modules). In short:

- The planned `scenarios.rs` became `compare.rs`, `sweep.rs` and `withdrawal_order.rs`, plus `batch.rs` for the shared concurrency.
- The planned `solver.rs` became `max_spending.rs`.
- `guidance.rs` and `logging.rs` were added in Steps 15 and 16.

Steps 19–22 add the HTTP transport, auth and limits, and a Lambda binary.

## Design reference

Steps refer back to this section. It's the target design; steps build it up piece by piece.

### How it fits together

```text
You ──► Claude / agent-harness ──MCP──► financials-mcp (Rust) ──HTTPS──► financials-api POST /projections
          (picks scenarios,             (applies overrides, runs            (ephemeral mode: pure
           explains results)             batches and searches, summarizes)    calculator, stores nothing)
```

Hosted (Steps 19–22), the same server runs on Lambda behind an HTTP API:

```text
Claude (any device) ──HTTPS + bearer token──► API Gateway ──► Lambda: financials-mcp ──HTTPS──► financials-api
```

Example questions it should eventually handle:

- "Can we retire 2 years earlier? What would we have to give up?"
- "What's the most we can spend each year and still have $200k left at the end?"
- "How sensitive is this plan to returns being 1% lower?"
- "Which account should we draw from first?"

### Principles

- **Stateless.** No storage of any kind. The plan travels with every tool call, which matches the API's `ephemeral` mode and the UI's browser-owned design.
- **One source of truth for the math.** All projections go through financials-api; the server never reimplements the engine.
- **Batch work on the server.** A comparison, sweep or search is *one* tool call that runs many projections.
- **Read-only.** Every tool is a pure computation, annotated `readOnlyHint: true`.

### The plan

A **plan** is exactly the financials-api projection request (`internal/projections/projection.go` → `Request`): `years`, `savingYears`, `drawdownYears`, `annualWithdrawalCents`, `annualWithdrawalInflationRateBasisPoints`, `people`, `gapAnnualWithdrawalCents`, `expenses`, `mortgage`, and `items`. Money is in **integer cents** and rates are in **basis points**, as in the API. financials-api is the authority on validation; the server passes its `400` messages back as tool errors.

Today the UI's "download backup" exports only `items`. People, expenses, the mortgage and timeline settings live only in the browser's `localStorage`. An "Export plan" action in financials-ui is listed under [Additional functionality](#additional-functionality).

### Summary shape

Every tool reports projections as a compact summary computed from the API's `Projection.totals` (`summary.rs`). Amounts are whole dollars. This is the real summary for the basic fixture plan:

```json
{
  "lastsThroughFinalYear": false,
  "depletionYear": 36,
  "yearsShort": 5,
  "endingBalance": 0,
  "lowestBalance": {"year": 36, "amount": 0},
  "totalWithdrawn": 2716528,
  "totalUnfunded": 655422,
  "phases": {"savingYears": 10, "transitionYears": 0, "drawdownYears": 30},
  "currency": "USD"
}
```

- `depletionYear` is the first year with `unfundedWithdrawalCents > 0`, or absent if the plan never falls short. `yearsShort` counts the short years.
- `lockedWhenShort` appears when money existed but couldn't be withdrawn yet in the first short year, so "locked" isn't mistaken for "ran out".
- `mortgage.paidOffInYear` appears for plans with a mortgage.
- Batch tools return a smaller `Headline` per result. Every tool includes an `assumptions` block (timeline, inflation, returns).

### Tools

| Tool | What it does | Limits |
|---|---|---|
| `run_projection(plan, overrides?)` | One projection, returning a summary and its assumptions | 20 overrides |
| `compare_scenarios(plan, scenarios[])` | A baseline plus named scenarios, run in parallel, each with its difference from the baseline | 12 scenarios |
| `sweep(plan, parameter, values[])` | Varies one parameter and reports the breakpoints where the outcome changes | 25 values |
| `find_max_spending(plan, constraints)` | Binary search for the highest spending that keeps `minBalance` through `throughYear` | 25 projections, $100 precision |
| `withdrawal_order_options(plan)` | Compares `withdrawalPriority` orderings across accounts | 13 variants |

### Override operations

| `op` | Effect |
|---|---|
| `shift_retirement` | Moves retirement by ±N years (everyone, or one `personId`); the final year stays fixed, so drawdown absorbs the change |
| `set_drawdown_years` | Sets `drawdownYears` |
| `set_annual_withdrawal` | Sets `annualWithdrawalCents` (non-itemized plans) |
| `scale_expenses` | Scales all drawdown spending to an integer `percent` (itemized expenses or the annual withdrawal, plus the gap withdrawal; not the mortgage) |
| `add_expense` / `remove_expense` / `set_expense` | Itemized expense edits |
| `adjust_returns` | Adds ±N basis points to every item's return (optionally drawdown only) |
| `set_item` | Changes one item's contribution, return or withdrawal priority |
| `set_inflation` | Sets the withdrawal inflation rate |

### Guidance for the model

- Use the tools for **every** number. Never estimate projections.
- Ask what the user is optimizing for (retire earliest, spend most, biggest buffer), then present trade-offs; don't pick a single "best" plan. This isn't financial advice.
- Always state assumptions and limitations: returns are fixed each year (no bad market years or sequence risk), and there are no taxes or Social Security rules beyond what the inputs include.
- Prefer one batch tool call over many `run_projection` calls.

### Security and privacy

- **No data at rest.** Nothing is stored, cached or written to disk.
- **No plan contents in logs.** Logs record tool name, duration, number of projections and error type only.
- **stdout is reserved for the MCP protocol** in stdio mode, so logs go to stderr.
- The financials-api access token is never printed, logged or returned to clients.
- The plan is visible to the model provider, as when pasting it into a chat; tool outputs stay compact.
- **Hosted mode (Steps 19–22):** every HTTP request needs the bearer token, the `Host` header must be on an allow list, and request size and time are capped. Plans pass through Lambda in memory only, CloudWatch logs follow the same no-contents rule, and secrets are `NoEcho` deploy parameters, never in git. Claude's custom connectors call the server from Anthropic's cloud, not from your device.

## Step checklist

### Step 01 — Rust project foundation

- **Status:** Completed
- **Branch:** `step-01-rust-project-foundation`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/1
- **Concept:** A Rust project is a Cargo package. Keep `main.rs` thin and put testable logic in a library crate (`lib.rs`).
- **Rust introduced:** Cargo, `Cargo.toml`, crates, modules, `fn main`, `#[test]`, `cargo fmt` and `cargo clippy`.
- **Functionality:**
  - `cargo new` with `src/main.rs` and `src/lib.rs`.
  - `main.rs` calls a library function that returns placeholder output.
  - Add `.gitignore` (`/target`, `.env`), an initial `README.md` with the purpose and run/test commands, and `AGENTS.md` with the same security-first rules as the other repos.
  - Add a GitHub Actions workflow running fmt, clippy and test on Linux and Windows.
- **Tests:**
  - A smoke test that the library function returns the expected placeholder output.
- **Verification:**
  - `cargo test`
  - `cargo run`

### Step 02 — Plan model with serde

- **Status:** Completed
- **Branch:** `step-02-plan-model-serde`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/2
- **Concept:** The plan is the API's projection request. Mirror it exactly in Rust types so there's no translation layer to drift.
- **Rust introduced:** structs, `derive`, `serde` and `serde_json`, `#[serde(rename_all = "camelCase")]`, `Option<T>`, `Vec<T>`, integer types (`i64` for cents, `i32` for basis points).
- **Functionality:**
  - `Plan`, `ItemInput`, `PersonInput`, `ExpenseInput` and `MortgageInput` types matching `financials-api/internal/projections/projection.go`.
  - Required and optional fields follow the UI's TypeScript request types; optional fields are omitted when absent (`skip_serializing_if`).
  - Made-up fixture plans in `tests/fixtures/` (`plan-basic.json`, `plan-household.json`), checked against a local financials-api.
- **Tests:**
  - The fixture deserializes, with the expected values.
  - A plan round-trips (deserialize, then serialize) to equivalent JSON.
  - Missing optional fields deserialize to `None` or zero; unknown fields are **rejected**, as the API does, so a misspelled field can't be silently dropped.
  - Field names serialize as camelCase (`annualWithdrawalCents`, not `annual_withdrawal_cents`).
- **Verification:**
  - `cargo test`

### Step 03 — Errors and input checks

- **Status:** Completed
- **Branch:** `step-03-errors-input-checks`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/3
- **Concept:** Fail early with clear, non-secret messages. financials-api is the real validator; the server only pre-checks the obvious things.
- **Rust introduced:** `Result<T, E>`, the `?` operator, `enum` error types with `thiserror`, `impl Display`, early returns.
- **Functionality:**
  - An `Error` enum for the app (invalid plan, configuration, API errors to come).
  - `Plan::check()` for quick pre-checks mirroring the API's wording: simple `years` 1–75 versus phase fields (mutually exclusive, 0–75, at least one projected year, per-person retirement), at most 30 expenses and no `annualWithdrawalCents` with expenses, at least one item, non-negative amounts.
  - `Plan::from_json_checked()` parses then checks (a showcase for `?`).
  - Error messages name the field at fault (`items[0].amountCents`, via `serde_path_to_error`) and never include amounts; serde's value-quoting messages are rewritten.
- **Tests:**
  - Each pre-check rejects a bad plan with a message naming the field.
  - A valid fixture plan passes.
  - Error messages contain no money values.
  - Pre-check wording verified against a local financials-api (all rules matched).
- **Follow-up for Step 07:** one financials-api `400` message (mortgage payment below the first month's interest) includes a cents amount, so passing API errors through must strip values too.
- **Verification:**
  - `cargo test`

### Step 04 — Configuration loading

- **Status:** Completed
- **Branch:** `step-04-configuration-loading`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/4
- **Concept:** Behavior depends on explicit configuration, and secrets must never leak through it.
- **Rust introduced:** `std::env`, implementing a trait by hand (`impl fmt::Debug` that redacts the token), constructors (`Config::from_env`), testing with injected values instead of global state.
- **Functionality:**
  - Read `FINANCIALS_API_URL` (default `http://localhost:8080`) and `FINANCIALS_ACCESS_TOKEN` (optional) from the process environment and an optional local `.env` file. The process environment wins, the same as in the other repos.
  - Commit `.env.example` with placeholders only; `.env` is gitignored.
  - `Debug` and any display of the config show the token as `<redacted>` (an `AccessToken` newtype with a hand-written `Debug` and no `Display`).
  - `http://` is only accepted for `localhost`/`127.0.0.1`/`[::1]`; other hosts need `https://`, so the token is never sent unencrypted. URLs with embedded credentials are rejected.
  - `.env` parsing matches financials-api's rules, and errors give line numbers only.
- **Tests:**
  - Defaults apply when nothing is set.
  - Environment values override `.env` values, which override defaults.
  - An invalid URL fails with a clear error.
  - Debug output never contains the token.
- **Verification:**
  - `cargo test`

### Step 05 — Projection response and summary

- **Status:** Completed
- **Branch:** `step-05-projection-summary`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/5
- **Concept:** Raw projections are large. The agent needs a compact, honest summary (see [Summary shape](#summary-shape)).
- **Rust introduced:** iterators (`iter`, `find`, `min_by_key`, `map`, `sum`), closures, borrowing (`&Projection`), converting cents to dollars without floating-point surprises.
- **Functionality:**
  - `Projection`, `YearlyBalance` and related response types mirroring the API.
  - `summarize(&Projection) -> Result<Summary, Error>`, a pure function computing `lastsThroughFinalYear`, `depletionYear`, `yearsShort`, `endingBalance`, `lowestBalance`, `totalWithdrawn`, `totalUnfunded`, `lockedWhenShort`, the phases, `mortgage.paidOffInYear` (nested `Option`: no mortgage / paid off / not paid off) and `currency`.
  - Response types are lenient (unknown fields and phases tolerated) while plans stay strict.
  - Real responses from a local financials-api for the two made-up fixture plans in `tests/fixtures/`.
- **Tests:**
  - The plan never runs out (`depletionYear` is `None`).
  - The plan runs out mid-drawdown, and in year 1.
  - Locked balances are reported and not treated as running out.
  - With a mortgage and without one.
  - Dollar rounding is correct at the edges (49 and 50 cents).
- **Verification:**
  - `cargo test`

### Step 06 — Projector trait and fake projector

- **Status:** Completed
- **Branch:** `step-06-projector-trait`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/6
- **Concept:** Put "run a projection" behind an interface, so everything built on top of it can be tested without the network.
- **Rust introduced:** traits, `async fn` in traits, `tokio` and `#[tokio::test]`, `Arc`, test doubles.
- **Functionality:**
  - `trait Projector: Send + Sync { fn project(&self, plan: &Plan) -> impl Future<Output = Result<Projection, Error>> + Send; }` (the expanded form of `async fn`, so the future is guaranteed `Send` for parallel batches).
  - `FakeProjector`, which returns scripted responses or calls a test-supplied function (`from_fn`) for each plan, and records the plans it received.
  - `project_and_summarize<P: Projector>()`: pre-check, project, summarize (the core every tool will call).
- **Tests:**
  - The fake returns scripted responses in order and records requests.
  - The fake can be told to return an error; running out of scripted responses panics as a test bug.
  - `project_and_summarize` summarizes, and an invalid plan never reaches the projector.
  - A projector shared via `Arc` across a spawned task through generic code (fails to compile without the `+ Send` bound).
- **Verification:**
  - `cargo test`

### Step 07 — financials-api HTTP client

- **Status:** Completed
- **Branch:** `step-07-financials-api-client`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/7
- **Concept:** The real projector calls `POST /projections`, with clear handling for every way an HTTP call can fail.
- **Rust introduced:** `reqwest`, async/await, `match` on status codes, timeouts, mapping errors into the app's `Error`.
- **Functionality:**
  - `ApiProjector` implements `Projector`, posting the plan as JSON and parsing the projection.
  - Sends `X-Financials-Access-Token` when configured.
  - A 10-second timeout per call.
  - Maps API `400` responses to an `InvalidPlan` error carrying the API's message, `401` to an auth error (with no token in the message), and timeouts and connection failures to clear errors.
  - Strips values from passed-through API messages (for example the mortgage check quotes the first month's interest in cents), keeping the field and rule.
  - Refuses redirects (the token is in a custom header that clients don't strip on cross-host redirects), caps response size at 8 MiB, marks the token header sensitive.
- **Tests (wiremock):**
  - A successful projection parses, and the request body is exactly the plan's JSON.
  - The access token header is sent only when configured.
  - The `400` message is passed through (split into problems, values stripped); `401` is reported as auth without any secret.
  - Timeouts, refused connections, `404`, `5xx`, redirects (the target never sees the request), oversized and non-projection responses become clear errors.
  - Manual: verified end to end against a local financials-api (open and token-protected).
- **Verification:**
  - `cargo test`

### Step 08 — Manual projection debug command

- **Status:** Completed
- **Branch:** `step-08-projection-debug-command`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/8
- **Concept:** Exercise the projection path directly before an MCP client drives it, the same way agent-harness's `tool` command let tools be debugged without a model.
- **Rust introduced:** `clap` derive (subcommands, arguments), reading files, `#[tokio::main]`, exit codes.
- **Functionality:**
  - `financials-mcp project <plan.json>` loads a plan, runs it through `ApiProjector`, and prints the summary as JSON.
  - Controlled errors for a missing file, invalid JSON, a failed pre-check and API errors, with a non-zero exit code.
- **Tests:**
  - Command parsing (subcommand, missing argument, unknown subcommand, clap's own definition check).
  - With a fake projector: the summary is printed for a valid plan, and each error case gives a clear message and a non-zero result.
  - The real binary: banner, `--help`, exit code 2 for usage mistakes, 1 for a missing file or an unreachable API, and a full run against a mock API (via `spawn_blocking`).
- **Verification:**
  - `cargo test`
  - Manual: start financials-api locally (`$env:FINANCIALS_STORAGE_DRIVER="ephemeral"; go run ./cmd/api`), then `cargo run -- project tests/fixtures/plan-basic.json`.

### Step 09 — First MCP server over stdio: `run_projection`

- **Status:** Completed
- **Branch:** `step-09-mcp-stdio-run-projection`
- **Pull Request:** https://github.com/zachfire9/financials-mcp/pull/9
- **Concept:** MCP basics. A client starts the server as a process and talks JSON-RPC over stdin/stdout. Tools are advertised with names, descriptions, input schemas and annotations.
- **Rust introduced:** the official MCP SDK (`rmcp`), generating JSON Schema for tool inputs (`schemars`), and using a library's macros and traits.
- **Functionality:**
  - `financials-mcp serve` starts an MCP server on stdio.
  - One tool: `run_projection(plan)`, returning the summary and the `assumptions` block, annotated read-only.
  - Short server instructions: use the tools for every number.
  - Nothing besides protocol messages is written to stdout.
  - Bad plans and API failures are tool errors (`isError`) the model can read, not protocol errors. The plan is advertised with its full schema but parsed by `Plan::from_json`, so errors keep never quoting values.
  - Plan field doc comments are model-facing (they become the schema's descriptions); Rust notes moved to `//` comments.
  - Minimum Rust raised to 1.88 (rmcp 3.x).
- **Tests:**
  - An in-process MCP client lists exactly one tool, with the expected schema and read-only annotation.
  - Calling `run_projection` with the fake projector returns the summary.
  - An invalid plan returns a tool error naming the field, not a crash.
  - The real binary speaks raw MCP over stdio, and nothing else reaches stdout.
- **Verification:**
  - `cargo test`
  - Manual: the MCP Inspector lists and calls the tool.
  - Manual: add a local `mcpServers` entry in Claude Desktop or Claude Code, then ask a question about a fixture plan.

### Step 10 — Override operations

- **Status:** Completed
- **Branch:** `step-10-override-operations`
- **Pull Request:** [#10](https://github.com/zachfire9/financials-mcp/pull/10)
- **Concept:** Scenarios are a small, explicit set of changes applied to a copy of the plan (see [Override operations](#override-operations)), so every change is easy to check and to describe.
- **Rust introduced:** enums with data (`enum Override { ShiftRetirement { years: i32, person_id: Option<String> }, ... }`), `match`, `#[serde(tag = "op")]`, `Clone` versus borrowing, mutable references.
- **Functionality:**
  - `apply_overrides(&Plan, &[Override]) -> Result<Plan, Error>`, applied in order to a clone. The original plan is never changed.
  - Unknown IDs and out-of-range results are errors naming the override at fault.
  - `run_projection` accepts optional `overrides`, and its `assumptions` reflect them.
- **Tests:**
  - Each op changes exactly the expected fields.
  - Ops combine in order.
  - The original plan is unchanged.
  - An unknown `personId`/`itemId` and invalid values give clear errors.
  - JSON with `"op": "shift_retirement"` deserializes into the right variant.
- **Verification:**
  - `cargo test`

### Step 11 — `compare_scenarios` with bounded concurrency

- **Status:** Completed
- **Branch:** `step-11-compare-scenarios`
- **Pull Request:** [#11](https://github.com/zachfire9/financials-mcp/pull/11)
- **Concept:** Batch work on the server: one tool call runs a baseline and several scenarios, in parallel but politely.
- **Rust introduced:** `tokio::task::JoinSet`, `Semaphore` for a concurrency limit (4), `Arc` sharing, collecting results in order.
- **Functionality:**
  - The `compare_scenarios(plan, scenarios[])` tool, with up to 12 named scenarios.
  - Returns one row per scenario: its summary plus its difference from the baseline (`depletionYear` change, `endingBalance` change).
  - One failed scenario is reported in its own row without failing the others.
- **Tests (fake projector):**
  - Rows come back in request order with correct differences.
  - No more than 4 projections run at once (the fake tracks how many are in flight).
  - More than 12 scenarios is rejected.
  - A single failing scenario shows as an error row.
- **Verification:**
  - `cargo test`
  - Manual: in Claude, "compare retiring 2 years earlier against returns 1% lower."

### Step 12 — `sweep` and breakpoint detection

- **Status:** Completed
- **Branch:** `step-12-sweep-breakpoints`
- **Pull Request:** [#12](https://github.com/zachfire9/financials-mcp/pull/12)
- **Concept:** Show where a plan stops working by varying one input across a range.
- **Rust introduced:** generic helper functions, `impl Iterator`, `windows()` for comparing neighbors.
- **Functionality:**
  - The `sweep(plan, parameter, values[])` tool, for `annual_withdrawal`, `retirement_shift`, `return_adjustment` and `expense_scale`, with up to 25 values.
  - Returns a compact series (one point per value) and the **breakpoints**: each neighboring pair of values where the plan goes from lasting through the final year to running short, or back.
- **Tests (fake projector):**
  - The breakpoint is found in the middle, at the first value, and not at all.
  - Values are checked (non-empty, at most 25, valid for the parameter).
- **Verification:**
  - `cargo test`

### Step 13 — `find_max_spending` search

- **Status:** Completed
- **Branch:** `step-13-find-max-spending`
- **Pull Request:** [#13](https://github.com/zachfire9/financials-mcp/pull/13)
- **Concept:** Because the engine is deterministic, the server can search for an answer instead of the model guessing.
- **Rust introduced:** loops with invariants, writing a pure search function that's generic over the `Projector` trait, careful integer arithmetic.
- **Functionality:**
  - The `find_max_spending(plan, constraints)` tool. `minBalanceCents` (default 0) must hold in every year from the first retirement through `throughYear` (default the final year), with no unfunded years.
  - Binary search on `annualWithdrawalCents`, or on a scale factor for itemized expenses.
  - Stops at a $100 precision or after 25 projections.
  - Returns the amount, the summary at that amount, and the number of projections used.
- **Tests (fake projector with a known answer):**
  - Converges to within $100 of the known maximum.
  - Respects the 25-projection cap.
  - Handles a plan that fails even at $0, and one that succeeds at the upper bound.
  - Works for itemized expenses.
- **Verification:**
  - `cargo test`
  - Manual: "what's the most we can spend and still have $200k left?"

### Step 14 — `withdrawal_order_options`

- **Status:** Completed
- **Branch:** `step-14-withdrawal-order-options`
- **Pull Request:** [#14](https://github.com/zachfire9/financials-mcp/pull/14)
- **Concept:** Answer "which account first?" by trying the meaningful orderings and comparing them.
- **Rust introduced:** generating combinations, deduplicating with `HashSet`, reusing existing building blocks (overrides and compare).
- **Functionality:**
  - The `withdrawal_order_options(plan)` tool generates distinct `withdrawalPriority` orders across accounts (at most 13 variants, every possible order for 3 accounts), runs them with the batch helper, and ranks them by how long the money lasts, then by ending balance.
- **Tests (fake projector):**
  - The variants are distinct and capped at 13.
  - The ranking order is correct.
  - A single-account plan returns one option with an explanation.
- **Verification:**
  - `cargo test`

### Step 15 — Model guidance and tool descriptions

- **Status:** Completed
- **Branch:** `step-15-model-guidance`
- **Pull Request:** [#15](https://github.com/zachfire9/financials-mcp/pull/15)
- **Concept:** Tool descriptions and server instructions are part of the program: they decide how well the model uses the tools.
- **Rust introduced:** `const` strings and documentation comments, plus a test that checks the guidance text.
- **Functionality:**
  - Final server instructions and tool descriptions covering the rules in [Guidance for the model](#guidance-for-the-model): every number from tools, ask about goals, state assumptions and limitations, prefer batch tools.
  - Tool errors suggest a fix (for example "at most 12 scenarios; split into two calls").
- **Tests:**
  - The instructions mention each required rule.
  - Every tool has a description and the read-only annotation.
  - Error messages for each limit include a suggested fix.
- **Verification:**
  - `cargo test`
  - Manual: a short conversation in Claude where the model asks about goals and states assumptions.

### Step 16 — Logging without plan contents

- **Status:** Completed
- **Branch:** `step-16-safe-logging`
- **Pull Request:** [#16](https://github.com/zachfire9/financials-mcp/pull/16)
- **Concept:** You need to see what the server did without ever logging private financial data, and in stdio mode logs must never touch stdout.
- **Rust introduced:** the `tracing` crate (spans, fields, subscribers), writing to stderr, structured fields.
- **Functionality:**
  - For each tool call, log the tool name, duration, number of projections and error type at `info` level, to stderr.
  - Level control through `FINANCIALS_MCP_LOG` (a level only; dependencies stay at `warn`, since rmcp logs full requests at `debug`).
  - No plan values, names, amounts or tokens in any log line.
- **Tests:**
  - A captured log for a tool call contains the expected fields.
  - It contains none of the fixture's names or amounts, nor the token.
  - Nothing is written to stdout outside the protocol.
- **Verification:**
  - `cargo test`

### Step 17 — Dependency and supply-chain checks

- **Status:** Completed
- **Branch:** `step-17-dependency-checks`
- **Pull Request:** [#17](https://github.com/zachfire9/financials-mcp/pull/17)
- **Concept:** A Rust project inherits the security of its dependencies. Check them continuously.
- **Rust introduced:** `Cargo.lock`, `cargo deny` (advisories, licenses, sources), Dependabot for Cargo and GitHub Actions.
- **Functionality:**
  - `cargo deny check` runs in CI, with a `deny.toml` policy.
  - Dependabot covers `cargo` and `github-actions`.
  - Actions are pinned to commit SHAs, matching agent-harness.
- **Tests/verification:**
  - CI passes with the new checks.
  - `cargo deny check` passes locally.

### Step 18 — Learning walkthrough documentation

- **Status:** Completed
- **Branch:** `step-18-learning-walkthrough-docs`
- **Pull Request:** [#20](https://github.com/zachfire9/financials-mcp/pull/20)
- **Concept:** The repo should be both a working app and a learning artifact.
- **Functionality:**
  - A walkthrough of the completed steps (`docs/walkthrough.md`), linked from a shorter README.
  - The architecture: the plan, the projector trait, the API client, the tools and the MCP transport (`docs/architecture.md`).
  - How an agent uses the tools, with an example conversation using real results for a fixture plan.
  - The test strategy, how to run it locally against financials-api, and how to connect Claude Desktop/Code.
  - Which Rust concepts each step introduced (`docs/rust-concepts.md`), and a tool reference (`docs/tools.md`).
  - `tests/docs.rs` checks links and anchors, and that the docs list every tool, subcommand, setting and source file.
- **Tests/verification:**
  - `cargo test`
  - Manually verify documented commands still match the app.
- **Verification:**
  - README is accurate and current.

### Step 19 — Streamable HTTP transport, locally

- **Status:** Completed
- **Branch:** `step-19-streamable-http`
- **Pull Request:** [#21](https://github.com/zachfire9/financials-mcp/pull/21)
- **Concept:** The same server over a second transport. MCP over HTTP lets clients that can't start a local process (Claude on the web and on a phone) reach the tools. Stateless mode suits Lambda, where any request may land on a fresh instance.
- **Rust introduced:** Cargo features (an `http` feature, on by default, and `#[cfg(feature = "http")]`), `tower::Service` (the request-to-response abstraction that rmcp, axum and Lambda share), axum routing, graceful shutdown with `tokio::signal`, `#[non_exhaustive]` config structs, and `macro_rules!` for running tests over both transports. (The multi-threaded runtime turned out not to be needed: requests mostly wait on the network, so the single-threaded runtime serves them concurrently.)
- **Functionality:**
  - `financials-mcp serve --http [--listen 127.0.0.1:8787]` serves the same five tools at `/mcp` with rmcp's Streamable HTTP server transport (`transport-streamable-http-server`), in stateless mode (`legacy_session_mode: false`, JSON responses).
  - Listens on loopback by default. rmcp's `Host` header check (protection against DNS rebinding) stays on, allowing loopback only for now; Step 20 makes the list configurable.
  - The same logging rules as stdio: one line per tool call, to stderr, with no plan contents.
  - Ctrl+C stops the server cleanly after in-flight requests finish.
- **Tests:**
  - The tool tests run over both transports through one shared helper: in-process stdio, and a local HTTP server on a random port, both with a fake projector.
  - Stateless: independent requests from separate clients work without a session id.
  - A request with a `Host` header that isn't allowed is rejected.
  - The crate still builds and passes tests without the `http` feature (`cargo test --no-default-features` in CI).
  - Also fixed: the Step 16 log-capture test was flaky when run alongside other tests (per-thread subscribers race with `tracing`'s global cache); capture now routes one global subscriber's output to each test's buffer.
- **Verification:**
  - `cargo test`
  - Manual: `claude mcp add --transport http financials-http http://127.0.0.1:8787/mcp`, then run a comparison.

### Step 20 — Bearer-token auth and request limits

- **Status:** Completed
- **Branch:** `step-20-http-auth-and-limits`
- **Pull Request:** [#22](https://github.com/zachfire9/financials-mcp/pull/22)
- **Concept:** A URL anyone can reach needs a lock. Before the server leaves your machine, every HTTP request must carry a long secret, and size and time limits cap what any one request can cost.
- **Rust introduced:** tower middleware (layers that wrap a service), constant-time comparison (`subtle::ConstantTimeEq`) and why `==` on secrets leaks timing, reusing Step 04's redacted secret newtype, `tokio::time::timeout`, and paused time in tests (`tokio::time::pause`).
- **Functionality:**
  - `serve --http` requires `MCP_BEARER_TOKEN` (at least 32 characters) and refuses to start without it. Every request must send `Authorization: Bearer <token>`, compared in constant time. Failures return `401` with `WWW-Authenticate: Bearer` and no other detail.
  - `MCP_ALLOWED_HOSTS` lists the hostnames accepted in the `Host` header (default: loopback only).
  - A request body limit of 1 MiB (plans are a few KB), and a 25-second time limit per tool call, returned as a tool error (API Gateway's limit is 30 seconds).
  - Rejected requests log `auth_failed` (or the limit hit) with no token or header contents.
  - As built: the body limit is rmcp's own (`with_max_request_body_bytes`), the time limit applies to every tool call (stdio too), and the middleware is `axum::middleware::from_fn_with_state` (a tower layer). The bearer token is redacted like the access token, with a constant-time `PartialEq`.
- **Tests:**
  - Missing, malformed, wrong and wrong-length tokens get `401`; the right token works.
  - Startup fails with a missing or short token.
  - An oversized body gets `413` before reaching any tool.
  - A slow fake projector hits the time limit (with paused time) and returns a tool error.
  - Captured logs contain neither the token nor the `Authorization` header.
- **Verification:**
  - `cargo test`
  - Manual: `curl` without a token gets `401`; with it, `tools/list` returns five tools.

### Step 21 — Lambda packaging with cargo-lambda

- **Status:** Completed
- **Branch:** `step-21-lambda-packaging`
- **Pull Request:** [#23](https://github.com/zachfire9/financials-mcp/pull/23)
- **Concept:** Lambda runs a function per request, with no long-lived server process. The tower service from Steps 19–20 plugs into Lambda through an adapter, so the tools, auth and limits are identical to local HTTP.
- **Rust introduced:** `lambda_http` (API Gateway events as ordinary `http` requests), work done once per cold start versus per request, a second binary (`src/bin/lambda.rs`) behind a `lambda` feature, cross-compiling to `aarch64` with `cargo-lambda`, and release-profile settings (`lto`, `strip`) for size and cold-start time.
- **Functionality:**
  - A `lambda` binary that loads configuration from the environment (`FINANCIALS_API_URL`, `FINANCIALS_ACCESS_TOKEN`, `MCP_BEARER_TOKEN`, `MCP_ALLOWED_HOSTS`, `FINANCIALS_MCP_LOG`), builds the server once per cold start, and serves each invocation through the Step 20 service stack.
  - Logs go to CloudWatch under the same rules as Step 16.
  - `cargo lambda build --release --arm64 --bin lambda` produces the `bootstrap` executable for `provided.al2023`.
  - CI builds the Lambda artifact so packaging breakage is caught early, and `cargo deny` covers the new crates.
  - As built: `lambda_http` 1.3.2 with only `apigw_http`; cargo-lambda in CI from hash-pinned PyPI wheels (`.github/requirements/cargo-lambda.txt`, updated by Dependabot); CI checks the binary is arm64 and needs glibc ≤ 2.34; `deny.toml` now checks all features, so the `lambda` dependencies are covered.
- **Tests:**
  - Handler tests with synthetic API Gateway HTTP API (v2) events: `initialize` and `tools/call` succeed with the right token; no token gets `401`; an oversized body is rejected.
  - The shared tool tests pass through the Lambda adapter too.
- **Verification:**
  - `cargo test`
  - Manual: `cargo lambda watch`, then call it locally with `curl`; note the artifact size in the PR.

### Step 22 — Deploy with SAM and GitHub OIDC, then connect Claude

- **Status:** Completed
- **Branch:** `step-22-deploy-and-connect`
- **Pull Request:** [#24](https://github.com/zachfire9/financials-mcp/pull/24)
- **Concept:** Infrastructure as code, mirroring financials-api. A SAM template describes the function and its API, a GitHub Actions workflow deploys it with short-lived OIDC credentials (no stored AWS keys), and Claude connects to the resulting URL.
- **Rust introduced:** little new Rust; how a release build gets from `cargo lambda build` into a SAM deployment in CI.
- **Functionality:**
  - `template.yaml`:
    - an arm64 `provided.al2023` function (128–256 MB, 29-second timeout)
    - an HTTP API with throttling, as in financials-api
    - optional reserved concurrency as a cost backstop
    - `NoEcho` parameters for `MCP_BEARER_TOKEN` and `FINANCIALS_ACCESS_TOKEN`, plus `FINANCIALS_API_URL` and `MCP_ALLOWED_HOSTS`
    - CloudWatch log retention of 14 days
    - the API URL as a stack output
  - `.github/workflows/deploy.yml`: test, `cargo deny`, build, then `sam deploy` with an OIDC role, on manual dispatch. Actions are pinned to SHAs, and real values live in repository variables and secrets, never in git.
  - `infra/iam/`: a least-privilege deploy policy and the OIDC trust policy with placeholders, plus a README, as in financials-api.
  - As built:
    - The API Gateway resources (Api, Integration, `ANY /mcp` Route, `$default` Stage, Permission) are written out instead of SAM `Events`. Then the function sets `MCP_ALLOWED_HOSTS` to the API's own domain without a circular dependency, so it's not a parameter.
    - `CodeUri` is cargo-lambda's `bootstrap.zip`, with no `sam build`. `deploy.yml` runs `ci.yml` as a reusable workflow and deploys the zip artifact the CI `lambda` job uploads. CI also runs `sam validate --lint`.
    - All deployment identifiers are GitHub secrets, because the repo's Actions logs are public. The workflow masks the endpoint and account id.
    - IAM is scoped to `financials-mcp-McpFunctionRole-*`, so the deploy role can't edit itself.
    - The financials-api client retries `429` up to 4 times (`THROTTLE_RETRY_DELAYS`, 3.75 s at most), since financials-api's default throttling (2/s, burst 5) is below what a batch tool sends.
  - README: deploying, rotating the bearer token, and connecting clients:
    - **Claude Code:** `claude mcp add --transport http financials https://<api-url>/mcp --header "Authorization: Bearer <token>"`.
    - **Claude (web, desktop, mobile):** a custom connector with the URL and a request header `Authorization` set to `Bearer <token>`. Request-header authentication for custom connectors is in beta and not available to every account yet. If the Request headers section is missing, use Claude Code; OAuth is listed under [Additional functionality](#additional-functionality).
- **Tests/verification:**
  - `sam validate --lint`; CI passes; the deploy workflow succeeds.
  - Smoke test against the deployed URL: no token gets `401`; with the token, `tools/list` returns five tools.
  - A comparison from Claude: from a phone if connector request headers are available, otherwise from Claude Code.
  - CloudWatch shows tool-call lines and no plan contents.

## Additional functionality

These items are intentionally not part of the first learning sequence. They are scoped as named follow-up milestones so a future PR can reference the item by name instead of by step number.

### financials-ui "Export plan"

- **Status:** Future
- **Repo:** financials-ui
- **Concept:** Getting a complete plan into a conversation should be one click.
- **Functionality:**
  - Add an "Export plan" action that downloads the exact `POST /projections` request body the UI would send (`financials-plan-YYYY-MM-DD.json`), including people, expenses, the mortgage and the timeline.
  - Keep the existing items-only backup unchanged.
- **Tests:**
  - The exported JSON equals the request the UI sends for the same session.
  - Older saved sessions without the newer fields still export valid plans.
- **Verification:**
  - Export from the UI, then run it with `financials-mcp project <file>`.

### OAuth for Claude custom connectors

- **Status:** Future
- **Concept:** Steps 19–22 authenticate with a static bearer token. If Claude's request-header authentication for custom connectors isn't available on the account, OAuth is the other way to connect from Claude on the web and phone.
- **Functionality:** OAuth 2.1 protected-resource metadata and token validation in front of the HTTP transport, scoped when needed. (Hosting itself, previously listed here as "Hosting on AWS Lambda", is now Steps 19–22.)

### agent-harness MCP client support

- **Status:** Future
- **Repo:** agent-harness
- **Concept:** agent-harness gains the ability to use tools from any MCP server, starting with financials-mcp over stdio.
- **Functionality:** scoped separately as an entry in agent-harness's `docs/plans/feature-backlog.md`.

### Stress testing and sequence risk

- **Status:** Future
- **Concept:** Fixed annual returns hide the risk of bad years early in retirement.
- **Functionality:**
  - A `stress_test(plan)` tool for preset scenarios (returns lower by 1% and 2%, higher inflation).
  - If financials-api adds per-year return overrides, scenarios with bad first few years.

### financials-api batch endpoint

- **Status:** Future
- **Concept:** Run N projections in one HTTP call instead of N calls. Not needed at personal scale; worth it only if latency or Lambda cost becomes noticeable.
