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
use financials_mcp::server::FinancialsServer;
use rmcp::ServiceExt;

/// `#[tokio::main]` turns `async fn main` into a normal `main` that starts a
/// tokio runtime and runs this function on it. `current_thread` uses a single
/// thread: projections mostly wait on the network, so tasks interleave on it.
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
        Some(Command::Serve) => serve().await,
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

/// Serves MCP over stdin/stdout until the client disconnects.
///
/// Configuration problems are reported before serving starts (on stderr, as
/// usual). An unreachable API is *not* a startup error: each tool call reports
/// it, so the model can tell the user.
async fn serve() -> Result<(), Error> {
    // Logs go to stderr (stdout carries the protocol), at the level set by
    // FINANCIALS_MCP_LOG. Only `serve` logs; `project` prints its result.
    financials_mcp::logging::init();
    let config = Config::from_env()?;
    let server = FinancialsServer::new(ApiProjector::new(&config)?);
    // The version only: the API URL and token stay out of the logs.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "serving MCP over stdio"
    );

    // `.serve(stdio())` performs the `initialize` handshake and returns a
    // running service; `.waiting()` resolves when the client disconnects.
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|_| Error::Server {
            problem: "the MCP client did not complete initialization".to_string(),
        })?;
    running.waiting().await.map_err(|_| Error::Server {
        problem: "the MCP server stopped unexpectedly".to_string(),
    })?;
    Ok(())
}

async fn project(plan: &std::path::Path) -> Result<(), Error> {
    let config = Config::from_env()?;
    let projector = ApiProjector::new(&config)?;
    // `std::io::stdout().lock()` takes the stdout lock once for the whole
    // write, instead of once per write call.
    run_project(&projector, plan, &mut std::io::stdout().lock()).await
}
