# Rust concepts

An index of the Rust concepts used in financials-mcp, grouped by topic. Each one points to where it appears in the code and compares it with Go and Python. The code explains each concept in a `//` comment where it first appears, and the [walkthrough](walkthrough.md) shows which step introduced it.

Topics:
- [Modules, crates and Cargo](#modules-crates-and-cargo)
- [Ownership and borrowing](#ownership-and-borrowing)
- [Types: structs, enums, Option and Result](#types-structs-enums-option-and-result)
- [Error handling](#error-handling)
- [Traits and generics](#traits-and-generics)
- [Iterators and closures](#iterators-and-closures)
- [Async and concurrency](#async-and-concurrency)
- [Macros and attributes](#macros-and-attributes)
- [Numbers](#numbers)
- [Testing](#testing)

## Modules, crates and Cargo

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Library and binary crates from one package | [`lib.rs`](../src/lib.rs), [`main.rs`](../src/main.rs) (Step 1) | A thin `cmd/…/main.go` over `internal/` |
| `pub mod x;` declares a module from `x.rs` | [`lib.rs`](../src/lib.rs) | Go compiles every file in a folder, and Python can import any file; Rust compiles only declared modules |
| `pub`, `pub(crate)` and private by default | `Plan::uses_phase_projection` is `pub(crate)` ([`plan.rs`](../src/plan.rs)) | Go's capitalization rule, with one extra level: "visible in this crate only" |
| `use` and re-exports (`pub use`) | `pub use error::Error;` in [`lib.rs`](../src/lib.rs) | Go import aliases; Python `from x import y` in `__init__.py` |
| `Cargo.toml` versions are ranges, `Cargo.lock` is exact | [`Cargo.toml`](../Cargo.toml), [`Cargo.lock`](../Cargo.lock) (Step 17) | `go.mod` plus `go.sum`; a fully pinned `requirements.txt` |
| `rust-version` and the version-aware resolver | `Cargo.toml` (Step 9: Cargo picked rmcp 2.2 for Rust 1.85) | Go's `go` directive; no direct Python equivalent |
| Features (optional parts of a crate) | `tracing-subscriber` with `default-features = false` ([`Cargo.toml`](../Cargo.toml)) | Go build tags, roughly; pip "extras" |
| Your own features and `#[cfg(feature = "…")]` | The `http` feature in [`Cargo.toml`](../Cargo.toml); `#[cfg(feature = "http")] pub mod http;` in [`lib.rs`](../src/lib.rs), and on CLI fields and match arms (Step 19) | Go build tags (`//go:build`); a feature flag, but decided at compile time |
| Several major versions of one crate at once | `syn` 2 and 3 in the tree (Step 17) | Go allows one version per module |
| Build scripts and proc macros run at compile time | `aws-lc-sys` (`build.rs`), `serde_derive` | `go generate`, but automatic |

## Ownership and borrowing

| Concept | Where | Compared with Go / Python |
|---|---|---|
| `String` (owned) versus `&str` (borrowed) | `banner()` in [`lib.rs`](../src/lib.rs) (Step 1) | Both are just `string` / `str` there; no ownership |
| Borrowing parameters: `&Plan`, `&str` | `Plan::from_json(json: &str)` ([`plan.rs`](../src/plan.rs)) | Passing a pointer, but the compiler guarantees it stays valid |
| `&` (shared) versus `&mut` (exclusive) | `apply_overrides(plan: &Plan, …)` can't change the plan; `apply_to(&self, plan: &mut Plan)` can ([`overrides.rs`](../src/overrides.rs), Step 10) | "Don't mutate the input" is a convention there; here it's a compile error |
| `clone()` for an independent copy | `plan.clone()` in `apply_overrides` | `copy.deepcopy`; Go copies structs but shares slices and maps |
| `.as_ref()` to borrow inside an `Option` | `projection.mortgage.as_ref().map(…)` ([`summary.rs`](../src/summary.rs), Step 5) | No equivalent needed with a garbage collector |
| `Path` versus `PathBuf` | `run_project(…, plan_path: &Path, …)` ([`cli.rs`](../src/cli.rs), Step 8) | `&str` versus `String`, for paths |
| Lifetimes (`'a`) on returned references and structs | `find_expense<'p>` ([`overrides.rs`](../src/overrides.rs)); `struct Search<'a, P>` ([`max_spending.rs`](../src/max_spending.rs), Step 13) | Implicit with a garbage collector |
| `'static`: owns its data, or lives forever | `FakeProjector::from_fn(… + 'static)`; spawned tasks ([`batch.rs`](../src/batch.rs)) | Goroutines capture anything, and the GC keeps it alive |
| `move` closures and `async move` blocks | Spawned tasks in [`batch.rs`](../src/batch.rs) (Step 11) | Go closures capture by reference |
| `+ use<>` on `impl Trait` returns | The `env_of` test helper in [`config.rs`](../src/config.rs) (Step 4) | No equivalent |
| RAII: cleanup when a value is dropped | `Mutex` guards, `_permit` in [`batch.rs`](../src/batch.rs), temp files in tests | `defer` or `with`, but written by the compiler |
| Constant-time comparison for secrets | `BearerToken::matches` with `subtle::ConstantTimeEq`, and a hand-written `PartialEq` that uses it ([`config.rs`](../src/config.rs), Step 20) | `hmac.Equal` / `subtle.ConstantTimeCompare` in Go; `hmac.compare_digest` in Python |

## Types: structs, enums, Option and Result

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Structs and `#[derive(Debug, Clone, PartialEq)]` | `Plan` ([`plan.rs`](../src/plan.rs), Step 2) | Go structs; Python dataclasses |
| `#[non_exhaustive]`: built with `Default` and builder methods | `StreamableHttpServerConfig` in `http::config` ([`http.rs`](../src/http.rs), Step 19) | Keyword-only arguments with defaults |
| Tuple structs and the newtype pattern | `AccessToken(String)` ([`config.rs`](../src/config.rs), Step 4) | `type AccessToken string`, but with behavior the compiler enforces |
| Type aliases | `type Order = Vec<u8>` ([`withdrawal_order.rs`](../src/withdrawal_order.rs)) | Go's `type X = Y` (with `=`) |
| `Option<T>` instead of nil | `withdrawal_available_after_year: Option<i32>` (Step 2): `None` means "default", `Some(0)` means "year 1" | `*int` in Go; `Optional[int]` in Python, unchecked |
| Nested `Option` for three states | `Summary::mortgage: Option<MortgageSummary { paid_off_in_year: Option<i32> }>` ([`summary.rs`](../src/summary.rs)) | Hard to express without extra flags |
| Enums without data | `Phase` ([`projection.rs`](../src/projection.rs)) | A Go `const` block of typed strings; Python `Enum` |
| Enums with data | `Error` ([`error.rs`](../src/error.rs)), `Override` ([`overrides.rs`](../src/overrides.rs)) | An interface plus one struct per case and a type switch |
| Exhaustive `match` | `Override::apply_to`, `Error::kind` | Go's `switch` falls through to `default` silently |
| Match guards and `matches!` | `None if plan.people.is_empty() =>` ([`overrides.rs`](../src/overrides.rs)) | An extra `if` inside a `case` |
| `if let`, `while let`, `let … else` | `while let Some(chunk) = …` ([`api.rs`](../src/api.rs)); `let Some(x) = … else { return … }` ([`max_spending.rs`](../src/max_spending.rs)) | `if v, ok := …; ok` in Go |
| Slice patterns | `[one]`, `[rest @ .., last]` in `describe` ([`withdrawal_order.rs`](../src/withdrawal_order.rs), Step 14) | Python's `match` sequence patterns |
| `Option` and `Result` helpers | `.map`, `.ok_or_else`, `.unwrap_or`, `.transpose`, `bool::then_some` ([`summary.rs`](../src/summary.rs), [`api.rs`](../src/api.rs)) | Nil checks written out |

## Error handling

| Concept | Where | Compared with Go / Python |
|---|---|---|
| `Result<T, E>` for anything that can fail | Every fallible function (Step 3) | `(T, error)` in Go, but you can't ignore the error; exceptions in Python |
| `?` returns early on `Err` | `Plan::from_json_checked` ([`plan.rs`](../src/plan.rs)) | `if err != nil { return err }` |
| One error enum with `thiserror` | [`error.rs`](../src/error.rs) | Several error types and `errors.As`; exception subclasses |
| Collecting `Result`s stops at the first error | `.collect::<Result<Vec<_>, _>>()` in `run_sweep` ([`sweep.rs`](../src/sweep.rs), Step 12) | A loop with an early return |
| `map_err` to convert errors | Throughout [`api.rs`](../src/api.rs) | `fmt.Errorf("…: %w", err)` |
| `expect` for "can't happen" cases | `to_json` in [`plan.rs`](../src/plan.rs) | `panic` / `assert` |
| Two kinds of failure in one type | `Result<CallToolResult, ErrorData>` in [`server.rs`](../src/server.rs) (Step 9): `Err` for protocol failures, `is_error` results for bad input | Usually mixed together |

## Traits and generics

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Traits, implemented explicitly | `impl Projector for ApiProjector` ([`api.rs`](../src/api.rs)) | Go interfaces are satisfied implicitly; Python protocols |
| Implementing a standard trait by hand | `impl fmt::Debug for AccessToken` ([`config.rs`](../src/config.rs)) | Implementing `String()` or `__repr__` |
| `tower::Service`: request in, future of a response out | rmcp's `StreamableHttpService`, routed by axum ([`http.rs`](../src/http.rs), Step 19) | Go's `http.Handler`; a WSGI/ASGI app |
| Middleware layers | `axum::middleware::from_fn_with_state(token, require_bearer_token)` and `.layer(…)` in [`http.rs`](../src/http.rs) (Step 20) | `func(http.Handler) http.Handler`; WSGI middleware |
| `-> impl Future + Send` in a trait | `Projector::project` ([`projector.rs`](../src/projector.rs), Step 6) | No equivalent |
| Generics, specialized per type at compile time | `project_all<P: Projector>` ([`batch.rs`](../src/batch.rs)), every tool | Go generics (usually one shared copy); Python type hints (not checked) |
| Trait objects (`Box<dyn Fn …>`) for runtime choice | `ProjectionFn` in [`projector.rs`](../src/projector.rs) | A Go interface value |
| `impl Trait` in arguments and returns | `out: &mut impl Write` ([`cli.rs`](../src/cli.rs)); `-> impl Iterator` ([`sweep.rs`](../src/sweep.rs)) | Accepting an interface; returning a generator |
| A decorator that implements the same trait | `Counted<P>` ([`logging.rs`](../src/logging.rs), Step 16) | Wrapping an interface; a Python decorator class |
| `From` for conversions | `impl From<&Summary> for Headline` ([`summary.rs`](../src/summary.rs)) | A constructor function |
| A hand-written `Clone` to avoid a bound | `impl Clone for FinancialsServer<P>` ([`server.rs`](../src/server.rs)) | No equivalent |
| `where` clauses | `FinancialsServer::call` ([`server.rs`](../src/server.rs)) | Type-parameter constraints |

## Iterators and closures

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Iterator chains, lazy | `summarize` ([`summary.rs`](../src/summary.rs), Step 5) | Python generator expressions; loops in Go |
| `enumerate`, `zip`, `windows(2)` | Breakpoints ([`sweep.rs`](../src/sweep.rs)); rows in [`compare.rs`](../src/compare.rs) | `enumerate`, `zip`, `itertools.pairwise` |
| Ranges `0..n` and `0..=n` | `(0..=MAX_YEARS).contains(&x)` ([`plan.rs`](../src/plan.rs)) | `range()` |
| Closures, `impl Fn` parameters | `minimal_with(change: impl FnOnce(&mut Plan))` in tests; `Config::from_sources` | Go `func` values; Python lambdas |
| Sorting by a key, stable, with `Reverse` | `rank_key` ([`withdrawal_order.rs`](../src/withdrawal_order.rs), Step 14) | `sorted(key=…)` with negated values |
| `HashSet` for uniqueness | Scenario names ([`compare.rs`](../src/compare.rs)), order dedup | Python `set`; `map[T]struct{}` in Go |

## Async and concurrency

| Concept | Where | Compared with Go / Python |
|---|---|---|
| `async fn`, futures and `.await` | [`projector.rs`](../src/projector.rs), [`api.rs`](../src/api.rs) (Step 6) | Python `async`/`await`; Go hides this behind goroutines |
| The tokio runtime, `#[tokio::main]` | [`main.rs`](../src/main.rs) (`current_thread`) | `asyncio.run`; the Go runtime |
| `tokio::spawn`, `JoinSet` | `project_all` ([`batch.rs`](../src/batch.rs), Step 11) | Goroutines with an `errgroup`; `asyncio.as_completed` |
| `Semaphore` for a concurrency limit | `project_all` (4 at a time) | A buffered channel; `asyncio.Semaphore` |
| Graceful shutdown with any future | `http::serve(…, shutdown: impl Future)`; Ctrl+C via `tokio::signal::ctrl_c`, `std::future::pending` in tests ([`http.rs`](../src/http.rs)) | `http.Server.Shutdown(ctx)`; a cancelled `asyncio` task |
| Cancelling by dropping a future | `tokio::time::timeout(TOOL_TIME_LIMIT, …)` in `FinancialsServer::call` ([`server.rs`](../src/server.rs), Step 20): when the timer wins, the work is dropped, which stops it | `context.WithTimeout`, passed all the way down; `asyncio.wait_for` |
| `Arc` for shared ownership | The projector shared across tasks and connections | Automatic with a GC |
| `Mutex` and atomics for shared state | `FakeProjector::received`, `Counted::count` | `sync.Mutex`, `atomic`; `threading.Lock` |
| `Send` and `Sync` | `trait Projector: Send + Sync`; the `+ Send` bounds in `FinancialsServer::call` | Checked by Go's race detector at runtime, if at all |
| `spawn_blocking` for blocking work | The end-to-end test in [`tests/cli.rs`](../tests/cli.rs) (Step 8) | `run_in_executor` |
| `tracing` spans attached to futures | `.instrument(span)` in [`server.rs`](../src/server.rs) and [`batch.rs`](../src/batch.rs) (Step 16) | `contextvars`; `context.Context` |

## Macros and attributes

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Function-like macros (`!`) | `format!`, `env!`, `matches!`, `include_str!` | `fmt.Sprintf`, `//go:embed` |
| `env!` expands where it's written | `get_info` ([`server.rs`](../src/server.rs), Step 9) | `ldflags -X` |
| `concat!` for compile-time strings | `INSTRUCTIONS` ([`guidance.rs`](../src/guidance.rs), Step 15) | Constant string concatenation |
| `#[cfg_attr(feature, doc = "…")]` | Help text for `--http` only in builds that have it ([`cli.rs`](../src/cli.rs), Step 20) | No direct equivalent |
| `macro_rules!`: your own macros | `on_every_transport!` in the [`server.rs`](../src/server.rs) tests: one test body, a test per transport (Step 19) | Table-driven tests or `pytest.mark.parametrize`, but generating real functions |
| `#[derive]` generates trait code | `Serialize`, `Deserialize`, `JsonSchema`, `Parser` | `encoding/json` reflection; dataclasses |
| serde attributes | `rename_all`, `default`, `skip_serializing_if`, `deny_unknown_fields`, `tag = "op"`, `untagged`, `flatten` | Go struct tags |
| Attribute macros that build code | `#[tool_router]`, `#[tool]`, `#[tool_handler]` ([`server.rs`](../src/server.rs)); `#[tokio::main]` | Python decorators, but at compile time |
| `///` doc comments are data | Tool descriptions, schemas and `--help` come from them (Steps 9, 15, 18) | Docstrings |
| `#[cfg(test)]` | Every `mod tests` | `_test.go` files |

## Numbers

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Integer money: `i64` cents, `i32` basis points | Everywhere, starting with [`plan.rs`](../src/plan.rs) | The same discipline, by convention |
| Rounding with integer division | `dollars()` ([`summary.rs`](../src/summary.rs)); `scale_cents` ([`overrides.rs`](../src/overrides.rs)) | Integer division truncates toward zero in Go; Python floors |
| Overflow panics in debug builds and wraps in release | Caught for real in a test fake (Step 13) | Go wraps silently; Python integers don't overflow |
| `checked_*`, `saturating_*`, `i128` | `add_years` ([`overrides.rs`](../src/overrides.rs)), `scale` ([`max_spending.rs`](../src/max_spending.rs)) | `math/bits`; not needed in Python |
| `as` versus `try_from` | `i32::try_from(value)` in [`sweep.rs`](../src/sweep.rs) (Step 12) | Go conversions truncate silently |

## Testing

| Concept | Where | Compared with Go / Python |
|---|---|---|
| Unit tests next to the code | `#[cfg(test)] mod tests` in every module | `_test.go` in the same package |
| Integration tests in `tests/` | [`tests/`](../tests) (public API and the real binary) | An external test package |
| `#[tokio::test]` | Async tests throughout | `pytest-asyncio` |
| Paused time: `#[tokio::test(start_paused = true)]` | The 25-second limit test in [`server.rs`](../src/server.rs) runs instantly (Step 20) | A fake clock (`freezegun`, injected `time.Now`) |
| Fakes behind a trait | `FakeProjector` ([`projector.rs`](../src/projector.rs)) | Interface fakes; `unittest.mock` |
| A mock HTTP server | wiremock in [`api.rs`](../src/api.rs) | `httptest`; `responses` |
| In-memory pipes for protocol tests | `tokio::io::duplex` in [`server.rs`](../src/server.rs) | `net.Pipe` |
| Capturing logs | `logging::capture` ([`logging.rs`](../src/logging.rs)) | `caplog` |
| `thread_local!`, `RefCell` and `Once` | `logging::capture` routes one global subscriber's lines to each test's buffer ([`logging.rs`](../src/logging.rs), Step 19) | `threading.local()`; `sync.Once` |
| `include_str!` for fixtures | `const PLAN: &str = include_str!(…)` | `//go:embed` |
