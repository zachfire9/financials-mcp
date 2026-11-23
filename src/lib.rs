//! financials-mcp: an MCP server that lets AI agents run retirement
//! projections through financials-api.
//!
//! This file is the root of the **library crate**. Almost all logic lives
//! here (and, later, in modules below it) so it can be tested directly.
//! `src/main.rs` is the **binary crate**: a thin entrypoint that calls into
//! this library. Cargo builds both from the same package, and the binary
//! refers to the library by the package name with `-` replaced by `_`:
//! `financials_mcp`.
//!
//! Step 01 only sets up the project; the planner tools arrive in later steps
//! (see `docs/plans/financials-mcp-learning-plan.md`).

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
    format!("{NAME} {VERSION}: retirement projection tools for AI agents (not implemented yet)")
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
    fn banner_says_tools_are_not_implemented_yet() {
        assert_eq!(
            banner(),
            "financials-mcp 0.1.0: retirement projection tools for AI agents (not implemented yet)"
        );
    }
}
