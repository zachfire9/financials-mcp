//! The `financials-mcp` executable.
//!
//! Kept deliberately thin: parse the command line, run the matching command
//! from the library, and turn the result into an exit code.

use std::process::ExitCode;

use clap::Parser;
use financials_mcp::api::ApiProjector;
use financials_mcp::cli::{Cli, Command, run_project};
use financials_mcp::config::Config;
use financials_mcp::error::Error;

/// `#[tokio::main]` turns `async fn main` into a normal `main` that starts a
/// tokio runtime and runs this function on it. `current_thread` uses a single
/// thread, which is plenty for one command; the MCP server will need more.
///
/// Returning `ExitCode` sets the process exit status: 0 for success, 1 for a
/// failure. (clap exits with 2 by itself for usage mistakes.)
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // `Cli::parse()` reads the real arguments; on `--help`, `--version` or a
    // usage error it prints the message and exits on its own.
    let cli = Cli::parse();

    let result = match cli.command {
        None => {
            println!("{}", financials_mcp::banner());
            Ok(())
        }
        Some(Command::Project { plan }) => project(&plan).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Errors go to stderr so stdout only ever holds the JSON result.
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn project(plan: &std::path::Path) -> Result<(), Error> {
    let config = Config::from_env()?;
    let projector = ApiProjector::new(&config)?;
    // `std::io::stdout().lock()` takes the stdout lock once for the whole
    // write, instead of once per write call.
    run_project(&projector, plan, &mut std::io::stdout().lock()).await
}
