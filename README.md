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

Steps 1–22 are complete: five tools over stdio for Claude Desktop and Claude Code, over HTTP (`serve --http`) with a required bearer token, an allowed-hosts list and request limits, and the same HTTP server on AWS Lambda, deployed with SAM from GitHub Actions so Claude can use it from any device.

| Tool | Answers |
|---|---|
| `run_projection` | "Does this plan last? What if we retire earlier?" |
| `compare_scenarios` | "Compare retiring two years earlier against spending 10% less." |
| `sweep` | "How much later would we need to retire for the money to last?" |
| `find_max_spending` | "What's the most we can spend and still have $200k left?" |
| `withdrawal_order_options` | "Which account should we draw from first?" |

**Next:** the planned steps are done. Follow-ups, such as OAuth for Claude connectors and stress testing, are listed under [Additional functionality](docs/plans/financials-mcp-learning-plan.md#additional-functionality).

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
| `MCP_BEARER_TOKEN` | *(none)* | Required for `serve --http`: the secret clients send as `Authorization: Bearer <token>`. At least 32 printable characters, no spaces. Never printed. |
| `MCP_ALLOWED_HOSTS` | loopback | For `serve --http`: comma-separated hostnames (or `host:port`) accepted in the `Host` header, such as the API's domain once hosted. No `https://` or path. |

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

`financials-mcp serve --http` serves the same tools at `http://127.0.0.1:8787/mcp` using MCP's Streamable HTTP transport, in stateless mode (no sessions; each request stands alone). It's locked:

- **A bearer token is required.** The server won't start without `MCP_BEARER_TOKEN` (at least 32 characters), and every request must send `Authorization: Bearer <token>`. Anything else gets `401`. The comparison runs in constant time, and the token never appears in logs.
- **Only allowed hosts.** Requests must be addressed to a host in `MCP_ALLOWED_HOSTS`, by default `localhost`, `127.0.0.1` or `::1`. Other `Host` headers get `403`, which blocks DNS-rebinding attacks from web pages.
- **Limits.** Request bodies over 1 MiB get `413`, and any tool call running past 25 seconds is stopped with a tool error (on stdio too).

Generate a token once and keep it somewhere safe, such as your `.env`. Either command prints a random 44-character token:

```powershell
# PowerShell 7
[Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
# or, with OpenSSL (included with Git for Windows)
openssl rand -base64 32
```

Then start the server and connect Claude Code with the same token:

```powershell
$env:FINANCIALS_API_URL="http://localhost:8080"
$env:MCP_BEARER_TOKEN="<your token>"
.\target\release\financials-mcp.exe serve --http                         # or --listen 127.0.0.1:9000
claude mcp add --transport http financials-http http://127.0.0.1:8787/mcp --header "Authorization: Bearer <your token>"
```

Ctrl+C stops it after requests in progress finish. Steps 21–22 host the same server on AWS Lambda.

### On AWS Lambda

The `lambda` binary (`src/bin/lambda.rs`, behind the `lambda` Cargo feature) runs the same locked HTTP server on AWS Lambda. [`lambda_http`](https://crates.io/crates/lambda_http) turns each API Gateway HTTP API event into an ordinary HTTP request for it. The tools, token check, allowed hosts and limits are identical to `serve --http`. [Deploying to AWS](#deploying-to-aws) covers putting it online. To build it yourself, install [cargo-lambda](https://www.cargo-lambda.info) (`pip install cargo-lambda`, which includes the Zig toolchain it cross-compiles with), then:

```powershell
cargo lambda build --release --arm64 --features lambda --bin lambda --output-format zip
# -> target/lambda/lambda/bootstrap.zip, for the provided.al2023 runtime on arm64 (Graviton)
```

To try it locally, run it under cargo-lambda's emulator and call it like a Lambda function URL. Put the settings in an env file, with values in quotes:

```powershell
cargo lambda watch --features lambda --bin lambda --env-file lambda.env   # MCP_BEARER_TOKEN, FINANCIALS_API_URL, MCP_ALLOWED_HOSTS="localhost:9000"
# then POST MCP requests to http://localhost:9000/lambda-url/lambda/mcp with the bearer token
```

The Lambda reads the same settings as `serve --http` from its environment variables. Set `MCP_ALLOWED_HOSTS` to the API's domain (the deployed template does this itself). CI builds the arm64 binary on every push and checks that it needs no newer glibc than Lambda's runtime has (2.34).

### From anywhere, once deployed

After [deploying](#deploying-to-aws), the server is at `https://<api-id>.execute-api.<region>.amazonaws.com/mcp` (the stack's `McpUrl` output) and needs the bearer token. financials-api must be deployed too, since Lambda can't reach `localhost`.

**Claude Code:** user scope keeps the token in your own `~/.claude.json`. Don't use `--scope project`, which writes it to `.mcp.json` in the repo.

```powershell
claude mcp add --scope user --transport http financials <McpUrl> --header "Authorization: Bearer <token>"
claude mcp list   # financials: ... - ✓ Connected
```

**Claude (web, desktop and mobile):** a custom connector works on every device once it's added, because Claude connects to it from Anthropic's cloud.

1. In Settings → Connectors, choose Add custom connector (Add → Custom → Web).
2. Name it `financials` and enter the `McpUrl`.
3. Choose **No sign-in**.
4. Under **Request headers**, add `Authorization` with the value `Bearer <token>`.

Request-header authentication for custom connectors is [in beta and not available to every account yet](https://claude.com/docs/connectors/custom/add-unlisted). If there's no Request headers section, use Claude Code. OAuth, the other way in, is listed under [Additional functionality](docs/plans/financials-mcp-learning-plan.md#oauth-for-claude-custom-connectors).

Then attach or paste a plan (the financials-api `POST /projections` request body) and ask, for example:

- *"Run a projection for this plan and tell me whether the money lasts."*
- *"What if I retired 2 years earlier?"*
- *"Compare retiring 2 years earlier against returns 1% lower."*
- *"How much later would we need to retire for the money to last?"*
- *"What's the most we can spend and still have $200k left?"*
- *"Which account should we draw from first?"*

The model is told to ask what you're optimizing for, to use the tools for every number, and to state the assumptions and limitations (fixed returns each year, no taxes). See [what happens in a conversation](docs/architecture.md#how-an-agent-uses-the-tools).

## Deploying to AWS

`template.yaml` is a SAM template (CloudFormation with a serverless shorthand). It describes everything the deployment needs:

```text
client --HTTPS + bearer token--> HTTP API: ANY /mcp, throttled --> Lambda (arm64, bootstrap)
Lambda --HTTPS--> financials-api POST /projections
```

- **The function** runs the `bootstrap` binary from `cargo lambda build`, with 128 MB and a 29-second timeout. Its settings are template parameters. The tokens are `NoEcho`, so CloudFormation never displays them. `MCP_ALLOWED_HOSTS` is set to the API's own domain automatically.
- **The HTTP API** routes only `/mcp` to the function, throttled to 2 requests a second with bursts of 5 by default. Other paths, and throttled requests, never reach Lambda.
- **A log group** keeps the function's logs for 14 days.
- **Optional reserved concurrency** caps how many copies of the function can run at once, as a cost backstop.

The [`Deploy` workflow](.github/workflows/deploy.yml) runs by hand. It runs all of CI first, then deploys the exact `bootstrap.zip` CI built and checked, using short-lived AWS credentials from GitHub's OIDC login. No AWS keys are stored. Finally, it smoke-tests the endpoint: `401` without the token, five tools with it.

### One-time setup

1. **financials-api is deployed**, and you have its URL (its stack's `FinancialsApiUrl` output) and access token, if it uses one.
2. **The deploy role exists in AWS.** Follow [infra/iam/README.md](infra/iam/README.md).
3. **Repository settings** (GitHub → Settings → Secrets and variables → Actions). This repo's Actions logs are public, so anything that identifies the deployment is a *secret*, which shows as `***` in logs:

   | Name | Kind | Value |
   |---|---|---|
   | `AWS_DEPLOY_ROLE_ARN` | secret | The deploy role's ARN (it contains the account id) |
   | `MCP_BEARER_TOKEN` | secret | A new token for clients ([generate one](#over-http-on-this-machine)) |
   | `FINANCIALS_API_URL` | secret | financials-api's base URL, `https://…`, without `/projections` |
   | `FINANCIALS_ACCESS_TOKEN` | secret, optional | financials-api's access token, if it requires one |
   | `AWS_REGION` | variable, optional | Default `us-east-1` (the IAM policies assume it) |
   | `MCP_STACK_NAME` | variable, optional | Default `financials-mcp` (the IAM policies assume it) |
   | `MCP_THROTTLING_RATE_LIMIT`, `MCP_THROTTLING_BURST_LIMIT` | variables, optional | Defaults `2` and `5` |
   | `MCP_RESERVED_CONCURRENCY` | variable, optional | Leave unset on accounts with a low concurrency quota |

### Deploying

Actions → Deploy → Run workflow, on `main`. Then get the endpoint for your clients:

```powershell
aws cloudformation describe-stacks --stack-name financials-mcp --query "Stacks[0].Outputs" --output table   # McpUrl, McpLogGroupName
```

The workflow masks the URL in its log on every deploy after the first. The first deploy's log shows the API id once. Delete that run's logs (the run's ⋯ menu → Delete all logs) if you'd rather it not stay public. The endpoint needs the token either way.

To check the logs after a conversation, run `aws logs tail <McpLogGroupName> --since 15m`. You should see one line per tool call, as in [Logs](#logs), plus Lambda's own `START`/`END`/`REPORT` lines, and no plan contents.

### financials-api's throttling

financials-api's template also throttles, by default to 2 requests a second with bursts of 5. One MCP tool call can send up to 25 projections, 4 at a time. When financials-api answers `429`, the server waits and retries up to 4 times (3.75 seconds of pauses at most) before reporting "financials-api is throttling requests". For sweeps or withdrawal-order comparisons, raise financials-api's `FINANCIALS_THROTTLING_RATE_LIMIT` and `FINANCIALS_THROTTLING_BURST_LIMIT` repository variables (for example to 5 and 10) and redeploy it.

### Rotating the token

1. Generate a new token, and update the `MCP_BEARER_TOKEN` secret.
2. Run the Deploy workflow. The old token stops working as soon as the deploy finishes.
3. Update the header in Claude Code (`claude mcp remove financials --scope user`, then add it again) and in the Claude connector.

The tokens live in the function's environment variables. AWS encrypts those at rest, but anyone with Lambda read access in the account can see them, so keep that access to yourself.

### Cost and tearing down

At personal use, Lambda, API Gateway and CloudWatch fall within or near the AWS free tier. Throttling (and reserved concurrency, if set) caps how much a leaked URL plus token could run up. To remove everything, run `aws cloudformation delete-stack --stack-name financials-mcp`, then delete the deploy role.

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
cargo test --all-features                   # the same, with the Lambda binary's code too
cargo fmt --check                           # formatting
cargo clippy --all-targets -- -D warnings   # lints, warnings fail
cargo deny check                            # dependency policy (deny.toml)
```

CI runs fmt, clippy and the tests on every push and pull request, on Linux and Windows, with default features, none and all, plus `cargo deny check`, an arm64 Lambda build and `sam validate --lint` on `template.yaml`, on Linux. It also runs weekly, so a newly published advisory shows up even when nothing changed. Install cargo-deny with `cargo install cargo-deny --locked`.

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
    lambda.rs           # the same server on AWS Lambda (the `lambda` feature)
    bin/
      lambda.rs         # the Lambda executable (`bootstrap` once built)
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
  template.yaml         # SAM template: the Lambda, its HTTP API and log group
  infra/iam/            # the GitHub deploy role's policies, with placeholders
  .github/workflows/    # ci.yml (every push) and deploy.yml (run by hand)
  .github/requirements/ # hash-pinned cargo-lambda for the CI Lambda build
  Cargo.toml / Cargo.lock
```

## Public repo rules

This repository is public. Real plans, backups and other financial data, tokens, and machine-specific values are never committed. Test fixtures are made up. See [AGENTS.md](AGENTS.md).
