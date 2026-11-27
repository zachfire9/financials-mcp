//! financials-mcp: an MCP server that lets AI agents run retirement
//! projections through financials-api.
//!
//! This file is the root of the **library crate**. Almost all logic lives in
//! the modules below it so it can be tested directly. `src/main.rs` is the
//! **binary crate**: a thin entrypoint that calls into this library. Cargo
//! builds both from the same package, and the binary refers to the library by
//! the package name with `-` replaced by `_`: `financials_mcp`.
//!
//! The modules, roughly in the order a tool call passes through them (see
//! `docs/architecture.md` for the full picture):
//!
//! - [`server`]: the MCP tools; [`guidance`]: what the model is told;
//!   [`logging`]: one log line per tool call, never plan contents.
//! - [`plan`]: the input; [`overrides`]: "what if" changes to a copy of it.
//! - [`compare`], [`sweep`], [`max_spending`], [`withdrawal_order`]: the batch
//!   tools, which run many projections through [`batch`].
//! - [`projector`]: the `Projector` trait (and a fake for tests); [`api`]: the
//!   real one, calling financials-api; [`config`]: its settings.
//! - [`projection`]: the API's answer; [`summary`]: the compact version tools
//!   return.
//! - [`error`]: every failure, with messages that never quote plan values;
//!   [`cli`]: the command line, including the `project` debug command.
//! - `http`: the Streamable HTTP transport for `serve --http` (with the
//!   default `http` feature).

// `pub mod plan;` declares a module and tells the compiler to load it from
// `src/plan.rs`. Unlike Go (every file in a folder is one package) or Python
// (every file is importable), a Rust file is only compiled if a parent module
// declares it. `pub` lets the binary and the integration tests use it, as
// `financials_mcp::plan::Plan`.
pub mod api;
pub mod batch;
pub mod cli;
pub mod compare;
pub mod config;
pub mod error;
pub mod guidance;
// Compiled only when the `http` feature is on (the default). Without it, the
// module, axum and rmcp's HTTP transport aren't built at all.
#[cfg(feature = "http")]
pub mod http;
pub mod logging;
pub mod max_spending;
pub mod overrides;
pub mod plan;
pub mod projection;
pub mod projector;
pub mod server;
pub mod summary;
pub mod sweep;
pub mod withdrawal_order;

// `pub use` re-exports an item so callers can write `financials_mcp::Error`
// instead of `financials_mcp::error::Error`.
pub use error::Error;

/// The package name, read from `Cargo.toml` at compile time.
///
/// `env!` is a macro (the `!` marks a macro call) that Cargo fills in during
/// the build, so the name and version can never drift from the manifest.
const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the one-line description printed when the binary runs.
///
/// It returns an owned `String` (built with `format!`), rather than borrowing
/// text, because the result is assembled at runtime.
pub fn banner() -> String {
    format!(
        "{NAME} {VERSION}: retirement projection tools for AI agents. \
         Run `{NAME} serve` to start the MCP server, or `{NAME} --help`."
    )
}

// Unit tests live next to the code they test, in a `tests` module that is
// only compiled for `cargo test` (`#[cfg(test)]`). `use super::*;` brings the
// parent module's items, including private ones, into scope.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_the_package_and_version() {
        assert!(banner().starts_with("financials-mcp 0.1.0: "));
    }

    #[test]
    fn banner_points_to_serve_and_help() {
        assert_eq!(
            banner(),
            "financials-mcp 0.1.0: retirement projection tools for AI agents. \
             Run `financials-mcp serve` to start the MCP server, or `financials-mcp --help`."
        );
    }
}
