//! The command line: `financials-mcp project <plan.json>`.
//!
//! This debug command exercises the whole projection path (load, pre-check,
//! call financials-api, summarize) without an MCP client, the same way
//! agent-harness's `tool` command let tools be tried without a model.
//!
//! The command is described with `clap`'s derive API: annotate a struct and
//! an enum, and clap generates the parser, `--help` text and error messages,
//! much like serde generates JSON code from annotated types.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::error::Error;
use crate::plan::Plan;
use crate::projector::{Projector, project_and_summarize};

// `///` doc comments on clap types become the `--help` text, so they're
// written for the person running the program. Rust notes go in `//` comments
// like this one (a test checks `--help` for leaks).

/// Retirement projection tools for AI agents, backed by financials-api.
#[derive(Debug, Parser)]
#[command(name = "financials-mcp", version)]
pub struct Cli {
    // `Option` because running with no subcommand is allowed (it prints the
    // banner).
    #[command(subcommand)]
    pub command: Option<Command>,
}

// One variant per subcommand. Each variant's fields are its arguments.
#[derive(Debug, Subcommand, PartialEq)]
pub enum Command {
    /// Run a plan through financials-api and print its summary as JSON.
    ///
    /// Reads FINANCIALS_API_URL and FINANCIALS_ACCESS_TOKEN from the
    /// environment or a local .env file.
    Project {
        /// Path to a plan JSON file (the POST /projections request body).
        plan: PathBuf,
    },
    /// Start the MCP server for an MCP client.
    ///
    /// Reads FINANCIALS_API_URL and FINANCIALS_ACCESS_TOKEN from the
    /// environment or a local .env file. Logs go to stderr; set
    /// FINANCIALS_MCP_LOG to error, warn, info (default), debug, trace or off.
    // A `///` line is shorthand for `#[doc = "..."]`, so `cfg_attr` can add
    // documentation only when a feature is on: this paragraph is in `--help`
    // only in builds that have `--http`.
    #[cfg_attr(
        feature = "http",
        doc = "",
        doc = "Serves on stdin/stdout by default, or over HTTP with --http. Then every \
               request must send \"Authorization: Bearer <token>\" matching MCP_BEARER_TOKEN \
               (required, at least 32 characters), and the Host header must be in \
               MCP_ALLOWED_HOSTS (default: loopback only)."
    )]
    Serve {
        // `#[cfg(feature = "http")]` on a field removes it from the program
        // when the feature is off, so `--http` doesn't exist in that build.
        /// Serve MCP over HTTP at http://<listen>/mcp instead of stdio.
        #[cfg(feature = "http")]
        #[arg(long)]
        http: bool,

        /// The address to listen on with --http. The default accepts
        /// connections from this machine only.
        #[cfg(feature = "http")]
        #[arg(long, value_name = "ADDRESS", default_value = crate::http::DEFAULT_LISTEN, requires = "http")]
        listen: std::net::SocketAddr,
    },
}

/// Runs the `project` command: load, check, project, summarize, print.
///
/// Generic over the projector (`P: Projector`) so tests can pass a
/// `FakeProjector`, and over the output (`out: &mut impl Write`) so tests can
/// capture it in a `Vec<u8>` instead of the terminal. `Write` is the standard
/// trait for anything bytes can be written to: files, stdout, buffers.
pub async fn run_project<P: Projector>(
    projector: &P,
    plan_path: &Path,
    out: &mut impl Write,
) -> Result<(), Error> {
    let text = read_plan_file(plan_path)?;
    let plan = Plan::from_json_checked(&text)?;
    let summary = project_and_summarize(projector, &plan).await?;

    // `to_string_pretty` can only fail for types that can't be JSON (maps
    // with non-string keys, say); a summary always can, hence `expect`.
    let json = serde_json::to_string_pretty(&summary).expect("a summary always serializes");
    // `writeln!` is `println!` for any `Write`. Writing to a closed pipe can
    // fail, so it returns a `Result` too.
    writeln!(out, "{json}").map_err(|_| Error::Output)?;
    Ok(())
}

