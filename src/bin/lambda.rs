//! The AWS Lambda executable: `bootstrap` once built with cargo-lambda.
//!
//! A second binary in the same package, like `src/main.rs`, sharing the
//! library crate. It's built only with the `lambda` feature (see
//! `[[bin]]` in `Cargo.toml`). All the work is in `financials_mcp::lambda`.

use std::process::ExitCode;

// A single-threaded runtime, as for `serve`: each Lambda instance handles one
// request at a time, and that request mostly waits on financials-api.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match financials_mcp::lambda::run().await {
        Ok(()) => ExitCode::SUCCESS,
        // Startup problems (a missing token, a bad setting) land in
        // CloudWatch Logs through stderr, and Lambda reports the failed init.
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
