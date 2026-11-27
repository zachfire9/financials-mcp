# Walkthrough

financials-mcp is being built one step per pull request, as a first Rust codebase: Steps 1–18 built the local server, and Steps 19–22 take it online. This page walks through them in order:
- **Built:** what the step added.
- **Look at:** where to look in today's code.
- **Rust:** which concepts it introduced (with links into the [Rust concepts index](rust-concepts.md)).
- **Lesson:** anything that went wrong along the way.

Each PR's description has the full story, including a "Rust notes" section comparing the step's Rust with Go and Python.

To follow along, read each step's files in order. Explanations of Rust concepts are in `//` comments where a concept first appears. The code has grown since each step, but those comments stayed.

**Contents:**
- **Foundations (1–8):**
  [project](#step-1-rust-project-foundation) ·
  [plan](#step-2-plan-model-with-serde) ·
  [errors](#step-3-errors-and-input-checks) ·
  [configuration](#step-4-configuration) ·
  [summary](#step-5-projection-response-and-summary) ·
  [projector trait](#step-6-projector-trait-and-fake-projector) ·
  [API client](#step-7-financials-api-http-client) ·
  [debug command](#step-8-projection-debug-command)
- **The MCP server (9–14):**
  [run_projection](#step-9-mcp-server-over-stdio) ·
  [overrides](#step-10-override-operations) ·
  [compare](#step-11-compare_scenarios) ·
  [sweep](#step-12-sweep-and-breakpoints) ·
  [max spending](#step-13-find_max_spending) ·
  [withdrawal order](#step-14-withdrawal_order_options)
- **Hardening (15–18):**
  [guidance](#step-15-model-guidance) ·
  [logging](#step-16-logging-without-plan-contents) ·
  [dependencies](#step-17-dependency-checks) ·
  [docs](#step-18-learning-walkthrough)
- **Going online (19–22):**
  [HTTP transport](#step-19-streamable-http-transport)

---

## Foundations

### Step 1: Rust project foundation

[PR #1](https://github.com/zachfire9/financials-mcp/pull/1)

- **Built:** a Cargo package with a thin binary over a library, a first test, and CI running fmt, clippy and tests on Linux and Windows.
- **Look at:** [`src/main.rs`](../src/main.rs), [`src/lib.rs`](../src/lib.rs), [`tests/cli.rs`](../tests/cli.rs), [`Cargo.toml`](../Cargo.toml).
- **Rust:**
  - [crates and modules](rust-concepts.md#modules-crates-and-cargo)
  - macros like `env!` and `format!`
  - [`&str` versus `String`](rust-concepts.md#ownership-and-borrowing)
  - [tests next to the code](rust-concepts.md#testing)

### Step 2: Plan model with serde

[PR #2](https://github.com/zachfire9/financials-mcp/pull/2)

- **Built:** `Plan` and its parts, mirroring the financials-api request field for field, plus the made-up fixture plans.
- **Look at:** [`src/plan.rs`](../src/plan.rs) (the table at the top maps Go struct tags to serde attributes), [`tests/fixtures/`](../tests/fixtures).
- **Rust:**
  - [structs and `derive`](rust-concepts.md#types-structs-enums-option-and-result)
  - [serde attributes](rust-concepts.md#macros-and-attributes)
  - [`Option` instead of nil](rust-concepts.md#types-structs-enums-option-and-result)
  - integer money
- **Lesson:** unknown fields are *rejected*, as the API does. Otherwise a misspelled `anualWithdrawalCents` would silently project with $0.

### Step 3: Errors and input checks

[PR #3](https://github.com/zachfire9/financials-mcp/pull/3)

- **Built:** the `Error` enum, `Plan::check()` pre-checks in financials-api's exact wording, and parse errors that name the field (`items[0].amountCents`) but never the value.
- **Look at:** [`src/error.rs`](../src/error.rs), `Plan::check` in [`src/plan.rs`](../src/plan.rs).
- **Rust:**
  - [`Result` and `?`](rust-concepts.md#error-handling)
  - enums with data
  - exhaustive `match`
  - `thiserror`
  - [ranges and iterators](rust-concepts.md#iterators-and-closures)
- **Lesson:** serde's own messages quote the bad value ("invalid type: floating point `10000.5`"), so they're rewritten before they reach a user, a model or a log.

### Step 4: Configuration

[PR #4](https://github.com/zachfire9/financials-mcp/pull/4)

- **Built:** `Config` from the environment, then `.env`, then defaults. The access token can't be printed, and `http://` is refused for anything but localhost.
- **Look at:** [`src/config.rs`](../src/config.rs) (`AccessToken`, `from_sources`).
- **Rust:**
  - [implementing a trait by hand](rust-concepts.md#traits-and-generics) (a redacting `Debug`)
  - the newtype pattern
  - closures as parameters for testable environment access
  - `let … else`
- **Lesson:** a real borrow-checker error in a test helper. Rust 2024 assumed a returned closure still borrowed its arguments, and `+ use<>` says it doesn't.

### Step 5: Projection response and summary

[PR #5](https://github.com/zachfire9/financials-mcp/pull/5)

- **Built:** the response types and `summarize`: does the money last, when it runs short, the lowest balance, totals, and money that was locked rather than gone.
- **Look at:** [`src/projection.rs`](../src/projection.rs), [`src/summary.rs`](../src/summary.rs).
- **Rust:**
  - [iterator chains](rust-concepts.md#iterators-and-closures) (`find`, `filter`, `min_by_key`, `sum`)
  - the `Option` toolkit
  - [`.as_ref()`](rust-concepts.md#ownership-and-borrowing)
  - enums without data
  - integer rounding

### Step 6: Projector trait and fake projector

[PR #6](https://github.com/zachfire9/financials-mcp/pull/6)

- **Built:** the `Projector` trait that everything builds on, `project_and_summarize`, and `FakeProjector` for tests.
- **Look at:** [`src/projector.rs`](../src/projector.rs).
- **Rust:**
  - [async and futures](rust-concepts.md#async-and-concurrency)
  - `-> impl Future + Send` in a trait
  - [generics versus trait objects](rust-concepts.md#traits-and-generics)
  - `Mutex` for interior mutability
  - `Arc`
- **Lesson:** I claimed a test wouldn't compile without `+ Send`, and it did. Concrete types let the compiler infer `Send`, so the test was rewritten to use generic code, where the bound really matters.

### Step 7: financials-api HTTP client

[PR #7](https://github.com/zachfire9/financials-mcp/pull/7)

- **Built:** `ApiProjector`, which handles every status code clearly. It also has a 10-second timeout, an 8 MiB response cap, no redirects (so the token can't follow one) and a sensitive token header.
- **Look at:** [`src/api.rs`](../src/api.rs) and its wiremock tests.
- **Rust:**
  - real `.await` points
  - `while let`
  - [matching with guards](rust-concepts.md#types-structs-enums-option-and-result)
  - `.transpose()`
  - the builder pattern
  - cheap `Clone` for shared handles
- **Lesson:** one financials-api `400` message quotes an amount in cents, so values are stripped from passed-through API errors too.

### Step 8: Projection debug command

[PR #8](https://github.com/zachfire9/financials-mcp/pull/8)

- **Built:** `financials-mcp project <plan.json>`, to run the whole path without an MCP client.
- **Look at:** [`src/cli.rs`](../src/cli.rs), [`src/main.rs`](../src/main.rs), [`tests/cli.rs`](../tests/cli.rs).
- **Rust:**
  - clap's derive API
  - `#[tokio::main]`
  - the [`Write` trait](rust-concepts.md#traits-and-generics) for testable output
  - `Path` versus `PathBuf`
  - `spawn_blocking`
- **Lesson:** I wrote that a blocking call in a test would deadlock, and it didn't, because wiremock runs on its own thread. The comment was corrected.

## The MCP server

### Step 9: MCP server over stdio

[PR #9](https://github.com/zachfire9/financials-mcp/pull/9)

- **Built:** `financials-mcp serve` with one tool, `run_projection`, returning a summary and its assumptions. Bad plans and API failures become tool errors the model can read.
- **Look at:** [`src/server.rs`](../src/server.rs) (`FinancialsServer`, `run_projection`, `get_info`), [`tests/serve.rs`](../tests/serve.rs).
- **Rust:**
  - [attribute macros](rust-concepts.md#macros-and-attributes) (`#[tool_router]`, `#[tool]`)
  - generic `impl` blocks
  - a hand-written `Clone` to avoid a bound
  - two failure layers in one type
- **Lessons:**
  - Cargo picked rmcp 2.2 instead of 3.11 because `rust-version` was too low (its resolver respects your minimum Rust version).
  - `env!` inside a library reports the *library's* name, so the server would have called itself "rmcp."

### Step 10: Override operations

[PR #10](https://github.com/zachfire9/financials-mcp/pull/10)

- **Built:** ten named "what if" operations (`shift_retirement`, `scale_expenses`, `adjust_returns`, …) applied in order to a copy of the plan, with errors that name the override at fault.
- **Look at:** [`src/overrides.rs`](../src/overrides.rs). The [tool reference](tools.md#overrides) has the full list.
- **Rust:**
  - [enums with data tagged in JSON](rust-concepts.md#types-structs-enums-option-and-result) (`#[serde(tag = "op")]`)
  - match guards
  - [`&Plan` versus `&mut Plan`](rust-concepts.md#ownership-and-borrowing)
  - [checked arithmetic](rust-concepts.md#numbers)
- **Design calls:** retiring earlier keeps the plan's final year fixed (drawdown absorbs it), and spending scales by an integer percent.

### Step 11: compare_scenarios

[PR #11](https://github.com/zachfire9/financials-mcp/pull/11)

- **Built:** a baseline plus up to 12 named scenarios in one call, run concurrently but no more than 4 at a time. Each row shows its change from the baseline, and one failure stays in its own row.
- **Look at:** [`src/compare.rs`](../src/compare.rs); the concurrency itself moved to [`src/batch.rs`](../src/batch.rs) in Step 12.
- **Rust:**
  - [`tokio::spawn`, `JoinSet` and `Semaphore`](rust-concepts.md#async-and-concurrency)
  - owned data in spawned tasks
  - a permit released on `Drop`
  - `#[serde(untagged)]` and `#[serde(flatten)]`
- **Lesson:** the test for the 4-at-a-time limit was checked by removing the limit: all 13 ran at once and the test failed.

### Step 12: sweep and breakpoints

[PR #12](https://github.com/zachfire9/financials-mcp/pull/12)

- **Built:** one input varied across up to 25 values, returning a compact series and every *breakpoint*, the neighboring values where the outcome flips. It also moved the concurrency code into the shared `batch::project_all`.
- **Look at:** [`src/sweep.rs`](../src/sweep.rs), [`src/batch.rs`](../src/batch.rs).
- **Rust:**
  - [`windows(2)` and slice patterns](rust-concepts.md#iterators-and-closures)
  - `impl Iterator` return types
  - collecting `Result`s
  - [monomorphized generics](rust-concepts.md#traits-and-generics)
  - `try_from`
- **Design call:** report every change of outcome, not "the first failing value," because retiring later *helps* rather than hurts.

### Step 13: find_max_spending

[PR #13](https://github.com/zachfire9/financials-mcp/pull/13)

- **Built:** a search for the most a plan can spend while keeping an optional minimum balance. It checks the floor, doubles to bracket the answer, then binary searches to within $100, in at most 25 projections.
- **Look at:** [`src/max_spending.rs`](../src/max_spending.rs).
- **Rust:**
  - loop invariants with `Option` and `let … else`
  - [overflow-safe arithmetic](rust-concepts.md#numbers) (`i128`, `saturating_mul`)
  - [a struct holding borrows](rust-concepts.md#ownership-and-borrowing) (lifetimes)
  - `&mut self` for sequential work
- **Lesson:** the real API rejects a $0 withdrawal when a plan has drawdown years, a rule the fakes didn't enforce. It was added to `Plan::check`, and the search now starts at 1 cent.

### Step 14: withdrawal_order_options

[PR #14](https://github.com/zachfire9/financials-mcp/pull/14)

- **Built:** every meaningful order for drawing down the accounts, deduplicated by what actually matters (the order of priority groups), projected and ranked.
- **Look at:** [`src/withdrawal_order.rs`](../src/withdrawal_order.rs).
- **Rust:**
  - [sorting by a tuple key with `Reverse`](rust-concepts.md#iterators-and-closures)
  - deduplicating with `HashSet`
  - recursive backtracking with `&mut`
  - slice patterns
  - the `From` trait
- **Lesson:** a cap of 12 silently dropped one of the 13 possible orders for three accounts, so the cap is 13. Rankings ignore taxes, and the tool description says so.

## Hardening

### Step 15: Model guidance

[PR #15](https://github.com/zachfire9/financials-mcp/pull/15)

- **Built:** the server instructions as one tested set, consistent tool descriptions, and a suggested fix in every limit's error.
- **Look at:** [`src/guidance.rs`](../src/guidance.rs), the tool doc comments in [`src/server.rs`](../src/server.rs).
- **Rust:**
  - [`concat!`](rust-concepts.md#macros-and-attributes)
  - `///` versus `//` (doc comments become tool descriptions)
  - constant tables in tests
- **Lesson:** `run_projection`'s description had carried my Rust teaching notes since Step 9. A test now checks every description and schema for leaks.

### Step 16: Logging without plan contents

[PR #16](https://github.com/zachfire9/financials-mcp/pull/16)

- **Built:** one log line per tool call on stderr (tool, duration, projections, error kind) and a debug line per projection. Nothing private appears in either.
- **Look at:** [`src/logging.rs`](../src/logging.rs), `FinancialsServer::call` in [`src/server.rs`](../src/server.rs).
- **Rust:**
  - [`tracing` events, spans and subscribers](rust-concepts.md#async-and-concurrency)
  - spans that follow futures
  - a decorator implementing `Projector`
  - closures returning futures
- **Lesson:** a test caught `FINANCIALS_MCP_LOG=debug` making rmcp log entire plans, so the setting now accepts only a level, and dependencies stay at `warn`.

### Step 17: Dependency checks

[PR #17](https://github.com/zachfire9/financials-mcp/pull/17)

- **Built:** a `cargo deny` policy (advisories, licenses, banned crates, sources), weekly CI, Dependabot, and a checksum-pinned cargo-deny binary.
- **Look at:** [`deny.toml`](../deny.toml), [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), [`.github/dependabot.yml`](../.github/dependabot.yml).
- **Rust:**
  - [`Cargo.lock` and semver ranges](rust-concepts.md#modules-crates-and-cargo)
  - features
  - duplicate versions
  - build scripts
- **Lesson:** the first run found a real vulnerability (RUSTSEC-2026-0592 in `url`), fixed with a lockfile update.

### Step 18: Learning walkthrough

[PR #20](https://github.com/zachfire9/financials-mcp/pull/20)

- **Built:** this walkthrough, the [architecture](architecture.md) overview with an example conversation, the [Rust concepts index](rust-concepts.md), the [tool reference](tools.md), a shorter README, and tests that keep the docs accurate.
- **Look at:** [`docs/`](.), [`tests/docs.rs`](../tests/docs.rs).
- **Lesson:** checking `--help` found the same leak as Step 15. A Rust note was printed in the program's help text, and now a test checks for that too.

## Going online

### Step 19: Streamable HTTP transport

PR_LINK

- **Built:** `financials-mcp serve --http`, the same five tools at `http://127.0.0.1:8787/mcp` over MCP's Streamable HTTP transport, stateless, with graceful shutdown on Ctrl+C. It's an optional `http` Cargo feature, on by default.
- **Look at:** [`src/http.rs`](../src/http.rs), the `on_every_transport!` macro in [`src/server.rs`](../src/server.rs)'s tests, and `serve_http` in [`src/main.rs`](../src/main.rs).
- **Rust:**
  - [Cargo features and `#[cfg]`](rust-concepts.md#modules-crates-and-cargo)
  - [`tower::Service`](rust-concepts.md#traits-and-generics)
  - `#[non_exhaustive]` structs built with builder methods
  - [`macro_rules!`](rust-concepts.md#macros-and-attributes)
  - [`thread_local!`, `RefCell` and `Once`](rust-concepts.md#testing)
- **Lesson:** running the tool tests over both transports exposed a **flaky test from Step 16**. It already failed about half the time on `main` when run with only the server tests. Thread-local log collectors race with `tracing`'s process-wide cache, so the capture now goes through one global subscriber that routes lines to each test's own buffer.

## What's next

Steps 20–22 add a bearer token and request limits, package the server for AWS Lambda, and deploy it with SAM, so Claude can use the tools from any device. See the [plan](plans/financials-mcp-learning-plan.md#step-20--bearer-token-auth-and-request-limits).
