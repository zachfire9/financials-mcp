# financials-mcp

An [MCP](https://modelcontextprotocol.io) server, written in Rust, that lets AI agents run retirement projections through [financials-api](https://github.com/zachfire9/financials-api) and explain the trade-offs. The model chooses which scenarios to try; the financials-api engine does all the math.

This is a learning project, built one reviewable step at a time. Each step teaches one Rust or MCP concept, includes deterministic tests, and lands through its own pull request. See the [learning plan](docs/plans/financials-mcp-learning-plan.md).

## Current status

- **Step 01 (Rust project foundation):** a Cargo package with a thin binary (`src/main.rs`) over a library crate (`src/lib.rs`), unit and integration tests, and CI on Linux and Windows.
- **Step 02 (plan model):** `Plan` and friends mirror the financials-api projection request with serde.
- **Step 03 (errors and pre-checks):** an `Error` enum whose messages name fields but never quote values, plus quick pre-checks in the API's own wording.
- **Step 04 (configuration):** `Config` loads the API URL and access token from the environment and `.env`, and never prints the token.
- **Steps 05–07:** projection summaries, the `Projector` trait with a fake for tests, and the real financials-api client.
- **Step 08 (debug command):** `financials-mcp project <plan.json>` runs a plan through financials-api and prints the summary.
- **Step 09 (MCP server):** `financials-mcp serve` speaks MCP over stdio with one tool, `run_projection`.
- **Step 10 (overrides):** `run_projection` takes optional `overrides` (retire earlier, spend less, lower returns, add an expense, and more) applied to a copy of the plan. See [Overrides](#overrides).
- **Step 11 (compare scenarios):** a `compare_scenarios` tool runs a baseline plus up to 12 named scenarios concurrently (at most 4 projections at once) and reports each one's change from the baseline. See [Comparing scenarios](#comparing-scenarios).
- **Step 12 (sweep):** a `sweep` tool varies one input across up to 25 values and reports the **breakpoints** where the plan stops (or starts) lasting. See [Sweeps](#sweeps).
- **Step 13 (max spending):** a `find_max_spending` tool searches for the most a plan can spend each year while keeping an optional minimum balance. See [Maximum spending](#maximum-spending).

## Requirements

- Rust stable, 1.88 or newer (the MCP SDK's minimum), installed with [rustup](https://rustup.rs).

On Windows:

```powershell
winget install Rustlang.Rustup
# open a new terminal, then:
rustc --version
```

## Configuration

Settings come from the process environment, then a local `.env` file (gitignored), then defaults. Copy `.env.example` to `.env` to start.

| Variable | Default | Meaning |
|---|---|---|
| `FINANCIALS_API_URL` | `http://localhost:8080` | Base URL of financials-api. `http://` is only allowed for `localhost`; other hosts need `https://`. |
| `FINANCIALS_ACCESS_TOKEN` | *(none)* | Sent as `X-Financials-Access-Token` when set. Never printed. |

A variable set in the environment wins over `.env`, even when it's empty (empty means "use the default").

## Try it against financials-api

Start financials-api locally in `ephemeral` mode (from the financials-api repo), then run a plan:

```powershell
# in financials-api
$env:FINANCIALS_STORAGE_DRIVER="ephemeral"; go run ./cmd/api

# in financials-mcp
cargo run -- project tests/fixtures/plan-household.json
```

It prints the summary as JSON on stdout and exits with 0. Problems go to stderr with exit code 1 (or 2 for a usage mistake).

## Connect an MCP client

Build the binary once, then point a client at it. The client starts `financials-mcp serve` itself, from its own working directory, so put settings in the client config (a `.env` in this repo won't be found). Start financials-api first.

```powershell
cargo build --release      # -> target\release\financials-mcp.exe
```

**Claude Desktop:** add an entry to `%APPDATA%\Claude\claude_desktop_config.json` (Settings → Developer → Edit Config), then restart Claude Desktop:

```json
{
  "mcpServers": {
    "financials": {
      "command": "C:\\path\\to\\financials-mcp\\target\\release\\financials-mcp.exe",
      "args": ["serve"],
      "env": { "FINANCIALS_API_URL": "http://localhost:8080" }
    }
  }
}
```

**Claude Code:** options go before the server name, and `--` separates them from the command:

```powershell
claude mcp add --transport stdio --env FINANCIALS_API_URL=http://localhost:8080 financials -- C:\path\to\financials-mcp\target\release\financials-mcp.exe serve
```

Add `--scope user` to make it available in every project (the default scope is the current project only). Then ask something like *"Run a projection for this plan and tell me whether the money lasts"* and attach or paste a plan JSON. Then follow up with *"What if I retired 2 years earlier?"* or *"Compare retiring 2 years earlier against returns 1% lower,"* or *"How much later would we need to retire for the money to last?"* or *"What's the most we can spend and still have $200k left?"*

## Overrides

`run_projection` accepts an optional `overrides` list: small, named changes applied in order to a copy of the plan, so a "what if" never needs a rewritten plan.

```json
{
  "plan": { "...": "..." },
  "overrides": [
    { "op": "shift_retirement", "years": -2 },
    { "op": "adjust_returns", "basisPoints": -100 }
  ]
}
```

| `op` | Fields | Effect |
|---|---|---|
| `shift_retirement` | `years`, optional `personId` | Retire later (+) or earlier (−), everyone or one person. The plan's final year stays fixed, so drawdown absorbs the change. |
| `set_drawdown_years` | `years` | Sets the drawdown length (moves the final year). |
| `set_annual_withdrawal` | `annualWithdrawalCents` | Plans without itemized expenses only. |
| `scale_expenses` | `percent` | Scales all drawdown spending (itemized expenses or the annual withdrawal, plus the gap withdrawal) to `percent`; 90 = 10% less. Not the mortgage. |
| `add_expense` | `expense` | Adds an itemized expense (same shape as the plan's). |
| `remove_expense` | `expenseId` | Removes an itemized expense. |
| `set_expense` | `expenseId` plus any of `annualAmountCents`, `inflationRateBasisPoints`, `fromYear`, `toYear` | Changes one expense. |
| `adjust_returns` | `basisPoints`, optional `drawdownOnly` | Adds to every account's return (−100 = one point lower). |
| `set_item` | `itemId` plus any of `annualContributionCents`, `annualReturnRateBasisPoints`, `drawdownAnnualReturnRateBasisPoints`, `withdrawalPriority` | Changes one account. |
| `set_inflation` | `basisPoints` | Sets the withdrawal inflation rate. |

The result's `assumptions` describe the changed plan and echo the overrides. Errors name the override at fault, such as `overrides[1] (shift_retirement): no person with id ...`, and never quote amounts. At most 20 overrides per call.

## Comparing scenarios

`compare_scenarios` takes the baseline plan and up to 12 named scenarios, each a list of [overrides](#overrides):

```json
{
  "plan": { "...": "..." },
  "scenarios": [
    { "name": "Retire 2 years later", "overrides": [{ "op": "shift_retirement", "years": 2 }] },
    { "name": "Spend 15% less", "overrides": [{ "op": "scale_expenses", "percent": 85 }] }
  ]
}
```

It returns the baseline's summary and assumptions, then one row per scenario in request order: its summary plus `changeFromBaseline` (scenario minus baseline: `depletionYear`, `yearsShort`, `endingBalance`, `lowestBalance`, `totalUnfunded`; positive means later, more or bigger). A scenario whose overrides don't apply, or whose projection fails, gets an `error` row instead and doesn't affect the others. A bad base plan or a failed baseline fails the whole call.

Scenario names must be unique and at most 80 characters. Projections run at most 4 at a time, to be polite to financials-api.

## Sweeps

`sweep` varies one input across 1 to 25 strictly ascending values, in that input's units:

| `parameter` | Values |
|---|---|
| `annual_withdrawal` | First-year withdrawal in cents (plans without itemized expenses) |
| `retirement_shift` | Years to retire later (+) or earlier (−), for everyone; the final year stays fixed |
| `return_adjustment` | Basis points added to every account's return |
| `expense_scale` | Drawdown spending as a percent of the plan's (90 = 10% less) |

```json
{ "plan": { "...": "..." }, "parameter": "retirement_shift", "values": [-4, -2, 0, 1, 2, 4] }
```

It returns one compact point per value (`lastsThroughFinalYear`, `depletionYear`, `yearsShort`, `endingBalance`, `lowestBalance`, `totalUnfunded`) and the `breakpoints`: each neighboring pair of values where the outcome changes, as `{ "lastsAt": 1, "shortAt": 0 }`. The real threshold lies between the two values, and a finer sweep narrows it down. Every value must be valid and every projection must succeed, since a series with gaps could hide a breakpoint.

## Maximum spending

`find_max_spending` answers "what's the most we can spend each year?" by searching rather than guessing. financials-api always gives the same projection for the same plan, so the server can run projections until it finds the answer:

```json
{ "plan": { "...": "..." }, "constraints": { "minBalanceCents": 20000000, "throughYear": 40 } }
```

- **Spending** is `annualWithdrawalCents`, or for itemized plans the sum of the expenses, all scaled by the same factor. The gap-year withdrawal and the mortgage stay as they are.
- **Constraints** (both optional): no unfunded years, and a balance of at least `minBalanceCents` (default 0) in every year from the first retirement through `throughYear` (default the final year). Saving years don't count, since a plan can start below the buffer and grow past it. Years after `throughYear` aren't checked, so the money may still run out later.
- **Search:** check the least spending allowed (the API requires at least 1 cent of withdrawal), then double from the current spending until a projection fails, then binary search to within $100. At most 25 projections.
- **Result:** `outcome` (`found`, `fails_at_zero` or `no_upper_limit`), `annualSpendingCents` (ready for a plan or the `set_annual_withdrawal` override) and `annualSpending` in dollars, `precision`, `projectionsUsed`, the summary at that amount and, for itemized plans, `scaleBasisPoints` and each scaled expense.

## Commands

```powershell
cargo run                                   # build and run the binary
cargo test                                  # unit + integration tests
cargo fmt --check                           # formatting
cargo clippy --all-targets -- -D warnings   # lints, warnings fail
```

CI runs the last three on every push and pull request, on Linux and Windows.

## Project layout

```text
financials-mcp/
  src/
    main.rs      # the executable: thin, calls into the library
    lib.rs       # the library crate: logic and unit tests
  tests/
    cli.rs       # integration tests: run the built binary like a user
  docs/plans/    # the learning plan
  Cargo.toml     # package manifest (name, version, edition, dependencies)
  Cargo.lock     # exact dependency versions (committed, as for any application)
```

The layout grows step by step; the intended direction is in the plan's "Proposed project shape".

## Public repo rules

This repository is public. Real plans, backups and other financial data, tokens, and machine-specific values are never committed. Test fixtures are made up. See [AGENTS.md](AGENTS.md).