/// Reads a plan file. Errors name the path but never include file contents.
fn read_plan_file(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|err| {
        // `path.display()` formats a path for people; `Path` itself has no
        // `Display` because paths aren't always valid text (on Unix they can
        // be arbitrary bytes).
        let problem = match err.kind() {
            std::io::ErrorKind::NotFound => format!("plan file not found: {}", path.display()),
            std::io::ErrorKind::InvalidData => {
                format!("plan file is not UTF-8 text: {}", path.display())
            }
            _ => format!("could not read plan file: {}", path.display()),
        };
        Error::PlanFile { problem }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    const PLAN_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/plan-basic.json"
    );
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn projection() -> Projection {
        serde_json::from_str(PROJECTION).unwrap()
    }

    /// Writes `contents` to a temporary file that's deleted when the returned
    /// value is dropped (RAII again: cleanup is tied to scope).
    fn temp_plan(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    // ---- argument parsing -------------------------------------------------

    #[test]
    fn project_takes_a_plan_path() {
        let cli = Cli::try_parse_from(["financials-mcp", "project", "my-plan.json"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Project {
                plan: PathBuf::from("my-plan.json")
            })
        );
    }

    #[test]
    fn serve_needs_no_arguments() {
        let cli = Cli::try_parse_from(["financials-mcp", "serve"]).unwrap();
        // `{ .. }` matches the variant whatever fields it has: none without
        // the `http` feature, `http` and `listen` with it.
        assert!(matches!(cli.command, Some(Command::Serve { .. })));
    }

    // These tests only exist in builds with the `http` feature.
    #[cfg(feature = "http")]
    #[test]
    fn serve_defaults_to_stdio_and_loopback() {
        let cli = Cli::try_parse_from(["financials-mcp", "serve"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Serve {
                http: false,
                listen: crate::http::DEFAULT_LISTEN.parse().unwrap(),
            })
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn serve_http_takes_a_listen_address() {
        let cli = Cli::try_parse_from([
            "financials-mcp",
            "serve",
            "--http",
            "--listen",
            "127.0.0.1:9000",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Serve {
                http: true,
                listen: "127.0.0.1:9000".parse().unwrap(),
            })
        );

        // `--listen` without `--http` is a mistake, and so is a bad address.
        let without_http =
            Cli::try_parse_from(["financials-mcp", "serve", "--listen", "127.0.0.1:9000"]);
        assert!(without_http.is_err());
        let bad_address =
            Cli::try_parse_from(["financials-mcp", "serve", "--http", "--listen", "localhost"]);
        assert!(bad_address.is_err());
    }

    #[test]
    fn no_subcommand_is_allowed() {
        let cli = Cli::try_parse_from(["financials-mcp"]).unwrap();
        assert_eq!(cli.command, None);
    }

    #[test]
    fn usage_mistakes_are_rejected() {
        let missing = Cli::try_parse_from(["financials-mcp", "project"]).unwrap_err();
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let unknown = Cli::try_parse_from(["financials-mcp", "frobnicate"]).unwrap_err();
        assert_eq!(unknown.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn the_cli_definition_is_valid() {
        // clap's own consistency check for the whole command definition.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    // ---- run_project ------------------------------------------------------

    #[tokio::test]
    async fn prints_the_summary_as_json() {
        let fake = FakeProjector::scripted(vec![Ok(projection())]);
        let mut out = Vec::new();

        run_project(&fake, Path::new(PLAN_PATH), &mut out)
            .await
            .unwrap();

        let printed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed["depletionYear"], 36);
        assert_eq!(printed["lastsThroughFinalYear"], false);
        assert_eq!(fake.received().len(), 1);
    }

    #[tokio::test]
    async fn a_missing_file_names_the_path() {
        let fake = FakeProjector::scripted(Vec::new());
        let err = run_project(&fake, Path::new("no/such/plan.json"), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PlanFile { .. }), "{err:?}");
        assert!(
            err.to_string()
                .contains("plan file not found: no/such/plan.json"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn invalid_json_is_reported_without_contents() {
        let file = temp_plan("{ \"items\": [ secret-ish text");
        let fake = FakeProjector::scripted(Vec::new());

        let err = run_project(&fake, file.path(), &mut Vec::new())
            .await
            .unwrap_err();

        assert!(matches!(err, Error::InvalidJson { .. }), "{err:?}");
        assert!(!err.to_string().contains("secret"), "{err}");
    }

    #[tokio::test]
    async fn a_plan_failing_the_pre_checks_never_reaches_the_api() {
        let file = temp_plan(r#"{"years": 99, "items": []}"#);
        let fake = FakeProjector::scripted(Vec::new());

        let err = run_project(&fake, file.path(), &mut Vec::new())
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "invalid plan: items are required; years must be between 1 and 75"
        );
        assert!(fake.received().is_empty());
    }

    #[tokio::test]
    async fn api_errors_pass_through() {
        let fake = FakeProjector::scripted(vec![Err(Error::Auth)]);
        let err = run_project(&fake, Path::new(PLAN_PATH), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth), "{err:?}");
    }
}
