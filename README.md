# financials-mcp

An [MCP](https://modelcontextprotocol.io) server, written in Rust, that lets AI agents run retirement projections through [financials-api](https://github.com/zachfire9/financials-api) and explain the trade-offs. Ask Claude "can we retire two years earlier?" or "what's the most we can spend and still have $200k left?", and it answers with numbers from the financials-api engine rather than estimates. The model chooses which scenarios to try; the engine does all the math.

It's stateless and read-only: the plan travels with every tool call, and nothing is stored.

This is also a **learning project**, built one reviewable step at a time as a first Rust codebase. Each step taught one Rust or MCP concept and landed as its own pull request. Start with the [walkthrough](docs/walkthrough.md) to follow along.

## Documentation

| Document | What's in it |
|---|---|
| [Walkthrough](docs/walkthrough.md) | Each step: what it built, where to look in the code, what it taught, and what went wrong along the way |
| [Architecture](docs/architecture.md) | How a tool call flows through the modules, the design decisions, how an agent uses the tools (with an example conversation), and the test strategy |
| [Rust concepts](docs/rust-concepts.md) | An index of Rust concepts, each linked to where it appears in this code, compared with Go and Python |
| [Tool reference](docs/tools.md) | The five tools, the override operations, and what the model is told |
| [Learning plan](docs/plans/financials-mcp-learning-plan.md) | The step-by-step plan, including Steps 19–22 (hosting on AWS Lambda) |

## Status

Steps 1–19 are complete: five tools over stdio for Claude Desktop and Claude Code, and over HTTP on this machine (`serve --http`).

| Tool | Answers |
|---|---|
| `run_projection` | "Does this plan last? What if we retire earlier?" |
| `compare_scenarios` | "Compare retiring two years earlier against spending 10% less." |
| `sweep` | "How much later would we need to retire for the money to last?" |
| `find_max_spending` | "What's the most we can spend and still have $200k left?" |
| `withdrawal_order_options` | "Which account should we draw from first?" |

**Next:** Steps 20–22 add a bearer token and limits, then host the HTTP server on AWS Lambda, so Claude can use the tools from any device. See the [plan](docs/plans/financials-mcp-learning-plan.md#step-20--bearer-token-auth-and-request-limits).

## Requirements

- Rust stable, 1.88 or newer (the MCP SDK's minimum), installed with [rustup](https://rustup.rs).
- A running [financials-api](https://github.com/zachfire9/financials-api), locally in `ephemeral` mode or deployed.

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
| `FINANCIALS_MCP_LOG` | `info` | Log level for `serve`: `error`, `warn`, `info`, `debug`, `trace` or `off`. Environment only (not `.env`). See [Logs](#logs). |

A variable set in the environment wins over `.env`, even when it's empty (empty means "use the default").

## Try it against financials-api

Start financials-api locally in `ephemeral` mode (from the financials-api repo), then run a made-up plan:

```powershell
# in financials-api
$env:FINANCIALS_STORAGE_DRIVER="ephemeral"; go run ./cmd/api

# in financials-mcp
cargo run -- project tests/fixtures/plan-household.json
```

`project` is a debug command: it prints the plan's summary as JSON on stdout and exits with 0. Problems go to stderr with exit code 1 (or 2 for a usage mistake).

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

Add `--scope user` to make it available in every project (the default scope is the current project only).

### Over HTTP, on this machine

`financials-mcp serve --http` serves the same tools at `http://127.0.0.1:8787/mcp` using MCP's Streamable HTTP transport, in stateless mode (no sessions; each request stands alone). Start it yourself, with settings in the environment or a `.env` in the folder you run it from:

```powershell
$env:FINANCIALS_API_URL="http://localhost:8080"
.\target\release\financials-mcp.exe serve --http                         # or --listen 127.0.0.1:9000
claude mcp add --transport http financials-http http://127.0.0.1:8787/mcp
```

It accepts requests addressed to `localhost`, `127.0.0.1` or `::1` only (other `Host` headers get `403`, which blocks DNS-rebinding attacks from web pages), and there's no authentication yet, so keep it on loopback. Step 20 adds a bearer token before it's ever hosted. Ctrl+C stops it after requests in progress finish.

Then attach or paste a plan (the financials-api `POST /projections` request body) and ask, for example:

- *"Run a projection for this plan and tell me whether the money lasts."*
- *"What if I retired 2 years earlier?"*
- *"Compare retiring 2 years earlier against returns 1% lower."*
- *"How much later would we need to retire for the money to last?"*
- *"What's the most we can spend and still have $200k left?"*
- *"Which account should we draw from first?"*

The model is told to ask what you're optimizing for, to use the tools for every number, and to state the assumptions and limitations (fixed returns each year, no taxes). See [what happens in a conversation](docs/architecture.md#how-an-agent-uses-the-tools).

## Logs

`financials-mcp serve` logs to **stderr**, since stdout carries the MCP protocol. Claude Desktop and Claude Code save a server's stderr in their MCP logs. At the default `info` level, each tool call gets one line:

```text
2026-11-22T21:08:24Z  INFO tool_call{tool="find_max_spending"}: tool call succeeded duration_ms=26 projections=12
2026-11-22T21:08:24Z  INFO tool_call{tool="sweep"}: tool call failed duration_ms=0 projections=0 error="sweep_shape"
```

`debug` adds a line per projection, with its duration and outcome. Logs never include amounts, names, ids, error messages, the API URL or the token. Only the error's *kind* is recorded.

`FINANCIALS_MCP_LOG` accepts **only a level**, and it applies to this program alone. Other libraries always stay at `warn`, because at `debug` the MCP library logs every request in full, plans included. For the same reason, `RUST_LOG` is ignored, and a value like `debug,rmcp=debug` is rejected with a warning.

## Commands

```powershell
cargo run                                   # build and run the binary (prints a banner)
cargo test                                  # unit + integration tests
cargo test --no-default-features            # the same, built without the HTTP transport
cargo fmt --check                           # formatting
cargo clippy --all-targets -- -D warnings   # lints, warnings fail
cargo deny check                            # dependency policy (deny.toml)
```

CI runs fmt, clippy and the tests on every push and pull request, on Linux and Windows, with and without default features, plus `cargo deny check` on Linux. It also runs weekly, so a newly published advisory shows up even when nothing changed. Install cargo-deny with `cargo install cargo-deny --locked`.

## Dependencies

[`deny.toml`](deny.toml) is the policy every dependency must meet. CI enforces it with `cargo deny check`:

- **Advisories:** no known vulnerabilities or yanked versions (from the [RustSec](https://rustsec.org) database), and no unmaintained direct dependencies. An ignored advisory must name its id, a reason and a revisit date.
- **Licenses:** permissive licenses only (MIT, Apache-2.0, BSD-3-Clause, ISC and a few more). A license on the list that nothing uses also fails, so the list stays accurate.
- **Bans:** no native OpenSSL or native-tls, since TLS is rustls. Duplicate versions are warnings.
- **Sources:** crates.io only, with no git dependencies or other registries.

`Cargo.lock` is committed and CI builds with `--locked`, so every build uses exactly the versions that were reviewed. Dependabot opens weekly PRs: crate patch and minor updates grouped into one, major versions separately, and pinned GitHub Actions SHAs. The cargo-deny binary in CI is pinned by version and SHA-256 in `.github/workflows/ci.yml`. Update both values together from the release's `.sha256` file.

## Project layout

```text
financials-mcp/
  src/
    main.rs             # the executable: parse the command line, run a command
    lib.rs              # the library crate root; lists the modules
    server.rs           # the MCP server: five tools, results, errors as tool results
    guidance.rs         # instructions sent to the model
    http.rs             # `serve --http`: the Streamable HTTP transport (the `http` feature)
    logging.rs          # stderr logging, a projection counter, no plan contents
    plan.rs             # the plan (API request) types, parsing and pre-checks
    overrides.rs        # "what if" changes applied to a copy of a plan
    compare.rs          # compare_scenarios
    sweep.rs            # sweep and breakpoints
    max_spending.rs     # find_max_spending search
    withdrawal_order.rs # withdrawal_order_options
    batch.rs            # run many projections, at most 4 at once
    projector.rs        # the Projector trait and a fake for tests
    api.rs              # the financials-api client (the real Projector)
    config.rs           # settings from the environment and .env
    projection.rs       # the API response types
    summary.rs          # projection -> compact summary
    error.rs            # every error, with value-free messages
    cli.rs              # the `project` debug command
  tests/
    cli.rs              # runs the built binary: banner, help, project command
    serve.rs            # runs `serve`: raw MCP over stdio, and over HTTP with --http
    docs.rs             # checks the docs' links and that they match the app
    plan_fixtures.rs    # the fixture plans load and pass the checks
    summary_fixtures.rs # the fixture responses summarize as expected
    fixtures/           # made-up plans and real API responses for them
  docs/                 # walkthrough, architecture, Rust concepts, tools, plan
  deny.toml             # dependency policy
  Cargo.toml / Cargo.lock
```

## Public repo rules

This repository is public. Real plans, backups and other financial data, tokens, and machine-specific values are never committed. Test fixtures are made up. See [AGENTS.md](AGENTS.md).
