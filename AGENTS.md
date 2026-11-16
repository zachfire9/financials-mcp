# AGENTS.md

Guidance for coding agents working in this repo. [README.md](README.md) has setup details, and the [learning plan](docs/plans/financials-mcp-learning-plan.md) is the source of truth for what gets built and in what order.

## What this is

A Rust MCP server for a personal retirement planner. Agents call its tools with a **plan** (the financials-api `POST /projections` request body); the server runs one or many projections through [financials-api](https://github.com/zachfire9/financials-api) and returns compact summaries. It is stateless and read-only: nothing is stored, and every tool is a pure computation.

It is also a **learning project**: each step on the plan teaches one Rust or MCP concept. Prefer the simplest Rust that works, keep each change to its step's scope, and explain any advanced feature (traits, generics, async, lifetimes) in the PR where it first appears.

## Commands

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                    # run before every commit
cargo run
```

## Working agreement

- One branch per step: `step-NN-short-description`. PR title: `Step N: Short step description`.
- Each PR explains the concept taught, what changed, the tests added, and how it was verified.
- On the step branch, mark the step completed in the plan and add the PR link.
- Zach reviews and merges each PR before the next step starts. Don't push to `main` directly.

## Conventions

- **Money is integer cents (`i64`); rates are integer basis points (`i32`)**, exactly as in financials-api (700 = 7%). Never use floats for amounts.
- **financials-api is the source of truth for the math.** Never reimplement the projection engine here.
- **Tests:** unit tests sit next to the code in `#[cfg(test)] mod tests`; integration tests live in `tests/`. Tests never call the real financials-api or an LLM: use a fake projector or a mock HTTP server. New behavior needs a test; bug fixes need a test that fails without the fix.
- **Formatting and lints:** `cargo fmt`; `cargo clippy` with warnings as errors. Line endings are LF (`.gitattributes`).
- **Commit messages** use conventional prefixes: `feat:`, `fix:`, `chore:`, `docs:`, `test:`.

## Security and privacy

- **No data at rest:** don't add storage, caches or files that hold plans.
- **No plan contents in logs or errors:** log tool names, timings, counts and error kinds only. Never amounts, names or tokens.
- **stdout belongs to the MCP protocol** once the stdio server exists (Step 09); logs go to stderr.
- Secrets (`FINANCIALS_ACCESS_TOKEN`, later `MCP_BEARER_TOKEN`) come from the environment or a local `.env`, are never printed (redact them in `Debug`), and are never committed.
- Ask for the narrowest access and dependencies a step needs; new dependencies should be justified in the PR.

## Public repo rules

This repo is public. Never commit:

- Real plans, backups or any financial data. Fixtures in `tests/fixtures/` are made up.
- Secrets or tokens (`.env` is gitignored; `.env.example` holds placeholders only).
- AWS account IDs, real API URLs or IDs, LAN IPs, hostnames or machine names. Use placeholders like `<api-url>`.

When staging, add specific paths rather than `git add -A`, so untracked local files (exported plans, notes) can't slip in.
