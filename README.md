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

Add `--scope user` to make it available in every project (the default scope is the current project only). Then ask something like *"Run a projection for this plan and tell me whether the money lasts"* and attach or paste a plan JSON.

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
